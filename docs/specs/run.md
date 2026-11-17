# Execution: runs

Owner: `crates/run`, `crates/records`, `crates/fabric-runtime`
Anchor: `crates/run/src/lib.rs`, `crates/records/src/stop.rs`

Run admits attempts and records their outcomes. [Launch](run-launch.md),
[settlement](run-settlement.md), [Git and delivery](run-git.md) and
[attribution](run-attribution.md) specify their boundaries. Paths are relative
to `tools/front-door/`.

## Operation IDs

The same operation ID and request digest replay the recorded outcome without
repeating effects; a different digest conflicts. Named graphs replay matching
inputs across the name family and allocate a new name for different inputs.
Collected names remain reserved. Each attempt has a unique work key, including
its graph-assigned attempt number.
- Why: lost replies and concurrent requests must not duplicate work or identity.

## Host Cargo home

Every lane uses the host-resolved `CARGO_HOME` (the host process's environment,
otherwise its `HOME/.cargo`), preserving the absolute source path Cargo fingerprints.
Adapter-selected homes and launching-shell values cannot replace that binding.
The launch's `cargo_home` diagnostic names both the path source and the effective
`domains.fabric.lanes.cargo_source_writes` setting source.

Lanes share the host's registry index, cache and sources and Git databases and
checkouts, and may write them by default, so a lane fetches what it needs once for
every later build. Cargo's package-cache locks and global-cache database (including
its rollback journal) are always writable. Cargo runs online whenever the lane has
open network access. `cargo_source_writes = false` makes the source stores
read-only and runs Cargo offline; a missing dependency then fails, and the Cargo
shim names the knob, its source and suggests fetching on the host. Cargo credentials (`credentials` and
`credentials.toml`) remain mandatory read/write denials, including relocated homes.
Host `config` and `config.toml` are read-only data; target seeding checks them with
the same compiler-selection validation as checked-in Cargo configuration.
Build directories and lane-specific tool configuration remain private to the lane
cache (or attempt scratch for read-only lanes). No separate lane download store,
Cargo home or symlink setup exists. The host's own build output supplies target
seed generations.
- Why: Cargo fingerprints dependency source paths; changing the home path defeats reuse even when symlinks reach the same content.

## Cargo target seed

A target seed store is shared across registered repositories on the host. Each
generation records a hash of the verified canonical shared Git directory and
verify-base commit plus its Rust compiler identity and effective
`CARGO_INCREMENTAL` and `CARGO_PROFILE_DEV_DEBUG` values, so a toolchain or
profile change rebuilds the seed even when the commit is unchanged. Compiler
identity uses toolchain pins read from the verify-base tree at the selected
workspace, not the registered checkout's current toolchain. The same selected
workspace path is used to validate and build the pinned checkout. Linked
checkouts share identity and another repository's newer generation does not
force a rebuild. A writer lane prefers the newest compatible generation for its
verified project identity, then uses a compatible generation from another
project if no own generation exists. The seed builder and writer preparation
use the same lane profile defaults and preserve explicit environment values;
incompatible profile generations are skipped. Refresh currentness requires the
recorded verify-base commit to match; writer selection uses compatible
generations and Cargo's normal per-unit freshness checks before reusing artifacts. Published generations are
immutable, and collection cannot race an active clone. `max_generations` keeps
history per project; `max_bytes` remains a host-wide allocated-byte limit and
can evict a project's only generation under byte pressure. Failed refreshes
retry at the next scheduled collection, while busy projects retry on the next
tick without rebuilding other failed projects. Full requests supersede queued
busy retries and reevaluate prior failures. Detached pending and active scopes
survive process interruption and are retried after a minute.
When a scheduled refresh fails, `provenant tick` reports a bounded cause category
and a next step from the saved result. This note does not expose persisted
command output, repository paths or environment values, and does not change the
tick outcome or retry schedule.
- Why: reuse must not introduce incompatible or partially collected build output.

## Publication order

Settlement imports and publishes private work before releasing writer custody;
the worktree lock remains held through both. A sealed terminal decision can be
replayed after publication contention without relaunching the provider.
- Why: a second writer must not race unfinished publication.

