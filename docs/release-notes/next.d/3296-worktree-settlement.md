## Fixes

### Git and safety

- `provenant gc` collects settled worktrees backed by retained refs while preserving their local branches, and retires orphaned pruning receipts with explicit custody uncertainty instead of retrying forever. (#3296)
