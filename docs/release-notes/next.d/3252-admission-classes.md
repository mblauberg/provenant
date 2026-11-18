## Features

### Host admission classes

- Heavy commands coordinate through named admission classes (`[admission.<class>]`)
  configured across host and project scopes, replacing single-purpose build slots.
  Shipped `build` class preserves existing build slot concurrency derived from host
  cores and memory. (#3252)
- Lanes run `cargo`, `cargo-nextest`, and `nextest` through the `build` admission
  class by default across both compilation and test execution phases, preventing host
  stampedes during concurrent test runs. Nested invocations detect re-entrancy and
  pass through without deadlocking. (#3252)
- Hooks and scripts acquire slots via `provenant admit <CLASS> -- <COMMAND> [ARGS...]`,
  releasing leases on process exit or termination. `provenant status` reports waiters,
  holders, wait durations, and admission classes. Saturated classes warn and proceed
  after a configurable bounded wait. (#3252)