## A writer stops on a token budget

Natural completion takes precedence over budget action. A warning does not stop
work; a hard boundary stops it. Providers without live usage expose an advisory
limit rather than a fabricated measurement. The token-budget stop keeps its
existing safe commit of dirty work. A wall deadline and explicit cancellation
never create a commit: settlement records the verified head, commits ahead of
the launch-bound remote observation, dirty path count, and push result in the optional
`AttemptRecord.stop_work` summary. Explicit cancellation imports and publishes
only commits already present at the stop; dirty paths remain in the worktree.
The host pushes the lane's own branch only when the persisted `lanes.publish`
grant is enabled and the launch-bound host transport is available. A failed or
unavailable push leaves the committed head and unpushed count visible.

| Record field | Reader compatibility |
| --- | --- |
| `AttemptRecord.stop_work` | Optional; older records read as absent, and older readers retain it as an unknown field. |

- Why: budget enforcement must preserve completed work and report its evidence.

## Lane verification and the host build cap

A writer preamble's verification paragraph is `lanes.verification` (`check`, `targeted` or `full`). Unset, the paragraph is the historical compile-check wording and names no source. A brief line `Verification scope: targeted` (or `check` or `full`) overrides the knob and the paragraph names source `brief`. When the knob and that line disagree, the paragraph follows the brief and the stop cause names both scopes. The host build cap is `domains.fabric.lanes.max_concurrent_builds`: unset, the count is derived from cores and memory (source `default`); a set value is source `host`; `0` disables the cap. A compiling cargo command that finds every slot busy waits a bounded time, then warns and proceeds.
- Why: a chair brief must be able to widen what a lane verifies, and parallel lanes must share one host cap instead of each taking the machine.

## Every limit names the layer that set it

`dispatch_profile` seals, for every effective knob not at its registry default,
the layer that set it: `launch` for a dispatch override, `host` for the host's
`[knobs]` table or its selected preset. `provenant_config::knob_source` is the
one reader; lane limits, CI scope, the gh budget and sandbox admission take
their reported source from it, so status JSON and the preamble show the true
layer.
- Why: a value reported under the wrong source sends the operator to the wrong
  place to change it.

## Lane Git and import

Writers use private Git state; the host does not execute with lane-mutable Git
configuration. For sandboxed lanes, confinement protects shared Git state.
Import uses Git's quarantined receive and verification; publication compares
against the pinned base. The base remains pinned until publication.
The GitHub credential receipt governs token delivery even when sandboxing is
off: `none` withholds host token environment variables, `injected` passes only
the selected token, and `ambient` preserves the native host environment.
- Why: untrusted lane state must not alter shared Git or publish over another writer.

## PR contributions JSON reference

The [attribution contract](run-attribution.md) governs contribution records.
The [contribution owner](../../tools/front-door/crates/run/src/pr_contribution.rs)
and [command renderer](../../tools/front-door/src/pr/contributions.rs) define the payload.

## Stop intent

Supervisor owns one typed stop-intent protocol. `stop-intent.json` names the
attempt scope, reason, requester and operation ID; guardian, admission,
import, publication and continuation readers use `read_stop_intent`.
`cancel.json` is reserved for explicit cancellation and cannot carry a steer.
A valid explicit cancellation takes precedence over steering. A foreign,
malformed or unreadable intent grants no continuation or publication authority.

A delivered steer may preserve committed work while its exact authorised
intent still matches. A current explicit owner cancellation separately permits
preservation under proven quiescent custody, including after an earlier steer
or natural exit. It vetoes continuation without changing the provider's sealed
stop cause. A replacement request invalidates an in-flight preservation token;
recovery must validate the new intent before acting. Stop delivery
ordering belongs to the provider exit: requesting a stop after a natural exit
does not turn the finished attempt into a cancellation.
- Why: every stop reader needs the same meaning, and a steering request must
  never accidentally cancel the work it intends to continue.
- Owner: `crates/supervisor/src/custody.rs`, `crates/run/src/settlement.rs`

## What follows a stop

