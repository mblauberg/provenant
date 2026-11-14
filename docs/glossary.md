# Glossary

Terms used by `GLOBAL-AGENTS.md` and the skills. Risk-tier criteria are
compiled into `provenant-records`.

- **Acceptance:** checks and the review the risk requires have passed, so the chair accepts the work ([global rules](../GLOBAL-AGENTS.md)); it grants no external-action authority.
- **User gate:** a one-way door reserved to the user by the [global rules](../GLOBAL-AGENTS.md).
- **Chair:** the harness the user started. It owns authority, user communication, run state, gates and final synthesis.
- **Lane:** a worker assignment with its own authority and output.
- **Graph node and attempt:** see [ADR 0039](adr/0039-graph-of-templates.md#decision).
- **Work key:** the only name for graph work, `NAME/NODE#N`: graph, node and attempt number within the project, represented by [`work_key.rs#WorkKey`](../tools/front-door/crates/records/src/work_key.rs).
- **Front door:** the skill that names the kind of work before acting: `scope`, `diagnose`, `session` or `deliver`.
- **One-way door:** a decision or action that is hard or impossible to reverse, such as a published release or a destructive change. It needs user approval.
- **Receipt:** see [ADR 0005](adr/0005-lifecycle-kernel-extends-delivery.md#decision).
- **Envelope:** the user-approved authority bounds for a piece of work: paths, actions, disclosure, secrets, expiry and approver. Delegation only narrows it.
- **Stage owner:** the one agent accountable for a lifecycle stage at a time, even when other agents contribute.
