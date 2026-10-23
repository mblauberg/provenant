# Rust lane supervisor: deletable test inventory

Companion to [the spec](rust-supervisor.md). Measured on `origin/main` at
`59bb0ba4`. A count is the test body's span (decorators included; for
TypeScript, the lines from one top-level `test(` to the next). File headers,
imports and shared helpers are not counted. A test is deleted with its slice's
Python fallback, never when Rust first lands. The baseline includes a 49-line
S1 installer-template test group, removed with the release cut-over and omitted
below. Names include later watchdog regressions; counts otherwise remain based
on the stated baseline.

**Firm** tests check only mechanics that Rust takes over and that the lane
conformance suite or Rust unit tests cover. **Conditional** tests go only when
the stated condition holds; otherwise they stay.

| Slice | Firm tests | Firm lines | Conditional tests | Conditional lines |
|---|---|---|---|---|
| S1 | 119 | 1,500 | 0 | 0 |
| S2 | 70 | 1,826 | 4 | 75 |
| S3 | 23 | 830 | 3 | 98 |
| S4 | 36 | 832 | 35 | 758 |
| **Total** | **248** | **4,988** | **42** | **931** |

For scale, all test bodies on that commit span 69,537 lines (59,745 in 3,182
Python tests, 9,792 in TypeScript), in 88,941 lines of test files. Firm
deletions are 7.2% of test-body lines and 5.6% of test-file lines; with the
conditional ones, 8.5% and 6.7%. Helpers used only by firm deletions add 137
lines, and the parity suite's support files (`parity_support.py`,
`conftest.py`, `bench_front_door.py`, 485 lines) go except for the git helpers
`test_stale.py` keeps. The firm Python deletions hold 68 of the 153 `sleep(`
calls and 157 of the 224 signal and kill references in Python tests.

## S1, firm

S1b removes the following Python-only tests and parity support after S1a's
switch. The binary advisory coverage is retained as `tests/test_stale.py`;
routing, resolution and exec cases are owned by `tools/front-door/tests/`.
Installer and install-drift tests use Rust binaries or stamped build fixtures.
The user dropped timed trials on 2026-10-23; see the supervisor spec.

- `tests/front_door_parity/test_check_custody.py`: whole file, 14 tests, 148 lines
- `tests/front_door_parity/test_dispatch.py`: whole file, 19 tests, 226 lines
- `tests/front_door_parity/test_env_exec.py`: whole file, 15 tests, 156 lines
- `tests/front_door_parity/test_help.py`: whole file, 10 tests, 129 lines
- `tests/front_door_parity/test_install.py`: whole file, 7 tests, 102 lines
- `tests/front_door_parity/test_resolution.py`: whole file, 20 tests, 249 lines
- `tests/test_provenant_cli.py` (26 tests, 431 lines):
  - `test_installed_stub_contains_only_resolution_and_delegation`
  - `test_route_preserves_argv_environment_stdio_and_exit_status_through_symlink`
  - `test_capabilities_delegates_to_the_existing_discovery_owner`
  - `test_root_prints_the_resolved_product_checkout`
  - `test_empty_root_environment_values_use_the_default_instance_pointer`
  - `test_stale_pointer_fails_loudly_instead_of_falling_back_to_fused_checkout`
  - `test_worktree_runs_from_arbitrary_cwd_without_changing_it`
  - `test_fabric_reads_delegate_to_the_fabric_cli`
  - `test_installed_copy_ignores_same_named_commands_in_its_bin_directory`
  - `test_split_root_preserves_instance_root_for_mcp_child`
  - `test_installed_stub_does_not_materialize_the_default_instance_root`
  - `test_checkout_dispatcher_does_not_inherit_agents_home_as_instance_root`
  - `test_checkout_dispatcher_resolves_the_instance_pointer_without_agents_home_fallback`
  - `test_explicit_product_root_overrides_instance_pointer_and_agents_home`
  - `test_installed_stub_delegates_non_mcp_commands_to_checkout_cli`
  - `test_non_git_product_nested_in_an_unrelated_repository_stays_non_git`
  - `test_owner_exec_failure_reports_command_context_without_traceback`
  - `test_every_delegated_command_preserves_each_supported_caller_cwd`
  - `test_check_and_fabric_delegate_without_reinterpreting_arguments`
  - `test_lease_maps_each_verb_onto_its_fabric_landing_command`
  - `test_lease_rejects_a_missing_or_unknown_verb`
  - `test_lease_help_prints_the_verb_mapping`
  - `test_dispatch_delegates_without_reinterpreting_arguments`
  - `test_missing_or_unknown_command_prints_usage_to_stderr_and_exits_2`
  - `test_help_is_concise_and_names_existing_command_owners`
  - `test_help_command_gives_detail_for_specific_command`
