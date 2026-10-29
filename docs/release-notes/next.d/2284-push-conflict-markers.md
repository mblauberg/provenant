## Additions
### Git and safety

- Set `[domains.run] push_conflict_markers = false` to turn off the advisory literal conflict-marker check in the Git pre-push hook, for fixtures that intentionally contain markers. Default-branch deletion, non-fast-forward updates and linked-worktree default-branch pushes are still refused (#2284).
