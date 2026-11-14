## Fixes

### Worktrees

- Release settled lane scratch and private Cargo output independently of worktree retention. GC measures nested allocated bytes consistently in preview and apply, reports failed cache operations with their paths, and skips busy observation gates immediately. `gc.free_floor` warns; `gc.critical_free` controls the small critical threshold for new builds. (#3078)
