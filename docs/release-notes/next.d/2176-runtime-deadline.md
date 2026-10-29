## Fixes
### Lanes and settlement

- Attempt runtime deadlines now hold on the wall clock, so a laptop asleep through a deadline no longer keeps its provider, worktree, lock and quota for hours. The attempt settles as `timed_out` with the reason `runtime deadline exceeded`, `lanes` shows each running lane's `runtime_deadline`, and `providers.<id>.runtime_deadline_seconds` caps the deadline per provider (#2176).
