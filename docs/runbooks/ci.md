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

## Required native test inventory

Each row must appear in the macOS nextest list and pass once in its named
invocation. The inventory check reads the preserved JUnit reports and the
effective environment captured by each invocation. Update this table when a
native test moves, changes name, or is added.

| Exact test name | Platform | Invocation in `rust-macos-tests` | Required environment | Prerequisite tools |
|---|---|---|---|---|
| `permissions::tests::escape::native_sandbox_blocks_path_escape_probes` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | sandbox-exec |
| `writer_production_plan_denies_outside_writes` | macOS | `dispatch` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_LANE_TOOLS_REQUIRED` | sandbox-exec, git |
| `writer_protected_changes_wait_for_review_and_ignore_upstream` | macOS | `dispatch` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_LANE_TOOLS_REQUIRED` | sandbox-exec, git |
| `worktrees::census_tests::native_full_census_places_clean_merged_worktree_and_same_user_sleep` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | git, sleep |
| `worktrees::census_tests::native_unreadable_windowserver_allows_cleanup_of_a_merged_clean_tree` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | git |
| `sys::pid::tests::native_occupancy_reports_image_and_open_file_with_identity_completeness` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | sleep |
| `sys::pid::tests::native_occupancy_never_proves_an_unqueryable_process_empty` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | sleep |
| `sys::pid::tests::native_current_process_is_not_an_eligible_platform_daemon` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | sleep |
| `sys::pid::tests::native_same_user_sleep_is_not_an_eligible_daemon` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | sleep |
| `sys::pid::tests::native_offsealed_platform_process_is_refused_when_present` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | sleep |
| `command_custom_shebang_runtime_keeps_credentials_and_writes_denied` | macOS | `provider-dispatch` | `PROVENANT_CONFINEMENT_REQUIRED` | sandbox-exec |
| `tests::claude_network_on_native_tls_files_are_readable` | macOS | `tls` | `PROVENANT_CONFINEMENT_REQUIRED` | sandbox-exec |
| `opencode::tests::read_only_search_runs_in_native_profile` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | rg, sandbox-exec |

## Rust test time budgets

For local native tests, pass `--success-output immediate` to `cargo nextest
run` to show `SKIP #n: ...` lines emitted by passing tests when a native
prerequisite is unavailable. The `PROVENANT_CONFINEMENT_REQUIRED`, `PROVENANT_POSTGRES_REQUIRED`, and
`PROVENANT_LANE_TOOLS_REQUIRED` variables make the corresponding supported
prerequisite checks fail rather than skip.

The default and CI nextest profiles mark tests slow after 10 seconds and stop
them after three intervals. Keep the CI retry count and `fail-on-flaky` policy
unchanged. A test that needs more time belongs in a named profile override in
`tools/front-door/.config/nextest.toml`, with a comment giving the reason.
`scripts/nextest-slowest` appends the 20 slowest JUnit cases to the Rust test
job summary.

The single macOS test job has a 20-minute ceiling. Baseline
[run 41756777072](https://github.com/mblauberg/provenant/actions/runs/41756777072)
took 12m13s: lint 1m03s, archive 2m54s, supervisor/custody 2m34s,
dispatch confinement 2m55s, and provider dispatch 1m27s. Queue, setup,
cache state, slowest cases and comparable cold and warm runs have not yet
been measured. Keep the existing single job and serial writer and isolated
census invocations until those measurements justify a different partition.

Keep the full-run list conservative when introducing build-time inputs or
workspace-wide configuration. The `main` push remains the full-workspace
backstop if a path is missed.

## Flaky-test quarantine

Add a flaky test to `tools/front-door/.config/quarantine.toml` only while it
has an open issue, an owner and an expiry within 30 days. CI validates each
entry against `cargo nextest list`, excludes quarantined tests from the
blocking partitions, then runs them in an advisory step that reports their
result. Remove the entry in the PR that fixes the test.
