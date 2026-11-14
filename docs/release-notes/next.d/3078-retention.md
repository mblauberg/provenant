## Fixes

### Git and safety

- Release settled lane scratch and private Cargo output independently of worktree retention. GC measures nested allocated bytes consistently in preview and apply, reports failed cache operations with their paths, and skips busy observation gates immediately. `gc.free_floor` warns; `gc.critical_free` controls the small critical threshold for new builds. (#3078)

- Cleanup covers managed and foreign worktrees using separate subsumed and landed evidence. The first foreign pass previews changes, idle clean worktrees retain their branches, and explicit discard archives the tip and working data before removal. Repository settings control exclusions and remote pruning. (#3078)
