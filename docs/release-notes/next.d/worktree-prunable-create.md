- `worktree create` now skips Git worktree registrations already marked
  prunable, so a deleted checkout does not block new worktree creation.
