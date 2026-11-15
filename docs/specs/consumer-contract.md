# Consumer contract

Owner: `crates/core/src/consumer_env.rs` (environment), `crates/config/src/repository_files.rs` (repository files), `crates/cli/src/external_contract.rs` (the outward declaration)
Anchor: `crates/cli/src/external_contract.rs::ExternalContract`

This page is what a repository, its scripts and its CI may rely on when they
adopt Provenant or run beside it. Each table below is the rendering of one
declaration in code.
[Using Provenant 2 from another repository](../runbooks/using-provenant-2-from-another-repository.md)
is the procedure.

## Stability tiers

Every consumer item carries one tier, declared on its entry. `evolving` is the
default: the item may change in any minor release, and the release notes name
the change. `stable` is kept small: an item is stable only when a consuming
repository relies on it today, and it changes or goes only in a major release,
with a release note. The stable items are `PROVENANT_SANDBOX`,
`PROVENANT_PROCESS_TREE`, `PROVENANT_LANE_CARGO_LINT_TARGET`, `provenant
process-tree --json` and the exit codes it is read through. `internal` is
Provenant's own wiring and may change in any release. A `PROVENANT_*` name the
environment table does not declare is internal, and every `PROVENANT_TEST_*`
name is internal by rule and never declared. Promoting an item is a one-entry
change to its declaration.

- Why: a consumer must know which surfaces it may build on before it builds,
  and promising only what is used keeps the rest free to improve.
- Owner: `crates/core/src/consumer_env.rs`

## No repository coupling

Provenant runs a lane in a repository that holds no Provenant files, scripts,
build files or instruction files, and the lane adds nothing to it beyond the
repository paths declared `created` below and the worktree roots of the
worktree-layout port. Every repository path, branch, remote, layout or command
Provenant uses is a declared default the repository may override through its
port table or settings, never a name compiled into a reader.

- Why: adopting Provenant, or running it beside a repository's own tooling,
  must not force a layout on the repository or collide with what it has.
- Owner: `crates/config/src/repository_files.rs`, `crates/config/src/ports.rs`

## Commands

A command is stable only when a stable entry on this page names it: today
`provenant process-tree --json`. Every other command, and the JSON envelope of
commands other than `process-tree`, is evolving. Human output, including printed
next-step command lines, is evolving: scripts read `--json`. A removed spelling exits 2 with one line
naming its replacement and is never an alias; 2.0 replaced `provenant
dispatch` with `provenant run TEMPLATE BRIEF`.

- Why: a script that drives Provenant needs a fixed grammar and one parseable form.
- Owner: `crates/cli/src/grammar.rs`, `crates/config/src/removed.rs`

## Run files are internal

Files under an attempt's run directory are Provenant's records, not an
interface. Consumers read run state through `provenant status --json`,
`provenant output` and `provenant trace`; direct readers are unsupported and
the file set may change in any release.

- Why: a consumer that parses records pins their layout and blocks every format change.
- Owner: `crates/records/src/attempt_file.rs`

## Environment

Channels: `lane` variables are set in every lane process, `lane_or_caller`
ones are also read from the caller, `port_command` ones are given to repository
port commands, and `caller` ones are read only from the caller. `HOME` is
listed because a lane's home is never the caller's.

