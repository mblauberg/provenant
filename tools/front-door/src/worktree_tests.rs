// Integration fixtures use real time to bound process coordination.
#![allow(clippy::disallowed_methods)]

#[path = "../tests/support/mod.rs"]
pub(super) mod support;
use provenant_core::Clock;
use provenant_os::exec::{ExecOutcome, Executor};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
use support::{Result, TestRig, code};

mod writer_cases;
use writer_cases::*;
mod gc_cases;
use gc_cases::*;
mod worktree_cases;
use worktree_cases::*;
mod lane_cases;
pub(crate) use lane_cases::lane_provider;
use lane_cases::*;
mod graph_cases;
use graph_cases::*;

// Only the harness=false target dispatches with these seams. The installed
// binary has no fixture flag, feature or environment override.
struct FreeWorkers;
impl provenant_confinement::worktrees::WorkerObserver for FreeWorkers {
    fn observe_roots(
        &self,
        roots: &[std::path::PathBuf],
    ) -> std::collections::BTreeMap<std::path::PathBuf, provenant_confinement::worktrees::Occupancy>
    {
        roots
            .iter()
            .cloned()
            .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
            .collect()
    }
}

struct UnknownWorkers;
impl provenant_confinement::worktrees::WorkerObserver for UnknownWorkers {
    fn observe_roots(
        &self,
        roots: &[PathBuf],
    ) -> BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy> {
        static OBSERVATIONS: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let late = std::env::var("FIXTURE_UNKNOWN_WORKERS").is_ok_and(|mode| mode == "late");
        let free = late && OBSERVATIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2;
        roots
            .iter()
            .cloned()
            .map(|root| {
                let observed = if free {
                    provenant_confinement::worktrees::Occupancy::Available
                } else {
                    provenant_confinement::worktrees::Occupancy::census_unknown(
                        root.clone(),
                        "fixture census unavailable",
                    )
                };
                (root, observed)
            })
            .collect()
    }
}

/// The driver's seams: free workers, the recorded boot and a holder that can
/// crash after durable exit.
struct Fixture;
/// Admission capacity is synthetic; process identity remains real for custody.
struct FixtureResources;
impl provenant_run::Resources for FixtureResources {
    fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
        use provenant_supervisor::census::ProcessObserver;
        provenant_supervisor::fixture::RecordedBootObserver.observe(pid)
    }
    fn memory(&self) -> std::io::Result<provenant_os::sys::resources::MemorySample> {
        Ok(provenant_os::sys::resources::MemorySample {
            available_bytes: 100,
            total_bytes: 100,
        })
    }
    fn disk(&self, _: &provenant_os::fs::Directory) -> std::io::Result<u64> {
        Ok(u64::MAX)
    }
}
impl crate::Process for Fixture {
    fn admission_resources(&self) -> &dyn provenant_run::Resources {
        &FixtureResources
    }
    fn workers(&self) -> &'static dyn provenant_confinement::worktrees::WorkerObserver {
        if std::env::var_os("FIXTURE_UNKNOWN_WORKERS").is_some() {
            &UnknownWorkers
        } else if std::env::var_os("FIXTURE_SYSTEM_WORKERS").is_some() {
            &provenant_confinement::worktrees::SystemWorkerObserver
        } else {
            &FreeWorkers
        }
    }
    fn holder(
        &self,
        holder: &dyn provenant_supervisor::Holder,
        guard: &mut dyn FnMut(&dyn provenant_supervisor::Holder) -> u8,
    ) -> u8 {
        guard(&FixtureHolder(holder))
    }
    fn supervise(
        &self,
        descriptors: &[OsString],
        mut hooks: provenant_supervisor::GuardianHooks<'_>,
    ) -> provenant_supervisor::custody::Result<()> {
        hooks.observer = Some(&provenant_supervisor::fixture::RecordedBootObserver);
        provenant_supervisor::supervise(descriptors, hooks)
    }
}

