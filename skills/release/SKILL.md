---
name: release
description: "Use for owner-authorised promotion of an accepted artifact: deploy, publish, send, roll out and observe. Not for building it (implement), accepting non-software work (deliver) or drafting (the writing skills)."
---

# Release

Promotion is a one-way door and is never automatic. Passing CI, delivery
acceptance and broad session authority do not authorise it. The owner's grant
must bind this artefact, this action, this target and an expiry.

## Before asking

Confirm and state in the request:

- the exact artefact (git revision or digest) equals the accepted one;
- the action (`deploy`, `publish`, `send`, `activate`) and the target;
- the disclosure and data impact, including anything public that cannot be
  recalled once copied;
- the reversal plan with an owner and a time bound, or an explicit statement
  that the action is irreversible;
- what proves success, where it will be observed, and for how long.

Put the request, with a recommended default, through `provenant ask --file
Q.md --session S`. Independent work continues while it waits.

## Commands

- Provenant itself: `provenant check release --base BASE --head HEAD` lists
  blocking findings for the range; `scripts/collate-release-notes --check`
  covers the notes; after the release lands, `provenant install --source DIR
  --dry-run` plans the install without changing anything.
- Other projects: follow their release runbook. This skill owns the gate, the
  runbook owns the mechanics.

## Promote and observe

1. One serial operator executes only the authorised command or named user
   operation. Reviewers never issue external actions.
2. Record the operation, the actor, the UTC interval and the result. Never
   record secrets.
3. Prove the outcome at the target against the success measure declared before
   the action. An operation that merely returned success is not the outcome.
4. On a stop condition, contain exposure and run the approved reversal, or
   escalate. Do not improvise an irreversible recovery.
5. Close only after the observation window passes. Route incidents to
   `diagnose` and process lessons to `retrospect`.

Report the end state plainly as `complete`, `reversed` or `failed`, with its
evidence and follow-up owners.
