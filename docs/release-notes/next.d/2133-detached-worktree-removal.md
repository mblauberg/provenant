## Fixes
### Git and safety

- Registered detached worktrees can now be removed with the usual safety checks. Unmerged detached commits need `--discard`, and refusals name the reason and a recovery command (#2133).