/// The worktree driver's entry. Its children re-enter it as the fake
/// provider or as the command edge with fixture seams.
pub fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let first = args.first().and_then(|arg| arg.to_str());
    let exit = |role: &str, result: Result| match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr().lock(), "{role}: {error}");
            ExitCode::FAILURE
        }
    };
    match first {
        // Fake provider identity probes for production account observation.
        Some("--version") => {
            let _ = writeln!(std::io::stdout().lock(), "fixture-provider 1.0.0");
            ExitCode::SUCCESS
        }
        Some("auth") => {
            let _ = writeln!(
                std::io::stdout().lock(),
                r#"{{"loggedIn":true,"email":"fixture@example.test"}}"#
            );
            ExitCode::SUCCESS
        }
        // The compose fixture's lanes run in its `compose` worktree; other
        // provider fixtures exercise lane MCP and nesting.
        Some("exec")
            if std::env::current_dir().is_ok_and(|cwd| cwd.ends_with(".worktrees/compose")) =>
        {
            exit("compose provider", crate::compose_tests::provider())
        }
        Some("exec" | "-p") => exit("lane provider", lane_provider()),
        Some("--writer-provider") => exit("writer provider", provider(&args)),
        // Bind a Unix-domain socket at the given path and connect to it.
        Some("--bind-socket") => exit("bind socket", bind_socket(&args)),
        // The fake `gh` of the `land` fixtures.
        Some("pr" | "repo" | "api") => {
            let args: Vec<String> = std::env::args().skip(1).collect();
            match crate::pr_land_tests::fake_gh(&args) {
                Ok(code) => ExitCode::from(code),
                Err(error) => {
                    let _ = writeln!(std::io::stderr().lock(), "fake gh: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        // The branch deletion of `land` with another process acting after
        // the cleanup check: `--land-race <merged head> [<git argument>...]`.
        Some("--land-race") => {
            let result = (|| -> Result<String> {
                let merged = args.get(1).and_then(|arg| arg.to_str()).ok_or("head")?;
                let between: Vec<&str> =
                    args.iter().skip(2).filter_map(|arg| arg.to_str()).collect();
                let (status, detail) = crate::pr::land::delete_after_check(
                    &std::env::current_dir()?,
                    merged.parse()?,
                    &between,
                )
                .map_err(|error| error.to_string())?;
                Ok(format!("{status}\n{detail}\n"))
            })();
            match result {
                Ok(text) => {
                    let _ = write!(std::io::stdout().lock(), "{text}");
                    ExitCode::SUCCESS
                }
                Err(error) => exit("land race", Err(error)),
            }
        }
        Some("--admission-only") => {
            let args = args.into_iter().skip(1);
            ExitCode::from(provenant_cli::run::with_deferred_provider_launch(|| {
                crate::dispatch(provenant_cli::parse(args), &Fixture)
            }))
        }
        Some("--state-dir" | "internal") => {
            let code = crate::dispatch(provenant_cli::parse(args), &Fixture);
            // A fixture-only observation after the real command has rendered.
            if let Some(path) = std::env::var_os("FIXTURE_LAND_CWD_FILE") {
                let result = std::env::current_dir()
                    .and_then(|cwd| fs::write(path, cwd.as_os_str().as_encoded_bytes()));
                if let Err(error) = result {
                    return exit("land cwd", Err(error.into()));
                }
            }
            ExitCode::from(code)
        }
        _ => exit(
            "worktree integration",
            suite(&args).map_err(|error| error.into()),
        ),
    }
}

/// The lane probe's socket step: `sun_path` on Darwin holds 103 bytes and a
/// terminator, so a lane `TMPDIR` that is too long fails here.
fn bind_socket(args: &[std::ffi::OsString]) -> Result {
    use std::os::unix::net::{UnixListener, UnixStream};
    let path = Path::new(args.get(1).ok_or("socket path")?);
    let length = path.as_os_str().len();
    if length >= 104 {
        return Err(format!("socket path is {length} bytes").into());
    }
    let listener = UnixListener::bind(path)?;
    drop(UnixStream::connect(path)?);
    listener.accept()?;
    fs::remove_file(path)?;
    writeln!(std::io::stdout().lock(), "lane-socket: PASS {length}")?;
    Ok(())
}

fn suite(args: &[std::ffi::OsString]) -> std::io::Result<()> {
    provenant_supervisor::fixture::run_libtest(
        args,
        &[
            "graph_stop_cancels_live_delivery_fetch_and_seed_preparation",
            "gc_fetches_default_and_removes_only_clean_merged_worktrees",
            "gc_output_requires_terminal_lanes_and_respects_keep_recent_dirty_and_links",
            "gc_collects_a_settled_lane_whose_completion_attempt_failed",
            "completion_and_sweep_retain_unlanded_or_dirty_lanes",
            "gc_collects_a_fast_forwarded_lane_its_settlement_missed",
            "gc_disk_alert_is_once_per_dip_and_manual_preview_does_not_latch",
            "graph_run_refuses_low_disk_writers",
            "gc_reports_terminal_tmp_paths_and_bytes_in_json_and_text",
            "lane_prepare_isolates_nested_state",
            "lane_prepare_isolates_nested_claude_state",
            "lane_prepare_isolates_nested_writer_state",
            "lane_prepare_isolates_nested_publishing_state",
            "lane_builds_and_tests_swift_in_a_registered_worktree",
            "read_only_lane_builds_and_tests_swift_in_scratch",
            "read_only_lane_builds_and_tests_cargo_in_scratch",
            "read_only_dispatch_applies_bound_git_and_project_writes",
            "writer_dispatch_uses_lane_cache_and_narrow_provider_state",
            "writer_graph_output_preserves_lens_diff_base",
            "writer_dispatch_edits_a_linked_worktree",
            "second_writer_is_refused_while_first_holds_the_worktree",
            "writer_settlement_merges_upstream_advance",
            "writer_terminal_refuses_sequencer_state",
            "writer_production_plan_denies_outside_writes",
            "writer_reconcile_refuses_unfinished_cherry_pick_and_releases",
            "recreated_worktree_accepts_writer_after_settlement",
            "existing_branch_with_base_names_the_repair_in_json_and_human_output",
            "create_from_remote_base_reports_ok_and_exists",
            "create_list_and_remove_a_managed_worktree",
            "gc_idle_keeps_live_and_unobservable_writers_and_removes_free_cache",
            "scheduled_gc_runs_outside_a_checkout_and_obeys_disable_and_writer_locks",
            "gc_trims_lane_build_directories_to_the_cap_and_spares_held_locks",
            "gc_reports_transient_and_preserves_unobserved_project_array",
            "gc_pressure_collects_only_the_selected_primary_project",
            "gc_refuses_bare_projects_and_their_linked_checkout_fallback",
            "provider_state_gc_sweeps_host_and_refuses_replaced_roots",
            "merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees",
            "merged_pruning_deletes_a_merged_branch_without_a_worktree",
            "remove_by_name_follows_the_branch_merge_state",
            "remove_by_name_deletes_a_branch_fully_contained_in_integration",
            "remove_resolves_directory_names_and_remote_integration_refs",
            "escaping_branch_is_invalid_and_writes_no_checkout_outside_worktree_root",
            "dispatch_into_git_worktree_without_prior_state",
            "dispatch_into_git_worktree_beside_managed_tree",
            "cross_project_writer_names_both_projects",
            "git_created_and_recreated_worktrees_are_dispatchable",
            "pr_land_merges_then_fetches_fast_forwards_and_prunes",
            "pr_land_default_batch_gates_then_cleans_up",
            "pr_land_prunes_the_selected_merged_worktree",
            "pr_land_from_lane_ignores_only_its_callers",
            "pr_land_refuses_incomplete_worker_census",
            "pr_land_canonicalises_quoted_worktree_paths",
            "pr_land_uses_writer_lock_and_preserves_attempt_recovery_evidence",
            "pr_land_writer_lock_preserves_dirty_and_output_cases",
            "pr_land_dirty_primary_does_not_block_later_batches",
            "pr_land_recovery_uses_recorded_merged_head",
            "pr_land_preserves_terminal_writer_evidence_in_host_store",
            "pr_land_primary_with_separate_git_directory",
            "pr_land_out_of_tree_with_separate_git_directory",
            "pr_land_refuses_each_unsafe_step_and_reports_the_rest",
            "pr_land_changes_nothing_when_the_merge_does_not_happen",
            "pr_land_default_batch_preserves_checkout_when_publication_is_unknown",
            "pr_land_leaves_a_local_namesake_of_a_fork_branch_alone",
            "pr_land_leaves_a_reused_branch_name_alone",
            "pr_land_deletes_an_unregistered_branch_only_at_the_merged_head",
            "pr_land_keeps_a_branch_held_by_a_paused_rebase",
            "pr_land_keeps_a_branch_held_by_a_bisect",
            "pr_land_keeps_a_branch_held_in_a_checkout_that_went_away",
            "pr_land_refuses_when_a_worktree_entry_cannot_be_read",
            "pr_land_refuses_when_worktree_state_is_malformed",
            "pr_open_refuses_what_the_receipts_cannot_support",
            "pr_contribution_roundtrip",
            "pr_contribution_unbound_project",
            "pr_contribution_land_warns_when_binding_fails",
        ],
        |name| {
            let result = match name {
                "pr_contribution_land_warns_when_binding_fails" => crate::pr_land_tests::pr_contribution_land_warns_when_binding_fails(),
                "pr_contribution_unbound_project" => crate::pr_land_tests::pr_contribution_unbound_project(),
                "pr_contribution_roundtrip" => crate::pr_land_tests::pr_contribution_roundtrip(),
                "graph_stop_cancels_live_delivery_fetch_and_seed_preparation" => graph_stop_cancels_live_delivery_fetch_and_seed_preparation(),
                "gc_fetches_default_and_removes_only_clean_merged_worktrees" => gc_fetches_default_and_removes_only_clean_merged_worktrees(),
                "gc_output_requires_terminal_lanes_and_respects_keep_recent_dirty_and_links" => gc_output_requires_terminal_lanes_and_respects_keep_recent_dirty_and_links(),
                "gc_collects_a_settled_lane_whose_completion_attempt_failed" => gc_collects_a_settled_lane_whose_completion_attempt_failed(),
                "completion_and_sweep_retain_unlanded_or_dirty_lanes" => completion_and_sweep_retain_unlanded_or_dirty_lanes(),
                "gc_collects_a_fast_forwarded_lane_its_settlement_missed" => gc_collects_a_fast_forwarded_lane_its_settlement_missed(),
                "gc_disk_alert_is_once_per_dip_and_manual_preview_does_not_latch" => gc_disk_alert_is_once_per_dip_and_manual_preview_does_not_latch(),
                "graph_run_refuses_low_disk_writers" => graph_run_refuses_low_disk_writers(),
                "gc_reports_terminal_tmp_paths_and_bytes_in_json_and_text" => gc_reports_terminal_tmp_paths_and_bytes_in_json_and_text(),
                "lane_prepare_isolates_nested_state" => lane_prepare_isolates_nested_state(),
                "lane_prepare_isolates_nested_claude_state" => lane_prepare_isolates_nested_claude_state(),
                "lane_prepare_isolates_nested_writer_state" => {
                    lane_prepare_isolates_nested_writer_state()
                }
                "lane_builds_and_tests_swift_in_a_registered_worktree" => {
                    lane_builds_and_tests_swift_in_a_registered_worktree()
                }
                "read_only_lane_builds_and_tests_swift_in_scratch" => {
                    read_only_lane_builds_and_tests_swift_in_scratch()
                }
                "read_only_lane_builds_and_tests_cargo_in_scratch" => {
                    read_only_lane_builds_and_tests_cargo_in_scratch()
                }
                "lane_prepare_isolates_nested_publishing_state" => {
                    lane_prepare_isolates_nested_publishing_state()
                }
                "read_only_dispatch_applies_bound_git_and_project_writes" => {
                    crate::compose_tests::read_only_dispatch_applies_bound_git_and_project_writes()
                }
                "writer_dispatch_uses_lane_cache_and_narrow_provider_state" => {
                    crate::compose_tests::writer_dispatch_uses_lane_cache_and_narrow_provider_state(
                    )
                }
                "writer_graph_output_preserves_lens_diff_base" => {
                    crate::fabric::peer_tests::writer_graph_output_preserves_lens_diff_base()
                }
                "writer_dispatch_edits_a_linked_worktree" => {
                    writer_dispatch_edits_a_linked_worktree()
                }
                "second_writer_is_refused_while_first_holds_the_worktree" => {
                    second_writer_is_refused_while_first_holds_the_worktree()
                }
                "writer_settlement_merges_upstream_advance" => {
                    writer_settlement_merges_upstream_advance()
                }
                "writer_terminal_refuses_sequencer_state" => {
                    writer_terminal_refuses_sequencer_state()
                }
                "writer_production_plan_denies_outside_writes" => {
                    writer_production_plan_denies_outside_writes()
                }
                "writer_reconcile_refuses_unfinished_cherry_pick_and_releases" => {
                    writer_reconcile_refuses_unfinished_cherry_pick_and_releases()
                }
                "recreated_worktree_accepts_writer_after_settlement" => {
                    recreated_worktree_accepts_writer_after_settlement()
                }
                "existing_branch_with_base_names_the_repair_in_json_and_human_output" => {
                    existing_branch_with_base_names_the_repair_in_json_and_human_output()
                }
                "create_from_remote_base_reports_ok_and_exists" => create_from_remote_base_reports_ok_and_exists(),
                "create_list_and_remove_a_managed_worktree" => {
                    create_list_and_remove_a_managed_worktree()
                }
                "gc_idle_keeps_live_and_unobservable_writers_and_removes_free_cache" => {
                    gc_idle_keeps_live_and_unobservable_writers_and_removes_free_cache()
                }
                "provider_state_gc_sweeps_host_and_refuses_replaced_roots" => {
                    crate::provider_retention::provider_state_gc_sweeps_host_and_refuses_replaced_roots()
                }
                "gc_pressure_collects_only_the_selected_primary_project" => gc_pressure_collects_only_the_selected_primary_project(),
                "gc_refuses_bare_projects_and_their_linked_checkout_fallback" => gc_refuses_bare_projects_and_their_linked_checkout_fallback(),
                "gc_reports_transient_and_preserves_unobserved_project_array" => gc_reports_transient_and_preserves_unobserved_project_array(),
                "scheduled_gc_runs_outside_a_checkout_and_obeys_disable_and_writer_locks" => {
                    scheduled_gc_runs_outside_a_checkout_and_obeys_disable_and_writer_locks()
                }
                "gc_trims_lane_build_directories_to_the_cap_and_spares_held_locks" => {
                    gc_trims_lane_build_directories_to_the_cap_and_spares_held_locks()
                }
                "merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees" => {
                    merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees()
                }
                "merged_pruning_deletes_a_merged_branch_without_a_worktree" => {
                    merged_pruning_deletes_a_merged_branch_without_a_worktree()
                }
                "remove_by_name_follows_the_branch_merge_state" => {
                    remove_by_name_follows_the_branch_merge_state()
                }
                "remove_by_name_deletes_a_branch_fully_contained_in_integration" => {
                    remove_by_name_deletes_a_branch_fully_contained_in_integration()
                }
                "remove_resolves_directory_names_and_remote_integration_refs" => {
                    remove_resolves_directory_names_and_remote_integration_refs()
                }
                "pr_land_default_batch_gates_then_cleans_up" => {
                    crate::pr_land_tests::pr_land_default_batch_gates_then_cleans_up()
                }
                "pr_land_merges_then_fetches_fast_forwards_and_prunes" => {
                    crate::pr_land_tests::pr_land_merges_then_fetches_fast_forwards_and_prunes()
                }
                "pr_land_prunes_the_selected_merged_worktree" => {
                    crate::pr_land_tests::pr_land_prunes_the_selected_merged_worktree()
                }
                "pr_land_from_lane_ignores_only_its_callers" => crate::pr_land_tests::pr_land_from_lane_ignores_only_its_callers(),
                "pr_land_refuses_incomplete_worker_census" => crate::pr_land_tests::pr_land_refuses_incomplete_worker_census(),
                "pr_land_canonicalises_quoted_worktree_paths" => crate::pr_land_tests::pr_land_canonicalises_quoted_worktree_paths(),
                "pr_land_uses_writer_lock_and_preserves_attempt_recovery_evidence" => crate::pr_land_tests::pr_land_uses_writer_lock_and_preserves_attempt_recovery_evidence(),
                "pr_land_writer_lock_preserves_dirty_and_output_cases" => crate::pr_land_tests::pr_land_writer_lock_preserves_dirty_and_output_cases(),
                "pr_land_dirty_primary_does_not_block_later_batches" => crate::pr_land_tests::pr_land_dirty_primary_does_not_block_later_batches(),
                "pr_land_recovery_uses_recorded_merged_head" => crate::pr_land_tests::pr_land_recovery_uses_recorded_merged_head(),
                "pr_land_preserves_terminal_writer_evidence_in_host_store" => crate::pr_land_tests::pr_land_preserves_terminal_writer_evidence_in_host_store(),
                "pr_land_refuses_each_unsafe_step_and_reports_the_rest" => {
                    crate::pr_land_tests::pr_land_refuses_each_unsafe_step_and_reports_the_rest()
                }
                "pr_land_primary_with_separate_git_directory" => crate::pr_land_tests::pr_land_primary_with_separate_git_directory(),
                "pr_land_out_of_tree_with_separate_git_directory" => crate::pr_land_tests::pr_land_out_of_tree_with_separate_git_directory(),
                "pr_open_refuses_what_the_receipts_cannot_support" => {
                    crate::pr_land_tests::pr_open_refuses_what_the_receipts_cannot_support()
                }
                "pr_land_default_batch_preserves_checkout_when_publication_is_unknown" => crate::pr_land_tests::pr_land_default_batch_preserves_checkout_when_publication_is_unknown(),
                "pr_land_changes_nothing_when_the_merge_does_not_happen" => {
                    crate::pr_land_tests::pr_land_changes_nothing_when_the_merge_does_not_happen()
                }
                "pr_land_leaves_a_reused_branch_name_alone" => {
                    crate::pr_land_tests::pr_land_leaves_a_reused_branch_name_alone()
                }
                "pr_land_deletes_an_unregistered_branch_only_at_the_merged_head" => {
                    crate::pr_land_tests::pr_land_deletes_an_unregistered_branch_only_at_the_merged_head()
                }
                "pr_land_keeps_a_branch_held_by_a_paused_rebase" => {
                    crate::pr_land_tests::pr_land_keeps_a_branch_held_by_a_paused_rebase()
                }
                "pr_land_keeps_a_branch_held_by_a_bisect" => {
                    crate::pr_land_tests::pr_land_keeps_a_branch_held_by_a_bisect()
                }
                "pr_land_keeps_a_branch_held_in_a_checkout_that_went_away" => {
                    crate::pr_land_tests::pr_land_keeps_a_branch_held_in_a_checkout_that_went_away()
                }
                "pr_land_refuses_when_a_worktree_entry_cannot_be_read" => {
                    crate::pr_land_tests::pr_land_refuses_when_a_worktree_entry_cannot_be_read()
                }
                "pr_land_refuses_when_worktree_state_is_malformed" => {
                    crate::pr_land_tests::pr_land_refuses_when_worktree_state_is_malformed()
                }
                "pr_land_leaves_a_local_namesake_of_a_fork_branch_alone" => {
                    crate::pr_land_tests::pr_land_leaves_a_local_namesake_of_a_fork_branch_alone()
                }
                "dispatch_into_git_worktree_without_prior_state" => {
                    dispatch_into_git_worktree_without_prior_state()
                }
                "dispatch_into_git_worktree_beside_managed_tree" => {
                    dispatch_into_git_worktree_beside_managed_tree()
                }
                "cross_project_writer_names_both_projects" => {
                    cross_project_writer_names_both_projects()
                }
                "git_created_and_recreated_worktrees_are_dispatchable" => {
                    git_created_and_recreated_worktrees_are_dispatchable()
                }
                _ => escaping_branch_is_invalid_and_writes_no_checkout_outside_worktree_root(),
            };
            result.map_err(|error| std::io::Error::other(error.to_string()))
        },
    )
}

pub(super) fn command(rig: &TestRig, args: &[&str]) -> Result<ExecOutcome> {
    let mut argv = vec![
        std::ffi::OsString::from("--state-dir"),
        rig.state.clone().into_os_string(),
    ];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    rig.execute_os(&std::env::current_exe()?, &argv, None)
}

pub(super) fn git(rig: &TestRig, args: &[&str]) -> Result<String> {
    let git = provenant_os::git::git_executable().ok_or("Git executable unavailable")?;
    let output = rig.execute(&git, args, None)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(String::from_utf8(output.stdout.bytes)?.trim().to_owned())
}

pub(super) fn repository() -> Result<TestRig> {
    let rig = TestRig::new()?;
    git(&rig, &["config", "user.name", "Fixture"])?;
    git(&rig, &["config", "user.email", "fixture@example.invalid"])?;
    // Detached auto-maintenance would write locks while tests snapshot the tree.
    git(&rig, &["config", "maintenance.auto", "false"])?;
    git(&rig, &["config", "gc.auto", "0"])?;
    git(&rig, &["checkout", "-q", "-b", "main"])?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\n")?;
    for name in [
        "AGENTS.md",
        "nested/AGENTS.md",
        ".claude/settings.json",
        ".github/workflows/check.yml",
    ] {
        let path = rig.project.join(name);
        fs::create_dir_all(path.parent().ok_or("fixture parent")?)?;
        fs::write(path, b"baseline instruction")?;
    }
    git(&rig, &["add", "."])?;
    git(&rig, &["commit", "-qm", "Initial"])?;
    Ok(rig)
}

pub(super) fn writer_backend_available() -> Result<bool> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let cancel = provenant_core::CancelToken::new();
    let backend = provenant_confinement::permissions::system_backend(
        &provenant_confinement::permissions::BackendContext {
            executor: &executor,
            cancel: &cancel,
            #[cfg(target_os = "macos")]
            env: &[],
        },
    );
    match backend {
        Ok(_) => Ok(true),
        Err(error) => {
            provenant_test_support::skip_sandbox_dependent_test(format!(
                "writer fixture skipped: {error}"
            ))?;
            Ok(false)
        }
    }
}

/// Run Git as the lane does: against its private view of refs and objects,
/// bound to the writer worktree.
fn lane_git(rig: &TestRig, scratch: &Path, worktree: &Path, args: &[&str]) -> Result<String> {
    let git = provenant_os::git::git_executable().ok_or("Git executable unavailable")?;
    let common = scratch.join("git-common");
    let binding = [
        format!("GIT_DIR={}", common.display()),
        format!("GIT_COMMON_DIR={}", common.display()),
        format!("GIT_WORK_TREE={}", worktree.display()),
        format!(
            "GIT_OBJECT_DIRECTORY={}",
            scratch.join("git-objects").display()
        ),
        "GIT_AUTHOR_NAME=Lane".to_owned(),
        "GIT_AUTHOR_EMAIL=lane@example.invalid".to_owned(),
        "GIT_COMMITTER_NAME=Lane".to_owned(),
        "GIT_COMMITTER_EMAIL=lane@example.invalid".to_owned(),
    ];
    let mut argv: Vec<std::ffi::OsString> = binding.iter().map(Into::into).collect();
    argv.push(git.into_os_string());
    argv.extend(["-C".into(), worktree.into()]);
    argv.extend(args.iter().map(std::ffi::OsString::from));
    let output = rig.execute_os(Path::new("/usr/bin/env"), &argv, None)?;
    assert_eq!(
        code(&output),
        0,
        "lane git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(String::from_utf8(output.stdout.bytes)?.trim().to_owned())
}

/// Crash only this fixture guardian after durable exit, before Run publication.
struct FixtureHolder<'a>(&'a dyn provenant_supervisor::Holder);
impl provenant_supervisor::Holder for FixtureHolder<'_> {
    fn admit(
        &self,
        context: &[u8],
        attempt: &provenant_os::fs::Directory,
    ) -> provenant_supervisor::custody::Result<()> {
        self.0.admit(context, attempt)
    }
    fn terminal(
        &self,
        context: &[u8],
        attempt: &provenant_os::fs::Directory,
        exit: &provenant_supervisor::custody::ExitObservation,
        recovery: &provenant_supervisor::custody::Recovery,
    ) -> provenant_supervisor::custody::Result<()> {
        let parsed: provenant_run::HolderContext = serde_json::from_slice(context)
            .map_err(|_| provenant_supervisor::custody::CustodyError::InvalidRecord)?;
        if parsed
            .roots
            .checkout
            .join(".fixture-crash-terminal")
            .exists()
        {
            std::process::exit(70);
        }
        self.0.terminal(context, attempt, exit, recovery)
    }
}

pub(super) fn writer_process_cleanup(
    context: &provenant_run::ExecutionContext,
) -> provenant_supervisor::fixture::ProcessCleanupGuard {
    // Guardian records live under runs, alongside worktree metadata and child stores.
    provenant_supervisor::fixture::ProcessCleanupGuard::new(context.state_root.as_path())
}

pub(super) fn start_writer(
    store: &provenant_run::RunStore,
    run: provenant_core::RunId,
    provider: &dyn provenant_run::Providers,
    holder: &dyn Fn(&provenant_run::AttemptRecord) -> provenant_run::Result<Vec<u8>>,
    call: &provenant_run::Request<'_>,
) -> Result<Vec<provenant_run::Started>> {
    Ok(store.start_with_writer_lease(
        run,
        provider,
        &|record, _| holder(record),
        &|directory, scope, binding| {
            provenant_supervisor::fixture::acquire_writer_with_binding(
                directory,
                scope,
                binding,
                provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                call.cancel,
                call.clock,
            )
        },
        None,
        call,
    )?)
}

pub(super) fn writer_context(
    rig: &TestRig,
) -> Result<(provenant_core::Roots, provenant_run::ExecutionContext)> {
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let location = provenant_config::ProjectLocation::new(rig.project.join(".git"))?;
    let binding = provenant_config::bind_project(&roots, location)?;
    let context = provenant_run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id: binding.project_id(),
        actor_id: provenant_core::ActorId::from_bytes([62; 16]),
        read_prompts: true,
    };
    Ok((roots, context))
}

