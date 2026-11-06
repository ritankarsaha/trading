//! Prediction-market strategies — the two that survived correct settlement + fees in the evidence base
//! (`claude-trading-skills/prediction-market-strategy`):
//!
//! 1. [`LongshotFade`] — maker-side favourite–longshot fade: rest post-only SELL-YES (= a maker NO bid) on cheap
//!    brackets, diversified across many events by the risk gate's group caps.
//! 2. [`SignalEdge`] — maker YES entry when a calibrated model (optionally Benter-blended with the market price)
//!    exceeds the market by more than the θ gate, sized with fractional Kelly.
//!
//! Both are maker-only (`post_only`), both requote with truthful-cancel `Replace`, and neither ever crosses.

use crate::*;
use std::collections::HashMap;
use tc_core::sizing;

/// How to turn a quoted price into the *true* YES probability for a longshot.
#[derive(Clone, Debug)]
pub enum Calibration {
    /// `p_true = price × shrink`. Placeholder prior (skills: a $0.05 contract historically wins ~2%) —
    /// REPLACE with the fitted table from `research/` once Phase 2 produces it.
    Shrink(f64),
    /// `(lo_inclusive, hi_exclusive, p_true)` buckets fitted from settled data.
    Table(Vec<(Px, Px, f64)>),
}

