# trading-ultra — Progress Log

Single source of truth for what we are building, what is done, and why. Newest entries at the bottom of the Log.
Companion: [`docs/REPO_AUDIT.md`](docs/REPO_AUDIT.md) (what every folder contains).

**Status board** (updated 2026-11-06, end of Step 1)

| Step | Description | Status |
|------|-------------|--------|
| 0 | Audit all repos + plan phases | ✅ DONE |
| Phase 1 | Engine core + paper venue | ✅ DONE (85 Rust + 18 Python tests, 3 golden suites) |
| Phase 2 | Data, calibration & leak-free research harness | 🟡 PARTIAL — Benter fit + ΔR² gate, event interchange, Kalshi converter done; DuckDB layer, walk-forward harness, autoresearch loop, real-data calibration NOT done |
| Phase 3 | Alpha library | 🟡 PARTIAL — 6 `quant-trading` strategies + pairs + longshot fade + θ-gated edge ported; Kronos/TradingAgents adapters written but NOT run against the real models; FinRL weight contract, regime/vol features, MC gate NOT done |
| Phase 4 | Execution, risk rails & live-ops | 🟡 PARTIAL — risk gate, halt/breaker, truthful cancels, conformance suite done; live order manager, venue reconciliation, smoke-test tool, separate breaker process, ratchet NOT done |
| Phase 5 | Venue rollout & scale | ⬜ not started — `venues/_template` + runbook rules ready |

---

## 1. Ground rules (read this first)

**No guaranteed profits.** The evidence inside these repos is unusually honest about this, and the plan follows it:

- Kalshi's average pre-fee return is ≈ −20% (takers/longshot buyers lose; makers earn). On Polymarket ~1% of accounts take ~76.5% of profit. A naïve bot is on the *losing* side of that split.
- Most published backtests in this folder are frictionless (`quant-trading` says so itself) or contain look-ahead; `prediction-market-strategy` documents backtests that showed +1640% and were pure bugs.
- A better forecast is **not** a trading edge when the market already prices it. The Benter test is the arbiter: does the model add ΔR² on top of the market price?

So "maximum profit" in this project means: **maximise risk-adjusted, fee-net expected value, and make it provable before capital is risked.** Concretely:

1. Every strategy goes through `backtest → paper → live micro-probe → ratchet`, with a **pre-declared kill line** written before data arrives.
2. Real money only after a strategy clears paper on that venue. Paper is the default mode; live needs an explicit flag **and** a passing smoke test.
3. Start with small, capped capital per venue; scale only on realised, venue-reconciled PnL (house-money ratchet).
4. I will not place any live order or touch any account without your explicit go-ahead at that step.

## 2. Target architecture

```
                         ┌───────────────────────── research plane (Python) ─────────────────────────┐
 history (parquet) ────► │ data store · calibration (Benter logit) · walk-forward harness ·          │
 prediction-market-      │ autoresearch loop (agent edits strategy.py, frozen evaluate.py)           │
 analysis dataset        │ feature/signal models: Kronos, TradingAgents, regime, vol                 │
                         └───────────────┬───────────────────────────────────────────────────────────┘
                                         │ versioned model artefacts + signal feed (Arrow IPC / gRPC)
                         ┌───────────────▼──────────── CORE ENGINE (Rust) ───────────────────────────┐
                         │ events → strategy → target positions → PORTFOLIO/RISK → order manager     │
                         │ same code path for backtest · paper · live (only the Venue differs)       │
                         │ append-only ledger · reconciliation vs venue fills · halt file · breakers │
                         └───┬───────────┬───────────┬───────────┬───────────┬───────────────────────┘
                             │           │           │           │           │      trait Venue { … }
                        venues/glimpse venues/polymarket venues/kalshi venues/zerodha venues/angelone
                        (each folder = adapter crate + venue-specific config, fees, calendars, rules)
```

### Language decision (my call)

