# Specifications

Owner: the chairs, per [`MAINTAINING.md`](../../MAINTAINING.md#specifications)
Anchor: [`MAINTAINING.md#Specifications`](../../MAINTAINING.md#specifications)

This directory holds Provenant's durable requirements: what must hold, what
must never happen, and why. Code and tests own what exists and how it
behaves. GitHub issues own current delivery state. Operator procedures live
in the [runbooks](../runbooks/).

[Engineering docs](../../skills/engineering-docs/SKILL.md) owns placement and
evidence conventions; [MAINTAINING](../../MAINTAINING.md#specifications) owns
the drift gate. Test citations use explicit `path.rs::symbol` paths relative to
`tools/front-door/`. `none (gap)` identifies missing enforcement; `none (review
gate)` identifies a review obligation.

Specs do not restate flags, schemas, tables or file layouts. Link or
generate them instead:

- Core CLI grammar: [`docs/reference/cli.md`](../reference/cli.md), from
  `provenant help --json`. The [complete CLI reference](../reference/cli-all.md)
  comes from `provenant help --all --json`.
- MCP tools: [`docs/reference/mcp-tools.md`](../reference/mcp-tools.md),
  from `crates/fabric-mcp/tests/fixtures/all-tool-schemas.json`.
- Config keys: [`docs/reference/config-keys.md`](../reference/config-keys.md),
  from `config/harness-knobs.json`.
- Seatbelt profiles: `crates/providers/tests/snapshots/seatbelt/`.

`scripts/generate-reference` writes the CLI, MCP and configuration references;
provider goldens are checked by the Rust snapshot test.

## Area specs

- [CLI](cli.md): envelope, exits, waits, hooks and checks
- [Disk](disk.md): read-only storage inventory
- [State](state.md): roots, records, config and managed install
- [Supervisor](supervisor.md): custody, confinement and worktrees
- [Routing](routing.md): routes, providers, accounts and quota
- [Route readiness](route-readiness.md): readiness evidence and projection
- [Tracker lint](tracker-lint.md): advisory issue checks
- [Run](run.md): admission, attempts, publication and reconcile
- [Retry owner](retry-owner.md): lineage, budgets, admission and recovery
- [Fabric](fabric.md): coordination store, MCP, graphs and peer hosts
- [Telemetry](telemetry.md): stats, trace and demand counters
- [Workflow](workflow.md): review coverage, receipts and sessions
- [Rust workspace rules](rust-workspace.md): layering, dependencies,
  errors, unsafe and tests

## Cross-cutting decisions

Each row points to the area that owns the contract. Amend that owner when
changing a cross-cutting rule.

| Topic | Contract owner |
|---|---|
| State roots, identities, layout and environment | [State](state.md#roots-and-identities) |
| Envelope and exits | [CLI](cli.md#one-bounded-envelope-never-truncated) |
| Wait budgets and observation | [CLI](cli.md#waits-are-readers) |
| Command grammar and checks | [CLI](cli.md), [generated reference](../reference/cli.md) |
| Selection, accounts and quota | [Routing](routing.md), [readiness](route-readiness.md) |
| Admission, attempts and publication | [Run](run.md), [retry owner](retry-owner.md) |
| Coordination, graph execution and peer hosts | [Fabric](fabric.md) |
| Configuration and managed installation | [State](state.md#configuration) |
| Records and digests | [State](state.md#records) |
| Usage and cost evidence | [Telemetry](telemetry.md) |
| Dependency and layering policy | [Rust workspace](rust-workspace.md#dependencies) |
