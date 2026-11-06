//! tc-risk: the gate every order passes through. No code path reaches a `Venue` without `RiskGate::check`.
//!
//! Rails (all from the live-ops doctrine in `claude-trading-skills`):
//! * halt file / breaker state blocks all new risk (risk-*reducing* orders are still allowed so a halt can
//!   never trap us in a position),
//! * independent drawdown breaker from peak equity, equity floor,
//! * per-instrument / per-event-group / total exposure caps measured as **worst-case loss** (binary shorts
//!   cost `1-px`, not `px`),
//! * fat-finger limits (max qty, price band vs mark), order-rate throttle,
//! * `post_only` is *mandatory* for maker-tagged strategies — a "maker" order that crosses must never
//!   silently become an unvalidated taker trade.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use tc_core::*;
use tc_ledger::Portfolio;

#[derive(Clone, Debug)]
pub struct RiskLimits {
    /// Worst-case loss on one instrument as a fraction of equity (skills default: 1.5%).
    pub max_instrument_frac: f64,
    /// Worst-case loss across one correlation group / event (default 5%).
    pub max_group_frac: f64,
    /// Worst-case loss across all open risk (default 25%).
    pub max_total_frac: f64,
    /// Hard cap on a single order's quantity (fat-finger).
    pub max_order_qty: Qty,
    /// Reject a Linear-instrument order priced more than this fraction away from the mark.
    pub price_band_frac: f64,
    /// Trip the breaker (full stop, manual resume) when equity falls this fraction below its peak.
    /// Skill default: 15% (conservative account).
    pub max_drawdown_frac: f64,
    /// Graduated response BEFORE the hard stop: `(drawdown_from_peak, size_multiplier)`, ascending.
    /// risk-management: 5–10% → cut size 25–50%; 10–15% → minimum size.
    pub dd_ladder: Vec<(f64, f64)>,
    /// No new risk for the rest of the UTC day once the day's loss reaches this fraction (skill: 3–5%).
    pub daily_loss_frac: f64,
    /// Weekly loss: ≥ reduce → 0.5× size; ≥ min → 0.25× size; ≥ halt → no new risk this week.
    pub weekly_reduce_frac: f64,
    pub weekly_min_frac: f64,
    pub weekly_halt_frac: f64,
    /// Consecutive losing results: ≥ `streak_half` → 0.5× size, ≥ `streak_min` → 0.25×, ≥ `streak_halt` → no new
    /// risk for `streak_halt_ns` (skill: 3 / 5 / 7 losses, 24 h).
    pub streak_half: u32,
    pub streak_min: u32,
    pub streak_halt: u32,
    pub streak_halt_ns: i64,
    /// Stand down entirely below this equity.
    pub equity_floor: Money,
    /// Max orders accepted per `rate_window_ns`.
    pub max_orders_per_window: usize,
    pub rate_window_ns: i64,
    /// Orders carrying one of these tags must be `post_only`.
    pub post_only_tags: HashSet<String>,
}

