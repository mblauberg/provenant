# Cross-family review in a persistent mission

Use this reference for the mission's recovery handoff around an independently
routed review. `orchestrate` owns provider routing, topology, the review ladder,
reviewer selection, coordination, and result contracts. Do not create a second
review policy here.

All answer-bearing reviews run through an `orchestrate` wave and are recorded
in Fabric. The run owns assignments and outputs; a host-session checkpoint
may retain a pointer to the task and evidence needed to resume.

## Before dispatch

Record the review in the relevant Fabric task and run assignment before launch:

- work or decision ID and exact artifact digest or revision;
- authority and disclosed paths;
- review question and required evidence;
- expected result and bounded time or retry limit.

The author or decision-maker cannot certify their own work. Let `orchestrate`
determine which review legs are required for the current risk and which are
advisory.

## Capture

Keep the independent result and its route receipt in Fabric or the run-owned
output declared for that task. If the reviewed artifact has a separate owner,
use the path that owner specifies. A worker's claim that another reviewer
passed is not evidence; inspect the independent result and receipt.

Keep each attempted leg visible as `pass`, `failed`, `unavailable`, or
`skipped`, with actual lineage and reason. Never erase a failed attempt after a
retry or silently substitute a same-family result.

## Adjudicate and repair

Check findings against live artifacts and objective checks; do not majority-vote
model prose. For gate-class artifacts, ask the independent reviewer to rerun
the highest-risk negative or mutation check when required by the owning review
contract. Route confirmed correctness defects through the enclosing lifecycle.

A failed review is evidence, not permission for an unbounded loop. Follow the
retry limit and escalation path declared by the owning lifecycle.

## Recovery

If dispatch or capture is interrupted, run `provenant reconcile --id RUN`,
inspect the Fabric task and existing output, then resume the exact attempt with
`fabric_run`'s `resume` operation (`target: { attempt }`,
`expected_generation: 0`, `operation_id`, `prompt`). `expected_generation`
counts earlier continuations, so pass 0; it is not the attempt record's
`generation`. On `Conflict`, read `fabric_run` `status`: wait if the target is running, target its child (`parent_attempt`) if one exists, otherwise use a fresh `operation_id`. Preserve partial results. Retry only missing or failed work within its bound; do not
relaunch a still-active task because a session lost its context.

## Checklist

- Exact target and authority are recorded before dispatch.
- Author and certifier are independent.
- Fabric task and run output retain the result and route receipt.
- Actual lineage and non-pass attempts remain visible.
- Findings are checked against current evidence.
- Retry bound and escalation path come from the owning lifecycle.
- The task, reviewed artifact, and run output point to the same result.
