# Quick win qw-docs: Chair-rule home and a one-page quickstart

Issues: #3784 #3814. Integration branch `batch/quick-wins`; about 300 changed lines.

## Outcome

(1) The one-accountable-chair rule gets its home in `skills/orchestrate`. (2) `docs/quickstart.md` gives one runnable first-install sequence: install, `provenant check install`, register an account, a first read-only lane, a first writer. `docs/README.md` and `README.md` link it as "start here", and `install.md` leads with first install, with upgrade material after it.

## Acceptance

Every `provenant` command in the new text exists; `scripts/check-doc-truth --binary "$(command -v provenant)"` reports no error for these files.

## Files (write scope)

- `skills/orchestrate/SKILL.md`
- `docs/quickstart.md`
- `docs/README.md`
- `README.md`
- `docs/runbooks/install.md`

## Tests

The doc-truth check itself.

## Release note

Add `docs/release-notes/next.d/3784-docs.md` in the style of the existing fragments (heading, area subheading, one bullet ending with #3784 #3814).

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
