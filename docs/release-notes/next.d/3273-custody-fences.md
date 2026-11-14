## Fixes
### Lanes and settlement

- Finished writer imports no longer leave a false worktree fence after an interrupted cleanup. An unresolved import names its fence, lane and exact reconciliation commands; restarting by WorkKey uses the same custody evidence and preserves retained work. (#3078, #3273, #3174)
