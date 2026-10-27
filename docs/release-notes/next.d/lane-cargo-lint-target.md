## Fixes

- Writer lanes get `PROVENANT_LANE_CARGO_LINT_TARGET`, a `target-lint/`
  directory in their Cargo lane cache. Lint-only runs (`cargo check`,
  `cargo clippy`) can set it as both `CARGO_TARGET_DIR` and
  `CARGO_BUILD_BUILD_DIR`, so they stop waiting on the build-directory lock that
  test and build runs hold. The directory is removed with the lane cache.
  Read-only lanes are unchanged.
- The orchestrate skill's Fabric brief template now tells writer lanes to batch
  targeted tests into one cargo invocation, to lint in the lint target, and to
  keep incremental builds on despite the sandbox's hard-link warning (#1853).
