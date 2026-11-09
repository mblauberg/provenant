## Fixes
### Lanes and settlement

- Status and lane listing check custody and supervisor liveness in each displayed attempt's own worktree slot instead of inventorying every retained worktree reservation once per row (#3079).
- A writer launch refuses a stored worktree path that no longer resolves to itself, such as a checkout moved behind a symlink after submission, so its custody cannot sit where status does not look (#3079).
