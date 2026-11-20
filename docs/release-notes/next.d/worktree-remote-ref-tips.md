## Fixes
### Git and safety
- Worktree creation, writer launches and landing evidence no longer refuse every request with "worktree Git output is malformed or incomplete" when the repository holds a remote ref without a `remote/branch` shape, such as `refs/remotes/pr123` from `git fetch origin pull/123/head:refs/remotes/pr123`. Every ref Git lists under `refs/remotes/` is now read as a remote ref. A Git read that fails or returns output Provenant cannot parse now names its subcommand, its exit status and Git's first stderr line, or the ref it could not read.
