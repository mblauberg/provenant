# Quick win qw-budget-completeness: Token-budget settlement keeps the decoder's completeness

Issues: #3405. Integration branch `batch/quick-wins`; about 150 changed lines.

## Outcome

The budget path no longer sets `decoded.complete = true`. One gate decides success, and a truncated, non-empty result under a done handoff does not settle as `Succeeded`.

## Acceptance

A budget-stopped lane with a truncated result and a done handoff settles as incomplete.

## Files (write scope)

- `tools/front-door/crates/run/src/holder.rs`

## Tests

A holder settlement test for that case, plus the empty-result case keeping its handoff-fence substitution.

## Release note

Add `docs/release-notes/next.d/3405-budget-completeness.md` in the style of the existing fragments (heading, area subheading, one bullet ending with #3405).

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
