## Fixes

- A worktree whose registration no longer matches the tree is no longer refused
  as an unknown writer. The refusal names its reason (a stale registration with
  the field that differs, a locked or prunable Git entry, a writer reservation
  or an unreadable record) and a repair. `provenant worktree adopt PATH --owner
  NAME` now replaces a stale registration when the writer lock is free and no
  live process is observed, so a tree re-added with `git worktree add` becomes
  dispatchable again. When `adopt` or `pr land` refuses because a writer is live,
  the message names the observed process by pid and command.
