## Fixes

A worktree-write dispatch that passes a `--write-root` equal to, or containing,
its `--worktree` is now refused up front. The error names the flag and says
that worktree-write already grants the worktree.
