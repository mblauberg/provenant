# Specifications

Owner: the chairs, per [`MAINTAINING.md`](../../MAINTAINING.md#specifications)
Anchor: `tools/front-door/tests/workspace_policy.rs`

This directory holds Provenant's durable requirements: what must hold, what
must never happen, and why. Code and tests own what exists and how it
behaves. GitHub issues own current delivery state. Operator procedures live
in the [runbooks](../runbooks/).

## How a spec reads

Each spec starts with an `Owner:` line naming the owning crates (or the chairs) and an
`Anchor:` line naming the main test files that pin it. Each rule then gives:

- the rule, as a must or must-never statement;
- **Why**, the reason it exists;
- **Test**, the enforcing test as `path::name`, with paths relative to
  `tools/front-door/`. `none (gap)` marks a rule that no test enforces yet and `none (review gate)` one that review enforces;
- **Owner**, the code that implements it.

Specs do not restate flags, schemas, tables or file layouts. Link or
generate them instead:

- CLI grammar: [`docs/reference/cli.md`](../reference/cli.md), from
  `provenant help --all --json`.
- MCP tools: [`docs/reference/mcp-tools.md`](../reference/mcp-tools.md),
  from `crates/fabric-mcp/tests/fixtures/tool-schemas.json`.
- Config keys: [`docs/reference/config-keys.md`](../reference/config-keys.md),
  from `config/harness-knobs.json`.
- Seatbelt profiles: `docs/reference/seatbelt/<provider>-<mode>.sb`.

`scripts/generate-reference` writes them and `--check` diffs them in CI.

## Area specs

- [CLI](cli.md): envelope, exits, waits, hooks and checks
- [Disk](disk.md): read-only storage inventory
- [State](state.md): roots, records, config and managed install
- [Supervisor](supervisor.md): custody, confinement and worktrees
- [Routing](routing.md): routes, providers, accounts and quota
- [Run](run.md): admission, attempts, publication and reconcile
- [Fabric](fabric.md): coordination store, MCP, graphs and peer hosts
- [Telemetry](telemetry.md): stats, trace and demand counters
- [Workflow](workflow.md): review coverage, receipts and sessions
- [Rust workspace rules](rust-workspace.md): layering, dependencies,
  errors, unsafe and tests

Peer hosts are specified in [fabric.md](fabric.md#peer-hosts); setup is in
the [host runbook](../runbooks/fabric-hosts.md). The [landing lease
runbook](../runbooks/landing-lease.md) covers acquiring a lease and using it to
guard a push.

## Cross-cutting decisions

The chairs settle these once. Area specs cite this table, not each other, and
a revision that disagrees proposes a change here first.

| Topic | Decision | Detail owner |
|---|---|---|
| State root | Default `~/.local/share/provenant/v1`; `--state-dir ABS` overrides `PROVENANT_STATE_DIR`. No fallback to any old root, and a root inside an old root is refused. | state |
| Layout | One state root per host, not per project. Run owns `runs/<RunId>/` and its store at the top level; Fabric owns `fabric/`; install owns `install/` and `releases/`. `ProjectId` is host-local and recorded on each run; fabric-runtime owns the cross-host project key used by federation. | state, with each area's subtree |
| Command contract | [cli](cli.md)'s JSON envelope (`schema_version`, `command`, `status`, `data`, `error`), its 1 MiB result cap and its exit codes: 0 ok, 1 failed, refused or findings, 2 invalid input or version, 3 cannot proceed (setup, I/O, contention), 5 pending (accepted, effect not yet happened, such as a queued merge), 124 wait deadline, 130 cancelled. Exit 4 is reserved, and no command emits it today. Partial coverage and input-required are reported in `data`. A wait that ends because its condition was met, including `InputRequired`, exits 0. | cli |
| Waits | Every wait takes an explicit budget. CLI waits are capped at 14400 s; MCP waits are capped at 55 s, under the client's tool timeout. | run, fabric |
| Run verbs | Run owns submission, observation, cancellation, continuation and recovery at the top level. Fabric owns messaging, tasks, identity, peer hosts and MCP. Supervisor adds no run verbs. The generated [CLI reference](../reference/cli.md) owns invocation syntax. | cli, run |
| Route and quota surface | Routing owns eligibility, pool weights and pacing; an account pin only filters eligibility, with no native-seat inference. Confidential requests exclude models that train on prompts, and free models need explicit opt-in. The [quota menu bar](../../apps/quota-menubar/Sources/QuotaMenuBar/QuotaModel.swift) obtains quota observations through the CLI. The generated [CLI reference](../reference/cli.md) owns route and quota syntax. | routing, cli |
| Accounts | Providers own account identity (each adapter detects its own account label), routing owns eligibility, and run records the resolved label. | providers, routing, run |
| Config | One user-owned `config.toml` under the state root. Compiled presets and bundled data (prices, catalogue, adapter table) ship as TOML assets. The user-decided read-only write exceptions (#1343) and advisory GitHub CLI deny list (#2033) live separately under `projects/<ProjectId>/`; they are not a general config overlay. No profile layering, project overlays, per-knob environment variables or profile mutation verbs; `config show` and `check config` remain, plus an optional `profile =` key. | state |
| Environment | Product inputs are `PROVENANT_STATE_DIR` and `PROVENANT_AGENT_LABEL` (identity: separate inboxes for several agents of one provider). Provider and harness variables pass through as their tools define them. No other `PROVENANT_*` inputs. | state, fabric |
| Checks and hooks | `check` is the one verifying verb, with subjects `config`, `install`, `harness`, `release` and `writing PATHS` (workflow's writing rules); each reports `Finding`s and exits 1 on findings. Repo-only lints such as date checks are workspace tests, not commands. Hooks are one `HookSpec` data table in cli (harness, protocol event, matcher, timeout, handler) that the installer also reads; handlers from any area register there, invoked as `hook <harness> <event>`. A guard hook denies input it cannot read or bound. | cli, with workflow and state |
| Records | Stored records are typed serde models with versioned headers. Fact types that only binary-edge code builds are `Serialize` only. | each area |
| Telemetry | No ledger. `telemetry stats` reduces run's terminal records with the price table. There is no OpenTelemetry collector. | telemetry |
| Installer | `install [--dry-run]`, `check install`, `accounts sync` and `uninstall`; re-applying `install` is the recovery path. Every harness-config change is printed before it is applied, and every registered account directory receives the same managed content. | state |
| Dependencies | `rusqlite` may serve run's store as well as Fabric's. `toml_edit` gains its `serde` feature for typed config ([rust-workspace](rust-workspace.md#dependencies)). | chairs |
