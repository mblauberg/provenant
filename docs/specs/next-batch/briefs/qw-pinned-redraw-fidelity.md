# Quick win qw-pinned-redraw-fidelity: Pinned exhausted route: advisory attempt, true cause, future expiry

Issues: #3764. Integration branch `batch/quick-wins`; about 300 changed lines.

## Outcome

F1: when the queue drains after an advisory redraw, the pinned route is attempted once on its refused account, with the warning. F2: a provider-refusal redraw records its real exclusion reason, not Exhausted. F3: an advisory expiry is used only when it is in the future.

## Acceptance

As in the issue's rescope comment.

## Files (write scope)

- `tools/front-door/crates/cli/src/run/provider/preparation.rs`
- `tools/front-door/crates/routing/src/`

## Tests

One test per finding at the preparation boundary.

## Release note

Add `docs/release-notes/next.d/3764-pinned-redraw-fidelity.md` in the style of the existing fragments (heading, area subheading, one bullet ending with #3764).

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
