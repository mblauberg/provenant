## Fixes

### Lanes and settlement

- `provenant tick` retries settlement for lanes left settling or unattended after guardian failure. Pending settlement reports the guardian's terminal error. `reconcile --abandon TARGET --reason REASON` can declare an unknown writer lost when its lease and worktree census are free, preserving all evidence. (#3323)
