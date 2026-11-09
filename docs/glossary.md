# Glossary

Terms used by `HARNESS.md`. Risk-tier criteria are listed there and compiled
into `provenant-records`.

- **Acceptance:** the delivery criterion owned by the [constitution](../HARNESS.md#lifecycle-and-user-gates); it grants no external-action authority.
- **User gate:** a decision reserved to the user by the [constitution](../HARNESS.md#lifecycle-and-user-gates).
- **Chair:** the harness the user started. It owns authority, user communication, run state, gates and final synthesis.
- **Lane:** a worker assignment with its own authority and output.
- **Graph node:** a scheduled unit of graph work, which may be an agent, action or composite.
- **Run:** a durable execution identity represented by [`run.rs#RunRecord`](../tools/front-door/crates/records/src/run.rs).
- **Attempt:** one execution under custody, represented by [`run.rs#AttemptRecord`](../tools/front-door/crates/records/src/run.rs). Retries and continuations create new attempts with lineage.
- **Front door:** the skill that names the kind of work before acting: `scope`, `diagnose`, `session`, `deliver` or `implement`.
- **One-way door:** a decision or action that is hard or impossible to reverse, such as a published release or a destructive change. It needs user approval.
- **Receipt:** a typed record of an outcome. Attempt receipts record route and outcome; delivery receipts record the delivery decision. Provenance comes from records, never from a model's self-report.
- **Envelope:** the user-approved authority bounds for a piece of work: paths, actions, disclosure, secrets, expiry and approver. Delegation only narrows it.
- **Stage owner:** the one agent accountable for a lifecycle stage at a time, even when other agents contribute.
