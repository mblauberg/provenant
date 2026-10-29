## Fixes
### Git and safety

- A worktree whose registration no longer matches is no longer refused as an unknown writer, and the refusal names the reason and the repair (#2067).
- `provenant worktree adopt PATH --owner NAME` replaces a stale registration when the writer lock is free, and a refusal because a writer is live names the process (#2067).
