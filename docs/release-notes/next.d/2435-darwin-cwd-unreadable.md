## Fixes

### Git and safety

- Worktree removal now keeps a tree when a live process's working directory cannot be read on macOS. The Darwin cwd probe reports a process gone only after an absence probe proves it is gone.
