## Fixes
### Fabric and MCP

- `provenant hosts` reports the base protocol fence, 6, beside the newest
  feature level the connected release negotiates, e.g. `protocol=6
  features=10`, so the base level no longer reads as stale against the newer
  feature levels in use (#2368).
