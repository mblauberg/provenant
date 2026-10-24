# Software delivery contract

Delivery acceptance is met by objective checks and tier-required review; the
chair reports the outcome and the user may still reject it. Explicit user
acceptance is required only for `terminal` work or when the approved scope
asks for it. A project may name its landing gate as acceptance.

The approved scope supplies acceptance criteria, risk and authority. Link
deterministic tests and security checks before independent review.
Stochastic or judgement-bearing behaviour also links an `evaluate` receipt.

Record the receipt and bind reviews with `provenant workflow receipt save` and
`provenant workflow review`; the [`deliver` contract](../../deliver/references/contract.md)
owns the commands and the repair ceilings (2 routine, 4 substantial, 5
crucial or terminal) and the return-to-`scope` rule.

## Evidence

The pull request is the evidence: exact-head CI, required review and the
repository merge. Keep raw run artifacts local and ignored; never force-track
them. Project durable tested-tree facts, review verdicts and pending gates into
tracked docs or the PR. Production promotion requires separate user authority.
