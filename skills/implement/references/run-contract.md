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

## Implementation detail

Writer tier follows task size. Migrations may need behaviour tests and
equivalence evidence. Use source grounding for version-sensitive external
interfaces and compatibility checks for migrations; see
[source grounding](source-grounding.md) and
[migration compatibility](migration-compatibility.md).

## Polish

Polish before review when the diff exceeds about 300 changed lines or
touches more than one crate or package (`review.polish`: `auto`, `always`
or `off`). A fresh writer lane applies refactor to the changed code only:
one responsibility per unit, typed states over flags, no unwrap or silent
fallback on production paths, no speculative traits or dead seams. Simple
first beats SOLID abstraction. Keep every fast check green and escalate
structural moves (new modules, public API or cross-crate moves) to the chair.

## Review and repair

Use code-review's finding contract for a contested finding; one re-audit adds
no gate. Respect the repair ceilings linked above. `review.rereview_scope`
and other `review.*` knobs in `provenant config show` adjust review steps.

For crucial work, consider tdd's optional fresh-context acceptance-test writer.
