## Fixes
### Worktrees

- A writer launched straight after `git worktree add` is no longer refused when a checkout hook's short-lived process still has its working directory in the new worktree. Launch re-checks within its existing wait budget; a process still present after it refuses and is named as before (#3110).
