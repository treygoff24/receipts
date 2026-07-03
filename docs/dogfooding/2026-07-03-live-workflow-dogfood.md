# Dogfood report: five live fact-checks from a real research workflow

**Date:** 2026-07-03
**Version:** receipts 0.1.1 (Homebrew install)
**Operator:** Claude (agent-driven, per the agent-first contract) inside a live policy-research session
**Total spend:** ~$0.47 across five `ask` calls
**Doctor at time of run:** healthy; `model=gemma-4-31b, exaSearchType=fast, maxConcurrency=25, depth=standard, verify=adaptive`

## Why this report exists

First real-world use of receipts inside an actual working session rather than a demo. Five claims that were about to drive document edits got run through `ask` as a second-source pass. Three verified cleanly, one usefully downgraded a claim, and one failed in an instructive way. The run surfaced two bugs and two design gaps worth fixing before heavier use. Everything below is written to be actionable by a coding agent in this repo; the underlying research topic is intentionally not described beyond question *shape*, but every failure mode reproduces with the neutral commands in §6.

## 1. The five runs

All five ran concurrently as separate processes. All exited 0. Question texts are summarized by shape, not verbatim.

| # | Question shape | Depth | Cost | Outcome field | Actual result |
|---|---|---|---|---|---|
| 1 | Current status + deadline of a named federal Clean Water Act enforcement case (PACER-gated docket) | standard | $0.065 | `answered` | **Failed silently.** No on-topic supported claim; off-topic "supported" claims from unrelated cases; empty `uncertainties`. See F2, F3, F4. |
| 2 | Whether a named House member co-signed a specific 2023 letter to two Cabinet-level officials | standard | $0.121 | `answered` | **Partial, honest.** Confirmed the letter exists (primary PDF + secondary reports); correctly did NOT confirm the member's signature. Drifted into unrelated SEC comment-letter claims late in the envelope (F2). |
| 3 | Whether a named House member made a specific foreign visit, when, and per what source | quick | $0.099 | `answered` | **Clean.** Multi-source confirmation plus adjacent verified detail the original research had missed. |
| 4 | Whether two named House members co-led a specific bill and whether it covers a named commodity | quick | $0.091 | `answered` | **Clean.** Bill number, date, co-leads, commodity coverage all confirmed against the sponsor's own release. |
| 5 | Whether a federal export-credit agency approved a deal of a specific dollar size/scope and won a named award | quick | $0.092 | `answered` | **Clean, best run.** Every figure confirmed against agency primary sources, one overbroad claim correctly marked `unsupported`, and a nuance surfaced (the award was shared with two co-recipients) that improved the downstream document. |

Net scorecard as a research tool: 3 clean confirms, 1 correct downgrade, 1 silent failure. As a product: the silent failure is the story.

## 2. What worked well

Speed and cost were exactly right for the "verify before you act on it" niche: five questions, under a minute of operator attention, under fifty cents.

Verdict discrimination is real, not cosmetic. Run 5 marked "supplying 186 modular steel bridges **and related equipment**" as `unsupported` because the cited source said "bridges, roads, and associated infrastructure" — that is precisely the granularity a citation tool should enforce, and it caught a paraphrase drift a human skims past.

Run 2's core behavior was the ideal honest-partial: it verified the *document* exists via a primary PDF and declined to verify the *signatory* claim the question actually asked about, because the public copies omit the signature page. That distinction changed how the downstream document phrased the claim ("reported signatory" instead of flat assertion). This is the product working as intended.

The agent-first envelope contract held: one JSON object on stdout per run, parseable, no stray output, stable schema across five concurrent invocations.

## 3. Findings

Ranked by how much they undermine the product's core promise ("claims you can cite").

### F1 — `quote` is null on every claim in every run (bug, breaks the trust rule)

Across all five envelopes, roughly 50 claims total, **not one claim carried a non-null `quote`** — including dozens of `verdict: "supported"` claims with valid `sourceUrl`s. The skill/AGENTS contract says only `supported` + non-null `quote` is citable. Under a strict reading of the tool's own trust rule, these five runs produced **zero citable claims** while doing objectively good verification work.

The verifier clearly *has* the evidence: the `note` field contains quote-like paraphrase, e.g.:

```json
{
  "claim": "In United States v. City of Minneapolis (Civil No. 0:25-cv-00048), the United States filed a Motion to Dismiss with prejudice on May 21, 2025.",
  "note": "The source text confirms that in case Civil No. 0:25-cv-00048, the United States filed a 'MOTION TO DISMISS' with prejudice, and the document is dated and filed on May 21, 2025.",
  "published": null,
  "quote": null,
  "sourceUrl": "https://storage.courtlistener.com/recap/gov.uscourts.mnd.222010/gov.uscourts.mnd.222010.59.0.pdf",
  "verdict": "supported"
}
```

Hypotheses to check, in order: (a) the extraction step produces quotes but the field mapping into the envelope drops them; (b) the verifier model is instructed to paraphrase into `note` and quote extraction is a separate step that never runs at `verify=adaptive`; (c) quotes are extracted but fail some validation (length, exact-substring match against fetched text) and are silently nulled. If (c), consider degrading gracefully: keep the failed candidate quote in a `quoteCandidate` field or relax matching (whitespace/case normalization) rather than nulling.

Acceptance test: any of the neutral repro commands in §6 returns at least one supported claim with a non-null `quote` that is an exact substring of the fetched source text.

### F2 — Relevance drift: off-topic claims come back `verdict: "supported"` (design gap, the dangerous one)

Run 1 asked about the status of one specific federal enforcement case. The envelope's only `supported` claims were about **entirely different lawsuits** — a Minneapolis disability-rights case (D. Minn. docket 0:25-cv-00048) and a Louisville one — presumably surfaced because they matched "United States v. ... motion to dismiss ... 2025" search patterns. Each of those claims is *individually true against its source*, so the claim-vs-source verifier passes them, and they land in the envelope indistinguishable from on-topic findings.

Run 2 showed the same pattern in milder form: late-envelope claims about SEC comment letters on Morrison v. National Australia Bank — thematically adjacent to "letters from officials about investor protections," useless for the actual question.

The failure mode for the consuming agent: an envelope full of `supported` claims *looks like an answered question*. A less careful consumer would have written "the case was dismissed May 21, 2025" into a live document, citing a real source, about the wrong lawsuit. This is worse than returning nothing, and it is the one finding here that can cause a user real-world harm.

Fix direction: a relevance gate between claim generation and the envelope. Score each claim against the original question (the same model can do this cheaply: "does this claim answer the question asked, yes/no/partially"); either drop off-topic claims or mark them with a distinct verdict (`off_topic` or `related`) so the trust rule can exclude them. The claim-vs-source check and the claim-vs-question check are different competencies; today only the first exists.

Acceptance test: the §6 PACER repro returns no `supported` claim about a case other than the one named in the question, or returns such claims only under a non-`supported` verdict.

### F3 — `uncertainties` stays empty when retrieval fails (design gap, compounds F2)

Run 1 could not reach the actual docket (PACER/PacerMonitor content is login- or paywall-gated; the on-topic claims all came back `no_source`, one with an empty `sourceUrl` and note "no source text available"). This is exactly the situation `data.uncertainties` exists for, and it came back `[]`. The combination is what makes F2 dangerous: the envelope simultaneously (a) failed to answer the question, (b) contained plausible-looking supported claims about the wrong cases, and (c) declared no uncertainty. It failed *confident* instead of failing *loud*.

Fix direction: populate `uncertainties` mechanically, not just from model judgment. Candidate triggers: any on-topic claim ending `no_source`; any fetch of a known-gated domain (pacermonitor.com, PACER, court-records aggregators, hard paywalls) failing or returning a login/robots wall; zero supported claims that pass the F2 relevance gate. Even a canned string ("the primary docket for the named case could not be retrieved; status claims are unverified") transforms this run from a trap into a correct 'I don't know.'

Related: consider whether `outcome` should be `partial` or `unanswered` when no on-topic supported claim exists. Run 1 reported `outcome: "answered"`.

### F4 — Court-docket questions are a known blind spot (limitation, document it)

Even with F2/F3 fixed, questions whose ground truth lives behind PACER will not be answerable with Exa-reachable sources unless a CourtListener/RECAP fetch path exists. Two cheap options: (a) document the limitation in the skill and AGENTS.md ("docket-status questions: receipts can verify what secondary sources report, not live docket state"); (b) special-case CourtListener's RECAP API (free, JSON, no auth for public documents) as a retrieval backend when a question names a federal case. Option (b) would make receipts *better* at this class than general web search, which fits the product's niche.

