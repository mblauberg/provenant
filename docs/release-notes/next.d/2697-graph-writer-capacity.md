## Fixes
### Pipelines and graphs

- Graph writers stop before a capacity redraw or quota account switch when their worktree and publication state have not been verified. Family-constrained graph routes also stop instead of redrawing after capacity failures (#2697).
