## Fixes
### Lanes
- Writer lanes are now told their working copy in every adapter's prompt: the absolute bound worktree path, branch and base, and that the primary checkout is not theirs. opencode lanes (Hy 4) read the primary checkout as the workspace root of a linked worktree and edited, branched and committed there while the bound worktree stayed at base. (#3611)
- Settlement now notices work made outside the bound worktree. When the primary checkout sits on a branch that did not exist at launch, or holds commits that no launch ref, fetched remote-tracking ref or other worktree reaches, the attempt settles `review_required` with the new `stray_work` reason, naming the branch or commits, instead of `succeeded`. A pull or fetch in the primary checkout is not stray work. (#3611)