fn writer_call<'a>(
    clock: &'a provenant_os::clock::SystemClock,
    cancel: &'a provenant_core::CancelToken,
) -> Result<provenant_run::Request<'a>> {
    Ok(provenant_run::Request { clock, cancel })
}

pub(super) fn open_writer_store(
    context: &provenant_run::ExecutionContext,
) -> Result<provenant_run::RunStore> {
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let mut store = provenant_run::RunStore::open(context, &call)?;
    store.set_admission_policy(provenant_run::AdmissionPolicy {
        startup_spacing_ms: 0,
        read_only_spacing_ms: 0,
        ..Default::default()
    });
    Ok(store)
}

pub(super) fn writer_run(
    writer: provenant_run::WriterRequest,
    operation: &str,
    seed: u8,
    prompt: &str,
) -> Result<provenant_run::PreparedRun> {
    let task = provenant_core::TaskId::from_bytes([seed; 16]);
    let attempt = provenant_core::AttemptId::from_bytes([seed.wrapping_add(1); 16]);
    let prompt_digest = provenant_records::CanonicalV1::file_digest(prompt.as_bytes());
    let clock = provenant_os::clock::SystemClock::new();
    Ok(provenant_run::PreparedRun {
        run_id: provenant_core::RunId::from_bytes([seed.wrapping_add(2); 16]),
        operation_id: provenant_core::OperationId::parse(operation)?,
        request: provenant_run::CanonicalRequest {
            extra: Default::default(),
            child_attempt_id_version: None,
            chair: None,
            fresh_continuation: None,
            schema_version: 1,
            tasks: vec![provenant_run::TaskRequest {
                cloud: None,
                context: None,
                extra: Default::default(),
                build_heavy: false,
                title: None,
                base: None,
                head: None,
                source_head: None,
                base_ref: None,
                account: None,
                profile: provenant_run::HarnessProfile::default(),
                task_id: task,
                prompt_digest,
                route: provenant_run::RequestedRoute::Model {
                    extra: Default::default(),
                    provider: "claude".into(),
                    model: "claude/test-model".into(),
                    effort: None,
                },
                authority_digest: provenant_core::Digest::from_bytes([seed; 32]),
                writer: Some(writer),
                provider_timeout_seconds: 20,
                delivery_id: None,
                deliver: None,
                access: Default::default(),
            }],
            concurrency: 1,
            parent: None,
            lane_depth: Some(1),
            parent_attempt: None,
        },
        attempts: vec![provenant_run::NewAttempt {
            attempt_id: attempt,
            task_id: task,
            prompt: prompt.into(),
        }],
        created_at: clock.wall_now()?,
    })
}

