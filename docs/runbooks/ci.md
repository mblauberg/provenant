# Continuous integration
Owner: repository maintainers
Anchor: [`ci.yml`](../../.github/workflows/ci.yml); [`check-native-inventory`](../../scripts/check-native-inventory)

## Rust front-door tests

Keep fixtures independent of host capacity and time. At admission boundaries,
inject `Resources` observations and an `AdmissionPolicy`, and use
`provenant_core::TestClock` for deadlines and spacing. Keep production defaults
in tests that exercise the defaults themselves. For asynchronous effects,
use `provenant_test_support::wait_for` with a named condition and a bounded
deadline; signal the condition where possible instead of relying on a delay.
Tests of native process or filesystem behaviour may still need real host facts.
`RunHolder` receives its resource observations and clock from its caller;
guardian fixtures can supply both without changing production admission floors.
Include process observations as well as the guardian identity when exercising
lease pruning, so a fixture does not consult the host's process table.
The worktree test driver supplies synthetic idle memory, process capacity and
disk space; its process-identity checks still exercise native custody.
`tests/workspace_policy.rs` checks test and fixture Rust sources for direct sleep
calls and imports. The bounded wait helper owns the polling delay.

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

This table is the policy source for tests that must run natively on macOS.
`scripts/check-native-inventory` requires each row to appear in the macOS
nextest list and pass exactly once in its named invocation with the required
environment, and `scripts/check-nextest-filters` reads the same rows. The checks
cannot find a native test that has no row, so add a row with the test.

| Exact test name | Platform | Invocation in `rust-macos-tests` | Required environment | Nextest binary ID |
|---|---|---|---|---|
| `tests::staged_skills_native_probes` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant-providers` |
| `permissions::tests::escape::native_sandbox_blocks_path_escape_probes` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant-confinement` |
| `run::provider::project_paths::tests::configured_shared_path_allows_lane_lock_contended_by_host` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant-cli` |
| `lane_prepare_isolates_nested_writer_state` | macOS | `capability` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_LANE_TOOLS_REQUIRED` | `provenant::worktrees` |
| `writer_production_plan_denies_outside_writes` | macOS | `dispatch` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_LANE_TOOLS_REQUIRED` | `provenant::worktrees` |
| `worktrees::census_tests::native_full_census_places_clean_merged_worktree_and_same_user_sleep` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-confinement` |
| `worktrees::census_tests::native_unreadable_windowserver_allows_cleanup_of_a_merged_clean_tree` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-confinement` |
| `sys::pid::tests::native_occupancy_reports_image_and_open_file_with_identity_completeness` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-os` |
| `sys::pid::tests::native_occupancy_never_proves_an_unqueryable_process_empty` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-os` |
| `sys::pid::tests::native_current_process_is_not_an_eligible_platform_daemon` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-os` |
| `sys::pid::tests::native_same_user_sleep_is_not_an_eligible_daemon` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-os` |
| `sys::pid::tests::native_offsealed_platform_process_is_refused_when_present` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | `provenant-os` |
| `command_custom_shebang_runtime_keeps_credentials_and_writes_denied` | macOS | `provider-dispatch` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant::command_adapter` |
| `tests::claude_network_on_native_tls_files_are_readable` | macOS | `tls` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant-providers` |
| `opencode::tests::read_only_search_runs_in_native_profile` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | `provenant-providers` |

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

The macOS test job has a 20-minute ceiling and a 15-minute target, in both
warm and cold-cache runs (cold runs: see below). Keep the serial writer and isolated census
invocations. Run `scripts/nextest-slowest` on a run's JUnit reports for its
slowest cases; do not keep a list of them here.

Keep the full-run list conservative when introducing build-time inputs or
workspace-wide configuration. The `main` push remains the full-workspace
backstop if a path is missed.

Each Rust cache step caches `$GITHUB_WORKSPACE/tools/front-door/target`,
including Cargo's nested build-dir layout. A `build.build-dir` outside that tree
is not cached unless its path is added to the action's `cache-directories`.

## Reproduce a CI failure locally

When a suite passes locally but fails in CI, the difference is usually
developer configuration. Vary `HOME` before `PATH`: run the suite with `HOME`
pointed at an empty directory, and change one environment axis per run. Do not
model CI with a minimal allowlist `PATH`, which manufactures unrelated
failures; subtract only the named executables under suspicion from the real
`PATH`. Record which single axis changed the outcome.

## Advisory test signals

[`test-signals.yml`](../../.github/workflows/test-signals.yml) never joins
`ci-status`; its scripts always exit 0 and report through warning annotations
and job summaries.

- `scripts/assertion-diff` flags `assert_eq!` replaced by `assert!`, a
  specific variant check replaced by `is_err()`, and removed assertions, tests
  or `#[should_panic]`. It is a line heuristic, so expect noise.
- The stress run repeats only the tests a pull request added or changed, with
  nextest `--stress-count 20` and no retries.
- `scripts/run-mutants` runs `cargo-mutants --in-diff` nightly and on a pull
  request labelled `mutants`, skipping files that mention `unsafe`, with at most
  40 mutants. A diff that touches only tests gets no mutation signal.

## Flaky-test quarantine

Add a flaky test to `tools/front-door/.config/quarantine.toml` only while it
has an open issue, an owner and an expiry within 30 days. CI validates each
entry against `cargo nextest list`, excludes quarantined tests from the
blocking partitions, then runs them in an advisory step that reports their
result. Remove the entry in the PR that fixes the test.

To measure a cold run, dispatch the CI workflow on the desired branch with `cold_cache` set to `true` (for example, `gh workflow run ci.yml --ref BRANCH -f cold_cache=true`). The run uses fresh Rust cache keys without deleting existing caches.

## Tool downloads

Workflow tools that are not installed by `taiki-e/install-action` come from
`scripts/download-with-retry`. It retries connection failures, HTTP 408, 429
and 5xx responses with doubling backoff, gives up after 180 seconds or five
attempts, and fails at once on any other HTTP error. `scripts/test-download-with-retry`
covers that policy against a local server.

Each workflow step that installs a tool has a `timeout-minutes`, so a stalled
install fails rather than retrying until the job is cancelled. The shared
`.github/actions/install-nextest` action pins nextest in one place, tries the
prebuilt download first, and falls back to `cargo install --locked` at the same
version when the download fails. Its calling workflow step supplies the timeout
because composite actions do not support `timeout-minutes` on their inner steps. The
actionlint and lychee archives are cached under a key that names their versions
and checksums. Every run still verifies the pinned checksum before extracting
them. Changing a pin changes its key, so update both together.
