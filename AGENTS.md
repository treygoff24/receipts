# receipts — agent guide

You are probably an AI agent setting this up for a human, or using it yourself. This file is the complete contract. The README is for humans; everything you need is here.

## What this tool does

`receipts` answers a research question with source-verified claims. Input: a question string. Output: a JSON envelope on stdout where every claim has `sourceUrl`, `quote`, `verdict`, and `relevance` fields, plus a `searchTrail` and an `uncertainties` list. It spends real money (typically $0.05–0.31 per question) against Cerebras and Exa APIs. It is non-interactive by design: no prompts, no colors, no spinners. Stdout carries exactly one success envelope; stderr carries exactly one error envelope.

Use it when you need a citable answer, a fact-check with receipts, or ground truth for a claim you're about to put in a document.

There is no synthesized prose answer field by default — `data.claims` is the API, and consumers are expected to read verdicts, not a summary sentence. (`--brief` adds an optional `data.brief` narrative built only from supported/partial claims; treat it as a convenience read, not a citable source.)

Limitation: `receipts` verifies what secondary, Exa-reachable web sources report — news coverage, agency press releases, public dockets that have been indexed. It cannot see behind PACER or other login/paywall-gated systems, so questions about the live status of a specific federal court docket will typically come back `no_source` or `partial`, not because the case is unclear but because the ground truth isn't reachable. Treat `outcome: "partial"` or `"unanswered"` on a docket-status question as "not indexed," not "nothing is happening." (A future CourtListener/RECAP backend could close this gap for federal cases; not built yet.)

A packaged version of this contract ships as an agent skill: `npx skills add treygoff24/receipts` installs it into your skills directory (Claude Code, Codex, Cursor, and friends).

## Install

Pick the first available path:

```sh
brew install treygoff24/tap/receipts
# or
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/treygoff24/receipts/releases/latest/download/receipts-installer.sh | sh
# or
cargo binstall receipts || cargo install receipts
```

Verify: `receipts --version` prints `receipts <semver>`.

## Setup for your human

Two secrets are required. Do not guess them; ask your human to provide them or set them in the environment/secret manager you have access to:

1. `CEREBRAS_API_KEY` — from https://cloud.cerebras.ai (free tier exists)
2. `EXA_API_KEY` — from https://exa.ai

Then self-verify without spending:

```sh
receipts doctor --json          # offline: config parse, key presence, resolved model
```

And with a minimal paid probe (fractions of a cent) once keys are set:

```sh
receipts doctor --online --json
```

`doctor` failures exit 2 and name the provider and env var to fix. It never prints secret values.

## Canonical invocations

```sh
receipts --json --depth quick ask "<question>"     # cheap fact-check, ~$0.05–0.10, ~10s
receipts --json ask "<question>"                   # standard, ~$0.15
receipts --json --depth deep ask "<question>"      # contested/multi-part, ~$0.31
receipts --json --max-dollars 0.10 ask "<q>"       # hard spend cap
receipts --json --dry-run ask "<q>"                # cost estimate, no keys, no spend
```

`receipts "<question>"` (no subcommand) defaults to `ask`. Always pass `--json` when you are the consumer.

## Reading the output

Success envelope (`receipts.cli.response.v1`, stdout):

- `data.outcome`: `"answered"` (at least one claim with `verdict: "supported"` AND `relevance: "direct"`) | `"partial"` (a budget cap hit, or on-topic claims exist — including `supported`-but-`related` — but none reached `supported`+`direct`) | `"unanswered"` (nothing on-topic survived)
- `data.claims[]`: `claim`, `sourceUrl` (http(s) URL or `null`), `quote` (nullable), `verdict` (`supported` | `partial` | `unsupported` | `no_source` | `off_topic`), `relevance` (`direct` | `related` | `off_topic`), `note`, `published`
- `data.uncertainties[]`: things it could not verify, populated both by the model and mechanically (any on-topic claim it couldn't source, a run where nothing was ever confirmed, or supported claims that exist but none are `relevance: "direct"`) — surface these to your human, do not drop them
- `costDollars.total`: actual spend; `estimated: true` only on dry runs. A dry-run's `costDollars` is the *expected* case (one search round per worker); budget against `data.projectedWorstCaseCost` if you need the hard ceiling
- `budget.hit`: non-null when a cap stopped work early

Two different questions, two different fields. `verdict` is claim-vs-source ("does the cited text say this?"); `relevance` is claim-vs-question ("does this claim answer or bear on what was asked?"), decided by a gate that runs before the (expensive) claim-vs-source check. Trust rule: citable requires ALL THREE — `verdict: "supported"` AND `relevance: "direct"` AND a non-null `quote` — cite via `sourceUrl`. A `supported`+`related` claim is true against its source but doesn't answer the question asked; treat it the same as a lead, not a fact. `verdict: "off_topic"` (always paired with `relevance: "off_topic"`) means the claim didn't bear on the question at all; it's kept in the envelope for visibility, never as a lead. `partial` and `unsupported` verdicts, and `no_source`, are leads, not facts. When `sourceUrl` is `null`, the original source had no usable URL — check `note` for what it was (e.g. a bare source name like "PacerMonitor").

Error envelope (`receipts.cli.error.v1`, stderr): `error.code`, `error.message`, `error.suggestedFix`. Stdout is empty on errors, with one exception below.

## Exit codes

| Code | Meaning | Your move |
| ---: | --- | --- |
| 0 | ok | parse stdout |
| 1 | usage | fix your flags; read `suggestedFix` |
| 2 | auth | keys missing/invalid; run `doctor`, tell your human which key |
| 3 | config | bad env/config value; read `suggestedFix` |
| 4 | network | retry with backoff |
| 5 | upstream | provider error; retry once, then report |
| 6 | rate limit | back off and retry |
| 10 | partial | SUCCESS envelope on stdout despite nonzero exit — budget hit; use the claims, note `budget.hit` |
| 11 | no input | you passed an empty question |

Exit 10 is the one non-obvious case: nonzero exit but stdout has usable verified claims. Handle it as a soft success.

## Machine self-description

```sh
receipts capabilities --json    # full contract: commands, spend/read-only annotations, env vars, tier costs
receipts schema all --json      # JSON Schema for both envelopes
```

If anything in this file disagrees with `capabilities` output, trust `capabilities` — it's generated from the code.
