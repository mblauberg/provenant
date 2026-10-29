## Fixes
### Git and safety

- New worktree branches are refused when the name differs from an existing branch only by case, Unicode normalisation or a path prefix, and origin refresh warns about branches that collide by case (#2132).
- A remote worktree base the remote did not advertise, such as `Origin/main` on a case-insensitive filesystem, now fails instead of using the local commit (#2132).
