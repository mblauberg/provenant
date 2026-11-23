# Slice w1-record-migration: Turn the pre-rename boundary aliases into a migration step

Issues: #3801 follow-up. Wave 1 of the next batch; integration branch `batch/next`. Expected size about 300 changed lines. Starts after w1-interfaces have integrated.

## Outcome

#3815 decodes the retired `Boundary` names (`git_isolation`, `data_integrity`) as `OtherWork` wherever a policy snapshot is read, and drops `launch_policy` from attempt rows on decode. That is a legacy reader. Replace it with migration step *pre-rename-boundary-records*: rewrite stored policy snapshots (retired names to `other_work`) and strip `launch_policy` from stored attempt rows, then delete the decode aliases so readers accept only current names. First establish whether any digest seals those rows (read the store and record codecs). If one does, do not rewrite: instead extend the drop-on-decode clean break to every snapshot reader and still delete the aliases, and say which in the handoff.

## Interface to honour

These types exist on the integration branch (or, for wave 1 w1-interfaces, are yours to create exactly). Use them as written; do not add parallel types.

```rust
// crates/records/src/migrate.rs  (owner: records; the runner is src/install/migrations.rs)
pub struct MigrationReport { pub id: &'static str, pub changed: Vec<String>, pub warnings: Vec<crate::warning::Warning> }
pub trait Migration {
    /// Stable id, recorded in the install receipt once applied.
    fn id(&self) -> &'static str;
    /// Must be idempotent: a rerun after a partial failure finishes the job.
    fn apply(&self, state_root: &std::path::Path) -> Result<MigrationReport, MigrationError>;
}
// Install runs every step not yet recorded in the receipt, in declaration order,
// after the live-attempt check and before swapping binaries; it records each id
// only after that step succeeds. A failed step leaves the receipt unchanged,
// prints its warning with the path and proceeds with the install.
// Steps are deleted in the release after the one that ships them.
```

## Files (write scope)

- `tools/front-door/crates/records/src/policy.rs`
- `tools/front-door/crates/records/src/run.rs`
- `tools/front-door/src/install/migrations.rs`
- `tools/front-door/crates/records/tests/forward_compatibility.rs`

## Tests

A state-root fixture with schema-2 rows holding both retired names: after the step, every row decodes with current names and the step is idempotent; a retired name after migration is refused by the decoder (no alias left). Keep #3815's retention test (legacy row beside a healthy row).

## Docs

Note the step under *Format changes migrate once at install* in `docs/specs/state.md`.

## Release note

Add `docs/release-notes/next.d/next-batch-w1-record-migration.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Changed: upgrade rewrites attempt records written before the boundary rename once; readers no longer accept the retired names.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
