## Additions

### Git and safety

- Added `provenant worktree locate`, which resolves a branch, lane, attempt or
  pull request to its checkout path; `worktree list` and `status --json` rows
  now carry the checkout path, lane and base; `worktree remove` records the
  removed tip in `removed.jsonl` and prunes a branch merged into its base. (#3810)