| Variable | Channel | Stability | Meaning | When |
|---|---|---|---|---|
| `PROVENANT_SANDBOX` | lane | stable | `on` when the lane runs inside Provenant's native sandbox, `off` when it does not. | every lane |
| `PROVENANT_LANE_DEPTH` | lane | evolving | Nesting depth: 1 for a lane the caller started, one more per nested lane. Absent or `0` means not in a lane. | every lane |
| `PROVENANT_PROCESS_TREE` | lane | stable | Names the sample behind `provenant process-tree --json`. Its presence is the contract; read the sample through that command, never the file. | while the supervisor samples the lane's processes |
| `PROVENANT_LANE_CARGO_LINT_TARGET` | lane | stable | A Cargo target directory beside the lane's own, for lint-only runs: point CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR at it so lints do not queue behind builds. | writer lanes |
| `PROVENANT_LANE_PORT_BASE` | lane | evolving | First port of the lane's private port block. | writer lanes holding an allocated port block |
| `PROVENANT_LANE_PORT_COUNT` | lane | evolving | Number of ports in the lane's private block. | writer lanes holding an allocated port block |
| `PROVENANT_STATE_DIR` | lane_or_caller | evolving | State root. A caller sets it to use a root other than `~/.local/share/provenant/v2`; in a lane it names the attempt's private store. | every lane; when a caller leaves it unset the default root applies |
| `PROVENANT_AGENT_LABEL` | lane_or_caller | evolving | The agent name inbox, fabric and checkpoint writes are attributed to: `lane-<attempt>` in a lane. A caller may set it to name itself. | every lane; when a caller leaves it unset the harness identity applies |
| `PROVENANT_PARENT_ATTEMPT_ID` | lane | evolving | The lane's own attempt. A `provenant run` inside the lane records it as the parent. | every lane |
| `HOME` | lane | evolving | The attempt's private home, where provider credentials and caches are staged. It is never the caller's home. | every lane |
| `PROVENANT_GIT` | lane_or_caller | evolving | The Git executable Provenant runs. In a lane it is the wrapped Git the lane must use. | every lane; when a caller leaves it unset Git is found on PATH |
| `PROVENANT_LANE_HEAD` | lane | evolving | The branch the writer lane delivers to. | lanes whose task names a head branch |
| `PROVENANT_LANE_BASE` | lane | evolving | The branch the lane's head is based on. | lanes whose task names both a head and a base |
| `PROVENANT_PRIMARY_CHECKOUT` | lane | evolving | The project's primary checkout: the directory holding the common Git directory. | lanes launched in a project |
| `PROVENANT_WORKTREE_GITDIR` | lane | evolving | The linked worktree's private Git directory. | writer lanes, and read-only lanes whose worktree has a private Git directory |
| `PROVENANT_SESSION_ID` | caller | evolving | Names the chair session the chair log records. When unset, settlement finds the session the chair saved. | set by the caller |
| `PROVENANT_PORT_PHASE` | port_command | evolving | The admission phase the command runs for. | admission port commands |
| `PROVENANT_ADMISSION_ATTEMPT` | port_command | evolving | The attempt being admitted. | admission port commands |
| `PROVENANT_ADMISSION_WORKTREE` | port_command | evolving | The writer's worktree. | admission port commands |
| `PROVENANT_ADMISSION_PROJECT` | port_command | evolving | The writer's project checkout. | admission port commands |
| `PROVENANT_ADMISSION_OWNER_PID` | port_command | evolving | The Provenant process that owns the admission. | admission port commands |
| `PROVENANT_ADMISSION_EPOCH` | port_command | evolving | The worktree lease epoch the admission holds. | admission port commands |
| `PROVENANT_PORT_OPERATION` | port_command | evolving | The typed forge operation requested from the repository adapter. | forge port commands |
| `PROVENANT_PORT_SOURCE` | port_command | evolving | Source of the selected build admission wrapper. | build admission port commands |
| `PROVENANT_PORT_REQUEST_PATH` | port_command | evolving | The forge request path, such as `repos/{owner}/{repo}`. | CI observation and repository discovery port commands |
| `PROVENANT_PORT_IF_NONE_MATCH` | port_command | evolving | The cached validator to send, or empty when none is sent. | CI observation and repository discovery port commands |
| `PROVENANT_VERIFY_TIER` | port_command | evolving | The verify tier being run. | verify port commands |
| `PROVENANT_VERIFY_PROJECT_ROOT` | port_command | evolving | The project checkout being verified. | verify port commands |
| `PROVENANT_VERIFY_EFFECTIVE_CONFIG_JSON` | port_command | internal | The resolved verify declaration, for Provenant's builtin verify engine. | verify port commands |
| `PROVENANT_RUN_STATE_DIR` | lane | internal | Run storage for children the lane submits. | every lane |
| `PROVENANT_ROOT` | lane | internal | Installed release assets. | every lane |
| `PROVENANT_CARGO_SOURCE_WRITES` | lane | internal | Whether lane Cargo may write registry sources; the cargo shim explains offline failures with it. | lanes with a Cargo home |
| `PROVENANT_CARGO_SOURCE_WRITES_SOURCE` | lane | internal | Where the Cargo source-write setting came from. | lanes with a Cargo home |
| `PROVENANT_BUILD_ADMISSION_BACKEND` | lane | internal | Encoded build admission backend selected by the host. | only for the cargo child of the lane's cargo shim |
| `PROVENANT_BUILD_ADMISSION_SOURCE` | lane | internal | Source of the selected build admission backend. | only for the cargo child of the lane's cargo shim |
| `PROVENANT_BUILD_SLOT` | lane | internal | Marks a cargo process that holds a host build slot. | only for the cargo child of the lane's cargo shim |
| `PROVENANT_BUILD_SLOT_STATE` | lane | internal | The host build-slot directory. | only for the cargo child of the lane's cargo shim |
| `PROVENANT_BUILD_SLOTS` | lane | internal | The host build-slot count. | only for the cargo child of the lane's cargo shim |
| `PROVENANT_LOCAL_CARGO` | lane | internal | The real cargo the shim runs. | only for the cargo child of the lane's cargo shim |
| `PROVENANT_BUILD_DISK_LIMITS` | lane | internal | Encoded build disk limits. | only for the cargo child of the lane's cargo shim |

