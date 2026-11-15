## Fixes

### Git and safety

- `provenant run --branch` now creates managed writers under the primary checkout when launched from a linked worktree or subdirectory, and reports a useful repair when a requested path does not match the configured layout. (#3317)
