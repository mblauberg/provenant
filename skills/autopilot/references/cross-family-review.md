# Cross-family review in a persistent mission

Use this reference only for the durable mission-state seam around an
independently routed review. `orchestrate` owns provider routing, topology,
the HARNESS risk ladder, reviewer selection, coordination,
degradation and result contracts. Do not copy those policies here.

All answer-bearing cross-family work runs under an `orchestrate` wave and is
recorded in Fabric so the mission state is visible across providers. This
mission adds persistence and recovery; it never invents a second routing
policy.

## Before dispatch

Flip the item's `QUEUE.md` row to `LEASED` and record the bounded wave in
`STATE.md` before launch:

- work/decision ID and exact artifact digest or revision;
- authority and disclosed paths;
- review question and required evidence;
- expected result and route-receipt paths;
- expiry, timeout and recovery instruction.

The author/decider cannot certify its own surface. Let `orchestrate` determine
which review legs load-bear for the current risk and which are advisory.

## Capture

Persist the independent provider result verbatim at the declared review path
under the mission directory (e.g. `.agent-run/<mission-id>/reviews/<id>-<family>.md`),
plus its Fabric route/result receipt. If the reviewed artifact is itself a
delegated decision, its `adr`/review sidecar path is whatever the owning
`implement`/`deliver` lifecycle declares — this skill does not invent a second
one. Then link both from `QUEUE.md`'s row and `STATE.md`. A worker's prose
claim that another reviewer passed has no authority; the independent result
and receipt are the source.

Every attempted leg remains visible as `pass`, `failed`, `unavailable` or
`skipped` with actual lineage and reason. Never erase a failed attempt after a
retry or silently substitute a same-family result.

## Adjudicate and repair

The chair checks findings against live artifacts and objective oracles; it does
not majority-vote model prose. For gate-class artifacts, ask the independent
reviewer to rerun the highest-risk negative or mutation oracle. Route confirmed
correctness defects through the enclosing lifecycle.

See [recovery-and-cadence.md](recovery-and-cadence.md) §4–§5 for the
PIERCE/owed distinction and the bounded repair ceiling. A failed review is
evidence, not permission for an unbounded loop.

## Recovery

If dispatch or capture is interrupted, preserve the partial result and
receipt, leave the `QUEUE.md` row `LEASED`, and make the next action explicit
in `STATE.md`. `RECONCILE` decides whether to attach, retry within the
declared bound, or escalate. Never relaunch merely because the task tracker
lost sight of a still-live provider run.

## Checklist

- Exact target and authority recorded before launch.
- Author and certifier are independent.
- Fabric result and route receipt persisted separately from the build report.
- Actual family/model/adapter and non-pass attempts retained.
- High-risk finding checked against live evidence.
- Repair bound and escalation path declared.
- `QUEUE.md`, the reviewed artifact and `STATE.md` point to the same result.
