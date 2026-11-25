## Fixes

### Lanes and settlement

- `provenant reconcile --abandon` no longer refuses a held writer attempt whose retained fence is missing with `INVALID`/`retained writer fence missing`. Abandon completes the attempt, leaves its worktree and uncommitted files untouched, and warns (`ABANDON_RETAINED_FENCE_MISSING`) with the custody slot that was to hold the fence. (#3960)
