# Slice w3-writer-dirty: Adopt unheld uncommitted work by default

Issues: #3649. Wave 3 of the next batch; integration branch `batch/next`. Expected size about 350 changed lines.

## Outcome

(#3649) For a worktree no live writer holds, the default `lanes.writer_dirty` becomes `wip_commit`: the preflight in `src/fabric/graph_execution.rs` commits the uncommitted work as a WIP commit and warns *WORKTREE_ADOPTED_DIRTY* naming the commit. A tree another lane holds still refuses (boundary 2). Update the default in `config/harness-knobs.json` and `crates/run/src/writer_dirty.rs`; the setting still shows its source.

## Interface to honour

These types exist on the integration branch (or, for wave 1 w1-interfaces, are yours to create exactly). Use them as written; do not add parallel types.

```rust
// crates/records/src/warning.rs  (owner: records; the sink is installed by the binary edge)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    /// Stable UPPER_SNAKE code from the declared catalogue in this module
    /// (same style and stability rules as refusal codes, ADR 0034).
    pub code: std::borrow::Cow<'static, str>, // built from a WarningCode const
    /// One sentence for a person; never a secret value (redaction as for errors).
    pub message: String,
    /// What it is about: a path, key, branch, attempt or route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The setting that caused it, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::provenance::Source>,
    /// One command or edit that clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
    #[serde(skip)]
    pub dedup: Dedup,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dedup {
    #[default]
    PerInvocation, // the same key collapses within one command (count kept)
    PerChange,     // shown once until its key changes; remembered in the state root
}
/// Declared catalogue: one const per code with a one-line meaning. Adding a code is additive.
pub struct WarningCode(pub &'static str);

impl Warning {
    pub fn new(code: WarningCode, message: impl Into<String>) -> Self;
    pub fn subject(self, s: impl Into<String>) -> Self;
    pub fn source(self, s: crate::provenance::Source) -> Self;
    pub fn repair(self, r: impl Into<String>) -> Self;
    pub fn once_per_change(self) -> Self;
    /// sha256 over code, subject and message: the dedup key.
    pub fn key(&self) -> String;
}

/// The only way library code reports a warning. Routes to, in order: the
/// innermost capture on this thread; the process sink the binary installed
/// for this command (one command per process, so worker threads reach it);
/// else stderr as "warning: <message>".
pub fn warn(w: Warning);
pub trait Sink: Send + Sync { fn accept(&self, w: Warning); }
/// Installs the process sink for the current command; dropping the guard restores the previous one.
pub fn install(sink: std::sync::Arc<dyn Sink>) -> SinkGuard;
/// Thread-local: collects warnings raised on this thread inside f (tests, or an
/// owner that must return them as data). Parallel tests do not see each other's.
pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<Warning>);
```

## Files (write scope)

- `tools/front-door/crates/run/src/writer_dirty.rs`
- `tools/front-door/src/fabric/graph_execution.rs`
- `config/harness-knobs.json`

## Tests

Unheld dirty tree: adopted with a WIP commit and the warning; held dirty tree: refused; explicit `refuse` setting still refuses.

## Docs

Update the `lanes.writer_dirty` entry in `skills/orchestrate/SKILL.md`.

## Release note

Add `docs/release-notes/next.d/next-batch-w3-writer-dirty.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Changed: a writer launched on an unheld worktree with uncommitted changes commits them as WIP and proceeds, by default.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
