## Fixes
### Accounts, quota and routing

- Review shape selection keys on the four risk tiers: routine selects the quick shape, substantial the standard shape, and crucial and terminal the deep shape. Before this, every tier fell back to the standard shape. Terminal-tier work also gets the adversarial lens in `review-council` (#2350).
- The `review.extra_reviewers`, `review.cross_family`, `review.rereview_scope` and `review.polish` knobs are marked not wired; setting them has no effect (#2350).
