# Multi-agent review topology

Use for substantial or higher changes, comprehensive reviews, and any surface
where one reviewer would carry too many unrelated concerns. Routine local
changes may use one reviewer plus objective checks when the extra coordination
would add no information.

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

Prefer 2–4 independent reviewers, not one agent per checklist item. Give every
reviewer one primary lens and one explicit overlap on the highest-risk
invariant. Domain/project skills supply specialised rubrics.

## Three-stage council

1. **Independent pass:** reviewers work blind and write namespaced artifacts.
   They return only supported findings under `finding-contract.md`.
2. **Blind challenge:** a fresh reviewer receives anonymised, randomised claim
   packets and labels each supported/contradicted/needs-evidence with a
   falsification step. Do not rank prose or majority-vote.
3. **Reduction:** the chair or fresh-context reducer deduplicates, checks source
   anchors, runs objective tests, preserves unresolved dissent and emits the
   final severity-ordered findings.

The implementer, stage owner or co-author cannot certify the reviewed surface.
Other-primary and distinct-family lanes follow `HARNESS.md`. Targeted lenses and
other-primary coverage are required from substantial risk up; distinct-family
review is used when available. Terminal work adds stronger targeted and
adversarial pressure, and a skipped distinct-family leg records its reason.
Missing other-primary coverage without a recorded skip and same-family fallback
remains a blocking omission. Review repair output
again because it is a new surface.

Each lens artifact records reviewer/family/model, source and artifact authority,
scope, base revision, checks, findings, rejected hypotheses and status. The
reducer records which lenses ran or failed; missing lanes never disappear.
