## Behaviour changes

### Git and safety

- Authorised lanes can share a worktree and commit disjoint paths without a repository-wide lock. Shared Git views reconcile host advances and nested launches, retain inactive displaced tips for recovery, and refuse only live divergent histories. Lone lanes retain automatic budget commits and failed-publication restoration. Overlapping path claims warn the owning lane; removal and GC preserve worktrees with live holders. See [lane lifecycle](../../runbooks/lane-lifecycle.md). (#3451)
