# Contributing

Issues and PRs welcome.

The gate: `cargo test && cargo clippy --all-targets && cargo fmt --check`. CI runs the same three; green gate, mergeable PR.

Two invariants to respect in any change:

1. Stdout carries exactly one success envelope; stderr carries exactly one error envelope. Nothing else ever prints. No prompts, colors, spinners, or interactive fallbacks.
2. `capabilities` and `schema` output must stay truthful — if you change a command, flag, exit code, or env var, update the contract they report and the tests in `tests/cli.rs` that pin it.

Design history lives in `docs/plans/`. If you're proposing something structural, read the design doc first.
