---
name: release
description: "Use before any deploy, publish, send or rollout."
---

# Release

Promotion is a one-way door and never automatic. Passing CI, delivery
acceptance and broad session authority do not authorise it. The owner's grant
binds this artefact, this action, this target and an expiry. The repository's
release runbook owns the mechanics; this skill owns the gate.

## Before asking

State in the request:

- the exact artefact (revision or digest) and that it equals the accepted one;
- the action and the target;
- the disclosure and data impact, including anything public that cannot be
  recalled;
- the reversal plan with an owner and a time bound, or that it is irreversible;
- what proves success, where it is observed and for how long.

Post it through `provenant ask` with a recommended default and carry on with
independent work while it waits.

## Promote and observe

1. One serial operator runs only the authorised operation. Reviewers never
   issue external actions.
2. Record the operation, actor, UTC interval and result, never secrets.
3. Prove the outcome at the target against the measure declared beforehand; a
   command that returned success is not the outcome.
4. On a stop condition, contain exposure and run the approved reversal, or
   escalate. Do not improvise an irreversible recovery.
5. Close after the observation window. Incidents go to `diagnose`, process
   lessons to `improve`.

Report the end state as `complete`, `reversed` or `failed`, with evidence and
follow-up owners.
