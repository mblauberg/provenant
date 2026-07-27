# Project Fabric Console sessions and chair

## Project sessions and accountable topology

The Console presents one selected project at a time. A trusted-project switcher
may change context, but v1 shall not aggregate a cross-project portfolio.

A Console project view may contain either topology:

1. **Coordinated session:** one Fabric coordination run and chair supervise
   multiple concurrent delivery runs or workstreams. Each workstream may have a
   lead and a bounded team, but it is not another chair authority.
2. **Independent sessions:** unrelated or deliberately isolated runs each have
   one chair and separate fabric session authority.

The project-scoped Console connection remains open in both topologies. When
exactly one attachable project session exists it may open a secondary session
client automatically. With zero or multiple attachable sessions it remains on
the project projection until the operator selects a stable session ID. Every
run projection, row summary, detail reference and detail carries that exact
session ID. Selecting a run opens only its secondary session client; returning
to the project selector closes only that secondary client.

The Console is a projection-only client. The project-session lifecycle is a
Fabric-owned protocol entity persisted by the daemon before its first run is
created. Each Fabric coordination run retains exactly one accountable chair.
In coordinated mode, every `.agent-run/<run>/RUN.json` records its parent
project session, coordination run and Fabric task/workstream owned by its lead;
the chair alone retains run-level authority and barrier accountability. In
independent mode, each coordination run has its own chair and no implicit
cross-run authority. Coordinated mode is the recommendation, not a hard
default.

Pressing `Start project session`, or launching an independent-mode run, is the
user starting that coordination chair. In coordinated mode, `Launch run`
creates a delivery run/workstream with a lead under the existing chair; it does
not create another chair. The Console submits the reviewed launch packet,
Fabric records and validates authority, and Herdr or a provider adapter performs
the external process action. The Console shall not autonomously invent a chair
or broaden its authority.

Parallel source writers shall use non-overlapping write scopes and separate
repository-owned `.worktrees/<task-agent>` worktrees when the project/session
launch packet grants worktree creation. This spec's user acceptance approves
that capability as an available envelope field; each active project/session
still records its chosen grant. Without it, the chair shall serialise
application or exchange immutable patch artifacts. Read-only workers do not
need worktrees. The fabric shall reject overlapping active writer leases before
launch.


## Chair autonomy

Within the project/session authority envelope, the chair may without user
approval:

- work solo or enable/disable paired-primary collaboration;
- add, retire, replace or reroute agents;
- appoint run leads and form bounded leader teams;
- adjust ordering, parallelism and review strategy;
- create approved branches and worktrees;
- allocate routine Git work;
- increase or reduce resource use within configured ceilings;
- replan after failures, discoveries or changed estimates.

Resource authority is persisted hierarchically from project to project session,
coordination run, team and agent across concurrency, provider turns, tokens/cost
where reported, and any project-defined dimension. Child limits only narrow the
parent. Admission atomically reserves aggregate capacity before dispatch and
releases or reconciles it after terminal/ambiguous effects and restart. Unknown
usage is shown as unknown and fails closed for new provider turns when the
remaining parent capacity cannot be proven; already-authorised in-flight work
may reach its bounded terminal state. The Console shows used, reserved,
remaining and unknown capacity at each level.

Paired programming is chair-selectable within authority and is not a default
requirement. The user may pin or prohibit a pair, chair family, model family,
visibility mode or resource ceiling at project, session, run or task level.
Preference precedence is `task > run > session > project > harness`; lower
levels may narrow authority automatically. Resolution is
intersection/minimum/earliest-expiry only; no lower layer overrides platform
policy, explicit user authority or a mandatory safety gate.

User acceptance of this spec is the constitutional decision to amend the
harness so that `paired-primary` is chair-selectable inside an approved
project/session authority envelope rather than separately user-opt-in for each
use. Risk-required other-primary review remains distinct from live paired
programming.

## Topology waves and continuity

The durable conclusions in the
[July 2026 continuity and routing snapshot](../../research/evidence-snapshots/agent-continuity-routing-2026-07.md)
specialise this Console behaviour; they do not create another specification or
state owner.

Each coordination run still has exactly one accountable chair. Before a
topology wave starts, Runs consumes only the exact Agent Fabric contract
`topologyWavePlanV1` plus `topologyWavePlanCurrentV1` projection. It shows
run/task/wave/revision/predecessor, dependency and decomposability evidence,
topology mode, current chair, every stage owner and write partition, contention,
budget, stop conditions, existing authority/policy refs, state, rationale ref
and digest. Missing or read-derived stale plans remain visible and cannot be
presented as ready to start. Dynamic team changes append a new plan revision and
rationale under the existing authority/policy; the Console never edits an old
plan, creates authority or chooses a team. Agent count is never shown as a
quality or progress measure. A host-native leg is one bounded task beneath the
chair unless a contract-tested native-child identity bridge exists.

The [fresh implementation context](intake-and-continuation.md#fresh-implementation-context) rule remains binding: substantial or larger implementation starts in a
fresh provider context from the accepted digest-bound scope/handoff. Resuming
the same full history is crash recovery, not fresh implementation or policy
rotation. Parent compact/rotate never implies child completion or continuity,
and no deep/native workflow may become a second chair, own a user gate or hold
an unpartitioned shared write scope.
