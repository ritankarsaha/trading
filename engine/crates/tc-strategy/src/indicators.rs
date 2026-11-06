//! Streaming indicators (f64 internally; strategies convert to/from `Px` at the edges).

#[derive(Clone, Debug)]
pub struct Ema {
    k: f64,
    pub v: Option<f64>,
}
impl Ema {
    pub fn new(n: usize) -> Self {
        Ema {
            k: 2.0 / (n as f64 + 1.0),
            v: None,
        }
    }
    pub fn update(&mut self, x: f64) -> f64 {
        let nv = match self.v {
            None => x,
            Some(p) => p + self.k * (x - p),
        };
        self.v = Some(nv);
        nv
    }
}

#[derive(Clone, Debug)]
pub struct Rolling {
    n: usize,
    buf: std::collections::VecDeque<f64>,
}
impl Rolling {
    pub fn new(n: usize) -> Self {
        Rolling {
            n,
            buf: Default::default(),
        }
    }
    pub fn push(&mut self, x: f64) {
        if self.buf.len() == self.n {
            self.buf.pop_front();
        }
        self.buf.push_back(x);
    }
    pub fn full(&self) -> bool {
        self.buf.len() == self.n
    }
    pub fn mean(&self) -> f64 {
        self.buf.iter().sum::<f64>() / self.buf.len().max(1) as f64
    }
    /// Sample standard deviation (n-1).
    pub fn std(&self) -> f64 {
        let l = self.buf.len();
        if l < 2 {
            return 0.0;
        }
        let m = self.mean();
        (self.buf.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (l as f64 - 1.0)).sqrt()
    }
    /// Mid-rank percentile of `x` within the window, in 0..=1: `(#less + ½·#equal) / n`. Ties count half, so a
    /// perfectly flat series ranks 0.5 ("normal"), not 1.0 ("extreme").
    pub fn rank_of(&self, x: f64) -> f64 {
        if self.buf.is_empty() {
            return 0.5;
        }
        // values within 0.5% of x are ties: a still-converging smoother (Wilder ATR) must not read as 'extreme'
        let tol = 0.005 * x.abs();
        let less = self.buf.iter().filter(|&&v| v < x - tol).count() as f64;
        let eq = self.buf.iter().filter(|&&v| (v - x).abs() <= tol).count() as f64;
        (less + 0.5 * eq) / self.buf.len() as f64
    }
    pub fn max(&self) -> f64 {
        self.buf.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
    }
    pub fn min(&self) -> f64 {
        self.buf.iter().cloned().fold(f64::INFINITY, f64::min)
    }
}

/// Wilder RSI.
#[derive(Clone, Debug)]
pub struct Rsi {
    n: usize,
    prev: Option<f64>,
    avg_gain: f64,
    avg_loss: f64,
    seen: usize,
}
impl Rsi {
    pub fn new(n: usize) -> Self {
        Rsi {
            n,
            prev: None,
            avg_gain: 0.0,
            avg_loss: 0.0,
            seen: 0,
        }
    }
    pub fn update(&mut self, x: f64) -> Option<f64> {
        let Some(p) = self.prev.replace(x) else {
            return None;
        };
        let d = x - p;
        let (g, l) = (d.max(0.0), (-d).max(0.0));
        self.seen += 1;
        let n = self.n as f64;
        if self.seen <= self.n {
            self.avg_gain += g / n;
            self.avg_loss += l / n;
            if self.seen < self.n {
                return None;
            }
        } else {
            self.avg_gain = (self.avg_gain * (n - 1.0) + g) / n;
            self.avg_loss = (self.avg_loss * (n - 1.0) + l) / n;
        }
        Some(if self.avg_loss == 0.0 {
            100.0
        } else {
            100.0 - 100.0 / (1.0 + self.avg_gain / self.avg_loss)
        })
    }
}

#[derive(Clone, Debug)]
pub struct Macd {
    fast: Ema,
    slow: Ema,
    sig: Ema,
    n: usize,
    warm: usize,
}
impl Macd {
    pub fn new(fast: usize, slow: usize, signal: usize) -> Self {
        Macd {
            fast: Ema::new(fast),
            slow: Ema::new(slow),
            sig: Ema::new(signal),
            n: 0,
            warm: slow + signal,
        }
    }
    /// Returns (macd, signal) once warmed up.
    pub fn update(&mut self, x: f64) -> Option<(f64, f64)> {
        let m = self.fast.update(x) - self.slow.update(x);
        let s = self.sig.update(m);
        self.n += 1;
        (self.n >= self.warm).then_some((m, s))
    }
}

