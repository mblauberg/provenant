# Specifications

Owner: the chairs, per [`MAINTAINING.md`](../../MAINTAINING.md#specifications)
Anchor: [`MAINTAINING.md#Specifications`](../../MAINTAINING.md#specifications)

This directory holds Provenant's durable requirements: what must hold, what
must never happen, and why. Code and tests own what exists and how it behaves,
GitHub issues own delivery state, and the [runbooks](../runbooks/) own
procedure.

Each spec is an invariants page. An entry states one invariant in a few
sentences, then gives a one-sentence `Why` and an `Owner`.
Narration, enumerations, history, flags, schemas, defaults and limits stay out:
name the owning module, type or declaration in `Anchor:` or link the generated
reference. Specs never name individual tests, line numbers or inventory counts.
Tests cite the section they pin with `Spec: docs/specs/NAME.md#anchor`.
`scripts/check-doc-truth` strictly resolves those Rust comments to spec files
and heading anchors, and warns when a page exceeds its word cap.
[Engineering docs](../../skills/engineering-docs/SKILL.md) owns placement.

The binary is the reference: `provenant help --all` for commands and
`provenant config schema` for config keys. `scripts/generate-reference` renders
them as Markdown under the untracked `<repository>/docs/reference/`.

## Area specs

- [Consumer contract](consumer-contract.md): what an adopting repository may rely on, and how stably
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
| Consumer stability, lane environment and repository files | [Consumer contract](consumer-contract.md) |
| Wait budgets and observation | [CLI](cli.md#waits-are-readers) |
| Command grammar and checks | [CLI](cli.md), `provenant help --all` |
| Selection, accounts, quota and readiness | [Routing](routing.md) |
| Admission, attempts and publication | [Run](run.md), its [launch](run-launch.md), [settlement](run-settlement.md) and [Git](run-git.md) specs, [retry owner](retry-owner.md) |
| Coordination, graph execution and peer hosts | [Fabric](fabric.md) |
| Configuration and managed installation | [State](state.md#configuration) |
| Records and digests | [State](state.md#records) |
| Usage and cost evidence | [Telemetry](telemetry.md) |
| Dependency and layering policy | [Rust workspace](rust-workspace.md#dependencies) |
