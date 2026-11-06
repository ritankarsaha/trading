# venues/

One folder per trading platform. Every venue uses the **same engine** (`../engine`): strategies, risk gate,
ledger, backtester and sim are shared. A venue folder contains only what is specific to that platform:

```
venues/<name>/
├── Cargo.toml        # own workspace; path-depends on ../../engine/crates/*
├── src/              # `impl Venue` adapter: auth, REST/WS, order+fill mapping, rate limits
├── venue.toml        # fees, tick/lot rules, trading calendar, limits, risk-limit overrides
├── RUNBOOK.md        # how to run, credentials, geo/compliance notes, incident playbook
└── tests/            # conformance suite + recorded-fixture tests (+ sandbox smoke test)
```

Start by copying `_template/`. **Gate to go live (all required, in order):**
1. `tc_conformance::run_all` passes against the venue's sandbox/mock.
2. 1-lot round-trip smoke test (place → verify resting → cancel → verify gone) on the real venue.
3. Paper-trade N days; compare paper vs live fill quality.
4. Live micro-probe at dust size with a pre-declared kill line; ratchet only on venue-reconciled realised PnL.

Planned order: **glimpse → polymarket → kalshi → zerodha → angelone**.
Credentials live in `.env` (gitignored), never in code, never in chat.
