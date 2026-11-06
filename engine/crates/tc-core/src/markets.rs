//! Market-structure helpers distilled from the trading skills (`kalshi-api`, `kalshi-weather-markets`,
//! `kalshi-crypto-index-markets`, `slippage-modeling`, `prediction-market-strategy`).

use crate::event::{Book, Level};
use crate::types::InstrumentId;
use crate::units::*;

// ───────────────────────────── Kalshi book convention ─────────────────────────────

/// Kalshi publishes **two bid ladders** (YES bids and NO bids); there is no ask book. The YES ask is the
/// complement of the best NO bid: `yes_ask = 1 − no_bid`. Getting this backwards silently inverts every signal
/// (`kalshi-api` skill). Inputs are best-first lists of (price, qty); output is a normal two-sided YES `Book`.
pub fn kalshi_book_from_ladders(
    instrument: InstrumentId,
    ts: Ts,
    yes_bids: &[(Px, Qty)],
    no_bids: &[(Px, Qty)],
) -> Book {
    let bids = yes_bids
        .iter()
        .map(|&(px, qty)| Level { px, qty })
        .collect();
    let mut asks: Vec<Level> = no_bids
        .iter()
        .map(|&(px, qty)| Level {
            px: Px::ONE - px,
            qty,
        })
        .collect();
    asks.sort_by_key(|l| l.px); // best (lowest) ask first
    Book {
        instrument,
        ts,
        bids,
        asks,
    }
}

// ───────────────────────────── Order ids ─────────────────────────────

/// Kalshi accepts `client_order_id` in `[A-Za-z0-9-]` only (bracket tickers contain `.`, our tags contain `_`/`:`),
/// and the venue enforces a length cap. Every engine-generated id passes through here.
pub fn sanitize_client_id(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if s.len() <= 36 {
        s
    } else {
        // keep the unique tail (the counter), truncate the readable head
        let tail = &s[s.len() - 12..];
        format!("{}{}", &s[..24], tail)
    }
}

pub fn is_venue_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 36 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

// ───────────────────────────── Slippage: walk the real ladder ─────────────────────────────

/// Walk a best-first ladder to fill `qty`. Returns `(depth_weighted_avg_px, slippage_vs_best, filled_qty)`;
/// `filled < qty` means the book cannot absorb the size (never pretend otherwise).
/// Selection must use this price, not the top-of-book (`prediction-market-strategy`: "slippage is part of selection").
pub fn walk_ladder(levels: &[Level], qty: Qty) -> Option<(Px, Px, Qty)> {
    let best = levels.first()?.px;
    let (mut remaining, mut notional, mut filled) = (qty.0, 0i128, 0i64);
    for l in levels {
        if remaining == 0 {
            break;
        }
        let take = remaining.min(l.qty.0);
        notional += l.px.0 as i128 * take as i128;
        filled += take;
        remaining -= take;
    }
    if filled == 0 {
        return None;
    }
    let avg = Px((notional / filled as i128) as i64);
    Some((avg, Px((avg.0 - best.0).abs()), Qty(filled)))
}

// ───────────────────────────── Gaussian bracket probabilities ─────────────────────────────

fn erf(x: f64) -> f64 {
    // Abramowitz–Stegun 7.1.26 is only ~1e-7 accurate; use a high-precision series/continued fraction instead.
    // For |x| < 3 use the Maclaurin series; beyond that erfc via continued fraction.
    let ax = x.abs();
    let r = if ax < 3.0 {
        let mut sum = ax;
        let mut term = ax;
        let x2 = ax * ax;
        for n in 1..200 {
            term *= -x2 / n as f64;
            let add = term / (2 * n + 1) as f64;
            sum += add;
            if add.abs() < 1e-17 {
                break;
            }
        }
        2.0 / std::f64::consts::PI.sqrt() * sum
    } else {
        // erfc(x) ≈ exp(-x²)/(x√π) · 1/(1+ 1/(2x²)/(1+2/(2x²)/(1+...)))  (continued fraction, Lentz-free form)
        let mut f = 0.0;
        for k in (1..60).rev() {
            f = (k as f64 / 2.0) / (ax + f);
        }
        1.0 - (-ax * ax).exp() / (std::f64::consts::PI.sqrt() * (ax + f))
    };
    if x < 0.0 {
        -r
    } else {
        r
    }
}

