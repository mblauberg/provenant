## Fixes
### Lanes
- Writer lanes are now told their working copy in every adapter's prompt: the absolute bound worktree path, branch and base, and that the primary checkout is not theirs. opencode lanes (Hy 4) read the primary checkout as the workspace root of a linked worktree and edited, branched and committed there while the bound worktree stayed at base. (#3611)
- Settlement now notices work made outside the bound worktree. When the primary checkout's HEAD moved during the attempt and the bound branch gained no commits, the attempt settles `review_required` with the new `stray_work` reason, naming the primary checkout and where it moved, instead of `succeeded`. A moved primary beside real lane commits only warns. (#3611)
