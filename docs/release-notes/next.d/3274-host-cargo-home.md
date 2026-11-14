## Fixes

### Lanes and settlement

- Lanes now use the host Cargo home, preserving dependency fingerprints when reusing host-built target seeds. Sources are read-only by default; enable `domains.fabric.lanes.cargo_source_writes` to fetch dependencies with open network access. Cargo credentials remain denied and build output stays private. (#3274)
