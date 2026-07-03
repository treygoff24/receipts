---
name: release
description: Cut and ship a receipts release end to end — version bump, changelog, contract-surface sync, tag, cargo-dist CI, crates.io publish, and channel verification. Use when the user says cut a release, ship a release, publish a new version, or tag vX.Y.Z.
---

# Shipping a receipts release

A release is one commit on main that bumps the version, plus a `vX.Y.Z` tag. The tag triggers the cargo-dist Release workflow (GitHub release + shell installer + Homebrew formula pushed to `treygoff24/homebrew-tap`); crates.io is published manually from the local machine. A release is not done until every channel shows the new version — step 5 is part of the release, not an afterthought.

## 1. Decide the version

Pre-1.0 semver: anything that breaks the JSON envelope, the emitted schema, exit codes, or CLI flags bumps MINOR; additive or fix-only changes bump PATCH. Breaking changes are allowed but must lead the changelog entry.

Done when: the new version is written in `Cargo.toml` and `cargo check` has refreshed `Cargo.lock`.

## 2. Sync the contract surfaces

The envelope contract lives in FIVE places that drift independently. Reconcile every one against actual behavior, not against each other:

- `src/commands/schema.rs` — must match what the binary actually emits; compare with `cargo run -- --json schema response | jq .`
- `README.md` — the sample envelope must be schema-valid field-for-field, including required fields (`command`, `requestId`, `budget`, `costDollars.model/search`, `diagnostics.retries`)
- `AGENTS.md` — trust rule, verdicts, `relevance`, outcome semantics
- `skills/receipts/SKILL.md` — same contract; this is the copy consumers install via `npx skills`
- `CHANGELOG.md` — new `## X.Y.Z` section at top, consumer-facing wording, breaking changes first, each naming the field and its old → new shape

Done when: all five describe the same fields, verdicts, and semantics, and the changelog's top section number equals `Cargo.toml`'s version.

## 3. Preflight

Commit everything (soft-wrapped body, subject ≤72 chars), then run `scripts/preflight-release.sh`. It verifies: on main, clean tree, fmt/clippy/tests green, changelog has the version, tag doesn't already exist.

Done when: the script prints `preflight OK`.

## 4. Ship

```sh
git push origin main
git tag vX.Y.Z && git push origin vX.Y.Z    # triggers the Release workflow
cargo publish --dry-run && cargo publish     # crates.io
```

Done when: `gh run list --repo treygoff24/receipts --limit 2` shows the Release workflow running for the tag, and `cargo publish` printed `Published receipts vX.Y.Z`.

## 5. Verify every channel

- Release workflow completes green: `gh run watch <run-id> --repo treygoff24/receipts` (it builds five targets; one target failing kills the whole release)
- `gh release view vX.Y.Z --repo treygoff24/receipts` lists installer artifacts
- `cargo search receipts --limit 1` shows the new version
- Homebrew tap formula updated: `gh api repos/treygoff24/homebrew-tap/contents/Formula/receipts.rb --jq .content | base64 -d | grep -i version`

Done when: all four channels show X.Y.Z.

If CI fails before anything published: fix the cause, delete the tag (`git push origin :refs/tags/vX.Y.Z`, `git tag -d vX.Y.Z`), and re-tag the fixed commit. If any artifact already published (GitHub release exists, crates.io uploaded, or formula pushed): never mutate the tag — ship a patch release instead.
