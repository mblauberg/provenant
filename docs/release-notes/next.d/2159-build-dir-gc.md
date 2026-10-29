## Behaviour changes
### Lanes and settlement

- Lanes now build with `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=line-tables-only`, so a lane's target no longer accumulates incremental sessions. Set `cargo_incremental` and `cargo_debug` in `[domains.fabric.lanes]` to restore either (#2159).
- Fabric collects idle Cargo profile directories in lane caches, least recently used first, until their allocated bytes are within `domains.fabric.lanes.build_dir_max_bytes` (20 GiB by default) and free space is at least `gc.free_floor`. A directory with any Cargo lock held, or written within `build_dir_grace_seconds` (15 minutes by default), is never removed (#2159).
- Collection runs when a lane settles, when admission finds free space below the floor, and in `provenant gc`, which now reports the lane build directories removed or reclaimable and the bytes reclaimed. A settling lane walks the caches at most once per `build_dir_survey_interval_seconds` (5 minutes by default) unless free space is below the floor (#2159).
