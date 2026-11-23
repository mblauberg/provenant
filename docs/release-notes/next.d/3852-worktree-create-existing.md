## Fixes
### Git and safety
- `provenant worktree create` now warns and reuses a clean worktree already at the requested branch and base, returning its path, while a conflicting base or dirty tree refuses with the existing path, branch and reason named. (#3852)
