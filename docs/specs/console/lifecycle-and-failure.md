# Project Fabric Console lifecycle and failure UX

## Lifecycle and failure behaviour

Project session states are:

```text
draft -> awaiting_launch -> launching -> active -> quiescing
      -> awaiting_acceptance -> closed
```

Exceptional states include `launch_failed`, `launch_ambiguous`,
`reconciling`, `visibility_degraded`, `recovery_required`, `quarantined` and
`cancelled`.

These states and their revisions are Fabric-owned and persisted in the same
SQLite transaction domain as runs and leases. The Console derives no lifecycle
state from local UI files. Draft launch content may live in a project artifact,
but its Fabric session record owns status, identity and active revision.

Project-session membership explicitly lists every coordination run, delivery
run/workstream, lease, provider action, required message, artifact obligation
and gate. `quiescing` freezes new membership. `awaiting_acceptance` requires all
members terminal or explicitly abandoned with reason, all required messages and
artifacts reconciled, no active lease/provider action and every scoped barrier
closed. `closed` additionally requires final acceptance or an explicit
cancel/failure terminal path. Membership and transition checks occur in one
transaction; concurrent close/reopen attempts use compare-and-set revisions.

Required behaviour:

- Console crash: agents continue; the Console resumes from Fabric state.
- Herdr loss: managed sessions continue as `visibility_degraded`; no task state
  is inferred from missing panes.
- Interactive chair pane loss: freeze its delivery/turn lease and explicitly
  reattach or rotate.
- Ambiguous launch: reconcile by stable action ID; never blindly duplicate it.
- Stale artifact digest or base revision: invalidate the pending launch and
  return it to review.
- Chair loss: freeze/revoke the old chair generation, require a persisted
  handoff and explicit takeover; never silently promote a peer or bypass an
  active lease.
- Overlapping writer or unreconciled predecessor: quarantine the affected
  scope while unrelated work continues.
- Direct terminal intervention: journal it where detectable and reconcile the
  affected task revision.

### Connection diagnosis

Connection is a staged, typed projection owned by the bootstrap, daemon and
protocol clients. The Console presents these stages in order:

1. configuration discovery;
2. compatibility registry, runtime provider identity/interface conformance and
   Git wrapper provenance/source-span verification;
3. daemon election and process readiness;
4. socket or transport connection;
5. protocol handshake and authentication;
6. feature negotiation;
7. project discovery; and
8. project-session attachment.

Each stage reports its stable stage ID, `pending`, `running`, `passed`, `failed`,
`blocked`, `unavailable` or `not-applicable` state, safe error code and summary,
observed time, source/freshness/revision, relevant non-secret version or digest
references, owning component and bounded remediation. A failed stage is failed;
downstream stages that did not execute are blocked, not falsely failed. Unknown
or absent evidence remains unavailable. The Console shall preserve the first
causal failure while allowing later reconciliation evidence to append.

The top status bar and System detail keep four axes separate: transport
connection, projection freshness, project/session attachment and optional
integration health. `connected`, `stale`, `degraded`, `unavailable` and
`attached` are not interchangeable labels. A healthy GitHub or Herdr adapter
cannot make Fabric connected, and an optional adapter outage cannot make live
local Fabric state unavailable.

Retry is a typed, idempotent action against the failed stage or its owning
bootstrap operation. It binds the current configuration and compatibility
digests and reconciles an ambiguous prior action before another effect. A
diagnostic command may be shown as inert copyable text; the Console does not
embed a shell or execute arbitrary remediation. Supported fresh launch and
bootstrap sequencing landed through issue #139 and PR #173; the Console owns
the stage presentation, diagnostics and reviewed retry path.

On the first Console/Fabric read or command, the client library uses a
lock-safe, idempotent bootstrap protocol to attach to the existing machine-wide
daemon or spawn it if the socket is absent. The daemon becomes the sole shared
socket and transaction owner before project-session creation. A short bootstrap
lease prevents duplicate starts and is reconciled after crash.

The daemon may stop only after global authoritative state proves that no active
project session, run, lease, provider action, bootstrap lease or attached
operator client remains. An attached Console intentionally keeps it alive
because the operator is working; closing the final Console allows idle shutdown
when no work remains. Concurrent project close and client detach requests are
idempotent and cannot stop another project's work. These services are on-demand,
not login services.


## Skill and lifecycle alignment

Implementation shall review and update the portable harness skills whose
behaviour contracts change. The purpose is to express the adaptive SDLC once,
not to embed Console-specific workflow into every skill.

At minimum, the implementation review shall cover:

- `scope` and `grill-me`: conversational scoping, explicit decision context and
  digest-bound handoff;
- `implement`: automatic minor work, fresh substantial implementation sessions
  and adaptive rather than frozen implementation plans;
- `orchestrate`: chair-selected pairing, dynamic leaders/teams, model routing
  and isolated concurrent writers;
- `session`: fresh-session thresholds, checkpoint/handoff, compaction and safe
  provider-session retention;
- `deliver`: canonical `delivery-run` ownership and explicit project-session,
  coordination-run and workstream relationships;
- `work-map`: stable specification and work-item route links, while GitHub
  issues and Project Status retain all live work state;
- `release`: exact accepted-artifact and target-bound promotion gates;
- `retrospect`: user-attention, gate latency and unnecessary-interruption
  evidence feeding the next scope cycle.

Only affected skills shall change. Existing trigger boundaries remain explicit:
`scope` decides, `implement` builds, `orchestrate` forms teams, `deliver` owns
acceptance evidence and `release` promotes. A skill shall not import the
Console, parse its screen, require Herdr or write Fabric persistence directly.
The Console shall not parse `SKILL.md` prose or reproduce skill logic. Skills
and the Console communicate only through stable project artifacts and typed
Fabric lifecycle/events.

Every changed skill shall receive focused positive, negative and adjacent
trigger evaluations plus portability coverage proving that its workflow still
works when the Console, Herdr and GitHub adapters are absent. Shared lifecycle
schema belongs to the protocol/delivery contract, not to any UI package.

Issue `#141` does not by itself justify installing a framework-specific TUI
skill or adding another global catalogue entry. During this scope, the Console
specs own the product and terminal contract; `ui-ux-design` may supply a
companion design lens under the lifecycle owner. A future portable
`terminal-interface-design` technique skill may be proposed after the method
proves useful in at least two projects and passes catalogue-budget, overlap,
licence/provenance and trigger evaluations. Such a skill may own reusable
layout, input, resize, terminal-safety and PTY evidence methods, but shall not
contain Agent Fabric authority, lifecycle, projection or geometry policy.

## Explicit exclusions

- No mandatory GitHub account, issue tracker or project board.
- No browser/HTML Console in this implementation scope.
- No Pi-based operator shell or replacement main harness; Pi remains an
  optional generic model adapter.
- No second task orchestrator, database or canonical status store.
- No arbitrary shell terminal inside the Console.
- No full provider TUI for every short-lived worker.
- No silent session deletion, context clearing or automatic chair takeover.
- No unattended/login daemon. An attached operator client or active project
  work may keep the on-demand daemon alive; it shuts down when neither remains.


### Deferred continuity and routing decisions

The following decisions remain nonbinding: automatic context-pressure control and its
thresholds/hysteresis/maximum compactions; task-local Pareto or learned routing
and its quality floors/trial volumes/expiry policy; versioned provider deep-mode
IDs or cross-primary deep execution; a total provider-visible discovery-surface
ceiling; and global model, effort, hook or paid data-residency preferences.
OpenCode activation has landed and is governed by the live adapter
compatibility configuration and Agent Fabric operations runbook.
