## Fixes

- At settlement, the host best-effort refreshes the `origin` tracking ref for
  each writer with a branch, including explicit and adopted worktrees, so a
  successful writer push can be reflected in the shared repository.
