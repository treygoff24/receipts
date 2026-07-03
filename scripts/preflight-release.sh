#!/usr/bin/env bash
# Deterministic release preflight for receipts. Judgment steps (version choice,
# contract-surface sync) live in .claude/skills/release/SKILL.md.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

branch=$(git branch --show-current)
[ "$branch" = "main" ] || { echo "FAIL: on branch '$branch', not main"; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "FAIL: working tree not clean"; exit 1; }

version=$(cargo pkgid | sed 's/.*[#@]//')
grep -q "^## ${version}\$" CHANGELOG.md || { echo "FAIL: CHANGELOG.md has no '## ${version}' section"; exit 1; }
if git rev-parse -q --verify "refs/tags/v${version}" >/dev/null; then
  echo "FAIL: tag v${version} already exists"
  exit 1
fi

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

echo "preflight OK: ready to ship v${version}"
