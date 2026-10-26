# Advisory fresh-reader check

Use for a non-trivial spec whose readiness is uncertain. Skip trivial items.
This is advice, with no automatic failure or new approval step. The chair
answers substantive questions in the spec before approval and records why any
noise was dismissed.

Dispatch through `orchestrate` (native first) with the spec text inline as the
only context; instruct no file reads. Do not send the parent deliberation,
implementation, proposed answers, reviewer notes or a held-back check. The
reader must mark what it cannot infer. Links in the spec are not extra inputs.
Keep the response in run evidence; tracked artifacts contain the resulting
clarifications, never scratch references.

Use this brief, followed by the spec:

> Read only the spec below. Restate the goal, list the files you would touch
> (mark guesses), propose a test plan, and list every open decision or question
> that would affect implementation. Distinguish missing requirements from
> engineering choices you can reasonably make. Do not implement, consult links
> or request more context. Return those four parts even if no gaps are found.

The chair checks each question against the spec. Correct omissions in place;
do not turn speculative alternatives or ordinary engineering choices into new
requirements. The check supplies no authority and does not replace review.

For the deferred comparison with cold reading, use `evaluate` with the
[past-spec fixtures](../evals/fresh-reader/README.md). Keep grading notes out of
reader briefs. Adoption remains provisional until that comparison shows value.
