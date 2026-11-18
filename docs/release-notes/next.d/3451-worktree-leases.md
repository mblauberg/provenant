## Behaviour changes

### Git and safety

- Authorised lanes can share a worktree and commit disjoint paths without a repository-wide lock. Overlapping path claims warn the owning lane; removal and GC preserve worktrees with live holders. See [lane lifecycle](../../runbooks/lane-lifecycle.md). (#3451)
