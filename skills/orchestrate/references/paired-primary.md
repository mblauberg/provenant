# Paired-primary operating contract

Paired mode lets Claude Code and Codex work continuously as equal-primary
capabilities without creating two competing authorities. It is an orchestration
topology, not a separate skill.

## Trigger

Use it when the user asks for a Claude/Codex pair, or when the chair determines
that substantial, multi-stage, low-oracle work benefits from continuous
cross-family challenge within the approved project/session authority envelope.
User policy may pin or prohibit pairing.
Skip it for routine edits, tightly coupled debugging, latency-sensitive work,
uncleared data, or work whose only shared write surface cannot be serialised.

## Roles

- **Session chair:** the harness the user started. It alone talks to the user,
  allocates authority, updates the run receipt, owns gates and sends the final
  response. Chair ownership is accountability, not epistemic superiority.
- **Stage owner:** one primary with sole authority for the current stage and its
  assigned source/artifact scopes. Ownership may rotate at a stage barrier.
- **Peer:** the other primary. It researches, challenges or reviews with
  source-read-only or namespaced artifact authority unless assigned a disjoint
  implementation scope.
- **Serial applier:** the sole writer for a shared source surface. Use patch-only
  workers when write scopes cannot be partitioned.

Each primary may delegate to native subagents inside its assigned scope.
Delegation never widens authority. Co-authorship or decision influence over a
surface disqualifies that participant from certifying its independent review;
use a fresh-context reviewer and record independence explicitly.

## Durable communication

Follow the Fabric/Herdr transport boundary in [herdr-panes.md](herdr-panes.md).
The run directory owns durable artifacts and lifecycle evidence.
Never make pane scrollback the only record.
Messages are delta-only and normally under 4 KiB: `stage | revision | artifact
path | sha256 | requested action | blocker`. Long context belongs in immutable
namespaced artifacts. Before either primary compacts or hands off, it closes a
stage checkpoint and records the exact revision in the named artifact.

Before dispatch, the chair records an assignment envelope and correlated
request through Fabric:

```text
task_id | stage | chair | owner | peer | base_revision
source_write_scope | artifact_scope | prohibited_actions
expected_output | objective_checks | human_gates | expected response time
```

The peer explicitly reads its inbox, which atomically claims delivery, then
acknowledges the claim after durable processing to prevent redelivery. It returns
supported claims, challenges, evidence paths, unresolved questions and its
artifact path through a correlated Fabric reply. Claim/ack and a reply prove
only that messages were delivered and related; they do not prove provider
liveness, task completion or result validity. The chair verifies the named
artifact and objective checks before rotating ownership. An expected response
time is a chair-owned run-artifact expectation, not a Fabric deadline or
callback.
Each stage ledger records writer actors and safe relative paths; overlapping
cross-family writer scopes fail the machine gate.

Use namespaced immutable messages (`pair/claude/`, `pair/codex/`) when a run
needs durable peer exchange. Only the chair mutates shared pair state.

## Decision and failure protocol

- Fact dispute: run the narrowest falsifying check.
- Reversible engineering dispute: chair adjudicates from evidence and records
  the losing case.
- Author/reviewer dispute: one falsification round, then one fresh-context or
  stronger targeted/adversarial pressure pass.
- One-way-door, scope-changing or still-deadlocked decision: user gate.
- Peer unavailable before start: solo mode plus `PAIR-NOT-RUN: <reason>`.
- Peer lost mid-stage: preserve partials, mark `PAIR-DEGRADED`, and reassign only
  if authority and review independence remain valid.
- Chair loss: persist a named handoff. A replacement chair must explicitly
  assume ownership and verify the referenced artifacts; Fabric does not
  transfer chair authority or provider custody automatically.

Use `skills/orchestrate/scripts/lease.py` only where its local lease mechanism
is explicitly selected for an autonomous loop. It serialises local ownership;
it is not Fabric state, provider custody, a liveness signal or an automatic
takeover mechanism.

Autonomous labs have exactly one active loop driver/orchestrator lease. The
other primary owns bounded stages, workflows or audits, never a competing loop.

## Default rotation

| Stage | Owner | Other primary |
|---|---|---|
| Scope/grill | chair | evidence research and adversarial spec audit |
| Design | assigned owner | independent option critique |
| Implementation | partitioned owner or serial applier | contract/regression pressure |
| Verification | non-author where practical | trajectory evidence supplier |
| Repair | implementation owner | original non-author re-verifies |
| Final synthesis | chair | fresh-context final challenge |
| Acceptance | user | neither primary substitutes |
