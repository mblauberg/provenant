## Breaking changes
### Lanes and settlement

- **Build wait settings are removed.** Delete `domains.fabric.lanes.build_wait_minutes` and `lanes.slot_wait_seconds`; busy build slots now warn and proceed. Resource admission also warns by default, and disk pressure never pauses running builds; set `domains.fabric.lanes.admission_policy.disk_floor = 'refuse'` for strict admission of new writers and builds (#3078).
