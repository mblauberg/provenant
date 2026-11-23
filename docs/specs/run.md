# Execution: runs

Owner: `crates/run`, `crates/records`, `crates/fabric-runtime`
Anchor: `crates/run/src/lib.rs`, `crates/records/src/stop.rs`

Run admits attempts and records their outcomes. [Launch](run-launch.md),
[settlement](run-settlement.md), [Git and delivery](run-git.md) and
[attribution](run-attribution.md) specify their boundaries. Paths are relative
to `tools/front-door/`.

## Operation IDs

The same operation ID and request digest replay the recorded outcome without
repeating effects; a different digest conflicts. A named graph replays matching
inputs and allocates a new name for different ones ([graph naming](run-launch.md#graph-naming)).
Each attempt has a unique work key, including its graph-assigned attempt number.
- Why: lost replies and concurrent requests must not duplicate work or identity.

## Host Cargo home

Every lane uses the host-resolved `CARGO_HOME` (the host process's environment,
else `HOME/.cargo`). Adapters and the launching shell cannot replace it.
- Why: Cargo fingerprints dependency source paths, so a different home path defeats
  reuse even when symlinks reach the same content.

Lanes share the host's registry, Git databases and checkouts and may write them, so a build
fetches once for every later build. `domains.fabric.lanes.cargo_source_writes = false` makes them
read-only and Cargo offline; a missing dependency then fails offline, and the admission wrapper
names the setting, its source and the host fetch to run. A lane's own manual cargo invocation sees
the read-only Cargo home and `PROVENANT_CARGO_SOURCE_WRITES`. `credentials` and `credentials.toml`
are always denied read and write, including in relocated homes; host `config` files are read-only
data.

## Lanes do not build

Lanes format and coordinate only. The chair runs one warm host-owned integrated
check per epic through the build port, capped by `[admission.build].slots`, and
CI runs clippy and the tests. `lanes.verification` sets the format-only writer
preamble. Lane Cargo output is bounded by `lanes.cargo_cache_max` and collected
after `gc.cargo_target_idle`; `domains.fabric.lanes.cargo_source_writes`
controls writes to the shared source caches.

## Publication order

Settlement imports and publishes private work before releasing writer custody; the
worktree lock stays held through both. A sealed terminal decision replays after
publication contention without relaunching the provider.
- Why: a second writer must not race unfinished publication.

## A writer stops on a token budget

Natural completion takes precedence over budget action. A warning does not stop work; a
hard boundary does. A provider without live usage exposes an advisory limit, never a
fabricated measurement. A token-budget stop commits dirty work safely. A wall deadline
and explicit cancellation never create a commit: settlement records the verified head,
commits ahead of the remote observation, dirty path count and push result in
`AttemptRecord.stop_work`, imports and publishes only commits already present, and leaves
dirty paths in the worktree. The host pushes the lane's own branch only when
`lanes.publish` is enabled and host transport is available; a failed push leaves the head
and unpushed count visible.

`status` and `result` report `budget_usage` beside the provider's totals: the unit
(`fresh_tokens`: uncached input, cache writes and output, excluding cache reads), the limit with
its source layer, and the fresh tokens counted. A missing figure is never a guessed zero.
- Why: a budget must preserve completed work, and one that excludes cache reads must not
  look unenforced beside a larger provider-reported input.

## Build strategy

The chair's host-owned warm check, `provenant verify` tiers and a landing Commands
gate dispatch build work through one runtime in `crates/run/src/build/`.
`build::run` is the only entry point and appends exactly one `JobOutcome` row per
job on every path. `build.*` leaves are typed when read: an unknown value warns
with the leaf and its source and falls back to the builtin value, so one bad leaf
never refuses a port table. There is one strategy, `slots`, declared by the
`BuildPort` in `crates/config/src/ports.rs`; lane strategies, wrapper layers and
command wrappers are removed. One host cap (`[admission.build].slots`) serves
every job. Lanes never enter this runtime.

Under `slots`, work runs in a host slot cloned from a bare store; layout and paths are in
[state](state.md#build-state-has-one-host-root). A slot holds one job at a time: custody
intent is written to `job.json` before spawn and the `intent.lock` lease is inherited by the
build's process tree, so a slot stays occupied until every child is gone. A working-tree
snapshot is a deterministic commit built from a copy of the lane's index; the lane's own
index, refs, worktree files and mtimes are never written. Nothing in a slot is copied back,
and a nonzero `slot_changes` count is reported on stderr. Stream output maps slot paths to
the worktree; no `--message-format` flag is injected.

A job runs locally, with its `exception` recorded and a warning, when it is sandboxed, not
eligible, offload is off, staging or the pool is unavailable or quarantined, or no snapshot
exists. A pool problem warns and runs locally; it never fails the chair's check.

Priority orders waiters and never preempts a running job. A free slot is taken at once;
only a busy pool writes a queue ticket whose liveness is its flock, so a killed waiter's
ticket is reaped by any other. Effective priority rises with wait (`build.queue_aging`), bounding
every level's wait.

Only an infrastructure failure inside 24 hours advances a slot's reset ladder (light reset, full
reset, quarantine); cancellation does not, and a clock rollback forces a full reset. Tick runs
hygiene and best-effort prewarm (`build maintain`) after collection; a scheduling failure warns
and never fails the tick.

`build report` derives its figures from `<state>/build/jobs.jsonl` (schema and field order:
`crates/run/src/build/ledger.rs`); an unknown row cannot pass. The host warm check,
`provenant verify` and a landing Commands gate each build one job through this runtime.

| Failure | Effect |
| --- | --- |
| admit killed mid-job | slot stays occupied until the lease frees; orphan group killed, then sync |
| store fetch or clone fails, store corrupt | `no_snapshot` or `pool_unavailable`, local with a warning; the store is recreated on the next fetch |
| checkout or clean fails | infra `git`, reset ladder, one retry before the first step |
| ENOSPC or EIO, or SEGV, BUS, ILL or ABRT in the child | infra `disk` or `crash_signal`, ladder |
| OS purges Caches | slot recreated with its record |
| ledger unwritable | row goes to the attempt scratch ledger and the report counts it missing |

- Why: the chair and landing share one strategy, one outcome row and one host cap; lanes do not build.
- Owner: `crates/run/src/build/`, `src/admit.rs`, `src/verify.rs`, `src/pr/landing/gate.rs`

## Verification and admission

A writer preamble describes its verification scope (`lanes.verification`,
default `targeted`) but the lane performs no build; the summary names what the
chair's integrated check and CI will run. A brief line `Verification scope:
check|targeted|full` overrides the paragraph and names source `brief`.

Admission attribution has host scope and serves the chair's host-owned warm
check. Before confinement each lane registers under
`<state>/admission/<class>/lanes/` for coordination; `admission::host_holders`
and `host_waiters` combine registered and standalone markers across projects.
Status and wait reasons name the holding project and lane. A stale registration
after a crash does not imply occupancy; a registration failure warns and the
host job proceeds. Registration grants no write to host state or other lanes'
records.
- Why: the chair can widen what the integrated check covers, and one host cap
  bounds all compilation.

## Every limit names the layer that set it

`dispatch_profile` seals, for every effective knob not at its default, the layer that set it:
`launch` for a dispatch override, `host` for the host's `[knobs]` table or preset.
`provenant_config::knob_source` is the one reader, so status JSON and the preamble show the true layer.
- Why: a value reported under the wrong source sends the operator to the wrong place to change it.

## Provider config notices are warnings

A provider's own deprecation notice for inherited configuration (for example a deprecated
Codex `[features]` key) is a warning, not a failed turn. The adapter classifies it at the
decode site, never sets the stream's error flag, shows each distinct notice once per attempt
beside the applied route, and points at `providers.codex.clean_config`.
- Why: a stale inherited key must be actionable without reading as a provider failure.
- Owner: `crates/providers/src/codex.rs`, `crates/providers/src/stream.rs`,
  `crates/cli/src/run.rs`, `crates/cli/src/run/output.rs`

## Lane Git and import

Writers of one verified worktree incarnation share its files and hold occupancy together.
Each attempt has private Git state (HEAD, index, refs, objects, configuration, credentials)
in its attempt directory, so siblings never see each other's staging or unpublished commits
([custody](run-launch.md#admission-and-custody)). The host never executes with lane-mutable
Git configuration; confinement protects the repository's shared Git state.
Import uses Git's quarantined receive and verification, and publication compare-and-swaps the
host branch against the tip it observed. A lane whose history diverged from the branch is merged
by the host and published when the merge is clean and `lanes.history.rebase` is `allow` (the
default). A real conflict, unrelated histories, a configured merge driver, or `rebase = refuse`
retains the lane's private Git and names the cause and the setting; a branch rewound behind the
pinned launch base is refused rather than fast-forwarded over. `lanes.history.unrooted_tip`
judges the lane head before any merge runs, so a merge cannot supply ancestry the head lacks. The
base stays pinned until publication.

The worktree's native index follows the branch from a recorded commit and the digest of the
index Provenant left, advanced under a per-worktree lock (never the admission gate) after
each publication and on every settlement or resume until it matches the tip. The advance is
advisory and never fails settlement. If Git refuses it, or the index or record changed or
cannot be read, both are left unchanged and status reports `index_out_of_sync` with the reason,
the paths and one command that resets them together.

Import retention refs belong to each attempt, even for an identical commit. Settlement clears
an attempt's import pins on every terminal outcome, and GC removes the pins of any settled or
missing attempt in one batched ref transaction. Cleanup keeps the import pin and import-extra
refs as recovery evidence while no writer slot is available or the fence listing has issues.
An unreadable run store leaves pins untouched: GC warns and carries on with its other passes.

A failed Git configuration read names its operation, the file when independent of a config
value, and a static key-class label, with a bounded diagnostic. Config keys, include paths,
values and stderr never enter errors, because values can contain credentials. A config-lock
collision from a sibling writer is waited out within one deadline shared by all attempts.

The GitHub credential receipt governs token delivery even when sandboxing is off: `none`
withholds host token variables, `injected` passes only the selected token, `ambient`
preserves the host environment and pins `GH_CONFIG_DIR` to the host's gh directory (as gh
resolves it) in the lane environment and `gh` shim, because an adapter may re-point the
lane's `XDG_CONFIG_HOME`. The receipt's `detail` names the directory and the setting that
chose it. A lane opens its own pull request with `gh pr create --body-file`;
`provenant pr open` needs a settled attempt.
- Why: untrusted lane state must not alter shared Git or publish over another writer.

## Writer pin and re-pin

A graph pins its source commit at submission; a retry keeps that pin and the predecessor's
terminal head. Before any plan node spends tokens the host preflights the writer worktree: a
dirty tree is refused at submission, naming the paths and `lanes.writer_dirty` with its layer.
That one setting chooses `refuse` (default), `wip_commit` (a local WIP commit recorded in
the graph events before launch) or `continue_in_place` (a warning naming the retained paths).
The preflight covers writer leaves in composites and loops. WIP mutation holds writer
exclusion, refuses unsettled reservations and rechecks head and dirty paths under it.
Credential and Git-internal roots are refused under every value.

On a manual retry whose worktree head descends from the pinned commit, the retry re-pins to
that head and warns with both ids. A head that is not a descendant is refused, naming the pin
and the head and what to do.
- Why: a refusal the host could predict must not waste a plan node's tokens, and an opaque
  cause sends the operator to the wrong place.
- Owner: `crates/run/src/writer_dirty.rs`, `crates/run/src/writer.rs`,
  `src/fabric/graph_execution.rs`, `src/fabric/owner.rs`

## PR contributions JSON reference

The [attribution contract](run-attribution.md) governs contribution records.
The [contribution owner](../../tools/front-door/crates/run/src/pr_contribution.rs)
and [command renderer](../../tools/front-door/src/pr/contributions.rs) define the payload.

## Stop intent

Supervisor owns one typed stop-intent protocol. `stop-intent.json` names the attempt scope,
reason, requester and operation ID; guardian, admission, import, publication and continuation
readers all use `read_stop_intent`. `cancel.json` is reserved for explicit cancellation and
cannot carry a steer. A valid explicit cancellation takes precedence over steering. A foreign,
malformed or unreadable intent grants no continuation or publication authority.

A delivered steer may preserve committed work while its authorised intent still matches. A
current explicit owner cancellation separately permits preservation under proven quiescent
custody, including after an earlier steer or natural exit, and vetoes continuation without
changing the provider's sealed stop cause. A replacement request invalidates an in-flight
preservation token. A stop requested after a natural exit does not turn the finished attempt
into a cancellation.
- Why: every stop reader needs the same meaning, and a steer must never cancel the work it
  intends to continue.
- Owner: `crates/supervisor/src/custody.rs`, `crates/run/src/settlement.rs`

## What follows a stop

Settlement classifies terminal outcome, sealed stop and graph node outcome together; no
settlement sent to the chair succeeds a graph node. Each cause declares its provider, host or
owner source once. Route-quality evidence uses provider-caused outcomes and work the
settlement declaration certifies; a host stop keeps its host source and is never quality
evidence. Missing account identity is unknown usage attribution, not a quality failure.
Each cause has one default next step and one per-node override point (`on_stop`); pinned causes
ignore overrides. A new route starts a fresh session. Unproved custody always goes to the
chair, and no successor writer starts while custody is uncertain. A restart preserves the
worktree and archives prior attempts. See [routing](routing.md) for route selection.
- Why: one decision keeps stop handling consistent and protects writer custody.

A writer's declared stop settles as `stopped_early`, or, when its final handoff is blocked,
as `blocked_environment` or `blocked_dependency` per its `blocked_on`, else `blocked` (an owner
decision, a missing capability or no kind given). The node's advance code records whether the
stop left committed work: `writer_declared_stop` when the head moved past its source commit,
`stopped_no_work` otherwise. Both take the `stopped_early` row unless the node overrides it:
a new route in a fresh session on the retained worktree. A junior route escalates to senior;
otherwise a declared route not yet launched is used, and an alias route may resolve again.

With no different route, a stop that can continue on its own route (`stopped_early`,
`lane_token_lost`, `context_handoff`) takes the same route with a fresh session on the
retained worktree and branch, briefed with the previous handoff. That fallback is bounded by
`domains.fabric.graph.limits.continuations` (the restart and pause reasons name its value and
source) and by progress: after the first round each further round must add a commit or change
the handoff's `remaining[]`, else it pauses for the chair. A node `on_stop` entry wins over the
fallback. Any other stop with no route, or with the budget spent, pauses for the chair.

A `blocked` stop is pinned to the chair, and its pause says which kind: only `owner_decision`
and `capability` read as such, and a handoff with no `blocked_on` says the kind was not given.
`blocked_environment` retries once on the same route in a fresh session, then pauses for the
chair as an environment problem; `blocked_dependency` pauses as a wait. Neither is pinned.
- Why: the chair must see whether a restart resumes committed work, and a writer that stops
  early on its only route should continue, not wait for a manual relaunch.
- Owner: `crates/records/src/stop.rs` (`STOP_TABLE`), `crates/fabric-runtime/src/graph/transitions.rs`,
  `crates/fabric-runtime/src/graph/next_step.rs`

| Cause | Default | Pinned | Retries |
| --- | --- | --- | --- |
| `stopped_early` | `new_route` | no | 0 |
| `blocked` | `chair` | yes | 0 |
| `blocked_environment` | `chair` | no | 1 |
| `blocked_dependency` | `chair` | no | 0 |
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
| `unclassified` | `new_route` | no | 1 |