- `tests/test_check_provenant_install.py` (4 tests, 39 lines):
  - `test_check_accepts_an_installed_stub_matching_the_template`
  - `test_check_rejects_installed_stub_drift`
  - `test_check_still_accepts_the_template_when_a_crate_exists`
  - `test_check_names_the_python_front_door_for_the_template`
- `tests/test_front_door_build.py` (3 tests, 12 lines):
  - `test_python_mode_never_looks_at_the_crate`
  - `test_auto_without_a_crate_is_the_template_with_no_note`
  - `test_auto_without_cargo_falls_back_with_the_reason`
- `tests/test_clean.py` (1 test, 8 lines):
  - `test_installed_template_routes_clean_to_product_owner`

## S2, firm

- `tests/test_provider_exec.py` (57 tests, 1,281 lines):
  - `test_unknown_cpu_census_is_not_zero_cpu_progress`
  - `test_new_session_grandchild_does_not_outlive_attempt`
  - `test_unobserved_double_fork_is_not_reaped_on_marker_alone`
  - `test_attempt_marker_matches_inherited_environment_only`
  - `test_snapshot_exception_still_kills_provider_and_records_attempt`
  - `test_internal_census_failure_warns_and_kills_provider`
  - `test_linux_snapshot_replaces_non_utf8_process_names`
  - `test_darwin_libproc_is_loaded_once`
  - `test_linux_tree_census_walks_task_children`
  - `test_linux_targeted_tree_census_does_not_warn_without_owner_row`
  - `test_watchdog_census_runs_about_once_per_second`
  - `test_missing_census_still_kills_provider_group_after_leader_exit`
  - `test_cleanly_exiting_child_is_not_reported_as_reaped`
  - `test_root_provider_keeps_two_second_sigterm_flush_grace`
  - `test_forged_fabric_token_does_not_spare_command`
  - `test_tracked_pre_exec_child_is_rechecked_before_signal`
  - `test_owner_promoted_at_signal_is_not_reported_as_reaped`
  - `test_verified_owner_stays_spared_through_transient_validation_loss`
  - `test_partial_census_keeps_a_live_verified_owner_group_spared`
  - `test_unavailable_census_preserves_verified_owner_and_root_kill`
  - `test_verified_owner_identity_does_not_spare_reused_pid`
  - `test_provider_exit_with_open_pipe_stops_once_and_keeps_reaped`
  - `test_token_without_owner_record_is_not_nested_owner`
  - `test_cleanup_does_not_adopt_marker_host_or_signal_a_descendants_shared_group`
  - `test_observed_code_mode_host_and_its_sibling_lanes_are_never_signalled`
  - `test_shared_host_group_change_then_missing_census_never_allows_group_signal`
  - `test_owner_validation_error_cannot_prevent_root_group_kill`
  - `test_forged_same_group_owner_cannot_suppress_provider_kill`
  - `test_owner_without_observed_parent_is_not_spared`
  - `test_reparented_marker_owner_without_ancestry_is_not_adopted`
  - `test_nested_owner_record_must_match_live_identity`
  - `test_nested_owner_accepts_legacy_inherited_locale_start`
  - `test_nested_owner_accepts_canonical_ps_when_computed_start_differs`
  - `test_nested_fabric_owner_and_its_child_are_spared`
  - `test_terminal_grace_with_live_claude_skips_settle_and_eof_child`
  - `test_all_adapters_observe_silence_without_killing_live_provider`
  - `test_all_adapters_wall_deadline_terminates_silent_provider`
  - `test_capture_registration_failure_returns_typed_receipt`
  - `test_silent_provider_fails_as_startup_timeout_and_is_reaped`
  - `test_provider_output_before_startup_window_keeps_running`
  - `test_cursor_terminal_event_closes_open_stdin_and_reaps_descendants`
  - `test_wall_timeout_terminates_without_output`
  - `test_capture_rollover_writes_linear_bytes_and_preserves_terminal_result`
  - `test_writer_watchdog_observation_does_not_reset_silence`
  - `test_cancel_allows_provider_session_flush`
  - `test_writer_progress_probe_is_bounded_and_reaches_late_files`
  - `test_writer_progress_uses_string_keys_for_large_tree_memory`
  - `test_writer_progress_advances_when_probe_budget_expires_immediately`
  - `test_writer_progress_tolerates_coarse_creation_clock`
  - `test_writer_wall_deadline_does_not_wait_for_silence_interval`
  - `test_writer_progress_sees_preserved_mtime_copy_during_initial_scan`
  - `test_writer_progress_sees_preserved_mtime_copy_at_root`
  - `test_writer_progress_tracks_deletion_without_counting_excluded_paths`
  - `test_writer_progress_ignores_new_excluded_and_ignored_paths`
  - `test_subreaper_is_held_only_while_attempts_run`
  - `test_attempt_restores_the_callers_subreaper_setting`
  - `test_subreaper_is_released_when_attempt_cleanup_raises`
  - `test_capture_keeps_tail_when_disk_writes_fail`
  - `test_handler_diagnostics_tolerate_enospc`
