## Fixes
### Git and safety

- New worktree branches are refused when the name differs from an existing local or remote-tracking branch only by case, Unicode normalisation or a path prefix. Origin refresh skips advertised branches that collide by case, with a warning, and refuses when names collide by normalisation; a fetch never writes into a tracking symref (#2132).
- Remote worktree and git-context bases are pinned to the commit the remote advertises, including with revision operators such as `~1`. A base that resolves to a ref the remote did not advertise, such as `Origin/main` on a case-insensitive filesystem, now fails instead of using the local commit (#2132).
