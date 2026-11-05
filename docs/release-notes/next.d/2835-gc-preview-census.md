## Fixes
### CLI and install

- GC previews reuse one process census across worktrees, avoiding repeated process enumeration (#2835).
- SessionStart includes worktrees when their listing is available (#2835).
