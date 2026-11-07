## Fixes
### Git and safety

- Lane Git commands select other repositories and scratch clones normally. `PROVENANT_GIT` uses the lane wrapper, keeping fetches on the lane repository in private Git state.
