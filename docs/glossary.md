# Glossary

Terms used by `GLOBAL-AGENTS.md` and the skills. Risk-tier criteria are
compiled into `provenant-records`.

- **Acceptance:** checks and the review the risk requires have passed, so the chair accepts the work ([global rules](../GLOBAL-AGENTS.md)); it grants no external-action authority.
- **User gate:** a one-way door reserved to the user by the [global rules](../GLOBAL-AGENTS.md).
- **Chair:** the harness the user started. It owns authority, user communication, run state, gates and final synthesis.
- **Lane:** a worker assignment with its own authority and output.
- **Graph node:** a scheduled unit of graph work, which may be an agent, action or composite.
- **Work key:** the only name for graph work, `NAME/NODE#N`: graph, node and attempt number within the project, represented by [`work_key.rs#WorkKey`](../tools/front-door/crates/records/src/work_key.rs).
- **Attempt:** one execution under custody, named by its work key and represented by [`run.rs#AttemptRecord`](../tools/front-door/crates/records/src/run.rs). Each launch admits exactly one attempt; retries and continuations create new attempts with lineage.
- **Front door:** the skill that names the kind of work before acting: `scope`, `diagnose`, `session` or `deliver`.
- **One-way door:** a decision or action that is hard or impossible to reverse, such as a published release or a destructive change. It needs user approval.
- **Receipt:** a typed record of an outcome. Attempt receipts record route and outcome; delivery receipts record the delivery decision. Provenance comes from records, never from a model's self-report.
- **Envelope:** the user-approved authority bounds for a piece of work: paths, actions, disclosure, secrets, expiry and approver. Delegation only narrows it.
- **Stage owner:** the one agent accountable for a lifecycle stage at a time, even when other agents contribute.
