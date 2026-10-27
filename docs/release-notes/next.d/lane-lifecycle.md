## Additions

Writer dispatch can create and own its worktree with `--worktree-branch` and
deliver with `--deliver none|push|integrate:BRANCH|pr`. Graph nodes share these
modes and a graph can open one integration PR. `lane deliver` picks up a
settled writer with clean-tree and exact-head checks. Resume starts fresh with
retained context when continuation is unavailable, unless `--no-fallback` is
set. Attempts recorded before #1855 have no launch evidence of running under an
owned tree, so continuation resumes them in place (fail open). Native linked
worktrees can be adopted, and graph scratch is excluded
locally through Git's info/exclude. Owned lane cleanup retains unsafe or
unmerged work and records why.
