## Fixes
### Fabric and MCP

- `provenant hosts doctor NAME` now resolves `NAME` through the configured peers and sends a read-only doctor action over the same `provenant fabric peer` transport as the other peer reads, including through a forced-command key. An unknown name is refused as `UNKNOWN_HOST` instead of being passed to SSH and reported as `SIGNIN_PROBE` (#2329).
