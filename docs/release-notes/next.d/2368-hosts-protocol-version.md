## Fixes
### Fabric and MCP

- `provenant hosts` reports the base protocol fence, 6, beside the local
  release's newest feature level, e.g. `protocol=6 features=10`, so the base
  level no longer reads as stale, and a protocol mismatch names the protocol
  this host expected when the refusing frame carries it (#2368).
