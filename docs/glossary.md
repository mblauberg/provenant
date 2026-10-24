# Glossary

Terms used by `HARNESS.md`. Risk-tier criteria are listed there and compiled
into `provenant-records`.

- **Acceptance:** objective checks plus the review required by the risk tier establish that delivered work meets its approved criteria. The chair reports the outcome and the user may still reject it. Explicit user acceptance is required only for `terminal` work or when the approved scope asks for it. A project may name its landing gate as acceptance. Acceptance grants no release, deploy, publish or send authority.
- **User gate:** approval of specs and scope, one-way doors, risk-tier downgrades, unresolved acceptance criteria, destructive or irreversible actions, external communications or production promotion; also final acceptance for `terminal` work or when the approved scope asks for it.
- **Chair:** the harness the user started. It owns authority, user communication, run state, gates and final synthesis.
- **Lane:** one dispatched worker run (native subagent or Fabric dispatch) with its own prompt, route and output file.
- **Front door:** the skill that names the kind of work before acting: `scope`, `diagnose`, `session`, `deliver` or `implement`.
- **One-way door:** a decision or action that is hard or impossible to reverse, such as a published release or a destructive change. It needs user approval.
- **Receipt:** the recorded `adapter/model@effort` and outcome of an attempt. Provenance comes from the receipt, never from a model's self-report.
- **Envelope:** the user-approved authority bounds for a piece of work: paths, actions, disclosure, secrets, expiry and approver. Delegation only narrows it.
- **Stage owner:** the one agent accountable for a lifecycle stage at a time, even when other agents contribute.