/// Average True Range (Wilder).
#[derive(Clone, Debug)]
pub struct Atr {
    n: usize,
    prev_close: Option<f64>,
    v: Option<f64>,
    acc: f64,
    seen: usize,
}
impl Atr {
    pub fn new(n: usize) -> Self {
        Atr {
            n,
            prev_close: None,
            v: None,
            acc: 0.0,
            seen: 0,
        }
    }
    pub fn update(&mut self, h: f64, l: f64, c: f64) -> Option<f64> {
        let tr = match self.prev_close {
            None => h - l,
            Some(pc) => (h - l).max((h - pc).abs()).max((l - pc).abs()),
        };
        self.prev_close = Some(c);
        self.seen += 1;
        let n = self.n as f64;
        if self.seen <= self.n {
            self.acc += tr;
            if self.seen == self.n {
                self.v = Some(self.acc / n);
            }
        } else if let Some(v) = self.v {
            self.v = Some((v * (n - 1.0) + tr) / n);
        }
        self.v
    }
}

/// Wilder's Average Directional Index (trend strength, 0–100). ADX > 25 trending, < 20 ranging.
#[derive(Clone, Debug)]
pub struct Adx {
    n: usize,
    prev: Option<(f64, f64, f64)>,
    tr_s: f64,
    pdm_s: f64,
    mdm_s: f64,
    seen: usize,
    dx_acc: f64,
    dx_n: usize,
    adx: Option<f64>,
}
impl Adx {
    pub fn new(n: usize) -> Self {
        Adx {
            n,
            prev: None,
            tr_s: 0.0,
            pdm_s: 0.0,
            mdm_s: 0.0,
            seen: 0,
            dx_acc: 0.0,
            dx_n: 0,
            adx: None,
        }
    }
    pub fn update(&mut self, h: f64, l: f64, c: f64) -> Option<f64> {
        let Some((ph, pl, pc)) = self.prev.replace((h, l, c)) else {
            return None;
        };
        let (up, down) = (h - ph, pl - l);
        let pdm = if up > down && up > 0.0 { up } else { 0.0 };
        let mdm = if down > up && down > 0.0 { down } else { 0.0 };
        let tr = (h - l).max((h - pc).abs()).max((l - pc).abs());
        self.seen += 1;
        let n = self.n as f64;
        if self.seen <= self.n {
            self.tr_s += tr;
            self.pdm_s += pdm;
            self.mdm_s += mdm;
            if self.seen < self.n {
                return None;
            }
        } else {
            self.tr_s += tr - self.tr_s / n;
            self.pdm_s += pdm - self.pdm_s / n;
            self.mdm_s += mdm - self.mdm_s / n;
        }
        if self.tr_s <= 0.0 {
            return self.adx;
        }
        let (pdi, mdi) = (
            100.0 * self.pdm_s / self.tr_s,
            100.0 * self.mdm_s / self.tr_s,
        );
        let dx = if pdi + mdi > 0.0 {
            100.0 * (pdi - mdi).abs() / (pdi + mdi)
        } else {
            0.0
        };
        if self.dx_n < self.n {
            self.dx_acc += dx;
            self.dx_n += 1;
            if self.dx_n == self.n {
                self.adx = Some(self.dx_acc / n);
            }
        } else if let Some(a) = self.adx {
            self.adx = Some((a * (n - 1.0) + dx) / n);
        }
        self.adx
    }
}

/// Parabolic SAR. `update` returns +1 (uptrend) / -1 (downtrend) once initialised.
#[derive(Clone, Debug)]
pub struct Sar {
    step: f64,
    max_af: f64,
    prev: Option<(f64, f64)>,  // (high, low) of previous bar
    prev2: Option<(f64, f64)>, // bar before that
    up: bool,
    sar: f64,
    ep: f64,
    af: f64,
}
impl Sar {
    pub fn new(step: f64, max_af: f64) -> Self {
        Sar {
            step,
            max_af,
            prev: None,
            prev2: None,
            up: true,
            sar: 0.0,
            ep: 0.0,
            af: step,
        }
    }
    pub fn update(&mut self, high: f64, low: f64) -> Option<i8> {
        let Some((ph, pl)) = self.prev.replace((high, low)) else {
            return None;
        };
        // SAR may not enter the range of the previous TWO bars (not the current one)
        let (pph, ppl) = self.prev2.unwrap_or((ph, pl));
        self.prev2 = Some((ph, pl));
        if self.ep == 0.0 {
            // initialise on the second bar
            self.up = high >= ph;
            self.sar = if self.up { pl } else { ph };
            self.ep = if self.up { high } else { low };
            return Some(if self.up { 1 } else { -1 });
        }
        self.sar += self.af * (self.ep - self.sar);
        if self.up {
            self.sar = self.sar.min(pl).min(ppl);
            if low < self.sar {
                // reversal to downtrend
                self.up = false;
                self.sar = self.ep;
                self.ep = low;
                self.af = self.step;
            } else if high > self.ep {
                self.ep = high;
                self.af = (self.af + self.step).min(self.max_af);
            }
        } else {
            self.sar = self.sar.max(ph).max(pph);
            if high > self.sar {
                self.up = true;
                self.sar = self.ep;
                self.ep = high;
                self.af = self.step;
            } else if low < self.ep {
                self.ep = low;
                self.af = (self.af + self.step).min(self.max_af);
            }
        }
        Some(if self.up { 1 } else { -1 })
    }
}

