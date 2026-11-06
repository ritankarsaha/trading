#!/usr/bin/env python3
"""Golden vectors for Gaussian bracket math from the skill's own weather_brackets.py (kalshi-weather-markets)."""
import importlib.util, json, pathlib, glob, os

ROOT = pathlib.Path(__file__).resolve().parents[2]
SRC = glob.glob(os.path.expanduser(
    "~/.claude/plugins/cache/agiprolabs-claude-trading-skills/trading-skills/*/skills/kalshi-weather-markets/scripts/weather_brackets.py"))[0]
spec = importlib.util.spec_from_file_location("wb", SRC)
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)

rows = []
for mu in [60.0, 68.3, 74.5, 75.2, 81.9]:
    for sg in [0.5, 1.0, 2.0, 3.5]:
        for floor in range(66, 84, 2):
            rows.append({"k": "bracket", "floor": floor, "cap": floor + 1, "mu": mu, "sigma": sg,
                         "p": m.bracket_p_yes(floor, floor + 1, mu, sg)})
        for strike in (65, 74, 83):
            for kind in ("greater", "less"):
                rows.append({"k": "thr", "strike": strike, "greater": kind == "greater", "mu": mu, "sigma": sg,
                             "p": m.threshold_p_yes(strike, mu, sg, kind=kind)})
rows.append({"k": "mus", "p10": 70.0, "p50": 75.0, "p90": 80.0, "scale": 1.1,
             "out": list(m.mu_sigma_from_quantiles(70.0, 75.0, 80.0, sigma_scale=1.1))})
out = ROOT / "engine/tests/golden/brackets.json"
out.write_text(json.dumps(rows))
print(f"{len(rows)} rows from {SRC}")
