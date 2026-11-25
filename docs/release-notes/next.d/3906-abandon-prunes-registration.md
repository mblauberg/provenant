## Fixes

### Lanes and settlement

- `provenant reconcile --abandon` clears the Git registration of a writer checkout that was removed by hand when that registration proves the abandoned lane's ownership, so its branch is no longer checked out at a missing path and no later worktree create fails with `StaleRegistration`. Only that one registration is removed; a locked, ambiguous or foreign registration is left alone and warned about with the exact `git -C <common> worktree prune` remedy. (#3906)