| Layer | Language | Why |
|-------|----------|-----|
| Core engine, order manager, risk, ledger, venue adapters, simulator | **Rust** (1.97 installed) | Memory-safe, no GC pauses, single static binary, the type system can make look-ahead and unit mix-ups (cents vs dollars) unrepresentable, excellent async (tokio) for WebSockets. For a system that moves real money, a compile-time guarantee beats the speed of writing Python. |
| Research, calibration, ML signals (Kronos, FinRL, TradingAgents), notebooks | **Python 3.12 + uv** (installed) | All the reference repos and the ML ecosystem are Python; research velocity matters more than latency here. |
| Bridge | Arrow/Parquet files + a local gRPC/ZeroMQ signal channel | Python never sends orders; it only publishes *signals/probabilities*. Orders only ever leave from the Rust engine, behind the risk gate. |
| UI/terminal | deferred | Out of scope until the engine is paper-profitable. |

Not chosen: Go (nofx is Go, but we gain nothing over Rust and lose the type-level guarantees), C++ (Fincept; unsafe by default), all-Python (fine for research, risky for a 24/7 order path). Latency note: the evidence says the profitable prediction-market niche is **maker/patience**, not sub-100 ms racing, so raw speed is *not* our bottleneck — correctness is. Rust is chosen for correctness first.

### Core abstractions (to be built in Phase 1)

- `Instrument` (venue, native id, tick size, lot, fee model, settlement spec, trading calendar) — **units are newtypes** (`Cents`, `Prob`, `Usd`, `Inr`); no bare `f64` prices.
- `Venue` trait: `subscribe`, `snapshot_book`, `place(order{post_only,tif,client_id})`, `cancel -> {Cancelled|AlreadyGone|Failed}` (truthful cancels), `fills`, `positions`, `balances`, `rate_limit_profile`. Implementations: `SimVenue` (backtest/paper), then real adapters.
- `Strategy` trait: consumes `Event`s with an **as-of clock** (data after `t` is inaccessible by API), emits `TargetPosition`s — the weight-vector idea from FinRL-X generalised to any instrument.
- `RiskGate`: per-order/market/event/venue/total caps, fractional-Kelly sizing, drawdown breaker (with deposit-jump detection), halt-file, equity floor. Nothing reaches a venue without passing it.
- `Ledger`: append-only, fills cohort-tagged at write time (maker/taker, strategy, band, source); PnL is computed **only from venue-confirmed fills/settlements**.

## 3. Strategy thesis (ranked by evidence in the repos)

| Rank | Strategy | Evidence | Venues |
|------|----------|----------|--------|
| 1 | **Maker-side favourite–longshot fade** (rest NO bids on ~$0.05–$0.20 brackets, diversified across many events) | Kalshi/Polymarket calibration + maker-vs-taker analyses; fee-inclusive CV backtest +7.9–9% maker | Kalshi, Polymarket, Glimpse (if it has comparable binary markets — to verify) |
| 2 | **Calibrated-model YES entry past θ gate** — Benter two-stage logit blend of our model with market price | Benter paper; `prediction-market-strategy` θ gates | Same |
| 3 | **Passive market-making** (two-sided quotes, spread + rebates) | Structurally favoured, inventory/adverse-selection risk, queue priority needed | Kalshi/Polymarket later |
| 4 | **Overround/dutching arb** within one venue | Real but marginal; legs fill sequentially | After 1–2 are live |
| 5 | **Equity/F&O signals** (TSMOM, regime-filtered rotation, Kronos/LLM features) with cost-aware backtests | FinRL-X, Kronos, TradingAgents — evidence of *net* edge is weaker; must clear ΔR²/walk-forward | Zerodha, Angel One |
| — | Cross-venue arb, latency front-running, copy-the-sharps, frictionless indicator systems | Flagged blocked / HFT / survivorship / unrealistic | **Not pursued** (revisit only with new evidence) |

## 4. The five phases

Each phase ends with a demonstrable artefact and an **exit gate**; we don't move on until it passes.

