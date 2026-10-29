## Fixes
### Git and safety

- `provenant land` no longer refuses to fast-forward the base checkout over untracked files, such as Provenant's own scratch, and no longer keeps a finished worktree because an unrelated process works in a deleted directory.
