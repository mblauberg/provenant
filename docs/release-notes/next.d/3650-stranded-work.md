## Fixes

### Chair, inbox and console

- `provenant status` and `provenant worktree list` now report stranded work: a registered or project worktree with uncommitted edits and no live holder, named by branch and linked issue or PR where the branch carries one, with changed file and line counts, the age of the newest edit and the `run implement --worktree` command that adopts it. `gc` keeps and names the same work instead of letting a stopped lane's edits disappear during cleanup. A clean worktree, one held by a live lane and one whose holder cannot be determined are never called stranded; the report only observes state and never refuses. (#3650)
