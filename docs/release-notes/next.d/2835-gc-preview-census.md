## Fixes
### CLI and install

- GC previews reuse a process census and Git worktree inventory across worktrees, avoiding repeated enumeration (#2835).
- SessionStart includes worktrees when their listing is available (#2835).
- GC skips per-worktree Git probes, status and index reads outside its cleanup scope, checking candidates under writer exclusion before cleanup (#2835).
