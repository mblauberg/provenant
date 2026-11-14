# Interpreting history comparisons

Each pair compares the second level with the first, sorted by its source value.
The estimator is a session-balanced linear probability comparison: each
session's turns together have weight one. It adjusts for project, calendar day,
work mix, context and compaction, plus other observed exposure dimensions,
except the derived within-session era and the dimension being compared.
Numeric context stays linear when some measurements are missing, with a
separate missingness indicator. Other controls are categorical. Missing controls
form explicit unknown levels and are counted. This does not adjust for
unrecorded causes. Tight daily controls can leave historical comparisons
unestimable; widen periods only with a stated sensitivity analysis.

Zero joint categorical overlap or rank loss reports `non_identifiable`: the
data cannot separate group from the controls. Otherwise `identifiable` means an estimable adjusted association,
not proof of causation. Both raw and adjusted differences are returned.

Uncertainty uses whole-session residual sign permutations, exactly enumerated
for up to ten sessions and seeded Monte Carlo above that. `permutation_interval`
is the null score interval, not a confidence interval. `confidence_interval`
is the estimate shifted by full-fit residual sign permutations; it is an
approximate interval, without studentisation or leverage correction. It relies
on independent sessions and symmetric session residuals; serial sessions from
one project can violate that assumption. Neither interval treats turns as
independent observations. Leave-one-session-out reports the range, sign flips
and losses of identifiability.

Read session counts, joint-overlap sessions, excluded replies, missing controls,
audit agreement and leave-one-out sensitivity before any effect claim. A few sessions, weak overlap,
rare outcomes, a wide interval or one influential session means inconclusive.
Report all pairwise comparisons as exploratory; there is no multiple-testing
correction. A null or non-identifiable result does not establish equivalence.
