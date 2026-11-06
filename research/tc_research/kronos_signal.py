"""Kronos → probability signal.

`KronosPredictor.predict(sample_count=N)` AVERAGES N sampled paths, which destroys the distribution. To get
P(close_T > strike) we draw N independent single-path forecasts and count. The result is a raw model probability:
it is only a *feature*. It earns capital only if the Benter test (`benter.oos_delta_r2`) shows it adds ΔR² over
the market price on held-out data.
"""
from __future__ import annotations
import sys
from pathlib import Path
from typing import Callable

import pandas as pd


def prob_above(final_closes: list[float], strike: float) -> float:
    """Laplace-smoothed fraction of sampled terminal closes above `strike` — never exactly 0 or 1."""
    n = len(final_closes)
    k = sum(1 for c in final_closes if c > strike)
    return (k + 1.0) / (n + 2.0)


def sample_final_closes(predict_once: Callable[[], pd.DataFrame], n_paths: int) -> list[float]:
    return [float(predict_once()["close"].iloc[-1]) for _ in range(n_paths)]


def load_kronos(kronos_repo: str | Path, model: str = "NeoQuasar/Kronos-small",
                tokenizer: str = "NeoQuasar/Kronos-Tokenizer-base", device: str | None = None):
    """Lazy import (needs torch + HF weights; not installed by default — see README)."""
    sys.path.insert(0, str(kronos_repo))
    from model import Kronos, KronosPredictor, KronosTokenizer  # type: ignore
    tok = KronosTokenizer.from_pretrained(tokenizer)
    mdl = Kronos.from_pretrained(model)
    return KronosPredictor(mdl, tok, device=device, max_context=512)


def kronos_prob_above(predictor, df: pd.DataFrame, x_ts: pd.Series, y_ts: pd.Series, strike: float,
                      n_paths: int = 30, T: float = 1.0, top_p: float = 0.9) -> float:
    def once():
        return predictor.predict(df=df, x_timestamp=x_ts, y_timestamp=y_ts, pred_len=len(y_ts),
                                 T=T, top_p=top_p, sample_count=1, verbose=False)
    return prob_above(sample_final_closes(once, n_paths), strike)
