## Fixes
### Lanes and settlement

- Writers in separate worktrees start concurrently without default host or project lane caps. Lock contention waits until the lane’s launch deadline; one writer per worktree and build-slot limits still apply (#3045).
