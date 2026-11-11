# Continuous integration
Owner: repository maintainers
Anchor: [`ci.yml`](../../.github/workflows/ci.yml); [`check-native-inventory`](../../scripts/check-native-inventory)

## Rust front-door tests

Keep fixtures independent of host capacity and time: inject `Resources`
observations, an `AdmissionPolicy` and `provenant_core::TestClock` at admission
boundaries, and wait on a named condition with `provenant_test_support::wait_for`
and a bounded deadline instead of sleeping (`tests/workspace_policy.rs` rejects
direct sleeps). Tests of native process or filesystem behaviour may still need
real host facts.

Pull requests run the tests for each changed workspace crate and its dependents,
from a deterministic `cargo metadata` graph. Pushes to `main`, and any changed
path outside a crate directory or that cannot be resolved safely, run the full
workspace. These paths always force the full workspace: `tools/front-door/Cargo.lock`,
`Cargo.toml`, `rust-toolchain.toml` and `.config/nextest.toml`,
`.github/workflows/ci.yml`, any `build.rs`, `tools/front-door/assets/`, and
`skills/` and `roles/` assets. Keep that list conservative when adding
build-time inputs; the `main` push is the backstop if a path is missed.

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

## Time budgets and caches

Pass `--success-output immediate` to `cargo nextest run` to see `SKIP #n` lines
from passing tests whose native prerequisite is missing; the
`PROVENANT_CONFINEMENT_REQUIRED`, `PROVENANT_POSTGRES_REQUIRED` and
`PROVENANT_LANE_TOOLS_REQUIRED` variables make those checks fail instead.

Nextest marks a test slow after 10 seconds and stops it after three intervals.
Keep the CI retry count and `fail-on-flaky` unchanged; a test that needs more
time gets a commented override in `tools/front-door/.config/nextest.toml`. The
macOS job has a 20-minute ceiling and a 15-minute target, warm or cold; keep the
serial writer and isolated census invocations. Find slow cases with
`scripts/nextest-slowest` on a run's JUnit reports. Rust caches cover
`$GITHUB_WORKSPACE/tools/front-door/target`; a `build.build-dir` outside it must be added to the
action's `cache-directories`. To measure a cold run:
`gh workflow run ci.yml --ref BRANCH -f cold_cache=true` (fresh cache keys, no
caches deleted).

## Reproduce a CI failure locally

A suite that passes locally and fails in CI usually differs by developer
configuration. Vary `HOME` before `PATH`: run with `HOME` set to an empty
directory, one environment axis per run, and subtract only the named executables
under suspicion from the real `PATH` (a minimal allowlist `PATH` manufactures
unrelated failures). Record which axis changed the outcome.

## Advisory signals and quarantine

[`test-signals.yml`](../../.github/workflows/test-signals.yml) never joins
`ci-status` and always exits 0: `scripts/assertion-diff` flags weakened
assertions (a noisy line heuristic), the stress run repeats added or changed
tests with `--stress-count 20`, and `scripts/run-mutants` mutates diffs nightly
and on PRs labelled `mutants`.

Quarantine a flaky test in `tools/front-door/.config/quarantine.toml` only while
it has an open issue, an owner and an expiry within 30 days. CI validates entries
against `cargo nextest list`, excludes them from blocking partitions and reports
their result in an advisory step. Remove the entry in the PR that fixes the test.

## Tool downloads

Workflow tools not installed by `taiki-e/install-action` use
`scripts/download-with-retry` (doubling backoff on connection failures, 408, 429
and 5xx; gives up after 180 seconds or five attempts; other HTTP errors fail at
once). Every tool-install step has a `timeout-minutes`. The
`.github/actions/install-nextest` action pins nextest once and falls back to
`cargo install --locked`; its calling step supplies the timeout because
composite actions cannot. Tool archive caches are keyed by version and checksum,
so change a pin and its key together.
