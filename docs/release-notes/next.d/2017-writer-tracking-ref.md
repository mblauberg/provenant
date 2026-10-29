## Fixes
### Git and safety

- Settlement refreshes the `origin` tracking ref for each writer with a branch, including explicit and adopted worktrees, so a successful push shows in the shared repository. The refresh is best effort (#2017).
