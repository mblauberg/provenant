## Fixes
### Lanes and settlement

- Writers that stop without a new commit now say they stopped with no work and acceptance remains unmet. Graph writer nodes continue once automatically in the same worktree; set `auto_continue_no_work = false` to disable this. Stopping a settled writer also prevents its pending automatic continuation.
