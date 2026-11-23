# Consumer contract

Owner: `crates/core/src/consumer_env.rs` (environment), `crates/config/src/repository_files.rs` (repository files), `crates/cli/src/external_contract.rs` (the outward declaration)
Anchor: `crates/cli/src/external_contract.rs::ExternalContract`

What a repository, its scripts and its CI may rely on when they adopt
Provenant or run beside it. Each table below renders one declaration in code
(`provenant-cli print-reference-data` prints them) and a test fails when they
disagree. [Using Provenant 2 from another repository](../runbooks/using-provenant-2-from-another-repository.md)
is the procedure.

## Stability tiers

Every consumer item carries one tier, declared on its entry. `evolving` is the
default: the item may change in any minor release, and the release notes name
the change. `stable` is kept small: an item is stable only when a consuming
repository relies on it today, and it changes or goes only in a major release,
with a release note. `internal` is Provenant's own wiring and may change in any
release. A `PROVENANT_*` name the environment table does not declare is
internal, and every `PROVENANT_TEST_*` name is internal by rule and never
declared. Promoting an item is a one-entry change to its declaration.

`PROVENANT_LANE_CARGO_LINT_TARGET` was formerly `stable`. It is removed rather
than retired in place: its only producer was the lane Cargo cache, which writer
lanes no longer create, so no lane sets it and no consumer can read it. The
removal is named in the release notes even though it lands outside a major.

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

Command names may change freely, and an unknown command or flag fails with
exit 2; there are no aliases. JSON envelopes other than `process-tree` are
evolving, and so is human output, including printed next-step command lines:
scripts read `--json`.

- Why: a script that drives Provenant needs a fixed grammar and one parseable form.
- Owner: `crates/cli/src/grammar.rs`

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
port commands, and `caller` ones are read only from the caller.

