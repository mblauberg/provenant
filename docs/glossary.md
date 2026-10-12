# Glossary

Terms used by `HARNESS.md`. Risk-tier criteria live in `config/risk-policy.json`.

- **Chair:** the harness the user started. It owns authority, user communication, run state, gates and final synthesis.
- **Lane:** one dispatched worker run (native subagent or Fabric dispatch) with its own prompt, route and output file.
- **Front door:** the skill that names the kind of work before acting: `scope`, `diagnose`, `session`, `deliver` or `implement`.
- **One-way door:** a decision or action that is hard or impossible to reverse, such as a published release or a destructive change. It needs user approval.
- **Receipt:** the recorded `adapter/model@effort` and outcome of an attempt. Provenance comes from the receipt, never from a model's self-report.
- **Envelope:** the user-approved authority bounds for a piece of work: paths, actions, disclosure, secrets, expiry and approver. Delegation only narrows it.
- **Stage owner:** the one agent accountable for a lifecycle stage at a time, even when other agents contribute.
