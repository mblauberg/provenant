## Breaking changes

### Lanes and settlement

- Replace `gc.lane_cargo_cache_max` with `lanes.cargo_cache_max` in host configuration. The budget accepts bytes or a percentage of the volume and covers private and shared Cargo targets, reclaiming idle targets by last use while protecting live attempts and held Cargo locks. (#3515, #3521)
