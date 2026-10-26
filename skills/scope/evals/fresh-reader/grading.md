# Grader-only rubric

Do not send this file, scores or other arms' output to readers. These notes
suggest distinctions; they do not prescribe a gap count or an answer.

A **genuine gap** is missing or contradictory information that would change
scope, observable behaviour, authority or acceptance, and cannot reasonably be
settled as an ordinary engineering choice. Cite the spec passage or absence
and the concrete clarification needed. A **noise** item asks for something the
spec already says, expands its scope, proposes an optional preference or claims
a normal implementation choice is blocking. Use **contested** pending
adjudication where the distinction is unclear. A response finding no gaps can
be correct. Deduplicate repeated questions; record missed explicit constraints
separately.

| Spec | Calibration |
|---|---|
| #761 | Five edits, excluded neighbouring deletions and docs-only test boundaries are explicit. Asking to delete the phrase tests here or require a new wording assertion misses the scope. Assess any claimed gap against those details. |
| #800 | Preserving every unique requirement is explicit. Assess each question about the destination and the editing approach for whether it changes scope or acceptance; do not treat every editing choice as missing design. |
| #1564 | Recording compiled grants in launch evidence and exposing them in lanes JSON is explicit. The record shape and proof of effective grants are terse; assess concrete questions about what must be observable. Naming a JSON field can be an engineering choice, not automatically a genuine gap. |

Compare classifications across arms only after each output has been graded
blind. Report the supporting passages and adjudication for candidate-only gaps.