- `tests/test_dispatch_run.py` (9 tests, 432 lines):
  - `test_timeout_records_reaped_exit`
  - `test_sigterm_interrupts_and_reaps_provider_group`
  - `test_signal_during_popen_return_reaps_provider_and_publishes_cancelled_attempt`
  - `test_external_task_cancel_reaps_only_owned_provider_group`
  - `test_normal_attempt_records_reaped_new_session_child`
  - `test_parent_cleanup_spares_real_direct_nested_cli`
  - `test_malformed_adapter_receipt_is_fail_closed`
  - `test_incomplete_success_receipt_is_fail_closed`
  - `test_progress_bookkeeping_enospc_does_not_signal_provider`
- `tests/test_cf_dispatch.py` (4 tests, 113 lines):
  - `test_opencode_dispatch_group_signal_terminates_provider_and_grandchild`
  - `test_opencode_normal_exit_reaps_grandchild`
  - `test_opencode_silence_observation_keeps_provider_alive`
  - `test_opencode_stderr_progress_keeps_writer_alive`

## S2, conditional

The Python status mapping these pin stays; each is rewritten as a pure-
function test over a synthetic `supervisor.exit.json`, and only the process-
driven body goes.

- `tests/test_provider_exec.py` (4 tests, 75 lines):
  - `test_capture_is_bounded_but_result_is_complete`
  - `test_external_provider_sigterm_without_a_result_fails_unless_cancelled`
  - `test_supervisor_signal_is_interrupted_without_a_cancel_request`
  - `test_markerless_cancel_keeps_cancelled_when_provider_flushes_and_exits_zero`

## S3, firm

- `tests/test_dispatch_run.py` (22 tests, 761 lines):
  - `test_cancel_cascade_does_not_signal_forged_owner`
  - `test_cancel_cascade_still_cancels_genuine_child`
  - `test_cancel_cascade_rejects_matching_sibling_environment`
  - `test_cancel_cascade_preserves_observed_owner_after_provider_exit`
  - `test_cancel_cascade_reports_owner_verification_failure`
  - `test_cancel_cascade_rejects_owner_replaced_after_enumeration`
  - `test_cancel_cascade_rejects_inherited_parent_identity`
  - `test_cancel_cascade_records_unavailable_process_census`
  - `test_cancel_cascade_retries_transient_skip_at_publication`
  - `test_cancel_cascade_rechecks_identity_before_each_signal`
  - `test_busy_nested_cli_preserves_the_active_owner_record`
  - `test_record_owner_keeps_the_launcher_host_the_reaper_reads`
  - `test_record_owner_without_a_launcher_hosts_itself`
  - `test_record_owner_inherits_token_only_for_the_same_run`
  - `test_attempt_publication_waits_for_custody`
  - `test_terminal_publication_includes_child_registered_while_waiting_for_custody`
  - `test_manifest_append_failure_retains_terminal_attempt`
  - `test_batch_marker_prelaunch_cancellation_has_no_provider_pid`
  - `test_real_dispatcher_stops_group_releases_writer_lease_and_closes_receipt`
  - `test_real_dispatcher_restores_signal_handlers`
  - `test_late_signal_after_provider_exit_preserves_attempt_publication`
  - `test_retire_provider_preserves_another_attempt_marker`
- `tests/test_provider_exec.py` (1 test, 69 lines):
  - `test_cancelled_attempt_stops_nested_children_even_when_cleanup_fails`

