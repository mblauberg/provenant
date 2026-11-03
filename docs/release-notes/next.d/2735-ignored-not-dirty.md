## Fixes
### Lanes and settlement

- `provenant run` accepts a worktree whose only uncommitted files are ignored by Git (such as `target/` or `.provenant/scratch/graphs/`). A worktree with uncommitted tracked or untracked changes still refuses, and the refusal names up to 12 dirty paths (#2735).
