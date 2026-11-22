# Interpreting history comparisons

`compare` in this skill's `analysis.py` holds the estimator: a session-balanced
linear probability comparison of the second level against the first, sorted by
its source value, with each session's turns weighing one. It adjusts for the
other observed exposure dimensions (project, day, work mix, context,
compaction), counts missing controls as explicit unknown levels and does not
adjust for unrecorded causes. Tight daily controls can leave a comparison
unestimable; widen periods only with a stated sensitivity analysis.

- `non_identifiable` (no joint categorical overlap, or rank loss) means the data
  cannot separate the group from the controls. `identifiable` means an
  estimable adjusted association, not causation. Raw and adjusted differences
  are both returned.
- Intervals come from whole-session residual sign permutations, never from
  independent turns. `permutation_interval` is the null score interval and
  `confidence_interval` an approximate interval that assumes independent
  sessions with symmetric residuals; serial sessions from one project can
  violate that. Dropping each session in turn reports the range of
  estimates, any sign flips and any loss of identifiability.

Read session counts, joint-overlap sessions, excluded replies, missing controls,
audit agreement and leave-one-out sensitivity before any effect claim. A few
sessions, weak overlap, rare outcomes, a wide interval or one influential
session means `inconclusive`. Report all pairwise comparisons as exploratory,
with no multiple-testing correction; a null or non-identifiable result does not
establish equivalence.
