## Fixes

### Lanes and settlement

- Lanes now use the host Cargo home, preserving dependency fingerprints when reusing host-built target seeds. Lanes fetch into the shared registry and Git stores by default; set `domains.fabric.lanes.cargo_source_writes = false` to make them read-only and run Cargo offline. Cargo credentials remain denied and build output stays private. (#3274)
