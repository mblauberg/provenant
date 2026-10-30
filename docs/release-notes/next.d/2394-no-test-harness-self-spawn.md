## Fixes

- Detached workers (settlement, reap, scheduled GC and provider updates) no longer start when the executable is a Cargo test harness. A unit test that reached the real settlement path relaunched its own harness, which reran the tests that spawned it until the host ran out of processes ([#2394](https://github.com/mblauberg/provenant/issues/2394)).
