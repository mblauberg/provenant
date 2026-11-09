## Fixes
### Pipelines and graphs

- Pinned read-only lanes always use a snapshot of the pinned commit, including with `light` or `shared` isolation. Unpinned read-only lanes under `shared` read the bound worktree or current directory live (#3051).