impl Default for RiskLimits {
    fn default() -> Self {
        RiskLimits {
            max_instrument_frac: 0.015,
            max_group_frac: 0.05,
            max_total_frac: 0.25,
            max_order_qty: Qty::units(10_000),
            price_band_frac: 0.20,
            max_drawdown_frac: 0.15,
            dd_ladder: vec![(0.05, 0.75), (0.075, 0.50), (0.10, 0.25)],
            daily_loss_frac: 0.04,
            weekly_reduce_frac: 0.05,
            weekly_min_frac: 0.07,
            weekly_halt_frac: 0.10,
            streak_half: 3,
            streak_min: 5,
            streak_halt: 7,
            streak_halt_ns: 24 * 3_600 * 1_000_000_000,
            equity_floor: Money::ZERO,
            max_orders_per_window: 600,
            rate_window_ns: 60_000_000_000,
            post_only_tags: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RiskReject {
    Halted(String),
    EquityFloor,
    NonPositiveEquity,
    OrderTooLarge,
    PriceBand,
    InstrumentCap,
    GroupCap,
    TotalCap,
    RateLimit,
    PostOnlyRequired,
    UnknownInstrument,
    DailyLossLimit,
    WeeklyLossLimit,
    LossStreakCooldown,
}

impl std::fmt::Display for RiskReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for RiskReject {}
impl RiskReject {
    /// Stable short name for counters (drops the payload of `Halted`).
    pub fn key(&self) -> &'static str {
        match self {
            RiskReject::Halted(_) => "halted",
            RiskReject::EquityFloor => "equity_floor",
            RiskReject::NonPositiveEquity => "non_positive_equity",
            RiskReject::OrderTooLarge => "order_too_large",
            RiskReject::PriceBand => "price_band",
            RiskReject::InstrumentCap => "instrument_cap",
            RiskReject::GroupCap => "group_cap",
            RiskReject::TotalCap => "total_cap",
            RiskReject::RateLimit => "rate_limit",
            RiskReject::PostOnlyRequired => "post_only_required",
            RiskReject::UnknownInstrument => "unknown_instrument",
            RiskReject::DailyLossLimit => "daily_loss_limit",
            RiskReject::WeeklyLossLimit => "weekly_loss_limit",
            RiskReject::LossStreakCooldown => "loss_streak_cooldown",
        }
    }
}

/// Read-only view of the world the gate needs.
pub struct RiskView<'a> {
    pub portfolio: &'a Portfolio,
    pub marks: &'a BTreeMap<InstrumentId, Px>,
    pub open_orders: &'a [OrderReq],
    pub instruments: &'a HashMap<InstrumentId, Instrument>,
}

pub struct RiskGate {
    pub limits: RiskLimits,
    peak_equity: Money,
    halted: Option<String>,
    halt_file: Option<PathBuf>,
    recent: VecDeque<Ts>,
    latest_equity: Money,
    latest_ts: Ts,
    day: i64,
    day_start_equity: Money,
    week: i64,
    week_start_equity: Money,
    consec_losses: u32,
    streak_block_until: Option<Ts>,
}

const DAY_NS: i64 = 86_400 * 1_000_000_000;

impl RiskGate {
    pub fn new(limits: RiskLimits, start_equity: Money) -> Self {
        RiskGate {
            limits,
            peak_equity: start_equity,
            halted: None,
            halt_file: None,
            recent: VecDeque::new(),
            latest_equity: start_equity,
            latest_ts: Ts(0),
            day: i64::MIN,
            day_start_equity: start_equity,
            week: i64::MIN,
            week_start_equity: start_equity,
            consec_losses: 0,
            streak_block_until: None,
        }
    }

    /// If this file exists, no new risk is accepted (operator kill switch: `touch HALT`).
    pub fn with_halt_file(mut self, p: impl Into<PathBuf>) -> Self {
        self.halt_file = Some(p.into());
        self
    }

    pub fn halted(&self) -> Option<&str> {
        self.halted.as_deref()
    }

    /// Re-baseline after a deposit/withdrawal so it is not mistaken for PnL or drawdown.
    pub fn rebase(&mut self, equity: Money) {
        self.peak_equity = equity;
    }

    /// Feed a realised trade result (closing fill or settlement PnL). Drives the loss-streak breaker.
    pub fn on_trade_result(&mut self, ts: Ts, pnl: Money) {
        if pnl.0 < 0 {
            self.consec_losses += 1;
            if self.consec_losses >= self.limits.streak_halt {
                self.streak_block_until = Some(Ts(ts.0 + self.limits.streak_halt_ns));
            }
        } else if pnl.0 > 0 {
            self.consec_losses = 0;
        }
    }

    fn dd(&self) -> f64 {
        if self.peak_equity.0 <= 0 {
            return 0.0;
        }
        ((self.peak_equity.0 - self.latest_equity.0) as f64 / self.peak_equity.0 as f64).max(0.0)
    }

    fn loss_frac(start: Money, now: Money) -> f64 {
        if start.0 <= 0 {
            return 0.0;
        }
        ((start.0 - now.0) as f64 / start.0 as f64).max(0.0)
    }

