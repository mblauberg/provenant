# Delivery contract

Delivery acceptance is met by objective checks and tier-required review; the
chair reports the outcome and the user may still reject it. Explicit user
acceptance is required only for `terminal` work or when the approved scope
asks for it. A project may name its landing gate as acceptance. Record the
acceptance basis separately from user approval. Acceptance never supplies
release or cleanup authority.

## Receipt

The receipt is a revisioned Rust record owned by `provenant-workflow` and
`provenant-records`, keyed by a typed `DeliveryId`.

```sh
provenant workflow receipt show <id>
provenant workflow receipt save <id> --expected-revision N --request <file|->
provenant workflow review --request <file|->
```

`save` is compare-and-swap on the revision. The first save is pending; an
accepted decision needs an existing receipt. `review` binds each reviewer
attempt from run custody (observed reviewer family, delivery match, output
reference, PR head equal to the attempt head) to a chair-supplied claim (lens,
verdict, findings, tier) and checks coverage against the risk tier. Accepting
a decision runs the same coverage check and is refused with `REVIEW_INCOMPLETE`
unless it passes. Waiving a cross-family leg needs an unexpired blocker
reference; the CLI records it but does not verify it.

## Repair budget

The ceiling is two repair cycles for `routine`, four for `substantial` and
five for `crucial` or `terminal`. It is a guardrail, not a target: stop and
return evidence to the user or `scope` when it is exceeded. A new requirement,
authority expansion or one-way-door decision returns to `scope`; it is not a
repair.

## Evidence

For a pull-request delivery the pull request is the evidence: exact-head CI,
the required review and the repository merge. Keep raw operational artifacts
local and ignored; project durable facts into tracked docs or the PR.
Stochastic assurance links a passing `evaluate` receipt. Crucial software and
agent-product work needs matching passing security checks for the changed
surface. Observation names window, signals, thresholds, owner, containment,
privacy and close condition.
