## Fixes

- `provenant worktree list` prints each path as a plain path, such as
  `path=/Users/user/Repos/app`, quoted only when needed. The structured path
  object stays in `--json`.