pub(super) struct WriterProvider {
    pub(super) state: PathBuf,
    pub(super) home: PathBuf,
    pub(super) project: provenant_core::ProjectId,
    pub(super) mode: &'static str,
}
impl provenant_run::Providers for WriterProvider {
    fn prepare(
        &self,
        task: &provenant_run::TaskRequest,
        _prompt: &str,
        paths: &provenant_run::LaunchPaths,
        resume: Option<&provenant_run::AttemptRecord>,
    ) -> std::result::Result<provenant_run::PreparedLaunch, provenant_run::PrepareRefusal> {
        if resume.is_some() {
            return Err("writer fixture does not resume".into());
        }
        let request = task.writer.as_ref().ok_or("writer request missing")?;
        let writer = provenant_run::prepare_writer(request, &self.state, self.project)
            .map_err(|error| error.to_string())?;
        let git = provenant_os::git::git_executable().ok_or("git")?;
        if let Some(source) = &task.source_head {
            writer
                .require_continuation_source(
                    &git,
                    source.parse().map_err(|_| "source")?,
                    task.head
                        .as_deref()
                        .ok_or("predecessor")?
                        .parse()
                        .map_err(|_| "head")?,
                )
                .map_err(|error| error.to_string())?;
        } else if let Some(head) = &task.head {
            writer
                .require_source(head.parse().map_err(|_| "head")?)
                .map_err(|error| error.to_string())?;
        }
        let executable =
            fs::canonicalize(std::env::current_exe().map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let argv = vec![
            executable.clone().into_os_string(),
            OsString::from("--writer-provider"),
            OsString::from(self.mode),
            writer.worktree.path().as_os_str().to_owned(),
        ];
        // Production lists every other checkout of the repository; the primary
        // subtree covers each sibling here.
        let primary = writer
            .worktree
            .path()
            .parent()
            .and_then(Path::parent)
            .ok_or("fixture primary")?;
        let protected = [provenant_confinement::permissions::PathGrant {
            path: primary.to_owned(),
            scope: provenant_confinement::permissions::PathScope::Subtree,
        }];
        let plan = provenant_cli::run::writer_fixture_plan(
            self.project,
            writer.worktree.path(),
            &self.home,
            &self.state,
            &protected,
            paths,
            &writer,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        )?;
        let login = self.home.join(".fixture-login");
        fs::write(&login, b"").map_err(|error| error.to_string())?;
        let profile = provenant_confinement::permissions::ProviderProfile {
            spec: provenant_confinement::permissions::ProviderProfileSpec {
                authentication: vec![provenant_confinement::permissions::AuthenticationProfile {
                    keychain: None,
                    mechanism: "login".into(),
                    // A placeholder stands in for a login file; no live
                    // credential is read and this adds no write permission.
                    roots: vec![provenant_confinement::permissions::PathGrant {
                        path: login,
                        scope: provenant_confinement::permissions::PathScope::Exact,
                    }],
                    in_place_writes: Vec::new(),
                }],
                process_executables: vec![provenant_confinement::permissions::PathGrant {
                    path: executable,
                    scope: provenant_confinement::permissions::PathScope::Exact,
                }],
                ..Default::default()
            },
            observations: writer_observations(),
        };
        let route = fixture_route();
        Ok(provenant_run::PreparedLaunch {
            skipped_routes: Vec::new(),
            argv,
            environment: Vec::new(),
            stdin: Vec::new(),
            secret_input: None,
            cwd: writer.worktree.path().to_owned(),
            route,
            confinement: Some(provenant_run::LaunchConfinement {
                plan,
                profile,
                writer: Some(writer),
                home: self.home.clone(),
                cargo_registry_seed: false,
                state_root: self.state.clone(),
            }),
            observed_head: None,
            repository: None,
        })
    }

    fn decode(
        &self,
        route: &provenant_run::RouteRecord,
        stdout: &[u8],
        exit: &provenant_supervisor::custody::ExitObservation,
        provider_state: &std::path::Path,
    ) -> std::result::Result<provenant_run::Decoded, String> {
        let registry = provenant_cli::run::Registry::default();
        provenant_run::Providers::decode(&registry, route, stdout, exit, provider_state)
    }
}

fn fixture_route() -> provenant_run::RouteRecord {
    provenant_run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/test-model".into()),
        effort: None,
        account_label: None,
        ..Default::default()
    }
}