Settlement classifies terminal outcome, sealed stop and graph node outcome together; no settlement sent to the chair succeeds a graph node.
Each cause declares provider, host or owner source once. Route-quality evidence uses provider-caused outcomes and successful work certified by the settlement declaration; a host stop keeps its host source and host failures remain non-quality evidence. Missing account identity remains unknown usage attribution, not a model-quality failure.
Each cause has one default next step, with one per-node override point. Pinned
causes ignore overrides. A new route starts a fresh session. Unproved custody
always goes to the chair; no successor writer starts while custody is uncertain.
A restart preserves the worktree and archives prior attempts. Bounds may turn a
restart into a chair decision. See [routing](routing.md) for route selection.
- Why: one decision keeps stop handling consistent and protects writer custody.

A writer's declared stop settles as `stopped_early`, or as `blocked` when its
final handoff is blocked. The node's advance code records whether the stop left
committed work: `writer_declared_stop` when the writer's head moved past its
source commit, and `stopped_no_work` otherwise, whether the worktree is clean or
holds uncommitted edits. Both codes take the `stopped_early` row below unless
the node overrides it: a new route, which restarts the writer in a fresh session
on its retained worktree. A junior route escalates to senior; otherwise the
restart uses a declared route not yet launched, and an alias route may resolve
again. With no such route, or with the continuation budget spent, the writer
pauses for the chair. A `blocked` stop always goes to the chair.
- Why: the chair must see whether a restart resumes committed work, without a
  second stop rule that could treat the two differently.
- Owner: `crates/fabric-runtime/src/graph/transitions.rs`,
  `crates/fabric-runtime/src/graph/next_step.rs`

| Cause | Default | Pinned | Retries |
| --- | --- | --- | --- |
| `stopped_early` | `new_route` | no | 0 |
| `blocked` | `chair` | yes | 0 |
| `handoff_missing` | `chair` | no | 0 |
| `handoff_malformed` | `chair` | no | 0 |
| `empty_result` | `new_route` | no | 1 |
| `uncommitted` | `chair` | no | 0 |
| `input_required` | `chair` | yes | 0 |
| `context_handoff` | `new_route` | no | 0 |
| `token_budget` | `chair` | no | 0 |
| `deadline` | `chair` | no | 0 |
| `cancelled` | `chair` | yes | 0 |
| `provider_stop` | `new_route` | no | 1 |
| `process_crash` | `new_route` | no | 1 |
| `auth_required` | `new_route` | no | 0 |
| `usage_limited` | `new_route` | no | 0 |
| `model_unavailable` | `new_route` | no | 0 |
| `adapter_unavailable` | `new_route` | no | 0 |
| `permission_denied` | `chair` | no | 0 |
| `identity_mismatch` | `chair` | yes | 0 |
| `lane_token_lost` | `new_route` | no | 0 |
| `source_view_refused` | `chair` | no | 0 |
| `route_refused` | `chair` | no | 0 |
| `access_policy_refused` | `chair` | no | 0 |
| `disk_floor` | `chair` | no | 0 |
| `disk_unobserved` | `new_route` | no | 1 |
| `prompt_unreadable` | `chair` | no | 0 |
| `cloud_placement_removed` | `chair` | no | 0 |
| `write_grants_refused` | `chair` | no | 0 |
| `worktree_busy` | `chair` | no | 0 |
| `launch_handoff_refused` | `chair` | no | 0 |
| `retained_writer_import` | `chair` | no | 0 |
| `unsupported_platform` | `chair` | no | 0 |
| `unknown_pacing_account` | `chair` | no | 0 |
| `unregistered_account` | `chair` | no | 0 |
| `credential_helper_failed` | `chair` | no | 0 |
| `command_unavailable` | `chair` | no | 0 |
| `host_native_unconfigured` | `chair` | no | 0 |
| `spawn_failure` | `new_route` | no | 1 |
| `settlement_failed` | `chair` | no | 0 |
| `lost` | `chair` | no | 0 |
| `unsettled` | `chair` | no | 0 |
| `quarantined` | `chair` | no | 0 |
| `steered` | `chair` | yes | 0 |
| `unclassified` | `new_route` | no | 0 |
