## Fixes
### Git and safety

- Cleanup covers managed and foreign worktrees using separate subsumed and landed evidence. The first foreign pass previews changes, idle clean worktrees retain their branches, and explicit discard archives the tip and working data before removal. Repository settings control exclusions and remote pruning. (#3078)
