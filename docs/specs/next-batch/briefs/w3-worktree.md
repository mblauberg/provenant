# Slice w3-worktree: worktree locate, carried paths and a remove that chairs can use

Issues: #3810. Wave 3 of the next batch; integration branch `batch/next`. Expected size about 650 changed lines.

## Outcome

Make the worktree commands the ones chairs use (#3810).
- `WorktreeInfo` (`crates/confinement/src/worktrees.rs`) gains `lane`, `attempt`, `base` and `writer`, read from `provenant-owner.json`; `worktree list` shows them and `status --json` rows gain `worktree: {path, base}`.
- New verb *worktree locate QUERY* (register it in `src/commands/worktree.rs`): QUERY is a branch, lane, attempt or PR number; prints the path alone in human mode (for `cd "$(provenant worktree locate x)"`) and `{path, branch, lane, base}` in the envelope; a miss refuses with close matches.
- `worktree remove`: refuses only a dirty tree or one a live writer holds (boundary 2); appends `{branch, tip, path, at}` to `<state>/worktrees/removed.jsonl` (*provenant.worktree-removed.v1*) and prints it; deletes the local branch when it is merged into its base (`ForgeContext`), else keeps it and says so.
- Move the remaining raw `git worktree` calls in production code into `crates/confinement/src/worktrees` owners.

## Interface to honour

These types exist on the integration branch (or, for wave 1 w1-interfaces, are yours to create exactly). Use them as written; do not add parallel types.

```rust
// crates/core/src/names.rs  (owner: core; no dependency on records)
/// Ranked close matches: case-insensitive; exact-after-folding first, then
/// prefix, then substring, then edit distance <= max(1, len/3); at most 5.
pub fn close_matches<'a>(query: &str, universe: impl IntoIterator<Item = &'a str>) -> Vec<&'a str>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameKind(pub &'static str); // "model", "adapter", "account", "graph", "command", ...

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fold { Exact, CaseFolded, Alias { alias: String } }

pub struct Resolved<T> { pub item: T, pub canonical: String, pub fold: Fold }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameRefusal { pub kind: NameKind, pub query: String, pub candidates: Vec<String> }
impl std::fmt::Display for NameRefusal {} // "unknown model `x`; close matches: a, b"

pub trait Catalogue {
    type Item;
    const KIND: NameKind;
    fn names(&self) -> Vec<String>;
    fn lookup(&self, canonical: &str) -> Option<Self::Item>;
    fn aliases(&self) -> Vec<(String, String)> { Vec::new() } // (alias, canonical)
}
/// Exact, then case-folded, then alias; else a refusal carrying close matches.
/// A fold other than Exact is the caller's cue to emit a warning.
pub fn resolve<C: Catalogue>(catalogue: &C, query: &str) -> Result<Resolved<C::Item>, NameRefusal>;
```

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

- `tools/front-door/crates/confinement/src/worktrees.rs`
- `tools/front-door/crates/confinement/src/worktrees/`
- `tools/front-door/src/worktrees.rs`
- `tools/front-door/crates/cli/src/grammar.rs`
- `tools/front-door/src/commands/worktree.rs`
- `tools/front-door/crates/cli/src/run/status.rs`

## Tests

locate by each query kind; remove refuses dirty and live-held, records the tip, prunes a merged branch and keeps an unmerged one; list rows carry lane and base.

## Docs

Update `docs/worktrees.md` and the branch step in `docs/runbooks/github-workflow.md` to use locate and remove.

## Release note

Add `docs/release-notes/next.d/next-batch-w3-worktree.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Added: `provenant worktree locate`; `worktree list` and `status --json` carry path, lane and base; `worktree remove` records the removed tip and prunes merged branches.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
