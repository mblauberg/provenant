# Slice w1-forge: ForgeContext: one remote, default branch, protected set and policy

Issues: #3288 #3505. Wave 1 of the next batch; integration branch `batch/next`. Expected size about 750 changed lines. Starts after w1-interfaces have integrated.

## Outcome

Add `crates/config/src/forge.rs` with `ForgeContext` and make it the one source of remote, default branch, protected branches and operation policy.
- Declarations: port-table entries *git.remote* and *git.protected* (project, host-project and host layers, through the port table's existing layered decode).
- Detection, in order: declared; the current branch's upstream remote; the sole remote; a refusal listing the remotes seen. Default branch: declared; `refs/remotes/<remote>/HEAD`; the forge API; else a refusal. Protected: declared, else the default branch plus forge-reported protected branches.
- Replace the three default-branch discoveries with `ForgeContext::resolve`.
- Pass `ForgeContext.policy` through every forge construction that injects `Policy::default()` today (#3505): `src/pr.rs`, `src/tracker.rs`, `src/pr/open.rs`, `src/retention_owner.rs`, `src/pr/status/batch.rs`, `src/pr/landing/adapters.rs`, `src/pr/observer/port.rs`, `src/install.rs`, `crates/run/src/delivery.rs`, `crates/cli/src/run.rs`, `src/fabric/owner.rs`, `src/fabric/interface.rs`; delete each `TODO(#3371 seam)`. Test-only uses may stay.
The literal sweep of `"origin"`, `"main"` and `"master"` is wave 2's `w2-remote-literals`; do not do it here beyond the files above.

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

- `tools/front-door/crates/config/src/forge.rs`
- `tools/front-door/crates/config/src/ports.rs`
- `tools/front-door/src/pr.rs`
- `tools/front-door/src/tracker.rs`
- `tools/front-door/src/pr/open.rs`
- `tools/front-door/src/retention_owner.rs`
- `tools/front-door/src/pr/status/batch.rs`
- `tools/front-door/src/pr/landing/adapters.rs`
- `tools/front-door/src/pr/observer/port.rs`
- `tools/front-door/src/install.rs`
- `tools/front-door/crates/run/src/delivery.rs`
- `tools/front-door/crates/cli/src/run.rs`
- `tools/front-door/src/fabric/owner.rs`
- `tools/front-door/src/fabric/interface.rs`

## Tests

Characterisation fixtures: a repository whose only remote is `upstream`; one with two remotes and an upstream-tracking branch; one with no `HEAD` symref (forge fallback via a fake forge); a declared value overriding detection with its source shown. A configured policy override reaches a forge operation (fake forge records the policy it got).

## Docs

Add the remote declaration to `docs/runbooks/configuration.md` (port table) and an invariant to `docs/specs/run-git.md`: one resolved forge context per command.

## Release note

Add `docs/release-notes/next.d/next-batch-w1-forge.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Added: declare the Git remote and protected branches in the port table; detection follows the branch upstream instead of assuming `origin`; configured forge policy now reaches every forge call.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
