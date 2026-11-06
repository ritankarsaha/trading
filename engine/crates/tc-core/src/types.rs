//! Instruments, orders, fills, fee models.

use crate::units::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstrumentId(pub String);

impl InstrumentId {
    pub fn new(s: impl Into<String>) -> Self {
        InstrumentId(s.into())
    }
}
impl std::fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}
impl Side {
    #[inline]
    pub fn sign(self) -> i64 {
        match self {
            Side::Buy => 1,
            Side::Sell => -1,
        }
    }
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tif {
    /// Good till cancelled (rests on the book).
    Gtc,
    /// Immediate-or-cancel: take what is available at the limit, cancel the remainder.
    Ioc,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Liquidity {
    Maker,
    Taker,
}

/// What kind of payoff the instrument has — drives risk (max loss) and settlement.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// Pays $1 if the event happens, else $0. Price ∈ (0,1). Long max loss = px, short max loss = 1-px.
    Binary,
    /// Linear instrument (equity, spot, perp without leverage modelling). Max loss ≈ notional.
    Linear,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FeeModel {
    None,
    /// Basis points of notional, separate for taker and maker (maker can be negative = rebate).
    Bps {
        taker: f64,
        maker: f64,
    },
    /// Kalshi: `ceil(rate × contracts × p × (1-p) × 100) / 100` dollars on takers; makers pay `maker_rate` version.
    KalshiQuadratic {
        taker_rate: f64,
        maker_rate: f64,
    },
}

impl FeeModel {
    pub fn kalshi() -> FeeModel {
        FeeModel::KalshiQuadratic {
            taker_rate: 0.07,
            maker_rate: 0.0,
        }
    }

    /// Fee in money for a fill. Always >= 0 for takers; makers may get a rebate (negative) under `Bps`.
    pub fn fee(&self, px: Px, qty: Qty, liq: Liquidity) -> Money {
        let q = qty.abs();
        match self {
            FeeModel::None => Money::ZERO,
            FeeModel::Bps { taker, maker } => {
                let bps = if liq == Liquidity::Taker {
                    *taker
                } else {
                    *maker
                };
                let notional = (px * q).as_f64();
                Money::from_f64(notional * bps / 10_000.0)
            }
            FeeModel::KalshiQuadratic {
                taker_rate,
                maker_rate,
            } => {
                let rate = if liq == Liquidity::Taker {
                    *taker_rate
                } else {
                    *maker_rate
                };
                kalshi_fee_exact(px, q, rate)
            }
        }
    }
}

/// Exact Kalshi fee: `ceil(rate × C × p × (1−p) × 100) / 100` dollars, in pure integer arithmetic.
/// `rate` is quantised to basis points (0.07 → 700). The float formula below can land a hair above an
/// integer (e.g. 175.00000000000003) and overcharge a cent; this cannot.
pub fn kalshi_fee_exact(px: Px, qty: Qty, rate: f64) -> Money {
    let rate_bp = (rate * 10_000.0).round() as i128;
    let p = px.0 as i128;
    let c = qty.abs().0 as i128;
    let s = SCALE as i128;
    // cents = rate_bp/1e4 × 100 × (c/1e4) × (p/1e4) × ((1e4−p)/1e4)
    let num = rate_bp * 100 * c * p * (s - p);
    let den: i128 = 10_000i128.pow(4); // 1e16
    let cents = (num + den - 1) / den; // ceil for non-negative num
                                       // cents → Money (1 cent = SCALE/100 raw units)
    Money((cents * (s / 100)) as i64)
}

/// Kalshi fee in dollars via floats — a port of `sizing.py` kept ONLY as the golden reference. Differs from
/// `kalshi_fee_exact` by at most one cent, when float error crosses an integer boundary.
pub fn kalshi_fee_dollars(price: f64, contracts: f64, rate: f64) -> f64 {
    (rate * contracts * price * (1.0 - price) * 100.0).ceil() / 100.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instrument {
    pub id: InstrumentId,
    /// Correlation group for risk caps — e.g. all brackets of one event share a group.
    pub group: String,
    pub kind: Kind,
    pub tick: Px,
    pub lot: Qty,
    pub fee: FeeModel,
}

impl Instrument {
    pub fn binary(id: &str, group: &str) -> Instrument {
        Instrument {
            id: InstrumentId::new(id),
            group: group.into(),
            kind: Kind::Binary,
            tick: Px::cents(1),
            lot: Qty::units(1),
            fee: FeeModel::None,
        }
    }
    pub fn linear(id: &str, group: &str, tick: Px) -> Instrument {
        Instrument {
            id: InstrumentId::new(id),
            group: group.into(),
            kind: Kind::Linear,
            tick,
            lot: Qty::units(1),
            fee: FeeModel::None,
        }
    }
    pub fn with_fee(mut self, fee: FeeModel) -> Self {
        self.fee = fee;
        self
    }

    /// Worst-case loss of holding `qty` (signed) acquired at `px`. Used by the risk gate.
    pub fn max_loss(&self, qty: Qty, px: Px) -> Money {
        match self.kind {
            Kind::Binary => {
                if qty.0 >= 0 {
                    px * qty
                } else {
                    (Px::ONE - px) * qty.abs()
                }
            }
            Kind::Linear => px * qty.abs(),
        }
    }

    pub fn valid_price(&self, px: Px) -> bool {
        if px.0 <= 0 || self.tick.0 <= 0 || px.0 % self.tick.0 != 0 {
            return false;
        }
        match self.kind {
            Kind::Binary => px < Px::ONE,
            Kind::Linear => true,
        }
    }
}

/// An order request. Limit-only by design: a "market" order is an IOC limit at a protective price,
/// so there is no way to send an unbounded-price order from this engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrderReq {
    /// Idempotency key, unique per order, chosen by the engine.
    pub client_id: String,
    pub instrument: InstrumentId,
    pub side: Side,
    pub px: Px,
    pub qty: Qty,
    pub tif: Tif,
    /// Reject rather than cross the book. Mandatory for maker strategies.
    pub post_only: bool,
    /// Cohort tag (strategy name / band) — stamped on every resulting fill at write time.
    pub tag: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    pub client_id: String,
    pub instrument: InstrumentId,
    pub side: Side,
    pub px: Px,
    pub qty: Qty, // always positive; direction is in `side`
    pub fee: Money,
    pub liquidity: Liquidity,
    pub ts: Ts,
    pub tag: String,
}

/// Truthful cancel semantics: "already gone" usually means *filled* — it is never treated as cancelled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelResult {
    Cancelled,
    AlreadyGone,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum PlaceError {
    #[error("rejected: {0}")]
    Rejected(String),
    #[error("post-only order would cross the book")]
    PostOnlyWouldCross,
    #[error("duplicate client id {0}")]
    Duplicate(String),
    #[error("venue error: {0}")]
    Venue(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kalshi_fee_matches_python_formula() {
        // python: ceil(0.07*1*0.18*0.82*100)/100 = ceil(1.0332)/100 = 0.02
        assert_eq!(kalshi_fee_dollars(0.18, 1.0, 0.07), 0.02);
        // 50¢ × 100 contracts: exactly 175¢. Python's float gives 176 (175.00000000000003); ours is exact.
        assert_eq!(kalshi_fee_dollars(0.5, 100.0, 0.07), 1.76);
        assert_eq!(
            kalshi_fee_exact(Px::cents(50), Qty::units(100), 0.07),
            Money::from_f64(1.75)
        );
        assert_eq!(
            kalshi_fee_exact(Px::cents(18), Qty::units(1), 0.07),
            Money::from_f64(0.02)
        );
    }

    #[test]
    fn binary_max_loss() {
        let i = Instrument::binary("X:YES", "X");
        assert_eq!(
            i.max_loss(Qty::units(10), Px::cents(20)),
            Money::from_f64(2.0)
        );
        assert_eq!(
            i.max_loss(Qty::units(-10), Px::cents(20)),
            Money::from_f64(8.0)
        );
    }

    #[test]
    fn tick_validation() {
        let i = Instrument::binary("X:YES", "X");
        assert!(i.valid_price(Px::cents(50)));
        assert!(!i.valid_price(Px::ONE));
        assert!(!i.valid_price(Px(5_050))); // 50.5¢ not on 1¢ tick
    }
}