fn writer_observations() -> BTreeMap<
    provenant_confinement::permissions::FixtureOperation,
    provenant_confinement::permissions::FixtureOutcome,
> {
    use provenant_confinement::permissions::{
        FixtureOperation as Operation, FixtureOutcome as Outcome,
    };
    [
        (Operation::DeclaredRead, Outcome::Allowed),
        (Operation::UndeclaredRead, Outcome::Denied),
        (Operation::UndeclaredWrite, Outcome::Denied),
        (Operation::SecretRead, Outcome::Denied),
        (Operation::ProtectedWrite, Outcome::Denied),
        (Operation::OtherNetwork, Outcome::Denied),
        (Operation::RuntimeRead, Outcome::Allowed),
        (Operation::AuthenticationRead, Outcome::Allowed),
        (Operation::ProtectedInstructionRead, Outcome::Allowed),
        (Operation::DeclaredProcessExec, Outcome::Allowed),
        (Operation::UndeclaredProcessExec, Outcome::Denied),
        (Operation::ProcessFork, Outcome::Denied),
        (Operation::WorktreeWrite, Outcome::Allowed),
        (Operation::GitIndexWrite, Outcome::Allowed),
        (Operation::GitHeadWrite, Outcome::Denied),
        (Operation::GitObjectsWrite, Outcome::Denied),
        (Operation::SelectedRefWrite, Outcome::Denied),
        (Operation::GitConfigWrite, Outcome::Denied),
        (Operation::OtherRefWrite, Outcome::Denied),
    ]
    .into()
}

