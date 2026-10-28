## Fixes

- Writer lanes can read briefs and graph documents in the primary checkout's untracked project scratch (`.provenant/scratch/`, or `.agent-run/`), which a linked worktree does not hold. Read-only lanes already read it through their working directory (#1942).
