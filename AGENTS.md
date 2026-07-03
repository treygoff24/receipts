# receipts — agent guide

You are probably an AI agent setting this up for a human, or using it yourself. This file is the complete contract. The README is for humans; everything you need is here.

## What this tool does

`receipts` answers a research question with source-verified claims. Input: a question string. Output: a JSON envelope on stdout where every claim has `sourceUrl`, `quote`, and `verdict` fields, plus a `searchTrail` and an `uncertainties` list. It spends real money (typically $0.05–0.31 per question) against Cerebras and Exa APIs. It is non-interactive by design: no prompts, no colors, no spinners. Stdout carries exactly one success envelope; stderr carries exactly one error envelope.

Use it when you need a citable answer, a fact-check with receipts, or ground truth for a claim you're about to put in a document.

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

- `data.outcome`: `"answered"` | `"partial"` | `"unanswered"`
- `data.claims[]`: `claim`, `sourceUrl`, `quote` (nullable), `verdict` (`supported` | `unsupported` | `uncertain`), `note`, `published`
- `data.uncertainties[]`: things it could not verify — surface these to your human, do not drop them
- `costDollars.total`: actual spend; `estimated: true` only on dry runs
- `budget.hit`: non-null when a cap stopped work early

Trust rule: treat only `verdict: "supported"` claims with a non-null `quote` as citable. Everything else is a lead, not a fact.

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
