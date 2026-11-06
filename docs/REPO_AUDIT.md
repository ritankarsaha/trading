# Repo Audit — what is in `trading-ultra/`

Audited 2026-11-06. Every folder was opened; READMEs read, package layouts and key source files inspected.
Verdict key: **CORE** = we build directly on its ideas/math · **FEED** = supplies signals/features to the engine · **REF** = reference architecture to learn from · **SKIP** = low value for this project.

> **Licence caution.** Licence files are stripped from most folders, so upstream licences are *unverified*.
> README badges show **nofx = AGPL-3.0** and **FinceptTerminal = AGPL-3.0**; `prediction-market-analysis` = MIT; `claude-trading-skills` = MIT.
> Rule for this project: **port ideas and re-implement; do not copy AGPL code into our engine** (AGPL's network clause would force us to publish the engine's source). Verify the rest upstream before copying any file.

---

## Summary table

| # | Folder | What it really is | Lang | Size | Verdict |
|---|--------|-------------------|------|------|---------|
| 1 | `prediction-market-analysis` | Largest public Polymarket + Kalshi dataset (36 GiB parquet) + indexers + 20 analysis scripts. Evidence base for where prediction-market edge lives | Python | 81 files | **CORE** |
| 2 | `benter_paper (3).pdf` | Benter, *Computer Based Horse Race Handicapping and Wagering Systems* — the canonical "model + market → combined probability → Kelly" paper | PDF | 1.7 MB | **CORE** |
| 3 | `claude-trading-skills` | 68 SKILL.md knowledge packs (Kalshi/Polymarket APIs, Kelly, microstructure, regime detection, walk-forward, slippage, risk…) + `sizing.py` | Markdown + py | 351 files | **CORE** (spec + checklists) |
| 4 | `nofx` | Production Go trading system: unified `Trader` interface over ~10 exchanges, AI decision loop, risk, grid, order sync, position reconciliation, web UI | Go + TS | 830 files, ~98k Go LOC | **REF** (execution architecture) |
| 5 | `FinRL-Trading` (FinRL-X) | Weight-centric pipeline: select → allocate → time → risk-overlay; `bt` backtester; Alpaca execution; adaptive rotation strategy | Python | 83 files | **REF** + **FEED** |
| 6 | `Kronos` | Open foundation model for candlesticks (OHLCV tokenizer + decoder-only Transformer, 4.1M–102M params, HF weights, fine-tune scripts) | Python/PyTorch | 91 files | **FEED** |
| 7 | `TradingAgents` | Multi-agent LLM framework (analysts → bull/bear debate → trader → risk debate → PM), point-in-time-safe data layer, backtest, memory/reflection; has a **Polymarket data vendor** | Python/LangGraph | 212 files | **FEED** (advisory signals) |
| 8 | `autoresearch` | Karpathy's "agent edits one file, fixed 5-min budget, single metric, keep/discard" loop for LLM training | Python | 10 files | **CORE** (as a *method*) |
| 9 | `quant-trading` | je-suis-tm's classic strategy scripts: MACD, RSI, Bollinger, Dual Thrust, Heikin-Ashi, London Breakout, Parabolic SAR, pair trading, VIX calculator, Monte Carlo, Oil/Ore Money | Python | 176 files | **REF** (textbook strategies) |
| 10 | `day-trader-skill` | 11 skills: scanner, news, entry, exit, risk, budget, broker (paper-default, human-confirm), alerts, **Time Machine** (10k-path Monte Carlo + historical replay of a setup) | Python (stdlib) | 84 files | **REF** (risk gate + replay idea) |
| 11 | `FinceptTerminal` | Bloomberg-style desktop terminal: C++20/Qt6 app, ~100 Python data connectors (macro, crypto, India, China…), algo engine, broker adapters | C++/Python | 3,457 files, 65 MB | **REF** (data-source catalogue) / mostly **SKIP** |
| 12 | `awesome-ai-in-finance` | Curated link list: agents, LLMs, papers, data sources, exchange APIs, arbitrage tools | Markdown | 11 files | **REF** (index only) |

---

## 1. prediction-market-analysis — CORE

