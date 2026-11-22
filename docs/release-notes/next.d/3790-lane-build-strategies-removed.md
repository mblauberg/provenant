## Breaking changes

### Lanes and settlement

- Lane build strategies and Cargo target seeding are removed: lanes format only, the chair runs one warm integrated check per epic, and CI runs clippy and tests. Action: remove `build.wrapper`, `build.command`, `build.seed`, `lanes.cargo_target`, `lanes.cargo_seed_budget`, `lanes.local_cargo` and `domains.fabric.lanes.cargo_seed` from your configuration; a leftover key now warns as unknown. The chair's `build.strategy = "slots"` and `[admission.build]` cap remain for the host-owned check. #3790
