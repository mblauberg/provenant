# Specifications

This directory is Provenant requirements doctrine, not a live work tracker or
globally applied cross-project harness policy. Each linked file is the
independent normative owner of one subsystem. Git owns content integrity and
history. GitHub issues and their Project Status fields own current delivery
state, owner, dependencies and user gates. Operator procedures live in the
[runbooks](../runbooks/).

## Area specs

- [CLI](cli.md): command tree, envelope, exit codes, hooks and checks
- [Disk](disk.md): read-only storage inventory and measurement limits
- [State](state.md): core, records, config, release and managed install
- [Supervisor](supervisor.md): process supervision, confinement, worktrees and Git evidence
- [Routing](routing.md): routing, providers and quota
- [Run](run.md): runs, admission, attempts and reconcile
- [Fabric](fabric.md): Fabric store, runtime, MCP and graphs
- [Telemetry](telemetry.md): stats, trace, counters and prices
- [Workflow](workflow.md): review coverage, receipts, session checkpoints and writing checks
- [Rust workspace rules](rust-workspace.md): crate layering, dependencies, errors, unsafe and tests

Fabric across the user's own hosts is specified in
[fabric.md](fabric.md#peer-hosts); setup is in the
[host runbook](../runbooks/fabric-hosts.md). The [landing lease
runbook](../runbooks/landing-lease.md) covers acquiring a lease and fencing a
push with it.

## Harness

- [Supervisor and confinement](supervisor.md): run supervision, custody and permission policy
- [Switching and comparing profiles](../runbooks/configuration.md)

## Cross-cutting decisions

The chairs settle these once. Area specs cite this table, not each other, and
a revision that disagrees proposes a change here first.

| Topic | Decision | Detail owner |
|---|---|---|
| State root | Default `~/.local/share/provenant/v1`; `--state-dir ABS` overrides `PROVENANT_STATE_DIR`. No fallback to any old root, and a root inside an old root is refused. | state |
| Layout | One state root per host, not per project. Run owns `runs/<RunId>/` and its store at the top level; Fabric owns `fabric/`; install owns `install/` and `releases/`. `ProjectId` is host-local and recorded on each run; fabric-runtime owns the cross-host project key used by federation. | state, with each area's subtree |
| Command contract | [cli](cli.md)'s JSON envelope (`schema_version`, `command`, `status`, `data`, `error`), its 1 MiB result cap and its exit codes: 0 ok, 1 failed, refused or findings, 2 invalid input or version, 3 cannot proceed (setup, I/O, contention), 124 wait deadline, 130 cancelled. Exit 4 is reserved and no command emits it today: partial coverage and input-required are reported in `data`. A wait that ends because its condition was met, including `InputRequired`, exits 0. | cli |
| Waits | Every wait takes an explicit budget. CLI waits allow up to 14400 s; MCP waits allow up to 55 s, below the client's tool timeout. | run, fabric |
| Run verbs | Run owns its verbs at the top level: `dispatch`, `lanes [--wait]`, `output`, `cancel`, `resume` and `reconcile` (recover queued and orphaned work after a scheduler dies). The `fabric` subtree keeps messaging, tasks, the agent label, peer hosts and MCP, and drops named sessions (run `resume` covers continuation); `supervisor` adds no run verbs. [cli](cli.md) freezes the exact flags. | cli, run |
| Route and quota freeze | `route list\|resolve` and `quota show\|tick` stay. Route inputs are `--model ADAPTER/ID` or `--route NAME`, optional effort; fallback is `none` or an explicit list of at most two models, default none, read-only only. `--confidential` restricts a request to models that do not train on prompts, and `--allow-free` admits free-tier models. `utility`, `junior`, `senior`, `principal`, `writing` and `design` are exact route names with fixed ordered candidates. Pools carry weights and pacing; an account pin only filters eligibility (`dispatch --account` or a task's `account`), and there is no native-seat inference. The live quota-menubar LaunchAgent calls tick ([QuotaModel.swift](../../apps/quota-menubar/Sources/QuotaMenuBar/QuotaModel.swift)); cli freezes these leaves and shared route inputs. | routing, cli |
| Accounts | Providers own account identity (each adapter detects its own account label), routing owns eligibility, and run records the resolved label. | providers, routing, run |
| Config | One user-owned `config.toml` under the state root. Compiled presets and bundled data (prices, catalogue, adapter table) ship as TOML assets. The user-decided read-only write exceptions (#1343) live separately at `projects/<ProjectId>/read-only.json`, owned by confinement; they are not a general config overlay. No profile layering, project overlays, per-knob environment variables or profile mutation verbs; `config show` and `check config` remain, plus an optional `profile =` key. | state |
| Environment | Product inputs are `PROVENANT_STATE_DIR` and `PROVENANT_AGENT_LABEL` (identity: separate inboxes for several agents of one provider). Provider and harness variables pass through as their tools define them. No other `PROVENANT_*` inputs. | state, fabric |
| Checks and hooks | `check` is the one verifying verb, with subjects `config`, `install`, `harness`, `release` and `writing PATHS` (workflow's writing rules); each reports `Finding`s and exits 1 on findings. Repo-only lints such as date checks are workspace tests, not commands. Hooks are one `HookSpec` data table in cli (harness, protocol event, matcher, timeout, handler) that the installer also reads; handlers from any area register there, invoked as `hook <harness> <event>`. A guard hook denies input it cannot read or bound. | cli, with workflow and state |
| Records | Stored records are typed serde models with versioned headers. Fact types that only edge code builds are `Serialize` only. | each area |
| Telemetry | No ledger. `telemetry stats` reduces run's terminal records with the price table. There is no OTel collector. | telemetry |
| Installer | `install [--dry-run]`, `check install` and `uninstall`; re-applying `install` is the recovery path. Migrating from a pre-1.0 install uses `--legacy-checkout ABS` and prints every harness-config change before applying it. | state |
| Dependencies | `rusqlite` may serve run's store as well as Fabric's. `toml_edit` gains its `serde` feature for typed config ([rust-workspace](rust-workspace.md#dependencies)). | chairs |