### Phase 1 — Engine core + paper venue  *(est. 1–2 sessions)*
**Build:** Rust workspace (`engine/`): core types with unit newtypes, event bus + as-of clock, `Venue` trait, `SimVenue` (limit-order-book matching with queue position, fees, partial fills), `RiskGate`, `Ledger`, config, structured logging, halt-file. Port `sizing.py` (Kalshi fee, net edge, θ, fractional Kelly, slippage guard) to Rust with golden tests against the Python original.
**Exit gate:** deterministic replay of a recorded event stream reproduces identical ledger bytes; risk gate property-tests (fuzz: no order can exceed caps); golden tests pass.

### Phase 2 — Data, calibration & leak-free research harness  *(1–2 sessions)*
**Build:** `research/` (Python/uv): ingest the `prediction-market-analysis` parquet into a query layer (DuckDB); settlement always from the venue's own `result`; decision-time snapshots; Benter two-stage logit + ΔR² scorer (read paper pp. 9+ for Kelly/pool limits first); walk-forward and expanding-window validators; **autoresearch-style loop** (`evaluate.py` frozen, `strategy.py` agent-edited, `program.md`, `results.tsv`) with a held-out period the agent can't see; concentration (top-N share) and both-halves checks baked into the scorer.
**Exit gate:** harness reproduces the repo's published maker/taker and longshot-bias findings on the real dataset; a deliberately leaky strategy is *rejected* by the harness (red-team test).

### Phase 3 — Alpha library  *(2–3 sessions)*
**Build:** longshot-fade and θ-gated YES strategies (as Rust `Strategy` + Python model signals); Kronos wrapper that emits path-sample probabilities (P(price>K at T)); TradingAgents-as-feature adapter for slow event markets; regime/volatility features; equity TSMOM/rotation port with costs; pre-trade Monte-Carlo sanity gate (Time-Machine idea).
**Exit gate:** each strategy has a written pre-declared kill line, a passing walk-forward with fees, and a ΔR² > 0 over market price on held-out data — or it is explicitly *not promoted*.

### Phase 4 — Execution, risk rails & live-ops  *(1–2 sessions)*
**Build:** order manager with idempotent client ids, truthful cancel states, restart-safe state rebuilt from venue fills (nofx-style reconcile), rate-limit probing with dust orders, `post_only` enforcement for maker strategies, independent drawdown-breaker process, kill switch, 1-lot round-trip smoke-test tool, paper↔live attribution ledger, alerting, house-money ratchet controller, decision-record template.
**Exit gate:** chaos tests (kill -9 mid-order, venue 429/5xx/disconnect, duplicate fills) leave positions and ledger consistent; smoke-test tool passes against the paper venue.

### Phase 5 — Venue rollout & scale  *(one venue at a time)*
Per-venue folder `venues/<name>/` = adapter crate + `venue.toml` (fees, tick/lot, calendar, limits) + runbook + tests + its own PROGRESS section. Order:
1. **Glimpse** — first integration and paper/test run (per your instruction).
2. **Polymarket** — official client signing (EIP-712), CLOB + Gamma, Polygon settlement; geo/KYC constraints apply.
3. **Kalshi** — REST + lifecycle WebSockets, exchange-shard routing, cents pricing; US-regulated, access constraints apply.
4. **Zerodha (Kite Connect)** and 5. **Angel One (SmartAPI)** — India equities/F&O; need daily-token login flow, SEBI/exchange rules (order-rate limits, algo-registration requirements), market calendar.
**Per-venue gate:** adapter passes shared conformance suite → 1-lot smoke test → paper N days → live micro-probe → ratchet. Scale only on venue-reconciled realised EV.

## 5. Open questions (blocking Phase 5, not Phase 1)

1. **Glimpse:** what is it (equities? prediction markets? crypto?), does it have a sandbox/paper mode, and where are the API docs/keys? *(A `glimpse-wiki` MCP server is configured in this Claude session — I'll read it when we start the Glimpse step unless you'd rather paste docs.)*
2. **Capital & risk tolerance** per venue (needed to set breaker baselines and the Kelly bankroll).
3. **Jurisdiction/compliance**: Polymarket/Kalshi access from India, and Zerodha/Angel algo-trading registration rules. I'll flag requirements per venue; you remain responsible for eligibility.
4. **Data budget:** the Polymarket/Kalshi dataset is 36 GiB compressed — confirm disk space before Phase 2 (this machine: 8 cores, 8 GB RAM; DuckDB streaming keeps memory OK).
5. **Compute for ML:** Kronos fine-tuning wants a GPU; inference on Mac is fine. Decide later whether to rent one.

