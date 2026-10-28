## Fixes

- `lanes --wait` no longer fails with a run-store path error when a running lane's guardian replaces its process checkpoint while the wait reads it. A handle opened just before the replacement is discarded and the name is read again; ownership, mode, kind and link checks are unchanged.
- A refused run-store path now names the path, relative to the state root, and the failed check (owner, mode, symlink, kind, links, containment, identity or missing) instead of reporting only that a path is not private or contained.
