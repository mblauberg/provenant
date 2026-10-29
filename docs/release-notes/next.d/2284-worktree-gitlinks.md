## Behaviour changes
### Git and safety

- Managed worktree creation and status now accept a tracked gitlink whose checkout is not initialised, because it has no nested Git configuration to run. An initialised gitlink still reports unknown and requires review (#2284).
