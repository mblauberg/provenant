# Quick win qw-gc-default-branch: gc protects the default branch on remote refs and linked worktrees

Issues: #3768. Integration branch `batch/quick-wins`; about 250 changed lines.

## Outcome

`remote_outcomes` applies the same default-branch exclusion as local retirement when `target` is `None`: in dry-run, and when `remote_default` fails. A linked worktree checked out on the default branch is never retired. Excluded branches show as Retained with a reason.

## Acceptance

No gc path retires or modifies the default branch.

## Files (write scope)

- `tools/front-door/src/lane_cleanup.rs`
- `tools/front-door/crates/confinement/src/worktrees/gc.rs`

## Tests

Remote-ledger fixtures for dry-run and a failed `remote_default`; a linked-worktree-on-default fixture.

## Release note

Add `docs/release-notes/next.d/3768-gc-default-branch.md` in the style of the existing fragments (heading, area subheading, one bullet ending with #3768).

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
