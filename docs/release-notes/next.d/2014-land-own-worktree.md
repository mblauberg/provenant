## Fixes
### Lanes and settlement

- `land` run from a clean merged worktree now removes that worktree and its branch, cleaning up from the primary checkout. Other live processes in the lane still block removal (#2014).
- `land` clears abandoned prepared and settled terminal writer reservations once the dirty and ignored-output checks pass. Live, malformed and unproved reservations still block removal, and `land` refuses with a retry message when it cannot finish its live-worker check (#2014).