    /// Multiplier (0..=1) strategies must apply to their sizes: the MOST conservative of the drawdown ladder, the
    /// weekly-loss ladder and the loss-streak ladder. 0.0 means "no new risk".
    pub fn size_scale(&self) -> f64 {
        // states in which `check` would refuse new risk → strategies must stand down instead of spamming rejects
        if self.halt_active().is_some()
            || Self::loss_frac(self.day_start_equity, self.latest_equity)
                >= self.limits.daily_loss_frac
            || self
                .streak_block_until
                .map_or(false, |t| self.latest_ts < t)
        {
            return 0.0;
        }
        let dd = self.dd();
        let mut s = 1.0f64;
        for &(thr, mult) in &self.limits.dd_ladder {
            if dd >= thr {
                s = s.min(mult);
            }
        }
        let wk = Self::loss_frac(self.week_start_equity, self.latest_equity);
        if wk >= self.limits.weekly_halt_frac {
            s = 0.0;
        } else if wk >= self.limits.weekly_min_frac {
            s = s.min(0.25);
        } else if wk >= self.limits.weekly_reduce_frac {
            s = s.min(0.5);
        }
        if self.consec_losses >= self.limits.streak_min {
            s = s.min(0.25);
        } else if self.consec_losses >= self.limits.streak_half {
            s = s.min(0.5);
        }
        s
    }

    /// Call after every equity change with the event time. Rolls the UTC day/week baselines and returns
    /// `Some(reason)` the moment the drawdown breaker trips.
    pub fn on_equity(&mut self, ts: Ts, equity: Money) -> Option<String> {
        let d = ts.0.div_euclid(DAY_NS);
        if d != self.day {
            self.day = d;
            // the day's baseline is the equity we carried in from the previous event
            self.day_start_equity = self.latest_equity;
        }
        let w = d.div_euclid(7);
        if w != self.week {
            self.week = w;
            self.week_start_equity = self.latest_equity;
        }
        self.latest_equity = equity;
        self.latest_ts = ts;
        if equity > self.peak_equity {
            self.peak_equity = equity;
        }
        if self.halted.is_none() && self.peak_equity.0 > 0 {
            let dd = (self.peak_equity.0 - equity.0) as f64 / self.peak_equity.0 as f64;
            if dd >= self.limits.max_drawdown_frac {
                let r = format!("drawdown breaker: {:.2}% from peak", dd * 100.0);
                self.halted = Some(r.clone());
                return Some(r);
            }
        }
        if self.halted.is_none() && equity < self.limits.equity_floor {
            let r = "equity below floor".to_string();
            self.halted = Some(r.clone());
            return Some(r);
        }
        None
    }

    pub fn manual_halt(&mut self, reason: &str) {
        self.halted = Some(reason.to_string());
    }

    /// Only an explicit operator action clears a halt — the engine never self-resumes.
    pub fn resume(&mut self) {
        self.halted = None;
    }

    /// Current halt reason (breaker/manual flag or halt file), if any.
    pub fn halt_reason(&self) -> Option<String> {
        self.halt_active()
    }

    fn halt_active(&self) -> Option<String> {
        if let Some(r) = &self.halted {
            return Some(r.clone());
        }
        if let Some(p) = &self.halt_file {
            if p.exists() {
                return Some(format!("halt file present: {}", p.display()));
            }
        }
        None
    }

    /// Worst-case-loss exposure of one instrument, with an extra hypothetical order folded in.
    fn instrument_exposure(
        &self,
        v: &RiskView,
        id: &InstrumentId,
        extra: Option<&OrderReq>,
    ) -> Money {
        let inst = &v.instruments[id];
        let pos = v.portfolio.positions.get(id);
        let pos_qty = pos.map(|p| p.qty).unwrap_or(Qty::ZERO);
        let ref_px = extra
            .map(|o| o.px)
            .or_else(|| v.marks.get(id).copied())
            .or_else(|| pos.map(|p| p.avg_px))
            .unwrap_or(Px::ZERO);
        let (mut buys, mut sells) = (Qty::ZERO, Qty::ZERO);
        for o in v.open_orders.iter().chain(extra) {
            if &o.instrument == id {
                match o.side {
                    Side::Buy => buys += o.qty,
                    Side::Sell => sells += o.qty,
                }
            }
        }
        let worst_long = pos_qty + buys;
        let worst_short = pos_qty - sells;
        let a = inst.max_loss(worst_long, ref_px);
        let b = inst.max_loss(worst_short, ref_px);
        if a > b {
            a
        } else {
            b
        }
    }

