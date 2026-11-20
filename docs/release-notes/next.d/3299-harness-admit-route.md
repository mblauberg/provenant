## Fixes

### Lanes and settlement

- The worktrees test harness now routes the lane cargo shim's `admit build`
  invocation to the real admission command instead of treating it as a libtest
  argument. The shim re-enters the harness through `current_exe`, so an
  unrecognised `admit` fell through to the test suite and printed "worktree
  integration: invalid input parameter", leaving the native writer-capability
  probe quarantined. With `admit` dispatched like the other internal commands,
  the probe verifies that a lane's cargo and nextest shims reach
  `provenant admit build`, so `lane_prepare_isolates_nested_writer_state`
  leaves quarantine. (#3299)
