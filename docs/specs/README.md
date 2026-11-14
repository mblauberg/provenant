# Specifications

Owner: the chairs, per [`MAINTAINING.md`](../../MAINTAINING.md#specifications)
Anchor: [`MAINTAINING.md#Specifications`](../../MAINTAINING.md#specifications)

This directory holds Provenant's durable requirements: what must hold, what
must never happen, and why. Code and tests own what exists and how it behaves,
GitHub issues own delivery state, and the [runbooks](../runbooks/) own
procedure.

Each spec is an invariants page. An entry states one invariant in a few
sentences, then gives a one-sentence `Why`, a `Test` citation and an `Owner`.
Narration, enumerations, history, flags, schemas, defaults and limits stay out:
cite the test that pins them or link the generated reference. Test citations
use `path.rs::symbol` relative to `tools/front-door/`; `none (gap)` marks
missing enforcement and `none (review gate)` a review obligation. A claim with
no test and no review gate is not an invariant. `scripts/check-doc-truth`
resolves every citation and warns when a page passes 2,000 words.
[Engineering docs](../../skills/engineering-docs/SKILL.md) owns placement.

The binary is the reference: `provenant help --all` for commands and
`provenant config schema` for config keys. `scripts/generate-reference` renders
them as Markdown under the untracked `<repository>/docs/reference/`.

## Area specs

- [CLI](cli.md): envelope, exits, waits, hooks and checks
- [Disk](disk.md): read-only storage inventory
- [State](state.md): roots, records, config and managed install
- [Supervisor](supervisor.md): custody, confinement and worktrees
- [Routing](routing.md): routes, providers, accounts, quota and readiness
- [Run](run.md): operation IDs, stop handling and the run contracts below
  - [Launch](run-launch.md): admission, confinement and lane environment
  - [Settlement](run-settlement.md): outcomes, continuation and retention
  - [Git and delivery](run-git.md): Git custody, worktrees and publication
  - [Attribution](run-attribution.md): PR contributions and work correlation
- [Retry owner](retry-owner.md): lineage, budgets, admission and recovery
- [Fabric](fabric.md): coordination store, lane relay, graphs and peer hosts
- [Telemetry](telemetry.md): usage ledger, cost and outcomes
- [Workflow](workflow.md): review coverage, receipts and sessions
- [Rust workspace rules](rust-workspace.md): layering, dependencies,
  errors, unsafe and tests

## Cross-cutting decisions

Each row names the area that owns the contract; amend that owner when changing
a cross-cutting rule.

| Topic | Contract owner |
|---|---|
| State roots, identities, layout and environment | [State](state.md#roots-and-identities) |
| Envelope and exits | [CLI](cli.md#one-bounded-envelope-never-truncated) |
| Wait budgets and observation | [CLI](cli.md#waits-are-readers) |
| Command grammar and checks | [CLI](cli.md), `provenant help --all` |
| Selection, accounts, quota and readiness | [Routing](routing.md) |
| Admission, attempts and publication | [Run](run.md), its [launch](run-launch.md), [settlement](run-settlement.md) and [Git](run-git.md) specs, [retry owner](retry-owner.md) |
| Coordination, graph execution and peer hosts | [Fabric](fabric.md) |
| Configuration and managed installation | [State](state.md#configuration) |
| Records and digests | [State](state.md#records) |
| Usage and cost evidence | [Telemetry](telemetry.md) |
| Dependency and layering policy | [Rust workspace](rust-workspace.md#dependencies) |
