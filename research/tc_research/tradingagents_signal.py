"""TradingAgents → probability signal (ADVISORY, slow, costs LLM dollars per call).

The 5-tier rating is mapped to an *uncalibrated prior* probability. That is acceptable only because the value is
used as a feature `ln f` in the Benter blend, where α is fitted out-of-sample: if the LLM has no information,
α → 0 and the signal is ignored automatically. 'REVIEW' (unparseable decision) yields NO signal, never a Hold.
"""
from __future__ import annotations
import sys
from pathlib import Path
from typing import Optional

RATING_PRIOR = {"Buy": 0.65, "Overweight": 0.57, "Hold": 0.50, "Underweight": 0.43, "Sell": 0.35}


def rating_to_prob(rating: str) -> Optional[float]:
    return RATING_PRIOR.get(rating)  # None for 'REVIEW' / anything unknown


def run_tradingagents(repo: str | Path, ticker: str, date: str, config: dict | None = None) -> Optional[float]:
    """Run the full agent graph for (ticker, date) and return the mapped probability, or None."""
    sys.path.insert(0, str(repo))
    from tradingagents.default_config import DEFAULT_CONFIG  # type: ignore
    from tradingagents.graph.trading_graph import TradingAgentsGraph  # type: ignore
    ta = TradingAgentsGraph(debug=False, config={**DEFAULT_CONFIG, **(config or {})})
    _state, signal = ta.propagate(ticker, date)
    return rating_to_prob(signal)