*Jon Becker's dataset + framework.* Parquet schemas: Kalshi `markets` / `trades` (cents prices, `taker_side`, `result`), Polymarket `markets` / `trades` (on-chain `OrderFilled` events from Polygon). `make setup` downloads the 36 GiB compressed dataset; `make index` re-collects; `make analyze` runs analyses.

Analyses shipped (`src/analysis/`): win-rate by price, calibration by bucket, **maker vs taker returns** (overall, by direction, by category, over time), EV of YES vs NO, mispricing by price, longshot volume share, trade size by role, returns by hour, VWAP by hour, statistical tests.

**Why it matters:** this is our **backtest and calibration substrate** for Kalshi/Polymarket and the place the "takers lose, makers win, longshots are overpriced" claim is measured rather than asserted. Cited research in README: Kalshi "Who Calibrates Markets?", Reichenbach & Walther on Polymarket price informativeness, Becker "Microstructure of Wealth Transfer".

**Gaps:** historical trades only — no order-book snapshots, so passive-fill simulation is an *upper bound* (see live-ops skill below). Data download is large (needs disk planning).

## 2. Benter paper — CORE (pages 1–8 read in full)

Key mechanics, to be implemented verbatim in the research plane:

1. Fundamental model produces probabilities `f_i` (multinomial logit).
2. Public/market-implied probabilities `π_i` come from odds.
3. **Fit a second-stage logit:** `c_i = exp(α·ln f_i + β·ln π_i) / Σ_j exp(α·ln f_j + β·ln π_j)`, with `α, β` by max-likelihood on **out-of-sample** `f` (in-sample `f` overstates the model).
4. Score a model by **ΔR² = R²(combined) − R²(market alone)**, not by its standalone accuracy. Benter's example: ΔR² = 0.0178 was enough for a profitable system; a "tipster" model with R² ≈ the fundamental model added only 0.0002 — i.e. *zero* value once the market is in the regression.
5. Bet where expected return `er = c × payout > 1`; size with **fractional Kelly**, with pool/liquidity limits (rest of paper, pp. 9+ — to be read in Phase 2 when implementing sizing).

Maps directly onto prediction markets: `π` = market price, `f` = our model, `c` = calibrated blend, edge = `c − price − fees`.

## 3. claude-trading-skills — CORE (as spec)

68 skills (only 3 plugin installs are active in Claude Code now). Most relevant:

- **Prediction markets:** `prediction-market-strategy` (edge thesis, fee-aware net edge, θ gates, fractional Kelly, backtest hygiene + "phantom-edge hall of fame", `scripts/sizing.py`), `prediction-market-live-ops` (live-money operating doctrine), `kalshi-api`, `polymarket-api`, `kalshi-weather-markets`, `kalshi-crypto-index-markets`.
- **Sizing/risk:** `kelly-criterion`, `position-sizing`, `risk-management`, `exit-strategies`, `portfolio-analytics`.
- **Research hygiene:** `walk-forward-validation`, `slippage-modeling`, `regime-detection`, `feature-engineering`, `signal-classification`, `market-microstructure(-traditional)`, `volatility-modeling`, `options-pricing`, `mean-reversion`, `cointegration-analysis`.
- **Crypto/DeFi (low priority for us):** Solana/Jito/DEX/MEV/pump.fun, LP math, tax skills.

Key facts extracted (these drive our strategy list — see PROGRESS.md §Strategy thesis):
- Kalshi taker fee `ceil(0.07·p·(1−p)·100)/100` per contract; Polymarket explicit taker fee 0 (spread/gas are the cost).
- Kalshi: avg pre-fee return ≈ −20%, concentrated in takers/longshot buyers. Polymarket: top ~1% of accounts take ~76.5% of profit, mostly resting limit orders.
- Only two strategies survived correct settlement + fees: **maker-side favourite–longshot fade** and **forecast-driven YES entry past a fee-adjusted θ gate**. Cross-venue arb, latency front-running and copy-the-sharps are flagged as blocked/HFT/survivorship.
- Defaults: θ = 15% (<$2k acct) / 20% (≥$2k) on Kalshi, ~5% Polymarket; quarter-Kelly; 1.5% bankroll per contract; 5% per event; 20–25% total exposure.
- Live-ops doctrine: evidence-gated stages with pre-declared kill lines; **print-replay EV of passive fills is an upper bound**; 1-lot round-trip smoke test of every order path; truthful cancels (404 ≠ cancelled); `post_only` for maker; halt-file + drawdown breaker; measure rate limits empirically; house-money ratchet for scaling; append-only ledgers reconciled to venue fills.

