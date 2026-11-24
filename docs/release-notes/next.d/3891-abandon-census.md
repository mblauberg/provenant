## Fixes

- `reconcile --abandon` no longer reports `ABANDON_BLOCKED` when the worktree census cannot read a host daemon that cannot occupy a lane: a process running as another user, a platform-signed system binary, or one whose readable executable and cwd are outside the worktree is now treated as non-occupying and reported in a warning naming the process. An unreadable same-user process whose executable is inside the worktree still blocks. (#3891)
