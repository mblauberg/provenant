# Multi-agent review topology

Use for substantial or higher changes, comprehensive reviews, and any surface
where one reviewer would carry too many unrelated concerns. Routine local
changes need only the chair plus objective checks under the
[`HARNESS.md` Review ladder](../../../HARNESS.md#review-ladder).

## Select lenses

Correctness/spec alignment is always represented. Add only lenses activated by
the dependency cone and risk profile:

| Lens | Trigger examples |
|---|---|
| Security/privacy | trust boundary, auth, secrets, user data, parser/input |
| Data/concurrency | persistence, migration, transactions, queues, retries, async |
| Performance/reliability | hot path, fan-out, caching, resource use, timeouts |
| Tests/verification | behaviour change, weak oracle, regression, flaky path |
| Architecture/structure | refactor, new abstraction, ownership or state-machine change |
| Readability/maintainability | public API, complex control flow, operational handoff |
| UX/accessibility | user-facing flow, copy, keyboard/screen-reader/responsive behaviour |
| Operations/release | config, telemetry, migration order, rollback, mixed versions |

Size the review to the tier and dependency cone. One independent reviewer is
sufficient at substantial risk; a broader council may use 2–4. Give each reviewer
a primary lens and, when useful, an explicit overlap on the highest-risk
invariant. Domain/project skills supply specialised rubrics.

## Three-stage council

1. **Independent pass:** reviewers work blind and write namespaced artifacts.
   Brief them with the target and rubric, never the chair's hypothesis or
   expected verdict. Each reads the rubric and reports `incomplete` if it is
   unreadable. They return supported findings under `finding-contract.md`.
2. **Blind challenge:** a fresh reviewer receives anonymised, randomised claim
   packets and labels each supported/contradicted/needs-evidence with a
   falsification step. Do not rank prose or majority-vote.
3. **Reduction:** the chair or fresh-context reducer deduplicates, checks source
   anchors, runs objective tests, preserves unresolved dissent and emits the
   final severity-ordered findings.

The implementer, stage owner or co-author cannot certify the reviewed surface.
The `HARNESS.md` Review ladder sets family and lens coverage, including the
recorded-blocker fallback when cross-family review is unavailable. Count passing
independent reviews by family and lens, regardless of receipt role. A failed leg
provides neither coverage nor a blocker record. Review repair output again
because it is a new surface.

Each lens artifact records reviewer/family/model, source and artifact authority,
scope, base revision, checks, findings, rejected hypotheses and status. The
reducer records which lenses ran or failed; missing lanes never disappear.
