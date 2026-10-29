## Fixes
### Lanes and settlement

- `run` with neither `--mode` nor a target (`--worktree`, `--branch` or `--worktree-branch`) is refused from a registered linked worktree, and the refusal names both repairs: `--worktree <path>` for a writer or `--mode read-only` for a reader. An explicit `--mode read-only` dispatches as before (#2300).
- A lane whose final handoff declares `stopped_early` or `blocked` now settles as `review_required` when it would otherwise succeed, so `run --wait` and `lanes` show the unfinished work. Its partial work is still delivered. The graph counts any outcome other than `succeeded` as a failed node, so such a lane fails its graph node (#2300).
