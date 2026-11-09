## Fixes
### Lanes and settlement

- Writers that declare `stopped_early` now continue automatically on the same worktree, including graph nodes. The default allows two continuations per run; model and project policy can override it or disable it with zero (#3086).
