## Fixes
### Fabric and MCP

- `provenant hosts` reports each host's current Fabric protocol level, 10, instead of the older level of the health probe, 6. The probe now speaks the newest level, so a peer on an older level stays listed with its own level and `reason: "peer_version"` (#2368).
