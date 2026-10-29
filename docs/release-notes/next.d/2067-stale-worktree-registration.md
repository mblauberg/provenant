## Fixes
### Git and safety

- A worktree whose registration no longer matches the tree is no longer refused as an unknown writer. The refusal names the reason (a stale registration and the differing field, a locked or prunable Git entry, a writer reservation or an unreadable record) and a repair (#2067).
- `provenant worktree adopt PATH --owner NAME` replaces a stale registration when the writer lock is free and no live process is observed, so a tree re-added with `git worktree add` can be dispatched again. When `adopt` or `pr land` refuses because a writer is live, the message names the process by pid and command (#2067).
