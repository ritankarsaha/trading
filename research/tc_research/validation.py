"""Leak-free validation (walk-forward-validation skill + López de Prado):

* `walk_forward_splits` — rolling/expanding windows with PURGE (labels that reach into the test period) and EMBARGO
  (serial-correlation buffer). Never shuffle a time series.
* `deflated_sharpe` — Bailey & López de Prado (2014): probability the observed Sharpe is genuinely > 0 after
  correcting for how many strategies were tried, return skew/kurtosis and sample length.
* `pbo_cscv` — Probability of Backtest Overfitting via combinatorially-symmetric cross-validation.

A strategy is only promoted when OOS ΔR² > 0 (benter.py) AND DSR ≥ 0.95 AND PBO < 0.5.
"""
from __future__ import annotations
import itertools
import math
from statistics import NormalDist
from typing import Iterator
import numpy as np

_N = NormalDist()
EULER_GAMMA = 0.5772156649015329


def walk_forward_splits(n: int, train: int, test: int, step: int | None = None, *, embargo: int = 0,
                        purge: int = 0, expanding: bool = False) -> Iterator[tuple[np.ndarray, np.ndarray]]:
    """Yield (train_idx, test_idx). `purge` = label horizon in bars (a train sample at i has a label that spans
    i..i+purge, so train must end `purge` bars before test starts); `embargo` = extra gap on top."""
    step = step or test
    start = 0
    while True:
        test_start = start + train + embargo + purge
        test_end = test_start + test
        if test_end > n:
            return
        train_end = test_start - embargo - purge          # exclusive
        train_start = 0 if expanding else start
        yield np.arange(train_start, train_end), np.arange(test_start, test_end)
        start += step


def deflated_sharpe(sr_hat: float, n_obs: int, n_trials: int, sr_var_trials: float, skew: float = 0.0,
                    kurt: float = 3.0) -> float:
    """All Sharpe values are PER-PERIOD (not annualised). `kurt` is raw kurtosis (3 for a normal). `sr_var_trials`
    is the variance of the per-period Sharpe across the trials you tried. Returns P(true SR > 0)."""
    if n_trials < 1 or n_obs < 3:
        raise ValueError("need n_trials >= 1 and n_obs >= 3")
    if n_trials == 1:
        sr0 = 0.0
    else:
        sr0 = math.sqrt(max(sr_var_trials, 0.0)) * (
            (1 - EULER_GAMMA) * _N.inv_cdf(1 - 1.0 / n_trials)
            + EULER_GAMMA * _N.inv_cdf(1 - 1.0 / (n_trials * math.e)))
    denom = math.sqrt(max(1 - skew * sr_hat + (kurt - 1) / 4.0 * sr_hat ** 2, 1e-12))
    return _N.cdf((sr_hat - sr0) * math.sqrt(n_obs - 1) / denom)


def pbo_cscv(perf: np.ndarray, n_blocks: int = 8) -> dict:
    """`perf`: T×N matrix of per-period returns for N candidate strategies/parameterisations. Splits T into
    `n_blocks` contiguous blocks; for every choice of half the blocks as in-sample, picks the best IS strategy and
    records its OOS rank. PBO = fraction of splits where that strategy lands in the bottom half OOS."""
    T, N = perf.shape
    if n_blocks % 2 or n_blocks < 4:
        raise ValueError("n_blocks must be even and >= 4")
    blocks = np.array_split(np.arange(T), n_blocks)
    logits = []
    for is_blocks in itertools.combinations(range(n_blocks), n_blocks // 2):
        oos_blocks = [b for b in range(n_blocks) if b not in is_blocks]
        is_idx = np.concatenate([blocks[b] for b in is_blocks])
        oos_idx = np.concatenate([blocks[b] for b in oos_blocks])
        is_sr = perf[is_idx].mean(0) / (perf[is_idx].std(0) + 1e-12)
        oos_sr = perf[oos_idx].mean(0) / (perf[oos_idx].std(0) + 1e-12)
        best = int(np.argmax(is_sr))
        rank = 1 + int((oos_sr < oos_sr[best]).sum())          # 1 = worst OOS … N = best
        omega = rank / (N + 1.0)
        logits.append(math.log(omega / (1 - omega)))
    logits = np.array(logits)
    return {"pbo": float((logits <= 0).mean()), "n_splits": len(logits), "median_logit": float(np.median(logits))}
