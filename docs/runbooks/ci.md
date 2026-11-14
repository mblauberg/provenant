# Continuous integration
Owner: repository maintainers
Anchor: [`ci.yml`](../../.github/workflows/ci.yml); [`check-native-inventory`](../../scripts/check-native-inventory)

## Rust front-door tests

Keep fixtures independent of host capacity and time: inject `Resources`
observations, an `AdmissionPolicy` and `provenant_core::TestClock` at admission
boundaries, and wait on a named condition with `provenant_test_support::wait_for`
and a bounded deadline instead of sleeping (Clippy rejects
direct sleeps through `clippy.toml`). Tests of native process or filesystem
behaviour may still need real host facts.

## Pipeline

`ci-status` is the single required check (also named in `MAINTAINING.md` and
`github-workflow.md`). PRs select from the merge-base diff. Main and `land/**`
pushes always run the full pipeline; missing Git history selects conservatively.

The workflow and selectors define the jobs and their scope. Pull requests select
affected crates and reverse dependencies; uncertain selection expands coverage.
See [`select-affected-crate-tests`](../../scripts/select-affected-crate-tests)
and [`select-macos-checks`](../../scripts/select-macos-checks). Prose changes
still run documentation and harness checks. See [test tiers](test-tiers.md)
for host verification.

## Required native test inventory

This table is the policy source for tests that must run natively on macOS.
The inventory scripts check the rows against native test results. Add a row
when introducing a test that needs native coverage.

| Exact test name | Platform | Native invocation | Required environment | Nextest binary ID |
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

Keep CI reliable and bounded. Consult the workflow and nextest configuration
for current budgets, cache and retry settings; investigate failures rather than
pinning incidental timing or counts. Policy checks live in the repository's
scripts and tests.

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

Use [`download-with-retry`](../../scripts/download-with-retry) for workflow
tool downloads. Keep a tool's version and cache key aligned.
