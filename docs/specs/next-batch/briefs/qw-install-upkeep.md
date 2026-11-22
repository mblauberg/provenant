# Quick win qw-install-upkeep: upgrade --verify runs the new binary; retire cargo-target-seeds; smoke leaves no registered scratch project

Issues: #3799 #3813 #3812. Integration branch `batch/quick-wins`; about 350 changed lines.

## Outcome

(1) After an upgrade installs, the verification runs the installed destination binary (exec with `upgrade --verify`), not the running pre-upgrade process. (2) Install removes `<state>/cargo-target-seeds/` when present, printing its path and size; it belongs to the seeding feature that #3790 removed. (3) The smoke fixture unregisters the scratch project it registers, so `tick` no longer reads its `ports.json`. Check (3) on v2.8.2 first: if #3800 or #3815 already removed the leftover registration, drop it from the slice.

## Acceptance

The verify program path equals the installed destination. A state root with `cargo-target-seeds/` loses it once and reports it. After `upgrade --verify`, the host registry lists no project under the scratch fixture root.

## Files (write scope)

- `tools/front-door/src/install.rs`
- `tools/front-door/src/release_smoke.rs`
- `tools/front-door/src/tick.rs`

## Tests

Inject the runner and assert its program path; run a state-root fixture with and without the directory; assert the registry after the smoke fixture.

## Release note

Add `docs/release-notes/next.d/3799-install-upkeep.md` in the style of the existing fragments (heading, area subheading, one bullet ending with #3799 #3813 #3812).

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
