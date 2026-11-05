## Fixes
### Pipelines and graphs

- Graph writer initialisation retries a contended worktree registry lock instead of immediately blocking the node (#2760).