pub(super) fn holder_context<'a>(
    rig: &'a TestRig,
    roots: &'a provenant_core::Roots,
    context: &'a provenant_run::ExecutionContext,
) -> impl Fn(&provenant_run::AttemptRecord) -> provenant_run::Result<Vec<u8>> + 'a {
    let seed = provenant_run::RootSeed {
        home: rig.home.clone(),
        checkout: rig.project.clone(),
        state_root: roots.state.as_path().to_owned(),
    };
    move |attempt| {
        provenant_run::HolderContext {
            schema_version: 1,
            roots: seed.clone(),
            project_id: attempt.project_id,
            actor_id: context.actor_id,
            run_id: attempt.run_id,
            attempt_id: attempt.attempt_id,
            host_delivery: None,
            launch: None,
            sealed: None,
        }
        .encode()
    }
}

pub(super) fn await_terminal(
    store: &provenant_run::RunStore,
    attempt: provenant_core::AttemptId,
    call: &provenant_run::Request<'_>,
) -> Result<provenant_run::AttemptRecord> {
    let mut result = None;
    // A third import is eligible after the five- and thirty-second backoffs.
    if provenant_test_support::wait_for(
        "writer terminal state",
        Duration::from_secs(60),
        || {
            let poll = provenant_run::Request {
                clock: call.clock,
                cancel: call.cancel,
            };
            match store.record(attempt, &poll) {
                Ok(record)
                    if matches!(record.state, provenant_run::AttemptState::Terminal { .. }) =>
                {
                    result = Some(Ok(record));
                    true
                }
                Ok(_) => false,
                Err(error) => {
                    result = Some(Err(error));
                    true
                }
            }
        },
        |done| *done,
    )
    .is_err()
    {
        let poll = provenant_run::Request {
            clock: call.clock,
            cancel: call.cancel,
        };
        let temporary = store.temporary_path(attempt, &poll)?;
        let directory = temporary.parent().ok_or("attempt directory")?;
        let progress = fs::read_to_string(directory.join("settlement-progress.json"))
            .unwrap_or_else(|error| error.to_string());
        let record = store.record(attempt, &poll)?;
        let waits = store.lane_waits_with_resources(
            std::slice::from_ref(&record),
            &poll,
            &FixtureResources,
        );
        let observations = store.admission_observations(&record, &poll, &FixtureResources);
        return Err(format!(
            "writer attempt did not reach terminal state: {:?}; admission: {waits:?}; current admission observations: {observations:?}; settlement: {progress}",
            record.state
        )
        .into());
    }
    Ok(result.expect("wait returned on terminal state or error")?)
}

fn assert_terminal_outcome(
    record: &provenant_run::AttemptRecord,
    expected: provenant_run::Outcome,
) -> Result {
    let provenant_run::AttemptState::Terminal { outcome, .. } = &record.state else {
        return Err("writer attempt was not terminal".into());
    };
    assert_eq!(*outcome, expected, "{:?}", record.state);
    Ok(())
}

pub(super) fn wait_for_writer_release(store: &provenant_run::RunStore) -> Result {
    let mut failure = None;
    let released = provenant_test_support::wait_for(
        "writer reservation release",
        Duration::from_secs(5),
        || match store.writer_fences() {
            Ok(fences) => fences.is_empty(),
            Err(error) => {
                failure = Some(error);
                true
            }
        },
        |done| *done,
    )
    .is_ok();
    if let Some(error) = failure {
        return Err(error.into());
    }
    released
        .then_some(())
        .ok_or_else(|| "writer reservation was not released after terminal publication".into())
}

struct ReleaseOnDrop(PathBuf);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, b"release");
    }
}

