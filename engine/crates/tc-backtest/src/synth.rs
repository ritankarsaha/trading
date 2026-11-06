//! Deterministic SYNTHETIC data generators — for tests and plumbing demos only.
//!
//! A result on synthetic data proves the machinery works (fills, accounting, risk, determinism). It says
//! NOTHING about real-market profitability: whether a longshot bias exists is built in here by construction.

use tc_core::*;

/// SplitMix64 — tiny, fast, deterministic, no dependencies.
pub struct Rng(pub u64);
impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn normal(&mut self) -> f64 {
        let (u1, u2) = (self.uniform().max(1e-12), self.uniform());
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

pub struct PmSynth {
    pub instruments: Vec<Instrument>,
    pub events: Vec<Event>,
}

/// `n` binary markets quoted at 4–30¢. `true_p = price × true_ratio` (ratio < 1 ⇒ longshots are overpriced,
/// the favourite–longshot bias; ratio = 1 ⇒ perfectly calibrated control). Quotes jitter ±1¢; takers lift the
/// ask at random; every market settles at the end with outcome ~ Bernoulli(true_p).
pub fn prediction_markets(n: usize, steps: usize, seed: u64, true_ratio: f64) -> PmSynth {
    let mut rng = Rng(seed);
    let mut instruments = vec![];
    let mut base = vec![];
    for i in 0..n {
        let id = format!("M{i}:YES");
        instruments.push(Instrument::binary(&id, &format!("M{i}")));
        let price_c = 4 + (rng.uniform() * 27.0) as i64; // 4..30 cents
        base.push((InstrumentId::new(id), price_c));
    }
    let mut events = vec![];
    let mut t = 1_000_000_000i64;
    for _ in 0..steps {
        for (id, price_c) in &base {
            t += 1_000;
            let jitter = (rng.uniform() * 3.0) as i64 - 1; // -1,0,+1
            let ask_c = (price_c + jitter).clamp(3, 97);
            let bid_c = (ask_c - 2).max(1);
            events.push(Event::Book(Book {
                instrument: id.clone(),
                ts: Ts(t),
                bids: vec![Level {
                    px: Px::cents(bid_c),
                    qty: Qty::units(50),
                }],
                asks: vec![Level {
                    px: Px::cents(ask_c),
                    qty: Qty::units(50),
                }],
            }));
            if rng.uniform() < 0.6 {
                t += 1;
                events.push(Event::Trade(TradeTick {
                    instrument: id.clone(),
                    ts: Ts(t),
                    px: Px::cents(ask_c),
                    qty: Qty::units(10 + (rng.uniform() * 90.0) as i64),
                    taker_side: Side::Buy,
                }));
            }
        }
    }
    for (id, price_c) in &base {
        t += 1_000;
        let p_true = (*price_c as f64 / 100.0 * true_ratio).clamp(0.0, 1.0);
        let win = rng.uniform() < p_true;
        events.push(Event::Settlement(Settlement {
            instrument: id.clone(),
            ts: Ts(t),
            price: if win { Px::ONE } else { Px::ZERO },
        }));
    }
    PmSynth {
        instruments,
        events,
    }
}

/// Geometric-Brownian-motion bars with a drift schedule, stamped at one-minute closes.
pub fn gbm_bars(id: &str, n: usize, seed: u64, drift_per_bar: f64, vol_per_bar: f64) -> Vec<Event> {
    let mut rng = Rng(seed);
    let mut px = 100.0f64;
    let mut out = vec![];
    for i in 0..n {
        let o = px;
        let mut hi = o;
        let mut lo = o;
        for _ in 0..4 {
            px *= (drift_per_bar / 4.0 + vol_per_bar / 2.0 * rng.normal()).exp();
            hi = hi.max(px);
            lo = lo.min(px);
        }
        out.push(Event::Bar(Bar {
            instrument: InstrumentId::new(id),
            ts: Ts::from_secs(60 * (i as i64 + 1)),
            open: Px::from_f64(o),
            high: Px::from_f64(hi),
            low: Px::from_f64(lo),
            close: Px::from_f64(px),
            volume: Qty::units(10_000),
        }));
    }
    out
}

/// Alternating regimes, SYNTHETIC by construction: `seg_len` bars of low-vol up/down TREND, then `seg_len` bars of
/// mean-reverting (Ornstein–Uhlenbeck) RANGE around the current level, repeating. Used only to show that the regime
/// gate routes strategies to their home regime; real markets do not announce their regimes this cleanly.
pub fn regime_switching_bars(id: &str, segments: usize, seg_len: usize, seed: u64) -> Vec<Event> {
    let mut rng = Rng(seed);
    let mut px = 100.0f64;
    let mut out = vec![];
    let mut i = 0i64;
    for s in 0..segments {
        let trending = s % 2 == 0;
        let drift = if (s / 2) % 2 == 0 { 0.0015 } else { -0.0015 };
        let anchor = px;
        for _ in 0..seg_len {
            let o = px;
            let mut hi = o;
            let mut lo = o;
            for _ in 0..4 {
                let ret = if trending {
                    drift / 4.0 + 0.0008 * rng.normal()
                } else {
                    // OU: pulled back toward the anchor, noisy
                    -0.35 * (px / anchor).ln() / 4.0 + 0.0022 * rng.normal()
                };
                px *= ret.exp();
                hi = hi.max(px);
                lo = lo.min(px);
            }
            i += 1;
            out.push(Event::Bar(Bar {
                instrument: InstrumentId::new(id),
                ts: Ts::from_secs(60 * i),
                open: Px::from_f64(o),
                high: Px::from_f64(hi),
                low: Px::from_f64(lo),
                close: Px::from_f64(px),
                volume: Qty::units(10_000),
            }));
        }
    }
    out
}
