# receipts

`receipts` is an agent-first Rust CLI for source-verified research. Stdout is reserved for result envelopes. Stderr is reserved for error envelopes. There are no prompts, colors, spinners, or interactive fallbacks.

## Install

```sh
cargo install --path .
```

## Environment

| Variable | Required for | Default |
| --- | --- | --- |
| `CEREBRAS_API_KEY` | `ask`, `doctor --online` | none |
| `EXA_API_KEY` | `ask`, `doctor --online` | none |
| `RECEIPTS_MODEL` | model default | `gemma-4-31b` |
| `RECEIPTS_API_BASE` | Cerebras compatible API base | `https://api.cerebras.ai/v1` |
| `RECEIPTS_EXA_BASE` | Exa compatible API base | `https://api.exa.ai` |
| `RECEIPTS_EXA_SEARCH_TYPE` | Exa search type: `fast`, `instant`, or `auto` | `fast` |
| `RECEIPTS_MAX_CONCURRENCY` | worker concurrency | `25` |

## Ask

Canonical call:

```sh
receipts --json --depth quick ask "What source supports this claim?"
```

`receipts "What source supports this claim?"` is the default-subcommand form for `ask`.

Success envelope shape:

```json
{
  "schema": "receipts.cli.response.v1",
  "ok": true,
  "command": "ask",
  "requestId": "00000000-0000-0000-0000-000000000000",
  "data": {
    "question": "what is prospera",
    "outcome": "answered",
    "claims": [
      {
        "claim": "Próspera is a ZEDE in Honduras.",
        "sourceUrl": "https://example.com/source",
        "quote": null,
        "verdict": "supported",
        "note": "source text supports the claim",
        "published": "2026-07-01"
      }
    ],
    "searchTrail": [{ "query": "prospera law", "results": 4 }],
    "uncertainties": []
  },
  "costDollars": { "model": 0.09, "search": 0.04, "total": 0.13, "estimated": false },
  "budget": { "hit": null },
  "diagnostics": { "durationMs": 12100, "retries": 0 }
}
```

`--dry-run` returns the same success envelope family with `data.dryRun: true` and `costDollars.estimated: true`. It does not require API keys and makes no provider requests.

## Exit codes

| Code | Meaning | Channel and shape |
| ---: | --- | --- |
| 0 | ok | success envelope on stdout |
| 1 | usage | error envelope on stderr, stdout empty |
| 2 | auth | error envelope on stderr, stdout empty, except `doctor` reports structured checks on stdout |
| 3 | config | error envelope on stderr, stdout empty |
| 4 | network | error envelope on stderr, stdout empty |
| 5 | upstream | error envelope on stderr, stdout empty |
| 6 | rate limit | error envelope on stderr, stdout empty |
| 10 | partial | success envelope on stdout with `ok: true`, `data.outcome: "partial"`, and `budget.hit` set; budget/partial-driven regardless of claim count; a zero-claim partial means the budget closed before work completed |
| 11 | no input | error envelope on stderr, stdout empty |

Exit 10 is deliberate: budget-hit partials are usable research results, so stdout carries the success envelope even though the process status is nonzero. Exit 10 is budget/partial-driven regardless of claim count; a zero-claim partial means the budget closed before work completed.

## Tiers

| Tier | Workers | Expected latency | Expected cost | Notes |
| --- | ---: | --- | --- | --- |
| quick | 2 | ~10s target | ~$0.05 to $0.10 typical | same question, complementary search angles |
| standard | 4 | ~9 to 15s measured | ~$0.15 measured | default |
| deep | 8 | ~9s measured | ~$0.31 measured | refinement pass and adaptive verification |

Budget caps use pre-launch projection. `--max-dollars X` and `--max-seconds N` stop new work, drain in-flight calls, and return the partial envelope when useful claims exist.

## Doctor

```sh
receipts doctor --json
receipts doctor --online --json
```

Offline doctor checks config parsing, key presence, resolved model, API bases, depth, verification policy, and concurrency. It never prints secret values. `--online` adds a minimal Cerebras chat probe and an Exa search probe.

Doctor output is a normal response envelope whose `data` is a structured report with `status`, `summary`, and `checks`. Missing or bad credentials exit 2 and name the provider plus the env var to set.

## Self-description

```sh
receipts capabilities --json
receipts schema all --json
receipts schema response --json
receipts schema error --json
```

`capabilities` returns version, commands, read-only/destructive/spend annotations, global flags, exit codes, env vars, tier expectations, and budget unit costs. `schema` returns JSON Schema for the success and error envelopes.
