"""Benter (1994) second-stage conditional logit and the ΔR² test.

    c_i = exp(α·ln f_i + β·ln π_i) / Σ_j exp(α·ln f_j + β·ln π_j)

f = our model's probabilities, π = the market's implied probabilities. α, β are fitted by maximum likelihood
on OUT-OF-SAMPLE f (in-sample f overstates the model). A model is worth trading only if it raises the pseudo-R²
over the market alone:  ΔR² = R²(combined) − R²(market).  (Benter's example: 0.0178 was enough to be profitable;
a 'tipster' model with R² ≈ the fundamental model added 0.0002 — i.e. nothing.)

Groups = one mutually-exclusive outcome set each (a race, or a binary market as a 2-outcome group).
"""
from __future__ import annotations
from dataclasses import dataclass
import numpy as np


@dataclass
class Group:
    f: np.ndarray       # model probabilities (len n)
    pi: np.ndarray      # market probabilities (len n)
    winner: int         # index of the realised outcome


def binary_group(f_yes: float, pi_yes: float, yes_won: bool) -> Group:
    return Group(np.array([f_yes, 1 - f_yes]), np.array([pi_yes, 1 - pi_yes]), 0 if yes_won else 1)


def _clip(p):
    return np.clip(np.asarray(p, float), 1e-9, 1 - 1e-9)


def _nll_grad_hess(theta: np.ndarray, groups: list[Group]):
    nll, g, H = 0.0, np.zeros(2), np.zeros((2, 2))
    for grp in groups:
        X = np.stack([np.log(_clip(grp.f)), np.log(_clip(grp.pi))], axis=1)  # n×2
        z = X @ theta
        z -= z.max()
        e = np.exp(z)
        p = e / e.sum()
        nll -= np.log(p[grp.winner])
        mean = p @ X
        g -= X[grp.winner] - mean
        Xc = X - mean
        H += (Xc * p[:, None]).T @ Xc
    return nll, g, H


def fit(groups: list[Group], iters: int = 50) -> tuple[float, float]:
    """Newton–Raphson MLE for (α, β). The likelihood is concave, so this converges from any start."""
    theta = np.array([0.0, 1.0])  # start at 'trust the market'
    for _ in range(iters):
        nll, g, H = _nll_grad_hess(theta, groups)
        step = np.linalg.solve(H + 1e-9 * np.eye(2), g)
        # backtracking line search keeps it monotone
        t = 1.0
        while t > 1e-6:
            if _nll_grad_hess(theta - t * step, groups)[0] <= nll:
                break
            t /= 2
        theta = theta - t * step
        if np.abs(t * step).max() < 1e-10:
            break
    return float(theta[0]), float(theta[1])


def combine(f, pi, alpha: float, beta: float) -> np.ndarray:
    z = alpha * np.log(_clip(f)) + beta * np.log(_clip(pi))
    z -= z.max()
    e = np.exp(z)
    return e / e.sum()


def _loglik(groups: list[Group], probs: list[np.ndarray]) -> float:
    return float(sum(np.log(_clip(p)[g.winner]) for g, p in zip(groups, probs)))


def pseudo_r2(groups: list[Group], probs: list[np.ndarray]) -> float:
    """R² = 1 − L(model)/L(1/N): Benter's / Bolton–Chapman's goodness of fit against a coin-flip baseline."""
    base = float(sum(np.log(1.0 / len(g.f)) for g in groups))
    return 1.0 - _loglik(groups, probs) / base


def delta_r2(groups: list[Group], alpha: float, beta: float) -> dict:
    mkt = [_clip(g.pi) / _clip(g.pi).sum() for g in groups]
    mod = [_clip(g.f) / _clip(g.f).sum() for g in groups]
    comb = [combine(g.f, g.pi, alpha, beta) for g in groups]
    r_p, r_f, r_c = pseudo_r2(groups, mkt), pseudo_r2(groups, mod), pseudo_r2(groups, comb)
    return {"R2_market": r_p, "R2_model": r_f, "R2_combined": r_c, "delta_R2": r_c - r_p,
            "alpha": alpha, "beta": beta}


def oos_delta_r2(groups: list[Group], train_frac: float = 0.6) -> dict:
    """Temporal hold-out: fit (α, β) on the first `train_frac` of groups (caller supplies them time-ordered),
    score ΔR² on the rest. NEVER shuffle — shuffled CV leaks the future."""
    k = int(len(groups) * train_frac)
    a, b = fit(groups[:k])
    out = delta_r2(groups[k:], a, b)
    out.update(n_train=k, n_test=len(groups) - k)
    return out
