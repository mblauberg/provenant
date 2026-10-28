## Fixes

- `land` removes a clean merged worktree and its branch when invoked from that worktree, using the verified primary checkout for cleanup and output. Its live-worker census excludes only itself and its ancestors through the session leader; other live processes retain the lane.
- Landing clears abandoned prepared writer reservations and settled terminal reservations through custody recovery. Live, malformed and unproved reservations still refuse removal.
