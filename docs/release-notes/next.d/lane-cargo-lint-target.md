## Fixes

- Writer lanes get `PROVENANT_LANE_CARGO_LINT_TARGET`, a `target-lint/`
  directory in their Cargo lane cache. Lint-only runs (`cargo check`,
  `cargo clippy`) can set it as both `CARGO_TARGET_DIR` and
  `CARGO_BUILD_BUILD_DIR`, so they stop waiting on the build-directory lock that
  test and build runs hold. The directory is removed with the lane cache.
  Read-only lanes are unchanged.
