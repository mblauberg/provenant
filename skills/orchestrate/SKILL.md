---
name: orchestrate
description: "Use when bounded fan-out, multi-agent research, cross-family review, parallel audits, Herdr control, or autonomous ready-issue implementation helps. Not for tiny work, coupled debugging, or run-until-STOP jobs; use diagnose or autopilot."
---

# Multi-agent orchestration

## Overview

Decompose -> waves -> reduce -> gate.

Claude subagent definitions are versioned separately from skill provider
metadata in `agents/`: `agy-reviewer.md`, `agy-stylist.md`, `codex-analyst.md`
and `codex-implementer.md`. `install-harness --platform claude` manages them
under `~/.claude/agents/`.

## Rules

- **Use parallel fan-out only after the decomposition/value gate passes.**
- Preflight dependencies/shared errors.
- **No concurrent shared-state writes.** Partition authorised writers into
  repository `.worktrees/<task-agent>`; otherwise use a serial applier.
- Parallel lanes stop ready-to-merge. The chair merges serially, refreshes the
  next branch from current main, then reruns checks and reviews after commit/tree
  changes.
- **Keep topology exact.** One chair; leaders settle recursive obligations.
  Handoff is a generation-bound operator action.
- **Answer-bearing external work uses a cooperative Fabric request/reply where
  available; Herdr only observes or sends fire-and-forget steering.** A normal
  Fabric inbox read claims delivery; acknowledge after durable processing to
  prevent redelivery. Claim/ack and a correlated reply evidence delivery, not
  provider liveness or completion. Without a round trip, record
  `FABRIC-ROUNDTRIP-UNAVAILABLE` and collect a named artifact.
- Record worker cwd; never assume repository.
- **Workers write full output to files**; return a digest/path.
- **A worker's report is a claim, not evidence.** Confirm claimed commits in
  `git log`, claimed counts against its own transcript, and re-run any failure
  it calls environmental. Lanes routinely report a clean result their own
  output contradicts.
- **Liveness: size proves nothing.** Compare CPU and session-log mtime; see
  `worker-liveness.md`. No growth keeps a live worker in the waiting path, but
  growth is not proof either: a wrapper can sit blocked long after its child
  exited. A detached task is not dead, and a live wrapper is not working.
  Observed PID exit is what fences terminal reporting, inspection and reuse.
- **Cross-family follows the HARNESS risk ladder.** Targeted lenses plus other
  primary; family separation is required only for the assurance claim being
  made, not for ordinary execution. Record terminal skips.
- **Configured workspace execution has broad provider choice.** An authorised
  chair may dispatch ordinary workspace work to any configured provider family;
  execution freedom is bounded by credential/auth-store exclusions, unrelated
  path containment, explicit denials, write/resource limits and external-action
  gates.
- **Objective checks outrank opinions. You own the final call.**
- Discover current model/tool options at runtime.

## When This Pays

Before parallel dispatch, require independent information or artefacts; stable
interfaces and dependencies; non-overlapping writes; independently checkable
return contracts; and expected information gain **greater than** coordination,
shared-state and tool-density cost.

If the gate fails, keep serial ownership with the chair or one specialist;
shared-error or tightly coupled work stays serial. Choose the smallest
passing topology.

## Adaptive Loop

1. Preflight authority/isolation/disclosure/receipts.
2. **Use native same-session subagents first.** Ordinary configured-provider
   CLI dispatch may use same-family routes. The current `cf_dispatch.sh`
   distinct-family requirement belongs to its assurance path; ordinary
   dispatch is owned by `scripts/dispatch_run.py` and may use same-family
   routes.
3. Dispatch parallel read/partitioned-write and serial shared-state waves;
   adapt leaders on evidence and keep one chair/stage owner.
4. Reduce to a claim/conflict map; verify the live tree before repair.
5. Add only informative waves: narrow, repair, verify, **cross-family broad
   review**, or **Document update wave**.
6. **Final gate:** no untriaged P0/P1, missing anchors, unresolved doc drift,
   unrecorded family status or user gate. Record `CROSS-FAMILY-NOT-RUN` when
   unavailable.

## Worker Contract

Every worker gets task class, route (`tier`, `model`, `effort`, route receipt),
identity, objective, authority, paths, output, checks, stop and budget;
validate payloads, never infer permission. See
[orchestration-contract.md](references/orchestration-contract.md).

## Autonomous-implementation mode

Pulls **accepted/ready** issues through `implement` unattended; merge stays
user-controlled, deferring to the nearest repository merge policy. Lower
authority than autopilot's run-until-STOP loop. See
[autonomous-implementation.md](references/autonomous-implementation.md).

## References

Load relevant [references](references/) only:
`trigger-boundary.md`, `routing-and-tiers.md`, `codex-subagents.md`,
`orchestration-contract.md`, `dynamic-workflows.md`, `paired-primary.md`,
`herdr-panes.md`, `layering-and-context.md`, `retrieval-and-tool-routing.md`,
`verification.md`, `cli-headless.md`,
`memory-scratchpad.md`, `evaluation-and-observability.md`, `domain-adaptation.md`,
`worker-liveness.md`, and `autonomous-implementation.md`. `scripts/` and
`evals/` hold helpers/guards;
`cf_dispatch.sh` is the direct provider-execution adapter. When Fabric
coordination is unavailable, record its result as a named degraded artifact;
it is not itself a Fabric round trip.

## Adapter-absent path

Without adapters, emit the skill-owned [portable kind](portable-workflow.v1.json).
It proves context only, not evidence, route state or ownership. Keep context
separate. Validate `accepted_artifact_identity`.
