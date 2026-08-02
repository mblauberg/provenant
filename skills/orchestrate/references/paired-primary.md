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
stage checkpoint and the peer acknowledges the exact generation/revision.

Before dispatch, the chair records an assignment envelope and correlated
request through Fabric:

```text
task_id | stage | chair | owner | peer | base_revision
source_write_scope | artifact_scope | prohibited_actions
expected_output | objective_checks | human_gates | deadline
```

The peer acknowledges the exact delivery, then returns supported claims,
challenges, evidence paths, unresolved questions and its artifact path through
the correlated Fabric reply. The owner returns artifact paths, scoped diff/hash,
checks and blockers. The chair closes the barrier before rotating ownership.
Baton transfer requires a completed prior stage, acknowledgement, result
revision/hash and no unowned in-flight worker.
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
- Chair loss: persist a handoff. Takeover needs an explicit lease-generation
  transition; never silently promote the peer. A retained launched chair uses
  Fabric's typed live-handoff custody and exact provider/session generations;
  the local orchestration-lease helper is not a substitute.

Use `skills/orchestrate/scripts/lease.py` for atomic acquire/renew/transfer/
release of the chair or autonomous-loop lease. A stale or competing holder fails
closed, and `--expected-generation` binds any action to an exact generation when
the caller supplies it. Only its `takeover` action requires that generation, and
it is deliberately expired-lease-only. Active-chair loss first needs Fabric
freeze/revocation plus a generation-bound recovery proof; never remove the
active-lease guard to force promotion.

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
