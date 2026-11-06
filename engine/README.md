# trading-ultra engine

One engine for every platform. Backtest, paper and live run **the same loop** (`tc_backtest::Engine`); only the
`Venue` implementation differs.

```
events ─► Venue.feed ─► fills ─► Portfolio + Ledger ─► state ─► breaker ─► Strategy ─► Actions ─► RiskGate ─► Venue.place
```

## Build / test / try
```bash
cd engine
cargo test --workspace                 # 85 tests incl. golden vectors vs the original Python sizing.py + skill bracket math
cargo build --release
./target/release/tc determinism        # run twice → identical ledger digest
./target/release/tc demo-pm            # longshot fade on SYNTHETIC biased / calibrated / adverse markets
./target/release/tc demo-bars          # ported quant-trading strategies on SYNTHETIC bars, with costs
./target/release/tc demo-regime        # regime gate + ATR exits + vol sizing vs plain, on SYNTHETIC regimes
./target/release/tc replay events.jsonl --strategy fade|edge --equity 10000 [--halt-file HALT]
```
`touch HALT` (with `--halt-file HALT`) blocks all new risk and cancels everything resting; risk-reducing orders still go through.

## Design rules (each exists because of a documented failure)
| Rule | Where enforced |
|---|---|
| Units are types; no bare `f64` money | `tc-core::units` |
| Strategies can't see the future: events are time-ordered, out-of-order = hard error | `Engine::step` |
| Orders are limit-only; maker strategies must be `post_only` (risk gate enforces by tag) | `tc-core::OrderReq`, `tc-risk` |
| Cancel is truthful; `AlreadyGone` ≠ cancelled; `Replace` places only if the cancel succeeded | `Venue`, `Action::Replace` |
| PnL only from venue-confirmed fills + venue-reported settlements; cohort-tagged at write time | `tc-ledger` |
| Risk is worst-case loss (binary short costs `1−px`), counts resting orders, per instrument/event/total | `tc-risk` |
| A halt never traps you: reducing orders pass; only an operator `resume()` clears it | `tc-risk` |
| Same data → same ledger digest | `tc-ledger::Ledger::digest` |
| Every venue adapter must pass the conformance suite | `tc-conformance` |
| Sizes shrink with drawdown / weekly loss / loss streak; 0 ⇒ stand down (`size_scale`) | `tc-risk`, `Ctx.size_scale` |
| Kelly fraction follows evidence (0.10→0.25→0.5×); pre-declared kill line at ≥50 realisations with net loss | `evidence_gate` in `tc-strategy` |
| Client ids are venue-safe (`[A-Za-z0-9-]`, ≤36) | `markets::sanitize_client_id` |

## Event interchange (research plane → engine)
One serde-JSON `Event` per line, e.g. `{"Signal":{"ts":…,"source":"kronos","instrument":"EV:M1","prob":0.3,"confidence":null}}`.
Prices/quantities are fixed-point raw ints (×10⁴); timestamps are ns. Writers: `research/tc_research/events.py`.

## Adding a platform
Copy `../venues/_template`, implement `Venue`, pass `tc_conformance::run_all`, then follow the go-live gate in `../venues/README.md`.
