# Native orchestration and discovery surfaces

Status: Durable research reference

Evidence snapshot: [July 2026 continuity and routing evidence](evidence-snapshots/agent-continuity-routing-2026-07.md)

Normative owners: [HARNESS topology](../../HARNESS.md),
ownership and topology,
the [harness route/topology evidence boundary](../specs/harness/lifecycle.md#route-and-topology-evidence-boundary),
project sessions and chair

## Conclusions

- One coordination run has one accountable chair and one stage owner. Equal
  primary families do not imply concurrent bosses.
- Topology follows task structure. Sequential/shared-state work defaults to one
  owner; independent questions may fan out under bounded budget, stop
  conditions and non-overlapping writes.
- A stage has one decomposition owner: `single-owner`, `fabric-explicit` or an
  opaque `host-native` leg. Mirroring unidentifiable native children into a
  second Fabric plan creates double scheduling and confused cancellation.
- Native/deep execution remains inside the chair's task and authority. It may
  not own a human gate, become another chair or hold an unpartitioned shared
  writer scope.
- Discovery should expose only capability- and authority-correct tools. More
  description is not automatically better; selection evidence matters.
- Dispatch and batch receipts own actual route facts. Route/topology evaluation
  is task-local and opt-in; no universal digest or version-lock protocol is
  required.
- OSS harnesses are pattern sources, not new canonical state owners. Event
  journals, stable task identity, immutable Git approval, worktree preflight,
  safe-turn delivery and dynamic terminal layout reinforce existing Fabric
  contracts.

## Evidence

| System/paper class | Useful pattern | Local boundary |
|---|---|---|
| Agent Orchestrator, Symphony, Gas Town/Beads | Stable work identity, reconciliation and persistent worker/worktree fleets. | Fabric owns coordination and task metadata; the user/chair envelope owns authority. |
| OpenCode and Codex | Structured events and substantial terminal/native orchestration implementations. | No provider host becomes a second chair. |
| Open SWE | Immutable Git approval bound to exact source/destination state. | Git effects remain typed, revision-bound and separately authorised. |
| OpenHands, Cline/Kanban, agtx, Claude Squad, Goose, Aider, SWE-agent, Continue, Vibe Kanban, Overstory | Alternative worker, review and operator patterns. | Popularity/maintenance are volatile; no wholesale dependency adoption. |
| Google scaling-agent-systems research | Parallelism helps decomposable work and can hurt sequential/tool-dense work. | Every topology wave records its task-structure rationale. |
| Tool-description research captured in the snapshot | Richer descriptions can improve selection while increasing steps/regressing cases. | Keep concise schemas and test the rendered surface. |

The adopted invariant is:

```text
observe external facts -> commit durable facts -> derive projection/attention -> typed act
```

Pane output, process presence, idle time, artifact existence and PR state are
observations. They never prove completion, authority or delivery.

## Discovery-surface record

When discovery context matters, dispatch or batch receipts may reference
source-labelled host, version, profile, native-mode and rendered-surface facts.
That evidence is not policy: no ceiling or optimisation claim follows merely
from recording it.

Tools absent from the current principal, negotiated feature set or activated
adapter capability are absent from discovery rather than advertised as stubs.
Adding always-visible surface requires trigger/selection evidence and the
existing generated-registry checks.

## Unknowns

- Versioned cross-host native/deep-mode IDs and conformance semantics.
- Cross-primary invocation of another host's deep mode.
- Provider-profile discovery targets and hard total ceilings.
- Stable identity/cancellation bridges for native children.
- Measured benefit of repository-map or progressive tool-discovery mechanisms
  on this harness.

## Refresh triggers

Refresh after a host-native orchestration change, new child identity/cancel
contract, recurring double-fan-out or routing failure, material tool-surface
change, or OSS architecture/ownership/archival change. Re-check exact upstream
state before describing a project as maintained, archived, popular or
compatible.
