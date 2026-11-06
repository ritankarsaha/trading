# Venue adapter notes (distilled from the skills + prior art; verify against live docs before coding)

Sources: `trading-skills:kalshi-api`, `polymarket-api`, `kalshi-weather-markets`, `kalshi-crypto-index-markets`,
`prediction-market-live-ops`; Fincept `fincept-qt/src/trading/brokers/{zerodha,angelone,fyers,upstox}` and
`scripts/prediction_kalshi.py` (AGPL — **read for API shape, never copy**).
The skills themselves warn: *endpoints and schemas have broken backward compatibility before — smoke-test against a live response.*

## Kalshi
| Topic | Rule | Engine consequence |
|---|---|---|
| Host / auth | `https://api.elections.kalshi.com/trade-api/v2`; RSA-PSS (SHA-256, MGF1, salt = digest len) over `{ts_ms}{METHOD}{path}`; path includes `/trade-api/v2`, **excludes query string**; ms not seconds | adapter-private; conformance can't test auth → smoke test must |
| No public endpoints | every call signed; demo book is near-empty & rate-limits at ~24 calls | paper-trade against the **sim**, not Kalshi demo |
| Order book | YES and NO are both **bid** ladders; `yes_ask = 1 − best_no_bid` | `tc_core::markets::kalshi_book_from_ladders` ✅ (tested) |
| Order body | dollar **strings**: `count_fp:"1.00"`, `yes_price_dollars:"0.01"`; `time_in_force` required (`good_till_canceled`/`immediate_or_cancel`/`fill_or_kill`); do **not** send `type` | map `Tif::{Gtc,Ioc}`; add `Fok` if a strategy needs it |
| `client_order_id` | `[A-Za-z0-9-]` only, bounded length | `sanitize_client_id` on every engine id ✅ (previously `_`/`:` would have 400'd) |
| Cancel | cancelling a filled order → **400**, not a no-op | map 400-on-cancel → `AlreadyGone`, never `Cancelled` ✅ matches `CancelResult` |
| Fees | taker ≈ 7% of potential profit, `ceil(0.07·C·p(1−p)·100)/100`; maker pays none; skill says fees are **deducted at settlement** | engine books the fee at fill (cash timing differs, total identical) — reconcile to venue fills/settlements |
| Settlement | use the venue's `result`; weather = NWS CLI, LST (no DST), integer °F, brackets **both-ends-inclusive**; `strike_type` must be read from the API, **never inferred from the ticker**; date lives in the ticker, not `close_time` | `bracket_p_yes_int` (±0.5 correction) ✅ golden-tested vs the skill's script; adapters must carry `strike_type` |
| Index/crypto | continuous price brackets: **no** ±0.5; hourly crypto is the most HFT-contested market; ~50-trade evidence for the index longshot-sell, crypto inconclusive | `bracket_p_yes_cont` ✅; do **not** trade hourly crypto with retail latency |
| Rate limits | measure with dust orders; advertised budgets can be 10× off | live-ops probe tool (Phase 4) |

## Polymarket
| Topic | Rule |
|---|---|
| Stack | CLOB (off-chain match, on-chain settle) on Polygon (chain 137), USDC collateral, **0 taker fee**, tick $0.01 |
| IDs | `condition_id`, `clob_token_ids` (YES/NO pair), `token_id` (single outcome), `event_id` — mixing them silently returns wrong data. YES and NO are **separate tokens** (our `Instrument` per token) |
| Gamma API | default sort is **oldest-first** — always pass `order=endDate&ascending=false&closed=false` or you get dead markets |
| Auth | use the official `py-clob-client` (EIP-712 + API-cred derivation); `signature_type=2` for UI/Safe accounts, `0` for raw EOA; keys via env only |
| After resolution | winning tokens must be **redeemed on-chain** (`redeemPositions`) — not auto-swept; disputes go to UMA |
| Access | **US persons are geo-blocked**; origin IP matters (compliance posture = datacenter location). User must confirm eligibility |
| Gate | ~5% minimum edge (no taker fee, spread + gas are the cost) |

## India brokers (Zerodha / Angel One) — prior art in Fincept
Fincept ships working C++ adapters (instrument-master parsers, order mapping, WebSockets: `ZerodhaWebSocket`, `AngelOneWebSocket`,
plus a `docs/ANGELONE_QT_CROSSCHECK.md`). Use as a **checklist of what the APIs need** (daily token login, instrument dump parsing,
tick binary formats, order/product types), then re-implement against the broker's own docs. Both also need: SEBI/exchange algo-trading
rules (order-rate limits, algo registration), market calendar/holidays, and a daily re-auth flow.

## Doctrine every adapter must keep (prediction-market-live-ops)
1-lot round-trip smoke test before any strategy · truthful cancels · consistent shard/account routing on every call · `post_only` for
makers · halt-file + independent breaker · measured rate limits · push (WebSocket) discovery over polling · restart at activity
boundaries · reconcile PnL to the venue's own fills/settlements, never internal marks.
