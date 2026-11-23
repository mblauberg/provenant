## Behaviour changes

### Git and safety

- An existing branch behind its base now warns `BRANCH_BEHIND_BASE` and proceeds at its current tip instead of refusing; `--base` records the merge target and never resets the branch. (#3644)
