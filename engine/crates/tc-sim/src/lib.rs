//! tc-sim: a deterministic simulated venue used for backtest and paper trading.
//!
//! Fill model (deliberately conservative — print-replay EV of passive fills is an UPPER BOUND, so we bias
//! against ourselves):
//! * **Taker** orders walk the visible ladder; depth consumed by one order is unavailable to the next until a
//!   fresh book arrives (no phantom depth).
//! * **Maker** orders rest with a *queue position* = size already displayed at our price. Trades at our price
//!   first drain the queue ahead of us; only the excess fills us. Trades strictly through our price fill us.
//! * Optional **latency**: an order becomes active `latency_ns` after submission and meets the book as it is then.
//! * `post_only` orders that would cross are rejected, never silently converted to takers.

use std::collections::{HashMap, HashSet};
use tc_core::*;

#[derive(Clone, Debug)]
pub struct SimConfig {
    /// Delay between submit and the order reaching the matching engine.
    pub latency_ns: i64,
    /// Full spread (bps of close) used to synthesise a book from a bar.
    pub bar_spread_bps: f64,
    /// Depth shown on each side of a synthesised bar book.
    pub bar_depth: Qty,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            latency_ns: 0,
            bar_spread_bps: 4.0,
            bar_depth: Qty::units(1_000_000),
        }
    }
}

#[derive(Clone, Debug)]
struct Resting {
    req: OrderReq,
    remaining: Qty,
    queue_ahead: Qty,
    seq: u64,
}

#[derive(Clone, Debug, Default)]
pub struct SimStats {
    pub post_only_rejects: u64,
    pub maker_fills: u64,
    pub taker_fills: u64,
}

pub struct SimVenue {
    name: String,
    cfg: SimConfig,
    instruments: HashMap<InstrumentId, Instrument>,
    books: HashMap<InstrumentId, Book>,
    resting: Vec<Resting>,
    pending: Vec<(Ts, u64, OrderReq)>,
    seen_ids: HashSet<String>,
    fills: Vec<Fill>,
    /// depth already taken from the current book, keyed by (instrument, book side, price)
    consumed: HashMap<(InstrumentId, bool, Px), Qty>,
    now: Ts,
    seq: u64,
    pub stats: SimStats,
}

impl SimVenue {
    pub fn new(name: &str, cfg: SimConfig, instruments: Vec<Instrument>) -> Self {
        SimVenue {
            name: name.into(),
            cfg,
            instruments: instruments.into_iter().map(|i| (i.id.clone(), i)).collect(),
            books: HashMap::new(),
            resting: Vec::new(),
            pending: Vec::new(),
            seen_ids: HashSet::new(),
            fills: Vec::new(),
            consumed: HashMap::new(),
            now: Ts(0),
            seq: 0,
            stats: SimStats::default(),
        }
    }

    fn take_consumed(&self, id: &InstrumentId, ask_side: bool, px: Px) -> Qty {
        self.consumed
            .get(&(id.clone(), ask_side, px))
            .copied()
            .unwrap_or(Qty::ZERO)
    }

    fn push_fill(&mut self, r: &OrderReq, px: Px, qty: Qty, liq: Liquidity, ts: Ts) {
        let fee = self.instruments[&r.instrument].fee.fee(px, qty, liq);
        match liq {
            Liquidity::Maker => self.stats.maker_fills += 1,
            Liquidity::Taker => self.stats.taker_fills += 1,
        }
        self.fills.push(Fill {
            client_id: r.client_id.clone(),
            instrument: r.instrument.clone(),
            side: r.side,
            px,
            qty,
            fee,
            liquidity: liq,
            ts,
            tag: r.tag.clone(),
        });
    }

    /// Does `req` cross the current book?
    fn crosses(&self, req: &OrderReq) -> bool {
        match self.books.get(&req.instrument) {
            None => false,
            Some(b) => match req.side {
                Side::Buy => b.best_ask().map_or(false, |l| l.px <= req.px),
                Side::Sell => b.best_bid().map_or(false, |l| l.px >= req.px),
            },
        }
    }

