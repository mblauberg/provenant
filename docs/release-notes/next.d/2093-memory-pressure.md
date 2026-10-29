## Behaviour changes
### Lanes and settlement

- Lane admission now defers at macOS warning or critical memory pressure and limits host-wide writer build concurrency, and `lanes` reports the refusal (#2093).
- `domains.fabric.lanes.max_concurrent_builds` and `cargo_build_jobs` set the build slots and default Cargo jobs per host; `-1` or omission selects automatic defaults (#2093).
- Writer slots held by dead lanes are now freed (#2093).
