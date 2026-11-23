# Next batch: shared seams

Owner: the chair running this batch
Anchor: `docs/specs/next-batch/` (graph documents and briefs), `docs/specs/next-batch-quick-wins.md`

This is a transient design record, not an invariants page. Each slice folds
its seam's invariant into the owning area spec (named per seam below) in the
same change. When the batch lands, delete this page, the quick-wins page and
`docs/specs/next-batch/`. Names written in *italics* are proposed codes,
config keys or commands. They are not in backticks because they do not exist
yet, and `scripts/check-doc-truth` checks backticked names.

The batch starts from `main` after these are merged: #3815 (#3801, #3802),
#3816 (#3805) and #3817 (#3803). It also needs #3806 settled and `main` green
(#3804). The quick wins land first.

## Decisions at a glance

| Seam | Decision | Rejected alternative and why |
|---|---|---|
| Warnings | A typed `Warning` goes through a `log`-style facade (`records::warning::warn`) to one sink per command. The sink is installed at the binary edge. The envelope carries `warnings[]`. | Return `Warned<T>` from every function: that means signature churn in about 70 files. The 35 hand-written `Effect::Warn` arms show that threading values by hand already failed to keep one format. |
| Warnings | A lint enforces it: `std::io::stderr` and `eprintln!` are disallowed outside the emitter (`clippy.toml`). | A test that greps for `"warning:"`: it misses `writeln!(stderr)`, which is how today's sites get around `print_stderr = "deny"`. |
| Dedup | One SQLite table, `warnings.sqlite3`, holds only the `PerChange` warnings. It is pruned by age. | One marker file per digest (today): never pruned, and it clutters the state root (#3813). |
| Settings | One `Layer` vocabulary. Each family declares its own `Precedence`. | One global linear order: families really do differ (limits rank tier and template; ports rank detected and host-project), and a forced order would change behaviour silently. |
| Settings | Wire spellings are chosen to match today's persisted strings. Only the strings that differ get a migration step. | Rename freely and keep aliases: #3801 showed what renames do to stored records. |
| Names | One ranker and `Catalogue` trait in `core`. Case folding and aliases proceed with a warning. | Clap aliases alone: they cover subcommands only, not models, accounts, graphs or help targets. |
| Domains | This batch adds a registry of `Domain` and `Verb` values. File splits come in the next batch. | Splitting `holder.rs` and `unified.rs` now: it conflicts with nearly every slice here. |
| Loop | One `ForgeContext` (remote, default branch, protected set, policy) is resolved once and passed down. | Fixing the `origin` literals one by one: discovery is already implemented three times. |
| Loop | `land` gains an owner-granted admin publication mode, read from the host layer only. | Keep `gh pr merge --admin` outside `land`: chairs then bypass `land`'s exact-candidate gate and its receipts. |
| Usage | Adapters emit `UsageReport` values. `AdapterSpec` declares each adapter's semantics. One accumulator merges them. | Per-adapter fixes in `run.rs`: the last report wins today, and bucket meaning is undeclared. |
| Migration | One install-time migration registry. Each step is idempotent and recorded in the receipt. | Decode-time aliases (as in #3815): that is a legacy reader, and it grows with each rename. |

## Seam 1: warnings and the output envelope

Area spec: [CLI](cli.md#one-bounded-envelope-clipped-with-a-warning), with a
new section, *Warnings are typed values*. Issues: #3778, #3813 (markers), #3812 (dedup).

**Contract.**
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

The envelope becomes version 2. `ENVELOPE_VERSION` in
`crates/records/src/wire/mod.rs` goes from 1 to 2. `warnings` is always
present; `next` arrives in wave 3:

```json
{"schema_version": 2, "command": "worktree locate", "status": "ok",
 "data": {"path": "/r/.worktrees/fix-x"},
 "warnings": [{"code": "NAME_CASE_FOLDED", "message": "branch Fix-X matched fix-x",
               "subject": "Fix-X", "repair": "use fix-x"}],
 "next": [{"argv": ["provenant", "pr", "status", "12"], "why": "check CI"}],
 "error": null}
```

**Modules and owners.**
- `records::warning` owns the type, the code catalogue and the facade.
- `src/output.rs` (new) owns the edge sink. It takes over `emit`,
  `emit_summarised`, `emit_with_writers` and `emit_unpublished` from
  `src/lib.rs`. The registry's `run()` installs one sink per command.
- In human mode the sink prints `warning: <message>` and then
  `  repair: <repair>` to stderr as each warning arrives, collapsing
  repeats. In `--json` mode nothing goes to stderr; every warning goes into
  the envelope.
- Long-running owners (`tick`, the daemon) install a sink per cycle and
  write through the dedup table.
- `Effect::Warn(String)` becomes `Effect::Warn(Warning)`, and
  `Policy::warn_or` calls `warn`. The roughly 35 match arms that format
  their own message are replaced by one renderer.
- The ad hoc `data.warnings` arrays in about 12 command payloads are
  replaced by `warn`. If the console app reads them, its decoding moves to
  the envelope field in the same slice, with fixtures regenerated by
  `generate-swift-fixtures`.

**Persisted formats.**
- `<state>/warnings.sqlite3` holds one STRICT table,
  `seen(key TEXT PRIMARY KEY, code TEXT NOT NULL, first_seen INTEGER, last_seen INTEGER, count INTEGER)`,
  set up through `provenant_sqlite::ensure_schema`.
- Each write prunes rows whose `last_seen` is older than
  *warnings.dedup_retention_days*. That is a sourced setting; the default is 30.
- Migration step *retire-warning-markers* deletes every
  `config-warning-*` and `root-warning-*` file in the state root, and
  reports how many it removed.

**Deleted.**
- The marker writers in `crates/config/src/loader.rs` and `src/roots.rs`.
- Each local `warning:` helper (at least 10) and the per-site
  `writeln!(io::stderr()…)` warning lines.
- Every `data.warnings` field.
- The string payload of `Effect::Warn`.

**Oracle.**
- The compiler: `Effect::Warn(Warning)` breaks every arm that still
  formats its own message.
- A lint: `clippy.toml` lists `std::io::stderr` under
  `disallowed-methods`, and only the emitter carries the allow. The chair's
  clippy run is therefore the sweep check.
- Characterisation tests:
  - a command that warns returns its warnings in the envelope and prints
    nothing to stderr under `--json`;
  - a `PerChange` warning shows once across two processes and again after
    its key changes;
  - pruning removes only stale rows.

## Seam 2: one sourced-settings vocabulary

Area spec: [State](state.md#configuration), with a new section, *Every
setting reports its layer*. Issue: #3777.

**Contract.**
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

Each family declares its precedence as a const beside its owner. The
precedence keeps today's order exactly; this seam unifies vocabulary and
merge, not semantics:

| Family (today) | Precedence, lowest first | Wire change |
|---|---|---|
| `LimitSource` | Default, Host, Project, Tier, Template, Launch | none |
| `PortSource` | Default, Detected, Host, HostProject, Project | `builtin` becomes `default` |
| `AdmissionSource` | Default, Host, HostProject, Project | none |
| `OverlaySource` | Default, Host, Project | `bundled` becomes `default` (not persisted) |
| `KnobSource`, schema `Layer` | Default, Profile, Host | `override` and `user` become `host` (not persisted) |
| `SettingSource` | Default, Host | `configuration` and `shipped_default` change |
| `LaneAccessSource` | Default, Host, Project, Tier, Template, Route, Launch | `route_policy` becomes `route` |
| `SandboxSourceReceipt` | Default, Host, Route, Parent | `knob` becomes a layer. Platform and restriction are not layers: they move to a `constraint` field. |
| `WeightLayer`, template `Layer`, `LandSource`, the stringly `Source.layer` in `src/pr/landing/policy.rs` | as declared today | in-memory only |

`OriginReceipt` (which config file was used) and `records::custody::Origin`
are provenance of a different kind and stay as they are.

The precedence drift: `LimitSource` ranks Template above Tier, the nearest
to the work. `docs/ARCHITECTURE.md` lists them the other way round, and the
`w1-provenance` slice corrects the document.

**Persisted formats and migration.**
- Before changing a spelling, each family slice makes an inventory of the
  records that carry the family.
- Records that no digest seals get a migration step that rewrites the
  changed strings, and the record's schema version is bumped.
- If a digest seals a record, the slice keeps that family's current
  spellings for the persisted variants, by naming the `Layer` variants to
  match, and records the exception here. Re-sealing evidence is out of scope.
- `LIVE_RECORD_FORMAT` goes up by one.

**The host-project directory key.** This is a likely bug fixed in this seam.
`host_project_dir` keys `projects/<ProjectId>`, while
`crates/config/src/overlays.rs` and `crates/config/src/admission.rs` key
`projects/<sha256 of the path>`. The `w1-project-dirs` slice:
1. confirms which one install and binding write;
2. makes `project_binding::host_project_dir` the only resolver;
3. adds a migration step that moves directories from the other key.

**Deleted.**
- The family enums listed above, with their `as_str` and `description` impls.
- The per-family merge loops.
- The stringly `Source.layer`.

**Oracle.**
- The compiler: each enum is deleted, so every user breaks.
- Characterisation tests, one per family, written before the change: the
  winner and the shadowed layers for a fixture with every layer set.
- A round-trip test for each migrated record.

**Follow-ons in this batch.**
- `config show` and `--explain` print each leaf's source.
- `TierCommand` accepts a plain string (a quick win).
- Read source, brief preamble, steer-on-settled and approval carry become
  sourced settings.

**Not in this batch:** a project layer for knobs. The knob registry is
host-only by design today, and widening it is a product decision for the
owner.

## Seam 3: one name and close-match resolver

Area spec: [CLI](cli.md#targets), with a new section, *Every name refusal
lists close matches*. Issues: #3808, #3506 (accounts), parts of #3809.

**Contract.**
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

**Rules.**
- A case-folded or alias match proceeds and warns with *NAME_CASE_FOLDED*
  or *NAME_ALIAS_USED*.
- A miss refuses with the domain's existing typed code (ADR 0034). It adds
  `candidates` to the error data and names them in the message.
- Candidates come only from catalogue names. The redaction guards
  `bare_model_detail` and `clap_error` stay.
- Name syntax: a `--name` with capitals folds and warns instead of refusing.
  Validity has one owner, `core::names::valid_name`, which replaces both
  `graph_name_fault` and `Target::parse::check_name`.
- Aliases come from the routing catalogue's route aliases and from clap
  `visible_alias` for subcommands. There is no second alias table.
- One account resolver, `providers::accounts::resolve`, replaces
  `accounts.rs resolve/resolve_typed`, `route/resolution.rs resolve_account_reference`
  and `preparation.rs select_account/select_retained_account`. It returns a
  typed identity and refuses through `NameRefusal`.

**Deleted.**
- The four distance functions: `core::edit_distance` (moved),
  `crates/fabric-runtime/src/graph/template.rs edit_distance`,
  `crates/config/src/knobs.rs distance` and
  `crates/routing/src/catalog.rs`'s ranker.
- The per-site cutoffs.
- The two name-syntax checks.
- The three account resolvers.

**Oracle.**
- `rg -n "fn (edit_distance|distance|levenshtein)"` finds one definition.
- An invariant test per catalogue: a one-edit typo of every name ranks that
  name first, and an unrelated string returns no candidates.
- A resolver test: case-folded input proceeds with the warning.

## Seam 4: command-domain ownership

Area spec: [Rust workspace](rust-workspace.md#crate-layering), with a new
section, *One owner per command domain*. Issue: #3774 (registry half). The
moves (#3507, #3776, #3486) come next batch.

**Contract.**
```rust
// src/commands/mod.rs  (owner: the binary; one module per domain under src/commands/)
pub struct Domain { pub name: &'static str, pub verbs: &'static [Verb] }
pub struct Verb {
    pub path: &'static [&'static str],      // ["worktree", "locate"]; the persisted usage verb is path.join(" ")
    pub stability: Stability,               // external_contract reads this
    pub run: fn(&Invocation) -> Result<Outcome, CommandError>,
}
pub struct Invocation<'a> { pub command: &'a cli::Command, pub globals: &'a cli::Globals }
pub static DOMAINS: &[&Domain] = &[&ci::DOMAIN, &cloud::DOMAIN, /* one per domain */];
// run() parses, finds the Verb by path, installs the output context, calls it, emits.
// Grammar stays in crates/cli/src/grammar.rs for this batch.
```

**What it buys.**
- New verbs in wave 3, such as *worktree locate* and *init*, register in
  their own domain module instead of editing an 800-line match.
- `usage_verbs()` (persisted usage verbs) and `external_contract` derive
  from `DOMAINS`, replacing their hand-kept string lists.

The persisted verb strings must not change. The slice snapshots today's
`usage_verbs()` output as a characterisation test, because that string set
is a persisted contract, not an implementation detail.

**Ratchet, warn-first.** `tests/workspace_policy.rs` gains two rules:
1. Every grammar leaf maps to exactly one `Verb`, and every `Verb` to a leaf.
2. A production file may not grow past its recorded line ceiling, and a new
   file is capped at 2,000 lines. The ceiling table lives in the test as an
   overridable default and is edited in the same PR that justifies it.

**Deleted.**
- The `run()` match arms in `src/lib.rs`.
- The duplicated verb string lists.

**Oracle.**
- The compiler.
- The workspace_policy mapping test.
- The usage-verb snapshot.

## Seam 5: the worktree and landing loop a chair actually uses

Area specs:
- [Supervisor](supervisor.md) for worktrees;
- [CLI](cli.md#landing-adds-no-merge-authority) for landing;
- [CI](ci.md) for observation.

Issues: #3288, #3505, #3810, #3500, #3644, #3649, #3510, #3191, #3285, #3768.

The loop a chair runs, end to end, with no raw `git worktree` and no
`gh pr merge`:
1. *provenant worktree locate* finds the checkout for a branch, lane,
   attempt or pull request (close matches on a miss).
2. `provenant run implement --pr N` repairs a pull request from its CI
   failures. An existing branch behind its base warns and proceeds;
   `--base` is the merge target, never a reset.
3. Unheld uncommitted work is adopted with a WIP commit that the warning
   names. Work that another lane holds is refused (boundary 2).
4. `provenant land` gates the exact candidate. Where the owner granted it,
   it publishes with admin authority, recorded in the receipt.
5. `provenant worktree remove` records the removed tip and prunes the merged
   local branch. It refuses only dirty or live-held trees (boundary 2).

**Contract.**
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

- **The remote declaration.** It is a port-table entry, *git.remote*, and
  *git.protected*, both `Sourced`. Every `"origin"`, `"main"`, `"master"`
  and `refs/heads/main` literal in production code moves to `ForgeContext`.
  Every forge construction that injects `Policy::default()` takes
  `ForgeContext.policy` instead (#3505). Default-branch discovery exists once.
- **`WorktreeInfo`.** It gains `lane`, `attempt`, `base` and `writer` from
  `provenant-owner.json`. `status --json` rows gain `worktree: {path, base}`.
- **Removed tips.** *worktree remove* appends `{branch, tip, path, at}` to
  `<state>/worktrees/removed.jsonl`, a new append-only record
  (*provenant.worktree-removed.v1*), and prints it.
- **Behind its base.** `WorktreeError::BranchExistsWithBase` is deleted. A
  branch behind its base emits *BRANCH_BEHIND_BASE* and proceeds.
  `WRITER_SCOPE` then no longer stands in for that cause.
- **Unheld dirty trees.** `lanes.writer_dirty` keeps its values; the default
  for unheld trees becomes `wip_commit`, and held trees still refuse.
- **One CI reader (#3510).** `src/pr/observer.rs` becomes it.
  `src/pr/landing/gate.rs await_pr_ci`, the PR path of `src/status_wait.rs`,
  `src/ci.rs` and `src/fabric/review_subject.rs` call it. Its retry and
  backoff values become one sourced family.
- **Admin publication.**
  - *domains.pr.land.admin_publication* takes `never` (the default) or
    `when_review_unavailable`.
  - It is honoured only from the `Host` layer. A value at any other layer
    warns with its source and is ignored.
  - With `when_review_unavailable`, a 405 caused by a review requirement
    the owner cannot give is retried with admin authority. `land`'s own gate
    must still have passed: CI green on the exact candidate, plus
    independent review evidence.
  - The receipt records `publication: {mode, source}`. The runbook's
    `--admin` paragraph then points to this setting.
  - This changes how merge authority is exercised, not who holds it.
    **The owner accepts this point before `w3-land` launches.**
- **#3191.** The batch target is derived from the members when `--target`
  is absent (and recorded with its source). `--head N=SHA` pins each
  member's reviewed head.
- **#3285.** The worktree-layout port accepts absolute and parent-relative
  roots, and `config ports` shows each root's source.
- **#3768.** gc protects the default branch across remote refs and linked
  worktrees. It is a quick win now; `w2-remote-literals` then moves it onto
  `ForgeContext.protected`.

**Deleted.**
- The hard-coded remote and branch literals.
- The three default-branch discoveries.
- `BranchExistsWithBase`.
- The `sleep` loop in `await_pr_ci`.
- The duplicated retry constants in the observer.
- Raw `git worktree` calls outside `crates/confinement/src/worktrees`.

**Oracle.**
- The compiler.
- `rg '"origin"'` over production code returns nothing.
- Characterisation tests:
  - a fake repository whose only remote is `upstream`;
  - a 405 review-requirement fake forge, with and without the host grant,
    and with a project-layer grant that is ignored with a warning;
  - a dirty unheld tree is adopted, and a held one is refused.

## Seam 6: adapter usage accounting

Area spec: [Telemetry](telemetry.md), with a new section, *Usage merges by
declared semantics*. Issues: #3709 (and the OpenCode set #3706-#3708, #3710).

**Contract.**
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

**Persisted formats.**
- `UsageRecord` (`crates/records/src/run.rs`) holds `tokens: Tokens`,
  `cost: Option<Cost>` and `source: UsageSource`, replacing the free
  string.
- The usage ledger goes from `provenant.usage-ledger.v3` to *v4*. Migration
  step *usage-ledger-v4* rewrites each line: buckets carry over, and source
  strings map through an exhaustive table. A source it cannot map is
  reported, not guessed, and the line is kept as is under a `.v3-unmapped`
  sidecar for the owner.
- The Claude-only ingest store writes through the same accumulator with
  `UsageSource::Ingest`.
- Usage stays separate from quota evidence. OpenCode quota still comes from
  its own database.

**Deleted.**
- The last-wins consumer in `crates/cli/src/run.rs`.
- The provider-name check in `crates/run/src/token_budget.rs enforces`,
  which reads `AdapterSpec.usage.live` instead.
- The non-optional `input` and `output` on the stream event.

**Oracle.**
- The compiler: the `ProviderEvent::Usage` shape changes.
- An accumulator invariant test: keyed reports are idempotent, keyless
  reports add, cumulative reports replace, and unstated buckets stay
  `None`.
- One recorded stream fixture per adapter totals correctly.

## Shared: persisted-state migration

Area spec: [State](state.md#records), with a new section, *Format changes
migrate once at install*.

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

Steps in this batch:
1. *retire-warning-markers*
2. *port-source-spellings* and any other family the inventory finds
3. *host-project-dirs*
4. *usage-ledger-v4*
5. *pre-rename-boundary-records*

**Step 5 and #3815.** Done by #3822 before this batch: the retired `Boundary`
aliases are deleted and run stores at base 12 are rewritten once on writable
open (base 13). No step 5 remains; the list keeps its number for reference.

## Dependents: output and adoption

- **#3444 and #3807, result projection.**
  - One producer in `crates/cli/src/run/output.rs` builds `ResultProjection`
    (`crates/records/src/lane_contract.rs`) from observed state: host facts
    first, lane claims second.
  - `output`, `result` and `status --full` render that projection.
  - The default view leads with verdict, findings and verify. `--full` adds
    the rest, and `--tail` counts bytes.
  - If any part of the projection stays unwired, it is deleted.
- **#3814, next steps.**
  - Next-step hints become envelope `next[]` data, rendered by one formatter.
  - That formatter grows from `CommandPrefix` in
    `crates/workflow/src/session/state.rs` and omits `--state-dir` when it
    equals the caller's default resolution.
  - The five independent formatters are deleted.
  - #3817 owns hook delivery. This slice changes only the text the hooks
    print.
- **#3814, init.** *provenant init* detects the remote and default branch
  through `ForgeContext` and the toolchain through port detection. It writes
  a minimal `.provenant/ports.json` and prints each choice with its source.
- **#3498 and #3652, brief preamble.** *lanes.brief_preamble* is a layered
  file knob (Host, Project, Launch) with role filters. It includes the
  project's no-build static checks line.
- **#3512 and #3360, read source.**
  - *lanes.read_source* takes `checkout`, `default_branch` or a ref, and can
    fetch first.
  - The commit read shows in `status` and `result`.
  - The plan node binds to `--base` through the same resolver.
- **#3809, steering a settled lane.** *lanes.steer_settled* takes `queue`
  (the default; the follow-up is named) or `warn`.

## Issue map

| Issue | Slice | Notes |
|---|---|---|
| #3778 | w1-interfaces, w1-envelope, w2-warn-* | closes with the batch |
| #3777 | w1-interfaces, w1-provenance, w1-project-dirs, w2-sources-*, w3-config-show | closes |
| #3808 | w1-names, w3-names-targets, QW help-and-grammar | closes |
| #3506 | w3-accounts | closes |
| #3774 | w1-registry | registry half; rescope the issue to the moves |
| #3288, #3505 | w1-forge, w2-remote-literals | close |
| #3810 | w3-worktree | closes |
| #3500, #3644 | w3-implement-pr | close |
| #3649 | w3-writer-dirty | closes |
| #3510 | w3-ci-observer | closes |
| #3191 | w3-land | closes |
| #3285 | w3-worktree-roots | closes |
| #3540 | w3-approval-carry | closes |
| #3709, #3706, #3708 | w1-usage, w3-opencode-stream | close |
| #3707 | w3-opencode-variants | closes |
| #3444, #3807 | w3-result-projection, QW output-null-headers | close |
| #3814 | w3-next-steps, w3-init, QW help-and-grammar, QW docs | closes |
| #3498, #3652 | w3-preamble | close |
| #3512, #3360 | w3-read-source | close |
| #3809 | QW output-running-partial, w3-steer-settled | closes |
| #3813 | w1-envelope, QW install-upkeep | closes |
| #3812 | QW tier-command-string, QW install-upkeep, w1-envelope | closes |
| #3407 | w1-writer-settlement | closes |
| #3801 follow-up | w1-record-migration | references #3801 |
| #3811, #3799, #3710, #3405, #3192, #3502, #3768, #3764, #3784 | quick wins | see the quick-wins page |
| #3551, #3779, #3251 | umbrellas | each slice that converts a refusal or prunes a pin cites them; they stay open |

## Excluded

| Issue | Reason |
|---|---|
| #3775 reclaim engine | Its own epic. gc is fragile, and each of its five owners needs its own characterisation test. It would conflict with the install and gc slices here. Next batch. |
| #3507, #3776, #3486 | File splits and test moves conflict with nearly every slice. Run them as codemod lanes right after this batch lands, using the registry. |
| #3286 | Needs a peer subscription capability in the peer protocol: a design of its own. |
| #3600 | Needs diagnosis first. After v2.8.2, run one read-only diagnosis lane on the upgrade logs. |
| #3592 | An investigation. Recheck after #3815: digest failures may share #3801's decode cause. |
| #3670 | Depends on what #3815 leaves. Lane Cargo caches are gone, so rescope it to package caches only. |
| #3788 | Swift fixtures, a separate toolchain. Envelope v2 may regenerate them anyway (w1-envelope). |
| #3425, #3348, #2225, #3447 | Measurement and evaluation, not seam work. |
| #3445, #3452, #3453, #3427-#3432 | Epics and research direction. Keep them as parents. |
| #3803, #3805, #3806, #3801, #3802 | In flight. This design assumes their outcomes: hooks deliver on PostToolUse, one menu-bar path, history refusals are a sourced setting, and attempt records decode. |

## Stale issues: close or keep

- **#3764: keep.** PR #3786's body does not cite F1-F3, so slice 2 did not
  close it. It becomes a quick win.
- **#3804 main is red: keep.** CI failed on `33eaa9cfe5`, the current `main`
  (22 Nov, 22:28Z). It gates this batch.
- **#3526: trace, then most likely close.** No live writer of the
  worktree-git containers was found since the candidate-scoped Git layout.
  Spend one read-only trace, then close as obsolete with the evidence.
- **#3774: keep, but rescope.** The registry lands here; the domain moves
  remain.
- **#3540: keep.** Its first bullet is already in #3502.
- **#3551, #3779, #3251: keep as umbrellas.** Update their bodies with this
  map when the batch lands.
- **#3505, #3288: close with w1-forge and w2-remote-literals.**

## Build plan

Three waves of `batch` documents share one integration branch,
`batch/next`:
- Wave 1 builds the seam cores.
- Wave 2 finishes the seams by moving every caller.
- Wave 3 holds the dependents.

Within a wave, `after` orders every pair of slices whose write scopes share a
file. The rest run in parallel with disjoint write scopes.

Lanes run `cargo fmt` only. Between waves the chair runs one integrated
clippy check on the integration head, then pushes it so CI tests run while
the next wave builds.

There is one gate per wave rather than one at the end. Lanes never compile,
and a broken base would multiply errors in every dependent slice. Each gate
costs one warm clippy run.

### Wave 1: seam cores (9 slices, about 5500 lines)
| Slice | Issues | Model | Lines | After | Outcome |
|---|---|---|---|---|---|
| w1-interfaces | #3777 #3778 #3808 #3709 | Hy | 550 | none | Shared seam types and the migration registry |
| w1-registry | #3774 | Hy | 800 | none | Command registry replaces the dispatch match |
| w1-envelope | #3778 #3813 #3812 | Hy | 650 | w1-interfaces, w1-registry | One warning emitter and envelope v2 |
| w1-provenance | #3777 | Hy | 700 | w1-interfaces | Persisted source families onto Layer |
| ~~w1-record-migration~~ | #3801 follow-up | - | - | - | Done by #3822 (aliases deleted; base-12 stores rewritten on open); dropped from wave 1 |
| w1-names | #3808 | Hy | 500 | w1-interfaces | One close-match ranker and name refusal |
| w1-forge | #3288 #3505 | Hy | 750 | w1-interfaces | ForgeContext: one remote, default branch, protected set and policy |
| w1-usage | #3709 (seam half) | Hy | 750 | w1-interfaces | UsageReport, declared semantics and one accumulator |
| w1-project-dirs | #3777 (bug) | Hy | 350 | w1-interfaces | One host-project directory key |
| w1-writer-settlement | #3407 | Hy | 450 | none | Writer settlement decides Git-operation state before publication |

### Wave 2: move every caller (7 slices, about 3500 lines)
| Slice | Issues | Model | Lines | After | Outcome |
|---|---|---|---|---|---|
| w2-warn-lib-a | #3778 | Hy | 500 | none | Warnings codemod: config, records, core, routing, quota, providers, os, sqlite, telemetry |
| w2-warn-lib-b | #3778 | Hy | 500 | none | Warnings codemod: run, confinement, supervisor, fabric-runtime, workflow and the remaining library crates |
| w2-warn-cli | #3778 | Hy | 500 | none | Warnings codemod: crates/cli |
| w2-warn-src | #3778 | Hy | 600 | none | Warnings codemod: the binary under src/ |
| w2-sources-config | #3777 | Hy | 450 | w2-warn-lib-a, w2-warn-cli, w2-warn-src | Config source families onto Layer |
| w2-sources-other | #3777 | Hy | 450 | w2-warn-lib-a, w2-warn-lib-b, w2-warn-src | Run, routing, fabric and landing source families onto Layer |
| w2-remote-literals | #3288 | Hy | 500 | w2-warn-lib-b, w2-warn-src | Replace compiled-in origin, main and master with ForgeContext |

### Wave 3: dependents (18 slices, about 9350 lines)
| Slice | Issues | Model | Lines | After | Outcome |
|---|---|---|---|---|---|
| w3-next-steps | #3814 | Hy | 450 | none | Next steps are envelope data with one formatter |
| w3-names-targets | #3808 | Hy | 550 | none | Close matches for graph, node, attempt, host, adapter, exclusion and help targets |
| w3-accounts | #3506 | Hy | 650 | none | One account reference resolver |
| w3-worktree | #3810 | Hy | 650 | none | worktree locate, carried paths and a remove that chairs can use |
| w3-ci-observer | #3510 | Hy | 600 | none | One CI observation transport |
| w3-implement-pr | #3500 #3644 | Hy | 700 | w3-ci-observer | run implement --pr N; an existing branch behind its base warns |
| w3-writer-dirty | #3649 | Hy | 350 | none | Adopt unheld uncommitted work by default |
| w3-land | #3191 + admin publication | Hy | 700 | w3-ci-observer | land: owner-granted admin publication, per-member heads, derived target |
| w3-approval-carry | #3540 | Hy | 350 | w3-land | Approval carry reads the format command from the port table |
| w3-opencode-stream | #3706 #3708 #3709 | Hy | 550 | none | OpenCode stream parity: step usage and cost, retry, 5xx |
| w3-opencode-variants | #3707 | Hy | 450 | w3-opencode-stream | OpenCode variants: classify and validate effort |
| w3-result-projection | #3444 #3807 | Hy | 700 | none | One result projection: facts first, verdict-first default |
| w3-init | #3814 | Hy | 600 | none | provenant init |
| w3-preamble | #3498 #3652 | Hy | 450 | none | Layered brief preamble with static checks |
| w3-read-source | #3512 #3360 | Hy | 600 | w3-implement-pr | Read-only source as a sourced setting; plan binds --base |
| w3-steer-settled | #3809 | Hy | 300 | none | Steer on a settled lane queues or warns, by setting |
| w3-worktree-roots | #3285 | Hy | 400 | w3-worktree | Worktree layout roots outside the checkout |
| w3-config-show | #3777 | Hy | 300 | none | config show and --explain print per-leaf sources |

Models: every slice and repair lane writes with Hy 4 Flash free
(`opencode/opencode/hy4-flash-free`, the free Zen route). Lanes run format
only, so the chair's integrated compile check is the oracle for each merge;
wave 1 showed that a wide rename spills past its write scope and the chair
repairs it before the next slice starts. DeepSeek stays available for a slice
that fails twice on Hy.

### Chair steps

1. **Preconditions.** `main` is green. #3815, #3816 and #3817 are merged and
   the quick wins have landed. The owner has accepted the admin publication
   point. Then run `git fetch`.
2. **The repair checkout.** The `batch` composite's review-repair stage
   needs a clean checkout on its own branch. Create it with
   `provenant worktree create batch/next-repair --base main`.
   `with.worktree` in each wave document names that path.
3. **Each wave.** From the root of the checkout that holds these files
   (`brief_file` paths resolve against the working directory), run
   `provenant run --file docs/specs/next-batch/wave-1.toml --explain`. Then
   launch it without `--explain` and block with `provenant wait` in the
   background.
4. **The gate.** Run the integrated clippy check on the integration head and
   push `batch/next`; open a draft PR after wave 1 so CI runs. Clippy errors
   go to a Hy repair lane on the integration branch. CI failures feed
   the next wave as repair slices, or the final repair round.
5. **Waves 2 and 3.** These adopt the existing branch, so they start from
   its tip. Before launching each one, set `review_base` in its document to
   the previous wave's tip, so the built-in findings panel reviews only that
   wave.
6. **After wave 3.** Run the review plan below, repair, wait for CI green on
   the exact head, then `provenant land`.

The built-in `findings-panel` in each `batch` run is a cheap per-wave
correctness pass on host-selected non-Claude lenses. The panel below is the
acceptance review.

## Review plan (after integration)

The subject is `origin/main...batch/next` at the exact head. Each reviewer
gets this page, its slices' briefs and the diff for its area. No writer
reviews its own slice. The panel has four lenses, chosen by risk.

| Lens | Reviewer | Scope |
|---|---|---|
| Boundaries and design coherence | Opus (chair-run native subagent) | The three boundaries: land admin publication (host-layer only, gate still required), `writer_dirty` adoption, *worktree remove*, settlement order (#3407), migrations that delete files. Each seam honours its contract; no shim, alias, fallback or second owner remains. |
| Migrations and persisted formats | Sonnet | Every migration step is idempotent, ordered and partial-failure safe. `LIVE_RECORD_FORMAT`, envelope v2, usage ledger v4, the dedup table, the host-project move and the removed-tips record. Live attempts across an upgrade. |
| Correctness by area | Sonnet x3 | (a) warnings, names, output, init; (b) settings, forge, worktree, landing, CI; (c) usage, OpenCode, run, writer. Tests pin invariants, not prose or counts. |
| Mechanical sweep | Haiku x2 | Residue searches: `"origin"` literals, `Policy::default()` at forge sites, distance functions, `io::stderr` outside the emitter, deleted symbols. One release-note fragment per slice. Area spec sections added and this page's invariants folded in. `scripts/check-doc-truth --binary` is clean. |

Findings go to Hy repair lanes. The same lens then re-reviews only the
changed hunks, with a fresh agent. The batch is accepted when CI is green on
the exact head and no finding of high severity is open.

## Risks

- **Conflicts.** The hotspots are `src/lib.rs`, `src/fabric/interface.rs`,
  `crates/config/src/ports.rs` and `src/pr/landing/`. `after` chains
  serialise the known overlaps. A conflict blocks only that slice, and
  codemod slices are cheap to rerun on the new tip.
- **Uncompiled bases.** The per-wave clippy gate and one repair lane are the
  mitigation. Hy codemods may sweep partially; the lint and the
  compiler-breaking type changes catch most of it, and the Haiku sweep
  catches the rest.
- **Envelope v2.** It changes the JSON contract, which is Evolving. The
  console app and fixtures must move in the same slice. The release note
  calls it out.
- **Migrations run at install.** A bad step touches every host. The steps
  are idempotent and recorded only on success. The real launches after
  release are the proof, on both hosts.
- **Admin publication.** It changes how merge authority is exercised and
  needs the owner's acceptance first.
- **Size.** About 19k changed lines in one PR. The area-split review and
  per-wave CI keep it reviewable. If review load is too high, land after
  wave 2 as its own PR.
