## Added

### Run

- `provenant run implement --pr N` repairs a pull request from its CI failures: it resolves the PR's head branch and base through the forge, locates or creates the worktree, and appends the failing check names and bounded log tails to the brief. (#3500)

## Changes

### Worktrees

- An existing branch behind its base now warns `BRANCH_BEHIND_BASE` and proceeds at its current tip instead of refusing; `--base` records the merge target and never resets the branch. (#3644)
