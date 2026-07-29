# Agent topology projection

Fabric projects agent topology only from records it owns. Result-shape feature
`agent-topology-projection.v1` adds one closed `topology` field to every agent
summary and agent detail returned by the operator-projection v2 operations.
It changes no baseline operation and no unnegotiated result.

~~~yaml
topology:
  topologyRevision: positive-daemon-global-revision
  teams:
    observation: Observed
    memberships:
      - teamId: exact-team-membership
        teamGeneration: positive-current-team-generation
        relationship: Lead | Member
        leadAgentId: exact-current-team-leader
  supervisor:
    observation: Observed
    agentId: exact-Fabric-registration-parent
  # or
  supervisor: {observation: Unobserved}
  currentTask:
    observation: Observed
    taskId: exact-active-claimed-task
    taskRevision: positive-current-task-revision
    ownerLeaseGeneration: positive-current-owner-generation
  # or
  currentTask: {observation: Unobserved}
  # or
  currentTask:
    observation: Unknown
    reason: MultipleActiveClaims
  nativeChildren: {observation: Unobserved}
~~~

`teams.observation` is always `Observed`: Fabric can enumerate the complete
`team_members` set for the projected run, so an empty `memberships` array is an
observed absence. Each membership joins the current `teams` row. `Lead` means
the agent equals that row's current `leader_agent_id`; otherwise it is
`Member`. Memberships are ordered by team ID and are never collapsed to one
inferred team. The current lead, team generation and relationship come from
the same row.

`supervisor` names only `agents.parent_agent_id`, the Fabric registration-parent
assignment. A null parent is `Unobserved`; the projector never substitutes the
chair, team lead, authority owner, provider parent or pane parent. The field
does not claim that a provider exposes a native reporting relationship.
`nativeChildren` is therefore explicitly and permanently `Unobserved` in v1.

`currentTask` is derived strictly from `tasks` rows in the same run whose
`state` is `active` and whose `owner_agent_id` is the projected agent. Exactly
one row yields `Observed`, including its task revision and owner-lease
generation as claim evidence. Zero rows yields `Unobserved`. More than one
yields `Unknown/MultipleActiveClaims`; the projector never chooses by task ID,
proposal, eligibility, participation, team ownership, message, process or pane.
A terminal task retains historical ownership but is not a current task.

`topologyRevision` is the positive `daemon_global_state.revision` captured in
the same read transaction as all topology rows. It is a conservative staleness
token: unrelated Fabric mutations may advance it, but a consumer must never
treat a changed value as current-equivalent. Fabric topology mutations occur
through transactions that also advance the global revision through owned-row
or event triggers. No read advances the revision.

The feature applies only to `fabric.v1.operator-projection.view-page` agent
rows and `fabric.v1.operator-projection.detail.read` agent detail. When it is
negotiated, every returned agent candidate carries `topology`; missing or mixed
presence fails closed. When it is not negotiated, any `topology` field fails
closed as unnegotiated. Unknown fields, observation arms, relationship values
and reasons are rejected. Existing projection, team, task and agent shapes stay
byte-compatible.

The `provider_lifecycle_intents` drop from issue #381 is included in this
release.
