//! Bar-driven strategies ported from `quant-trading` (MACD, RSI, Bollinger, Dual Thrust, Heikin-Ashi,
//! Parabolic SAR). Unlike the originals — which are explicitly frictionless — these trade through the sim
//! venue, so fees, spread and slippage are in every result.
//!
//! A `BarSignal` maps a closed bar to a desired direction in {-1, 0, +1}; `BarStrategy` turns direction into
//! protective IOC limit orders sized in whole units.

use crate::indicators::*;
use crate::*;

pub trait BarSignal {
    fn name(&self) -> &'static str;
    /// Desired direction after this (closed) bar: +1 long, -1 short, 0 flat. `None` = still warming up.
    fn update(&mut self, bar: &Bar) -> Option<i8>;
}

pub struct MacdCross {
    macd: Macd,
}
impl MacdCross {
    pub fn new(fast: usize, slow: usize, signal: usize) -> Self {
        MacdCross {
            macd: Macd::new(fast, slow, signal),
        }
    }
}
impl BarSignal for MacdCross {
    fn name(&self) -> &'static str {
        "macd"
    }
    fn update(&mut self, b: &Bar) -> Option<i8> {
        let (m, s) = self.macd.update(b.close.as_f64())?;
        Some(if m > s { 1 } else { -1 })
    }
}

/// RSI mean reversion: enter long < `lo`, short > `hi`; exit when RSI re-crosses 50.
pub struct RsiReversion {
    rsi: Rsi,
    lo: f64,
    hi: f64,
    dir: i8,
}
impl RsiReversion {
    pub fn new(n: usize, lo: f64, hi: f64) -> Self {
        RsiReversion {
            rsi: Rsi::new(n),
            lo,
            hi,
            dir: 0,
        }
    }
}
impl BarSignal for RsiReversion {
    fn name(&self) -> &'static str {
        "rsi"
    }
    fn update(&mut self, b: &Bar) -> Option<i8> {
        let r = self.rsi.update(b.close.as_f64())?;
        match self.dir {
            0 if r < self.lo => self.dir = 1,
            0 if r > self.hi => self.dir = -1,
            1 if r >= 50.0 => self.dir = 0,
            -1 if r <= 50.0 => self.dir = 0,
            _ => {}
        }
        Some(self.dir)
    }
}

/// Bollinger-band reversion: long below lower band, short above upper band, flat on return to the mean.
pub struct BollingerReversion {
    w: Rolling,
    k: f64,
    dir: i8,
}
impl BollingerReversion {
    pub fn new(n: usize, k: f64) -> Self {
        BollingerReversion {
            w: Rolling::new(n),
            k,
            dir: 0,
        }
    }
}
impl BarSignal for BollingerReversion {
    fn name(&self) -> &'static str {
        "bollinger"
    }
    fn update(&mut self, b: &Bar) -> Option<i8> {
        let c = b.close.as_f64();
        self.w.push(c);
        if !self.w.full() {
            return None;
        }
        let (m, sd) = (self.w.mean(), self.w.std());
        match self.dir {
            0 if c < m - self.k * sd => self.dir = 1,
            0 if c > m + self.k * sd => self.dir = -1,
            1 if c >= m => self.dir = 0,
            -1 if c <= m => self.dir = 0,
            _ => {}
        }
        Some(self.dir)
    }
}

