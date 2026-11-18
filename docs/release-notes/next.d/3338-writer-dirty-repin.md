## Fixes
### Lanes and settlement
- Preflight a writer worktree before any plan node runs: a dirty tree is refused immediately with its paths and the `lanes.writer_dirty` setting, whose overridable choices are refuse (default), wip-commit (recorded in graph events) and continue-in-place. Manual retry re-pins to the worktree head when it descends from the pinned commit, warns with both ids, and refuses a non-descendant with both ids; a pin mismatch now has its own honest cause text. (#3338)
