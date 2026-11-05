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

| Exact test name | Platform | Invocation in `rust-macos-tests` | Required environment | Prerequisite tools | Nextest binary ID |
|---|---|---|---|---|---|
| `tests::staged_skills_native_probes` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | sandbox-exec | `provenant-providers` |
| `permissions::tests::escape::native_sandbox_blocks_path_escape_probes` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | sandbox-exec | `provenant-confinement` |
| `lane_prepare_isolates_nested_writer_state` | macOS | `capability` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_LANE_TOOLS_REQUIRED` | sandbox-exec, git, rg, cargo, cargo-nextest, swift, node, pnpm, python3 | `provenant::worktrees` |
| `writer_production_plan_denies_outside_writes` | macOS | `dispatch` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_LANE_TOOLS_REQUIRED` | sandbox-exec, git | `provenant::worktrees` |
| `writer_protected_changes_wait_for_review_and_ignore_upstream` | macOS | `dispatch` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_LANE_TOOLS_REQUIRED` | sandbox-exec, git | `provenant::worktrees` |
| `worktrees::census_tests::native_full_census_places_clean_merged_worktree_and_same_user_sleep` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | git, sleep | `provenant-confinement` |
| `worktrees::census_tests::native_unreadable_windowserver_allows_cleanup_of_a_merged_clean_tree` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | git | `provenant-confinement` |
| `sys::pid::tests::native_occupancy_reports_image_and_open_file_with_identity_completeness` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | sleep | `provenant-os` |
| `sys::pid::tests::native_occupancy_never_proves_an_unqueryable_process_empty` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | sleep | `provenant-os` |
| `sys::pid::tests::native_current_process_is_not_an_eligible_platform_daemon` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | sleep | `provenant-os` |
| `sys::pid::tests::native_same_user_sleep_is_not_an_eligible_daemon` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | sleep | `provenant-os` |
| `sys::pid::tests::native_offsealed_platform_process_is_refused_when_present` | macOS | `census` | `PROVENANT_CONFINEMENT_REQUIRED, PROVENANT_POSTGRES_REQUIRED` | sleep | `provenant-os` |
| `command_custom_shebang_runtime_keeps_credentials_and_writes_denied` | macOS | `provider-dispatch` | `PROVENANT_CONFINEMENT_REQUIRED` | sandbox-exec | `provenant::command_adapter` |
| `tests::claude_network_on_native_tls_files_are_readable` | macOS | `tls` | `PROVENANT_CONFINEMENT_REQUIRED` | sandbox-exec | `provenant-providers` |
| `opencode::tests::read_only_search_runs_in_native_profile` | macOS | `custody` | `PROVENANT_CONFINEMENT_REQUIRED` | rg, sandbox-exec | `provenant-providers` |

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
dispatch confinement 2m55s, and provider dispatch 1m27s. The baseline queue, setup, cache state and slowest cases were not recorded.

At reviewed head `f99b107d7`, [run 41787629232, attempt 1](https://github.com/mblauberg/provenant/actions/runs/41787629232/attempts/1) used a warm cache. The macOS test job took 700 s, release smoke 297 s, front door shards 301 s and 285 s, lint 116 s, and changed-test stress 920 s. The whole run took 12m05s (06:45:15–06:57:20 UTC). The macOS job started at 06:45:32, 17 s after the run began (including the scope dependency, not a separately measured queue). Setup through nextest installation took 23 s; lint 65 s, archive 163 s, native tool installation 46 s, list 3 s, custody 153 s, Cargo seed 4 s, dispatch 165 s, provider dispatch 70 s, and TLS 3 s. The single job meets 15 minutes in this warm run; this does not establish cold-cache headroom. A cold-cache run and its slowest tests remain unmeasured. Keep the serial writer and isolated census invocations, and compare a shared archive with independent builds if future measurements justify splitting the job.

The 20 slowest cases in that run's preserved native JUnit reports (seconds):

| Test (binary) | Seconds |
|---|---:|
| `read_only_lane_builds_and_tests_swift_in_scratch` (`provenant::worktrees`) | 33.254 |
| `lane_builds_and_tests_swift_in_a_registered_worktree` (`provenant::worktrees`) | 30.617 |
| `opus_writer_dispatch_advises_a_cheaper_route` (`provenant::dispatch_provider`) | 15.045 |
| `review_fans_out_and_collects_fake_provider` (`provenant::dispatch_provider`) | 14.046 |
| `lane_prepare_isolates_nested_writer_state` (`provenant::worktrees`) | 13.255 |
| `opencode_dispatch_observes_v2_wal_session` (`provenant::dispatch_provider`) | 12.012 |
| `lane_prepare_isolates_nested_publishing_state` (`provenant::worktrees`) | 11.448 |
| `lane_prepare_isolates_nested_state` (`provenant::worktrees`) | 10.771 |
| `interrupted_progress_recovers_on_another_account_in_the_same_worktree` (`provenant::dispatch_provider`) | 10.338 |
| `writer_protected_changes_wait_for_review_and_ignore_upstream` (`provenant::worktrees`) | 10.257 |
| `command_network_is_per_task_through_manifest_and_single_task_route` (`provenant::command_adapter`) | 9.895 |
| `graph_settlement_advances_writer_and_account_routed_review` (`provenant::dispatch_provider`) | 9.819 |
| `command_lane_reads_its_supervisors_process_tree_through_the_cli` (`provenant::command_adapter`) | 9.586 |
| `worktrees::lifecycle_tests::remote_base_uses_exact_advertisement_instead_of_tracking_ref` (`provenant-confinement`) | 9.470 |
| `owned_dispatch_retains_failed_attempt_and_replays` (`provenant::dispatch_provider`) | 9.343 |
| `quarantine::custody_integration_tests::quarantine_attributes_protected_changes_against_delivery_merge_base` (`provenant-confinement`) | 9.309 |
| `worktrees::lifecycle_tests::gc_merge_proof_requires_containment_in_the_pinned_default` (`provenant-confinement`) | 9.148 |
| `codex_dispatch_observes_rollout` (`provenant::dispatch_provider`) | 8.870 |
| `writer_dispatch_edits_a_linked_worktree` (`provenant::worktrees`) | 8.805 |
| `unconfigured_host_native_repair_launches_alias_fallback` (`provenant::dispatch_provider`) | 8.080 |

Keep the full-run list conservative when introducing build-time inputs or
workspace-wide configuration. The `main` push remains the full-workspace
backstop if a path is missed.

## Flaky-test quarantine

Add a flaky test to `tools/front-door/.config/quarantine.toml` only while it
has an open issue, an owner and an expiry within 30 days. CI validates each
entry against `cargo nextest list`, excludes quarantined tests from the
blocking partitions, then runs them in an advisory step that reports their
result. Remove the entry in the PR that fixes the test.
