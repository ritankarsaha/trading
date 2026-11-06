//! Regime detection (`regime-detection` skill): every strategy has a home regime. Trend-followers bleed in chop,
//! mean-reverters get run over in trends. [`RegimeGated`] lets a signal trade only in its home regime and forces
//! flat otherwise.
//!
//! Quadrants from two axes — trend strength (ADX: >25 trending, <20 ranging) and volatility (ATR percentile vs its
//! own recent history). Crypto/short-horizon rule of thumb from the skill: ATR lookback 50–100, ADX 10–14.

use crate::bar::BarSignal;
use crate::indicators::*;
use tc_core::Bar;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Regime {
    pub adx: f64,
    /// Fraction of the recent ATR window at or below the current ATR (0 = quietest, 1 = most volatile).
    pub vol_pct: f64,
}

impl Regime {
    pub fn trending(&self) -> bool {
        self.adx > 25.0
    }
    pub fn ranging(&self) -> bool {
        self.adx < 20.0
    }
    pub fn high_vol(&self) -> bool {
        self.vol_pct > 0.75
    }
    pub fn quadrant(&self) -> &'static str {
        match (self.trending(), self.ranging(), self.high_vol()) {
            (true, _, false) => "trending/low-vol",
            (true, _, true) => "trending/high-vol",
            (_, true, false) => "ranging/low-vol",
            (_, true, true) => "ranging/high-vol (chop — sit out)",
            _ => "transitional",
        }
    }
}

pub struct RegimeTracker {
    adx: Adx,
    atr: Atr,
    atr_hist: Rolling,
}

impl RegimeTracker {
    pub fn new(adx_period: usize, atr_period: usize, vol_lookback: usize) -> Self {
        RegimeTracker {
            adx: Adx::new(adx_period),
            atr: Atr::new(atr_period),
            atr_hist: Rolling::new(vol_lookback),
        }
    }
    pub fn update(&mut self, b: &Bar) -> Option<Regime> {
        let (h, l, c) = (b.high.as_f64(), b.low.as_f64(), b.close.as_f64());
        let adx = self.adx.update(h, l, c);
        let atr = self.atr.update(h, l, c);
        let (adx, atr) = (adx?, atr?);
        self.atr_hist.push(atr);
        if !self.atr_hist.full() {
            return None;
        }
        Some(Regime {
            adx,
            vol_pct: self.atr_hist.rank_of(atr),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Home {
    /// Trend-following: trade only when ADX says trending.
    Trend,
    /// Mean-reversion: trade only when ranging AND not in a high-vol chop.
    Reversion,
}

/// Wraps any [`BarSignal`]; the inner signal is always updated (so its state stays warm) but its output is
/// forced to flat (0) outside its home regime. While the regime is still warming up, it returns `None`.
pub struct RegimeGated<S: BarSignal> {
    inner: S,
    tracker: RegimeTracker,
    home: Home,
    pub last: Option<Regime>,
}

impl<S: BarSignal> RegimeGated<S> {
    pub fn new(inner: S, home: Home) -> Self {
        RegimeGated {
            inner,
            tracker: RegimeTracker::new(14, 14, 100),
            home,
            last: None,
        }
    }
    pub fn with_tracker(mut self, t: RegimeTracker) -> Self {
        self.tracker = t;
        self
    }
}

impl<S: BarSignal> BarSignal for RegimeGated<S> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn update(&mut self, b: &Bar) -> Option<i8> {
        let sig = self.inner.update(b);
        let reg = self.tracker.update(b)?;
        self.last = Some(reg);
        let allowed = match self.home {
            Home::Trend => reg.trending(),
            Home::Reversion => reg.ranging() && !reg.high_vol(),
        };
        sig.map(|d| if allowed { d } else { 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tc_core::*;

    fn bar(i: i64, p: f64) -> Bar {
        Bar {
            instrument: InstrumentId::new("X"),
            ts: Ts::from_secs(60 * i),
            open: Px::from_f64(p),
            high: Px::from_f64(p + 0.5),
            low: Px::from_f64(p - 0.5),
            close: Px::from_f64(p),
            volume: Qty::units(1),
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
    fn reversion_is_forced_flat_in_a_trend_but_trend_signal_passes() {
        let mut rev = RegimeGated::new(Always(1), Home::Reversion);
        let mut trend = RegimeGated::new(Always(1), Home::Trend);
        let (mut r, mut t) = (None, None);
        for i in 0..400 {
            let p = 100.0 + i as f64 * 0.8; // relentless trend
            r = rev.update(&bar(i, p));
            t = trend.update(&bar(i, p));
        }
        assert_eq!(
            r,
            Some(0),
            "mean-reversion must stand aside in a trend: {:?}",
            rev.last
        );
        assert_eq!(t, Some(1), "trend signal allowed: {:?}", trend.last);
        assert!(rev.last.unwrap().trending());
    }

    #[test]
    fn reversion_trades_in_quiet_chop() {
        let mut rev = RegimeGated::new(Always(-1), Home::Reversion);
        let mut r = None;
        for i in 0..400 {
            let p = 100.0 + if i % 2 == 0 { 0.3 } else { -0.3 };
            r = rev.update(&bar(i, p));
        }
        assert_eq!(r, Some(-1), "{:?}", rev.last);
    }

    #[test]
    fn warming_up_returns_none() {
        let mut g = RegimeGated::new(Always(1), Home::Trend);
        assert_eq!(g.update(&bar(0, 100.0)), None);
    }
}
