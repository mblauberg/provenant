# Slice w1-project-dirs: One host-project directory key

## Amendments (chair, 23 Nov 2026, against main af8a73e3e9 after v2.8.2)
These supersede any conflicting text below.
- ProjectId is the canonical key: `config/src/ports.rs` already calls the `<project_key>` layout obsolete and warns on a misplaced `ports.json`. No need to confirm the key.
- `config/src/ports.rs` and `config/src/github_policy.rs` also build `projects/` paths; move them too (in your write scope).


Issues: #3777 (bug). Wave 1 of the next batch; integration branch `batch/next`. Expected size about 350 changed lines. Starts after w1-interfaces have integrated.

## Outcome

Host-project state is keyed two ways: `project_binding::host_project_dir` uses `projects/<ProjectId>`, while `crates/config/src/overlays.rs` and `crates/config/src/admission.rs` build `projects/<sha256 of the project path>`. Confirm which key install and project binding actually write, make `host_project_dir` the only resolver every owner calls, and add migration step *host-project-dirs* that moves directories written under the other key (merging only when the target is absent; a conflict warns with both paths and leaves both).

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

- `tools/front-door/crates/config/src/project_binding.rs`
- `tools/front-door/crates/config/src/overlays.rs`
- `tools/front-door/crates/config/src/admission.rs`
- `tools/front-door/src/install/migrations.rs`

## Tests

A characterisation test that a host-project overlay and admission setting written through binding is read back by overlays and admission (fails today if the keys differ); migration fixture with the old key moves it; conflict fixture warns and leaves both.

## Docs

Note the one key under *Roots and identities* in `docs/specs/state.md`.

## Release note

Add `docs/release-notes/next.d/next-batch-w1-project-dirs.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Fixed: host-project overlay and admission settings are read from the same directory project binding writes; upgrade moves any written under the other key.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
