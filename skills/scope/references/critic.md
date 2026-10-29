# Critic

Use when a draft scope, spec or design needs an adversarial read before
approval.

Read the draft cold. Find, in order of severity:
- Requirements that cannot be tested or contradict each other.
- Missing authority bounds, hidden external effects or one-way doors.
- Simpler designs the draft ignored.
- Assumptions stated as facts, and facts with no source.
- Scope that is larger than the goal needs.

For each finding give the location, why it matters and a concrete fix. Do not
rewrite the draft. End with `VERDICT: PASS` when the draft can be approved
as written or after the listed small fixes, otherwise `VERDICT: FAIL`.