/// Standard normal CDF.
pub fn phi(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// P(YES) for an **integer-settled** bracket `{floor, cap}` (both ends inclusive), e.g. a 2°F weather bracket
/// `B74.5` = {74, 75}. The ±0.5 continuity correction is mandatory: dropping it, or treating a 2°F inclusive
/// bracket as a 1°F half-open one, produced a +1640% phantom backtest (`kalshi-weather-markets`).
pub fn bracket_p_yes_int(floor: i64, cap: i64, mu: f64, sigma: f64) -> f64 {
    phi((cap as f64 + 0.5 - mu) / sigma) - phi((floor as f64 - 0.5 - mu) / sigma)
}

/// One-sided threshold on an integer-settled value. `greater`: YES iff value ≥ strike+1; `less`: YES iff ≤ strike−1.
pub fn threshold_p_yes_int(strike: i64, mu: f64, sigma: f64, greater: bool) -> f64 {
    if greater {
        1.0 - phi((strike as f64 + 0.5 - mu) / sigma)
    } else {
        phi((strike as f64 - 0.5 - mu) / sigma)
    }
}

/// P(YES) for a **continuous-price** bracket `[floor, cap]` (index / crypto markets). NO ±0.5 correction here
/// (`kalshi-crypto-index-markets`). Pass `f64::NEG_INFINITY` / `INFINITY` for open tails.
pub fn bracket_p_yes_cont(floor: f64, cap: f64, mu: f64, sigma: f64) -> f64 {
    let hi = if cap.is_infinite() {
        1.0
    } else {
        phi((cap - mu) / sigma)
    };
    let lo = if floor.is_infinite() {
        0.0
    } else {
        phi((floor - mu) / sigma)
    };
    hi - lo
}

/// `(mu, sigma)` from ensemble quantiles: σ = max((p90 − p10)/2.56, 0.5)·scale, floored at 0.1.
pub fn mu_sigma_from_quantiles(p10: f64, p50: f64, p90: f64, sigma_scale: f64) -> (f64, f64) {
    let raw = ((p90 - p10) / 2.56).max(0.5) * sigma_scale;
    (p50, raw.max(0.1))
}

/// Sum of YES prices across a mutually-exclusive partition. Fair = 1.0; the excess is the house margin and
/// concentrates in the cheap tails (the favourite–longshot bias).
pub fn overround(yes_prices: &[f64]) -> f64 {
    yes_prices.iter().sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kalshi_ladders_to_two_sided_book() {
        let b = kalshi_book_from_ladders(
            InstrumentId::new("M:YES"),
            Ts(1),
            &[
                (Px::cents(40), Qty::units(10)),
                (Px::cents(38), Qty::units(5)),
            ],
            &[
                (Px::cents(55), Qty::units(7)),
                (Px::cents(52), Qty::units(3)),
            ],
        );
        // best NO bid 55¢ → YES ask 45¢; next NO bid 52¢ → YES ask 48¢
        assert_eq!(b.best_bid().unwrap().px, Px::cents(40));
        assert_eq!(b.best_ask().unwrap().px, Px::cents(45));
        assert_eq!(b.asks[1].px, Px::cents(48));
        assert_eq!(b.spread().unwrap(), Px::cents(5));
        // P(YES) mid per the skill: (best_yes_bid + (1 − best_no_bid)) / 2 = (0.40 + 0.45)/2
        assert_eq!(b.mid().unwrap(), Px::from_f64(0.425));
    }

    #[test]
    fn client_ids_are_venue_safe() {
        for raw in [
            "longshot_fade-1",
            "macd:SYN-12",
            "KXHIGHNY-26JUN02-B75.5",
            "pairs:A/B-3",
        ] {
            let s = sanitize_client_id(raw);
            assert!(is_venue_safe_id(&s), "{raw} -> {s}");
        }
        let long = sanitize_client_id(&format!("{}-123456789012", "x".repeat(80)));
        assert!(long.len() <= 36 && long.ends_with("123456789012"));
        assert!(!is_venue_safe_id("a_b") && !is_venue_safe_id("a.b") && !is_venue_safe_id(""));
    }

    #[test]
    fn ladder_walk_reports_depth_weighted_price_and_shortfall() {
        let asks = vec![
            Level {
                px: Px::cents(50),
                qty: Qty::units(5),
            },
            Level {
                px: Px::cents(52),
                qty: Qty::units(5),
            },
        ];
        let (avg, slip, filled) = walk_ladder(&asks, Qty::units(8)).unwrap();
        assert_eq!(filled, Qty::units(8));
        assert_eq!(avg, Px::from_f64((0.50 * 5.0 + 0.52 * 3.0) / 8.0));
        assert!(slip > Px::ZERO);
        let (_, _, partial) = walk_ladder(&asks, Qty::units(50)).unwrap();
        assert_eq!(partial, Qty::units(10)); // book can only absorb 10
    }

    #[test]
    fn phi_known_values() {
        assert!((phi(0.0) - 0.5).abs() < 1e-15);
        assert!((phi(1.0) - 0.8413447460685429).abs() < 1e-12);
        assert!((phi(-1.96) - 0.024997895148220435).abs() < 1e-12);
        assert!((phi(4.0) - 0.9999683287581669).abs() < 1e-10);
    }

    #[test]
    fn brackets_partition_to_one() {
        // a full integer partition (open tails + 2°F brackets) must sum to exactly 1
        let (mu, sg) = (75.2, 2.0);
        let mut total = threshold_p_yes_int(66, mu, sg, false); // ≤ 65
        let mut lo = 66;
        while lo < 84 {
            total += bracket_p_yes_int(lo, lo + 1, mu, sg);
            lo += 2;
        }
        total += threshold_p_yes_int(83, mu, sg, true); // ≥ 84
        assert!((total - 1.0).abs() < 1e-12, "{total}");
    }

    #[test]
    fn continuity_correction_matters() {
        // the +1640% bug: a 2°F inclusive bracket is NOT a 1°F window
        let right = bracket_p_yes_int(74, 75, 74.5, 1.0);
        let wrong = bracket_p_yes_cont(74.0, 75.0, 74.5, 1.0);
        assert!(right > wrong * 1.3, "{right} vs {wrong}");
    }
}
