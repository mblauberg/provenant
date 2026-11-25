## Fixes

### Lanes and settlement

- `provenant run implement --branch <existing> --base REF` now launches from the branch tip when the branch is behind `REF`. `--base` is the merge target, not an ancestry requirement or a reset: the lane records the given base and warns `BRANCH_BEHIND_BASE` with the number of commits behind, while a branch that already contains its base launches without that warning. This also applies when a clean checkout is reused at an occupied worktree path, the usual fix-up on an open PR. A base with no merge base with the branch still refuses with `WRITER_SCOPE`, naming `--worktree` and the merge or rebase remedy; a shallow clone whose missing history cannot prove unrelatedness warns and proceeds. (#3644)