/// Dual Thrust opening-range breakout. `range = max(HH − LC, HC − LL)` over the previous `n` bars;
/// long if close > open + k1·range, short if close < open − k2·range (state is held otherwise).
pub struct DualThrust {
    n: usize,
    k1: f64,
    k2: f64,
    hist: std::collections::VecDeque<(f64, f64, f64)>, // (high, low, close)
    dir: i8,
}
impl DualThrust {
    pub fn new(n: usize, k1: f64, k2: f64) -> Self {
        DualThrust {
            n,
            k1,
            k2,
            hist: Default::default(),
            dir: 0,
        }
    }
}
impl BarSignal for DualThrust {
    fn name(&self) -> &'static str {
        "dual_thrust"
    }
    fn update(&mut self, b: &Bar) -> Option<i8> {
        let out = if self.hist.len() == self.n {
            let hh = self
                .hist
                .iter()
                .map(|x| x.0)
                .fold(f64::NEG_INFINITY, f64::max);
            let ll = self.hist.iter().map(|x| x.1).fold(f64::INFINITY, f64::min);
            let hc = self
                .hist
                .iter()
                .map(|x| x.2)
                .fold(f64::NEG_INFINITY, f64::max);
            let lc = self.hist.iter().map(|x| x.2).fold(f64::INFINITY, f64::min);
            let range = (hh - lc).max(hc - ll);
            let (o, c) = (b.open.as_f64(), b.close.as_f64());
            if c > o + self.k1 * range {
                self.dir = 1;
            } else if c < o - self.k2 * range {
                self.dir = -1;
            }
            Some(self.dir)
        } else {
            None
        };
        if self.hist.len() == self.n {
            self.hist.pop_front();
        }
        self.hist
            .push_back((b.high.as_f64(), b.low.as_f64(), b.close.as_f64()));
        out
    }
}

/// Heikin-Ashi trend: two consecutive same-colour HA candles set direction.
pub struct HeikinAshiTrend {
    ha: HeikinAshi,
    last_color: i8,
    dir: i8,
    started: bool,
}
impl HeikinAshiTrend {
    pub fn new() -> Self {
        HeikinAshiTrend {
            ha: HeikinAshi::default(),
            last_color: 0,
            dir: 0,
            started: false,
        }
    }
}
impl BarSignal for HeikinAshiTrend {
    fn name(&self) -> &'static str {
        "heikin_ashi"
    }
    fn update(&mut self, b: &Bar) -> Option<i8> {
        let (o, _h, _l, c) = self.ha.update(
            b.open.as_f64(),
            b.high.as_f64(),
            b.low.as_f64(),
            b.close.as_f64(),
        );
        let color: i8 = if c > o { 1 } else { -1 };
        if self.started && color == self.last_color {
            self.dir = color;
        }
        self.last_color = color;
        self.started = true;
        Some(self.dir)
    }
}

pub struct ParabolicSar {
    sar: Sar,
}
impl ParabolicSar {
    pub fn new(step: f64, max_af: f64) -> Self {
        ParabolicSar {
            sar: Sar::new(step, max_af),
        }
    }
}
impl BarSignal for ParabolicSar {
    fn name(&self) -> &'static str {
        "parabolic_sar"
    }
    fn update(&mut self, b: &Bar) -> Option<i8> {
        self.sar.update(b.high.as_f64(), b.low.as_f64())
    }
}

/// How many units to hold when the signal fires.
#[derive(Clone, Copy, Debug)]
pub enum Sizing {
    Fixed(i64),
    /// Fixed-fractional / volatility-adjusted (`position-sizing`): risk `pct` of equity per trade with the stop at
    /// `atr_mult × ATR` ⇒ `units = equity × pct × size_scale / (atr_mult × ATR)`. Shrinks automatically when vol rises.
    /// `max_notional_frac` caps position value as a fraction of equity (position-sizing ladder: the most binding
    /// constraint wins). Without it, a quiet market implies absurd leverage.
    RiskPct {
        pct: f64,
        atr_mult: f64,
        max_notional_frac: f64,
    },
}

/// Exit rules layered on top of the signal (`exit-strategies`). Priority: hard ATR stop > time stop > signal.
#[derive(Clone, Copy, Debug, Default)]
pub struct Exits {
    /// Hard stop at `entry ∓ mult × ATR(atr_len)`. Never moved away from price.
    pub atr_stop_mult: Option<f64>,
    /// Close a position that is not profitable after this many bars.
    pub max_bars_unprofitable: Option<usize>,
    /// After a stop-out, stay flat this many bars (prevents instantly re-buying the same falling knife).
    pub cooldown_bars: usize,
}