## 6. Repo layout (as built)

```
trading-ultra/
├── PROGRESS.md                 # this file
├── docs/REPO_AUDIT.md          # what every reference repo contains
├── engine/                     # Rust workspace — THE common engine (see engine/README.md)
│   ├── crates/tc-core          #   units (Px/Qty/Money/Prob/Ts), instruments, orders, events, Venue trait, sizing + Benter
│   ├── crates/tc-ledger        #   cost-basis portfolio, append-only hash-chained ledger
│   ├── crates/tc-sim           #   simulated venue: queue-aware maker fills, depth-consuming takers, latency, post-only
│   ├── crates/tc-risk          #   RiskGate: caps, breaker, halt file, fat-finger, rate limit, post-only enforcement
│   ├── crates/tc-strategy      #   Strategy trait, indicators, bar strategies, pairs, longshot fade, signal edge
│   ├── crates/tc-backtest      #   Engine (one loop for backtest/paper/live), Report, synthetic data
│   ├── crates/tc-conformance   #   contract every Venue adapter must pass
│   ├── crates/tc-cli           #   `tc` binary: demo-pm | demo-bars | determinism | replay
│   └── tests/golden/           #   vectors generated from the ORIGINAL Python sizing.py
├── research/                   # Python (uv): events interchange, Benter fit/ΔR², Kronos/TradingAgents/Kalshi adapters
├── venues/                     # one folder per platform; _template/ + README (gate to go live)
└── (reference repos, untouched, read-only)
```
Reference repos stay where they are; we never edit them.

---

## 7. Log

### 2026-11-06 — Step 0: audit & plan  ✅
- Installed `trading-skills` plugin from the `agiprolabs/claude-trading-skills` marketplace (68 skills now available in-session).
- Audited all 11 folders + Benter PDF (pages 1–8 read; pp. 9+ on Kelly/pool limits deferred to Phase 2). Findings in `docs/REPO_AUDIT.md`.
- **Decisions:** (D1) Rust core engine + Python research plane, orders only leave from Rust behind the risk gate. (D2) Re-implement, don't copy, AGPL code (nofx, Fincept). (D3) Strategy priority: maker longshot fade → Benter-blended YES entries → market-making; ML/LLM models are *features* judged by ΔR² vs market price. (D4) Paper-by-default; live only with explicit flag + smoke test + your approval. (D5) Autoresearch method adopted for Phase 2 with a frozen evaluator and hidden holdout.
- **Observations:** licence files are missing from most folders (verify upstream before copying any code); `quant-trading` assumes zero costs so its strategies are references only; Fincept is mostly an app, valuable only as a data-source catalogue; no folder has Zerodha/Angel/Glimpse code.
- **Not done (by design):** no code, no accounts touched, no network/API calls to any venue.
- **Next:** your go-ahead for Phase 1 and answers to §5 Q1–Q2 (Glimpse docs, capital/risk tolerance).

### 2026-11-06 — Step 1: common engine built  ✅ (Phases 1 done; 2–4 partially)
**Built:** Rust workspace (`engine/`, ~4.9k LOC, 58 tests) + Python research package (`research/`, 13 tests) + venue template.
- **Units are types.** `Px/Qty/Money` are fixed-point integers (1e-4); `Prob` can't hold NaN or 1.3; orders are limit-only (a "market" order is an IOC at a protective price).
- **Correctness evidence (not just "tests pass"):**
  - Sizing port is checked against the **original Python `sizing.py` on 2,793 grid points** (golden test).
  - Replay is **bit-for-bit deterministic** (identical ledger digest across runs; changes when data changes).
  - Books close **exactly**: after all markets settle `cash == start + Σrealised − fees`, including a 500-trade odd-lot fuzz test.
  - Maker strategy provably never takes liquidity (`taker_fills == 0`); breaker trips on a wrong-regime world and cancels all resting orders.
  - Conformance suite passes on the sim **and demonstrably catches a venue that lies about cancels**.
  - Cross-language: Python-written events replay through the Rust binary; malformed probabilities are refused.
