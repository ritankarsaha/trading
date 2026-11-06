//! Fixed-point unit newtypes. No bare `f64` prices/quantities/money anywhere in the engine:
//! mixing cents with dollars or price with quantity is a compile error, and all accounting is
//! integer arithmetic so replays are bit-for-bit deterministic.
//!
//! Everything is scaled by `SCALE = 10_000` (1e-4 of the quote currency / of one unit).
//! A Kalshi price of 65¢ is `Px(6_500)`; INR 1,234.55 is `Px(12_345_500)`.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

pub const SCALE: i64 = 10_000;

macro_rules! scaled {
    ($name:ident) => {
        #[derive(
            Copy,
            Clone,
            Debug,
            Default,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            Serialize,
            Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub i64);
        impl $name {
            pub const ZERO: $name = $name(0);
            #[inline]
            pub fn from_f64(v: f64) -> Self {
                $name((v * SCALE as f64).round() as i64)
            }
            #[inline]
            pub fn as_f64(self) -> f64 {
                self.0 as f64 / SCALE as f64
            }
            #[inline]
            pub fn raw(self) -> i64 {
                self.0
            }
            #[inline]
            pub fn abs(self) -> Self {
                $name(self.0.abs())
            }
            #[inline]
            pub fn is_zero(self) -> bool {
                self.0 == 0
            }
        }
        impl Add for $name {
            type Output = $name;
            #[inline]
            fn add(self, o: $name) -> $name {
                $name(self.0 + o.0)
            }
        }
        impl Sub for $name {
            type Output = $name;
            #[inline]
            fn sub(self, o: $name) -> $name {
                $name(self.0 - o.0)
            }
        }
        impl Neg for $name {
            type Output = $name;
            #[inline]
            fn neg(self) -> $name {
                $name(-self.0)
            }
        }
        impl AddAssign for $name {
            #[inline]
            fn add_assign(&mut self, o: $name) {
                self.0 += o.0;
            }
        }
        impl SubAssign for $name {
            #[inline]
            fn sub_assign(&mut self, o: $name) {
                self.0 -= o.0;
            }
        }
        impl Sum for $name {
            fn sum<I: Iterator<Item = $name>>(it: I) -> $name {
                $name(it.map(|x| x.0).sum())
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{:.4}", self.as_f64())
            }
        }
    };
}

scaled!(Px); // price per unit, in quote currency
scaled!(Qty); // signed quantity (contracts / shares / coins)
scaled!(Money); // amount in quote currency

impl Px {
    /// Price from whole cents (Kalshi style): `Px::cents(65)` = $0.65.
    pub const fn cents(c: i64) -> Px {
        Px(c * (SCALE / 100))
    }
    /// The settlement value of a winning binary contract ($1.00).
    pub const ONE: Px = Px(SCALE);
}

impl Qty {
    /// Whole units (contracts / shares).
    pub const fn units(n: i64) -> Qty {
        Qty(n * SCALE)
    }
    #[inline]
    pub fn signum(self) -> i64 {
        self.0.signum()
    }
}

/// price × quantity = money. Computed in i128 and truncated toward zero.
impl Mul<Qty> for Px {
    type Output = Money;
    #[inline]
    fn mul(self, q: Qty) -> Money {
        Money(((self.0 as i128 * q.0 as i128) / SCALE as i128) as i64)
    }
}

impl Money {
    pub const fn dollars(d: i64) -> Money {
        Money(d * SCALE)
    }
    /// Multiply by a dimensionless fraction (risk limits etc.). Truncates toward zero.
    #[inline]
    pub fn frac(self, f: f64) -> Money {
        Money((self.0 as f64 * f) as i64)
    }
}

/// Probability in the open interval (0, 1). Constructor validates; there is no way to hold NaN or 1.3.
#[derive(Copy, Clone, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Prob(f64);

impl Prob {
    pub fn new(p: f64) -> Option<Prob> {
        if p.is_finite() && p > 0.0 && p < 1.0 {
            Some(Prob(p))
        } else {
            None
        }
    }
    /// Clamp into [eps, 1-eps] — for model outputs that may touch the boundary.
    pub fn clamped(p: f64) -> Prob {
        let p = if p.is_finite() { p } else { 0.5 };
        Prob(p.clamp(1e-6, 1.0 - 1e-6))
    }
    #[inline]
    pub fn get(self) -> f64 {
        self.0
    }
}
impl TryFrom<f64> for Prob {
    type Error = String;
    fn try_from(v: f64) -> Result<Self, String> {
        Prob::new(v).ok_or_else(|| format!("probability out of (0,1): {v}"))
    }
}
impl From<Prob> for f64 {
    fn from(p: Prob) -> f64 {
        p.0
    }
}

/// Event time, nanoseconds since the Unix epoch (UTC).
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Ts(pub i64);

impl Ts {
    pub const fn from_secs(s: i64) -> Ts {
        Ts(s * 1_000_000_000)
    }
    pub const fn from_millis(ms: i64) -> Ts {
        Ts(ms * 1_000_000)
    }
    pub fn secs(self) -> i64 {
        self.0 / 1_000_000_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn px_times_qty() {
        // 65¢ × 10 contracts = $6.50
        assert_eq!(Px::cents(65) * Qty::units(10), Money::from_f64(6.5));
        // negative qty (short) gives negative money
        assert_eq!(Px::cents(50) * Qty::units(-4), Money::from_f64(-2.0));
    }

    #[test]
    fn prob_validates() {
        assert!(Prob::new(0.0).is_none());
        assert!(Prob::new(1.0).is_none());
        assert!(Prob::new(f64::NAN).is_none());
        assert!(Prob::new(0.3).is_some());
        assert!(serde_json::from_str::<Prob>("1.2").is_err());
    }

    #[test]
    fn cents_roundtrip() {
        assert_eq!(Px::cents(7).as_f64(), 0.07);
        assert_eq!(Px::ONE - Px::cents(7), Px::cents(93));
    }
}
