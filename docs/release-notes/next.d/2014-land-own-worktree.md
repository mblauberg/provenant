## Fixes
### Lanes and settlement

- `land` run from a clean merged worktree now removes that worktree and its branch. It also clears abandoned writer reservations, and refuses with a retry message when it cannot finish its live-worker check (#2014).
