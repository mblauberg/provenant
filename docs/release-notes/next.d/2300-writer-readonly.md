## Fixes
### Lanes and settlement

- `run` now refuses an implicit read-only lane from a registered linked worktree and names the `--worktree` repair. A lane declaring an early stop now settles as `review_required` when it would otherwise succeed, making unfinished work visible in `run --wait` and `lanes` (#2300).