| Variable | Channel | Stability | Meaning | When |
|---|---|---|---|---|
| `PROVENANT_ROUTE` | lane | evolving | Applied adapter/model with optional @effort. | when the applied model is known |
| `PROVENANT_MODEL` | lane | evolving | Applied model identifier. | when the applied model is known |
| `PROVENANT_EFFORT` | lane | evolving | Applied effort. | when an effort is applied |
| `PROVENANT_ACCOUNT` | lane | evolving | Applied account label. | when an account label is known |
| `PROVENANT_SANDBOX` | lane | stable | `on` when the lane runs inside Provenant's native sandbox, `off` when it does not. | every lane |
| `PROVENANT_LANE_DEPTH` | lane | evolving | Nesting depth: 1 for a lane the caller started, one more per nested lane. Absent or `0` means not in a lane. | every lane |
| `PROVENANT_PROCESS_TREE` | lane | stable | Names the sample behind `provenant process-tree --json`. Its presence is the contract; read the sample through that command, never the file. | while the supervisor samples the lane's processes (`domains.run.resource_sample_interval_ms` set, or an admission class sets `memory_per_slot`; off otherwise) |
| `PROVENANT_LANE_PORT_BASE` | lane | evolving | First port of the lane's private port block. | writer lanes holding an allocated port block |
| `PROVENANT_LANE_PORT_COUNT` | lane | evolving | Number of ports in the lane's private block. | writer lanes holding an allocated port block |
| `PROVENANT_STATE_DIR` | lane_or_caller | evolving | State root. A caller sets it to use a root other than `~/.local/share/provenant/v2`; in a lane it names the attempt's private store. | every lane; when a caller leaves it unset the default root applies |
| `PROVENANT_AGENT_LABEL` | lane_or_caller | evolving | The agent name inbox, fabric and checkpoint writes are attributed to: `lane-<attempt>` in a lane. A caller may set it to name itself. | every lane; when a caller leaves it unset the harness identity applies |
| `PROVENANT_PARENT_ATTEMPT_ID` | lane | evolving | The lane's own attempt. A `provenant run` inside the lane records it as the parent. | every lane |
| `HOME` | lane | evolving | The home directory selected during provider launch preparation. Unsandboxed lanes use the caller's home by default; a provider or selected account may choose an account-specific or private home in either mode. Provider cache and credential locations may also differ from `HOME`. | every lane |
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
| `PROVENANT_PORT_REQUEST_PATH` | port_command | evolving | The forge request path, such as `repos/{owner}/{repo}`. | CI observation and repository discovery port commands |
| `PROVENANT_PORT_IF_NONE_MATCH` | port_command | evolving | The cached validator to send, or empty when none is sent. | CI observation and repository discovery port commands |
| `PROVENANT_VERIFY_SELECTION` | port_command | internal | The affected package selection for the verify engine. | verify port commands |
| `PROVENANT_VERIFY_TIER` | port_command | evolving | The verify tier being run. | verify port commands |
| `PROVENANT_VERIFY_PROJECT_ROOT` | port_command | evolving | The project checkout being verified. | verify port commands |
| `PROVENANT_VERIFY_EFFECTIVE_CONFIG_JSON` | port_command | internal | The resolved verify declaration, for Provenant's builtin verify engine. | verify port commands |
| `PROVENANT_RUN_STATE_DIR` | lane | internal | Run storage for children the lane submits. | every lane |
| `PROVENANT_ROOT` | lane | internal | Installed release assets. | every lane |
| `PROVENANT_CARGO_SOURCE_WRITES` | lane | internal | Whether the lane Cargo home may write registry sources. | lanes with a Cargo home |
| `PROVENANT_CARGO_SOURCE_WRITES_SOURCE` | lane | internal | Where the Cargo source-write setting came from. | lanes with a Cargo home |
| `PROVENANT_ADMISSION_CLASS` | lane | internal | The admission class name (e.g. `build`). | only for admitted commands |
| `PROVENANT_ADMISSION_SLOT` | lane | internal | Marks a process that holds an admission class slot. | only for admitted commands |
| `PROVENANT_ADMISSION_SLOT_STATE` | lane | internal | The host admission state directory. | only for admitted commands |
| `PROVENANT_ADMISSION_SLOTS` | lane | internal | The host admission class slot count. | only for admitted commands |
| `PROVENANT_ADMISSION_WAIT_DIR` | lane | internal | The lane scratch directory for admission wait signal. | only for admitted commands |
| `PROVENANT_ADMISSION_DISK_LIMITS` | lane | internal | Encoded admission disk limits. | only for admitted commands |
| `PROVENANT_ADMISSION_MEMORY_LIMIT` | lane | internal | Admission memory limit in bytes. | only for admitted commands |
| `PROVENANT_ADMISSION_TRIM_ACTION` | lane | internal | Trim action executed on disk floor or memory pressure. | only for admitted commands |

- Why: a repository's scripts detect and adapt to a lane only through these names.
- Owner: `crates/core/src/consumer_env.rs`

## State root

The state root is `~/.local/share/provenant/v2` unless `PROVENANT_STATE_DIR`
names another. The default root does not warn because the workspace is the home
directory or its ancestor, and an unusual root's advisory overlaps are reported
once per state-root digest. The previous major's root,
`~/.local/share/provenant/v1`, is never written
([install runbook](../runbooks/install.md#first-20-install-after-1x)). This tier
is evolving.

- Why: two majors must be able to coexist without touching each other's state.
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

Every command exits with the code of its envelope `status`. This tier is stable.

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
- Owner: `crates/records/src/command_result.rs`

## Process tree

Inside a lane where `PROVENANT_PROCESS_TREE` is set, `provenant process-tree
--json` returns the envelope with the supervisor's latest sample as `data`, or
`unavailable` when no fresh, complete sample exists. Consumers may rely on
these fields; new fields may be added. With no `--root`, the command returns the
supervised command's complete sampled subtree. `--root PID` selects a subtree
within that sample. The output tier is stable.

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
