- `worktree create` now skips Git worktree registrations already marked
  prunable, so a deleted checkout does not block new worktree creation. It leaves
  the stale registration intact. Removal reports stale registrations with
  `git worktree prune` guidance, and merged branch pruning skips missing checkouts
  when checking whether a branch is still in use (#1783).
