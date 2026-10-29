## Behaviour changes
### Lanes and settlement

- Lane admission defers at macOS warning or critical memory pressure, and host-wide writer build concurrency is limited. Admission and `lanes` report pressure and slot refusals (#2093).
- `domains.fabric.lanes.max_concurrent_builds` and `domains.fabric.lanes.cargo_build_jobs` set the build slots and default Cargo jobs per host; `-1` or omission selects automatic defaults. Claude Bash commands receive the chosen `CARGO_BUILD_JOBS` (#2093).
- Writer slots are released when their guardian is gone or their boot has ended, and kept while the owner store is busy, missing, replaced or unreadable. Admission retries while host configuration is unreadable, and invalid build settings are reported immediately (#2093).
