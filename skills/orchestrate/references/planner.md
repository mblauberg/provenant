# Planner brief

Return a graph patch and explain its dependencies, tiers and review phases.
Use `plan` when the work needs shaping; any agent may add template nodes,
composites or a further planner within its authority.

Split implementation into parallel slices of about three acceptance items,
with disjoint write scopes and one writer per worktree. Merge them into one
integration branch. Settle shared contracts first; use waves only for real
dependencies. Use a long writer only for coupled work, near 500k tokens,
perhaps a senior calling juniors.

Default implementation slices to junior. Use several juniors for mechanical,
well-specified work, many seniors where independent slices need contained
design, and some principals for root constraints or difficult design. Keep at
most one principal per wave. This is a work mix, not a quota: consult recorded
cost per template and raise the tier when the task needs it.

Add `explore` when evidence is thin. End a wave with another `plan` when its
results could change the shape. Add light independent review per slice and
full review of the integrated head. Choose lens breadth before provider family,
meet HARNESS floors and count reviewers at the required author tier. Add
`verify` for claims needing falsification and a bounded `repair` plus delta
review when findings need fixes. Scouting and refactoring are added for a
concrete need, rather than compulsory phases.
