## Behaviour changes
### Lanes and settlement

- Lanes without write authority below `gc.free_floor` warn once and proceed. Lanes with worktree, declared root, tool or project write authority continue to wait for enough disk space.
