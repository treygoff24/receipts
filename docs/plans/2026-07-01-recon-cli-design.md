# Design: `recon` — research at function-call latency

> **Historical design doc.** Written under the project's original name, `recon`; the repo was later renamed to `receipts` (see `CHANGELOG.md`). Kept for design history — not a description of the current CLI. For current behavior, see `README.md` and `AGENTS.md`.

*2026-07-01, from the Session 1 brainstorm (Trey + Fable). Status: draft — two decisions confirmed by Trey (blocking-first exec model; the premise reframe), the rest are Fable's recommendations pending his read. Prototype evidence: `swarm.py` in the volley repo.*

## What it is

An open-source, agent-first CLI (single static binary, Rust) that any coding/ops agent calls to get **source-verified research in seconds**. Backed by a Cerebras-served fast model (default `gemma-4-31b`, ~1,000 tok/s, 50-wide parallelism) for the swarm work and Exa for search. The user supplies both API keys.

**The category is NOT "deep research."** Deep-research harnesses are minutes-scale, prose-out, human-consumed. `recon` is **function-call-scale**: 8–30s, ~$0.05–0.30/call, structured-out, agent-consumed. The calling agent uses it the way it uses grep — freely, repeatedly, mid-task. Measured basis (volley Session 1): 4-worker verified swarm answered real legal-status questions in 8.6–14.4s at ~$0.15 including search costs.

## The core architectural bet

**Don't let the fast model write the final answer.** Session 1 data: gemma-31b is an excellent fact-finder and per-claim verifier, and a mediocre analyst. The calling agent (Claude, Codex, whatever) is the smart one. So the primary output is **verified claims**, not prose:

