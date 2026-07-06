# recon v1 implementation plan

> **Historical design doc.** Written under the project's original name, `recon`; the repo was later renamed to `receipts` (see `CHANGELOG.md`). Kept for design history — not a description of the current CLI. For current behavior, see `README.md` and `AGENTS.md`.

Design authority: `2026-07-01-recon-cli-design.md` (same dir). This doc maps it to modules, in build order. Prototype evidence lives in the volley repo (`swarm.py`, `LOG.md`).

## Stack decisions

- **No async runtime.** The pipeline is a bounded fan-out (≤25 concurrent HTTP calls); `std::thread` + `mpsc` channels handle it. `ureq` for HTTP (sync, rustls). Tokio earns nothing here but binary size and compile time.
- Crates: `clap` (derive), `serde`/`serde_json`, `ureq` (json + rustls), `thiserror`, `toml`, `uuid` (v4, requestId). Nothing else without a fight.
- Exa via **direct REST** (`api.exa.ai`), not shelling to exa-agent — recon must work standalone. `EXA_API_KEY` env.
- Cerebras via OpenAI-compat REST. `CEREBRAS_API_KEY` env. Model default `gemma-4-31b`, `RECON_MODEL`/`--model` override.

## Modules (build order; each lands with its test)

1. **`error.rs`** — `ReconError` enum ↔ exit codes: Usage=1, Auth=2, Config=3, Network=4, Upstream=5, RateLimit=6, Partial=10, NoInput=11. Carries `provider: Option<Provider>` (Cerebras|Exa), `retryable: bool`, `partial: Option<serde_json::Value>`.
2. **`envelope.rs`** — success (`recon.cli.response.v1`, stdout) + error (`recon.cli.error.v1`, stderr; stdout empty). camelCase serde. TTY → human render, pipe → JSON, `--json` forces. Golden tests: serialize both envelopes, assert exact field names.
3. **`config.rs`** — env > `~/.config/recon/config.toml` > defaults. Keys, model, concurrency caps, tier defaults.
4. **`providers/cerebras.rs`** — `chat(messages, opts) -> ChatResponse`. Non-streaming always. Retry ladder from measured behavior: 429 token-window needs 20s+ backoff, ≤6 tries; 5xx exponential. Meters `usage` into a shared `Spend` (Mutex). **JSON repair at this boundary** (strip control chars, fix bare `\u` — the volley `jloads` lesson) + one re-roll on parse failure. Real User-Agent (`recon/<version> (github.com/treygoff/recon)`).
5. **`providers/exa.rs`** — `SearchProvider` trait: `search(query) -> Vec<SourceDoc>`; `contents(url) -> Option<String>`. Exa impl, costs metered from response `costDollars`.
6. **`budget.rs`** — `Budget::may_launch(projected_unit_cost) -> bool` (pre-launch projection per design), records hit reason. Worst-case unit costs derived from tier constants.
7. **`pipeline/`** — `decompose.rs`, `worker.rs` (tool-loop, ≤5 rounds, source cache), `extract.rs` (≤15 claims, re-roll once, fail-item-only), `verify.rs` (4-verdict; adaptive escalation: judge-1 partial/low-conf → +2 judges majority), `brief.rs` (only with `--brief`). Every stage: per-item fault isolation.
8. **`tiers.rs`** — quick (2 same-question workers, complementary angles, dedup by (normalized claim, url)) / standard (4 subq) / deep (8 subq + refinement pass). Constants in one place.
9. **`main.rs`** — clap: `ask` (default subcommand), `doctor [--online]`, `capabilities`, `schema`, global `--json --model --depth --max-seconds --max-dollars --verify --brief --dry-run`.
10. **`doctor`** — offline: key presence, config parse. `--online`: 1-token Cerebras call + cheapest Exa call; fail exit 2 with provider named, before any paid fan-out ever runs (ask runs the offline check implicitly).

## Testing gate

`cargo test` = golden envelope tests + unit tests per module + one integration test against a mock HTTP server (spawn `std::net` listener returning canned Cerebras/Exa responses; run the full quick tier through it; assert envelope, exit code, spend arithmetic). `cargo clippy -- -D warnings`. No network in tests.

## Acceptance (v1 ships when)

- `recon ask "question" --json` returns the design's envelope in <30s standard tier against live keys.
- `recon doctor --online` catches a bad key of either provider at exit 2.
- Budget cap kill-test: `--max-dollars 0.01` on deep tier returns exit 10 with partial claims and `budget.hit`.
- Unanswerable-question test returns exit 0, `outcome: "unanswered"`.
- README documents the agent contract with envelope examples; `--help` is agent-parseable.
