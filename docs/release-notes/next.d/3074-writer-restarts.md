## Behaviour changes
### Lanes and settlement

- Stopped writers restart from their handoff on the same worktree, keeping commits and uncommitted edits, on another eligible pool route, unless model policy selects session resume. Stops needing an owner decision or missing capability return to the chair without launching more work. `run --wait` and graph status keep a graph pending until a stopped writer's restart is saved, rather than reporting the stopped attempt as final. (#3074)
