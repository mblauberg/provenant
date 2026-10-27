## Fixes

- `worktree create --base <remote>/<branch>` fetches an unresolved branch from
  a configured remote even when the repository fetches only `main`, without
  changing its fetch refspec. Unresolved base errors name the requested base
  (#1782).