/// Test-only child entry point. The parent launch test invokes the same binary
/// under the real supervisor and confinement plan; production builds have no
/// route to this fixture.
pub(crate) fn provider(args: &[OsString]) -> Result {
    if args.first().map(OsString::as_os_str) != Some(std::ffi::OsStr::new("--writer-provider")) {
        return Err("writer provider fixture switch missing".into());
    }
    let mode = args
        .get(1)
        .and_then(|value| value.to_str())
        .ok_or("writer provider fixture mode missing")?;
    let worktree = args
        .get(2)
        .map(PathBuf::from)
        .ok_or("writer provider worktree missing")?;
    match mode {
        "edit" => {}
        "instruction-edit" => {
            // A different size from the baseline: Git can miss a same-size
            // edit made within the checkout's timestamp granularity.
            fs::write(worktree.join("AGENTS.md"), b"writer instructions, edited\n")?;
        }
        "control" => {
            for name in [
                "nested/AGENTS.md",
                ".claude/settings.json",
                ".github/workflows/check.yml",
            ] {
                fs::write(worktree.join(name), b"changed control")?;
            }
        }
        "negative" => {
            let primary = worktree
                .parent()
                .and_then(Path::parent)
                .ok_or("fixture primary")?;
            for target in [
                primary.join("negative-write"),
                primary.join(".worktrees/sibling/negative-write"),
                worktree.join("escape/negative-write"),
                primary.join(".git/config"),
                primary.join(".git/hooks/negative-hook"),
            ] {
                let error = fs::write(&target, b"forbidden").err().ok_or_else(|| {
                    format!("write escaped production plan: {}", target.display())
                })?;
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied,
                    "{}: {error}",
                    target.display()
                );
            }
        }
        "hold" => {
            let ready = worktree.join(".writer-fixture-ready");
            let release = worktree.join(".writer-fixture-release");
            fs::write(&ready, b"ready")?;
            provenant_test_support::wait_for(
                "writer fixture release",
                Duration::from_secs(10),
                || release.exists(),
                |released| *released,
            )?;
        }
        _ => return Err("unknown writer provider fixture mode".into()),
    }
    fs::write(worktree.join("writer-edited"), "writer fixture completed")?;
    // The answer reports a handoff; the assistant message repeats it so the
    // decoder ties the answering model to the final result.
    let answer = "writer fixture completed\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[],\"note\":\"writer fixture\"}\n```";
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    for event in [
        serde_json::json!({"type":"system","subtype":"init","session_id":"writer-fixture-session"}),
        serde_json::json!({"type":"assistant","message":{"model":"test-model","role":"assistant","content":[{"type":"text","text":answer}]},"session_id":"writer-fixture-session"}),
        serde_json::json!({"type":"result","session_id":"writer-fixture-session","subtype":"success","is_error":false,"result":answer}),
    ] {
        serde_json::to_writer(&mut output, &event)?;
        output.write_all(b"\n")?;
    }
    Ok(())
}

pub(super) fn json(output: &ExecOutcome) -> Result<Value> {
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}

pub(super) fn ok(rig: &TestRig, args: &[&str]) -> Result<Value> {
    let output = command(rig, args)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let value = json(&output)?;
    assert_eq!(value["status"], "ok");
    Ok(value)
}

fn prepare_lane_cache(rig: &TestRig, path: &Path, branch: &str) -> Result<PathBuf> {
    use provenant_confinement::{
        permissions::{WorktreeWriteAuthority, prepare_writer_directories},
        repo::{GitRefName, verify_worktree},
        utility::{UtilityBudget, UtilityRuntime},
    };
    use provenant_os::exec::SystemExecutor;

    let runtime = UtilityRuntime::new(
        fs::canonicalize(provenant_os::git::git_executable().ok_or("Git executable unavailable")?)?,
        rig.state.join("utility-home"),
    )?;
    let executor = SystemExecutor::new(1)?;
    let repo = runtime.open_repo(
        &executor,
        &rig.project,
        UtilityBudget::bounded(Duration::from_secs(5))?,
        &provenant_core::CancelToken::new(),
    )?;
    let worktree = verify_worktree(&repo, path)?;
    let branch = GitRefName::new(format!("refs/heads/{branch}"))?;
    let authority = WorktreeWriteAuthority::for_worktree(&worktree, &branch, &rig.home)?;
    prepare_writer_directories(&worktree, &branch, &authority, &rig.home)?;
    let cache = authority.cargo_cache().path.clone();
    fs::write(cache.join("artifact"), b"lane build output")?;
    Ok(cache)
}

fn is_fenced(error: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        error.downcast_ref::<provenant_supervisor::custody::CustodyError>(),
        Some(provenant_supervisor::custody::CustodyError::Fenced)
    )
}

fn branch_commit(rig: &TestRig, branch: &str) -> Result<std::path::PathBuf> {
    ok(rig, &["worktree", "create", branch, "--json"])?;
    let path = rig.project.join(".worktrees").join(branch);
    fs::write(path.join(branch), branch)?;
    let text = path.to_str().ok_or("non-text fixture path")?;
    git(rig, &["-C", text, "add", branch])?;
    git(rig, &["-C", text, "commit", "-qm", branch])?;
    Ok(path)
}

fn has_branch(rig: &TestRig, name: &str) -> Result<bool> {
    let reference = format!("refs/heads/{name}");
    Ok(!git(rig, &["for-each-ref", reference.as_str()])?.is_empty())
}

// Existing state for a sibling does not change Git membership.

fn git_worktree_dispatch_is_accepted(register_sibling: bool) -> Result {
    let rig = repository()?;
    if register_sibling {
        ok(
            &rig,
            &["worktree", "create", "managed", "--base", "HEAD", "--json"],
        )?;
    }
    git(
        &rig,
        &["worktree", "add", "-qb", "foreign", ".worktrees/foreign"],
    )?;
    let (_, context) = writer_context(&rig)?;
    let request = provenant_run::resolve_writer(
        context.state_root.as_path(),
        &rig.project,
        Path::new(".worktrees/foreign"),
    )?;
    provenant_run::prepare_writer(&request, context.state_root.as_path(), context.project_id)?;

    Ok(())
}

// Git membership makes a recreated tree dispatchable without adoption.

fn escaping_branch_is_invalid_and_writes_no_checkout_outside_worktree_root() -> Result {
    let rig = repository()?;
    let output = rig.command(&["worktree", "create", "../escape", "--json"])?;
    assert_eq!(code(&output), 2);
    assert_eq!(json(&output)?["status"], "invalid");
    assert!(!rig.project.join("escape").exists());
    assert!(!rig.root.join("escape").exists());
    assert!(!rig.project.join(".worktrees").exists());
    assert_eq!(git(&rig, &["branch", "--format=%(refname:short)"])?, "main");
    rig.assert_legacy_unchanged()
}

