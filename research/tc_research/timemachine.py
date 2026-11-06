"""Pre-trade Monte-Carlo gate — the idea behind day-trader-skill's `trader-timemachine`, as a pure function.

Bootstraps the rest of the session from the instrument's own recent bar returns and reports how often a plan
(entry/stop/target) hits the target first, the stop first, or neither — plus mean R. It is a simulation of the
instrument's OWN recent past, not a forecast, and it checks closes only (intrabar excursions are not modelled)."""
from __future__ import annotations
import numpy as np


def simulate_plan(returns_pool: np.ndarray, bars_left: int, entry: float, stop: float, target: float,
                  sims: int = 10_000, seed: int = 0, long: bool = True) -> dict:
    """`returns_pool`: recent per-bar simple returns. Bars are resampled IID with replacement."""
    if bars_left < 1 or len(returns_pool) < 10:
        raise ValueError("need >= 1 bar left and >= 10 historical returns")
    rng = np.random.default_rng(seed)
    r = rng.choice(returns_pool, size=(sims, bars_left), replace=True)
    path = entry * np.cumprod(1.0 + r, axis=1)
    hit_t = (path >= target) if long else (path <= target)
    hit_s = (path <= stop) if long else (path >= stop)
    first_t = np.where(hit_t.any(1), hit_t.argmax(1), bars_left + 1)
    first_s = np.where(hit_s.any(1), hit_s.argmax(1), bars_left + 1)
    t_first = first_t < first_s            # ties (same bar) are counted against us: stop wins
    s_first = first_s <= first_t
    s_first &= first_s <= bars_left
    neither = ~(t_first | s_first)
    risk = abs(entry - stop)
    final = path[:, -1]
    r_mult = np.where(t_first, abs(target - entry) / risk,
                      np.where(s_first, -1.0, ((final - entry) if long else (entry - final)) / risk))
    return {"p_target_first": float(t_first.mean()), "p_stop_first": float(s_first.mean()),
            "p_neither": float(neither.mean()), "mean_R": float(r_mult.mean()), "sims": sims}