Envelope skeleton matches exa-agent exactly (same consumer population calls both side by side): `schema` version string, `ok`, `command`, recon-specific payload nested under `data`, camelCase throughout. **Success → stdout; on failure stdout stays empty and a `recon.cli.error.v1` envelope goes to stderr** (exa-agent's proven split).

```json
{
  "schema": "recon.cli.response.v1",
  "ok": true,
  "command": "ask",
  "requestId": "…",
  "data": {
    "question": "...",
    "outcome": "answered | partial | unanswered",
    "claims": [
      {"claim": "...", "sourceUrl": "...", "quote": "...",
       "verdict": "supported | partial | unsupported | no_source",
       "note": "...", "published": "2026-03-19"}
    ],
    "searchTrail": [{"query": "...", "results": 4}],
    "uncertainties": ["..."]
  },
  "costDollars": {"model": 0.09, "search": 0.04, "total": 0.13, "estimated": false},
  "budget": {"hit": null},
  "diagnostics": {"durationMs": 12100, "retries": 0}
}
```

Verdicts are four-state (the prototype's own bug report demanded it): `supported` = assert; `partial` = usable with hedging; `unsupported` = source read and does not back the claim (discard); `no_source` = never got checkable text (re-search, don't discard as false). All four are always reported — nothing is silently dropped. An all-unsupported or no-results run is still exit 0 / `ok: true` with `outcome: "unanswered"` and `uncertainties` explaining why — an empty answer is a successful research result, not an error.

Error envelope (stderr) carries `error.code` (stable string), `error.category`, `error.retryable`, `error.provider` (`cerebras` | `exa` | none — the fix differs per provider), and `error.partial` with whatever pipeline output existed at failure time.

The consumer synthesizes. `--brief` optionally appends a gemma-written prose summary (for humans/logs); it is explicitly the convenience path, not the product.

## Execution model (confirmed by Trey)

Blocking call, named depth tiers, hard budget caps:

- `--depth quick` (~10s target): no decomposition; 2 workers attack the SAME question from complementary angles (one literal search, one reformulated) — their claims merge at the extraction stage, deduped by (normalized claim, url).
- `--depth standard` (~30s, default): decompose to 4 sub-questions, 4 workers, ≤5 rounds (the measured configuration from swarm.py — 8.6–14.4s actuals leave headroom), single-judge verification.
- `--depth deep` (~30s budget; **measured**: 8-worker swarm.py run on a real policy question = 8.5s, 67 claims, $0.31 all-in with single-judge verification — the earlier 2-min guess was wildly conservative): 8 workers, ≤5 rounds, adaptive verification (below), one refinement pass (unresolved sub-questions get a second worker; the refinement+escalation additions have latency headroom of ~3× before touching the budget).
- `--max-seconds N` / `--max-dollars X`: hard caps that override the tier. **Enforcement semantics:** actual costs are known only when calls return, so caps are enforced by *pre-launch projection* — before starting any new worker, round, or verification batch, project (metered spend so far + worst-case cost of the next unit); if over cap, don't launch it, drain in-flight calls, and return what exists. Bounded overshoot: at most one in-flight round per worker. Result ships with exit 10 (partial), `budget.hit: "dollars" | "seconds"`, and `data.outcome: "partial"`; claims that finished verification carry their verdicts, extracted-but-unverified claims ship as `no_source` with a note, raw worker text is not included.

No async job mode in v1. If deep runs create demand, `--detach` in v2.

**Verification policy (adaptive, not fixed-N):** default is one judge per claim. If the judge returns `partial` or flags low confidence, escalate that claim to two more judges and take the majority — Session 1's vote-margin finding (agreement predicts correctness) applied at the claim level, spending extra votes only where the first read is weak. `--verify paranoid` forces 3 judges on every claim; `--verify off` skips verification (returns raw extracted claims, all marked `no_source`). Escalation-threshold calibration is an open empirical task, flagged in v1 docs as such.

## Pipeline (per call)

1. **Decompose** (skip at quick): 1 fast-model call → sub-questions.
2. **Workers** (parallel): tool-calling loop, `search` tool → Exa (`--num-results 4 --text`). Non-streaming, flat schema, clean history — the calibrated-reliable configuration (20/20, 10/10 in Session 1).
3. **Extract claims** (structured output): atomic claims with raw http source URLs.
4. **Verify** (parallel, the soul of the tool): each claim judged against cached source text by a fresh model instance. Three-tier verdict — *supported* (assert), *partial* (include, hedged), *unsupported* (report in a `rejected` section, never silently drop). Session 1 lesson: binary keep/drop starved answers of facts the question asked for. `--verify paranoid` = 3 independent judges, majority.
5. **Brief** (optional, `--brief`): prose synthesis from supported+partial claims only.

## Agent-first contract (copied shamelessly from exa-agent)

- Success envelope on stdout; on failure stdout is empty and the error envelope goes to stderr (exa-agent's exact split). TTY → human-readable, pipe → JSON; `--json` forces.
- Stable exit codes: 0 ok · 1 usage (bad flags/values) · 2 auth · 4 network · 5 upstream · 6 rate-limit · 10 partial (budget hit / some workers failed) · 11 no input.
- Self-describing offline: `recon capabilities`, `recon schema`, `recon --help` with did-you-mean. An agent should never guess flags.
- `recon doctor`: offline key-presence check; `doctor --online` probes BOTH providers with unbilled/cheapest calls before any paid fan-out. Bad `EXA_API_KEY` must fail at exit 2 before Cerebras tokens burn.
- `--dry-run` prints the planned fan-out and a **projected** cost without spending — envelope marks it `costDollars.estimated: true`. (The "never estimated" rule below applies to *actual-run* spend reporting only; dry-run projections are explicitly labeled guesses.)

## Engineering facts to bake in (all hit in Session 1)

- Cerebras rate windows differ by shape: req/min for many-small, tok/min for few-large → per-tier concurrency defaults (50 small / 25 token-heavy), long backoff on 429 (a minute-window can't be out-slept in 2s), per-job fault isolation, resumable-in-run accounting. **The 50/25 numbers are one account's measured limits, not universal** — config/env overridable (`RECON_MAX_CONCURRENCY`), and read the rate-limit headers Cerebras returns to adapt live rather than assuming.
- Structured output mangles non-ASCII occasionally (Próspera → control chars): sanitize/validate all extracted strings; reject control characters at the parse boundary.
- Model emits bad JSON ~1%: one re-roll, then escape-repair (`\u` fix), then fail that claim only.
- Real User-Agent on every HTTP call (Cloudflare 403s default UAs).
- Search provider behind a trait (`SearchProvider: search(query) -> Vec<SourceDoc>`); Exa is the only v1 impl. Model endpoint/id configurable (`RECON_MODEL`, `--model`); Cerebras-OpenAI-compat is the only v1 target.
- Costs metered from response `usage` + Exa `costDollars` envelopes — never estimated.

## Config

Env-first: `CEREBRAS_API_KEY`, `EXA_API_KEY` (reuse existing exa-agent credentials file as fallback read, maybe). `~/.config/recon/config.toml` for defaults (model, tier, caps). No telemetry.

## Name

**`recon`** — confirmed by Trey 2026-07-01 ("recon and sift both slap, recon is clearly superior").

## Not in v1 (deliberately)

Async jobs · MCP server mode (`recon serve --mcp` is the natural v2 — same core, second transport) · non-Exa search providers · recursive self-deepening workers · caching/dedup across calls · local corpus/RAG. Each has a seam left for it; none earn v1 complexity.

## Review status

Adversarially reviewed by plan-reviewer 2026-07-01 (13 findings, verdict needs-revision) — all findings folded into this revision: error-envelope contract specified (exa-agent split), budget-cap enforcement semantics defined (pre-launch projection, bounded overshoot), fourth verdict `no_source` restored, dry-run/metering tension resolved, concurrency defaults made configurable, `doctor` added, adaptive claim-level vote-escalation replaces fixed 3-vote, deep tier flagged unvalidated, quick-tier worker semantics defined, envelope aligned to exa-agent skeleton, unanswerable-question case named, usage exit code added, provider attribution in errors.

## Decisions closed 2026-07-01

1. Name: **recon** (Trey). 2. New repo `treygoff/recon`, **Apache-2.0** (Trey). 3. Deep tier validated by measurement (8.5s / $0.31 at 8 workers). 4. Output contract: claims-JSON-primary (Trey's blanket approval of the design summary). No open questions remain; next artifact is the implementation plan in the recon repo.