#[cfg(target_os = "macos")]
fn clone_rust_toolchain(rig: &TestRig, home: &Path) -> Result {
    // Build-time rustup identity locates tools only. The child HOME and all
    // mutable state stay in the rig. APFS clones avoid a toolchain-size copy.
    let rustup = option_env!("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| option_env!("HOME").map(|home| PathBuf::from(home).join(".rustup")))
        .ok_or("build-time Rust home")?;
    let toolchain = option_env!("RUSTUP_TOOLCHAIN").ok_or("build-time Rust toolchain")?;
    let source = rustup.join("toolchains").join(toolchain);
    let target = home.join(".rustup/toolchains/fixture");
    let output = rig.execute_os(
        Path::new("/bin/cp"),
        &[
            "-cR".into(),
            source.into_os_string(),
            target.clone().into_os_string(),
        ],
        None,
    )?;
    assert_eq!(
        code(&output),
        0,
        "toolchain clone: {}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    for tool in ["cargo", "rustc", "rustdoc"] {
        std::os::unix::fs::symlink(
            target.join("bin").join(tool),
            home.join(".cargo/bin").join(tool),
        )?;
    }
    // Cargo finds subcommands on PATH. Install only the selected executable
    // into the isolated fixture home, leaving host Cargo state untouched.
    if let Some(nextest) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("cargo-nextest"))
            .find(|path| path.is_file())
    }) {
        rig.copy_executable(&nextest, &home.join(".cargo/bin/cargo-nextest"))?;
    }
    Ok(())
}

/// The credential process boundary returns synthetic bytes and runs nothing.
struct FixtureCredential;
impl Executor for FixtureCredential {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> std::result::Result<ExecOutcome, provenant_os::exec::ExecError> {
        // `auth status` answers the identity check; anything else reads the credential.
        let bytes: &[u8] = if options.argv.iter().any(|arg| arg == "status") {
            br#"{"loggedIn":true,"email":"fixture@example.test"}"#
        } else {
            br#"{"claudeAiOauth":{"accessToken":"fixture-token","expiresAt":4000000000000}}"#
        };
        Ok(ExecOutcome {
            termination: provenant_os::exec::Termination::Exit(0),
            stdout: provenant_os::exec::CapturedStream {
                bytes: bytes.to_vec(),
                ..Default::default()
            },
            stderr: Default::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        })
    }
}

// Confinement startup exercises the shared graph child request without a
// launch. The host relay tests own admission, depth limits and cancellation.
fn assert_lane_child_preview(result: &Value) -> Result {
    assert!(
        result.get("isError").is_none_or(|value| value == false),
        "{result}"
    );
    let operation: provenant_fabric_runtime::graph::relay::RelayOp =
        serde_json::from_value(result["structuredContent"].clone())?;
    let provenant_fabric_runtime::graph::relay::RelayOp::Add(request) = operation else {
        return Err(format!("expected a graph child request: {result}").into());
    };
    assert_eq!(request.template, "task", "{result}");
    assert_eq!(request.brief, "read only", "{result}");
    assert!(!request.writer, "{result}");
    Ok(())
}

/// Identity reports errors in text; other tools keep structured codes.
fn mcp_error_code(result: &Value) -> &str {
    result["structuredContent"]["code"]
        .as_str()
        .unwrap_or_else(|| {
            result["content"][0]["text"]
                .as_str()
                .and_then(|text| text.split_once(':'))
                .map_or("", |(code, _)| code)
        })
}

fn age_gc_output(path: &Path) -> Result {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            age_gc_output(&entry?.path())?;
        }
    }
    fs::File::open(path)?.set_modified(std::time::UNIX_EPOCH)?;
    Ok(())
}

// Port the former dispatch preparation cancellation regression through the
// graph front door. Entering seeding a second time proves the first launcher
// released the same worktree's custody before stop returned.

#[cfg(test)]
mod predicate_tests {
    #[test]
    fn nextest_missing_tool_is_reported_when_required() -> super::Result {
        use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor};
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let cargo = root.join("cargo");
        std::fs::write(&cargo, "#!/bin/sh\n[ \"$FIXTURE_NEXTEST\" = 1 ]\n")?;
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o700))?;
        let executor = SystemExecutor::new(1)?;
        for (available, required, expected) in
            [(false, false, 0), (false, true, 15), (true, true, 0)]
        {
            let env = [
                ("PATH".into(), root.as_os_str().to_owned()),
                ("TMPDIR".into(), root.as_os_str().to_owned()),
                (
                    "FIXTURE_NEXTEST".into(),
                    if available { "1" } else { "0" }.into(),
                ),
                (
                    "PROVENANT_LANE_TOOLS_REQUIRED".into(),
                    if required { "1" } else { "0" }.into(),
                ),
            ];
            let argv = [
                "/bin/sh".into(),
                "-c".into(),
                super::LANE_NEXTEST_PROBE.into(),
            ];
            let mut options = ExecOptions::new(&argv, &root, super::Duration::from_secs(3));
            options.env = Some(&env);
            options.stdout = StreamPolicy::Capture { limit: 4096 };
            options.stderr = StreamPolicy::Capture { limit: 4096 };
            let output = executor.execute(options)?;
            assert_eq!(super::code(&output), expected);
            let stdout = String::from_utf8_lossy(&output.stdout.bytes);
            let stderr = String::from_utf8_lossy(&output.stderr.bytes);
            assert_eq!(stdout.contains("lane-nextest: PASS"), available);
            if !available {
                assert!(stderr.contains("cargo-nextest unavailable"), "{stderr}");
            }
        }
        Ok(())
    }

    #[test]
    fn lane_tool_prerequisite_uses_injected_path_and_required_control() -> super::Result {
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor};
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let executor = SystemExecutor::new(1)?;
        for tools in [&[][..], &["node"][..], &["pnpm"][..], &["node", "pnpm"][..]] {
            let bin = root.join(format!("tools-{}", tools.join("-")));
            std::fs::create_dir(&bin)?;
            for name in tools {
                let path = bin.join(name);
                std::fs::write(&path, "#!/bin/sh\nexit 0\n")?;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            }
            for required in [None, Some("0"), Some("1")] {
                let mut env = vec![
                    ("PATH".into(), bin.as_os_str().to_owned()),
                    ("TMPDIR".into(), root.as_os_str().to_owned()),
                ];
                if let Some(value) = required {
                    env.push(("PROVENANT_LANE_TOOLS_REQUIRED".into(), value.into()));
                }
                let argv = [
                    "/bin/sh".into(),
                    "-ec".into(),
                    super::LANE_NODE_PNPM_PROBE.into(),
                ];
                let mut options = ExecOptions::new(&argv, &root, super::Duration::from_secs(3));
                options.env = Some(&env);
                options.stdout = provenant_os::exec::StreamPolicy::Capture { limit: 4096 };
                options.stderr = provenant_os::exec::StreamPolicy::Capture { limit: 4096 };
                let output = executor.execute(options)?;
                let available = tools.len() == 2;
                assert_eq!(
                    super::code(&output),
                    if !available && required == Some("1") {
                        15
                    } else {
                        0
                    }
                );
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout.bytes)
                        .contains("lane-node-pnpm-compat: PASS"),
                    available
                );
                if !available && required != Some("1") {
                    assert!(String::from_utf8_lossy(&output.stderr.bytes).contains("SKIP #1:"));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn only_the_narrowest_probe_capable_claude_case_runs_the_native_shell_probe() {
        assert!(super::native_shell_probe_case("claude", true, false));
        assert!(!super::native_shell_probe_case("claude", false, false));
        assert!(!super::native_shell_probe_case("claude", true, true));
        assert!(!super::native_shell_probe_case("codex", true, false));
        assert!(!super::native_shell_probe_case("codex", true, true));
    }
}
