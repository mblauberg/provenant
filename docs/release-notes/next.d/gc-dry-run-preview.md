## Fixes

- `gc --dry-run` now lists a merged worktree that would stay dirty after its
  output is pruned as retained with reason `dirty`, as `gc --apply` does. It
  was previously left out of the report (#1723).
