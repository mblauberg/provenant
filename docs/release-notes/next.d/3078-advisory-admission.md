## Breaking changes
### Lanes and settlement

- **Build wait settings are removed.** Delete `domains.fabric.lanes.build_wait_minutes` and `lanes.slot_wait_seconds`; busy build slots now warn and proceed. Resource admission also warns by default, and disk pressure never pauses running builds; set `policy.ADMISSION_DISK` and `policy.BUILD_DISK` to `refuse` for strict admission of new writers and builds (#3078).
