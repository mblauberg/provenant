## Fixes
### Fabric and MCP

- `fabric_mail` `send` accepts a call without `artifacts`, which now means no artifacts, and `fabric_context` with no `op` runs `inspect`. MCP `invalid_input` errors include a `field` when the rejected field is known, and continuation generation conflicts include `current_generation`. An invalid Fabric input from the `fabric` command now names the flag it rejected, such as `--to` (#2350).
