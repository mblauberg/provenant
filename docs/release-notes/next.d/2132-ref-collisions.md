## Fixes

- Refuse case-folding and Unicode-normalisation collisions in origin refreshes and new worktree branches. Pin remote worktree bases to the exact advertised commit.
- Skip unsupported advertised branch names during refresh, warn about stale refused tracking refs, and detect collisions between branch path prefixes. Remote bases require a reachable remote.
- Refuse remote revision aliases that cannot be verified as an exact branch, and use the advertised default branch for pre-push checks instead of the local tracking HEAD.