impl Calibration {
    pub fn p_true(&self, px: Px) -> Option<f64> {
        match self {
            Calibration::Shrink(s) => Some((px.as_f64() * s).clamp(1e-6, 1.0 - 1e-6)),
            Calibration::Table(t) => t
                .iter()
                .find(|(lo, hi, _)| px >= *lo && px < *hi)
                .map(|x| x.2),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FadeConfig {
    pub band_lo: Px,
    pub band_hi: Px,
    pub calibration: Calibration,
    /// Minimum edge per contract (dollars) = `px − p_true`.
    pub min_edge: f64,
    pub kelly_frac: f64,
    pub max_bet_fraction: f64,
    pub max_units: i64,
    /// Pre-declared kill line: after this many realisations, negative net PnL ⇒ the strategy stands down.
    pub kill_min_results: u64,
    /// After a rejected order, leave that instrument alone for this long.
    pub reject_backoff_ns: i64,
    /// Quote one tick inside the best ask when the spread allows (queue priority) instead of joining it.
    pub improve: bool,
    pub tag: String,
}

impl Default for FadeConfig {
    fn default() -> Self {
        FadeConfig {
            band_lo: Px::cents(5),
            band_hi: Px::cents(20),
            calibration: Calibration::Shrink(0.6),
            min_edge: 0.02,
            kelly_frac: 0.25,
            max_bet_fraction: 0.015,
            max_units: 1_000,
            kill_min_results: 50,
            reject_backoff_ns: 60_000_000_000,
            improve: false,
            tag: "longshot_fade".into(),
        }
    }
}

pub struct LongshotFade {
    cfg: FadeConfig,
    universe: Option<Vec<InstrumentId>>,
    live: HashMap<InstrumentId, (String, Px)>,
    backoff: HashMap<InstrumentId, Ts>,
    n: u64,
}

impl LongshotFade {
    pub fn new(cfg: FadeConfig, universe: Option<Vec<InstrumentId>>) -> Self {
        LongshotFade {
            cfg,
            universe,
            live: HashMap::new(),
            backoff: HashMap::new(),
            n: 0,
        }
    }
}

impl Strategy for LongshotFade {
    fn name(&self) -> &str {
        &self.cfg.tag
    }

    fn on_reject(&mut self, now: Ts, req: &OrderReq, _reason: &str) {
        self.live.remove(&req.instrument);
        self.backoff.insert(
            req.instrument.clone(),
            Ts(now.0 + self.cfg.reject_backoff_ns),
        );
    }

    fn on_event(&mut self, ctx: &Ctx, ev: &Event) -> Vec<Action> {
        match ev {
            Event::Settlement(s) => {
                self.live.remove(&s.instrument);
                vec![]
            }
            Event::Book(b) => {
                if let Some(u) = &self.universe {
                    if !u.contains(&b.instrument) {
                        return vec![];
                    }
                }
                let Some(inst) = ctx.inst(&b.instrument) else {
                    return vec![];
                };
                if self
                    .backoff
                    .get(&b.instrument)
                    .map_or(false, |&t| ctx.now < t)
                {
                    return vec![]; // recently refused: leave it alone
                }
                let Some((kf, scale)) = evidence_gate(
                    ctx,
                    &self.cfg.tag,
                    self.cfg.kelly_frac,
                    self.cfg.kill_min_results,
                ) else {
                    return self.cancel_live(&b.instrument); // kill line hit or risk scale 0: stand down
                };
                // forget a live order that has filled / been removed
                if let Some((cid, _)) = self.live.get(&b.instrument) {
                    if !ctx.open_orders.iter().any(|o| &o.client_id == cid) {
                        self.live.remove(&b.instrument);
                    }
                }
                let Some(ask) = b.best_ask() else {
                    return self.cancel_live(&b.instrument);
                };
                let mut px = ask.px;
                if self.cfg.improve {
                    let inside = Px(ask.px.0 - inst.tick.0);
                    if b.best_bid().map_or(true, |bid| inside > bid.px) {
                        px = inside;
                    }
                }
                if px < self.cfg.band_lo || px >= self.cfg.band_hi || !inst.valid_price(px) {
                    return self.cancel_live(&b.instrument);
                }
                let Some(p_true) = self.cfg.calibration.p_true(px) else {
                    return self.cancel_live(&b.instrument);
                };
                let edge = px.as_f64() - p_true; // EV/contract of the NO side, maker fee = 0
                if edge < self.cfg.min_edge {
                    return self.cancel_live(&b.instrument);
                }
                // Selling YES at `px` == buying NO at (1-px) with win prob (1-p_true).
                let want = sizing::kelly_contracts_with_fee(
                    1.0 - p_true,
                    1.0 - px.as_f64(),
                    0.0,
                    ctx.equity().as_f64(),
                    kf,
                    self.cfg.max_bet_fraction * scale,
                )
                .floor() as i64;
                let want = want.min(self.cfg.max_units);
                let held_short = (-ctx.pos(&b.instrument).0).max(0) / SCALE;
                let units = want - held_short;
                if units <= 0 {
                    return self.cancel_live(&b.instrument);
                }
                if let Some((_, lpx)) = self.live.get(&b.instrument) {
                    if *lpx == px {
                        return vec![]; // already quoting this level
                    }
                }
                let order = OrderReq {
                    client_id: id_gen(&self.cfg.tag, &mut self.n),
                    instrument: b.instrument.clone(),
                    side: Side::Sell,
                    px,
                    qty: Qty::units(units),
                    tif: Tif::Gtc,
                    post_only: true,
                    tag: self.cfg.tag.clone(),
                };
                let prev = self
                    .live
                    .insert(b.instrument.clone(), (order.client_id.clone(), px));
                match prev {
                    Some((old, _)) => vec![Action::Replace {
                        cancel: old,
                        place: order,
                    }],
                    None => vec![Action::Place(order)],
                }
            }
            _ => vec![],
        }
    }
}

impl LongshotFade {
    fn cancel_live(&mut self, id: &InstrumentId) -> Vec<Action> {
        match self.live.remove(id) {
            Some((cid, _)) => vec![Action::Cancel(cid)],
            None => vec![],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fees {
    Kalshi,
    Zero,
}

#[derive(Clone, Debug)]
pub struct EdgeConfig {
    /// Only act on signals from this source (None = any).
    pub source: Option<String>,
    pub theta: f64,
    /// Benter blend `(alpha, beta)` fitted out-of-sample in `research/`. None = trust the model as-is.
    pub benter: Option<(f64, f64)>,
    pub fees: Fees,
    pub kelly_frac: f64,
    pub max_bet_fraction: f64,
    pub max_units: i64,
    pub kill_min_results: u64,
    pub reject_backoff_ns: i64,
    pub tag: String,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        EdgeConfig {
            source: None,
            theta: 0.15,
            benter: None,
            fees: Fees::Kalshi,
            kelly_frac: 0.25,
            max_bet_fraction: 0.015,
            max_units: 1_000,
            kill_min_results: 50,
            reject_backoff_ns: 60_000_000_000,
            tag: "signal_edge".into(),
        }
    }
}

pub struct SignalEdge {
    cfg: EdgeConfig,
    live: HashMap<InstrumentId, (String, Px)>,
    backoff: HashMap<InstrumentId, Ts>,
    n: u64,
}

impl SignalEdge {
    pub fn new(cfg: EdgeConfig) -> Self {
        SignalEdge {
            cfg,
            live: HashMap::new(),
            backoff: HashMap::new(),
            n: 0,
        }
    }
}

impl Strategy for SignalEdge {
    fn name(&self) -> &str {
        &self.cfg.tag
    }

    fn on_reject(&mut self, now: Ts, req: &OrderReq, _reason: &str) {
        self.live.remove(&req.instrument);
        self.backoff.insert(
            req.instrument.clone(),
            Ts(now.0 + self.cfg.reject_backoff_ns),
        );
    }

    fn on_event(&mut self, ctx: &Ctx, ev: &Event) -> Vec<Action> {
        match ev {
            Event::Settlement(s) => {
                self.live.remove(&s.instrument);
                vec![]
            }
            Event::Signal(sig) => {
                if let Some(src) = &self.cfg.source {
                    if &sig.source != src {
                        return vec![];
                    }
                }
                let id = &sig.instrument;
                let (Some(inst), Some(book)) = (ctx.inst(id), ctx.book(id)) else {
                    return vec![];
                };
                if self.backoff.get(id).map_or(false, |&t| ctx.now < t) {
                    return vec![];
                }
                let Some((kf, scale)) = evidence_gate(
                    ctx,
                    &self.cfg.tag,
                    self.cfg.kelly_frac,
                    self.cfg.kill_min_results,
                ) else {
                    return self.cancel(id);
                };
                if let Some((cid, _)) = self.live.get(id) {
                    if !ctx.open_orders.iter().any(|o| &o.client_id == cid) {
                        self.live.remove(id);
                    }
                }
                let Some(ask) = book.best_ask() else {
                    return self.cancel(id);
                };
                let mid = book
                    .mid()
                    .map(|m| m.as_f64())
                    .unwrap_or_else(|| ask.px.as_f64());
                // calibrated probability: Benter blend of model and market, if configured
                let p = match self.cfg.benter {
                    Some((a, b)) => sizing::benter_combine_binary(
                        sig.prob.get(),
                        mid.clamp(1e-6, 1.0 - 1e-6),
                        a,
                        b,
                    ),
                    None => sig.prob.get(),
                };
                // maker bid θ below fair value, never at/through the ask
                let cents = sizing::limit_price_cents(p, self.cfg.theta);
                if cents <= 0 {
                    return self.cancel(id);
                }
                let mut px = Px::cents(cents);
                if px >= ask.px {
                    px = Px(ask.px.0 - inst.tick.0);
                }
                if px.0 <= 0 || !inst.valid_price(px) {
                    return self.cancel(id);
                }
                let fee = match self.cfg.fees {
                    Fees::Kalshi => 0.0, // maker fill: no taker fee (maker_rate modelled in the instrument)
                    Fees::Zero => 0.0,
                };
                let edge = p - px.as_f64() - fee;
                if edge < self.cfg.theta - 1e-9 && px.0 == Px::cents(cents).0 {
                    return self.cancel(id);
                }
                let want = sizing::kelly_contracts_with_fee(
                    p,
                    px.as_f64(),
                    fee,
                    ctx.equity().as_f64(),
                    kf,
                    self.cfg.max_bet_fraction * scale,
                )
                .floor() as i64;
                let want = want.min(self.cfg.max_units);
                let held = ctx.pos(id).0.max(0) / SCALE;
                let units = want - held;
                if units <= 0 {
                    return self.cancel(id);
                }
                if let Some((_, lpx)) = self.live.get(id) {
                    if *lpx == px {
                        return vec![];
                    }
                }
                let order = OrderReq {
                    client_id: id_gen(&self.cfg.tag, &mut self.n),
                    instrument: id.clone(),
                    side: Side::Buy,
                    px,
                    qty: Qty::units(units),
                    tif: Tif::Gtc,
                    post_only: true,
                    tag: self.cfg.tag.clone(),
                };
                let prev = self.live.insert(id.clone(), (order.client_id.clone(), px));
                match prev {
                    Some((old, _)) => vec![Action::Replace {
                        cancel: old,
                        place: order,
                    }],
                    None => vec![Action::Place(order)],
                }
            }
            _ => vec![],
        }
    }
}

impl SignalEdge {
    fn cancel(&mut self, id: &InstrumentId) -> Vec<Action> {
        match self.live.remove(id) {
            Some((cid, _)) => vec![Action::Cancel(cid)],
            None => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap};

    fn lvl(c: i64, q: i64) -> Level {
        Level {
            px: Px::cents(c),
            qty: Qty::units(q),
        }
    }

    struct World {
        pf: tc_ledger::Portfolio,
        marks: BTreeMap<InstrumentId, Px>,
        books: HashMap<InstrumentId, Book>,
        insts: HashMap<InstrumentId, Instrument>,
        open: Vec<OrderReq>,
    }
    fn world() -> World {
        let id = InstrumentId::new("M:YES");
        World {
            pf: tc_ledger::Portfolio::new(Money::dollars(10_000)),
            marks: BTreeMap::new(),
            books: HashMap::new(),
            insts: [(id, Instrument::binary("M:YES", "M"))].into(),
            open: vec![],
        }
    }
    fn ctx(w: &World) -> Ctx<'_> {
        Ctx {
            now: Ts(1),
            portfolio: &w.pf,
            marks: &w.marks,
            books: &w.books,
            instruments: &w.insts,
            open_orders: &w.open,
            size_scale: 1.0,
        }
    }
    fn book(bid: i64, ask: i64) -> Book {
        Book {
            instrument: InstrumentId::new("M:YES"),
            ts: Ts(1),
            bids: vec![lvl(bid, 100)],
            asks: vec![lvl(ask, 100)],
        }
    }

    #[test]
    fn fade_quotes_postonly_sell_in_band() {
        let w = world();
        let mut s = LongshotFade::new(FadeConfig::default(), None);
        let acts = s.on_event(&ctx(&w), &Event::Book(book(8, 10)));
        let Action::Place(o) = &acts[0] else {
            panic!("{}", acts.len())
        };
        assert_eq!(
            (o.side, o.px, o.post_only, o.tif),
            (Side::Sell, Px::cents(10), true, Tif::Gtc)
        );
        assert!(o.qty.0 > 0);
    }

    #[test]
    fn fade_ignores_out_of_band() {
        let w = world();
        let mut s = LongshotFade::new(FadeConfig::default(), None);
        assert!(s.on_event(&ctx(&w), &Event::Book(book(30, 32))).is_empty()); // favourite, not a longshot
        assert!(s.on_event(&ctx(&w), &Event::Book(book(1, 2))).is_empty()); // too thin below band
    }

    #[test]
    fn fade_requotes_with_truthful_replace() {
        let mut w = world();
        let mut s = LongshotFade::new(FadeConfig::default(), None);
        let a = s.on_event(&ctx(&w), &Event::Book(book(8, 10)));
        let Action::Place(first) = &a[0] else {
            panic!()
        };
        w.open.push(first.clone());
        let b = s.on_event(&ctx(&w), &Event::Book(book(9, 11)));
        assert!(matches!(b[0], Action::Replace { .. }));
        // same level again → no churn
        let Action::Replace { place, .. } = &b[0] else {
            panic!()
        };
        w.open = vec![place.clone()];
        assert!(s.on_event(&ctx(&w), &Event::Book(book(9, 11))).is_empty());
    }

    #[test]
    fn signal_edge_posts_bid_theta_below_fair_value() {
        let mut w = world();
        w.books.insert(InstrumentId::new("M:YES"), book(18, 20));
        let mut s = SignalEdge::new(EdgeConfig {
            theta: 0.15,
            ..Default::default()
        });
        let sig = Event::Signal(Signal {
            ts: Ts(1),
            source: "m".into(),
            instrument: InstrumentId::new("M:YES"),
            prob: Prob::new(0.30).unwrap(),
            confidence: None,
        });
        let acts = s.on_event(&ctx(&w), &sig);
        let Action::Place(o) = &acts[0] else { panic!() };
        // limit_price_cents(0.30, 0.15) = floor(15.0) = 15 (float: 0.15000000000000002 → 15)
        assert_eq!((o.side, o.px), (Side::Buy, Px::cents(15)));
        assert!(o.post_only);
    }

    #[test]
    fn signal_edge_never_crosses_ask() {
        let mut w = world();
        w.books.insert(InstrumentId::new("M:YES"), book(8, 10));
        let mut s = SignalEdge::new(EdgeConfig {
            theta: 0.05,
            ..Default::default()
        });
        let sig = Event::Signal(Signal {
            ts: Ts(1),
            source: "m".into(),
            instrument: InstrumentId::new("M:YES"),
            prob: Prob::new(0.60).unwrap(), // fair 60¢ → bid 55¢ would cross a 10¢ ask
            confidence: None,
        });
        let acts = s.on_event(&ctx(&w), &sig);
        let Action::Place(o) = &acts[0] else { panic!() };
        assert!(o.px < Px::cents(10));
    }

    #[test]
    fn fade_stands_down_at_kill_line_and_scales_with_risk() {
        let mut w = world();
        let tag = "longshot_fade".to_string();
        // 60 realisations, net negative → pre-declared kill line → no quotes, live order cancelled
        w.pf.cohort.insert(
            tag.clone(),
            tc_ledger::CohortStats {
                results: 60,
                wins: 20,
            },
        );
        w.pf.realized_by_tag.insert(tag.clone(), Money(-1));
        let mut s = LongshotFade::new(FadeConfig::default(), None);
        assert!(s.on_event(&ctx(&w), &Event::Book(book(8, 10))).is_empty());
        // same history but profitable → trades
        w.pf.realized_by_tag.insert(tag.clone(), Money::dollars(50));
        let mut s = LongshotFade::new(FadeConfig::default(), None);
        assert!(!s.on_event(&ctx(&w), &Event::Book(book(8, 10))).is_empty());
    }

    #[test]
    fn fade_size_follows_kelly_schedule_and_risk_scale() {
        let units = |results: u64, scale: f64| -> i64 {
            let mut w = world();
            w.pf.cohort.insert(
                "longshot_fade".into(),
                tc_ledger::CohortStats {
                    results,
                    wins: results,
                },
            );
            let mut c = ctx(&w);
            c.size_scale = scale;
            let mut s = LongshotFade::new(FadeConfig::default(), None);
            match s.on_event(&c, &Event::Book(book(8, 10))).pop() {
                Some(Action::Place(o)) => o.qty.0 / SCALE,
                _ => 0,
            }
        };
        let full = units(200, 1.0);
        assert!(full > 0);
        assert!(units(200, 0.5) < full, "drawdown scale must shrink size");
        assert_eq!(units(200, 0.0), 0, "scale 0 ⇒ stand down");
        // with the 1.5% bankroll cap not binding, the evidence schedule also shrinks early sizing
        let mut cfg = FadeConfig::default();
        cfg.max_bet_fraction = 1.0;
        cfg.max_units = 1_000_000;
        cfg.kelly_frac = 0.5; // the configured cap; the evidence schedule climbs toward it
        let sized = |results: u64| {
            let mut w = world();
            w.pf.cohort.insert(
                "longshot_fade".into(),
                tc_ledger::CohortStats {
                    results,
                    wins: results,
                },
            );
            let mut s = LongshotFade::new(cfg.clone(), None);
            match s.on_event(&ctx(&w), &Event::Book(book(8, 10))).pop() {
                Some(Action::Place(o)) => o.qty.0 / SCALE,
                _ => 0,
            }
        };
        assert!(
            sized(0) < sized(50) && sized(50) < sized(150),
            "{} {} {}",
            sized(0),
            sized(50),
            sized(150)
        );
    }
}
