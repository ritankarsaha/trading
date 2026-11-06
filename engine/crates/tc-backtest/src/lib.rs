//! tc-backtest: the `Engine` — one event loop for backtest, paper and live (only the `Venue` differs).
//!
//! Per event, in this fixed order (the order is part of the leak-prevention contract):
//!  1. reject out-of-order timestamps (time never goes backwards),
//!  2. venue consumes the event (sim: update books, match resting orders),
//!  3. venue fills → portfolio + ledger (+ strategy `on_fill`),
//!  4. engine state updates (books, marks, settlements),
//!  5. breaker check; on a halt: cancel every resting order,
//!  6. each strategy sees the event and returns actions,
//!  7. every action passes the risk gate before touching the venue.

pub mod synth;

use std::collections::{BTreeMap, HashMap};
use tc_core::*;
use tc_ledger::{Ledger, Portfolio, Record};
use tc_risk::{RiskGate, RiskView};
use tc_strategy::{Action, Ctx, Strategy};

#[derive(Debug)]
pub enum EngineError {
    OutOfOrder { prev: Ts, got: Ts },
}
impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::OutOfOrder { prev, got } => {
                write!(f, "event time went backwards: {} -> {}", prev.0, got.0)
            }
        }
    }
}
impl std::error::Error for EngineError {}

#[derive(Clone, Debug, Default)]
pub struct Counters {
    pub orders_sent: u64,
    pub risk_rejects: u64,
    pub venue_rejects: u64,
    pub fills: u64,
    pub maker_fills: u64,
    pub taker_fills: u64,
    pub replaces_skipped: u64,
    pub halts: u64,
    pub reject_reasons: BTreeMap<String, u64>,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub start_equity: Money,
    pub end_equity: Money,
    pub pnl: Money,
    pub return_pct: f64,
    pub max_drawdown_pct: f64,
    /// Mean/stdev of per-event equity changes. Not annualised — compare runs on the same data only.
    pub sharpe_per_step: f64,
    pub fees_paid: Money,
    pub counters: Counters,
    pub realized_by_tag: BTreeMap<String, Money>,
    pub open_positions: usize,
    pub digest: u64,
}

pub struct Engine<V: Venue> {
    pub venue: V,
    pub instruments: HashMap<InstrumentId, Instrument>,
    pub portfolio: Portfolio,
    pub ledger: Ledger,
    pub risk: RiskGate,
    pub strategies: Vec<Box<dyn Strategy>>,
    pub books: HashMap<InstrumentId, Book>,
    pub marks: BTreeMap<InstrumentId, Px>,
    pub counters: Counters,
    start_equity: Money,
    last_ts: Option<Ts>,
    curve: Vec<Money>,
}

impl<V: Venue> Engine<V> {
    pub fn new(
        venue: V,
        instruments: Vec<Instrument>,
        start_cash: Money,
        risk: RiskGate,
        ledger: Ledger,
        strategies: Vec<Box<dyn Strategy>>,
    ) -> Self {
        Engine {
            venue,
            instruments: instruments.into_iter().map(|i| (i.id.clone(), i)).collect(),
            portfolio: Portfolio::new(start_cash),
            ledger,
            risk,
            strategies,
            books: HashMap::new(),
            marks: BTreeMap::new(),
            counters: Counters::default(),
            start_equity: start_cash,
            last_ts: None,
            curve: vec![start_cash],
        }
    }

    fn apply_fills(&mut self) {
        for f in self.venue.drain_fills() {
            self.counters.fills += 1;
            match f.liquidity {
                Liquidity::Maker => self.counters.maker_fills += 1,
                Liquidity::Taker => self.counters.taker_fills += 1,
            }
            let realized = self.portfolio.apply_fill(&f);
            if realized.0 != 0 {
                // a realisation feeds the loss-streak breaker (net of this fill's fee)
                self.risk.on_trade_result(f.ts, realized - f.fee);
            }
            self.ledger.append(Record::Fill(f.clone()));
            for s in self.strategies.iter_mut() {
                if s.name() == f.tag {
                    s.on_fill(&f);
                }
            }
        }
    }

    fn cancel_all(&mut self, ts: Ts, why: &str) {
        for o in self.venue.open_orders() {
            let r = self.venue.cancel(&o.client_id);
            self.ledger.append(Record::Note {
                ts,
                text: format!("cancel-all ({why}) {} -> {:?}", o.client_id, r),
            });
        }
    }

