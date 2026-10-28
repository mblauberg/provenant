## Fixes

- `land` removes a clean merged worktree and its branch when invoked from that worktree, using the verified primary checkout for cleanup and output. Its live-worker census excludes only itself and its ancestors through the session leader; other observable live processes retain the lane.
- Landing refuses an incomplete worker census or unproved ancestor chain with a retry message. Ancestors must have strictly older native start stamps.
- Landing clears abandoned prepared writer reservations and settled terminal reservations through custody recovery after dirty and ignored-output prechecks pass. Live, malformed and unproved reservations still refuse removal.