/// Trades one instrument toward `signal`'s direction with protective IOC limits, optional ATR/time exits, and
/// fixed or volatility-adjusted sizing.
pub struct BarStrategy<S: BarSignal> {
    pub instrument: InstrumentId,
    signal: S,
    sizing: Sizing,
    exits: Exits,
    long_only: bool,
    slippage_bps: f64,
    tag: String,
    n: u64,
    atr: Atr,
    entry: Option<(f64, i8)>, // (entry price, direction)
    bars_held: usize,
    cooldown_left: usize,
}

impl<S: BarSignal> BarStrategy<S> {
    pub fn new(
        instrument: InstrumentId,
        signal: S,
        units: i64,
        long_only: bool,
        slippage_bps: f64,
    ) -> Self {
        let tag =
            tc_core::markets::sanitize_client_id(&format!("{}-{}", signal.name(), instrument));
        BarStrategy {
            instrument,
            signal,
            sizing: Sizing::Fixed(units),
            exits: Exits::default(),
            long_only,
            slippage_bps,
            tag,
            n: 0,
            atr: Atr::new(14),
            entry: None,
            bars_held: 0,
            cooldown_left: 0,
        }
    }
    pub fn with_exits(mut self, e: Exits) -> Self {
        self.exits = e;
        self
    }
    pub fn with_sizing(mut self, s: Sizing) -> Self {
        self.sizing = s;
        self
    }
}

impl<S: BarSignal> Strategy for BarStrategy<S> {
    fn name(&self) -> &str {
        &self.tag
    }

