## Fixes
### Runs and graphs

- Graph writer launches retry transient worktree registry lock timeouts within their admission deadline, while retaining refusals for live writers and dirty worktrees (#2760).