    /// Order reaches the matching engine at `ts`.
    fn activate(&mut self, ts: Ts, req: OrderReq) -> Result<(), PlaceError> {
        if req.post_only && self.crosses(&req) {
            self.stats.post_only_rejects += 1;
            return Err(PlaceError::PostOnlyWouldCross);
        }
        let mut remaining = req.qty;
        // ---- taker leg: walk the opposite side
        if let Some(book) = self.books.get(&req.instrument).cloned() {
            let (levels, ask_side) = match req.side {
                Side::Buy => (&book.asks, true),
                Side::Sell => (&book.bids, false),
            };
            for lvl in levels {
                if remaining.0 == 0 {
                    break;
                }
                let ok = match req.side {
                    Side::Buy => lvl.px <= req.px,
                    Side::Sell => lvl.px >= req.px,
                };
                if !ok {
                    break;
                }
                let used = self.take_consumed(&req.instrument, ask_side, lvl.px);
                let avail = Qty((lvl.qty.0 - used.0).max(0));
                let take = Qty(avail.0.min(remaining.0));
                if take.0 > 0 {
                    *self
                        .consumed
                        .entry((req.instrument.clone(), ask_side, lvl.px))
                        .or_default() += take;
                    self.push_fill(&req, lvl.px, take, Liquidity::Taker, ts);
                    remaining -= take;
                }
            }
        }
        // ---- rest the remainder
        if remaining.0 > 0 && req.tif == Tif::Gtc {
            let queue_ahead = self
                .books
                .get(&req.instrument)
                .map(|b| {
                    let own = if req.side == Side::Buy {
                        &b.bids
                    } else {
                        &b.asks
                    };
                    own.iter()
                        .find(|l| l.px == req.px)
                        .map(|l| l.qty)
                        .unwrap_or(Qty::ZERO)
                })
                .unwrap_or(Qty::ZERO);
            self.seq += 1;
            self.resting.push(Resting {
                req,
                remaining,
                queue_ahead,
                seq: self.seq,
            });
        }
        Ok(())
    }

    fn activate_due(&mut self) {
        let now = self.now;
        let mut due: Vec<(Ts, u64, OrderReq)> = Vec::new();
        self.pending.retain(|p| {
            if p.0 <= now {
                due.push(p.clone());
                false
            } else {
                true
            }
        });
        due.sort_by_key(|p| (p.0, p.1));
        for (ts, _, req) in due {
            let _ = self.activate(ts, req); // post-only rejects are counted in stats
        }
    }

    fn on_book(&mut self, b: &Book) {
        self.books.insert(b.instrument.clone(), b.clone());
        self.consumed.retain(|(id, _, _), _| id != &b.instrument);
        let ts = b.ts;
        let mut order: Vec<usize> = (0..self.resting.len())
            .filter(|&i| self.resting[i].req.instrument == b.instrument)
            .collect();
        order.sort_by_key(|&i| self.resting[i].seq);
        let mut to_fill: Vec<(usize, Px, Qty)> = Vec::new();
        for i in order {
            let r = self.resting[i].clone();
            let (levels, ask_side) = match r.req.side {
                Side::Buy => (&b.asks, true),
                Side::Sell => (&b.bids, false),
            };
            let mut rem = r.remaining;
            for lvl in levels {
                if rem.0 == 0 {
                    break;
                }
                let ok = match r.req.side {
                    Side::Buy => lvl.px <= r.req.px,
                    Side::Sell => lvl.px >= r.req.px,
                };
                if !ok {
                    break;
                }
                let used = self.take_consumed(&b.instrument, ask_side, lvl.px);
                let avail = Qty((lvl.qty.0 - used.0).max(0));
                let take = Qty(avail.0.min(rem.0));
                if take.0 > 0 {
                    *self
                        .consumed
                        .entry((b.instrument.clone(), ask_side, lvl.px))
                        .or_default() += take;
                    // maker fills at our own limit price (no price improvement credited)
                    to_fill.push((i, r.req.px, take));
                    rem -= take;
                }
            }
            // queue can only shrink when displayed size at our level shrinks
            let own = if r.req.side == Side::Buy {
                &b.bids
            } else {
                &b.asks
            };
            let shown = own
                .iter()
                .find(|l| l.px == r.req.px)
                .map(|l| l.qty)
                .unwrap_or(Qty::ZERO);
            self.resting[i].queue_ahead = Qty(self.resting[i].queue_ahead.0.min(shown.0));
        }
        self.apply_maker_fills(to_fill, ts);
    }

    fn apply_maker_fills(&mut self, fills: Vec<(usize, Px, Qty)>, ts: Ts) {
        for (i, px, qty) in fills {
            let req = self.resting[i].req.clone();
            self.resting[i].remaining -= qty;
            self.push_fill(&req, px, qty, Liquidity::Maker, ts);
        }
        self.resting.retain(|r| r.remaining.0 > 0);
    }