    /// Returns `Err(reason)` if the risk gate or venue refused the order.
    fn submit(&mut self, ts: Ts, req: OrderReq) -> Result<(), String> {
        let open = self.venue.open_orders();
        let verdict = {
            let view = RiskView {
                portfolio: &self.portfolio,
                marks: &self.marks,
                open_orders: &open,
                instruments: &self.instruments,
            };
            self.risk.check(ts, &req, &view)
        };
        if let Err(r) = verdict {
            self.counters.risk_rejects += 1;
            *self
                .counters
                .reject_reasons
                .entry(r.key().to_string())
                .or_default() += 1;
            self.ledger.append(Record::Reject {
                ts,
                client_id: req.client_id.clone(),
                reason: r.to_string(),
            });
            return Err(r.to_string());
        }
        self.counters.orders_sent += 1;
        let cid = req.client_id.clone();
        let result = match self.venue.place(ts, req) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.counters.venue_rejects += 1;
                self.ledger.append(Record::Reject {
                    ts,
                    client_id: cid,
                    reason: e.to_string(),
                });
                Err(e.to_string())
            }
        };
        self.apply_fills(); // immediate (taker) fills
        result
    }

    pub fn step(&mut self, ev: &Event) -> Result<(), EngineError> {
        let ts = ev.ts();
        if let Some(prev) = self.last_ts {
            if ts < prev {
                return Err(EngineError::OutOfOrder { prev, got: ts });
            }
        }
        self.last_ts = Some(ts);

        self.venue.feed(ev);
        self.apply_fills();

        match ev {
            Event::Book(b) => {
                if let Some(m) = b.mid() {
                    self.marks.insert(b.instrument.clone(), m);
                }
                self.books.insert(b.instrument.clone(), b.clone());
            }
            Event::Trade(t) => {
                if !self.books.contains_key(&t.instrument) {
                    self.marks.insert(t.instrument.clone(), t.px);
                }
            }
            Event::Bar(b) => {
                self.marks.insert(b.instrument.clone(), b.close);
            }
            Event::Settlement(s) => {
                let pnl = self.portfolio.apply_settlement(&s.instrument, s.price);
                if pnl.0 != 0 {
                    self.risk.on_trade_result(ts, pnl);
                }
                self.ledger.append(Record::Settlement {
                    ts,
                    instrument: s.instrument.clone(),
                    price: s.price,
                    pnl,
                });
                self.marks.remove(&s.instrument);
                self.books.remove(&s.instrument);
            }
            Event::Signal(_) | Event::Timer(_) => {}
        }

        let equity = self.portfolio.equity(&self.marks);
        if let Some(reason) = self.risk.on_equity(ts, equity) {
            self.counters.halts += 1;
            self.ledger.append(Record::Halt {
                ts,
                reason: reason.clone(),
            });
        }
        if let Some(reason) = self.risk.halt_reason() {
            // a halt (breaker, manual, or halt file) cancels everything resting
            if !self.venue.open_orders().is_empty() {
                self.cancel_all(ts, &reason);
            }
        }

        // strategies (sized by the risk gate's graduated ladders: drawdown, weekly loss, loss streak)
        let size_scale = self.risk.size_scale();
        let mut strategies = std::mem::take(&mut self.strategies);
        for st in strategies.iter_mut() {
            let open = self.venue.open_orders();
            let actions = {
                let ctx = Ctx {
                    now: ts,
                    portfolio: &self.portfolio,
                    marks: &self.marks,
                    books: &self.books,
                    instruments: &self.instruments,
                    open_orders: &open,
                    size_scale,
                };
                st.on_event(&ctx, ev)
            };
            for a in actions {
                match a {
                    Action::Place(req) => {
                        if let Err(why) = self.submit(ts, req.clone()) {
                            st.on_reject(ts, &req, &why);
                        }
                    }
                    Action::Cancel(id) => {
                        let r = self.venue.cancel(&id);
                        if let CancelResult::Failed(m) = r {
                            self.ledger.append(Record::Note {
                                ts,
                                text: format!("cancel {id} failed: {m}"),
                            });
                        }
                    }
                    Action::Replace { cancel, place } => match self.venue.cancel(&cancel) {
                        CancelResult::Cancelled => {
                            if let Err(why) = self.submit(ts, place.clone()) {
                                st.on_reject(ts, &place, &why);
                            }
                        }
                        other => {
                            // AlreadyGone ⇒ it most likely filled; do NOT stack a new order on top of it.
                            self.counters.replaces_skipped += 1;
                            self.ledger.append(Record::Note {
                                ts,
                                text: format!("replace skipped: cancel {cancel} -> {other:?}"),
                            });
                        }
                    },
                }
            }
        }
        self.strategies = strategies;

        self.curve.push(self.portfolio.equity(&self.marks));
        Ok(())
    }

    pub fn run<I: IntoIterator<Item = Event>>(&mut self, events: I) -> Result<Report, EngineError> {
        for ev in events {
            self.step(&ev)?;
        }
        Ok(self.report())
    }

    pub fn report(&self) -> Report {
        let end = self.portfolio.equity(&self.marks);
        let mut peak = self.curve[0].0 as f64;
        let mut mdd = 0.0f64;
        for e in &self.curve {
            let v = e.0 as f64;
            peak = peak.max(v);
            if peak > 0.0 {
                mdd = mdd.max((peak - v) / peak);
            }
        }
        let rets: Vec<f64> = self
            .curve
            .windows(2)
            .map(|w| (w[1].0 - w[0].0) as f64)
            .collect();
        let n = rets.len().max(1) as f64;
        let mean = rets.iter().sum::<f64>() / n;
        let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / n;
        let sharpe = if var > 0.0 { mean / var.sqrt() } else { 0.0 };
        Report {
            start_equity: self.start_equity,
            end_equity: end,
            pnl: end - self.start_equity,
            return_pct: 100.0 * (end.0 - self.start_equity.0) as f64
                / self.start_equity.0.max(1) as f64,
            max_drawdown_pct: 100.0 * mdd,
            sharpe_per_step: sharpe,
            fees_paid: self.portfolio.fees_paid,
            counters: self.counters.clone(),
            realized_by_tag: self.portfolio.realized_by_tag.clone(),
            open_positions: self
                .portfolio
                .positions
                .values()
                .filter(|p| p.qty.0 != 0)
                .count(),
            digest: self.ledger.digest(),
        }
    }
}
