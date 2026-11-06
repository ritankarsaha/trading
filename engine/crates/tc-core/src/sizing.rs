//! Fee-aware edge selection and fractional-Kelly sizing — a faithful port of
//! `claude-trading-skills/skills/prediction-market-strategy/scripts/sizing.py`
//! (golden-tested against the Python original in `tests/golden.rs`), plus Benter's two-stage logit blend.

use crate::types::kalshi_fee_dollars;

/// Kalshi taker fee per contract (dollars) at `price`.
pub fn kalshi_fee(price: f64) -> f64 {
    kalshi_fee_dollars(price, 1.0, 0.07)
}

/// `net_edge = p_model − ask − fee(ask)`. The ONLY valid selection metric.
pub fn net_edge(p_model: f64, ask: f64, with_kalshi_fee: bool) -> f64 {
    p_model
        - ask
        - if with_kalshi_fee {
            kalshi_fee(ask)
        } else {
            0.0
        }
}

/// Minimum net-edge gate θ by account size (Kalshi): 15% under $2,000, else 20%.
pub fn theta_for_account(bankroll: f64) -> f64 {
    if bankroll < 2000.0 {
        0.15
    } else {
        0.20
    }
}

/// Polymarket gate (no explicit taker fee; spread and gas are the cost).
pub const THETA_POLYMARKET: f64 = 0.05;

/// Maker bid in whole cents, posted θ below model fair value.
pub fn limit_price_cents(p_model: f64, theta: f64) -> i64 {
    ((p_model - theta) * 100.0).floor() as i64
}

/// Fractional-Kelly contract count, capped at `max_bet_fraction` of bankroll. 0.0 if no edge.
pub fn kelly_contracts(
    p_model: f64,
    entry: f64,
    bankroll: f64,
    kelly_frac: f64,
    max_bet_fraction: f64,
) -> f64 {
    let edge = net_edge(p_model, entry, true);
    if edge <= 0.0 || !(0.0 < entry && entry < 1.0) {
        return 0.0;
    }
    let f = (edge / (1.0 - entry)).clamp(0.0, 1.0);
    let stake = (kelly_frac * f * bankroll).min(max_bet_fraction * bankroll);
    stake / entry
}

/// Kelly sizing with an explicit fee model (e.g. Polymarket: zero fee). `fee_per_contract` is in dollars.
pub fn kelly_contracts_with_fee(
    p_model: f64,
    entry: f64,
    fee_per_contract: f64,
    bankroll: f64,
    kelly_frac: f64,
    max_bet_fraction: f64,
) -> f64 {
    let edge = p_model - entry - fee_per_contract;
    if edge <= 0.0 || !(0.0 < entry && entry < 1.0) {
        return 0.0;
    }
    let f = (edge / (1.0 - entry)).clamp(0.0, 1.0);
    let stake = (kelly_frac * f * bankroll).min(max_bet_fraction * bankroll);
    stake / entry
}

/// Slippage acceptable iff edge > 0 and slip ≤ max_frac × edge.
pub fn slippage_ok(edge: f64, slip: f64, max_frac: f64) -> bool {
    edge > 0.0 && slip <= max_frac * edge
}

/// Benter (1994) second-stage logit: `c_i = softmax(α·ln f_i + β·ln π_i)`.
/// `f` = our model's probabilities, `pi` = market-implied probabilities (same length, each in (0,1)).
/// α, β are fitted out-of-sample in the research plane (`research/tc_research/benter.py`).
/// Returns a probability vector summing to 1 (for a mutually-exclusive outcome set).
pub fn benter_combine(f: &[f64], pi: &[f64], alpha: f64, beta: f64) -> Vec<f64> {
    assert_eq!(f.len(), pi.len(), "benter_combine: length mismatch");
    let z: Vec<f64> = f
        .iter()
        .zip(pi)
        .map(|(&fi, &pii)| alpha * fi.max(1e-12).ln() + beta * pii.max(1e-12).ln())
        .collect();
    let m = z.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let e: Vec<f64> = z.iter().map(|x| (x - m).exp()).collect();
    let s: f64 = e.iter().sum();
    e.iter().map(|x| x / s).collect()
}

