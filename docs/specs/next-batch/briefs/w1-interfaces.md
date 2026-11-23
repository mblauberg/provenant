# Slice w1-interfaces: Shared seam types and the migration registry

## Amendments (chair, 23 Nov 2026, against main af8a73e3e9 after v2.8.2)
These supersede any conflicting text below.
- SQLite stores already migrate on writable open (`provenant_sqlite::Migration { from, to, sql }` in `crates/sqlite/src/schema.rs`; documented in `docs/specs/state.md` "one-shot migrations"). `records::migrate` and the install-receipt migration steps are for non-SQLite state only (files, ledgers, receipts); say so in the module docs and in the new doc section so it agrees with state.md. Do not duplicate or wrap the SQLite mechanism.
- Current numbers: `LIVE_RECORD_FORMAT` = 6 (`crates/records/src/install.rs`), run store base 13, `INSTALL_RECEIPT_SCHEMA_VERSION` = 2 (no `migrations` field yet).
- The w1-record-migration slice was dropped (done by #3822); there is no *boundary-aliases* step to register.


Issues: #3777 #3778 #3808 #3709. Wave 1 of the next batch; integration branch `batch/next`. Expected size about 550 changed lines.

## Outcome

Add the shared seam types exactly as given below, with unit tests of their invariants, and the install-time migration runner. Nothing else in the workspace changes behaviour yet: later slices adopt these types.
- `records::provenance`: implement `Precedence::merge` (highest listed layer wins; ties keep the last given; unlisted layers go to `ignored`).
- `records::warning`: implement the facade: a thread-local capture stack checked first, then a process-global `RwLock<Option<Arc<dyn Sink>>>` with a guard that restores the previous sink. With no sink installed, `warn` writes `warning: <message>` to stderr; this one function carries `#[allow(clippy::print_stderr, clippy::disallowed_methods)]` and nothing else may. Declare an initial catalogue: *CONFIG_KEY_IGNORED*, *CONFIG_MALFORMED*, *SETTING_LAYER_IGNORED*, *NAME_CASE_FOLDED*, *NAME_ALIAS_USED*, *BRANCH_BEHIND_BASE*, *WORKTREE_ADOPTED_DIRTY*, *LIVE_RECORD_FORMAT_CHANGED*, *MIGRATION_STEP_FAILED*. Codes are UPPER_SNAKE, like refusal codes.
- `core::names`: move `core::edit_distance` here (keep it `pub` for the moment; w1-names deletes the other copies) and implement `close_matches`, `resolve` and `valid_name` (lower-case ASCII letters, digits, `-` and `_`; must start with a letter or digit; `valid_name` folds case and reports whether it folded).
- `records::usage`: the shared bucket and cost types.
- `records::migrate` and `src/install/migrations.rs`: the trait, an empty ordered step list, and the runner wired into install after the live-attempt check and before the binary swap. Record applied ids in the install receipt (new `migrations: Vec<String>` field with `#[serde(default)]`), only after each step succeeds. A failed step warns *MIGRATION_STEP_FAILED* with the path and the install proceeds.

## Interface to honour

These types exist on the integration branch (or, for wave 1 w1-interfaces, are yours to create exactly). Use them as written; do not add parallel types.

```rust
// crates/records/src/provenance.rs  (owner: records)
/// Where a setting's value came from. One vocabulary for every family.
/// Wire spelling is snake_case and equals the current LimitSource spelling
/// where they overlap, so limit receipts keep their bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Default,      // built-in or bundled value (today: Default, Builtin, Bundled, ShippedDefault)
    Profile,      // a named preset bundle (today: KnobSource::Profile, schema Layer::Profile)
    Detected,     // derived from the repository or host (stack, remote, default branch)
    Host,         // the owner's host config file
    HostProject,  // host state bound to one project
    Project,      // the repository's .provenant files
    Account,      // an account-scoped policy
    Tier,         // the routing tier selected for the work
    Template,     // the template a node expands from
    Route,        // the resolved route's policy (today: RoutePolicy, sandbox Route)
    Parent,       // inherited from the launching lane
    Launch,       // a flag, environment variable or launch request
}

/// Layer plus the exact place, for example "~/.config/provenant/config.toml#lanes.writer_dirty",
/// ".provenant/ports.json#verify.remote" or "--max-turns".
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Source {
    pub layer: Layer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// A value with its source. Serialises as {"value": .., "source": "<layer>", "origin": ".."}.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sourced<T> {
    pub value: T,
    #[serde(flatten)]
    pub source: Source,
}

/// One family's precedence, lowest first. Families declare it as a const;
/// the merge and every explanation read it, so order lives in one place per family.
pub struct Precedence(pub &'static [Layer]);

impl Precedence {
    /// The candidate from the highest layer present; ties keep the last given.
    /// A candidate whose layer the family does not list is ignored and
    /// returned in Merge::ignored, so the caller can warn with its source.
    pub fn merge<T>(&self, candidates: impl IntoIterator<Item = Sourced<T>>) -> Merge<T>;
}
pub struct Merge<T> { pub winner: Option<Sourced<T>>, pub shadowed: Vec<Sourced<T>>, pub ignored: Vec<Sourced<T>> }
```

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
// crates/records/src/usage.rs  (owner: records; shared by UsageRecord and the stream)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: Option<u64>, pub output: Option<u64>,
    pub cache_read: Option<u64>, pub cache_write: Option<u64>,
    pub cache_write_1h: Option<u64>, pub reasoning: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageScope { Step, Cumulative }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost { pub micros: u64, pub currency: String, pub stated_by: CostOrigin }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostOrigin { Provider, Catalogue }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource { Stream, SessionStore, Ingest }

// crates/providers/src/usage.rs  (owner: providers)
pub struct UsageReport { pub scope: UsageScope, pub key: Option<String>, pub model: Option<String>, pub tokens: Tokens, pub cost: Option<Cost> }
/// Declared per adapter in AdapterSpec; the accumulator reads it, nobody else guesses.
pub struct UsageSemantics {
    pub scope: UsageScope,              // what one report means
    pub input_includes_cache_read: bool,
    pub live: bool,                     // reports arrive during the run (token budget can act)
}
/// The one merge: Step reports with a key replace that key's earlier report;
/// keyless Step reports add; a Cumulative report replaces the running total.
/// Buckets stay None until some report states them.
pub struct UsageAccumulator { /* .. */ }
impl UsageAccumulator {
    pub fn new(semantics: UsageSemantics) -> Self;
    pub fn push(&mut self, report: UsageReport);
    pub fn total(&self) -> (Tokens, Option<Cost>); // input normalised to exclude cache reads
}
// ProviderEvent::Usage { .. } becomes ProviderEvent::Usage(UsageReport).
```

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

- `tools/front-door/crates/records/src/provenance.rs`
- `tools/front-door/crates/records/src/warning.rs`
- `tools/front-door/crates/records/src/usage.rs`
- `tools/front-door/crates/records/src/migrate.rs`
- `tools/front-door/crates/records/src/lib.rs`
- `tools/front-door/crates/core/src/names.rs`
- `tools/front-door/crates/core/src/lib.rs`
- `tools/front-door/src/install/migrations.rs`
- `tools/front-door/src/install.rs`

## Tests

Invariant tests: merge picks the highest listed layer and reports shadowed and ignored candidates; a `Sourced` round-trips to the documented JSON shape; `warn` under `capture` collects and prints nothing; nested sinks restore; a `PerChange` key is stable for equal content and changes with the subject; `close_matches` ranks a one-edit typo first and returns nothing for an unrelated string; `resolve` folds case with `Fold::CaseFolded`; the migration runner records only successful ids and reruns a failed step.

## Docs

Add sections to `docs/specs/state.md` (*Every setting reports its layer*, *Format changes migrate once at install*) and `docs/specs/cli.md` (*Warnings are typed values*), each one invariant, a Why and an Owner; tests cite them with `Spec: docs/specs/NAME.md#anchor`.

## Release note

Add `docs/release-notes/next.d/next-batch-w1-interfaces.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Internal: shared types for settings, warnings, names and usage, and an install-time migration runner.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
