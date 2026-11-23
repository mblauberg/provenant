# Slice w2-warn-lib-b: Warnings codemod: run, confinement, supervisor, fabric-runtime, workflow and the remaining library crates

Issues: #3778. Wave 2 of the next batch; integration branch `batch/next`. Expected size about 500 changed lines.

## Outcome

Wave 1 added `records::warning` and a lint that forbids `std::io::stderr` and `std::io::stdout` outside the emitter; existing sites carry a temporary `#[allow(clippy::disallowed_methods)]` marked `// next-batch wave 2 removes`. In crates/run, crates/confinement, crates/supervisor, crates/fabric-runtime, crates/workflow and any other library crate not listed in w2-warn-lib-a:
- Replace every production warning print (`eprintln!("warning: ..")`, `writeln!(io::stderr(), "warning: ..")`, local `warn_*` helpers, and any `data.warnings: Vec<String>` payload field) with `records::warning::warn(Warning::new(CODE, message)...)`, setting `subject`, `source` (when a setting caused it) and `repair` where the old text had them. Use a code from the catalogue in `crates/records/src/warning.rs`; add a code there (UPPER_SNAKE, one-line meaning) when none fits, and list added codes in the handoff.
- Configuration and root warnings that used to write marker files use `.once_per_change()`.
- Delete each local warning helper and each `data.warnings` field once its last user moves; update consumers of the deleted field (search the whole workspace and `apps/`).
- Remove the temporary `#[allow]` and its marker comment from every site you convert. Any non-warning stderr print you find (progress, prompts) either moves to the emitter or keeps an `#[allow]` with a one-line reason; list those in the handoff.
- Do not edit files outside these directories except the warning catalogue; if a site needs it, list it in the handoff.

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

- `tools/front-door/crates/run/`
- `tools/front-door/crates/confinement/`
- `tools/front-door/crates/supervisor/`
- `tools/front-door/crates/fabric-runtime/`
- `tools/front-door/crates/workflow/`

## Tests

No new behaviour tests. Where a converted site had a test asserting its stderr text, change it to assert the `Warning` code captured with `records::warning::capture`.

## Docs

none.

## Release note

none (one fragment for the whole warnings seam comes from w1-envelope).

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