## 4. nofx — REF (execution architecture; AGPL — do not copy)

Go module, ~98k LOC. Package map: `trader/` (per-exchange adapters: binance, bybit, okx, bitget, gate, kucoin, hyperliquid, aster, lighter, indodax; `execution/`, `syncloop/`, `position_rebuild`, `runtime_health`, throttle, risk, grid), `kernel/` (AI prompt/decision engine, schema + validation), `market/` (klines, indicators, OI), `provider/` (alpaca, twelvedata, hyperliquid, coinank, nofxos), `mcp/` (multi-LLM client + payment), `store/` (GORM persistence: orders, positions, equity, decisions), `manager/`, `planning/` (trade plans: market/limit/ladder/TWAP), `security/`, `wallet/`, `web/` (React UI).

Patterns worth re-implementing (clean-room):
- One **`Trader` interface** (balance, positions, open/close, stop/TP, limit orders, cancel, trade history) with a **GridTrader** extension; each exchange is a package implementing it → exactly the shape we need for "one engine, many platform folders".
- **Order sync + position reconciliation** against exchange fills (position is rebuilt from fills, not trusted from memory).
- **Throttle, runtime health, risk gate** between the decision and the order.
- **Planning-then-approve** flow (Frontier) vs autonomous (Autopilot) — a good human-in-the-loop switch.

Gaps vs our needs: perps/crypto-centric; no prediction-market venues, no India brokers, no Bayesian/probability calibration layer.

## 5. FinRL-Trading (FinRL-X) — REF + FEED

Weight-centric contract: `w_t = R(T(A(S(X≤t))))` — selection → allocation → timing → risk overlay; the **target weight vector is the only interface** between strategy and execution, identical in backtest and live. Modules: `data/` (Yahoo/FMP/WRDS, SQLite), `strategies/` (ML bucket selection, DRL allocators PPO/SAC, TSMOM, KAMA timing, adaptive rotation with regime filter + walk-forward), `backtest/` (bt), `trading/` (Alpaca manager, trade executor, performance analyzer).

Take: the **weight-vector contract** (we generalise it to "target position per instrument" for non-equity venues), walk-forward discipline, regime/risk-off overlay. Useful for the India-equities legs (Zerodha/Angel) later.

## 6. Kronos — FEED

`KronosTokenizer` quantises OHLCV into hierarchical discrete tokens (s1/s2 bits); `Kronos` decoder-only Transformer; `KronosPredictor.predict(df, x_timestamp, y_timestamp, pred_len, T, top_p, sample_count)` returns forecast OHLCV; `predict_batch` for many series. Models: mini (4.1M, ctx 2048), small (24.7M, ctx 512), base (102.3M, ctx 512) on HF `NeoQuasar/*`. Fine-tune scripts (Qlib and CSV) included. Needs torch; `sample_count>1` gives a **distribution of paths**, which is what we want for probabilities (e.g. P(BTC > K at T) for Kalshi crypto-index brackets, P(close>open) for equities).

Caveats: no claim of tradable edge after costs in the README; treat as **one feature among many** and validate via ΔR²-style gain over the market price. Mac laptop: CPU/MPS inference feasible for small/mini, fine-tuning needs a GPU.

## 7. TradingAgents — FEED (advisory)

LangGraph pipeline: 4 analysts (market, news, sentiment, fundamentals) → bull/bear researchers → research manager → trader → 3-way risk debate (aggressive/conservative/neutral) → portfolio manager; structured outputs, rating scale, decision-log memory with reflection/settlement. Strong **point-in-time/no-look-ahead** data layer, backtest over ticker×date grid, `polymarket.py` data vendor, 10+ LLM providers.

Use: an **LLM "second opinion" feature** for slow, event-driven markets (politics, macro, earnings) and as a research-note generator. Not for latency-sensitive decisions (LLM calls cost seconds + dollars per run). Its decisions must pass the same ΔR² test as any other signal.

