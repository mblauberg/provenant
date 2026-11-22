# Slice w1-envelope: One warning emitter and envelope v2

Issues: #3778 #3813 #3812. Wave 1 of the next batch; integration branch `batch/next`. Expected size about 650 changed lines. Starts after w1-interfaces, w1-registry have integrated.

## Outcome

Build the edge of the warnings seam.
- New `src/output.rs` owns emission: move `emit`, `emit_summarised`, `emit_with_writers` and `emit_unpublished` out of `src/lib.rs` and route the registry's calls through it. It installs one sink per command: human mode prints `warning: <message>` (plus `  repair: <repair>`) to stderr as each warning arrives and collapses repeats by key; `--json` prints nothing to stderr and puts every warning into the envelope.
- `records::CommandResult` gains `warnings: Vec<Warning>`, always serialised; `ENVELOPE_VERSION` becomes 2; regenerate fixtures that embed the envelope (`crates/cli/src/bin/generate-swift-fixtures.rs`). If the console app decodes `data.warnings`, move it to the envelope field in this slice.
- `Effect::Warn(String)` becomes `Effect::Warn(Warning)`; `Policy::warn_or` calls `records::warning::warn`; update every match arm to forward the value (no arm formats text).
- Dedup: `PerChange` warnings go through `<state>/warnings.sqlite3` (one STRICT table `seen(key TEXT PRIMARY KEY, code TEXT NOT NULL, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, count INTEGER NOT NULL)` via `provenant_sqlite::ensure_schema`); a seen key is not shown again; each write prunes rows older than *warnings.dedup_retention_days* (register it as a host setting, default 30, shown with its source).
- Replace the marker writers (`config-warning-{digest}` in `crates/config/src/loader.rs`, `root-warning-{digest}` in `src/roots.rs`) with `PerChange` warnings, and add migration step *retire-warning-markers* that deletes those files and reports the count.
- Add `std::io::stderr` and `std::io::stdout` to `disallowed-methods` in `tools/front-door/clippy.toml` with a reason pointing at `records::warning`; allow it only in `src/output.rs` and the facade's fallback. Existing call sites elsewhere get a temporary `#[allow]` with the comment `// next-batch wave 2 removes` so wave 2 can find them all; list the count in your handoff.

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

- `tools/front-door/src/output.rs`
- `tools/front-door/src/lib.rs`
- `tools/front-door/src/commands/mod.rs`
- `tools/front-door/crates/records/src/command_result.rs`
- `tools/front-door/crates/records/src/wire/mod.rs`
- `tools/front-door/crates/records/src/policy.rs`
- `tools/front-door/crates/cli/src/render.rs`
- `tools/front-door/crates/config/src/loader.rs`
- `tools/front-door/src/roots.rs`
- `tools/front-door/crates/sqlite/`
- `tools/front-door/clippy.toml`
- `tools/front-door/crates/cli/src/bin/generate-swift-fixtures.rs`

## Tests

Characterisation: a command that warns returns the warning in the envelope and nothing on stderr under `--json`; human mode prints it once; a `PerChange` warning shows once across two invocations sharing a state root and again after its key changes; pruning removes only stale rows; the marker migration deletes markers and nothing else.

## Docs

Amend `docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning` for the v2 shape; note `warnings.sqlite3` in `docs/specs/disk.md`.

## Release note

Add `docs/release-notes/next.d/next-batch-w1-envelope.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Changed: the JSON envelope is version 2 and carries a `warnings` array; warnings no longer print to stderr under `--json`; repeated configuration and root warnings show once per change, and the state root's `config-warning-*` and `root-warning-*` marker files are removed on upgrade.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
