## Behaviour changes
### Lanes and settlement

- Stopped writers restart from their handoff on the same worktree, keeping commits and uncommitted edits, on another eligible pool route, unless model policy selects session resume. Stops needing an owner decision or missing capability return to the chair without launching more work. A writer's stop is saved together with its restart, so `run --wait` and graph status never report the stopped attempt as final; a restart the host refuses pauses the graph for the chair with the reason. (#3074)
