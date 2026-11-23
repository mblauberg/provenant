# Quick win qw-check-install-roots: check install verifies the recorded install roots, whatever HOME is

Issues: #3811. Integration branch `batch/quick-wins`; about 200 changed lines. Order: after #3816 merges (it edits `src/checks.rs`).

## Outcome

`provenant check install` builds its targets from the install receipt's recorded roots, not from the caller's `$HOME`. With `HOME` set to an account directory (as inside a lane), a healthy install reports 0 findings.

## Acceptance

`Targets::new`, or its replacement, takes the recorded roots from the receipt, and no path in the check reads `HOME` directly.

## Files (write scope)

- `tools/front-door/src/checks.rs`
- `tools/front-door/crates/config/src/managed.rs`
- `tools/front-door/crates/cli/src/check/mod.rs`

## Tests

A check test with a fixture receipt and a context whose home differs from the recorded root reports no `missing_receipt`. Pass the home through the context; never mutate the process environment.

## Release note

Add `docs/release-notes/next.d/3811-check-install-roots.md` in the style of the existing fragments (heading, area subheading, one bullet ending with #3811).

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
