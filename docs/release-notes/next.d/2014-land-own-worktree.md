## Fixes

- `land` run from a clean merged worktree now removes that worktree and its branch, using the primary checkout for cleanup. Other live processes in the lane still block removal (#2014).
- `land` refuses with a retry message when it cannot complete its check for live workers (#2014).
- `land` clears abandoned prepared writer reservations and settled terminal reservations once the dirty and ignored-output checks pass. Live, malformed and unproved reservations still block removal (#2014).
