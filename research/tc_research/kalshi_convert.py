"""prediction-market-analysis Kalshi parquet → engine events (trades + venue-reported settlements).

LIMITATION (important): the dataset has TRADES, not order-book snapshots. Trades + settlements can drive
trade-based studies and calibration, but a *maker* strategy cannot be replayed honestly without book depth —
inventing depth from trades is exactly the 'phantom depth' bug. Maker strategies need recorded book snapshots
(Phase 2: record live books from venue feeds).

Instrument id = f"{event_ticker}:{ticker}" (the YES contract); the engine groups risk by the part before ':'.
Settlement uses the venue's own `result` ('yes'/'no') — never a self-derived truth. Unresolved markets are skipped.
"""
from __future__ import annotations
import argparse
import pandas as pd
from . import events as E


def _ns(series: pd.Series) -> pd.Series:
    return pd.to_datetime(series, utc=True).astype("int64") if pd.api.types.is_datetime64_any_dtype(series) \
        else pd.to_datetime(series, utc=True).astype("int64")


def convert_frames(markets: pd.DataFrame, trades: pd.DataFrame) -> list[dict]:
    mk = markets.set_index("ticker")
    evs: list[tuple[int, int, dict]] = []   # (ts, order, event): trades before same-instant settlement
    t = trades[trades["ticker"].isin(mk.index)].copy()
    t["ts"] = _ns(t["created_time"])
    for r in t.itertuples(index=False):
        iid = f"{mk.at[r.ticker, 'event_ticker']}:{r.ticker}"
        side = "Buy" if r.taker_side == "yes" else "Sell"     # taker bought NO == sold YES at yes_price
        evs.append((int(r.ts), 0, E.trade(iid, int(r.ts), r.yes_price / 100.0, float(r.count), side)))
    m = markets[markets["result"].isin(["yes", "no"])].copy()
    m["ts"] = _ns(m["close_time"])
    for r in m.itertuples(index=False):
        iid = f"{r.event_ticker}:{r.ticker}"
        evs.append((int(r.ts), 1, E.settlement(iid, int(r.ts), r.result == "yes")))
    evs.sort(key=lambda x: (x[0], x[1]))
    return [e for _, _, e in evs]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--markets", required=True, help="parquet file or dir of markets")
    ap.add_argument("--trades", required=True, help="parquet file or dir of trades (filter first — it is huge)")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    with open(a.out, "w") as f:
        n = E.write_jsonl(convert_frames(pd.read_parquet(a.markets), pd.read_parquet(a.trades)), f)
    print(f"wrote {n} events -> {a.out}")


if __name__ == "__main__":
    main()
