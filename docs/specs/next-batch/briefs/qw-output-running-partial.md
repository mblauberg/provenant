# Quick win qw-output-running-partial: output on a running attempt prints the partial result

Issues: #3809. Integration branch `batch/quick-wins`; about 200 changed lines.

## Outcome

`provenant output ATTEMPT` on a running attempt prints what has arrived, under a `running` header, and exits 0. `RunError::Nonterminal` is deleted from this path.

## Acceptance

A running attempt's output exits 0, carries `status: running` in the envelope and states that the result is partial.

## Files (write scope)

- `tools/front-door/crates/run/src/reader.rs`
- `tools/front-door/crates/cli/src/run/output.rs`

## Tests

A reader test with a nonterminal attempt fixture.

## Release note

Add `docs/release-notes/next.d/3809-output-running-partial.md` in the style of the existing fragments (heading, area subheading, one bullet ending with #3809).

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
