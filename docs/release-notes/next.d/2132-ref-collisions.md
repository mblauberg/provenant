## Fixes

- Refuse new worktree branches that differ from an existing local or remote-tracking branch only by case, including non-ASCII case, or by a path prefix.
- Origin refresh leaves out advertised branches whose names collide, with a warning, and fails only if the remote default branch collides. `HEAD` counts as an existing name, so a branch called `head` collides with `origin/HEAD`. A fetch never writes into a tracking symref, and refresh fetches pass `core.precomposeunicode=true`.
- Pin remote worktree bases and git-context bases to the commit the remote advertises, applying revision operators such as `~1` to that commit. This covers every spelling of the tracking root, including `remotes/origin/HEAD~1`. A base that Git resolves to a remote-tracking ref the remote did not advertise (such as `Origin/main` on a case-insensitive filesystem) now fails instead of using the local tracking commit. Pre-push uses the advertised default branch.