    fn on_trade(&mut self, t: &TradeTick) {
        let mut budget = t.qty;
        let mut order: Vec<usize> = (0..self.resting.len())
            .filter(|&i| self.resting[i].req.instrument == t.instrument)
            .collect();
        order.sort_by_key(|&i| self.resting[i].seq);
        let mut to_fill = Vec::new();
        for i in order {
            if budget.0 == 0 {
                break;
            }
            let r = &mut self.resting[i];
            // a taker buy hits resting sells; a taker sell hits resting buys
            let (hit, through) = match (r.req.side, t.taker_side) {
                (Side::Buy, Side::Sell) => (t.px <= r.req.px, t.px < r.req.px),
                (Side::Sell, Side::Buy) => (t.px >= r.req.px, t.px > r.req.px),
                _ => (false, false),
            };
            if !hit {
                continue;
            }
            if !through {
                // trade AT our price: queue ahead of us trades first
                let q = Qty(r.queue_ahead.0.min(budget.0));
                r.queue_ahead -= q;
                budget -= q;
            } else {
                r.queue_ahead = Qty::ZERO;
            }
            let take = Qty(r.remaining.0.min(budget.0));
            if take.0 > 0 && r.queue_ahead.0 == 0 {
                budget -= take;
                to_fill.push((i, r.req.px, take));
            }
        }
        self.apply_maker_fills(to_fill, t.ts);
    }

    fn on_bar(&mut self, bar: &Bar) {
        // resting orders: strictly-through bars fill us, capped by bar volume
        let mut cap = if bar.volume.0 > 0 {
            bar.volume
        } else {
            Qty(i64::MAX / 4)
        };
        let mut order: Vec<usize> = (0..self.resting.len())
            .filter(|&i| self.resting[i].req.instrument == bar.instrument)
            .collect();
        order.sort_by_key(|&i| self.resting[i].seq);
        let mut to_fill = Vec::new();
        for i in order {
            let r = &self.resting[i];
            let through = match r.req.side {
                Side::Buy => bar.low < r.req.px,
                Side::Sell => bar.high > r.req.px,
            };
            if through && cap.0 > 0 {
                let take = Qty(r.remaining.0.min(cap.0));
                cap -= take;
                to_fill.push((i, r.req.px, take));
            }
        }
        self.apply_maker_fills(to_fill, bar.ts);
        // then synthesise a book around the close so taker orders have something to hit
        let half = Px(((bar.close.0 as f64) * self.cfg.bar_spread_bps / 20_000.0).round() as i64);
        let half = Px(half.0.max(1));
        let book = Book {
            instrument: bar.instrument.clone(),
            ts: bar.ts,
            bids: vec![Level {
                px: bar.close - half,
                qty: self.cfg.bar_depth,
            }],
            asks: vec![Level {
                px: bar.close + half,
                qty: self.cfg.bar_depth,
            }],
        };
        self.on_book(&book);
    }
}

impl Venue for SimVenue {
    fn name(&self) -> &str {
        &self.name
    }

    fn feed(&mut self, ev: &Event) {
        self.now = self.now.max(ev.ts());
        match ev {
            Event::Book(b) => self.on_book(b),
            Event::Trade(t) => self.on_trade(t),
            Event::Bar(b) => self.on_bar(b),
            Event::Settlement(s) => {
                // the venue drops every resting order on a settled market
                self.resting.retain(|r| r.req.instrument != s.instrument);
                self.pending.retain(|p| p.2.instrument != s.instrument);
                self.books.remove(&s.instrument);
            }
            Event::Signal(_) | Event::Timer(_) => {}
        }
        self.activate_due();
    }

    fn place(&mut self, now: Ts, req: OrderReq) -> Result<(), PlaceError> {
        let Some(inst) = self.instruments.get(&req.instrument) else {
            return Err(PlaceError::Rejected(format!(
                "unknown instrument {}",
                req.instrument
            )));
        };
        if req.qty.0 <= 0 || req.qty.0 % inst.lot.0 != 0 {
            return Err(PlaceError::Rejected(
                "quantity not a positive multiple of lot".into(),
            ));
        }
        if !inst.valid_price(req.px) {
            return Err(PlaceError::Rejected(format!("invalid price {}", req.px)));
        }
        if !self.seen_ids.insert(req.client_id.clone()) {
            return Err(PlaceError::Duplicate(req.client_id));
        }
        self.now = self.now.max(now);
        if self.cfg.latency_ns > 0 {
            self.seq += 1;
            self.pending
                .push((Ts(now.0 + self.cfg.latency_ns), self.seq, req));
            Ok(())
        } else {
            self.activate(now, req)
        }
    }

