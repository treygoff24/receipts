---
name: receipts
description: Get source-verified answers with the receipts CLI — every claim returns with a source URL, quote, and verdict. Use when you need a citable answer to a factual question, when a claim is about to go into a document and must be ground-truthed first, or when the user asks to fact-check something. Spends real money per question (~$0.05–0.35).
---

# receipts

`receipts` answers a research question with claims you can cite. It searches the web, reads sources, and returns a JSON envelope where every claim carries `sourceUrl`, `quote`, and `verdict`. It is non-interactive and agent-first: one success envelope on stdout, one error envelope on stderr, nothing else.

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

## 3. Use the answer

The trust rule: only claims with `verdict: "supported"` AND a non-null `quote` are citable — cite them with their `sourceUrl`. Everything else is a lead, not a fact. Never drop `data.uncertainties`; surface them to your human alongside the answer.

Done when: every claim you repeat downstream traces to a supported claim's `sourceUrl`, and uncertainties are disclosed.

## Contract source of truth

`receipts capabilities --json` and `receipts schema all --json` are generated from the code and override anything here or in the repo's AGENTS.md.
