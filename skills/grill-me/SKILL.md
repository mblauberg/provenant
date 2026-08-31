---
name: grill-me
description: "Use when the user asks to be grilled or wants one-question-at-a-time stress testing of a plan or design. Not for writing the final spec or implementing it; use scope or implement."
---

Interview the highest-leverage unresolved branch first. Resolve dependent
decisions one at a time; do not exhaustively interrogate low-value detail. For
each question, give the evidence already known, 2–3 concrete options when useful,
and a recommended answer with its trade-off.

Read the repository's `Repository process` declaration before recording scope
and stories.
If it is absent or ambiguous, use an unambiguous existing owner; otherwise
return the proposed register in chat without inventing or writing a new owner.
The parent tracker issue owns the scope/story when it declares `issue-tracker`;
use the named project-docs home when it declares `project-docs`. Do not create a
parallel story or status record.

Keep the explicit decision context current: intake/revision, constraints,
evidence, decided branches, parked owner calls and the next unresolved branch.
Do not start a parallel interview when revising the same request.

Ask one question at a time. Record decided, parked and still-open branches in
the enclosing `scope` artifact when write authority exists; otherwise keep a
compact chat register.

If a question can be answered from the workspace or current authoritative
sources, investigate it instead of spending user attention. Stop when the
shared decision/acceptance contract is clear, the user asks to stop, or the next
branch needs unavailable owner authority. Return the resolved register to
`scope` for a digest-bound handoff; the interview itself is not approval.

## Adapter-absent path

Console, Herdr and GitHub are optional. Continue from canonical project
artifacts and emit the skill-owned artifact kind in
[portable-workflow.v1.json](portable-workflow.v1.json). That filesystem
artifact proves only that a context object existed. Retain and identify the
canonical context separately; this output is not itself workflow evidence or
approval of the decision context. The runner validates the contract's declared
context fields, including `accepted_artifact_identity`, before emission.
