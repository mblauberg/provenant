## Fixes

- Refuse new worktree branches that differ from an existing local or remote-tracking branch only by case, including non-ASCII case, or by a path prefix.
- Origin refresh leaves out advertised branches whose names collide, with a warning, and fails only if the remote default branch collides.
- Pin remote worktree bases and git-context bases to the commit the remote advertises, applying revision operators such as `~1` to that commit. Pre-push uses the advertised default branch.
