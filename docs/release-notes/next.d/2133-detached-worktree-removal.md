## Fixes

- Remove registered detached worktrees with the usual custody and dirty checks, preserving branches. Unmerged detached commits require `--discard`; refusals name the reason and a recovery command.