    fn cancel(&mut self, client_id: &str) -> CancelResult {
        if let Some(pos) = self
            .resting
            .iter()
            .position(|r| r.req.client_id == client_id)
        {
            self.resting.remove(pos);
            return CancelResult::Cancelled;
        }
        if let Some(pos) = self.pending.iter().position(|p| p.2.client_id == client_id) {
            self.pending.remove(pos);
            return CancelResult::Cancelled;
        }
        // filled, expired, or never existed — NOT a successful cancel
        CancelResult::AlreadyGone
    }

    fn open_orders(&self) -> Vec<OrderReq> {
        self.resting
            .iter()
            .map(|r| {
                let mut q = r.req.clone();
                q.qty = r.remaining;
                q
            })
            .collect()
    }

    fn drain_fills(&mut self) -> Vec<Fill> {
        std::mem::take(&mut self.fills)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lvl(c: i64, q: i64) -> Level {
        Level {
            px: Px::cents(c),
            qty: Qty::units(q),
        }
    }
    fn book(ts: i64, bids: Vec<Level>, asks: Vec<Level>) -> Event {
        Event::Book(Book {
            instrument: InstrumentId::new("M:YES"),
            ts: Ts(ts),
            bids,
            asks,
        })
    }
    fn order(id: &str, side: Side, c: i64, q: i64, tif: Tif, post: bool) -> OrderReq {
        OrderReq {
            client_id: id.into(),
            instrument: InstrumentId::new("M:YES"),
            side,
            px: Px::cents(c),
            qty: Qty::units(q),
            tif,
            post_only: post,
            tag: "t".into(),
        }
    }
    fn sim() -> SimVenue {
        SimVenue::new(
            "sim",
            SimConfig::default(),
            vec![Instrument::binary("M:YES", "M")],
        )
    }

    #[test]
    fn taker_walks_ladder_and_depth_is_not_reused() {
        let mut s = sim();
        s.feed(&book(1, vec![lvl(48, 10)], vec![lvl(50, 5), lvl(52, 5)]));
        s.place(Ts(1), order("a", Side::Buy, 52, 8, Tif::Ioc, false))
            .unwrap();
        let f = s.drain_fills();
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].px, f[0].qty), (Px::cents(50), Qty::units(5)));
        assert_eq!((f[1].px, f[1].qty), (Px::cents(52), Qty::units(3)));
        // second order sees only 2 left at 52
        s.place(Ts(1), order("b", Side::Buy, 52, 8, Tif::Ioc, false))
            .unwrap();
        let f2 = s.drain_fills();
        assert_eq!(f2.iter().map(|x| x.qty.0).sum::<i64>(), Qty::units(2).0);
    }

    #[test]
    fn post_only_crossing_rejected() {
        let mut s = sim();
        s.feed(&book(1, vec![lvl(48, 10)], vec![lvl(50, 5)]));
        let e = s
            .place(Ts(1), order("a", Side::Buy, 50, 1, Tif::Gtc, true))
            .unwrap_err();
        assert_eq!(e, PlaceError::PostOnlyWouldCross);
        assert!(s.open_orders().is_empty());
    }

    #[test]
    fn maker_needs_queue_to_clear_before_fill() {
        let mut s = sim();
        s.feed(&book(1, vec![lvl(48, 10)], vec![lvl(50, 5)]));
        s.place(Ts(1), order("m", Side::Buy, 48, 3, Tif::Gtc, true))
            .unwrap();
        let tr = |t, q| {
            Event::Trade(TradeTick {
                instrument: InstrumentId::new("M:YES"),
                ts: Ts(t),
                px: Px::cents(48),
                qty: Qty::units(q),
                taker_side: Side::Sell,
            })
        };
        s.feed(&tr(2, 6)); // drains 6 of the 10 ahead
        assert!(s.drain_fills().is_empty());
        s.feed(&tr(3, 6)); // drains remaining 4, then 2 fill us
        let f = s.drain_fills();
        assert_eq!(f.len(), 1);
        assert_eq!(
            (f[0].qty, f[0].liquidity),
            (Qty::units(2), Liquidity::Maker)
        );
        assert_eq!(s.open_orders()[0].qty, Qty::units(1));
    }

    #[test]
    fn trade_through_fills_at_our_limit() {
        let mut s = sim();
        s.feed(&book(1, vec![lvl(48, 10)], vec![lvl(50, 5)]));
        s.place(Ts(1), order("m", Side::Buy, 48, 3, Tif::Gtc, true))
            .unwrap();
        s.feed(&Event::Trade(TradeTick {
            instrument: InstrumentId::new("M:YES"),
            ts: Ts(2),
            px: Px::cents(46),
            qty: Qty::units(3),
            taker_side: Side::Sell,
        }));
        let f = s.drain_fills();
        assert_eq!((f[0].px, f[0].qty), (Px::cents(48), Qty::units(3)));
        assert!(s.open_orders().is_empty());
    }

    #[test]
    fn cancel_after_fill_is_already_gone_not_cancelled() {
        let mut s = sim();
        s.feed(&book(1, vec![lvl(48, 0)], vec![lvl(50, 5)]));
        s.place(Ts(1), order("m", Side::Buy, 48, 1, Tif::Gtc, true))
            .unwrap();
        s.feed(&Event::Trade(TradeTick {
            instrument: InstrumentId::new("M:YES"),
            ts: Ts(2),
            px: Px::cents(47),
            qty: Qty::units(5),
            taker_side: Side::Sell,
        }));
        assert_eq!(s.cancel("m"), CancelResult::AlreadyGone);
        assert_eq!(s.drain_fills().len(), 1);
    }

    #[test]
    fn duplicate_client_id_rejected() {
        let mut s = sim();
        s.feed(&book(1, vec![lvl(48, 10)], vec![lvl(50, 5)]));
        s.place(Ts(1), order("a", Side::Buy, 40, 1, Tif::Gtc, true))
            .unwrap();
        assert!(matches!(
            s.place(Ts(1), order("a", Side::Buy, 40, 1, Tif::Gtc, true)),
            Err(PlaceError::Duplicate(_))
        ));
    }

    #[test]
    fn latency_defers_activation() {
        let cfg = SimConfig {
            latency_ns: 1_000,
            ..Default::default()
        };
        let mut s = SimVenue::new("sim", cfg, vec![Instrument::binary("M:YES", "M")]);
        s.feed(&book(1, vec![lvl(48, 10)], vec![lvl(50, 5)]));
        s.place(Ts(1), order("a", Side::Buy, 50, 2, Tif::Ioc, false))
            .unwrap();
        assert!(s.drain_fills().is_empty()); // not yet active
                                             // book moves away before the order arrives → no fill at the stale price
        s.feed(&book(2_000, vec![lvl(54, 10)], vec![lvl(56, 5)]));
        assert!(s.drain_fills().is_empty());
    }

    #[test]
    fn settlement_drops_resting_orders() {
        let mut s = sim();
        s.feed(&book(1, vec![lvl(48, 10)], vec![lvl(50, 5)]));
        s.place(Ts(1), order("m", Side::Buy, 40, 1, Tif::Gtc, true))
            .unwrap();
        s.feed(&Event::Settlement(Settlement {
            instrument: InstrumentId::new("M:YES"),
            ts: Ts(5),
            price: Px::ONE,
        }));
        assert!(s.open_orders().is_empty());
    }

    #[test]
    fn bar_synthesises_book_for_taker() {
        let mut s = SimVenue::new(
            "sim",
            SimConfig::default(),
            vec![Instrument::linear("AAPL", "AAPL", Px(100))],
        );
        s.feed(&Event::Bar(Bar {
            instrument: InstrumentId::new("AAPL"),
            ts: Ts(1),
            open: Px::from_f64(100.0),
            high: Px::from_f64(101.0),
            low: Px::from_f64(99.0),
            close: Px::from_f64(100.0),
            volume: Qty::units(1000),
        }));
        let mut o = order("x", Side::Buy, 0, 10, Tif::Ioc, false);
        o.instrument = InstrumentId::new("AAPL");
        o.px = Px::from_f64(100.5);
        s.place(Ts(1), o).unwrap();
        let f = s.drain_fills();
        assert_eq!(f.len(), 1);
        assert!(f[0].px > Px::from_f64(100.0)); // paid half-spread
    }
}
