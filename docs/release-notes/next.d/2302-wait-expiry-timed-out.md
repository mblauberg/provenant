## Fixes
### MCP

- `fabric_run` wait reports `timed_out: true` when its budget runs out before the first observation, instead of an error reply without that field (#2302).
