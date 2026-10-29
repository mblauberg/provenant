## Behaviour changes

- `domains.fabric.lanes.max_concurrent_builds` now limits cargo builds, not live writer lanes. Writers are admitted on the lane caps and the memory and disk floors alone, so idle writers no longer block each other or other projects, and the `writer build slots full` refusal is gone (#2197).
- Each lane with the Rust toolchain gets a `cargo` shim that takes a host-wide build slot around `build`, `check`, `clippy`, `test`, `bench`, `doc`, `nextest` and the other compiling commands, and releases it when the command exits, including after a crash. Slots are shared by every project on the same state directory. `cargo_build_jobs` still sets the parallelism of each build.
- `provenant lanes` reports a lane whose cargo command is queued as `waiting for build slot: n of m in use`.
