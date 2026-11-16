## Fixes

### Git and safety

- Landing cleanup now checks worktree cleanliness and output eligibility before collecting build output. Dirty or unknown worktrees keep their output and branch. (#3327)
