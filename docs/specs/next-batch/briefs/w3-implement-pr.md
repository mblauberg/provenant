# Slice w3-implement-pr: run implement --pr N; an existing branch behind its base warns

Issues: #3500 #3644. Wave 3 of the next batch; integration branch `batch/next`. Expected size about 700 changed lines. Starts after w3-ci-observer have integrated.

## Outcome

(#3500) `provenant run implement --pr N`: resolve the PR's head branch and base through the forge, locate or create its worktree, and append the failing checks (names and bounded log tails from the CI observer) to the brief. (#3644) An existing branch behind its base warns *BRANCH_BEHIND_BASE* and proceeds; `--base` records the merge target and never resets the branch. Delete `WorktreeError::BranchExistsWithBase` and its `WRITER_SCOPE` mapping in `crates/run/src/service.rs` (and the arms in `src/worktrees.rs`, `src/handoff.rs`, `crates/run/src/lib.rs`). Coordinate with #3806's sourced history setting if it has landed: use it rather than adding a parallel knob.

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

- `tools/front-door/src/fabric/interface.rs`
- `tools/front-door/crates/confinement/src/worktrees/lifecycle.rs`
- `tools/front-door/crates/confinement/src/worktrees.rs`
- `tools/front-door/crates/run/src/lib.rs`
- `tools/front-door/src/handoff.rs`
- `tools/front-door/crates/cli/src/grammar.rs`

## Tests

A fake forge PR with a failing check: the launch binds branch and base and the brief carries the failure; a branch behind its base launches with the warning; `--base` does not move the branch.

## Docs

Update the repair step in `docs/runbooks/github-workflow.md`.

## Release note

Add `docs/release-notes/next.d/next-batch-w3-implement-pr.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Added: `provenant run implement --pr N` repairs a pull request from its CI failures. Changed: an existing branch behind its base warns instead of refusing.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
