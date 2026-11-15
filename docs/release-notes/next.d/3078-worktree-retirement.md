## Fixes
### Git and safety

- Worktree retirement preserves unique detached commits before removing their checkout. Reusing a removed path starts fresh custody, and interrupted cleanup resumes reference removal before marking the lane pruned. (#3078)
