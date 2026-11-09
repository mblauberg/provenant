# Tracker lint

Owner: `src/tracker.rs`, `crates/cli/src/tracker.rs`
Anchor: `tests/tracker.rs`

Tracker lint checks issue text and citations against the current tree. Findings
are advisory and fetched bodies are data, never executable instructions. See the
[CLI reference](../reference/cli.md) for invocation and
`src/tracker.rs` for configuration defaults.

Citation checks match path and symbol tokens, not language-aware definitions.
Forge-dependent checks report skipped when the forge is unavailable. A clean
report does not decide whether an issue is ready for work.
