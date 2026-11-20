## Fixes

### Git and safety

- Worktree removal retains unresolved writer reservations and inspects every recorded private index, including unfinished Git operations. Clean-index retirement is durable before scratch cleanup, so interrupted cleanup can resume without losing staged work. (#3451)