    fn on_event(&mut self, ctx: &Ctx, ev: &Event) -> Vec<Action> {
        let Event::Bar(bar) = ev else { return vec![] };
        if bar.instrument != self.instrument {
            return vec![];
        }
        let close = bar.close.as_f64();
        let atr = self.atr.update(bar.high.as_f64(), bar.low.as_f64(), close);
        let Some(mut dir) = self.signal.update(bar) else {
            return vec![];
        };
        if self.long_only && dir < 0 {
            dir = 0;
        }
        let held = ctx.pos(&self.instrument);

        // ---- exits override the signal (hard stop first, then time stop)
        if held.0 != 0 {
            self.bars_held += 1;
            if let (Some((entry_px, edir)), Some(a)) = (self.entry, atr) {
                let pnl_per_unit = (close - entry_px) * edir as f64;
                if let Some(m) = self.exits.atr_stop_mult {
                    if pnl_per_unit <= -m * a {
                        dir = 0;
                        self.cooldown_left = self.exits.cooldown_bars;
                    }
                }
                if let Some(maxb) = self.exits.max_bars_unprofitable {
                    if dir != 0 && self.bars_held > maxb && pnl_per_unit <= 0.0 {
                        dir = 0;
                        self.cooldown_left = self.exits.cooldown_bars;
                    }
                }
            }
        } else {
            self.entry = None;
            self.bars_held = 0;
        }
        if self.cooldown_left > 0 {
            self.cooldown_left -= 1;
            if held.0 == 0 {
                dir = 0; // flat and cooling down: no new entries
            }
        }

        // ---- size
        let units = match self.sizing {
            Sizing::Fixed(u) => (u as f64 * ctx.size_scale).floor() as i64,
            Sizing::RiskPct {
                pct,
                atr_mult,
                max_notional_frac,
            } => match atr {
                Some(a) if a > 0.0 && close > 0.0 => {
                    let eq = ctx.equity().as_f64() * ctx.size_scale;
                    let by_risk = eq * pct / (atr_mult * a);
                    let by_notional = eq * max_notional_frac / close;
                    by_risk.min(by_notional).floor() as i64
                }
                _ => 0,
            },
        };
        // A position being closed does not need fresh size; only entries are scaled down.
        let target = if dir == 0 {
            Qty::ZERO
        } else {
            Qty::units(dir as i64 * units)
        };
        let delta = target - held;
        if delta.0 == 0 {
            return vec![];
        }
        let Some(inst) = ctx.inst(&self.instrument) else {
            return vec![];
        };
        let side = if delta.0 > 0 { Side::Buy } else { Side::Sell };
        if held.0 == 0 && dir != 0 {
            self.entry = Some((close, dir));
            self.bars_held = 0;
        }
        let slip = self.slippage_bps / 10_000.0;
        let raw = Px::from_f64(close * (1.0 + slip * side.sign() as f64));
        let px = snap(raw, inst.tick, side == Side::Buy);
        vec![Action::Place(OrderReq {
            client_id: id_gen(&self.tag, &mut self.n),
            instrument: self.instrument.clone(),
            side,
            px,
            qty: delta.abs(),
            tif: Tif::Ioc,
            post_only: false,
            tag: self.tag.clone(),
        })]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(i: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar {
            instrument: InstrumentId::new("X"),
            ts: Ts::from_secs(i * 60),
            open: Px::from_f64(o),
            high: Px::from_f64(h),
            low: Px::from_f64(l),
            close: Px::from_f64(c),
            volume: Qty::units(100),
        }
    }

    #[test]
    fn macd_goes_long_in_uptrend_and_short_in_downtrend() {
        let mut s = MacdCross::new(5, 12, 4);
        let mut last = None;
        for i in 0..60 {
            let p = 100.0 + i as f64;
            last = s.update(&bar(i, p, p + 1.0, p - 1.0, p));
        }
        assert_eq!(last, Some(1));
        for i in 60..140 {
            let p = 160.0 - (i - 60) as f64;
            last = s.update(&bar(i, p, p + 1.0, p - 1.0, p));
        }
        assert_eq!(last, Some(-1));
    }

    #[test]
    fn rsi_reversion_enters_on_selloff_exits_when_rsi_recrosses_50() {
        let mut s = RsiReversion::new(5, 30.0, 70.0);
        let mut dir = None;
        for i in 0..10 {
            dir = s.update(&bar(i, 100.0, 100.0, 100.0, 100.0 - i as f64 * 2.0));
        }
        assert_eq!(dir, Some(1)); // oversold → long
                                  // recover: must pass through flat (RSI crossing 50) before it can reach overbought and go short
        let mut seq = vec![];
        for i in 10..40 {
            dir = s.update(&bar(i, 100.0, 100.0, 100.0, 82.0 + (i - 10) as f64 * 2.0));
            seq.push(dir.unwrap());
        }
        let first_flat = seq
            .iter()
            .position(|&d| d == 0)
            .expect("must go flat on recovery");
        assert!(seq[..first_flat].iter().all(|&d| d == 1));
        assert_eq!(*seq.last().unwrap(), -1); // keeps rising → overbought → short
    }

    #[test]
    fn bollinger_shorts_spike() {
        let mut s = BollingerReversion::new(10, 2.0);
        let mut dir = None;
        for i in 0..10 {
            dir = s.update(&bar(i, 100.0, 100.0, 100.0, 100.0 + (i % 2) as f64 * 0.2));
        }
        assert_eq!(dir, Some(0));
        dir = s.update(&bar(11, 100.0, 100.0, 100.0, 108.0));
        assert_eq!(dir, Some(-1));
    }

    #[test]
    fn dual_thrust_breakout() {
        let mut s = DualThrust::new(3, 0.5, 0.5);
        let mut dir = None;
        for i in 0..4 {
            dir = s.update(&bar(i, 100.0, 101.0, 99.0, 100.0));
        }
        assert_eq!(dir, Some(0));
        dir = s.update(&bar(5, 100.0, 110.0, 100.0, 109.0));
        assert_eq!(dir, Some(1));
    }

    #[test]
    fn bar_strategy_sends_protective_ioc_toward_target() {
        use std::collections::{BTreeMap, HashMap};
        let id = InstrumentId::new("X");
        let inst = Instrument::linear("X", "X", Px(100));
        let insts: HashMap<_, _> = [(id.clone(), inst)].into();
        let pf = tc_ledger::Portfolio::new(Money::dollars(10_000));
        let marks = BTreeMap::new();
        let books = HashMap::new();
        let ctx = Ctx {
            now: Ts(0),
            portfolio: &pf,
            marks: &marks,
            books: &books,
            instruments: &insts,
            open_orders: &[],
            size_scale: 1.0,
        };
        struct AlwaysLong;
        impl BarSignal for AlwaysLong {
            fn name(&self) -> &'static str {
                "long"
            }
            fn update(&mut self, _: &Bar) -> Option<i8> {
                Some(1)
            }
        }
        let mut st = BarStrategy::new(id, AlwaysLong, 10, false, 10.0);
        let acts = st.on_event(&ctx, &Event::Bar(bar(1, 100.0, 100.0, 100.0, 100.0)));
        let Action::Place(o) = &acts[0] else { panic!() };
        assert_eq!(
            (o.side, o.qty, o.tif),
            (Side::Buy, Qty::units(10), Tif::Ioc)
        );
        assert!(o.px > Px::from_f64(100.0) && o.px <= Px::from_f64(100.2)); // 10 bps protective cap
    }

