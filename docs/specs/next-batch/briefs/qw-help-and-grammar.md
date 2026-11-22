# Quick win qw-help-and-grammar: Nested help paths, globals once in help --all, config path [DIR], stale doc strings

Issues: #3808 #3814. Integration branch `batch/quick-wins`; about 350 changed lines. Order: after #3817 merges (it edits `crates/cli/src/lib.rs`).

## Outcome

(1) `provenant help run implement` prints that subcommand's help; an unknown path lists the valid children at the deepest matched level. (2) `help --all` prints the global options once, at the top. (3) `config path [DIR]` takes an optional positional directory, and the ignored required `--project` bool on it is deleted with no alias. (4) The `pr` doc string no longer claims a watch command, and `worktree`'s `--base` doc string matches what the flag does.

## Acceptance

All four hold in `provenant help --all` and `provenant config path .`.

## Files (write scope)

- `tools/front-door/crates/cli/src/lib.rs`
- `tools/front-door/crates/cli/src/grammar.rs`
- `tools/front-door/src/lib.rs`

## Tests

A help-tree test: nested resolution, and each global option name appears once in `help --all`. Count names, never snapshot prose. A grammar test for the positional.

## Release note

Add `docs/release-notes/next.d/3808-help-and-grammar.md` in the style of the existing fragments (heading, area subheading, one bullet ending with #3808 #3814).

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
