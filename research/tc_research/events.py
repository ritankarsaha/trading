"""Event interchange with the Rust engine: one JSON object per line, serde's externally-tagged enum format.

Units: prices are the engine's fixed-point `Px` raw integers (1e-4 of the quote currency) → `px(0.65) == 6500`;
quantities likewise (`qty(10) == 100_000`); timestamps are integer nanoseconds since the Unix epoch.
"""
from __future__ import annotations
import json
from typing import Iterable, Optional, TextIO

SCALE = 10_000


def px(dollars: float) -> int:
    return int(round(dollars * SCALE))


def qty(units: float) -> int:
    return int(round(units * SCALE))


def _lvl(p: float, q: float) -> dict:
    return {"px": px(p), "qty": qty(q)}


def book(instrument: str, ts_ns: int, bids: list[tuple[float, float]], asks: list[tuple[float, float]]) -> dict:
    return {"Book": {"instrument": instrument, "ts": ts_ns,
                     "bids": [_lvl(*b) for b in bids], "asks": [_lvl(*a) for a in asks]}}


def trade(instrument: str, ts_ns: int, price: float, units: float, taker_side: str) -> dict:
    assert taker_side in ("Buy", "Sell")
    return {"Trade": {"instrument": instrument, "ts": ts_ns, "px": px(price), "qty": qty(units),
                      "taker_side": taker_side}}


def settlement(instrument: str, ts_ns: int, yes: bool) -> dict:
    """`yes` MUST come from the venue's own `result` field — never from a self-derived truth."""
    return {"Settlement": {"instrument": instrument, "ts": ts_ns, "price": px(1.0 if yes else 0.0)}}


def signal(instrument: str, ts_ns: int, source: str, prob: float, confidence: Optional[float] = None) -> dict:
    """`ts_ns` = when the signal became AVAILABLE, never the time of the data it describes."""
    if not (0.0 < prob < 1.0):
        raise ValueError(f"probability must be in (0,1), got {prob}")
    return {"Signal": {"ts": ts_ns, "source": source, "instrument": instrument, "prob": prob,
                       "confidence": confidence}}


def write_jsonl(events: Iterable[dict], out: TextIO) -> int:
    n = 0
    for e in events:
        out.write(json.dumps(e, separators=(",", ":")) + "\n")
        n += 1
    return n
