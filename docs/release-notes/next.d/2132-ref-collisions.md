## Fixes

- Refuse case-folding and Unicode-normalisation collisions in origin refreshes and new worktree branches. Pin remote worktree bases to the exact advertised commit.
- Skip unsupported advertised branch names during refresh, warn about stale refused tracking refs, and detect collisions between branch path prefixes. Remote bases require a reachable remote.
