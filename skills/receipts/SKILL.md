---
name: receipts
description: Get source-verified answers with the receipts CLI — every claim returns with a source URL, quote, and verdict. Use when you need a citable answer to a factual question, when a claim is about to go into a document and must be ground-truthed first, or when the user asks to fact-check something. Spends real money per question (~$0.05–0.35).
---

# receipts

`receipts` answers a research question with claims you can cite. It searches the web, reads sources, and returns a JSON envelope where every claim carries `sourceUrl`, `quote`, `verdict`, and `relevance`. It is non-interactive and agent-first: one success envelope on stdout, one error envelope on stderr, nothing else.

## 1. Preflight

Run `receipts doctor --json`. Healthy → proceed. Not installed → `brew install treygoff24/tap/receipts` or `cargo binstall receipts` or the installer at https://github.com/treygoff24/receipts. Exit 2 → a key is missing; ask your human for `CEREBRAS_API_KEY` (cloud.cerebras.ai) and/or `EXA_API_KEY` (exa.ai) — never guess or fabricate keys.

Done when: doctor reports `healthy`.

## 2. Ask

Pick depth by stakes, not habit:

```sh
receipts --json --depth quick ask "<question>"    # cheap fact-check, ~$0.05–0.10
receipts --json ask "<question>"                  # standard research, ~$0.15–0.25
receipts --json --depth deep ask "<question>"     # contested or multi-part, ~$0.30–0.50
```

Cap spend with `--max-dollars X` when budget matters; estimate first with `--dry-run` (free, no keys). One focused question per call — decompose compound questions yourself and make one call each.

Done when: you hold a parsed envelope (exit 0) or a partial one (exit 10 — budget hit, stdout still carries usable claims; treat as soft success). Any other nonzero exit: read `error.suggestedFix` on stderr; retry with backoff only on exits 4/5/6.

## 3. Use the claims

There is no synthesized prose answer field — `data.claims` is the API, and every consumer reads verdicts. (`--brief` adds an optional `data.brief` narrative built only from supported/partial claims; treat it as a convenience summary, never as a citable source on its own.)

Every claim has two independent judgments: `verdict` (claim-vs-source — does the cited text say this?) and `relevance` (claim-vs-question — does this claim answer or bear on what you asked?), decided by a gate that runs before the source check. The trust rule: citable requires all three — `verdict: "supported"` AND `relevance: "direct"` AND a non-null `quote` — cite with `sourceUrl`. A `supported`+`related` claim is true against its source but doesn't answer your question; treat it as a lead, not an answer. `verdict: "off_topic"` (paired with `relevance: "off_topic"`) means the claim didn't bear on the question at all — visibility only, never a lead. `partial`, `unsupported`, and `no_source` are leads, not facts. If `sourceUrl` is `null`, the source had no usable URL — check `note` for what it was. Never drop `data.uncertainties`; surface them to your human alongside the answer.

Known blind spot: `receipts` verifies what secondary, Exa-reachable web sources report. It cannot see behind PACER or other login/paywall-gated systems, so a question about the live status of a federal court docket typically comes back `partial` or `unanswered`, not because nothing is happening but because the primary record isn't reachable — don't read that as "the case is inactive."

Done when: every claim you repeat downstream traces to a `supported`+`direct` claim's `sourceUrl` with a non-null `quote`, and uncertainties are disclosed.

## Contract source of truth

`receipts capabilities --json` and `receipts schema all --json` are generated from the code and override anything here or in the repo's AGENTS.md.
