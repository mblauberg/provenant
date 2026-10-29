## Fixes

- `lanes --wait` no longer fails with a run-store path error when a running lane replaces its process checkpoint during the read (#2057).
- A refused run-store path error now names the path, relative to the state root, and the failed check (owner, mode, symlink, kind, links, containment, identity or missing) (#2057).
