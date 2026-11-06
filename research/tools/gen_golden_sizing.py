#!/usr/bin/env python3
"""Generate golden vectors from the ORIGINAL sizing.py so the Rust port (tc-core::sizing) is checked against
the source of truth rather than against our own expectations.  Run: python3 -I research/tools/gen_golden_sizing.py"""
import importlib.util, json, pathlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
SRC = ROOT / "claude-trading-skills/skills/prediction-market-strategy/scripts/sizing.py"
spec = importlib.util.spec_from_file_location("sizing", SRC)
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)

rows = []
for pi in range(1, 20):
    p = round(pi * 0.05, 2)
    for ai in range(1, 50):
        ask = round(ai * 0.02, 2)
        for bank in (500.0, 5000.0, 50000.0):
            edge = m.net_edge(p, ask)
            rows.append({
                "p": p, "ask": ask, "bank": bank,
                "fee": m.kalshi_fee(ask),
                "edge": edge,
                "theta": m.theta_for_account(bank),
                "limit_c": m.limit_price_cents(p, m.theta_for_account(bank)),
                "kelly": m.kelly_contracts(p, ask, bank),
                "slip_ok": m.slippage_ok(edge, 0.03),
            })
out = ROOT / "engine/tests/golden/sizing.json"
out.write_text(json.dumps(rows))
print(f"wrote {len(rows)} rows -> {out}")
