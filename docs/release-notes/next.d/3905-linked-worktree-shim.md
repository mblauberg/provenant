## Fixes

### Lanes

- The lane Git shim now presents the attempt's private repository as a registered linked worktree: `git rev-parse --git-dir` points at a private registration below the attempt's `git-common`, `--git-common-dir` still points at the private common store, and `git worktree list` resolves the lane checkout. Repository tooling that clears `GIT_*` before invoking Git therefore no longer mistakes the lane for an unregistered worktree root. Settlement and budget checks preserve unfinished operations in that registration before falling back to direct common-directory state. Lane refs, objects and index remain attempt-private. (#3905)
