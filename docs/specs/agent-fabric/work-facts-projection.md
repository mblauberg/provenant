# Work facts projection

Fabric projects workflow facts only from records it owns. Result-shape feature
`work-facts-projection.v1` adds one closed `workflow` field to every work
summary and task detail returned by the operator-projection v2 operations. It
changes no baseline operation and no unnegotiated result.

~~~yaml
workflow:
  workflowRevision: positive-daemon-global-revision
  objective: {observation: Observed, value: exact-task-objective}
  dependencies:
    observation: Observed
    dependencyRevision: positive-current-run-dependency-revision
    taskIds: [exact-dependency-task-id, ...]
  coordinationRun:
    observation: Observed
    projectSessionId: exact-task-session
    coordinationRunId: exact-task-run
  workstream:
    observation: Observed
    workstreamId: exact-workstream
    deliveryRunId: exact-delivery-run
    workstreamRevision: positive-stored-revision
    state: active | complete | cancelled | degraded | abandoned
  # or
  workstream: {observation: Unobserved}
  # or
  workstream: {observation: Unknown, reason: MultipleWorkstreamBindings}
  parentTask: {observation: Unobserved}
  plan:
    observation: Observed
    planRevision: positive-current-run-plan-revision
  # or
  plan: {observation: Unobserved}
  task:
    observation: Observed
    state: blocked | ready | active | complete | cancelled | degraded
    owner:
      observation: Observed
      agentId: exact-task-owner
      ownerLeaseGeneration: positive-current-owner-generation
    # or
    owner: {observation: Unobserved}
  checks:
    observation: Observed
    items:
      - checkId: exact-objective-check-id
        state: pending | pass | fail
  barriers:
    observation: Observed
    items:
      - kind: run
        barrierId: exact-run-barrier-identity
        state: closed
      - kind: stage
        barrierId: exact-stage-barrier-identity
        stageId: exact-stored-stage-id
        state: closed
      - kind: task-request
        barrierId: exact-request-barrier-identity
        requestId: exact-task-request
        state: blocked | released | abandoned
  declaredWriteScopes:
    observation: Observed
    leases:
      - leaseId: exact-task-bound-write-lease
        generation: positive-current-lease-generation
        state: active | quarantined | released
        paths: [exact-canonical-path, ...]
  runTaskStates:
    observation: Observed
    counts:
      blocked: nonnegative-server-count
      ready: nonnegative-server-count
      active: nonnegative-server-count
      complete: nonnegative-server-count
      cancelled: nonnegative-server-count
      degraded: nonnegative-server-count
~~~

`objective`, `task.state` and `task.owner` come from the exact `tasks` row.
The owner is `Unobserved` when the task has no owner; Fabric never substitutes a
proposal, participant, team lead or provider session. The enclosing legacy
state and check-state fields remain byte-compatible. The codec requires the
workflow task state to equal the enclosing state and requires the enclosing
check state to equal the deterministic roll-up of the projected check rows.

`dependencies` enumerates the complete `task_dependencies` set for the task,
ordered by dependency task ID, and carries the exact current
`runs.dependency_revision` under which that complete graph is stored. An empty
array is an observed absence. The projection does not derive parentage from the
dependency graph, team ownership or task creation order. Fabric has no
task-parent record in this schema, so `parentTask` is explicitly and
permanently `Unobserved` in v1.

`coordinationRun` is the task's stored `tasks.run_id` joined to the owning
`runs.project_session_id`. `workstream` comes only from an exact direct root
binding in `workstreams.fabric_task_id` or an exact non-root chain through
`team_owned_tasks`, `workstream_custody.team_id` and its workstream. Duplicate
paths to the same workstream collapse by identity. Zero distinct rows yields
`Unobserved`, exactly one yields `Observed`, and more than one yields
`Unknown/MultipleWorkstreamBindings`; the projector never picks the first row.
`plan` is the latest immutable `run_plan_declarations` revision for that
coordination run. No declaration yields `Unobserved`. This is the current
run-plan binding, not a claim that the task was created by that plan.

`checks` enumerates all `task_objective_checks` rows for the task, ordered by
check ID. `barriers` enumerates both the run's complete stored run/stage
`barriers` set and exact task-request barriers joined through `task_requests`
and `task_request_barriers`. The discriminated kind preserves their different
scope and lifecycle; a run/stage closure is not relabelled as a task-specific
edge. Items are ordered by kind and identity. Empty arrays are observed
absences. Unknown check states, barrier kinds or barrier states fail closed.

`declaredWriteScopes` enumerates write leases explicitly linked to the task by
`task_obligation_bindings`, joined to `leases` and `write_scope_entries`.
Leases are ordered by lease ID and paths by canonical path. An empty array is an
observed absence. Authority source paths, worktrees, owners and overlapping
run-level leases are not write-scope declarations and are never substituted.
A dangling or contradictory binding fails closed.

`runTaskStates` counts every `tasks` row in the task's coordination run in the
same server read, independent of page cursor or limit. It is therefore the
server-scoped state total used to render current and remaining work; a Console
must not count one returned page. The codec requires the selected task's state
count to be positive and rejects unknown state keys or negative counts. These
counts are not a denominator, completion ratio, percentage or ETA.

Every collection is sorted, unique and bounded. Dependencies are limited to
1,024; checks and barriers to 256 each; task-bound write leases to 128 and paths
per lease to 256. Overflow returns `RESOURCE_EXHAUSTED` before an unencodable
result is emitted. Invalid stored identities, revisions, states, dangling
bindings or cross-run relationships return `RECOVERY_REQUIRED`.

`workflowRevision` is the positive `daemon_global_state.revision` captured in
the same SQLite read transaction as the task and all projected workflow rows.
It is a conservative staleness token. The codec requires it to equal the
operator result's `snapshotRevision`; no read advances either revision.

The feature applies only to `fabric.v1.operator-projection.view-page` work rows
and `fabric.v1.operator-projection.detail.read` task detail. When negotiated,
every returned work candidate carries `workflow`; missing or mixed presence
fails closed. When not negotiated, any `workflow` field fails closed as
unnegotiated. Unknown fields, observation arms, states and reasons are rejected.
Existing projection, task, run, check, barrier, lease and workstream shapes stay
byte-compatible.