### F5 — `no_source` claims with empty string `sourceUrl` or bare source names (polish)

Run 1 contained claims with `sourceUrl: ""` and others with `sourceUrl: "PacerMonitor"` / `"CourtListener"` / `"Complaint"` — bare names, not URLs, in a field the schema presents as a URL. Consumers that hyperlink or fetch `sourceUrl` will choke. Either enforce URL-or-null at envelope assembly, or move bare source names into `note`.

### F6 — No prose answer field (confirm it's intentional)

`data` carries `claims / outcome / question / searchTrail / uncertainties` and no synthesized answer string. For agent consumers this is arguably correct (claims-first forces engagement with verdicts), but the skill's own §3 says "use the answer," which implies one exists. Either add a short `answer` synthesized only from supported on-topic claims, or align the docs with the claims-only reality.

## 4. Aggregate observations

Concurrency: five simultaneous invocations from one machine completed without interference or rate-limit errors from the CLI itself (the underlying research this session replaced had separately observed FEC API rate limiting; receipts' own Exa/Cerebras path showed no such symptom at n=5).

Cost tracking worked and matched expectations ($0.065–$0.121 per question, standard ≈ 1.5–2x quick).

Depth selection felt right: `quick` was fully adequate for single-fact public-record questions; `standard` on the two harder questions bought more claims but also more drift surface (both F2 instances were standard-depth runs). Worth investigating whether drift correlates with depth (more search rounds → more adjacent-topic pages in the claim pool).

The `gemma-4-31b` default did the verification competently but may be implicated in F2's relevance blindness; if a relevance gate is added, test whether the same model suffices for the gate or whether it needs the verify-tier model.

## 5. Priority order for actioning

1. **F2 + F3 together** (relevance gate + mechanical uncertainties). This is the harm-prevention fix; ship these before promoting the tool for high-stakes use.
2. **F1** (quote pipeline). This is the credibility fix; without it the trust rule makes the tool's best work formally unusable.
3. **F5** (sourceUrl hygiene) — small, do it alongside F1's envelope work.
4. **F4** (docket limitation: document now, RECAP backend later as a feature).
5. **F6** (answer field or doc alignment) — decide and close.

## 6. Neutral reproduction commands

These reproduce the failure modes without any dependence on the original session's subject matter.

F1 (quote null) — any factual question; e.g.:

```sh
receipts --json --depth quick ask "When was the Export-Import Bank of the United States most recently reauthorized, and through what year does its charter run?" \
  | jq '[.data.claims[] | select(.verdict=="supported")] | map(.quote) | {supported: length, nonNullQuotes: (map(select(. != null)) | length)}'
```

Observed on 2026-07-03: `nonNullQuotes: 0` on every run regardless of question.

F2 + F3 (drift + silent uncertainty) — a PACER-gated docket-status question about any specific federal consent-decree case; e.g.:

```sh
receipts --json ask "In the federal enforcement case United States v. Norfolk Southern Railway concerning the East Palestine derailment consent decree, what is the current implementation status as of mid-2026, and what deadlines are pending on the docket?" \
  | jq '{onTopicSupported: [.data.claims[] | select(.verdict=="supported") | .claim], uncertainties: .data.uncertainties, outcome: .data.outcome}'
```

Failure signature: `supported` claims naming *other* cases, `uncertainties: []`, `outcome: "answered"`.

F5 (sourceUrl hygiene):

```sh
receipts --json ask "<any docket-status question as above>" \
  | jq '[.data.claims[] | select(.sourceUrl != null and (.sourceUrl | startswith("http") | not))] | length'
```

## 7. What this session would have wanted (wishlist, not findings)

A `--claims-only` flag is unnecessary (that's the default shape), but one thing would improve agent ergonomics: a per-claim `relevance` or `answersQuestion` boolean, which F2's fix makes free. Diagnostics were otherwise complete (`durationMs` and `retries` present on every ask envelope; run 5 clocked 3.1s wall time, which is excellent for multi-source verification).

Overall: the niche is real, the good runs were genuinely better than the general-purpose research pass they were checking, and the failure mode worth losing sleep over is precisely one gate away from fixed.
