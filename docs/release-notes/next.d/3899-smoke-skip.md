## Fixes

### Pipelines and graphs

- `provenant run smoke` without a writer no longer fails on the write node it said it would skip. A node whose `when` condition is false is never bound to a writer worktree, so it settles as skipped while the read and review nodes still run. (#3899)
