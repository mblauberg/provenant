# Glossary

Terms used by `GLOBAL-AGENTS.md` and the skills. Code, specs and `provenant help`
define everything else.

- **Acceptance:** checks and the review the risk requires have passed, so the chair accepts the work ([global rules](../GLOBAL-AGENTS.md)); it grants no external-action authority.
- **User gate:** a one-way door reserved to the user by the [global rules](../GLOBAL-AGENTS.md).
- **One-way door:** a decision or action that is hard or impossible to reverse, such as a published release or a destructive change. It needs user approval.
- **Chair:** the harness the user started. It owns authority, user communication, run state, gates and final synthesis.
- **Lane:** a worker assignment with its own authority and output.
- **Graph node and attempt:** see [ADR 0039](adr/0039-graph-of-templates.md#decision).
- **Work key:** the only name for graph work, `NAME/NODE#N`: graph, node and attempt number within the project, represented by [`work_key.rs#WorkKey`](../tools/front-door/crates/records/src/work_key.rs).
- **Receipt:** see [ADR 0005](adr/0005-lifecycle-kernel-extends-delivery.md#decision).
- **Authority envelope:** the user-approved authority bounds for a piece of work: paths, actions, disclosure, secrets, expiry and approver. Delegation only narrows it. Not the JSON result envelope of the CLI ([CLI spec](specs/cli.md#one-bounded-envelope-clipped-with-a-warning)).