/// Heikin-Ashi candle stream.
#[derive(Clone, Debug, Default)]
pub struct HeikinAshi {
    prev: Option<(f64, f64)>, // (ha_open, ha_close)
}
impl HeikinAshi {
    /// Returns (ha_open, ha_high, ha_low, ha_close).
    pub fn update(&mut self, o: f64, h: f64, l: f64, c: f64) -> (f64, f64, f64, f64) {
        let ha_c = (o + h + l + c) / 4.0;
        let ha_o = match self.prev {
            None => (o + c) / 2.0,
            Some((po, pc)) => (po + pc) / 2.0,
        };
        self.prev = Some((ha_o, ha_c));
        (ha_o, h.max(ha_o).max(ha_c), l.min(ha_o).min(ha_c), ha_c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsi_all_gains_is_100_and_all_losses_is_0() {
        let mut r = Rsi::new(5);
        let mut last = None;
        for i in 0..20 {
            last = r.update(100.0 + i as f64);
        }
        assert_eq!(last, Some(100.0));
        let mut r = Rsi::new(5);
        for i in 0..20 {
            last = r.update(100.0 - i as f64);
        }
        assert!(last.unwrap() < 1e-9);
    }

    #[test]
    fn rsi_matches_independent_reference() {
        // Reference value computed independently in Python (Wilder smoothing, n=14, simple-average seed):
        // 70.53278948369497 for this 15-close series.
        let closes = [
            44.3389, 44.0902, 44.1497, 43.6124, 44.3278, 44.8264, 45.0955, 45.4245, 45.8433,
            46.0826, 45.8931, 46.0328, 45.614, 46.282, 46.282,
        ];
        let mut r = Rsi::new(14);
        let mut out = None;
        for c in closes {
            out = r.update(c);
        }
        assert!(
            (out.unwrap() - 70.53278948369497).abs() < 1e-9,
            "got {out:?}"
        );
    }

    #[test]
    fn ema_converges_to_constant() {
        let mut e = Ema::new(10);
        let mut v = 0.0;
        for _ in 0..200 {
            v = e.update(5.0);
        }
        assert!((v - 5.0).abs() < 1e-9);
    }

    #[test]
    fn sar_flips_on_reversal() {
        let mut s = Sar::new(0.02, 0.2);
        let mut dirs = vec![];
        for i in 0..15 {
            dirs.push(s.update(100.0 + i as f64 + 1.0, 100.0 + i as f64));
        }
        assert_eq!(*dirs.last().unwrap(), Some(1));
        for i in 0..15 {
            dirs.push(s.update(115.0 - i as f64, 114.0 - i as f64 - 1.0));
        }
        assert_eq!(*dirs.last().unwrap(), Some(-1));
    }

    #[test]
    fn adx_high_in_trend_low_in_chop() {
        let mut a = Adx::new(14);
        let mut v = None;
        for i in 0..120 {
            let p = 100.0 + i as f64 * 0.8;
            v = a.update(p + 0.5, p - 0.5, p);
        }
        assert!(v.unwrap() > 40.0, "trend adx {v:?}");
        let mut a = Adx::new(14);
        for i in 0..200 {
            let p = 100.0 + if i % 2 == 0 { 0.6 } else { -0.6 };
            v = a.update(p + 0.5, p - 0.5, p);
        }
        assert!(v.unwrap() < 20.0, "chop adx {v:?}");
    }

    #[test]
    fn percentile_rank() {
        let mut r = Rolling::new(4);
        for x in [1.0, 2.0, 3.0, 4.0] {
            r.push(x);
        }
        assert_eq!(r.rank_of(3.0), 0.625); // 2 below + half of the tie
        assert_eq!(r.rank_of(0.0), 0.0);
        assert_eq!(r.rank_of(9.0), 1.0);
        let mut flat = Rolling::new(5);
        for _ in 0..5 {
            flat.push(2.0);
        }
        assert_eq!(flat.rank_of(2.0), 0.5); // constant series is 'normal', not 'extreme'
    }

    #[test]
    fn rolling_stats() {
        let mut r = Rolling::new(3);
        for x in [1.0, 2.0, 3.0, 4.0] {
            r.push(x);
        }
        assert!(r.full());
        assert_eq!(r.mean(), 3.0);
        assert_eq!(r.max(), 4.0);
        assert_eq!(r.min(), 2.0);
        assert!((r.std() - 1.0).abs() < 1e-12);
    }
}
