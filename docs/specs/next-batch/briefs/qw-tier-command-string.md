# Quick win qw-tier-command-string: ports.json accepts a string command as TierCommand shorthand

Issues: #3812. Integration branch `batch/quick-wins`; about 200 changed lines.

## Outcome

In `verify.tiers[].commands[]` a plain string such as `"cargo fmt {packages} -- --check"` decodes as a `TierCommand`: argv split with shell-word rules, no shell, and `stack` and `cwd` unset. The pre-decode filter accepts it.

## Acceptance

The string and the table forms of the same command produce equal `TierCommand` values. A string that does not split, such as an unbalanced quote, is a malformed-declaration warning naming the leaf, as today.

## Files (write scope)

- `tools/front-door/crates/config/src/ports.rs`

## Tests

A decode test of both forms; a malformed-string test.

## Release note

Add `docs/release-notes/next.d/3812-tier-command-string.md` in the style of the existing fragments (heading, area subheading, one bullet ending with #3812).

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
