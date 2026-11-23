# Slice w2-remote-literals: Replace compiled-in origin, main and master with ForgeContext

Issues: #3288. Wave 2 of the next batch; integration branch `batch/next`. Expected size about 500 changed lines.

## Outcome

Replace every production `"origin"`, `"main"`, `"master"` and `refs/heads/main` literal with values from a `ForgeContext` passed in by the caller (resolve it once per command at the owner; never rebuild it in a helper). Known sites include `src/handoff.rs`, `src/cloud.rs`, `src/gc_support.rs`, `crates/confinement/src/worktrees/gc.rs`, `crates/run/src/delivery.rs`, `crates/confinement/src/worktrees/lifecycle.rs`, `crates/run/src/writer.rs` and `src/lane_cleanup.rs`; find the rest with `rg -n '"origin"|"main"|"master"|refs/heads/main' --type rust` outside test modules. Move gc's default-branch protection onto `ForgeContext::is_protected`. Test fixtures may keep literals.

## Interface to honour

These types exist on the integration branch (or, for wave 1 w1-interfaces, are yours to create exactly). Use them as written; do not add parallel types.

```rust
// crates/config/src/forge.rs  (owner: config ports; the forge port consumes it)
pub struct ForgeContext {
    pub remote: Sourced<String>,          // declared, else the branch's upstream remote, else the sole remote
    pub default_branch: Sourced<String>,  // declared, else refs/remotes/<remote>/HEAD, else the forge API
    pub protected: Sourced<Vec<String>>,  // declared, else {default branch} plus forge-reported protected branches
    pub policy: Sourced<provenant_records::policy::Policy>, // the owner's resolved operation policy
}
impl ForgeContext {
    /// Resolved once per command by the owner that needs a forge or a remote;
    /// passed down, never rebuilt. Detection that finds nothing is a refusal
    /// that lists the remotes or branches it saw (with close matches).
    pub fn resolve(project: &Project, config: &Config, git: &Git) -> Result<Self, ForgeContextError>;
    pub fn is_protected(&self, branch: &str) -> bool;
}
```

## Files (write scope)

- `tools/front-door/src/handoff.rs`
- `tools/front-door/src/cloud.rs`
- `tools/front-door/src/gc_support.rs`
- `tools/front-door/crates/confinement/src/worktrees/gc.rs`
- `tools/front-door/crates/run/src/delivery.rs`
- `tools/front-door/crates/confinement/src/worktrees/lifecycle.rs`
- `tools/front-door/crates/run/src/writer.rs`
- `tools/front-door/src/lane_cleanup.rs`

## Tests

Extend the `upstream`-only fixture from w1-forge to a handoff, a delivery and a gc dry-run: each uses `upstream` and protects its default branch.

## Docs

none.

## Release note

Add `docs/release-notes/next.d/next-batch-w2-remote-literals.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Fixed: handoff, delivery, cloud and gc work with a remote that is not named `origin` and a default branch that is not `main`.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
