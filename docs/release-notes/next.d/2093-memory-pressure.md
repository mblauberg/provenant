## Behaviour changes

- Defer lane admission at macOS warning or critical memory pressure and limit host-wide writer build concurrency. Configure the build slots and default Cargo jobs per host (#2093).
- Release stale writer slots when their recorded guardian is gone or their boot has ended, and report pressure and slot refusals.
- Set `domains.fabric.lanes.max_concurrent_builds` and `domains.fabric.lanes.cargo_build_jobs`, with `-1` or omission selecting automatic defaults. Claude Bash commands receive the chosen `CARGO_BUILD_JOBS` after environment scrubbing.
- Retain writer slots while the owner store is busy, missing, replaced or unreadable; a changed boot or confirmed guardian death releases them.
- Retry admission while host configuration is unreadable, and report parsed invalid build settings immediately.
