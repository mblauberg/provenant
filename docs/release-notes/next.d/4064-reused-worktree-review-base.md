## Fixes

### Pipelines and graphs

- A findings-panel that sources an implement node reusing an existing worktree now resolves the writer's base from the node's `base`, then the worktree branch's upstream, then its merge base with the default branch, and reviews base..pushed head instead of failing every lens on an empty range. `run --explain` names the resolved commit and its source; a node-sourced panel with no resolvable base refuses the graph before launch, while plain writers stay advisory. (#4064)
