## Fixes

Lane settlement now collects its declared ignored output and Cargo cache even
when delivery pickup retains the worktree. Settlement and landing collection
share GC's existing lock, with the daily sweep as a backstop. Final worktree
removal honours regenerable and keep settings when output is recreated after
collection, so regenerable build files no longer strand merged worktrees.
