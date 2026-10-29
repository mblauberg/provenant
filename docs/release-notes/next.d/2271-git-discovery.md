## Fixes
### Git and safety

- Ignore inherited Git directory redirects when finding repository paths, including for session handoffs, pull request landing and source inventory. (#2271)
- Keep repository-root protection active when a Git marker is malformed. (#2271)