## 8. autoresearch — CORE (method, not code)

Three-file contract: immutable `prepare.py` (data + metric), mutable `train.py` (the only thing the agent edits), human-edited `program.md` (agent instructions). Fixed 5-minute budget → experiments are comparable; one scalar metric; keep if better, discard if not; simplicity criterion; `results.tsv` log. The code trains a small GPT on one H100 — **not directly usable on this Mac**; the *loop* is what we copy.

Our adaptation (Phase 2): `evaluate.py` (frozen, leak-free walk-forward harness that returns one scalar, e.g. out-of-sample fee-adjusted EV/$ or ΔR² vs market), `strategy.py` (the agent-editable file), `program.md` (research org instructions), `results.tsv`. Guard rails against the agent overfitting the harness: frozen holdout the agent never sees, pre-registered rules, top-N-share concentration check (from live-ops doctrine).

## 9. quant-trading — REF

Classic indicator strategies (MACD, RSI, Bollinger pattern, Dual Thrust, Heikin-Ashi, London Breakout, Parabolic SAR, Awesome Oscillator, Shooting Star), stat-arb (pair trading; Oil-Money FX vs crude; Ore-Money), options (straddle, VIX calculator per CBOE white paper), Monte Carlo. **Explicitly frictionless** (author states no slippage/fees) — so none of these results can be trusted as-is. Value: textbook reference implementations to port into the engine's strategy library and test *with* costs. Expectation set honestly: plain indicator strategies are the weakest edge class in this folder.

## 10. day-trader-skill — REF

Stdlib-only Python tools over free public data (Yahoo, Nasdaq symbol lists, SEC EDGAR, House Clerk). Worth porting conceptually: `trader-risk` (limit check before any order), `trader-budget` (size to max-loss-per-trade), `trader-broker` (**paper by default, human yes required**), `trader-timemachine` (10,000 bootstrap replays of the rest of the session from the stock's own 5-min bars + historical replay of the same rule → P(target first / stop first / neither)). The Time Machine is a cheap, useful **pre-trade sanity gate** for intraday equity legs.

## 11. FinceptTerminal — REF / mostly SKIP

C++20/Qt6 desktop app (3,457 files; AGPL-3.0; README upsells paid "Enterprise"/"Quantcept"). Components: `algo_engine/` (condition evaluator, indicator engine, backtest engine, position manager, F&O), `trading/` (account manager, broker adapters), `datahub/`, `mcp/`, and **~100 Python data scripts** (`fincept-qt/scripts/`: macro — BLS, BEA, BIS, ECB, BoJ, FRED-style; crypto — CoinGecko, DefiLlama, Coinglass; India-relevant data; CBOE VIX; CFTC; congress; shipping/AIS; etc.).

Value for us is the **data-source catalogue** (what exists and how each API is shaped), not the app. Building our own terminal UI is out of scope until the engine is profitable on paper.

## 12. awesome-ai-in-finance — REF

Link index. Useful sections: Agents (ATLAS, OpenFinClaw, **oracle3** — Kalshi/Polymarket/DFlow agent with Wang-transform pricing & arbitrage, ProfitPlay), Papers, Data Sources, Exchange API, Arbitrage. We mine it for follow-up repos when a phase needs one; we do not vendor it.

---

## Cross-repo observations (these shape the design)

1. **The strongest, best-evidenced edge in the whole folder is structural, not predictive:** be a *maker* in prediction markets and sell overpriced longshots. Predictive models (Kronos, LLM agents, DRL) are credible only as *additive features validated by ΔR² against the market price* (Benter's test), not as stand-alone alpha.
2. **Everyone in the evidence is bitten by the same bug class:** look-ahead, wrong settlement source, phantom depth, shadow/live conflation, in-sample calibration. The harness must make these *impossible by construction*, not by discipline.
3. **Execution architecture is solved prior art** (nofx `Trader` interface, order sync/reconcile, throttle, health, halt/breaker). We re-implement it as a `Venue` trait in a fast systems language.
4. **There is no existing code in the folder for:** Zerodha, Angel One, Glimpse, Kalshi/Polymarket *order placement* beyond skill docs, a unified cross-venue risk book, or a live paper-vs-live attribution ledger. Those are what we build.
