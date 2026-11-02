# Continuous integration
Owner: repository maintainers
Anchor: .github/workflows/ci.yml

## Rust front-door tests

Pull requests run the tests for one changed Rust workspace crate and every
workspace package that depends on it, using a deterministic `cargo metadata`
dependency graph. Both nextest hash partitions keep their existing partition
and serial-process behaviour. The provider recording example test runs when
the provider crate is affected.

A change to several crates selects each crate and its dependents. The Rust
front-door job runs the full workspace on pushes to `main` and when a changed
path falls outside a crate directory or cannot be resolved safely. These paths also force the full workspace:

- `tools/front-door/Cargo.lock`
- `tools/front-door/Cargo.toml`
- `tools/front-door/rust-toolchain.toml`
- `tools/front-door/.config/nextest.toml`
- `.github/workflows/ci.yml`
- any `build.rs`
- `tools/front-door/assets/`
- `skills/` and `roles/` assets

## Rust test time budgets

The default and CI nextest profiles mark tests slow after 10 seconds and stop
them after three intervals. Keep the CI retry count and `fail-on-flaky` policy
unchanged. A test that needs more time belongs in a named profile override in
`tools/front-door/.config/nextest.toml`, with a comment giving the reason.
`scripts/nextest-slowest` appends the 20 slowest JUnit cases to the Rust test
job summary.

Keep the full-run list conservative when introducing build-time inputs or
workspace-wide configuration. The `main` push remains the full-workspace
backstop if a path is missed.
