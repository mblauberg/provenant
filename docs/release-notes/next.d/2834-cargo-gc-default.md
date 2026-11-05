## Behaviour changes
### CLI and install

- GC defaults to collecting eligible `target` directories for Cargo projects without a project-specific GC entry. An explicit entry overrides the default (#2834).
- GC can reclaim ignored output in idle unknown-writer worktrees after a complete process census finds no holder, while keeping the worktree (#2834).
