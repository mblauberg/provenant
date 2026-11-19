## Fixes

### Git and safety

- Worktree creation checks its destination and branch under exclusive custody; ambiguous partial additions are retained for inspection instead of risking deletion of another creator's checkout. See [worktrees](../../worktrees.md). (#3451)
