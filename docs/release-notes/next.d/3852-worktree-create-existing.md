## Fixes
### Git and safety
- `provenant worktree create` now warns and reuses a clean worktree already on the requested branch and at or ahead of the requested base, returning its path, while an unrelated base or dirty tree refuses with the existing path, branch and reason named. (#3852)
