## Fixes
### Lanes
- A lane's HOME and XDG state directories are now rebound into the attempt scratch whenever they resolve inside the worktree the lane writes, with a warning naming the variable and both paths. Previously a tool could write state such as `.local/state/gh/device-id` under a HOME inside its worktree and have it committed with the lane's work. Values outside the worktree are unchanged. (#3457)
