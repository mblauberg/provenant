## Fixes

### Telemetry and providers

- `provenant check providers --live` no longer reports `worktree_busy` for one route when several routes are probed together: live probes on the same checkout run one at a time, so each route settles with its own cause rather than a probe refusing its sibling for custody of the shared worktree.