## S3, conditional

Only once no Python owner writes attempt bundles (S3's fallback deleted): a
Rust-committed bundle is complete when `attempt.json` reads terminal, so
readers no longer wait through gaps.

- `tests/test_run_controls.py` (3 tests, 98 lines):
  - `test_cancel_waits_through_attempt_sidecar_gap`
  - `test_attempt_terminal_tolerates_receipt_renamed_before_its_digest_sidecar`
  - `test_cancel_waits_through_late_summary_bundle_completion`

## S4, firm

- `tests/test_dispatch_run.py` (9 tests, 207 lines):
  - `test_reap_attempt_after_group_sigkill_is_terminal_and_idempotent`
  - `test_reap_attempt_identity_and_legacy_progress`
  - `test_reap_attempt_never_touches_live_owner`
  - `test_reap_attempt_preserves_terminal_publication_after_snapshot`
  - `test_reap_attempt_skips_busy_publisher_and_releases_run_custody`
  - `test_reap_attempt_refreshes_stop_evidence_after_snapshot`
  - `test_reap_attempt_owners_without_run_token`
  - `test_process_evidence_without_start_time_is_unknown`
  - `test_reap_attempt_recovers_missing_pgid`
  - `test_reaper_checkpoint_failure_still_publishes_terminal`
  - `test_reaper_git_summaries_share_one_pass_budget`
- `tests/test_exec_layout.py` (1 test, 16 lines):
  - `test_orphan_reaping_does_not_close_a_live_unindexed_provider`
- `runtime/fabric/tests/process-reaping.test.ts`: whole file, 3 tests, 131 lines
- `runtime/fabric/tests/host-liveness.test.ts`: whole file, 6 tests, 38 lines
- `runtime/fabric/tests/execution-lifecycle.test.ts` (15 tests, 384 lines):
  - `records the owner pid and process group inside the run directory`
  - `removes the record when the owner exits`
  - `keeps custody until a provider outliving its owner has stopped`
  - `holds owner cleanup through host shutdown until a resistant provider stops`
  - `signals the owner process group, so the provider child dies too`
  - `keeps a batch lane alive and readable after its MCP host exits`
  - `leaves no running provider process after its MCP host is killed`
  - `keeps the owner record until an orphan that ignores SIGTERM is killed`
  - `does not mask a still-running orphan with the host-gone reason`
  - `reaps orphans on the dispatch path without a daemon`
  - `keeps a CLI-dispatched run alive when a later dispatch reaps orphans`
  - `reaps a CLI-dispatched provider once its owner exits`
  - `never reaps a run whose host is still alive`
  - `reads final files when the owner finishes during the liveness probe`
  - `retains custody when a live owner has no verifiable start identity`
- `runtime/fabric/tests/named-sessions.test.ts` (2 tests, 56 lines):
  - `releases a finished turn whose retained provider record names a reused pid with no start time`
  - `keeps an MCP turn busy while its orphan provider with no start time outlives the owner`

## S4, conditional

`worker_liveness.py` stays: the direct-CLI fallback still runs workers outside
Fabric, so `tests/test_worker_liveness.py` is listed below only for when that
fallback retires. The other tests go only if `clean.py`, the `exec_layout`
orphan scan, `batch_run.py` and Fabric's kill and prune paths take liveness
from `lane status`; otherwise they stay.

- `tests/test_exec_layout.py` (1 test, 24 lines):
  - `test_orphan_reaping_uses_shared_root_and_leaves_live_receipts`
- `tests/test_clean.py` (1 test, 18 lines):
  - `test_pid_alive_accepts_legacy_locale_start_after_canonical_check`
- `tests/test_worker_liveness.py`: whole file, 26 tests, 397 lines
- `tests/test_batch_run.py` (1 test, 43 lines):
  - `test_real_dispatch_cancellation_reaps_provider_process`
- `runtime/fabric/tests/execution-lifecycle.test.ts` (3 tests, 182 lines):
  - `prints an unconfirmed stop reason from dispatch kill`
  - `fails dispatch kill after signalling only the owner of a live provider`
  - `cancels a cold-start run before the attempt directory exists`
- `runtime/fabric/tests/execution-pruning.test.ts` (3 tests, 94 lines):
  - `does not report interrupted while a finalising owner is alive`
  - `never prunes a run still in flight`
  - `keeps an aged run while its recorded provider session is live`
