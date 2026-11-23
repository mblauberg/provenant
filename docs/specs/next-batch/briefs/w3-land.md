# Slice w3-land: land: owner-granted admin publication, per-member heads, derived target

Issues: #3191 + admin publication. Wave 3 of the next batch; integration branch `batch/next`. Expected size about 700 changed lines. Starts after w3-ci-observer have integrated.

## Outcome

**Owner gate: launch only after the owner accepts admin publication.**
(Admin) Add *domains.pr.land.admin_publication* (`never` default, `when_review_unavailable`), honoured only from the Host layer; any other layer warns *SETTING_LAYER_IGNORED* with its source and is ignored. With `when_review_unavailable`, a 405 whose cause is a review requirement the authenticated owner cannot satisfy is retried with admin authority (the same API `gh pr merge --admin` uses), only after land's own gate passed (green CI on the exact candidate, independent review evidence). The receipt records `publication: {mode, source}`. Never bypass for red CI, a missing review receipt or another cause of 405.
(#3191) Derive the batch target from the members when `--target` is absent and record it with its source in the receipt and `--explain`; members on differing bases are excluded with both bases named. `--head N=SHA` pins each member's reviewed head; the single `--head SHA` form stays.
Update the `--admin` paragraph in `docs/runbooks/github-workflow.md#merge` to point to this setting.

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

## Files (write scope)

- `tools/front-door/src/pr/landing/`
- `tools/front-door/crates/fabric-runtime/src/config.rs`
- `docs/runbooks/github-workflow.md`

## Tests

Fake forge: 405 review-requirement with and without the host grant; a project-layer grant ignored with the warning; red CI never bypasses; batch target derivation and per-member head pins.

## Docs

Amend *Landing adds no merge authority* in `docs/specs/cli.md`: admin publication exercises the owner's own authority, granted only in the host layer, after the land gate.

## Release note

Add `docs/release-notes/next.d/next-batch-w3-land.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Added: `provenant land` can publish with the owner's admin authority when a ruleset demands a review the owner cannot give, if the host config grants it; batches derive their target and pin a reviewed head per member.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
