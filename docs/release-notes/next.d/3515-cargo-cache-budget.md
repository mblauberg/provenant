## Breaking changes

### Lanes and settlement

- Replace `gc.lane_cargo_cache_max` and `domains.fabric.lanes.build_dir_max_bytes` with `"lanes.cargo_cache_max"` in the host configuration `[knobs]` table. The budget defaults to 10% of the cache volume, accepts byte or percentage overrides and covers private and shared Cargo targets, reclaiming idle targets by last use while protecting live attempts and held Cargo locks. (#3515, #3521)