- **Bugs found and fixed by those tests (worth remembering):**
  1. Average-price accounting leaked money via integer truncation → switched to **cost-basis** accounting (found by the books-close test, then a flip-truncation variant by the fuzz test).
  2. Parabolic SAR clamped against the *current* bar's low, so it could never reverse → clamp uses the previous two bars.
  3. Settlement PnL was not attributed to the strategy cohort → positions remember the opening tag.
  4. The Python original's float fee formula can overcharge a cent (`175.00000000000003 → 176`); the engine uses **exact integer** Kalshi fees and keeps the float version only as the golden reference.
  5. A test constant I wrote from memory ("Wilder RSI = 70.46") was wrong; replaced with an independently computed value (70.5328).
- **What the demos show (SYNTHETIC data — plumbing only):** the longshot fade makes money when longshots are overpriced by construction, is flat on a calibrated control, and loses when longshots are *underpriced* — i.e. the edge is exactly as real as the bias in the data. The ported indicator strategies (MACD, RSI, Bollinger, Dual Thrust, Heikin-Ashi, SAR) mostly lose to fees+spread on a near-random walk, as the audit predicted.

**Known limitations / not yet true (read before trusting anything):**
- `LongshotFade` uses a **placeholder prior** (`true_p = price × 0.6`). It must be replaced by a table fitted on real settled data (Phase 2). Until then it has no demonstrated real-world edge.
- Sim maker-fill model (queue position, trade-through) is conservative but **unvalidated against a real venue**; bar-based resting fills are optimistic by design (capped by bar volume).
- `LongshotFade`/`SignalEdge` re-submit every tick after a risk reject (2,481 rejects vs 440 orders in the demo, all `total_cap`). Harmless in sim, wasteful live → needs back-off.
- Kalshi dataset has trades, **not order books**: maker strategies cannot be replayed on it honestly. Phase 2 must *record* live book snapshots.
- Kronos and TradingAgents adapters are unit-tested with fakes only; neither model has been downloaded or run (8 GB RAM Mac; waiting for the bigger laptop).
- No live path exists in the binary. The `Venue` trait is synchronous; live adapters will own their async runtime internally.
- Not yet read: Benter paper pp. 9+ (Kelly with pool limits) — needed when we add liquidity-aware sizing.

**Next (after you move to the bigger-RAM laptop):** (1) download the 36 GB dataset, run Phase 2 (DuckDB layer, walk-forward + autoresearch harness, fit the real longshot calibration and Benter α/β, add book recorder); (2) run Kronos for real and see whether ΔR² > 0; (3) then Glimpse: need its API docs/sandbox, capital, and risk tolerance.

### 2026-11-06 — Step 1b: skills actually invoked and folded into the engine  ✅
Previously I had only *read* some `SKILL.md` files. Now the `Skill` tool was used for 13 skills and their rules were turned into tested code.
**Invoked:** `kelly-criterion`, `risk-management`, `walk-forward-validation`, `slippage-modeling`, `kalshi-api`, `polymarket-api`, `position-sizing`,
`regime-detection`, `market-microstructure`, `exit-strategies`, `strategy-framework`, `kalshi-crypto-index-markets`, `kalshi-weather-markets`
(+ `prediction-market-strategy` / `-live-ops` read earlier).