    fn ctx_with<'a>(
        pf: &'a tc_ledger::Portfolio,
        marks: &'a std::collections::BTreeMap<InstrumentId, Px>,
        books: &'a std::collections::HashMap<InstrumentId, Book>,
        insts: &'a std::collections::HashMap<InstrumentId, Instrument>,
        scale: f64,
    ) -> Ctx<'a> {
        Ctx {
            now: Ts(0),
            portfolio: pf,
            marks,
            books,
            instruments: insts,
            open_orders: &[],
            size_scale: scale,
        }
    }

    struct Always(i8);
    impl BarSignal for Always {
        fn name(&self) -> &'static str {
            "always"
        }
        fn update(&mut self, _: &Bar) -> Option<i8> {
            Some(self.0)
        }
    }

    #[test]
    fn atr_stop_forces_exit_and_cooldown_blocks_reentry() {
        use std::collections::{BTreeMap, HashMap};
        let id = InstrumentId::new("X");
        let insts: HashMap<_, _> = [(id.clone(), Instrument::linear("X", "X", Px(100)))].into();
        let (marks, books) = (BTreeMap::new(), HashMap::new());
        let mut pf = tc_ledger::Portfolio::new(Money::dollars(100_000));
        let mut st = BarStrategy::new(id.clone(), Always(1), 10, false, 0.0).with_exits(Exits {
            atr_stop_mult: Some(2.0),
            max_bars_unprofitable: None,
            cooldown_bars: 3,
        });
        // warm ATR on calm bars (range 1.0), enter long
        let mut entered = false;
        for i in 0..20 {
            let acts = st.on_event(
                &ctx_with(&pf, &marks, &books, &insts, 1.0),
                &Event::Bar(bar(i, 100.0, 100.5, 99.5, 100.0)),
            );
            if let Some(Action::Place(o)) = acts.first() {
                assert_eq!(o.side, Side::Buy);
                let f = Fill {
                    client_id: o.client_id.clone(),
                    instrument: id.clone(),
                    side: o.side,
                    px: o.px,
                    qty: o.qty,
                    fee: Money::ZERO,
                    liquidity: Liquidity::Taker,
                    ts: Ts(0),
                    tag: st.name().into(),
                };
                pf.apply_fill(&f);
                entered = true;
            }
        }
        assert!(entered && pf.position(&id) == Qty::units(10));
        // price collapses 5 ATRs → hard stop must flatten
        let acts = st.on_event(
            &ctx_with(&pf, &marks, &books, &insts, 1.0),
            &Event::Bar(bar(30, 100.0, 100.0, 94.0, 94.0)),
        );
        let Action::Place(o) = &acts[0] else {
            panic!("expected stop-out")
        };
        assert_eq!((o.side, o.qty), (Side::Sell, Qty::units(10)));
        let f = Fill {
            client_id: o.client_id.clone(),
            instrument: id.clone(),
            side: Side::Sell,
            px: o.px,
            qty: o.qty,
            fee: Money::ZERO,
            liquidity: Liquidity::Taker,
            ts: Ts(0),
            tag: st.name().into(),
        };
        pf.apply_fill(&f);
        // the always-long signal wants back in, but cooldown holds it flat
        for i in 31..33 {
            assert!(st
                .on_event(
                    &ctx_with(&pf, &marks, &books, &insts, 1.0),
                    &Event::Bar(bar(i, 94.0, 94.5, 93.5, 94.0))
                )
                .is_empty());
        }
    }

    #[test]
    fn notional_cap_binds_in_quiet_markets() {
        use std::collections::{BTreeMap, HashMap};
        let id = InstrumentId::new("X");
        let insts: HashMap<_, _> = [(id.clone(), Instrument::linear("X", "X", Px(100)))].into();
        let (marks, books) = (BTreeMap::new(), HashMap::new());
        let pf = tc_ledger::Portfolio::new(Money::dollars(100_000));
        let mut st =
            BarStrategy::new(id.clone(), Always(1), 10, false, 0.0).with_sizing(Sizing::RiskPct {
                pct: 0.01,
                atr_mult: 2.0,
                max_notional_frac: 0.25,
            });
        let mut units = 0;
        for i in 0..30 {
            // ATR ≈ 0.1 → risk-based size would be 5000 units (= $500k notional); cap = 25% of $100k = 250 units
            if let Some(Action::Place(o)) = st
                .on_event(
                    &ctx_with(&pf, &marks, &books, &insts, 1.0),
                    &Event::Bar(bar(i, 100.0, 100.05, 99.95, 100.0)),
                )
                .first()
            {
                units = o.qty.0 / SCALE;
                break;
            }
        }
        assert_eq!(units, 250);
    }

    #[test]
    fn risk_pct_sizing_shrinks_when_volatility_rises_and_with_risk_scale() {
        use std::collections::{BTreeMap, HashMap};
        let id = InstrumentId::new("X");
        let insts: HashMap<_, _> = [(id.clone(), Instrument::linear("X", "X", Px(100)))].into();
        let (marks, books) = (BTreeMap::new(), HashMap::new());
        let pf = tc_ledger::Portfolio::new(Money::dollars(100_000));
        let first_order_units = |range: f64, scale: f64| -> i64 {
            let mut st = BarStrategy::new(id.clone(), Always(1), 10, false, 0.0).with_sizing(
                Sizing::RiskPct {
                    pct: 0.01,
                    atr_mult: 2.0,
                    max_notional_frac: 10.0,
                },
            );
            for i in 0..30 {
                let acts = st.on_event(
                    &ctx_with(&pf, &marks, &books, &insts, scale),
                    &Event::Bar(bar(
                        i,
                        100.0,
                        100.0 + range / 2.0,
                        100.0 - range / 2.0,
                        100.0,
                    )),
                );
                if let Some(Action::Place(o)) = acts.first() {
                    return o.qty.0 / SCALE;
                }
            }
            0
        };
        // risk $1000, stop 2×ATR: ATR≈1 → 500 units; ATR≈2 → 250 units
        let (calm, wild) = (first_order_units(1.0, 1.0), first_order_units(2.0, 1.0));
        assert!(
            (calm - 500).abs() <= 5 && (wild - 250).abs() <= 5,
            "{calm} {wild}"
        );
        assert!(first_order_units(1.0, 0.5) < calm);
    }
}