/// Binary-market version of the Benter blend (two outcomes: YES with `f`/`pi`, NO with the complements).
pub fn benter_combine_binary(f: f64, pi: f64, alpha: f64, beta: f64) -> f64 {
    benter_combine(&[f, 1.0 - f], &[pi, 1.0 - pi], alpha, beta)[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benter_identity_when_alpha0_beta1() {
        // alpha=0, beta=1 → the blend equals the (normalised) market probabilities
        let c = benter_combine_binary(0.9, 0.3, 0.0, 1.0);
        assert!((c - 0.3).abs() < 1e-12);
    }

    #[test]
    fn benter_moves_towards_model() {
        let c = benter_combine_binary(0.6, 0.3, 0.5, 0.8);
        assert!(c > 0.3 && c < 0.6);
    }

    #[test]
    fn kelly_zero_without_edge() {
        assert_eq!(kelly_contracts(0.08, 0.12, 5000.0, 0.25, 0.015), 0.0);
    }
}

// ───────────────────────────── Kelly hygiene (kelly-criterion / position-sizing skills) ─────────────────────────────

/// Lower bound of the Wilson score interval for a win rate (default z = 1.96). Size from THIS, not the point estimate:
/// it builds sampling-luck conservatism in automatically.
pub fn wilson_lower(wins: u64, total: u64, z: f64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let (n, p) = (total as f64, wins as f64 / total as f64);
    let denom = 1.0 + z * z / n;
    let centre = p + z * z / (2.0 * n);
    let spread = z * ((p * (1.0 - p) + z * z / (4.0 * n)) / n).sqrt();
    ((centre - spread) / denom).max(0.0)
}

/// Fractional-Kelly schedule by evidence (`kelly-criterion`): < 30 trades → 0.10×, 30–100 → 0.25×, 100+ → 0.50×.
/// Full Kelly is never returned: overbetting 2× yields zero growth, underbetting 2× still yields ~75%.
pub fn kelly_fraction_for_sample(n_trades: u64) -> f64 {
    match n_trades {
        0..=29 => 0.10,
        30..=99 => 0.25,
        _ => 0.50,
    }
}

/// Reduce per-position Kelly for correlated bets: `n_eff = n / (1 + (n−1)ρ)`, scale = `n_eff / n`.
pub fn correlation_scale(n_positions: usize, avg_corr: f64) -> f64 {
    if n_positions <= 1 {
        return 1.0;
    }
    let n = n_positions as f64;
    (n / (1.0 + (n - 1.0) * avg_corr.clamp(0.0, 1.0))) / n
}

/// Edge classification from the skill: < 0 negative, < 0.02 "no meaningful edge (costs exceed it)",
/// < 0.10 marginal, < 0.20 good, else excellent-or-overfit (verify before trusting).
pub fn edge_class(edge: f64) -> &'static str {
    if edge < 0.0 {
        "negative"
    } else if edge < 0.02 {
        "none"
    } else if edge < 0.10 {
        "marginal"
    } else if edge < 0.20 {
        "good"
    } else {
        "excellent-or-overfit"
    }
}

#[cfg(test)]
mod kelly_hygiene_tests {
    use super::*;

    #[test]
    fn wilson_matches_reference_and_is_conservative() {
        // 60 wins of 100: Wilson 95% lower bound ≈ 0.5020 (independently computed)
        let w = wilson_lower(60, 100, 1.96);
        assert!((w - 0.5020).abs() < 1e-3, "{w}");
        assert!(w < 0.60);
        assert!(wilson_lower(6, 10, 1.96) < w); // fewer trades → wider → lower bound
        assert_eq!(wilson_lower(0, 0, 1.96), 0.0);
    }

    #[test]
    fn kelly_schedule_is_evidence_gated_and_never_full() {
        assert_eq!(kelly_fraction_for_sample(0), 0.10);
        assert_eq!(kelly_fraction_for_sample(29), 0.10);
        assert_eq!(kelly_fraction_for_sample(30), 0.25);
        assert_eq!(kelly_fraction_for_sample(99), 0.25);
        assert_eq!(kelly_fraction_for_sample(100), 0.50);
        assert!((0..1000).all(|n| kelly_fraction_for_sample(n) < 1.0));
    }

    #[test]
    fn correlated_bets_shrink() {
        assert_eq!(correlation_scale(1, 0.9), 1.0);
        assert!((correlation_scale(4, 0.0) - 1.0).abs() < 1e-12);
        assert!(correlation_scale(4, 0.7) < 0.5);
    }
}