- Why: a repository's scripts detect and adapt to a lane only through these names.
- Owner: `crates/core/src/consumer_env.rs`

## State root

The state root is `~/.local/share/provenant/v2` unless `PROVENANT_STATE_DIR`
names another. The 1.x root `~/.local/share/provenant/v1` is never written by
2.x, so reinstalling 1.x finds its state as it was left. This tier is evolving.

- Why: two majors must be able to coexist and roll back without migration.
- Owner: `crates/core/src/roots.rs`, `src/state_handoff.rs`

## Repository files

All are optional; a repository with none gets compiled and host defaults.
`[launch_policy]` in `.provenant/config.toml` accepts only `confidential` and
`protected_paths`; `[domains.pr.land]` accepts the keys
`provenant config schema` lists for it.

| Path | Entry | Access | Stability | Purpose |
|---|---|---|---|---|
| `.provenant/config.toml` | file | read | evolving | Repository settings: `[launch_policy]` and `[domains.pr.land]`. |
| `.provenant/ports.json` | file | read | evolving | Repository port table: verify, land, CI observation, worktree layout, naming, post-create, tracker, release and admission backends. |
| `.provenant/templates` | directory | read | evolving | Repository graph templates, layered over the bundled and user templates. |
| `.provenant/tracker-lint.toml` | file | read | evolving | Severity of the tracker lint rules. |
| `.provenant/scratch` | directory | created | internal | Operator scratch for briefs and graph documents; Provenant excludes it through `.git/info/exclude`. |

- Why: a repository must know every path Provenant may read or create in it.
- Owner: `crates/config/src/repository_files.rs`

## Exit codes

Every command exits with the code of its envelope `status`. This tier is stable,
because `provenant process-tree --json` is read through it.

| Status | Code |
|---|---|
| `ok` | 0 |
| `failed` | 1 |
| `refused` | 1 |
| `invalid` | 2 |
| `error` | 3 |
| `unavailable` | 4 |
| `pending` | 5 |
| `waiting` | 0 |
| `timeout` | 124 |
| `cancelled` | 130 |

- Why: a script branches on the exit code before it parses any output.
- Owner: `crates/cli/src/render.rs`

## Process tree

Inside a lane where `PROVENANT_PROCESS_TREE` is set, `provenant process-tree
--json` returns the envelope with the supervisor's latest sample as `data`, or
`unavailable` when no fresh, complete sample exists. Consumers may rely on
these fields; new fields may be added. This tier is stable.

| Object | Field |
|---|---|
| data | `schema_version` |
| data | `root_pid` |
| data | `sampled_at` |
| data | `swap_used_bytes` |
| data | `swap_total_bytes` |
| data | `processes` |
| processes[] | `pid` |
| processes[] | `ppid` |
| processes[] | `pgid` |
| processes[] | `rss_bytes` |

- Why: a memory guard in a repository's build needs a stable reading of its own lane.
- Owner: `crates/supervisor/src/process_tree.rs`