| Skill | What changed in the engine (all tested) |
|---|---|
| kelly-criterion / position-sizing | Wilson lower bound; **evidence-gated Kelly** (0.10× <30 results → 0.25× → 0.5×, capped by config); correlation scaling; edge classes; fixed-fractional / **volatility-adjusted sizing with a notional cap** (binding constraint wins) |
| risk-management | graduated **drawdown ladder** (size ×0.75/0.5/0.25), hard stop 15%, **daily loss limit** (UTC day), **weekly ladder**, **loss-streak breaker** (3/5/7 → ×0.5/×0.25/24h block); `size_scale()` is the most conservative of all ladders and is 0 whenever new risk is blocked so strategies stand down instead of spamming |
| prediction-market-live-ops | **pre-declared kill line** (≥50 realisations with negative net PnL ⇒ stand down); `on_reject` back-off (rejects 2,481 → 32 in the demo) |
| kalshi-api | bids-only book → `kalshi_book_from_ladders`; **venue-safe client ids** (`[A-Za-z0-9-]`, ≤36 — my old ids with `_`/`:` would have been rejected); truthful-cancel mapping; order-schema notes in `docs/VENUE_NOTES.md` |
| kalshi-weather / crypto-index | Gaussian bracket math with the mandatory **±0.5 continuity correction** for integer settlement and none for continuous price; quantile→(μ,σ); overround — **golden-tested vs the skill's own Python on 301 cases** |
| slippage-modeling | `walk_ladder` (depth-weighted price + shortfall, never pretends the book absorbs size) |
| regime-detection | ADX, ATR-percentile (mid-rank), 4-quadrant classifier, `RegimeGated` (trend signals only when trending, reversion only when ranging & not high-vol) |
| exit-strategies | hard ATR stop (never widened), unprofitable-time stop, post-stop cooldown |
| walk-forward-validation | `research/validation.py`: purged + embargoed rolling/expanding splits, **Deflated Sharpe**, **PBO (CSCV)** |
| day-trader-skill (timemachine idea) | `research/timemachine.py` bootstrap pre-trade gate (ties resolve against us) |
| strategy-framework | lifecycle gate adopted as policy (backtest → paper → small live → scale; ≥100 trades, OOS Sharpe>1, retire on rolling Sharpe<0) — enforced via kill line + evidence-gated sizing; written strategy specs still to do per strategy in Phase 3 |
| market-microstructure | DEX/Solana-flow oriented → mostly not applicable; kept only the buy/sell-pressure idea for a later trade-flow feature |
| FinceptTerminal | not copied (AGPL); mined for **prior art**: working Zerodha/Angel One/Fyers/Upstox adapters and a Kalshi REST wrapper → `docs/VENUE_NOTES.md` |

**Bugs found by the new tests:** percentile rank read a flat market as "extreme volatility" (ties → now mid-rank with a 0.5% tolerance for converging smoothers); risk-%-sizing implied ~2× leverage in quiet markets and was silently rejected every bar (now notional-capped); risk ladders interacted (a 6% drawdown is also a weekly loss) — behaviour is right (most conservative wins), tests now isolate and assert it; my DSR test had the monotonicity backwards below the luck threshold (the formula was right).

**Evidence (SYNTHETIC — mechanisms, not profit):** regime-switching market where regimes are built in: plain MACD/Bollinger/RSI **lose** (sum −$81.7k over 6 seeds×3) vs gated+ATR-stop+vol-sized **+$14.7k**. Longshot fade ordering holds across seeds: overpriced > calibrated > underpriced. 85 Rust + 18 Python tests, 3 golden suites.

**Skills NOT yet loaded (mapped to the phase that needs them — loading them now would add text without changing code):**
Phase 2: `portfolio-analytics`, `volatility-modeling`, `feature-engineering`, `signal-classification`, `ohlcv-processing`, `trade-journal`, `trade-accounting`, `pandas-ta`/`ta-lib`/`vectorbt`/`backtrader` (research tooling) · Phase 3: `mean-reversion`, `cointegration-analysis` (fit β for `PairsZ`), `correlation-analysis` (group caps), `sentiment-analysis`, `rl-execution`, `market-microstructure-traditional`, `options-pricing` (VIX/straddle from `quant-trading`), `copy-trading` (skill itself flags it as a survivorship illusion) · Out of scope (Solana/DeFi/tax): `birdeye/helius/jito/pumpfun/dex-*/lp-math/impermanent-loss/mev/token-*/sybil/whale/wallet`, `*-tax*`, `regulatory-reporting`, `fixed-income`.

**Still true / not done:** longshot calibration is a placeholder; Kronos/TradingAgents unrun; no live path; sim fill model unvalidated against a real venue; Benter pp. 9+ unread.
