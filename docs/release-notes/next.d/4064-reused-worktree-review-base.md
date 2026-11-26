## Fixes

### Pipelines and graphs

- A findings-panel that sources an implement node reusing an existing worktree now resolves that writer's base from the node's `base`, then the worktree branch's upstream, then its merge base with the default branch, and reviews base..pushed head instead of failing every lens on an empty range. Each writer's panels receive their own base; an inferred base is not used as the graph-wide fallback for another writer. `run --explain` names each resolved commit and source; a node-sourced panel with no resolvable base refuses the graph before launch, while plain writers stay advisory. (#4064)
