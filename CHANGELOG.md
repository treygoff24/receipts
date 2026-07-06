# Changelog

## 0.2.1

Fixed the outbound User-Agent header pointing at a nonexistent repo path (treygoff → treygoff24) and replaced project-specific test fixture strings with generic placeholders. No behavior changes.

## 0.2.0

Breaking schema changes, driven by a dogfooding pass that found claims about the wrong subject coming back `supported`, quotes that were never populated, and non-URL strings in `sourceUrl`.

- New `verdict: "off_topic"` and a new per-claim `relevance` field (`direct` | `related` | `off_topic`): a relevance gate now checks each claim against the original question before the (expensive) claim-vs-source verifier ever runs. Claims judged off-topic get `verdict: "off_topic"` and skip verification entirely; claims judged merely related (rather than a direct answer) still get verified but are marked `relevance: "related"`, so a claim that's true against its source but doesn't answer the question can't pass as citable on its own.

- `quote` is now populated: the verification judge returns an exact supporting quote alongside its verdict, validated as a substring of the fetched source text (whitespace/case normalized). A quote that fails validation, or is missing/empty, is dropped (`null`) with a note explaining why, rather than trusted blind.

- `data.uncertainties` is now populated mechanically in addition to whatever the model records: any on-topic claim that couldn't be sourced names what wasn't verified, a run where nothing on-topic was ever confirmed says so explicitly, and a run with supported claims that never directly answer the question says that too.

- `data.outcome` semantics changed: `answered` now requires at least one claim with `verdict: "supported"` AND `relevance: "direct"` (previously any supported *or* partial claim was enough, which let off-topic-adjacent and merely-related runs report `answered` with nothing that actually answered the question).

- `sourceUrl` is now `string | null` (was `string`) — enforced to be a valid http(s) URL at envelope assembly. Empty strings and bare source names (e.g. `"PacerMonitor"`) become `null`, with the name preserved in `note`.

- Documented the PACER/docket blind spot: `receipts` verifies what secondary, Exa-reachable sources report, not live docket state behind logins or paywalls.

## 0.1.1

- `--dry-run` now reports the expected-case cost in `costDollars` (one search round per worker) instead of the worst case, which overstated typical spend ~6x. The worst-case ceiling remains available as `data.projectedWorstCaseCost`, joined by the new `data.projectedCost`.

## 0.1.0

Initial release.

- `ask` — source-verified research: decompose, parallel Exa search, source reading, verification pass; every claim returns with `sourceUrl`, `quote`, and `verdict`
- Depth tiers (`quick` / `standard` / `deep`), hard budget caps (`--max-dollars`, `--max-seconds`) with graceful partial results (exit 10)
- `--dry-run` cost estimation with no keys and no spend
- `doctor` (offline and `--online`) credential and config diagnosis
- `capabilities` and `schema` machine self-description
- Stable JSON envelopes: `receipts.cli.response.v1` (stdout) and `receipts.cli.error.v1` (stderr)