    pub fn check(&mut self, now: Ts, req: &OrderReq, v: &RiskView) -> Result<(), RiskReject> {
        let inst = v
            .instruments
            .get(&req.instrument)
            .ok_or(RiskReject::UnknownInstrument)?;

        // ---- hygiene rails first
        if self.limits.post_only_tags.contains(&req.tag) && !req.post_only {
            return Err(RiskReject::PostOnlyRequired);
        }
        if req.qty > self.limits.max_order_qty {
            return Err(RiskReject::OrderTooLarge);
        }
        if inst.kind == Kind::Linear {
            if let Some(m) = v.marks.get(&req.instrument) {
                let dev = (req.px.0 - m.0).abs() as f64 / m.0.max(1) as f64;
                if dev > self.limits.price_band_frac {
                    return Err(RiskReject::PriceBand);
                }
            }
        }
        // ---- throttle
        while let Some(t) = self.recent.front() {
            if now.0 - t.0 > self.limits.rate_window_ns {
                self.recent.pop_front();
            } else {
                break;
            }
        }
        if self.recent.len() >= self.limits.max_orders_per_window {
            return Err(RiskReject::RateLimit);
        }

        // ---- stand-downs (new risk only)
        // ---- is this order risk-reducing? (closes part/all of a position, never flips it)
        let pos = v.portfolio.position(&req.instrument);
        let signed = req.qty.0 * req.side.sign();
        let reducing = pos.0 != 0 && pos.signum() != signed.signum() && req.qty.0 <= pos.abs().0;

        let equity = v.portfolio.equity(v.marks);
        if !reducing {
            if let Some(why) = self.halt_active() {
                return Err(RiskReject::Halted(why));
            }
            if equity.0 <= 0 {
                return Err(RiskReject::NonPositiveEquity);
            }
            if Self::loss_frac(self.day_start_equity, equity) >= self.limits.daily_loss_frac {
                return Err(RiskReject::DailyLossLimit);
            }
            if Self::loss_frac(self.week_start_equity, equity) >= self.limits.weekly_halt_frac {
                return Err(RiskReject::WeeklyLossLimit);
            }
            if self.streak_block_until.map_or(false, |t| now < t) {
                return Err(RiskReject::LossStreakCooldown);
            }
            if equity < self.limits.equity_floor {
                return Err(RiskReject::EquityFloor);
            }
            // ---- exposure caps (worst-case loss, including resting orders and this one)
            let inst_exp = self.instrument_exposure(v, &req.instrument, Some(req));
            if inst_exp > equity.frac(self.limits.max_instrument_frac) {
                return Err(RiskReject::InstrumentCap);
            }
            let mut group_exp = Money::ZERO;
            let mut total_exp = Money::ZERO;
            let mut ids: Vec<&InstrumentId> = v.portfolio.positions.keys().collect();
            ids.extend(v.open_orders.iter().map(|o| &o.instrument));
            ids.push(&req.instrument);
            ids.sort();
            ids.dedup();
            for id in ids {
                let Some(i2) = v.instruments.get(id) else {
                    continue;
                };
                let e = if id == &req.instrument {
                    inst_exp
                } else {
                    self.instrument_exposure(v, id, None)
                };
                total_exp += e;
                if i2.group == inst.group {
                    group_exp += e;
                }
            }
            if group_exp > equity.frac(self.limits.max_group_frac) {
                return Err(RiskReject::GroupCap);
            }
            if total_exp > equity.frac(self.limits.max_total_frac) {
                return Err(RiskReject::TotalCap);
            }
        }
        self.recent.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct W {
        pf: Portfolio,
        marks: BTreeMap<InstrumentId, Px>,
        open: Vec<OrderReq>,
        insts: HashMap<InstrumentId, Instrument>,
    }
    fn world() -> W {
        let mut insts = HashMap::new();
        for (id, g) in [
            ("A:YES", "A"),
            ("A2:YES", "A"),
            ("A3:YES", "A"),
            ("A4:YES", "A"),
            ("B:YES", "B"),
        ] {
            insts.insert(InstrumentId::new(id), Instrument::binary(id, g));
        }
        W {
            pf: Portfolio::new(Money::dollars(10_000)),
            marks: BTreeMap::new(),
            open: vec![],
            insts,
        }
    }
    fn ord(inst: &str, side: Side, c: i64, q: i64) -> OrderReq {
        OrderReq {
            client_id: format!("{inst}{c}{q}"),
            instrument: InstrumentId::new(inst),
            side,
            px: Px::cents(c),
            qty: Qty::units(q),
            tif: Tif::Gtc,
            post_only: true,
            tag: "mm".into(),
        }
    }
    fn chk(g: &mut RiskGate, w: &W, o: &OrderReq) -> Result<(), RiskReject> {
        let v = RiskView {
            portfolio: &w.pf,
            marks: &w.marks,
            open_orders: &w.open,
            instruments: &w.insts,
        };
        g.check(Ts(0), o, &v)
    }

    #[test]
    fn instrument_cap_is_worst_case_loss() {
        let w = world();
        let mut g = RiskGate::new(RiskLimits::default(), Money::dollars(10_000));
        // equity 10k → 1.5% = $150. Buying 700 YES @ 20¢ risks $140 → ok
        assert!(chk(&mut g, &w, &ord("A:YES", Side::Buy, 20, 700)).is_ok());
        // selling 700 YES @ 20¢ risks (1-0.20)*700 = $560 → rejected (the short costs 1-px)
        assert_eq!(
            chk(&mut g, &w, &ord("A:YES", Side::Sell, 20, 700)),
            Err(RiskReject::InstrumentCap)
        );
    }

    #[test]
    fn resting_orders_count_toward_caps() {
        let mut w = world();
        let mut g = RiskGate::new(RiskLimits::default(), Money::dollars(10_000));
        w.open.push(ord("A:YES", Side::Buy, 20, 700)); // $140 resting
        assert_eq!(
            chk(&mut g, &w, &ord("A:YES", Side::Buy, 20, 100)),
            Err(RiskReject::InstrumentCap)
        );
    }

    #[test]
    fn group_cap_across_brackets() {
        let mut w = world();
        let mut g = RiskGate::new(RiskLimits::default(), Money::dollars(10_000));
        // group "A" cap = 5% = $500. Three brackets already hold $140 each (700 @ 20¢) = $420.
        for id in ["A2:YES", "A3:YES", "A4:YES"] {
            w.pf.positions.insert(
                InstrumentId::new(id),
                tc_ledger::Position::new(Qty::units(700), Px::cents(20)),
            );
        }
        // a fourth $140 bracket (inside the $150 instrument cap) would take the group to $560 > $500
        assert_eq!(
            chk(&mut g, &w, &ord("A:YES", Side::Buy, 20, 700)),
            Err(RiskReject::GroupCap)
        );
        // a smaller order that keeps the group ≤ $500 passes ($80 → $500)
        assert!(chk(&mut g, &w, &ord("A:YES", Side::Buy, 20, 400)).is_ok());
        // a different event is unaffected
        assert!(chk(&mut g, &w, &ord("B:YES", Side::Buy, 20, 700)).is_ok());
    }

    #[test]
    fn maker_tag_must_be_post_only() {
        let w = world();
        let mut l = RiskLimits::default();
        l.post_only_tags.insert("mm".into());
        let mut g = RiskGate::new(l, Money::dollars(10_000));
        let mut o = ord("A:YES", Side::Buy, 20, 10);
        o.post_only = false;
        assert_eq!(chk(&mut g, &w, &o), Err(RiskReject::PostOnlyRequired));
    }

    #[test]
    fn breaker_blocks_new_risk_but_allows_reducing() {
        let mut w = world();
        let mut g = RiskGate::new(RiskLimits::default(), Money::dollars(10_000));
        w.pf.positions.insert(
            InstrumentId::new("A:YES"),
            tc_ledger::Position::new(Qty::units(100), Px::cents(20)),
        );
        assert!(g.on_equity(Ts(0), Money::dollars(8_000)).is_some()); // -20% > 15%
                                                                      // new risk blocked
        assert!(matches!(
            chk(&mut g, &w, &ord("B:YES", Side::Buy, 20, 10)),
            Err(RiskReject::Halted(_))
        ));
        // reducing sell allowed
        let mut sell = ord("A:YES", Side::Sell, 25, 100);
        sell.client_id = "close".into();
        assert!(chk(&mut g, &w, &sell).is_ok());
        // flipping sell is NOT reducing
        let flip = ord("A:YES", Side::Sell, 25, 150);
        assert!(matches!(chk(&mut g, &w, &flip), Err(RiskReject::Halted(_))));
    }

    #[test]
    fn halt_file_blocks() {
        let w = world();
        let p = std::env::temp_dir().join("tc_halt_test_file");
        std::fs::write(&p, b"x").unwrap();
        let mut g = RiskGate::new(RiskLimits::default(), Money::dollars(10_000)).with_halt_file(&p);
        assert!(matches!(
            chk(&mut g, &w, &ord("A:YES", Side::Buy, 20, 10)),
            Err(RiskReject::Halted(_))
        ));
        std::fs::remove_file(&p).unwrap();
        assert!(chk(&mut g, &w, &ord("A:YES", Side::Buy, 20, 10)).is_ok());
    }

    #[test]
    fn rate_limit() {
        let w = world();
        let mut l = RiskLimits::default();
        l.max_orders_per_window = 2;
        let mut g = RiskGate::new(l, Money::dollars(10_000));
        assert!(chk(&mut g, &w, &ord("A:YES", Side::Buy, 20, 1)).is_ok());
        assert!(chk(&mut g, &w, &ord("A:YES", Side::Buy, 21, 1)).is_ok());
        assert_eq!(
            chk(&mut g, &w, &ord("A:YES", Side::Buy, 22, 1)),
            Err(RiskReject::RateLimit)
        );
    }

    #[test]
    fn graduated_size_scale_follows_drawdown_ladder() {
        // isolate the drawdown ladder: a drawdown within one week is ALSO a weekly loss, and the gate always
        // applies the most conservative of all ladders (asserted at the end).
        let mut l = RiskLimits::default();
        l.weekly_reduce_frac = 9.0;
        l.weekly_min_frac = 9.0;
        l.weekly_halt_frac = 9.0;
        l.daily_loss_frac = 9.0; // isolate the drawdown ladder from the same-day stand-down
        let mut g = RiskGate::new(l, Money::dollars(10_000));
        assert_eq!(g.size_scale(), 1.0);
        g.on_equity(Ts(0), Money::dollars(9_400)); // -6%
        assert_eq!(g.size_scale(), 0.75);
        g.on_equity(Ts(1), Money::dollars(9_200)); // -8%
        assert_eq!(g.size_scale(), 0.50);
        g.on_equity(Ts(2), Money::dollars(8_900)); // -11%
        assert_eq!(g.size_scale(), 0.25);
        assert!(g.halted().is_none()); // not yet the hard stop (15%)
        g.on_equity(Ts(3), Money::dollars(8_400)); // -16%
        assert!(g.halted().is_some());
    }

    #[test]
    fn size_scale_is_the_most_conservative_of_all_ladders() {
        let mut l = RiskLimits::default();
        l.daily_loss_frac = 9.0;
        let mut g = RiskGate::new(l, Money::dollars(10_000));
        g.on_equity(Ts(0), Money::dollars(9_400)); // dd ladder says 0.75, weekly (-6%) says 0.5
        assert_eq!(g.size_scale(), 0.5);
    }

    #[test]
    fn loss_streak_ladder_and_cooldown() {
        let w = world();
        let mut g = RiskGate::new(RiskLimits::default(), Money::dollars(10_000));
        for i in 0..3 {
            g.on_trade_result(Ts(i), Money(-1));
        }
        assert_eq!(g.size_scale(), 0.5);
        for i in 3..5 {
            g.on_trade_result(Ts(i), Money(-1));
        }
        assert_eq!(g.size_scale(), 0.25);
        g.on_trade_result(Ts(10), Money(5)); // a win resets the streak
        assert_eq!(g.size_scale(), 1.0);
        for i in 0..7 {
            g.on_trade_result(Ts(100 + i), Money(-1));
        }
        g.on_equity(Ts(110), Money::dollars(10_000));
        assert_eq!(
            g.size_scale(),
            0.0,
            "strategies must see the cooldown and stand down"
        );
        assert_eq!(
            chk(&mut g, &w, &ord("A:YES", Side::Buy, 20, 10)),
            Err(RiskReject::LossStreakCooldown)
        );
        // 24h later it clears
        let v = RiskView {
            portfolio: &w.pf,
            marks: &w.marks,
            open_orders: &w.open,
            instruments: &w.insts,
        };
        assert!(g
            .check(
                Ts(100 + 24 * 3_600 * 1_000_000_000 + 10),
                &ord("A:YES", Side::Buy, 20, 10),
                &v
            )
            .is_ok());
    }

    #[test]
    fn daily_loss_limit_blocks_new_risk_until_next_utc_day() {
        let mut w = world();
        let mut g = RiskGate::new(RiskLimits::default(), Money::dollars(10_000));
        g.on_equity(Ts(1_000), Money::dollars(10_000)); // day baseline
        w.pf.cash = Money::dollars(9_500); // -5% intraday (> 4% limit, < 5% weekly-reduce edge case ignored)
        g.on_equity(Ts(2_000), Money::dollars(9_500));
        assert_eq!(
            chk(&mut g, &w, &ord("A:YES", Side::Buy, 20, 10)),
            Err(RiskReject::DailyLossLimit)
        );
        // next UTC day: baseline resets to the current equity
        let tomorrow = Ts(DAY_NS + 5);
        g.on_equity(tomorrow, Money::dollars(9_500));
        let v = RiskView {
            portfolio: &w.pf,
            marks: &w.marks,
            open_orders: &w.open,
            instruments: &w.insts,
        };
        assert!(g
            .check(tomorrow, &ord("A:YES", Side::Buy, 20, 10), &v)
            .is_ok());
    }

    #[test]
    fn weekly_ladder() {
        let mut l = RiskLimits::default();
        l.daily_loss_frac = 9.0;
        let mut g = RiskGate::new(l, Money::dollars(10_000));
        g.on_equity(Ts(0), Money::dollars(10_000));
        g.on_equity(Ts(1), Money::dollars(9_480)); // -5.2% week
        assert_eq!(g.size_scale(), 0.5);
        g.on_equity(Ts(2), Money::dollars(9_250)); // -7.5%
        assert_eq!(g.size_scale(), 0.25);
        g.on_equity(Ts(3), Money::dollars(8_990)); // -10.1%
        assert_eq!(g.size_scale(), 0.0);
    }

    #[test]
    fn same_day_loss_beyond_limit_makes_strategies_stand_down() {
        let mut g = RiskGate::new(RiskLimits::default(), Money::dollars(10_000));
        g.on_equity(Ts(0), Money::dollars(10_000));
        g.on_equity(Ts(1), Money::dollars(9_550)); // -4.5% on the day (limit 4%)
        assert_eq!(g.size_scale(), 0.0);
        g.on_equity(Ts(DAY_NS + 1), Money::dollars(9_550)); // next UTC day: baseline resets
        assert!(g.size_scale() > 0.0);
    }
}
