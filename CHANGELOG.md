# Changelog

## 0.1.0

Initial release.

- `ask` — source-verified research: decompose, parallel Exa search, source reading, verification pass; every claim returns with `sourceUrl`, `quote`, and `verdict`
- Depth tiers (`quick` / `standard` / `deep`), hard budget caps (`--max-dollars`, `--max-seconds`) with graceful partial results (exit 10)
- `--dry-run` cost estimation with no keys and no spend
- `doctor` (offline and `--online`) credential and config diagnosis
- `capabilities` and `schema` machine self-description
- Stable JSON envelopes: `receipts.cli.response.v1` (stdout) and `receipts.cli.error.v1` (stderr)
