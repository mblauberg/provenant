# Slice w3-init: provenant init

Issues: #3814. Wave 3 of the next batch; integration branch `batch/next`. Expected size about 600 changed lines.

## Outcome

(#3814) New verb *provenant init* (`src/commands/init.rs`, registered in `DOMAINS`): detect remote and default branch through `ForgeContext` and the toolchain through port-table stack detection, write a minimal `.provenant/ports.json` (only what differs from detection), and print each choice with its source. An existing `ports.json` is left untouched and the command prints what it would add (warn and proceed, never overwrite).

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

## Files (write scope)

- `tools/front-door/src/commands/init.rs`
- `tools/front-door/src/commands/mod.rs`
- `tools/front-door/crates/cli/src/grammar.rs`
- `tools/front-door/crates/config/src/ports.rs`
- `docs/runbooks/install.md`

## Tests

Fixture repository: init writes the minimal file and reports sources; a second run with the file present changes nothing and says so.

## Docs

Add init to the quickstart and `docs/runbooks/install.md` first-install section.

## Release note

Add `docs/release-notes/next.d/next-batch-w3-init.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Added: `provenant init` sets up a repository's port table from detected remote, default branch and toolchain.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
