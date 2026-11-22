# Slice w1-provenance: Persisted source families onto Layer

Issues: #3777. Wave 1 of the next batch; integration branch `batch/next`. Expected size about 700 changed lines. Starts after w1-interfaces have integrated.

## Outcome

Move the persisted source families onto `records::provenance::Layer` and `Sourced<T>`, with each family's `Precedence` declared as a const beside its owner, preserving today's order exactly:
- `LimitSource` (Default, Host, Project, Tier, Template, Launch; wire unchanged).
- `PortSource` (Default, Detected, Host, HostProject, Project; `builtin` becomes `default`) and `PortCommandReceipt`.
- `SettingSource` (Default, Host).
- `LaneAccessSource` (Default, Host, Project, Tier, Template, Route, Launch; `route_policy` becomes `route`).
- `SandboxSourceReceipt` (Default, Host, Route, Parent): `knob` maps to the knob's actual layer, and platform and restriction move to a new `constraint` field, because they explain a cap, not where a value came from.
First inventory every persisted record carrying these types (attempt rows, receipts, ledgers). For records no digest seals, add migration step *port-source-spellings* (and siblings) that rewrites the changed strings, and bump the record's schema version. Where a digest seals a record, keep that variant's existing spelling by naming the Layer variant to match, and say so in the handoff. Bump `LIVE_RECORD_FORMAT` by one. Fix `docs/ARCHITECTURE.md` so its precedence listing matches `LimitSource` (Template above Tier).

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

- `tools/front-door/crates/records/src/limit.rs`
- `tools/front-door/crates/records/src/ports.rs`
- `tools/front-door/crates/records/src/run.rs`
- `tools/front-door/crates/records/src/diagnostic.rs`
- `tools/front-door/crates/config/src/ports.rs`
- `tools/front-door/src/install/migrations.rs`
- `tools/front-door/crates/records/src/install.rs`
- `docs/ARCHITECTURE.md`

## Tests

Write a characterisation test per family first: a fixture with every layer set reports the same winner and shadowed layers before and after. Add a migration round-trip test per rewritten record, and an idempotence test (running a step twice changes nothing the second time).

## Docs

Fill the *Every setting reports its layer* section in `docs/specs/state.md` with the per-family rule (precedence is declared once per family).

## Release note

Add `docs/release-notes/next.d/next-batch-w1-provenance.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Changed: limit, port, lane-access and sandbox receipts report their source with one shared layer vocabulary; upgrade rewrites stored spellings once.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
