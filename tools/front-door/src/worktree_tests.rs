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
    time::{Duration, Instant},
};
use support::{Result, TestRig, code, eventually};

// Only the harness=false target dispatches with these seams. The installed
// binary has no fixture flag, feature or environment override.
struct FreeWorkers;
impl provenant_confinement::worktrees::WorkerObserver for FreeWorkers {
    fn observe_roots(
        &self,
        roots: &[std::path::PathBuf],
    ) -> std::collections::BTreeMap<std::path::PathBuf, provenant_confinement::worktrees::WorkerState>
    {
        roots
            .iter()
            .cloned()
            .map(|root| (root, provenant_confinement::worktrees::WorkerState::Free))
            .collect()
    }
}

struct UnknownWorkers;
impl provenant_confinement::worktrees::WorkerObserver for UnknownWorkers {
    fn observe_roots(
        &self,
        roots: &[PathBuf],
    ) -> BTreeMap<PathBuf, provenant_confinement::worktrees::WorkerState> {
        static OBSERVATIONS: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let late = std::env::var("FIXTURE_UNKNOWN_WORKERS").is_ok_and(|mode| mode == "late");
        let observed = if late && OBSERVATIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2
        {
            provenant_confinement::worktrees::WorkerState::Free
        } else {
            provenant_confinement::worktrees::WorkerState::Unknown
        };
        roots.iter().cloned().map(|root| (root, observed)).collect()
    }
}

/// The driver's seams: free workers, the recorded boot and a holder that can
/// crash after durable exit.
struct Fixture;
impl crate::Process for Fixture {
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
        // The fake `gh` of the `land` fixtures.
        Some("pr" | "repo") => {
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

fn suite(args: &[std::ffi::OsString]) -> std::io::Result<()> {
    provenant_supervisor::fixture::run_libtest(
        args,
        &[
            "gc_fetches_default_and_removes_only_clean_merged_worktrees",
            "gc_output_requires_terminal_lanes_and_respects_keep_recent_dirty_and_links",
            "gc_disk_alert_is_once_per_dip_and_manual_preview_does_not_latch",
            "dispatch_refuses_low_disk_writers",
            "resume_refuses_low_disk_writer",
            "gc_reports_terminal_tmp_paths_and_bytes_in_json_and_text",
            "lane_prepare_isolates_nested_state",
            "read_only_dispatch_applies_bound_git_and_project_writes",
            "writer_dispatch_uses_lane_cache_and_narrow_provider_state",
            "writer_dispatch_edits_a_linked_worktree",
            "second_writer_is_refused_while_first_holds_the_worktree",
            "writer_terminal_refuses_sequencer_state",
            "writer_protected_changes_wait_for_review_and_ignore_upstream",
            "writer_production_plan_denies_outside_writes",
            "writer_reconcile_refuses_unfinished_cherry_pick_and_releases",
            "recreated_worktree_accepts_writer_after_settlement",
            "create_list_and_remove_a_managed_worktree",
            "gc_idle_keeps_live_and_unobservable_writers_and_removes_free_cache",
            "scheduled_gc_runs_outside_a_checkout_and_obeys_disable_and_reservations",
            "gc_reports_transient_and_preserves_unobserved_project_array",
            "gc_pressure_collects_only_the_selected_primary_project",
            "gc_refuses_bare_projects_and_their_linked_checkout_fallback",
            "provider_state_gc_sweeps_children_and_refuses_replaced_roots",
            "remove_a_managed_worktree_deletes_its_legacy_lane_cache",
            "merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees",
            "merged_pruning_deletes_a_merged_branch_without_a_worktree",
            "remove_by_name_follows_the_branch_merge_state",
            "remove_by_name_deletes_a_branch_fully_contained_in_integration",
            "remove_resolves_directory_names_and_remote_integration_refs",
            "escaping_branch_is_invalid_and_writes_no_checkout_outside_worktree_root",
            "dispatch_into_an_unregistered_worktree_names_the_create_command",
            "dispatch_into_an_unregistered_worktree_beside_registered_ones_names_the_create_command",
            "re_added_worktree_is_adopted_then_dispatchable",
            "pr_land_merges_then_fetches_fast_forwards_and_prunes",
            "pr_land_prunes_the_selected_merged_worktree",
            "pr_land_from_lane_ignores_only_its_callers",
            "pr_land_refuses_incomplete_worker_census",
            "pr_land_canonicalises_quoted_worktree_paths",
            "pr_land_recovers_only_abandoned_prepared_writer_leases",
            "pr_land_recovers_terminal_writer_leases_in_parent_and_child_stores",
            "pr_land_primary_with_separate_git_directory",
            "pr_land_out_of_tree_with_separate_git_directory",
            "pr_land_refuses_each_unsafe_step_and_reports_the_rest",
            "pr_land_changes_nothing_when_the_merge_does_not_happen",
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
        ],
        |name| {
            let result = match name {
                "pr_contribution_roundtrip" => crate::pr_land_tests::pr_contribution_roundtrip(),
                "gc_fetches_default_and_removes_only_clean_merged_worktrees" => gc_fetches_default_and_removes_only_clean_merged_worktrees(),
                "gc_output_requires_terminal_lanes_and_respects_keep_recent_dirty_and_links" => gc_output_requires_terminal_lanes_and_respects_keep_recent_dirty_and_links(),
                "gc_disk_alert_is_once_per_dip_and_manual_preview_does_not_latch" => gc_disk_alert_is_once_per_dip_and_manual_preview_does_not_latch(),
                "dispatch_refuses_low_disk_writers" => dispatch_refuses_low_disk_writers(),
                "resume_refuses_low_disk_writer" => resume_refuses_low_disk_writer(),
                "gc_reports_terminal_tmp_paths_and_bytes_in_json_and_text" => gc_reports_terminal_tmp_paths_and_bytes_in_json_and_text(),
                "lane_prepare_isolates_nested_state" => lane_prepare_isolates_nested_state(),
                "read_only_dispatch_applies_bound_git_and_project_writes" => {
                    crate::compose_tests::read_only_dispatch_applies_bound_git_and_project_writes()
                }
                "writer_dispatch_uses_lane_cache_and_narrow_provider_state" => {
                    crate::compose_tests::writer_dispatch_uses_lane_cache_and_narrow_provider_state(
                    )
                }
                "writer_dispatch_edits_a_linked_worktree" => {
                    writer_dispatch_edits_a_linked_worktree()
                }
                "second_writer_is_refused_while_first_holds_the_worktree" => {
                    second_writer_is_refused_while_first_holds_the_worktree()
                }
                "writer_protected_changes_wait_for_review_and_ignore_upstream" => {
                    writer_protected_changes_wait_for_review_and_ignore_upstream()
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
                "create_list_and_remove_a_managed_worktree" => {
                    create_list_and_remove_a_managed_worktree()
                }
                "gc_idle_keeps_live_and_unobservable_writers_and_removes_free_cache" => {
                    gc_idle_keeps_live_and_unobservable_writers_and_removes_free_cache()
                }
                "provider_state_gc_sweeps_children_and_refuses_replaced_roots" => {
                    crate::provider_retention::provider_state_gc_sweeps_children_and_refuses_replaced_roots()
                }
                "gc_pressure_collects_only_the_selected_primary_project" => gc_pressure_collects_only_the_selected_primary_project(),
                "gc_refuses_bare_projects_and_their_linked_checkout_fallback" => gc_refuses_bare_projects_and_their_linked_checkout_fallback(),
                "gc_reports_transient_and_preserves_unobserved_project_array" => gc_reports_transient_and_preserves_unobserved_project_array(),
                "scheduled_gc_runs_outside_a_checkout_and_obeys_disable_and_reservations" => {
                    scheduled_gc_runs_outside_a_checkout_and_obeys_disable_and_reservations()
                }
                "remove_a_managed_worktree_deletes_its_legacy_lane_cache" => {
                    remove_a_managed_worktree_deletes_its_legacy_lane_cache()
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
                "pr_land_merges_then_fetches_fast_forwards_and_prunes" => {
                    crate::pr_land_tests::pr_land_merges_then_fetches_fast_forwards_and_prunes()
                }
                "pr_land_prunes_the_selected_merged_worktree" => {
                    crate::pr_land_tests::pr_land_prunes_the_selected_merged_worktree()
                }
                "pr_land_from_lane_ignores_only_its_callers" => crate::pr_land_tests::pr_land_from_lane_ignores_only_its_callers(),
                "pr_land_refuses_incomplete_worker_census" => crate::pr_land_tests::pr_land_refuses_incomplete_worker_census(),
                "pr_land_canonicalises_quoted_worktree_paths" => crate::pr_land_tests::pr_land_canonicalises_quoted_worktree_paths(),
                "pr_land_recovers_only_abandoned_prepared_writer_leases" => crate::pr_land_tests::pr_land_recovers_only_abandoned_prepared_writer_leases(),
                "pr_land_recovers_terminal_writer_leases_in_parent_and_child_stores" => crate::pr_land_tests::pr_land_recovers_terminal_writer_leases_in_parent_and_child_stores(),
                "pr_land_refuses_each_unsafe_step_and_reports_the_rest" => {
                    crate::pr_land_tests::pr_land_refuses_each_unsafe_step_and_reports_the_rest()
                }
                "pr_land_primary_with_separate_git_directory" => crate::pr_land_tests::pr_land_primary_with_separate_git_directory(),
                "pr_land_out_of_tree_with_separate_git_directory" => crate::pr_land_tests::pr_land_out_of_tree_with_separate_git_directory(),
                "pr_open_refuses_what_the_receipts_cannot_support" => {
                    crate::pr_land_tests::pr_open_refuses_what_the_receipts_cannot_support()
                }
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
                "dispatch_into_an_unregistered_worktree_names_the_create_command" => {
                    dispatch_into_an_unregistered_worktree_names_the_create_command()
                }
                "dispatch_into_an_unregistered_worktree_beside_registered_ones_names_the_create_command" => {
                    dispatch_into_an_unregistered_worktree_beside_registered_ones_names_the_create_command()
                }
                "re_added_worktree_is_adopted_then_dispatchable" => {
                    re_added_worktree_is_adopted_then_dispatchable()
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
        Err(error) if std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_none() => {
            writeln!(std::io::stderr(), "writer fixture skipped: {error}")?;
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

fn writer_dispatch_edits_a_linked_worktree() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    let path = rig.project.join(".worktrees/writer");
    let (roots, context) = writer_context(&rig)?;
    let cleanup = writer_process_cleanup(&context);
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let absolute =
        provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let first = writer_run(absolute, "writer-first", 41, "write the fixture file")?;
    let first_attempt = first.attempts[0].attempt_id;
    let first_record = store.create(&first, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "edit",
    };
    let holder = holder_context(&rig, &roots, &context);
    let started = start_writer(&store, first_record.run_id, &provider, &holder, &call)?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == first_attempt),
        "{started:?}"
    );
    let first_terminal = await_terminal(&store, first_attempt, &call)?;
    // The fixture edits without committing: no plain success, and the dirty
    // path is named for the chair.
    assert_terminal_outcome(&first_terminal, provenant_run::Outcome::Uncommitted)?;
    assert!(matches!(
        &first_terminal.state,
        provenant_run::AttemptState::Terminal { reason: Some(reason), .. }
            if reason.contains("uncommitted edits") && reason.contains("writer-edited")
    ));
    assert_eq!(
        fs::read_to_string(path.join("writer-edited"))?,
        "writer fixture completed"
    );
    wait_for_writer_release(&store)?;
    let path_text = path.to_str().ok_or("fixture worktree encoding")?;
    git(&rig, &["-C", path_text, "add", "writer-edited"])?;
    git(&rig, &["-C", path_text, "commit", "-qm", "Writer edit"])?;

    // Fabric supplies absolute paths; the CLI also accepts repo-relative ones.
    let relative = provenant_run::resolve_writer(
        context.state_root.as_path(),
        &rig.project,
        Path::new(".worktrees/writer"),
    )?;
    let second = writer_run(relative, "writer-released", 42, "write after release")?;
    let second_attempt = second.attempts[0].attempt_id;
    let second_record = store.create(&second, &call)?;
    let started = start_writer(&store, second_record.run_id, &provider, &holder, &call)?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == second_attempt),
        "{started:?}"
    );
    let second_terminal = await_terminal(&store, second_attempt, &call)?;
    assert_terminal_outcome(&second_terminal, provenant_run::Outcome::Succeeded)?;
    wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

fn second_writer_is_refused_while_first_holds_the_worktree() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    let path = rig.project.join(".worktrees/writer");
    let (roots, context) = writer_context(&rig)?;
    let cleanup = writer_process_cleanup(&context);
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let relative = provenant_run::resolve_writer(
        context.state_root.as_path(),
        &rig.project,
        Path::new(".worktrees/writer"),
    )?;
    let first = writer_run(relative.clone(), "writer-held", 51, "hold the writer lease")?;
    let first_attempt = first.attempts[0].attempt_id;
    let first_record = store.create(&first, &call)?;
    let release = path.join(".writer-fixture-release");
    let ready = path.join(".writer-fixture-ready");
    let _release_on_drop = ReleaseOnDrop(release.clone());
    let _ = fs::remove_file(&release);
    let _ = fs::remove_file(&ready);
    let first_provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "hold",
    };
    let holder = holder_context(&rig, &roots, &context);
    let started = start_writer(&store, first_record.run_id, &first_provider, &holder, &call)?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == first_attempt),
        "{started:?}"
    );
    wait_for_path(&ready, Duration::from_secs(8), "first writer hold point")?;

    let second = writer_run(relative, "writer-contender", 52, "contend for writer lease")?;
    let second_attempt = second.attempts[0].attempt_id;
    let second_record = store.create(&second, &call)?;
    let second_provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "edit",
    };
    let started = start_writer(
        &store,
        second_record.run_id,
        &second_provider,
        &holder,
        &call,
    )?;
    assert!(matches!(
        started.as_slice(),
        [provenant_run::Started::Refused { attempt, kind: provenant_run::RefusalKind::WorktreeBusy, .. }]
            if *attempt == second_attempt
    ));

    fs::write(&release, b"release")?;
    let first_terminal = await_terminal(&store, first_attempt, &call)?;
    assert_terminal_outcome(&first_terminal, provenant_run::Outcome::Uncommitted)?;
    assert!(matches!(
        store.record(second_attempt, &call)?.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Rejected,
            ..
        }
    ));
    wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

fn writer_protected_changes_wait_for_review_and_ignore_upstream() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    for own_change in [true, false] {
        let rig = repository()?;
        ok(
            &rig,
            &["worktree", "create", "writer", "--base", "HEAD", "--json"],
        )?;
        let path = rig.project.join(".worktrees/writer");
        let path_text = path.to_str().ok_or("fixture worktree encoding")?;
        let (roots, context) = writer_context(&rig)?;
        let cleanup = writer_process_cleanup(&context);
        let store = open_writer_store(&context)?;
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let call = writer_call(&clock, &cancel)?;
        let writer =
            provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
        let prepared = writer_run(writer, "protected-review", 101, "hold for protected commit")?;
        let attempt = prepared.attempts[0].attempt_id;
        let record = store.create(&prepared, &call)?;
        let release = path.join(".writer-fixture-release");
        let _release_on_drop = ReleaseOnDrop(release.clone());
        let provider = WriterProvider {
            state: context.state_root.as_path().to_owned(),
            home: fs::canonicalize(&rig.home)?,
            project: context.project_id,
            mode: "hold",
        };
        let holder = holder_context(&rig, &roots, &context);
        let started = start_writer(&store, record.run_id, &provider, &holder, &call)?;
        assert!(
            matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == attempt)
        );
        wait_for_path(
            &path.join(".writer-fixture-ready"),
            Duration::from_secs(8),
            "writer hold",
        )?;
        // Upstream advances after launch while local main stays at the base.
        git(&rig, &["checkout", "-qb", "upstream"])?;
        fs::write(
            rig.project.join(".github/workflows/check.yml"),
            b"upstream workflow",
        )?;
        fs::write(rig.project.join(".github/new.yml"), b"upstream addition")?;
        fs::remove_file(rig.project.join("nested/AGENTS.md"))?;
        git(&rig, &["add", "-A"])?;
        git(&rig, &["commit", "-qm", "Upstream protected changes"])?;
        git(&rig, &["update-ref", "refs/remotes/origin/main", "HEAD"])?;
        git(
            &rig,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        )?;
        git(&rig, &["checkout", "-q", "main"])?;
        fs::write(path.join("source.txt"), b"writer source")?;
        git(&rig, &["-C", path_text, "add", "source.txt"])?;
        git(&rig, &["-C", path_text, "commit", "-qm", "Writer source"])?;
        git(
            &rig,
            &["-C", path_text, "merge", "--no-edit", "origin/main"],
        )?;
        if own_change {
            fs::write(path.join(".github/workflows/check.yml"), b"writer workflow")?;
            git(
                &rig,
                &["-C", path_text, "add", ".github/workflows/check.yml"],
            )?;
            git(
                &rig,
                &["-C", path_text, "commit", "-qm", "Writer protected change"],
            )?;
        }
        let head = git(&rig, &["-C", path_text, "rev-parse", "HEAD"])?;
        fs::write(&release, b"release")?;
        let terminal = await_terminal(&store, attempt, &call)?;
        let value = serde_json::to_value(&terminal)?;
        assert_eq!(
            value["state"]["outcome"],
            if own_change {
                "review_required"
            } else {
                "succeeded"
            }
        );
        if own_change {
            assert_eq!(value["head"], head);
            assert_eq!(
                value["protected_paths"],
                serde_json::json!([
                    {"encoding": "utf8", "path": ".github/workflows/check.yml"}
                ])
            );
            assert!(terminal.delivery.is_none());
        }
        wait_for_writer_release(&store)?;
        cleanup.disarm();
    }
    Ok(())
}

fn writer_terminal_refuses_sequencer_state() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    let path = rig.project.join(".worktrees/writer");
    let (roots, context) = writer_context(&rig)?;
    let cleanup = writer_process_cleanup(&context);
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let request = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let writer =
        provenant_run::prepare_writer(&request, context.state_root.as_path(), context.project_id)?;
    // Simulate an unfinished Git operation without widening the stub's grant.
    fs::write(
        writer
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD"),
        b"fixture sequencer state",
    )?;
    let prepared = writer_run(request, "writer-sequencer", 61, "create sequencer state")?;
    let attempt = prepared.attempts[0].attempt_id;
    let record = store.create(&prepared, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "control",
    };
    let holder = holder_context(&rig, &roots, &context);
    let started = start_writer(&store, record.run_id, &provider, &holder, &call)?;
    assert!(matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == attempt));
    let terminal = await_terminal(&store, attempt, &call)?;
    let provenant_run::AttemptState::Terminal {
        outcome, reason, ..
    } = terminal.state
    else {
        return Err("writer attempt was not terminal".into());
    };
    assert_eq!(outcome, provenant_run::Outcome::Failed);
    assert!(
        reason
            .as_deref()
            .is_some_and(|text| text.contains("writer terminal check"))
    );
    assert!(path.join(".git").is_file());
    let quarantine = context
        .state_root
        .as_path()
        .join("runs")
        .join(record.run_id.to_string())
        .join("attempts")
        .join(attempt.to_string())
        .join("quarantine");
    let manifest: Value = serde_json::from_slice(&fs::read(quarantine.join("manifest.json"))?)?;
    assert_eq!(manifest["review_required"], true);
    let evidence: Value = serde_json::from_slice(&fs::read(quarantine.join("evidence.json"))?)?;
    assert_eq!(evidence["review_required"], true);
    wait_for_writer_release(&store)?;

    // A private rewind may publish through CAS, retaining instruction review.
    fs::remove_file(
        writer
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD"),
    )?;
    let path_text = path.to_str().ok_or("fixture worktree encoding")?;
    git(&rig, &["-C", path_text, "reset", "--hard", "HEAD"])?;
    fs::write(path.join("source-base"), b"new base")?;
    git(&rig, &["-C", path_text, "add", "source-base"])?;
    git(&rig, &["-C", path_text, "commit", "-qm", "Writer base"])?;
    let request = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let prepared = writer_run(
        request,
        "writer-rewind",
        91,
        "hold while the base is rewritten",
    )?;
    let attempt = prepared.attempts[0].attempt_id;
    let record = store.create(&prepared, &call)?;
    let release = path.join(".writer-fixture-release");
    let _release_on_drop = ReleaseOnDrop(release.clone());
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "hold",
    };
    let started = start_writer(&store, record.run_id, &provider, &holder, &call)?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == attempt),
        "{started:?}"
    );
    wait_for_path(
        &path.join(".writer-fixture-ready"),
        Duration::from_secs(8),
        "sequencer writer hold point",
    )?;
    // A lane now rewinds its private ref. Rewinding the shared ref instead is
    // a competing host update, which correctly blocks CAS publication.
    let previous = git(&rig, &["-C", path_text, "rev-parse", "HEAD^"])?;
    let private_ref = context
        .state_root
        .as_path()
        .join("runs")
        .join(record.run_id.to_string())
        .join("attempts")
        .join(attempt.to_string())
        .join("tmp/candidate-0/git-common")
        .join(writer.branch.as_str());
    fs::write(private_ref, format!("{previous}\n"))?;
    fs::write(release, b"release")?;
    let terminal = await_terminal(&store, attempt, &call)?;
    assert_terminal_outcome(&terminal, provenant_run::Outcome::ReviewRequired)?;
    assert!(
        matches!(terminal.state, provenant_run::AttemptState::Terminal { reason: Some(reason), .. } if reason == "protected changes require review")
    );
    assert_eq!(
        git(&rig, &["-C", path_text, "rev-parse", "HEAD"])?,
        previous
    );
    wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

fn writer_production_plan_denies_outside_writes() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = repository()?;
    for name in ["writer", "sibling"] {
        ok(
            &rig,
            &["worktree", "create", name, "--base", "HEAD", "--json"],
        )?;
    }
    let path = rig.project.join(".worktrees/writer");
    std::os::unix::fs::symlink(&rig.project, path.join("escape"))?;
    let (roots, context) = writer_context(&rig)?;
    let cleanup = writer_process_cleanup(&context);
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let request = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let prepared = writer_run(
        request,
        "writer-negative",
        71,
        "probe the production write boundary",
    )?;
    let attempt = prepared.attempts[0].attempt_id;
    let record = store.create(&prepared, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "negative",
    };
    let started = start_writer(
        &store,
        record.run_id,
        &provider,
        &holder_context(&rig, &roots, &context),
        &call,
    )?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == attempt),
        "{started:?}"
    );
    assert_terminal_outcome(
        &await_terminal(&store, attempt, &call)?,
        provenant_run::Outcome::Uncommitted,
    )?;
    for target in [
        rig.project.join("negative-write"),
        rig.project.join(".worktrees/sibling/negative-write"),
        rig.project.join(".git/hooks/negative-hook"),
    ] {
        assert!(!target.exists(), "{}", target.display());
    }
    wait_for_writer_release(&store)?;
    cleanup.disarm();
    Ok(())
}

/// Crash only this fixture guardian after durable exit, before Run publication.
struct FixtureHolder<'a>(&'a dyn provenant_supervisor::Holder);
impl provenant_supervisor::Holder for FixtureHolder<'_> {
    fn provider_lease(
        &self,
        context: &[u8],
        resolved_route: &str,
    ) -> provenant_supervisor::custody::Result<Option<provenant_os::fs::LockGuard>> {
        self.0.provider_lease(context, resolved_route)
    }
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

fn writer_reconcile_refuses_unfinished_cherry_pick_and_releases() -> Result {
    if !writer_backend_available()? {
        return Ok(());
    }
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    fs::write(
        rig.project.join(".fixture-crash-terminal"),
        b"crash after exit",
    )?;
    let path = rig.project.join(".worktrees/writer");
    let (roots, context) = writer_context(&rig)?;
    let cleanup = writer_process_cleanup(&context);
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let request = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let writer =
        provenant_run::prepare_writer(&request, context.state_root.as_path(), context.project_id)?;
    let prepared = writer_run(request, "writer-recovered", 81, "recover an exited writer")?;
    let attempt = prepared.attempts[0].attempt_id;
    let record = store.create(&prepared, &call)?;
    let provider = WriterProvider {
        state: context.state_root.as_path().to_owned(),
        home: fs::canonicalize(&rig.home)?,
        project: context.project_id,
        mode: "edit",
    };
    let started = start_writer(
        &store,
        record.run_id,
        &provider,
        &holder_context(&rig, &roots, &context),
        &call,
    )?;
    assert!(
        matches!(started.as_slice(), [provenant_run::Started::Launched(id)] if *id == attempt),
        "{started:?}"
    );
    let attempt_path = context
        .state_root
        .as_path()
        .join("runs")
        .join(record.run_id.to_string())
        .join("attempts")
        .join(attempt.to_string());
    wait_for_path(
        &attempt_path.join("process-exit.json"),
        Duration::from_secs(8),
        "reconciled writer process exit",
    )?;
    assert!(!matches!(
        store.record(attempt, &call)?.state,
        provenant_run::AttemptState::Terminal { .. }
    ));
    // Incomplete quarantine must leave the attempt retryable, not seal a
    // terminal record whose missing manifest can never be released.
    let snapshot_path = attempt_path.join("quarantine/snapshot.json");
    let baseline = fs::read(&snapshot_path)?;
    fs::write(&snapshot_path, b"{}")?;
    let uncertain = store.reconcile_for_fixture(record.run_id, &provider, &call)?;
    assert!(
        matches!(uncertain.as_slice(), [(id, provenant_run::Reconciled::Uncertain { .. })] if *id == attempt)
    );
    assert!(!matches!(
        store.record(attempt, &call)?.state,
        provenant_run::AttemptState::Terminal { .. }
    ));
    assert!(!attempt_path.join("quarantine/manifest.json").exists());
    fs::write(snapshot_path, baseline)?;
    // The operation remains unfinished when the durable exit is recovered.
    fs::write(
        writer
            .worktree
            .repo()
            .private_git_dir()
            .join("CHERRY_PICK_HEAD"),
        b"unfinished cherry-pick",
    )?;
    // The backoff itself has unit coverage. Emulate its expiry so this fixture
    // can exercise recovery of the repaired archive without a real-time wait.
    let progress = attempt_path.join("settlement-progress.json");
    let mut journal: Value = serde_json::from_slice(&fs::read(&progress)?)?;
    journal["retry_after_ms"] = 0.into();
    fs::write(progress, serde_json::to_vec(&journal)?)?;
    let found = store.reconcile_for_fixture(record.run_id, &provider, &call)?;
    assert_eq!(found, vec![(attempt, provenant_run::Reconciled::Finished)]);
    let terminal = store.record(attempt, &call)?;
    assert_terminal_outcome(&terminal, provenant_run::Outcome::Failed)?;
    assert!(
        matches!(terminal.state, provenant_run::AttemptState::Terminal { reason: Some(reason), .. } if reason.contains("in-progress Git operation"))
    );
    assert!(attempt_path.join("release.json").is_file());
    assert!(store.writer_fences()?.is_empty());
    cleanup.disarm();
    Ok(())
}

pub(super) fn writer_process_cleanup(
    context: &provenant_run::ExecutionContext,
) -> provenant_supervisor::fixture::ProcessCleanupGuard {
    // Guardian records live under runs, alongside worktree metadata and child stores.
    provenant_supervisor::fixture::ProcessCleanupGuard::new(context.state_root.as_path())
}

fn start_writer(
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
    Ok(provenant_run::Request {
        clock,
        budget: provenant_core::WaitBudget::cli(clock.mono_now(), 30)?,
        cancel,
    })
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
            child_attempt_id_version: None,
            chair: None,
            fresh_continuation: None,
            schema_version: 1,
            tasks: vec![provenant_run::TaskRequest {
                build_heavy: false,
                title: None,
                base: None,
                head: None,
                base_ref: None,
                account: None,
                profile: provenant_run::HarnessProfile::default(),
                task_id: task,
                prompt_digest,
                route: provenant_run::RequestedRoute::Model {
                    provider: "claude".into(),
                    model: "claude/test-model".into(),
                    effort: None,
                },
                fallback: Vec::new(),
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

struct WriterProvider {
    state: PathBuf,
    home: PathBuf,
    project: provenant_core::ProjectId,
    mode: &'static str,
}
impl provenant_run::Providers for WriterProvider {
    fn prepare(
        &self,
        task: &provenant_run::TaskRequest,
        _prompt: &str,
        paths: &provenant_run::LaunchPaths,
        resume: Option<&provenant_run::AttemptRecord>,
    ) -> std::result::Result<provenant_run::PreparedLaunch, String> {
        if resume.is_some() {
            return Err("writer fixture does not resume".into());
        }
        let request = task.writer.as_ref().ok_or("writer request missing")?;
        let writer = provenant_run::prepare_writer(request, &self.state, self.project)
            .map_err(|error| error.to_string())?;
        let executable =
            fs::canonicalize(std::env::current_exe().map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let argv = vec![
            executable.clone().into_os_string(),
            OsString::from("--writer-provider"),
            OsString::from(self.mode),
            writer.worktree.path().as_os_str().to_owned(),
        ];
        let plan = provenant_cli::run::writer_fixture_plan(
            self.project,
            writer.worktree.path(),
            &self.home,
            paths,
            &writer,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        )?;
        let profile = provenant_confinement::permissions::ProviderProfile {
            spec: provenant_confinement::permissions::ProviderProfileSpec {
                authentication: vec![provenant_confinement::permissions::AuthenticationProfile {
                    mechanism: "login".into(),
                    // A public executable stands in for a login file; no live
                    // credential is read and this adds no write permission.
                    roots: vec![provenant_confinement::permissions::PathGrant {
                        path: executable.clone(),
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
            provider_lease: None,
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

fn holder_context<'a>(
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

fn await_terminal(
    store: &provenant_run::RunStore,
    attempt: provenant_core::AttemptId,
    call: &provenant_run::Request<'_>,
) -> Result<provenant_run::AttemptRecord> {
    let mut result = None;
    // A third import is eligible after the five- and thirty-second backoffs.
    if !eventually(Duration::from_secs(60), || {
        match store.record(attempt, call) {
            Ok(record) if matches!(record.state, provenant_run::AttemptState::Terminal { .. }) => {
                result = Some(Ok(record));
                true
            }
            Ok(_) => false,
            Err(error) => {
                result = Some(Err(error));
                true
            }
        }
    }) {
        let temporary = store.temporary_path(attempt, call)?;
        let directory = temporary.parent().ok_or("attempt directory")?;
        let progress = fs::read_to_string(directory.join("settlement-progress.json"))
            .unwrap_or_else(|error| error.to_string());
        return Err(format!(
            "writer attempt did not reach terminal state: {:?}; settlement: {progress}",
            store.record(attempt, call)?.state
        )
        .into());
    }
    Ok(result.expect("eventually returned on terminal state or error")?)
}

fn assert_terminal_outcome(
    record: &provenant_run::AttemptRecord,
    expected: provenant_run::Outcome,
) -> Result {
    let provenant_run::AttemptState::Terminal { outcome, .. } = &record.state else {
        return Err("writer attempt was not terminal".into());
    };
    assert_eq!(*outcome, expected);
    Ok(())
}

pub(super) fn wait_for_writer_release(store: &provenant_run::RunStore) -> Result {
    let mut failure = None;
    let released = eventually(Duration::from_secs(5), || match store.writer_fences() {
        Ok(fences) => fences.is_empty(),
        Err(error) => {
            failure = Some(error);
            true
        }
    });
    if let Some(error) = failure {
        return Err(error.into());
    }
    released
        .then_some(())
        .ok_or_else(|| "writer reservation was not released after terminal publication".into())
}

fn wait_for_path(path: &Path, timeout: Duration, label: &str) -> Result {
    if !eventually(timeout, || path.exists()) {
        return Err(format!("timed out waiting for {label}: {}", path.display()).into());
    }
    Ok(())
}

fn pause(duration: Duration) {
    let deadline = Instant::now() + duration;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        thread::park_timeout(left);
    }
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
            if !eventually(Duration::from_secs(10), || release.exists()) {
                return Err("writer fixture hold timed out".into());
            }
        }
        _ => return Err("unknown writer provider fixture mode".into()),
    }
    fs::write(worktree.join("writer-edited"), "writer fixture completed")?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    for event in [
        serde_json::json!({"type":"system","subtype":"init","session_id":"writer-fixture-session"}),
        serde_json::json!({"type":"assistant","message":{"model":"test-model","role":"assistant","content":[{"type":"text","text":"writer fixture completed"}]},"session_id":"writer-fixture-session"}),
        serde_json::json!({"type":"result","session_id":"writer-fixture-session","subtype":"success","is_error":false,"result":"writer fixture completed"}),
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

fn gc_refuses_bare_projects_and_their_linked_checkout_fallback() -> Result {
    for name in ["bare.git", ".git"] {
        let mut rig = repository()?;
        let container = rig.root.join("bare-container");
        fs::create_dir(&container)?;
        let bare = container.join(name);
        git(
            &rig,
            &[
                "clone",
                "--bare",
                "-q",
                rig.project.to_str().ok_or("path")?,
                bare.to_str().ok_or("path")?,
            ],
        )?;
        let linked = rig.root.join("bare-linked");
        git(
            &rig,
            &[
                "--git-dir",
                bare.to_str().ok_or("path")?,
                "worktree",
                "add",
                "-qb",
                "bare-linked",
                linked.to_str().ok_or("path")?,
            ],
        )?;
        let registration = git(
            &rig,
            &[
                "--git-dir",
                bare.to_str().ok_or("path")?,
                "worktree",
                "list",
                "--porcelain",
            ],
        )?;
        rig.project = linked.clone();
        let from_linked = command(&rig, &["gc", "--apply", "--json"])?;
        assert_ne!(
            code(&from_linked),
            0,
            "bare-backed linked checkout accepted as primary: {name}"
        );
        let value: Value = serde_json::from_slice(&from_linked.stdout.bytes)?;
        assert_eq!(value["error"]["code"], "GC");
        assert!(
            value["error"]["message"]
                .as_str()
                .ok_or("message")?
                .contains("bare repository")
        );
        rig.project = bare.clone();
        let from_bare = command(&rig, &["gc", "--apply", "--json"])?;
        assert_ne!(code(&from_bare), 0);
        assert!(bare.join("HEAD").is_file() && linked.join(".git").is_file());
        assert_eq!(
            git(&rig, &["worktree", "list", "--porcelain"])?,
            registration
        );
    }
    let rig = repository()?;
    let separate = rig.root.join("separate-git-dir");
    git(
        &rig,
        &[
            "init",
            "-q",
            "--separate-git-dir",
            separate.to_str().ok_or("path")?,
        ],
    )?;
    ok(&rig, &["gc", "--pressure", "--apply", "--json"])?;
    Ok(())
}

fn gc_pressure_collects_only_the_selected_primary_project() -> Result {
    let mut rig = repository()?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\ntarget/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["commit", "-qm", "ignore output"])?;
    let primary = rig.project.clone();
    let other = rig.root.join("other-project");
    git(
        &rig,
        &[
            "clone",
            "-q",
            primary.to_str().ok_or("path")?,
            other.to_str().ok_or("path")?,
        ],
    )?;
    rig.project = other;
    ok(&rig, &["worktree", "create", "other-idle", "--json"])?;
    let other_tree = rig.project.join(".worktrees/other-idle");
    let (_, other_context) = writer_context(&rig)?;
    fs::create_dir(other_tree.join("target"))?;
    fs::write(other_tree.join("target/artifact"), "other output")?;
    age_gc_output(&other_tree.join("target"))?;
    rig.project = primary;
    ok(&rig, &["worktree", "create", "idle", "--json"])?;
    let tree = rig.project.join(".worktrees/idle");
    let (_, context) = writer_context(&rig)?;
    fs::create_dir(tree.join("target"))?;
    fs::write(tree.join("target/artifact"), "selected output")?;
    age_gc_output(&tree.join("target"))?;
    let config = format!(
        "schema_version=1\n[gc]\nschedule=false\nfree_floor='1B'\n[gc.projects.'{}']\nregenerable=['target']\n[gc.projects.'{}']\nregenerable=['target']\n",
        context.project_id, other_context.project_id
    );
    fs::write(rig.state.join("config.toml"), &config)?;
    rig.project = tree.clone();
    let disabled = ok(&rig, &["gc", "--pressure", "--apply", "--json"])?;
    assert!(
        disabled["data"]["worktree_cleanup"]
            .as_array()
            .ok_or("cleanup")?
            .is_empty()
    );
    assert!(tree.join("target/artifact").is_file());
    let disabled_text = command(&rig, &["gc", "--pressure", "--apply"])?;
    assert_eq!(code(&disabled_text), 0);
    let disabled_text = String::from_utf8(disabled_text.stdout.bytes)?;
    assert!(
        disabled_text.contains("pressure collection skipped: gc.schedule=false"),
        "{disabled_text}"
    );
    fs::write(
        rig.state.join("config.toml"),
        config.replace("schedule=false", "schedule=true"),
    )?;
    let pressure = ok(&rig, &["gc", "--pressure", "--apply", "--json"])?;
    assert_eq!(
        pressure["data"]["worktree_cleanup"]
            .as_array()
            .ok_or("cleanup")?
            .len(),
        1
    );
    assert!(!tree.join("target").exists());
    assert!(other_tree.join("target/artifact").is_file());
    assert!(tree.is_dir() && other_tree.is_dir());
    let empty = command(&rig, &["gc", "--pressure", "--apply"])?;
    assert_eq!(code(&empty), 0);
    let empty = String::from_utf8(empty.stdout.bytes)?;
    assert!(
        empty.contains("pressure collection: no eligible data in scope"),
        "{empty}"
    );
    Ok(())
}

fn gc_reports_transient_and_preserves_unobserved_project_array() -> Result {
    let mut rig = repository()?;
    let parent = rig.root.join("scratchpads");
    fs::create_dir(&parent)?;
    let moved = parent.join("repository");
    fs::rename(&rig.project, &moved)?;
    rig.project = moved;
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_ROOT" {
            *value = rig.project.clone().into_os_string();
        }
    }
    let common = rig.project.join(".git");
    let outside = rig.root.join("outside");
    git(
        &rig,
        &[
            "worktree",
            "add",
            "-qb",
            "outside",
            outside.to_str().ok_or("path")?,
        ],
    )?;
    rig.project = outside;
    let preview = ok(&rig, &["gc", "--dry-run", "--json"])?;
    assert_eq!(
        preview["data"]["unobserved_projects"],
        serde_json::json!([])
    );
    assert_eq!(
        preview["data"]["skipped_transient"],
        serde_json::json!([common])
    );
    let output = command(&rig, &["gc", "--dry-run"])?;
    assert_eq!(code(&output), 0);
    let text = String::from_utf8(output.stdout.bytes)?;
    assert!(text.contains("skipped transient project"), "{text}");
    let mut normal = repository()?;
    ok(&normal, &["worktree", "create", "observe", "--json"])?;
    let scratch = normal.root.join("scratchpads");
    fs::create_dir(&scratch)?;
    let linked = scratch.join("linked");
    git(
        &normal,
        &[
            "worktree",
            "add",
            "-qb",
            "linked",
            linked.to_str().ok_or("path")?,
        ],
    )?;
    normal.project = linked;
    let normal_preview = ok(&normal, &["gc", "--dry-run", "--json"])?;
    assert_eq!(
        normal_preview["data"]["skipped_transient"],
        serde_json::json!([])
    );
    Ok(())
}

fn scheduled_gc_runs_outside_a_checkout_and_obeys_disable_and_reservations() -> Result {
    let mut rig = repository()?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\ntarget/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["commit", "-qm", "ignore output"])?;
    ok(&rig, &["worktree", "create", "idle", "--json"])?;
    let worktree = rig.project.join(".worktrees/idle");
    let cache = prepare_lane_cache(&rig, &worktree, "idle")?;
    fn age(path: &Path) -> Result {
        if path.is_dir() {
            for entry in fs::read_dir(path)? {
                age(&entry?.path())?;
            }
        }
        fs::File::open(path)?.set_modified(std::time::UNIX_EPOCH)?;
        Ok(())
    }
    age(&cache)?;
    let project = fs::read_dir(rig.state.join("worktrees"))?
        .next()
        .ok_or("project")??
        .path();
    let registration = fs::read_dir(&project)?
        .find_map(|entry| {
            let path = entry.ok()?.path();
            path.join("identity.json").is_file().then_some(path)
        })
        .ok_or("registration")?;
    // launchd starts without a checkout as its working directory.
    rig.project = rig.home.clone();
    fs::write(
        rig.state.join("config.toml"),
        b"schema_version=1\n[gc]\nfree_floor='1B'\nschedule=false\n",
    )?;
    let disabled = ok(&rig, &["gc", "--scheduled", "--idle-days", "1", "--json"])?;
    assert!(
        disabled["data"]["removed"]
            .as_array()
            .ok_or("removed")?
            .is_empty()
    );
    assert!(cache.is_dir());
    let pressure_disabled = ok(
        &rig,
        &[
            "gc",
            "--scheduled",
            "--pressure",
            "--apply",
            "--idle-days",
            "1",
            "--json",
        ],
    )?;
    assert!(
        pressure_disabled["data"]["removed"]
            .as_array()
            .ok_or("removed")?
            .is_empty()
    );
    assert!(cache.is_dir());
    fs::write(
        rig.state.join("config.toml"),
        b"schema_version=1\n[gc]\nfree_floor='1B'\n",
    )?;
    fs::write(registration.join("reservation.json"), b"reserved")?;
    let reserved = ok(&rig, &["gc", "--scheduled", "--idle-days", "1", "--json"])?;
    assert!(
        reserved["data"]["removed"]
            .as_array()
            .ok_or("removed")?
            .is_empty()
    );
    assert!(cache.is_dir());
    fs::remove_file(registration.join("reservation.json"))?;
    let dry = ok(
        &rig,
        &[
            "gc",
            "--scheduled",
            "--idle-days",
            "1",
            "--dry-run",
            "--json",
        ],
    )?;
    assert_eq!(dry["data"]["removed"][0]["reason"], "idle");
    assert!(cache.is_dir());
    fs::write(cache.join("artifact"), b"recent build")?;
    let removed = ok(
        &rig,
        &[
            "gc",
            "--scheduled",
            "--idle-days",
            "1",
            "--free-floor",
            "18446744073709551615B",
            "--json",
        ],
    )?;
    assert_eq!(removed["data"]["removed"][0]["reason"], "free_floor");
    assert!(removed["data"]["free_bytes_before"].is_u64());
    assert!(removed["data"]["free_bytes_after"].is_u64());
    assert!(!cache.exists());
    let project_id = project
        .file_name()
        .ok_or("project id")?
        .to_str()
        .ok_or("project id")?;
    fs::write(
        rig.state.join("config.toml"),
        format!("schema_version=1\n[gc.projects.'{project_id}']\nregenerable=['target']\n"),
    )?;
    fs::create_dir(worktree.join("target"))?;
    fs::write(worktree.join("target/artifact"), "old output")?;
    age_gc_output(&worktree.join("target"))?;
    let pressure = ok(
        &rig,
        &[
            "gc",
            "--scheduled",
            "--pressure",
            "--apply",
            "--free-floor",
            "18446744073709551615B",
            "--json",
        ],
    )?;
    assert!(worktree.is_dir());
    assert!(!worktree.join("target").exists());
    assert_eq!(
        pressure["data"]["worktree_cleanup"][0]["idle_output"][0]["removed"],
        true
    );
    assert!(
        pressure["data"]["worktree_cleanup"][0]["worktrees"]["removed"]
            .as_array()
            .ok_or("removed")?
            .is_empty()
    );
    Ok(())
}

fn gc_idle_keeps_live_and_unobservable_writers_and_removes_free_cache() -> Result {
    use provenant_os::fs::Directory;
    use provenant_os::fs::LockMode;
    use std::ffi::OsStr;

    fn make_idle(path: &Path) -> Result {
        if path.is_dir() {
            for entry in fs::read_dir(path)? {
                make_idle(&entry?.path())?;
            }
        }
        fs::File::open(path)?.set_modified(std::time::UNIX_EPOCH)?;
        Ok(())
    }

    for writer in [
        "live",
        "live_reserved",
        "free",
        "missing",
        "unsafe",
        "reserved",
    ] {
        let rig = repository()?;
        ok(&rig, &["worktree", "create", "idle", "--json"])?;
        let worktree = rig.project.join(".worktrees/idle");
        let cache = prepare_lane_cache(&rig, &worktree, "idle")?;
        let projects =
            fs::read_dir(rig.state.join("worktrees"))?.collect::<std::io::Result<Vec<_>>>()?;
        let project = projects.first().ok_or("project state")?.path();
        let states = fs::read_dir(project)?.collect::<std::io::Result<Vec<_>>>()?;
        let state = states
            .iter()
            .find(|entry| entry.path().join("identity.json").is_file())
            .ok_or("worktree registration")?
            .path();
        let directory = Directory::open_anchor(&state)?;
        let lock = OsStr::new("writer.lock");
        let held = match writer {
            "live" | "live_reserved" => {
                let held = directory.try_lock_existing(lock, LockMode::Exclusive)?;
                if writer == "live_reserved" {
                    fs::write(state.join("reservation.json"), b"active writer")?;
                }
                Some(held)
            }
            "missing" => {
                fs::remove_file(state.join(lock))?;
                None
            }
            "unsafe" => {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(state.join(lock), fs::Permissions::from_mode(0o666))?;
                None
            }
            "reserved" => {
                fs::write(state.join("reservation.json"), b"unsettled writer")?;
                None
            }
            _ => None,
        };
        make_idle(&cache)?;
        let listed = ok(&rig, &["worktree", "list", "--json"])?;
        let listed_writer = listed["data"]
            .as_array()
            .ok_or("worktrees")?
            .iter()
            .find(|info| info["branch"] == "refs/heads/idle")
            .ok_or("idle worktree")?["writer"]
            .as_str()
            .ok_or("writer state")?;
        assert_eq!(
            listed_writer,
            match writer {
                "live" => "live",
                "free" => "free",
                _ => "unknown",
            }
        );
        for dry_run in [true, false] {
            let mut args = vec!["gc", "--idle-days", "1", "--json"];
            if dry_run {
                args.push("--dry-run");
            }
            let result = ok(&rig, &args)?;
            assert_eq!(result["data"]["dry_run"], dry_run);
            if writer == "free" {
                assert_eq!(result["data"]["removed"][0]["reason"], "idle");
                assert_eq!(cache.exists(), dry_run);
            } else {
                assert!(
                    result["data"]["removed"]
                        .as_array()
                        .ok_or("removed")?
                        .is_empty()
                );
                assert_eq!(
                    result["data"]["kept"][0]["reason"],
                    // A reservation makes the listing unknown even when the
                    // kernel lock is held; GC preserves that observation.
                    if writer == "live" {
                        "live"
                    } else {
                        "unobservable"
                    }
                );
                assert!(cache.is_dir());
            }
        }
        if writer == "missing" {
            assert!(!state.join(lock).exists(), "gc must not recreate the lock");
        }
        drop(held);
        if writer.starts_with("live") {
            let _released = directory.try_lock_existing(lock, LockMode::Exclusive)?;
        } else if writer == "missing" {
            assert!(matches!(
                directory.try_lock_existing(lock, LockMode::Exclusive),
                Err(error) if error.is_not_found()
            ));
        }
        rig.assert_legacy_unchanged()?;
    }
    Ok(())
}

fn create_list_and_remove_a_managed_worktree() -> Result {
    let rig = repository()?;
    let base = git(&rig, &["rev-parse", "HEAD"])?;
    let created = ok(
        &rig,
        &[
            "worktree",
            "create",
            "feature/topic",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    assert_eq!(created["command"], "worktree create");
    assert_eq!(created["data"]["head"], base);
    assert!(rig.project.join(".worktrees/feature-topic/.git").is_file());
    let listed = ok(&rig, &["worktree", "list", "--json"])?;
    let trees = listed["data"]
        .as_array()
        .ok_or("worktree list is not an array")?;
    assert_eq!(trees.len(), 2);
    assert!(
        trees
            .iter()
            .any(|tree| tree["branch"] == "refs/heads/feature/topic" && tree["dirty"] == "clean")
    );
    let cache = prepare_lane_cache(
        &rig,
        &rig.project.join(".worktrees/feature-topic"),
        "feature/topic",
    )?;
    ok(&rig, &["worktree", "create", "feature/gone", "--json"])?;
    let gone = rig.project.join(".worktrees/feature-gone");
    let orphan = prepare_lane_cache(&rig, &gone, "feature/gone")?;
    git(
        &rig,
        &["worktree", "remove", gone.to_str().ok_or("worktree path")?],
    )?;
    assert!(!gone.exists());
    assert!(orphan.is_dir());
    let removed = ok(&rig, &["worktree", "remove", "feature/topic", "--json"])?;
    assert_eq!(
        removed["data"]["removed"]
            .as_array()
            .ok_or("missing removals")?
            .len(),
        1
    );
    assert!(!rig.project.join(".worktrees/feature-topic").exists());
    assert!(!cache.exists(), "orphaned lane cache remains after removal");
    assert!(
        orphan.is_dir(),
        "removing one worktree must preserve other orphaned caches"
    );
    assert_eq!(
        fs::read(orphan.join("artifact"))?,
        b"lane build output",
        "removing one worktree must preserve other orphaned caches"
    );
    assert_eq!(
        ok(&rig, &["worktree", "list", "--json"])?["data"]
            .as_array()
            .ok_or("missing list")?
            .len(),
        1
    );
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(!orphan.exists(), "gc must still sweep orphaned caches");
    rig.assert_legacy_unchanged()
}

fn recreated_worktree_accepts_writer_after_settlement() -> Result {
    use provenant_os::fs::Directory;
    use provenant_supervisor::custody::{Scope, WriterBinding};

    let rig = repository()?;
    ok(&rig, &["worktree", "create", "reused", "--json"])?;
    let path = rig.project.join(".worktrees/reused");
    let (_, context) = writer_context(&rig)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let scope = Scope {
        run: provenant_core::RunId::from_bytes([31; 16]),
        attempt: provenant_core::AttemptId::from_bytes([32; 16]),
        generation: 1,
    };
    let state = rig
        .state
        .join("worktrees")
        .join(context.project_id.to_string())
        .join(provenant_confinement::worktrees::state_dir_name(&path));
    let acquire = |scope| -> Result<_> {
        let request =
            provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
        let writer = provenant_run::prepare_writer(
            &request,
            context.state_root.as_path(),
            context.project_id,
        )?;
        let binding = WriterBinding::from_verified(
            writer.worktree.repo().root_dir(),
            writer.worktree.repo().private_dir(),
            writer.branch.as_str(),
            provenant_core::GitOid::parse(&git(&rig, &["rev-parse", "HEAD"])?)?,
        )?;
        Ok(provenant_supervisor::fixture::acquire_writer_with_binding(
            Directory::open_anchor(&state)?,
            scope,
            binding,
            provenant_core::WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?)
    };
    let old_tree = Directory::open_anchor(&path)?;
    drop(acquire(scope)?);
    assert!(acquire(scope).is_err_and(|error| {
        error
            .to_string()
            .contains("writer reservation requires recovery")
    }));
    provenant_supervisor::custody::acquire_recovery(
        Directory::open_anchor(&state)?,
        provenant_core::WaitBudget::nonblocking(clock.mono_now()),
        &cancel,
        &clock,
    )?
    .withdraw_prepared()?;
    ok(&rig, &["worktree", "remove", "reused", "--json"])?;
    ok(&rig, &["worktree", "create", "reused", "--json"])?;
    // The settled run cannot carry a later generation onto a new worktree.
    assert!(
        acquire(Scope {
            generation: 2,
            ..scope
        })
        .is_err_and(|error| error
            .to_string()
            .contains("writer reservation requires recovery"))
    );
    let lease = acquire(Scope {
        run: provenant_core::RunId::from_bytes([33; 16]),
        attempt: provenant_core::AttemptId::from_bytes([34; 16]),
        ..scope
    })?;
    lease.withdraw_unlaunched()?;
    // The first run cannot return to the path by readmitting its attempt
    // once another run has owned it.
    assert!(
        acquire(Scope {
            generation: 2,
            ..scope
        })
        .is_err_and(|error| error
            .to_string()
            .contains("writer reservation requires recovery"))
    );
    drop(old_tree);
    Ok(())
}

fn remove_a_managed_worktree_deletes_its_legacy_lane_cache() -> Result {
    let rig = repository()?;
    ok(&rig, &["worktree", "create", "legacy", "--json"])?;
    let worktree = rig.project.join(".worktrees/legacy");
    let cache = prepare_lane_cache(&rig, &worktree, "legacy")?;
    fs::write(
        cache.join(".provenant-worktree-owner"),
        [
            b"provenant-lane-cargo-owner-v1\n".as_slice(),
            fs::canonicalize(&worktree)?.as_os_str().as_encoded_bytes(),
        ]
        .concat(),
    )?;
    ok(&rig, &["worktree", "remove", "legacy", "--json"])?;
    assert!(!worktree.exists());
    assert!(!cache.exists(), "legacy lane cache remains after removal");
    rig.assert_legacy_unchanged()
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

fn merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees() -> Result {
    for since_scope in [false, true] {
        let rig = repository()?;
        let old = branch_commit(&rig, "old")?;
        git(&rig, &["merge", "--no-ff", "-qm", "Merge old", "old"])?;
        let bound = git(&rig, &["rev-parse", "HEAD"])?;
        let merged = branch_commit(&rig, "merged")?;
        git(&rig, &["merge", "--no-ff", "-qm", "Merge clean", "merged"])?;
        let dirty = branch_commit(&rig, "dirty")?;
        git(&rig, &["merge", "--no-ff", "-qm", "Merge dirty", "dirty"])?;
        fs::write(dirty.join("precious"), "uncommitted work")?;
        let unmerged = branch_commit(&rig, "unmerged")?;
        let mut args = vec![
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "main",
            "--json",
        ];
        if since_scope {
            args.extend(["--since", &bound]);
        } else {
            args.extend([
                "--branch", "merged", "--branch", "dirty", "--branch", "unmerged",
            ]);
        }
        let result = ok(&rig, &args)?;
        assert_eq!(result["data"]["pruned_refs"], serde_json::json!(["merged"]));
        assert!(!merged.exists());
        assert!(old.is_dir());
        assert!(unmerged.is_dir());
        assert_eq!(
            fs::read_to_string(dirty.join("precious"))?,
            "uncommitted work"
        );
        assert_eq!(git(&rig, &["rev-parse", "old"])?.len(), 40);
        assert_eq!(git(&rig, &["rev-parse", "dirty"])?.len(), 40);
        assert_eq!(git(&rig, &["rev-parse", "unmerged"])?.len(), 40);
        let refused = command(&rig, &["worktree", "remove", "dirty", "--json"])?;
        assert_eq!(code(&refused), 1);
        assert_eq!(json(&refused)?["status"], "refused");
        assert!(dirty.join("precious").is_file());
        rig.assert_legacy_unchanged()?;
    }
    Ok(())
}

fn remove_by_name_deletes_a_branch_fully_contained_in_integration() -> Result {
    let rig = repository()?;
    git(
        &rig,
        &["remote", "add", "origin", "/missing/offline-upstream.git"],
    )?;
    // At the integration tip: nothing unique, so the name removal deletes it.
    ok(
        &rig,
        &["worktree", "create", "at-tip", "--base", "HEAD", "--json"],
    )?;
    let at_tip = rig.project.join(".worktrees/at-tip");
    let output = command(&rig, &["worktree", "remove", "at-tip", "--json"])?;
    assert_eq!(code(&output), 0);
    assert!(
        String::from_utf8_lossy(&output.stderr.bytes)
            .contains("using the current integration resolution")
    );
    let removed = json(&output)?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["at-tip"])
    );
    assert!(!at_tip.exists());
    assert!(!has_branch(&rig, "at-tip")?);

    // An ancestor of a later integration tip: its commit is contained in main.
    let old = git(&rig, &["rev-parse", "HEAD"])?;
    ok(
        &rig,
        &["worktree", "create", "behind", "--base", &old, "--json"],
    )?;
    git(
        &rig,
        &["commit", "--allow-empty", "-qm", "advance integration"],
    )?;
    let removed = ok(&rig, &["worktree", "remove", "behind", "--json"])?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["behind"])
    );
    assert!(!has_branch(&rig, "behind")?);
    rig.assert_legacy_unchanged()
}

fn remove_resolves_directory_names_and_remote_integration_refs() -> Result {
    let rig = repository()?;
    git(
        &rig,
        &["remote", "add", "origin", "/missing/offline-upstream.git"],
    )?;
    // The worktree directory is the branch with `/` replaced by `-`.
    ok(
        &rig,
        &[
            "worktree",
            "create",
            "fix/shipped",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    let shipped = rig.project.join(".worktrees/fix-shipped");
    let shipped_text = shipped.to_str().ok_or("non-text fixture path")?;
    fs::write(shipped.join("shipped.txt"), "work")?;
    git(&rig, &["-C", shipped_text, "add", "shipped.txt"])?;
    git(&rig, &["-C", shipped_text, "commit", "-qm", "shipped"])?;
    // Local main stays behind; the work is only on the remote-tracking ref.
    let tip = git(&rig, &["rev-parse", "fix/shipped"])?;
    git(&rig, &["update-ref", "refs/remotes/origin/main", &tip])?;

    let removed = ok(
        &rig,
        &[
            "worktree",
            "remove",
            "fix-shipped",
            "--integration",
            "origin/main",
            "--json",
        ],
    )?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["fix/shipped"])
    );
    assert!(!shipped.exists());
    assert!(!has_branch(&rig, "fix/shipped")?);

    // The merged sweep accepts the same remote-tracking integration ref.
    let swept = branch_commit(&rig, "swept")?;
    let swept_tip = git(&rig, &["rev-parse", "swept"])?;
    // A merge of the branch into the remote's main: contained, and not on the
    // first-parent history, so the sweep may delete it.
    let head = git(&rig, &["rev-parse", "HEAD"])?;
    let merge_tree = git(&rig, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let next = git(
        &rig,
        &[
            "commit-tree",
            &merge_tree,
            "-p",
            &head,
            "-p",
            &swept_tip,
            "-m",
            "merge swept",
        ],
    )?;
    git(&rig, &["update-ref", "refs/remotes/origin/main", &next])?;
    let stale = git(&rig, &["rev-parse", "HEAD"])?;
    git(
        &rig,
        &[
            "worktree",
            "remove",
            "--force",
            swept.to_str().ok_or("path")?,
        ],
    )?;
    let result = ok(
        &rig,
        &[
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "origin/main",
            "--branch",
            "swept",
            "--json",
        ],
    )?;
    assert_eq!(result["data"]["pruned_refs"], serde_json::json!(["swept"]));
    assert!(!has_branch(&rig, "swept")?);
    assert_eq!(git(&rig, &["rev-parse", "HEAD"])?, stale);
    rig.assert_legacy_unchanged()
}

fn has_branch(rig: &TestRig, name: &str) -> Result<bool> {
    let reference = format!("refs/heads/{name}");
    Ok(!git(rig, &["for-each-ref", reference.as_str()])?.is_empty())
}

fn remove_by_name_follows_the_branch_merge_state() -> Result {
    let rig = repository()?;
    let merged = branch_commit(&rig, "merged")?;
    git(&rig, &["merge", "--no-ff", "-qm", "Merge merged", "merged"])?;
    let unmerged = branch_commit(&rig, "unmerged")?;
    let dirty = branch_commit(&rig, "dirty")?;
    let dirty_head = git(&rig, &["rev-parse", "dirty"])?;
    fs::write(dirty.join("precious"), "uncommitted work")?;

    let refused = command(
        &rig,
        &["worktree", "remove", "dirty", "--discard", "--json"],
    )?;
    assert_eq!(code(&refused), 1);
    assert_eq!(json(&refused)?["status"], "refused");
    assert!(has_branch(&rig, "dirty")?);
    assert!(dirty.join("precious").is_file());

    let removed = ok(&rig, &["worktree", "remove", "merged", "--json"])?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["merged"])
    );
    assert!(!merged.exists());
    assert!(!has_branch(&rig, "merged")?);

    let kept = ok(&rig, &["worktree", "remove", "unmerged", "--json"])?;
    assert!(
        kept["data"]["pruned_refs"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert_eq!(kept["data"]["retained"][0]["branch"], "unmerged");
    assert_eq!(kept["data"]["retained"][0]["reason"], "not_merged");
    assert!(!unmerged.exists());
    assert!(has_branch(&rig, "unmerged")?);

    fs::remove_file(dirty.join("precious"))?;
    let discarded = ok(
        &rig,
        &["worktree", "remove", "dirty", "--discard", "--json"],
    )?;
    assert_eq!(discarded["data"]["discarded"][0]["branch"], "dirty");
    assert_eq!(discarded["data"]["discarded"][0]["head"], dirty_head);
    assert!(!dirty.exists());
    assert!(!has_branch(&rig, "dirty")?);
    rig.assert_legacy_unchanged()
}

fn merged_pruning_deletes_a_merged_branch_without_a_worktree() -> Result {
    let rig = repository()?;
    let stale = branch_commit(&rig, "stale")?;
    let stale_text = stale.to_str().ok_or("non-text fixture path")?;
    git(&rig, &["worktree", "remove", "--force", stale_text])?;
    git(&rig, &["merge", "--no-ff", "-qm", "Merge stale", "stale"])?;
    let pruned = ok(
        &rig,
        &[
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "main",
            "--branch",
            "stale",
            "--json",
        ],
    )?;
    assert_eq!(pruned["data"]["pruned_refs"], serde_json::json!(["stale"]));
    assert!(!has_branch(&rig, "stale")?);
    rig.assert_legacy_unchanged()
}

fn dispatch_into_an_unregistered_worktree_names_the_create_command() -> Result {
    unregistered_worktree_dispatch_is_refused(false)
}

// Once another worktree is registered the project's state directory exists,
// so the missing record is the foreign worktree's own.
fn dispatch_into_an_unregistered_worktree_beside_registered_ones_names_the_create_command() -> Result
{
    unregistered_worktree_dispatch_is_refused(true)
}

fn unregistered_worktree_dispatch_is_refused(register_sibling: bool) -> Result {
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
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "hello")?;
    let refused = command(
        &rig,
        &[
            "dispatch",
            "--prompt-file",
            prompt.to_str().ok_or("prompt path")?,
            "--model",
            "codex/id",
            "--mode",
            "worktree-write",
            "--worktree",
            ".worktrees/foreign",
            "--operation-id",
            "foreign",
            "--json",
        ],
    )?;
    let value = json(&refused)?;
    assert_eq!(code(&refused), 1, "{value}");
    assert_eq!(value["status"], "refused", "{value}");
    assert_eq!(value["error"]["code"], "WORKTREE_UNREGISTERED", "{value}");
    let message = value["error"]["message"].as_str().ok_or("message")?;
    assert!(
        message.contains("provenant worktree create --base <branch> <branch>"),
        "{message}"
    );
    Ok(())
}

// A tree re-added with plain Git at a path Provenant registered earlier is a
// stale registration: dispatch names it and the repair, and `worktree adopt`
// from outside the tree makes the tree dispatchable.
fn re_added_worktree_is_adopted_then_dispatchable() -> Result {
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "stale", "--base", "HEAD", "--json"],
    )?;
    git(&rig, &["worktree", "remove", ".worktrees/stale"])?;
    git(
        &rig,
        &["worktree", "add", "-qb", "stale-again", ".worktrees/stale"],
    )?;
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "hello")?;
    let prompt = prompt.to_str().ok_or("prompt path")?;
    let dispatch = |operation: &str| -> Result<Value> {
        json(&command(
            &rig,
            &[
                "dispatch",
                "--prompt-file",
                prompt,
                "--model",
                "codex/id",
                "--mode",
                "worktree-write",
                "--worktree",
                ".worktrees/stale",
                "--operation-id",
                operation,
                "--json",
            ],
        )?)
    };
    let refused = dispatch("stale-before")?;
    assert_eq!(refused["status"], "refused", "{refused}");
    let message = refused["error"]["message"].as_str().ok_or("message")?;
    assert!(message.contains("registration is stale"), "{message}");
    assert!(
        message.contains("provenant worktree adopt PATH --owner NAME"),
        "{message}"
    );

    let adopted = ok(
        &rig,
        &[
            "worktree",
            "adopt",
            ".worktrees/stale",
            "--owner",
            "chair",
            "--json",
        ],
    )?;
    assert_eq!(adopted["data"]["writer"], "free", "{adopted}");
    let (_, context) = writer_context(&rig)?;
    let request = provenant_run::resolve_writer(
        context.state_root.as_path(),
        &rig.project,
        Path::new(".worktrees/stale"),
    )?;
    provenant_run::prepare_writer(&request, context.state_root.as_path(), context.project_id)?;
    // Dispatch now gets past custody; whatever it reports is not the stale
    // registration.
    let after = dispatch("stale-after")?;
    let message = after["error"]["message"].as_str().unwrap_or_default();
    assert!(!message.contains("writer scope refused"), "{after}");
    Ok(())
}

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
    for tool in ["cargo", "rustc"] {
        std::os::unix::fs::symlink(
            target.join("bin").join(tool),
            home.join(".cargo/bin").join(tool),
        )?;
    }
    Ok(())
}

fn lane_prepare_isolates_nested_state() -> Result {
    use provenant_confinement::permissions::PathScope;
    let rig = repository()?;
    let cleanup = provenant_supervisor::fixture::ProcessCleanupGuard::new(&rig.state);
    ok(
        &rig,
        &["worktree", "create", "writer", "--base", "HEAD", "--json"],
    )?;
    ok(
        &rig,
        &[
            "worktree",
            "create",
            "nested-child",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    let worktree = rig.project.join(".worktrees/writer");
    // The repository's Cargo config names a checked-in rustc wrapper.
    fs::create_dir_all(worktree.join(".cargo"))?;
    fs::write(
        worktree.join(".cargo/config.toml"),
        "[build]\nrustc-wrapper = \".cargo/rustc-wrapper.sh\"\n",
    )?;
    fs::write(
        worktree.join(".cargo/rustc-wrapper.sh"),
        "#!/bin/sh\nexec \"$@\"\n",
    )?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            worktree.join(".cargo/rustc-wrapper.sh"),
            fs::Permissions::from_mode(0o755),
        )?;
    }
    let worktree_arg = worktree.to_str().ok_or("worktree path")?;
    git(&rig, &["-C", worktree_arg, "add", ".cargo"])?;
    git(
        &rig,
        &[
            "-C",
            worktree_arg,
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-q",
            "-m",
            "wrapper",
        ],
    )?;
    let (roots, context) = writer_context(&rig)?;
    let identity = command(&rig, &["fabric", "identity", "--json"])?;
    assert_eq!(
        code(&identity),
        0,
        "{}",
        String::from_utf8_lossy(&identity.stderr.bytes)
    );
    let home = rig.root.join("fixture-home");
    fs::create_dir_all(home.join(".claude-fixture"))?;
    if !cfg!(target_os = "macos") {
        // Off macOS, Claude keeps its login in the account directory, not a keychain.
        fs::write(
            home.join(".claude-fixture/.credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"fixture-token","expiresAt":4000000000000}}"#,
        )?;
    }
    fs::create_dir_all(home.join(".codex"))?;
    fs::write(
        home.join(".codex/auth.json"),
        br#"{"tokens":{"account_id":"fixture-one"}}"#,
    )?;
    for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
        fs::create_dir_all(home.join(path))?;
    }
    fs::write(home.join(".rustup/settings.toml"), b"fixture settings")?;
    for secret in ["credentials.toml", "credentials"] {
        fs::write(home.join(".cargo").join(secret), b"synthetic credential")?;
    }
    // Cargo fails when its user config exists but is unreadable, so lanes
    // read it; registry tokens belong in the denied credentials files.
    fs::write(home.join(".cargo/config.toml"), b"[net]\nretry = 1\n")?;
    fs::create_dir_all(home.join("Library/Keychains"))?;
    fs::write(
        home.join("Library/Keychains/login.keychain-db"),
        b"synthetic credential",
    )?;
    // Clone immutable tools into the isolated home. No test reads host
    // provider configuration, login state or Cargo credentials.
    #[cfg(target_os = "macos")]
    clone_rust_toolchain(&rig, &home)?;
    let executable = rig.root.join("provider/bin/codex");
    fs::create_dir_all(executable.parent().ok_or("provider parent")?)?;
    rig.copy_executable(&std::env::current_exe()?, &executable)?;
    // Publishing must never depend on gh or login state from the host PATH.
    std::os::unix::fs::symlink(&executable, executable.with_file_name("gh"))?;
    // Production preparation needs registered, freshly observed accounts.
    let home = fs::canonicalize(&home)?;
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let mut accounts = String::new();
    let mut claude_key = String::new();
    let probe_root = tempfile::tempdir()?;
    for (adapter, label, directory) in [
        ("codex", "default", home.join(".codex")),
        ("claude", "fixture", home.join(".claude-fixture")),
    ] {
        let key = provenant_providers::accounts::detect(
            &executor,
            adapter,
            &executable,
            &home,
            "fixture".as_ref(),
            &directory,
            probe_root.path(),
        )?;
        if adapter == "claude" {
            claude_key.clone_from(&key);
        }
        accounts.push_str(&format!(
            "[domains.accounts.entries.{adapter}-{label}]\nadapter={adapter:?}\nlabel={label:?}\nkey={key:?}\ndirectory={:?}\n",
            directory.to_str().ok_or("account path")?
        ));
    }
    let config = |depth: u8| {
        format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\n[knobs]\n'lanes.max_depth'={depth}\n{accounts}"
        )
    };
    fs::write(rig.state.join("config.toml"), config(1))?;
    for (adapter, writer, publishing, seed) in [
        ("codex", false, false, 91),
        ("codex", true, false, 92),
        ("claude", false, false, 93),
        ("claude", true, false, 94),
        ("codex", true, true, 95),
        ("claude", true, true, 96),
    ] {
        if publishing {
            fs::create_dir_all(home.join(".config/gh"))?;
        }
        let request =
            provenant_run::resolve_writer(roots.state.as_path(), &rig.project, &worktree)?;
        let mut prepared = writer_run(
            request,
            &format!("lane-prepare-{adapter}-{writer}-{publishing}"),
            seed,
            "fixture prompt",
        )?;
        let task = &mut prepared.request.tasks[0];
        if !writer {
            task.writer = None;
        }
        // Catalogue models only: production refuses unverified exact IDs.
        task.route = provenant_run::RequestedRoute::Model {
            provider: "claude".into(),
            model: "claude/claude-sonnet-5".into(),
            effort: None,
        };
        let attempt = roots.state.as_path().join(format!(
            "runs/{}/attempts/{}",
            prepared.run_id, prepared.attempts[0].attempt_id
        ));
        let paths = provenant_run::LaunchPaths {
            attempt_id: prepared.attempts[0].attempt_id,
            lane_depth: 1,
            run_id: prepared.run_id,
            operation_id: prepared.operation_id.clone(),
            task_index: 0,
            deadline_unix_ms: 4_000_000_000_000,
            scratch: attempt.join("scratch"),
            provider_state: attempt.join("provider-state"),
            temporary: attempt.join("tmp"),
        };
        // The active release root is its own directory, never the project.
        let release = rig.state.with_file_name("release");
        fs::create_dir_all(&release)?;
        let registry = provenant_cli::run::Registry {
            roots: Some(&roots),
            home: Some(&home),
            assets: Some(&release),
            cwd: Some(&rig.project),
            state_root: Some(roots.state.as_path()),
            project_id: Some(context.project_id),
            host: None,
            cancel: None,
        };
        // Simulate a partially written first candidate. Fallback must have
        // independent setup paths rather than failing on the same auth entry.
        task.fallback.push(provenant_run::RouteRecord {
            provider: Some(adapter.into()),
            model: Some(
                if adapter == "codex" {
                    "codex/gpt-6-luna"
                } else {
                    "claude/claude-haiku-5-5"
                }
                .into(),
            ),
            ..Default::default()
        });
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let call = writer_call(&clock, &cancel)?;
        let store = provenant_run::RunStore::open(&context, &call)?;
        store.create(&prepared, &call)?;
        let task = &prepared.request.tasks[0];
        for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
            fs::create_dir_all(path)?;
        }
        let staging = &paths.temporary;
        // Setup repairs partial files in place (#1461), so a non-empty directory
        // fails the Claude primary on its settings path; a Codex fallback must
        // also not collide with a partial candidate-0 auth entry.
        for poison in [
            staging.join("candidate-0/provenant-settings.json/partial"),
            paths.provider_state.join("candidate-0/auth.json"),
        ] {
            fs::create_dir_all(poison.parent().ok_or("candidate parent")?)?;
            fs::write(&poison, b"partial setup")?;
        }
        // Bound to the registered account, as the host loader binds it.
        let token = provenant_providers::claude::refresh_for_account(
            &FixtureCredential,
            &provenant_providers::claude::Login {
                executable: &executable,
                home: &home,
                user: "fixture".as_ref(),
                config_dir: &home.join(".claude-fixture"),
            },
            &provenant_providers::identity::RegisteredAccount::new(
                "claude",
                &claude_key,
                home.join(".claude-fixture"),
            )?,
            &|| 1000,
            2000,
        )?;
        // Prepared below lanes.max_depth, so its children may launch; the
        // limit then drops to exercise refusal at dispatch time.
        fs::write(rig.state.join("config.toml"), config(2))?;
        let mut launch = provenant_cli::run::prepare_fixture(
            &registry,
            task,
            "fixture prompt",
            &paths,
            &executable,
            Some(&token),
        )
        .map_err(|e| format!("prepare {adapter} writer={writer}: {e}"))?;
        assert_eq!(launch.route.provider.as_deref(), Some(adapter));
        fs::write(rig.state.join("config.toml"), config(1))?;
        assert!(
            launch
                .environment
                .iter()
                .any(|(key, value)| key == provenant_run::LANE_DEPTH_ENV && value == "1")
        );
        let confinement = launch.confinement.as_ref().ok_or("confinement")?;
        assert_eq!(
            confinement.plan.permissions().writer_publishing(),
            publishing
        );
        let selected_git =
            fs::canonicalize(provenant_os::git::git_executable().ok_or("host Git")?)?;
        let scratch_git = confinement.plan.permissions().scratch.path.join("bin/git");
        let git_entry = fs::symlink_metadata(&scratch_git)?.file_type();
        let wrapped = writer || adapter == "claude" || publishing;
        assert_eq!(
            git_entry.is_file(),
            wrapped,
            "Git setup for {adapter} writer={writer} publishing={publishing}"
        );
        if !wrapped {
            assert_eq!(fs::canonicalize(&scratch_git)?, selected_git);
        }
        if wrapped {
            // Claude's env scrub and publishing both need a Git wrapper.
            let wrapper = fs::read_to_string(&scratch_git)?;
            assert!(wrapper.contains("GIT_CONFIG_GLOBAL=/dev/null"));
            let git = format!("'{}'", selected_git.display());
            assert!(
                wrapper.contains(&format!("exec {git}")),
                "Git exec for {adapter} writer={writer} publishing={publishing}"
            );
            assert_eq!(wrapper.contains(" -c 'include.path="), publishing);
        }
        let scratch = &confinement.plan.permissions().scratch.path;
        assert_eq!(scratch.join("publishing.gitconfig").exists(), publishing);
        assert_eq!(scratch.join("bin/gh").exists(), publishing);
        if publishing {
            let gh_wrapper = fs::read_to_string(scratch.join("bin/gh"))?;
            assert!(gh_wrapper.contains(&format!("exec '{}'", executable.display())));
        }
        assert!(
            launch
                .environment
                .iter()
                .any(|(key, value)| key == "PROVENANT_GIT" && value == selected_git.as_os_str())
        );
        assert!(
            confinement
                .profile
                .spec
                .process_executables
                .iter()
                .any(|grant| grant.path == selected_git && grant.scope == PathScope::Exact)
        );
        let permissions = confinement.plan.permissions();
        assert_eq!(permissions.scratch.path, staging.join("candidate-1"));

        if adapter == "codex" {
            assert!(
                fs::symlink_metadata(
                    permissions
                        .private_state
                        .as_ref()
                        .ok_or("private state")?
                        .path
                        .join("auth.json")
                )?
                .file_type()
                .is_symlink()
            );
        } else {
            let generated = permissions.scratch.path.join("provenant-mcp.json");
            let mcp: Value = serde_json::from_slice(&fs::read(generated)?)?;
            assert_eq!(
                mcp["mcpServers"]["fabric"]["env"]["PROVENANT_GIT"],
                selected_git.to_str().ok_or("Git path")?
            );
            assert_eq!(
                mcp["mcpServers"]["fabric"]["env"][provenant_run::LANE_DEPTH_ENV],
                "1"
            );
            let settings: Value = serde_json::from_slice(&fs::read(
                permissions.scratch.path.join("provenant-settings.json"),
            )?)?;
            assert!(
                settings["hooks"]
                    .as_object()
                    .is_some_and(|hooks| !hooks.is_empty())
            );
        }
        for grant in &confinement.profile.spec.state_writes {
            assert!(
                !grant.overlaps(&permissions.scratch),
                "scratch overlaps {grant:?}"
            );
            assert!(
                !grant.overlaps(permissions.private_state.as_ref().ok_or("private state")?),
                "private state overlaps {grant:?}"
            );
            for denied in [
                attempt.join("quarantine"),
                attempt.join("quarantine-result.json"),
                attempt.join("custody.lock"),
                roots
                    .state
                    .as_path()
                    .join("runs/other/attempts/other/custody.lock"),
            ] {
                assert!(
                    !(grant.path == denied
                        || (grant.scope == PathScope::Subtree && denied.starts_with(&grant.path))),
                    "custody writable through {grant:?}"
                );
            }
        }
        let frames = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":999,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"fixture\",\"version\":\"1\"}}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"fabric_run\",\"arguments\":{\"op\":\"submit\",\"operation_id\":\"nested-depth\",\"tasks\":[{\"prompt\":\"read only\",\"route\":{\"kind\":\"model\",\"provider\":\"codex\",\"model\":\"gpt-6-luna\"},\"timeout_seconds\":1}],\"concurrency\":1,\"wait_seconds\":1}}}\n"
        );
        let mut options = provenant_os::exec::ExecOptions::new(
            &launch.argv,
            &launch.cwd,
            Duration::from_secs(10),
        );
        options.env = Some(&launch.environment);
        options.stdin = Some(frames.as_bytes());
        options.stdout = provenant_os::exec::StreamPolicy::Capture { limit: 64 * 1024 };
        options.stderr = provenant_os::exec::StreamPolicy::Capture { limit: 64 * 1024 };
        let output = provenant_os::exec::SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let replies = output
            .stdout
            .bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let result = &replies
            .iter()
            .find(|reply| reply["id"] == 1)
            .ok_or("MCP reply")?["result"];
        assert_eq!(mcp_error_code(result), "lane_depth", "{result}");
        assert!(!roots.state.as_path().join("tmp/results").exists());
        assert!(
            rig.state
                .join("children")
                .join(paths.attempt_id.to_string())
                .join("tmp/results")
                .is_dir()
        );
        assert!(!worktree.join("provenant-mcp.json").exists());
        assert!(!worktree.join("provenant-settings.json").exists());
        assert_eq!(
            git(
                &rig,
                &[
                    "-C",
                    worktree.to_str().ok_or("worktree path")?,
                    "status",
                    "--porcelain"
                ]
            )?,
            ""
        );
        // Raise the host limit only for a second call. It must create records
        // in the configured child store without launching in the lane.
        if adapter == "codex" && !writer {
            fs::write(rig.state.join("config.toml"), config(2))?;
            let mut options = provenant_os::exec::ExecOptions::new(
                &launch.argv,
                &launch.cwd,
                Duration::from_secs(10),
            );
            options.env = Some(&launch.environment);
            options.stdin = Some(frames.as_bytes());
            let output = provenant_os::exec::SystemExecutor::new(1)?.execute(options)?;
            assert_eq!(
                code(&output),
                0,
                "{}",
                String::from_utf8_lossy(&output.stderr.bytes)
            );
            assert!(
                rig.state
                    .join("children")
                    .join(paths.attempt_id.to_string())
                    .join("run.sqlite3")
                    .exists()
            );
            let mut inputs = provenant_core::RootInputs::new(home.clone(), rig.project.clone());
            inputs.state_dir = Some(
                rig.state
                    .join("children")
                    .join(paths.attempt_id.to_string()),
            );
            let queued_context = provenant_run::ExecutionContext {
                state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
                ..context.clone()
            };
            let queued = provenant_run::RunStore::open(&queued_context, &call)?;
            assert_eq!(
                queued.queued_runs(&call)?.len(),
                1,
                "lane launched its own child"
            );
            // This depth probe must not consume startup spacing when the
            // confined fixture raises the host limit again.
            for run in queued.queued_runs(&call)? {
                queued.refuse_queued(run, "depth probe only", &call)?;
            }
            fs::write(rig.state.join("config.toml"), config(1))?;
        }
        let parent_context = provenant_run::HolderContext {
            schema_version: 1,
            roots: provenant_run::RootSeed {
                home: home.clone(),
                checkout: rig.project.clone(),
                state_root: rig.state.clone(),
            },
            project_id: context.project_id,
            actor_id: context.actor_id,
            run_id: prepared.run_id,
            attempt_id: paths.attempt_id,
            host_delivery: None,
            launch: Some(provenant_run::HostLaunchContext {
                configuration_root: rig.state.clone(),
                cwd: launch.cwd.clone(),
                executables: [("codex".into(), executable.clone())].into(),
                executable_search_path: Vec::new(),
                git: selected_git.clone(),
                codex_home: None,
                cargo_build_jobs: None,
                fixture_state_write: true,
            }),
            sealed: Some(provenant_run::SealedTask {
                request_digest: prepared.request.digest()?,
                task_id: task.task_id,
                effective_network: task.access.network(task.writer.is_some()),
            }),
        };
        verify_lane_profile(
            &mut launch,
            &attempt,
            roots.state.as_path(),
            frames.as_bytes(),
            &config,
            &parent_context,
        )
        .map_err(|e| format!("confined {adapter} writer={writer}: {e}"))?;
    }
    cleanup.disarm();
    Ok(())
}

/// Fake provider forwards no provider environment to its MCP child, only a
/// baseline PATH as real providers do.
pub(crate) fn lane_provider() -> Result {
    if std::env::var(provenant_run::LANE_DEPTH_ENV).as_deref() == Ok("2") {
        let mut prompt = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut prompt)?;
        let state = PathBuf::from(std::env::var_os("CODEX_HOME").ok_or("child CODEX_HOME")?);
        fs::write(
            state.join("fake-child-launched"),
            b"confined child executed",
        )?;
        writeln!(
            std::io::stdout(),
            "{{\"type\":\"thread.started\",\"thread_id\":\"fixture-child\"}}"
        )?;
        if prompt.contains("wait for host cancellation") {
            thread::park_timeout(Duration::from_secs(5));
        }
        return Ok(());
    }
    use provenant_os::exec::ExecOptions;
    use provenant_os::exec::Executor;
    use provenant_os::exec::StreamPolicy;
    use provenant_os::exec::SystemExecutor;
    let mut entry = serde_json::Map::new();
    for arg in std::env::args().filter(|arg| arg.starts_with("mcp_servers.fabric.")) {
        let value: Value = toml_edit::de::from_str(&arg)?;
        entry.extend(
            value["mcp_servers"]["fabric"]
                .as_object()
                .ok_or("MCP config")?
                .clone(),
        );
    }
    if entry.is_empty() {
        let args: Vec<_> = std::env::args_os().collect();
        let index = args
            .iter()
            .position(|arg| arg == "--mcp-config")
            .ok_or("MCP config flag")?;
        let config: Value =
            serde_json::from_slice(&fs::read(args.get(index + 1).ok_or("MCP config path")?)?)?;
        entry = config["mcpServers"]["fabric"]
            .as_object()
            .ok_or("MCP entry")?
            .clone();
    }
    let mut argv = vec![OsString::from(
        entry["command"].as_str().ok_or("MCP command")?,
    )];
    argv.extend(
        entry["args"]
            .as_array()
            .ok_or("MCP args")?
            .iter()
            .map(|arg| arg.as_str().map(OsString::from).ok_or("MCP arg"))
            .collect::<std::result::Result<Vec<_>, _>>()?,
    );
    let env = entry["env"]
        .as_object()
        .ok_or("MCP env")?
        .iter()
        .map(|(key, value)| {
            Ok((
                OsString::from(key),
                OsString::from(value.as_str().ok_or("MCP env value")?),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    // A real provider hands its MCP child a baseline PATH. Mirror that with
    // this fake's own directory first, so the nested dispatch resolves it.
    let mut env = env;
    if env.iter().all(|(key, _)| key != "PATH") {
        let mut directories = vec![
            std::env::current_exe()?
                .parent()
                .ok_or("provider bin")?
                .to_path_buf(),
        ];
        directories.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        env.push(("PATH".into(), std::env::join_paths(directories)?));
    }
    let mut input = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut input)?;
    let cwd = std::env::current_dir()?;
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(8));
    options.env = Some(&env);
    options.stdin = Some(&input);
    options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stderr = StreamPolicy::Capture { limit: 64 * 1024 };
    let output = SystemExecutor::new(1)?
        .execute(options)
        .map_err(|e| format!("MCP spawn: {e:?}"))?;
    std::io::stdout().write_all(&output.stdout.bytes)?;
    std::io::stderr().write_all(&output.stderr.bytes)?;
    if code(&output) != 0 {
        return Err("configured MCP child failed".into());
    }
    let stripped = env
        .iter()
        .filter(|(key, _)| {
            key != provenant_run::LANE_DEPTH_ENV && key != provenant_run::PARENT_ATTEMPT_ENV
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(8));
    options.env = Some(&stripped);
    options.stdin = Some(&input);
    let stripped_output = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(
        code(&stripped_output),
        0,
        "{}",
        String::from_utf8_lossy(&stripped_output.stderr.bytes)
    );
    let reply = stripped_output
        .stdout
        .bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        reply
            .iter()
            .find(|reply| reply["id"] == 1)
            .map(|reply| mcp_error_code(&reply["result"]))
            .ok_or("stripped MCP reply")?,
        "invalid_input"
    );
    // Exercise the same publisher used by CLI overflow under confinement.
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME")?);
    let assets = PathBuf::from(std::env::var_os("PROVENANT_ROOT").ok_or("assets")?);
    let mut inputs = provenant_core::RootInputs::new(home, assets);
    inputs.state_dir = Some(PathBuf::from(entry["args"][1].as_str().ok_or("state arg")?));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    provenant_cli::fabric::publish(
        &provenant_records::CommandResult::new(
            "fixture",
            provenant_records::Outcome::Ok,
            "overflow",
        ),
        &roots,
    )?;
    Ok(())
}

fn verify_lane_profile(
    launch: &mut provenant_run::PreparedLaunch,
    attempt: &Path,
    state: &Path,
    frames: &[u8],
    config: &dyn Fn(u8) -> String,
    parent: &provenant_run::HolderContext,
) -> Result {
    use provenant_confinement::permissions::{
        AccessMode, AnchoredDirectory, Anchors, BackendContext, FixtureOperation, FixtureOutcome,
        prepare_writer_directories, required_anchors, system_backend,
    };
    use provenant_os::exec::ExecOptions;
    use provenant_os::exec::StreamPolicy;
    use provenant_os::exec::SystemExecutor;

    let executor = SystemExecutor::new(1)?;
    let cancel = provenant_core::CancelToken::new();
    let backend = match system_backend(&BackendContext {
        executor: &executor,
        cancel: &cancel,
        #[cfg(target_os = "macos")]
        env: &[],
    }) {
        Ok(backend) => backend,
        Err(error) if std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_none() => {
            writeln!(std::io::stderr(), "lane profile execution skipped: {error}")?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let confinement = launch.confinement.as_mut().ok_or("confinement")?;
    // Synthetic fake-provider evidence exercises the compiler only. This does
    // not activate Codex or replace its missing live state-write recording.
    let profile = &mut confinement.profile;
    profile
        .observations
        .insert(FixtureOperation::StateWrite, FixtureOutcome::Allowed);
    let writer = confinement
        .writer
        .as_ref()
        .map(|w| (&w.worktree, &w.branch));
    // Run creates the writer's ref parents and lane cache under custody.
    if let (Some((worktree, branch)), AccessMode::WorktreeWrite(authority)) =
        (writer, &confinement.plan.permissions().mode)
    {
        prepare_writer_directories(worktree, branch, authority, &confinement.home)?;
    }
    if let Some(writer) = confinement.writer.as_ref() {
        writer.prepare_private_git_for_fixture(
            &provenant_os::fs::Directory::open_anchor(attempt)?,
            &confinement.plan.permissions().scratch.path,
        )?;
    }
    let directories = required_anchors(confinement.plan.permissions(), profile, writer)
        .iter()
        .map(AnchoredDirectory::open_canonical)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        home: &confinement.home,
        directories: &directories,
        worktree: writer,
    };
    let compiled = confinement
        .plan
        .compile(backend.as_ref(), profile, &anchors)?;
    // Start the same host holder used by internal supervise. This manually
    // compiled parent has no guardian of its own, so its fixture holder has no
    // admission or terminal work; all child handoff/custody remains real.
    struct ParentFixture;
    impl provenant_supervisor::Holder for ParentFixture {
        fn admit(
            &self,
            _: &[u8],
            _: &provenant_os::fs::Directory,
        ) -> provenant_supervisor::custody::Result<()> {
            Ok(())
        }
        fn terminal(
            &self,
            _: &[u8],
            _: &provenant_os::fs::Directory,
            _: &provenant_supervisor::custody::ExitObservation,
            _: &provenant_supervisor::custody::Recovery,
        ) -> provenant_supervisor::custody::Result<()> {
            Ok(())
        }
    }
    use provenant_supervisor::Holder;
    let host = provenant_cli::run::HostHolder::new(&ParentFixture);
    host.admit(
        &parent.encode()?,
        &provenant_os::fs::Directory::open_anchor(attempt)?,
    )?;
    let shell_writer = launch
        .argv
        .iter()
        .any(|arg| arg == "Read,Grep,Glob,Edit,Write,Bash");
    // Native shell proof, separate from live provider/Bash/token-scrub proof.
    if cfg!(target_os = "macos") && shell_writer {
        let scratch = &confinement.plan.permissions().scratch.path;
        let project = scratch.join("cargo-probe");
        fs::create_dir_all(project.join("src"))?;
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = 'lane-probe'\nversion = '0.1.0'\nedition = '2024'\n",
        )?;
        fs::write(
            project.join("src/lib.rs"),
            "#[test] fn writer_can_test() { assert_eq!(2 + 2, 4); }\n",
        )?;
        let argv = ["/bin/sh", "-c", concat!(
            "if /usr/bin/security help >/dev/null 2>&1; then exit 11; fi; ",
            "if /bin/cat \"$HOME/Library/Keychains/login.keychain-db\" >/dev/null 2>&1; then exit 12; fi; ",
            "for f in credentials.toml credentials; do ",
            "if /bin/cat \"$HOME/.cargo/$f\" >/dev/null 2>&1; then exit 13; fi; done; ",
            "if GIT_CONFIG_GLOBAL=/nonexistent/gitconfig git config --global --list >/dev/null 2>&1; then :; else exit 14; fi; ",
            "git -c user.name=lane -c user.email=lane@example.test commit -q --allow-empty -m probe && ",
            "test \"$(command -v git)\" = \"$TMPDIR/bin/git\" && ",
            "git --version && git status --porcelain && ",
            "cargo test --lib --offline --manifest-path \"$TMPDIR/cargo-probe/Cargo.toml\""
        )].map(OsString::from);
        let command = compiled.command(&argv, &provenant_core::AttemptId::from_bytes([0; 16]))?;
        let mut options = ExecOptions::new(&command, &launch.cwd, Duration::from_secs(30));
        options.env = Some(&launch.environment);
        let output = executor.execute(options)?;
        assert_eq!(
            code(&output),
            0,
            "shell probe: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        assert!(String::from_utf8_lossy(&output.stdout.bytes).contains("1 passed"));
    }
    // Start the configured MCP inside the compiled profile too: an
    // unconfined fake provider cannot detect ungranted startup writes.
    let mcp_argv = compiled.command(
        &launch.argv,
        &provenant_core::AttemptId::from_bytes([0; 16]),
    )?;
    let mut options = ExecOptions::new(&mcp_argv, &launch.cwd, Duration::from_secs(10));
    options.env = Some(&launch.environment);
    options.stdin = Some(frames);
    let output = executor.execute(options)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let replies = output
        .stdout
        .bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        replies
            .iter()
            .find(|reply| reply["id"] == 1)
            .map(|reply| mcp_error_code(&reply["result"]))
            .ok_or("confined MCP reply")?,
        "lane_depth"
    );
    // Admission below the limit must also open the isolated child store under
    // the profile, rather than only exercising the early depth refusal.
    let child = launch
        .environment
        .iter()
        .find(|(key, _)| key == provenant_core::RUN_STATE_DIRECTORY_ENV)
        .map(|(_, value)| PathBuf::from(value))
        .ok_or("child store environment")?;
    let run_count = || -> Result<usize> {
        match fs::read_dir(child.join("runs")) {
            Ok(entries) => Ok(entries.collect::<std::io::Result<Vec<_>>>()?.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error.into()),
        }
    };
    // Startup spacing is host-wide: the child would otherwise wait out the
    // default 20 s after the parent's own start, past this test's deadline.
    // Clear only the last-start time; the lease counts stay real.
    let ledger = state.join("admission.sqlite3");
    if ledger.exists() {
        let db = rusqlite::Connection::open(&ledger)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute("DELETE FROM spacing", [])?;
    }
    fs::write(state.join("config.toml"), config(2))?;
    let mut inputs =
        provenant_core::RootInputs::new(confinement.home.clone(), parent.roots.checkout.clone());
    inputs.state_dir = Some(child.clone());
    let child_context = provenant_run::ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: parent.project_id,
        actor_id: parent.actor_id,
        read_prompts: true,
    };
    let clock = provenant_os::clock::SystemClock::new();
    let call = writer_call(&clock, &cancel)?;
    // Admission is not launch: the host decides from its sealed parent task.
    let admit = |frames: &str| -> Result<provenant_core::AttemptId> {
        let before = run_count()?;
        let mut options = ExecOptions::new(&mcp_argv, &launch.cwd, Duration::from_secs(10));
        options.env = Some(&launch.environment);
        options.stdin = Some(frames.as_bytes());
        let nested = executor.execute(options)?;
        assert_eq!(
            code(&nested),
            0,
            "{}",
            String::from_utf8_lossy(&nested.stderr.bytes)
        );
        assert!(
            run_count()? > before,
            "confined child admission created no run"
        );
        // The child must be admitted as a target, not merely leave a record.
        let accepted = nested
            .stdout
            .bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let targets = &accepted
            .iter()
            .find(|reply| reply["id"] == 1)
            .ok_or("confined child reply")?["result"]["structuredContent"]["receipt"]["targets"];
        assert_eq!(
            targets.as_array().map(Vec::len),
            Some(1),
            "confined child was not admitted: {targets}"
        );
        Ok(provenant_core::AttemptId::parse(
            targets[0]["attempt"].as_str().ok_or("child attempt id")?,
        )?)
    };
    let nested_frames =
        String::from_utf8(frames.to_vec())?.replace("nested-depth", "confined-child");
    if confinement.writer.is_some() {
        // A writer lane cannot hand a writer child any registered worktree:
        // the host refuses the proposal before any provider launches.
        for (name, worktree) in [
            ("confined-writer", ".worktrees/nested-child"),
            ("confined-parent-writer", ".worktrees/writer"),
        ] {
            let proposal = admit(&nested_frames.replace("confined-child", name).replace(
                "\"timeout_seconds\":1",
                &format!(
                    "\"timeout_seconds\":1,\"access\":\"worktree_write\",\"worktree\":\"{worktree}\""
                ),
            ))?;
            let store = provenant_run::RunStore::open(&child_context, &call)?;
            let refused = await_terminal(&store, proposal, &call)?;
            assert_terminal_outcome(&refused, provenant_run::Outcome::Rejected)?;
            assert_ne!(refused.provider_launched, Some(true), "{worktree}");
            let provenant_run::AttemptState::Terminal { reason, .. } = &refused.state else {
                return Err("writer proposal was not terminal".into());
            };
            assert!(
                reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("a lane cannot propose a writer child")),
                "{worktree}: {reason:?}"
            );
        }
    }
    let cancel_child =
        launch.route.provider.as_deref() == Some("codex") && confinement.writer.is_none();
    let nested_frames = if cancel_child {
        nested_frames
            .replace("read only", "wait for host cancellation")
            .replace("\"timeout_seconds\":1", "\"timeout_seconds\":10")
    } else {
        nested_frames
    };
    // The host must reach the confined fake child, including when a Claude
    // parent cannot read Codex credentials.
    let attempt_id = admit(&nested_frames)?;
    let store = provenant_run::RunStore::open(&child_context, &call)?;
    let forged = provenant_core::ActorId::from_bytes([66; 16]);
    if cancel_child {
        let run = store.record(attempt_id, &call)?.run_id;
        wait_for_path(
            &child.join(format!(
                "runs/{run}/attempts/{attempt_id}/provider-state/candidate-0/fake-child-launched"
            )),
            Duration::from_secs(8),
            "confined child launch",
        )?;
        // The lane can write its child's store and attempt directory. It
        // marks the child finished, renames the requester in its launch
        // summary and plants a stop the guardian ignores. The host must still
        // stop the child, from the scope and requester it held at hand-off.
        let attempt = child.join(format!("runs/{run}/attempts/{attempt_id}"));
        let mut finished = store.record(attempt_id, &call)?;
        let original = finished.clone();
        let mut sibling = finished.clone();
        finished.state = provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Rejected,
            exit_code: None,
            finished_at: finished.queued_at,
            reason: Some("rejected, says the lane".into()),
        };
        finished.started_at = None;
        let mut result = serde_json::to_value(&finished.prompt)?;
        result["path"]["path"] = format!("attempts/{attempt_id}/result.md").into();
        finished.result = Some(serde_json::from_value(result)?);
        rusqlite::Connection::open(child.join("run.sqlite3"))?.execute(
            "UPDATE attempts SET record=?1, terminal=1 WHERE id=?2",
            rusqlite::params![
                provenant_records::CanonicalV1::bytes(&finished)?.as_bytes(),
                attempt_id.to_string()
            ],
        )?;
        let path = attempt.join("launch.json");
        let mut summary: Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(summary["requester"], serde_json::to_value(parent.actor_id)?);
        summary["requester"] = serde_json::to_value(forged)?;
        fs::remove_file(&path)?;
        fs::write(&path, serde_json::to_vec(&summary)?)?;
        let scope = &summary["scope"];
        let planted = serde_json::json!({
            "schema_version": 1,
            "scope": scope,
            "request_id": format!("{run}.{attempt_id}.{}.cancel", scope["generation"]),
            "requester": forged,
            "reason": "explicit_cancellation",
        });
        fs::write(attempt.join("cancel.json"), serde_json::to_vec(&planted)?)?;
        // Scan reads of lane-written rows once ended the host's launcher
        // before its stop. First an undecodable attempt row, then a queued
        // sibling whose run the lane renames, so the owner check refuses it.
        let db = rusqlite::Connection::open(child.join("run.sqlite3"))?;
        let owner: Vec<u8> = db.query_row(
            "SELECT record FROM runs WHERE id=?1",
            [run.to_string()],
            |row| row.get(0),
        )?;
        let (project, task): (String, String) = db.query_row(
            "SELECT project, task FROM attempts WHERE id=?1",
            [attempt_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let garbage = provenant_core::AttemptId::from_bytes([67; 16]);
        db.execute(
            "INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) VALUES (?1,?2,?3,?4,2,1,0,x'00')",
            rusqlite::params![garbage.to_string(), run.to_string(), project, task],
        )?;
        // Several of the launcher's 100 ms scans see it.
        pause(Duration::from_millis(500));
        db.execute("DELETE FROM attempts WHERE id=?1", [garbage.to_string()])?;
        sibling.attempt_id = provenant_core::AttemptId::from_bytes([68; 16]);
        sibling.state = provenant_run::AttemptState::Queued;
        sibling.generation = 0;
        sibling.attempt_number = 2;
        sibling.revision = 1;
        sibling.started_at = None;
        let mut prompt = serde_json::to_value(&sibling.prompt)?;
        prompt["path"]["path"] = prompt["path"]["path"]
            .as_str()
            .ok_or("prompt path")?
            .replace(&attempt_id.to_string(), &sibling.attempt_id.to_string())
            .into();
        sibling.prompt = serde_json::from_value(prompt)?;
        let mut renamed = store.run(run, &call)?;
        renamed.actor_id = forged;
        db.execute_batch("BEGIN")?;
        db.execute(
            "INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) VALUES (?1,?2,?3,?4,2,1,0,?5)",
            rusqlite::params![
                sibling.attempt_id.to_string(),
                run.to_string(),
                project,
                task,
                provenant_records::CanonicalV1::bytes(&sibling)?.as_bytes()
            ],
        )?;
        db.execute(
            "UPDATE runs SET record=?1 WHERE id=?2",
            rusqlite::params![
                provenant_records::CanonicalV1::bytes(&renamed)?.as_bytes(),
                run.to_string()
            ],
        )?;
        db.execute_batch("COMMIT")?;
        // Several of the launcher's 100 ms scans see it.
        pause(Duration::from_millis(500));
        // The launcher reports the refused read, after it stops the child.
        let reported = host.provider_exited(
            &parent.encode()?,
            Some(provenant_supervisor::machine::StopReason::ExplicitCancellation),
        );
        // The forged terminal record stands, so the guardian's own exit is
        // the evidence. Unstopped, the child would run to its wall deadline.
        wait_for_path(
            &attempt.join("process-exit.json"),
            Duration::from_secs(8),
            "cancelled child process exit",
        )?;
        let exit: Value = serde_json::from_slice(&fs::read(attempt.join("process-exit.json"))?)?;
        assert_eq!(exit["stop_reason"], "explicit_cancellation", "{exit}");
        let Err(provenant_supervisor::custody::CustodyError::Io(reported)) = reported else {
            return Err(format!("launcher hid the refused read: {reported:?}").into());
        };
        assert!(
            reported.to_string().contains("names another owner"),
            "{reported}"
        );
        db.execute(
            "DELETE FROM attempts WHERE id=?1",
            [sibling.attempt_id.to_string()],
        )?;
        db.execute(
            "UPDATE runs SET record=?1 WHERE id=?2",
            rusqlite::params![owner, run.to_string()],
        )?;
        // The forged terminal row bypassed Run's terminal transaction, leaving
        // admission held. Restore the running row so the guardian can settle
        // it before the next profile starts. Restore the trigger in the same
        // transaction, before the row is visible again.
        let trigger: String = db.query_row(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='immutable_terminal_update'",
            [],
            |row| row.get(0),
        )?;
        db.execute_batch("BEGIN; DROP TRIGGER immutable_terminal_update;")?;
        db.execute(
            "UPDATE attempts SET record=?1, terminal=0 WHERE id=?2",
            rusqlite::params![
                provenant_records::CanonicalV1::bytes(&original)?.as_bytes(),
                attempt_id.to_string()
            ],
        )?;
        db.execute_batch(&trigger)?;
        db.execute_batch("COMMIT")?;
    }
    let record = await_terminal(&store, attempt_id, &call)?;
    let provenant_run::AttemptState::Terminal { reason, .. } = &record.state else {
        return Err("child attempt was not terminal".into());
    };
    let reason = reason.as_deref().unwrap_or_default();
    // Host setup links the registered credential. The parent's profile has
    // no cross-provider credential read.
    assert!(!reason.contains("setup failed"), "{reason}");
    if launch.route.provider.as_deref() == Some("codex") {
        let candidates = child.join(format!(
            "runs/{}/attempts/{attempt_id}/provider-state",
            record.run_id
        ));
        let linked = fs::read_dir(&candidates)?
            .map(|entry| Ok(fs::read_link(entry?.path().join("auth.json")).ok()))
            .collect::<Result<Vec<_>>>()?;
        assert_eq!(
            linked.into_iter().flatten().collect::<Vec<_>>(),
            vec![confinement.home.join(".codex/auth.json")],
            "{reason}"
        );
    }
    if !cancel_child {
        assert_eq!(record.provider_launched, Some(true), "{reason}");
    }
    // The lane's operation keeps its own actor. The host launches for, and
    // records as owner, the requester sealed in the parent's holder context.
    let actor: String = rusqlite::Connection::open(child.join("run.sqlite3"))?.query_row(
        "SELECT actor FROM operations WHERE run=?1",
        [record.run_id.to_string()],
        |row| row.get(0),
    )?;
    assert_ne!(actor, parent.actor_id.to_string());
    assert_eq!(store.run(record.run_id, &call)?.actor_id, parent.actor_id);
    assert_eq!(record.verified_owner, Some(parent.actor_id));
    if !cancel_child {
        let summary: Value = serde_json::from_slice(&fs::read(child.join(format!(
            "runs/{}/attempts/{attempt_id}/launch.json",
            record.run_id
        )))?)?;
        assert_eq!(summary["requester"], serde_json::to_value(parent.actor_id)?);
    }
    let provider_state = child.join(format!(
        "runs/{}/attempts/{attempt_id}/provider-state",
        record.run_id
    ));
    assert!(
        provider_state
            .join("candidate-0/fake-child-launched")
            .exists(),
        "{reason}"
    );
    host.provider_exited(&parent.encode()?, None)?;
    fs::write(state.join("config.toml"), config(1))?;
    // The child store's git-home is the in-lane fabric context's own, not the
    // host's: its preparation keeps its git home under the host root.
    assert!(child.join("tmp/git-home").is_dir());
    // Host probes leave no directory or lease behind once the child launched.
    let probes: Vec<_> = fs::read_dir(state)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("probe-"))
        .collect();
    assert!(probes.is_empty(), "{probes:?}");
    let denied = [
        attempt.join("custody.lock"),
        attempt.join("reserve"),
        attempt.join("quarantine/probe"),
        attempt.join("quarantine-result.json"),
        attempt.join("process.json"),
        state.join("runs/other/attempts/other/process.json"),
    ];
    for path in &denied {
        fs::create_dir_all(path.parent().ok_or("custody parent")?)?;
        fs::write(path, b"sealed custody")?;
    }
    let marker = confinement
        .plan
        .permissions()
        .scratch
        .path
        .join("allowed-write");
    let mut argv: Vec<OsString> = vec!["/bin/sh".into(), "-c".into(),
        "printf allowed > \"$1\" || exit 10; shift; for target in \"$@\"; do if printf changed > \"$target\"; then exit 20; fi; done".into(), "probe".into(), marker.as_os_str().to_owned()];
    argv.extend(denied.iter().map(|path| path.as_os_str().to_owned()));
    argv = compiled.command(&argv, &provenant_core::AttemptId::from_bytes([0; 16]))?;
    let mut options = ExecOptions::new(&argv, &launch.cwd, Duration::from_secs(5));
    options.env = Some(&launch.environment);
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    let output = executor.execute(options)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert_eq!(fs::read(marker)?, b"allowed");
    for path in denied {
        assert_eq!(fs::read(path)?, b"sealed custody");
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

/// Identity and submit report errors in text; other tools keep structured codes.
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

fn gc_fetches_default_and_removes_only_clean_merged_worktrees() -> Result {
    use provenant_os::fs::Directory;
    use provenant_os::fs::LockMode;
    use std::ffi::OsStr;
    let rig = repository()?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\n**/target/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["commit", "-qm", "ignore output"])?;
    let remote = rig.root.join("upstream.git");
    git(
        &rig,
        &[
            "init",
            "--bare",
            "--initial-branch=trunk",
            remote.to_str().ok_or("remote")?,
        ],
    )?;
    git(
        &rig,
        &["remote", "add", "origin", remote.to_str().ok_or("remote")?],
    )?;
    let stale_main = git(&rig, &["rev-parse", "main"])?;
    ok(&rig, &["worktree", "create", "fresh-old", "--json"])?;
    let merged = branch_commit(&rig, "merged")?;
    let merged_tip = git(&rig, &["rev-parse", "merged"])?;
    let tree = git(&rig, &["rev-parse", "merged^{tree}"])?;
    let tip = git(
        &rig,
        &[
            "commit-tree",
            &tree,
            "-p",
            &stale_main,
            "-p",
            &merged_tip,
            "-m",
            "merge lane",
        ],
    )?;
    git(
        &rig,
        &["push", "origin", &format!("{tip}:refs/heads/trunk")],
    )?;
    ok(
        &rig,
        &["worktree", "create", "fresh-tip", "--base", &tip, "--json"],
    )?;
    for name in [
        "named",
        "explicit",
        "dirty",
        "live",
        "queued",
        "unrecorded",
        "locked",
    ] {
        ok(
            &rig,
            &["worktree", "create", name, "--base", "merged", "--json"],
        )?;
    }
    let cache = prepare_lane_cache(&rig, &merged, "merged")?;
    git(&rig, &["worktree", "lock", ".worktrees/locked"])?;
    fs::write(rig.project.join(".worktrees/dirty/untracked"), "keep")?;
    let live = rig.project.join(".worktrees/live");
    let state_project = fs::read_dir(rig.state.join("worktrees"))?
        .next()
        .ok_or("project state")??
        .path();
    let state = state_project.join(provenant_confinement::worktrees::state_dir_name(&live));
    let state = Directory::open_anchor(&state)?;
    let _guard = state.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)?;
    let unmerged = branch_commit(&rig, "unmerged")?;
    let (_, context) = writer_context(&rig)?;
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let merged_writer =
        provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &merged)?;
    let ended = writer_run(merged_writer, "gc-merged", 120, "finished")?;
    store.create(&ended, &call)?;
    store.cancel(ended.attempts[0].attempt_id, "finished", &call)?;
    fs::create_dir(merged.join("target"))?;
    fs::write(merged.join("target/artifact"), "old build")?;
    age_gc_output(&merged.join("target"))?;
    let output_config = format!(
        "[gc.projects.'{}']\nregenerable=['target']\n",
        context.project_id
    );
    fs::write(
        rig.state.join("config.toml"),
        format!("schema_version=1\n[gc]\nfree_floor='1B'\n{output_config}"),
    )?;
    let queued_path = rig.project.join(".worktrees/queued");
    let queued_writer =
        provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &queued_path)?;
    let queued = writer_run(queued_writer, "gc-queued", 121, "waiting")?;
    store.create(&queued, &call)?;
    fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\n{output_config}foreign_worktrees='merged-clean'\n"
        ),
    )?;
    let queued_preview = ok(&rig, &["gc", "--dry-run", "--json"])?;
    assert!(
        queued_preview["data"]["worktree_cleanup"][0]["worktrees"]["retained"]
            .as_array()
            .ok_or("retained")?
            .iter()
            .any(|entry| entry["branch"] == "queued" && entry["reason"] == "unknown_writer")
    );
    assert!(queued_path.is_dir());
    fs::write(
        rig.state.join("config.toml"),
        format!("schema_version=1\n[gc]\nfree_floor='1B'\n{output_config}"),
    )?;
    let explicit = ok(
        &rig,
        &[
            "worktree",
            "remove",
            "explicit",
            "--integration",
            "main",
            "--json",
        ],
    )?;
    assert_eq!(explicit["data"]["retained"][0]["reason"], "not_merged");
    assert!(has_branch(&rig, "explicit")?);
    let named = ok(&rig, &["worktree", "remove", "named", "--json"])?;
    assert_eq!(named["data"]["pruned_refs"], serde_json::json!(["named"]));
    assert_eq!(git(&rig, &["rev-parse", "main"])?, stale_main);
    assert_eq!(git(&rig, &["rev-parse", "origin/trunk"])?, tip);
    // One broken registration must not stop the remaining sweep.
    git(
        &rig,
        &["worktree", "add", "-qb", "broken", ".worktrees/broken"],
    )?;
    fs::remove_dir_all(rig.project.join(".worktrees/broken"))?;
    let dry = ok(&rig, &["gc", "--dry-run", "--json"])?;
    assert_eq!(dry["data"]["dry_run"], true);
    assert!(merged.is_dir());
    assert!(cache.is_dir());
    assert!(dry["data"]["worktree_cleanup"][0]["dry_run"] == true);
    assert!(
        dry["data"]["worktree_cleanup"][0]["output"]
            .as_array()
            .ok_or("output")?
            .iter()
            .any(|entry| entry["reason"] == "would_remove" && entry["removed"] == false)
    );
    assert!(
        dry["data"]["worktree_cleanup"][0]["worktrees"]["pruned_refs"]
            .as_array()
            .ok_or("refs")?
            .contains(&serde_json::json!("merged"))
    );
    // Alert failure must not block reclamation, including on a full disk.
    fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\nalert_below='18446744073709551615B'\n{output_config}"
        ),
    )?;
    fs::create_dir(rig.state.join("gc"))?;
    fs::set_permissions(
        rig.state.join("gc"),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    fs::create_dir(rig.state.join("gc/alert.lock"))?;
    let collected = ok(&rig, &["gc", "--scheduled", "--apply", "--json"])?;
    assert!(
        collected["data"]["alerts"][0]
            .as_str()
            .ok_or("alert failure")?
            .contains("disk alert failed")
    );
    assert!(
        collected["data"]["worktree_cleanup"][0]["worktrees"]["pruned_refs"]
            .as_array()
            .ok_or("refs")?
            .contains(&serde_json::json!("merged"))
    );
    assert!(!merged.exists());
    assert!(!cache.exists());
    assert!(!has_branch(&rig, "merged")?);
    assert!(rig.project.join(".worktrees/dirty").is_dir());
    assert!(live.is_dir());
    assert!(unmerged.is_dir());
    assert!(queued_path.is_dir());
    for name in ["fresh-old", "fresh-tip", "unrecorded"] {
        assert!(
            rig.project.join(".worktrees").join(name).is_dir(),
            "{name} was swept"
        );
        assert!(has_branch(&rig, name)?);
    }
    for (branch, reason) in [
        ("fresh-old", "no_unique_commit"),
        ("fresh-tip", "at_integration_tip"),
        ("locked", "locked_or_prunable"),
    ] {
        let retained = collected["data"]["worktree_cleanup"][0]["worktrees"]["retained"]
            .as_array()
            .ok_or("retained")?;
        assert!(
            retained
                .iter()
                .any(|entry| entry["branch"] == branch && entry["reason"] == reason),
            "expected {branch} with {reason}; got {retained:?}"
        );
    }
    assert!(
        !collected["data"]["worktree_cleanup"][0]["worktrees"]["retained"]
            .as_array()
            .ok_or("retained")?
            .iter()
            .any(|entry| entry["branch"] == "main")
    );
    assert!(has_branch(&rig, "unmerged")?);
    assert_eq!(git(&rig, &["rev-parse", "main"])?, stale_main);
    store.cancel(queued.attempts[0].attempt_id, "queue ended", &call)?;
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(!queued_path.exists());
    Ok(())
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

fn gc_output_requires_terminal_lanes_and_respects_keep_recent_dirty_and_links() -> Result {
    let rig = repository()?;
    fs::write(
        rig.project.join(".gitignore"),
        ".worktrees/\n**/target/\n**/node_modules\n",
    )?;
    fs::create_dir_all(rig.project.join("tracked/target"))?;
    fs::write(rig.project.join("tracked/target/keep"), "tracked")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["add", "-f", "tracked/target/keep"])?;
    git(&rig, &["commit", "-qm", "ignore build output"])?;
    ok(&rig, &["worktree", "create", "ended", "--json"])?;
    let path = rig.project.join(".worktrees/ended");
    for output in [
        "old/target",
        "kept/target",
        "recent/target",
        "another/node_modules",
    ] {
        fs::create_dir_all(path.join(output))?;
        fs::write(path.join(output).join("artifact.lock"), "regenerate")?;
        age_gc_output(&path.join(output))?;
    }
    age_gc_output(&path.join("tracked/target"))?;
    fs::write(path.join("recent/target/new"), "new build")?;
    let external = rig.root.join("external");
    fs::create_dir(&external)?;
    fs::write(external.join("keep"), "external")?;
    std::os::unix::fs::symlink(&external, path.join("node_modules"))?;
    let (roots, context) = writer_context(&rig)?;
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let writer = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let prepared = writer_run(writer.clone(), "gc-ended", 101, "regenerate")?;
    let attempt = prepared.attempts[0].attempt_id;
    store.create(&prepared, &call)?;
    let config = format!(
        "schema_version=1\n[gc]\nfree_floor='1B'\n[gc.projects.'{}']\nregenerable=['target','node_modules']\nkeep=['kept/target']\n",
        context.project_id
    );
    fs::write(rig.state.join("config.toml"), &config)?;
    // Queued is live, even without a running process or held writer lease.
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(path.join("old/target").is_dir());
    store.cancel(attempt, "fixture ended", &call)?;
    let before_child =
        crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)?;
    assert!(before_child.is_current());
    assert_eq!(
        crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)?
            .get(&path),
        Some(&true)
    );
    // A queued child lane also blocks output pruning.
    let mut child_inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    child_inputs.state_dir = Some(rig.state.join("children/nested"));
    let child_context = provenant_run::ExecutionContext {
        state_root: child_inputs
            .resolve(&provenant_os::fs::SystemPathResolver)?
            .state,
        ..context.clone()
    };
    let child_store = open_writer_store(&child_context)?;
    let child_run = writer_run(writer, "gc-child", 111, "nested")?;
    child_store.create(&child_run, &call)?;
    assert!(!before_child.is_current());
    assert!(
        !crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)
            .is_ok_and(|states| states.get(&path) == Some(&true))
    );
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(path.join("old/target").is_dir());
    child_store.cancel(child_run.attempts[0].attempt_id, "child ended", &call)?;
    let snapshot =
        crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)?;
    assert!(snapshot.is_current());
    let writer = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let later = writer_run(writer, "gc-later", 112, "queued after snapshot")?;
    store.create(&later, &call)?;
    assert!(!snapshot.is_current());
    store.cancel(later.attempts[0].attempt_id, "later ended", &call)?;
    assert!(!snapshot.is_current(), "a changed snapshot stays invalid");
    let dry = ok(&rig, &["gc", "--dry-run", "--json"])?;
    assert_eq!(dry["data"]["dry_run"], true);
    assert!(path.join("old/target").is_dir());
    assert!(
        dry["data"]["worktree_cleanup"][0]["output"]
            .as_array()
            .ok_or("output")?
            .iter()
            .all(|entry| entry["removed"] == false)
    );
    fs::write(path.join("dirty"), "keep")?;
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(path.join("old/target").is_dir());
    fs::remove_file(path.join("dirty"))?;
    // Unreadable sealed lane state protects the worktree.
    let request = rig
        .state
        .join("runs")
        .join(prepared.run_id.to_string())
        .join("request.json");
    let sealed = fs::read(&request)?;
    fs::write(&request, b"broken")?;
    assert!(
        !crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)
            .is_ok_and(|states| states.get(&path) == Some(&true))
    );
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(path.join("old/target").is_dir());
    fs::write(&request, sealed)?;
    // No declaration grants no output deletion.
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n",
    )?;
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(path.join("old/target").is_dir());
    fs::write(rig.state.join("config.toml"), config)?;
    let applied = ok(&rig, &["gc", "--scheduled", "--apply", "--json"])?;
    assert!(!path.join("old/target").exists(), "{applied}");
    assert!(!path.join("another/node_modules").exists());
    assert!(path.join("kept/target").is_dir());
    assert!(path.join("tracked/target/keep").is_file());
    assert!(path.join("recent/target/new").is_file());
    assert!(
        fs::symlink_metadata(path.join("node_modules"))?
            .file_type()
            .is_symlink()
    );
    assert!(external.join("keep").is_file());
    Ok(())
}

fn gc_reports_terminal_tmp_paths_and_bytes_in_json_and_text() -> Result {
    use std::os::unix::fs::DirBuilderExt;
    let rig = repository()?;
    ok(&rig, &["worktree", "create", "gc-tmp", "--json"])?;
    let (_, context) = writer_context(&rig)?;
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let mut input = writer_run(
        provenant_run::WriterRequest {
            project: rig.project.clone(),
            worktree: rig.project.join(".worktrees/gc-tmp"),
        },
        "gc-tmp",
        201,
        "fixture",
    )?;
    input.request.tasks[0].writer = None;
    input.created_at = provenant_core::WallTime::from_micros(1)?;
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    store.withdraw(
        id,
        provenant_run::AttemptState::Queued,
        provenant_run::Outcome::Cancelled,
        "fixture",
        provenant_core::WallTime::from_micros(2)?,
        &call,
    )?;
    let attempt = rig
        .state
        .join(format!("runs/{}/attempts/{id}", input.run_id));
    let temporary = attempt.join("tmp");
    fs::DirBuilder::new().mode(0o700).create(&temporary)?;
    fs::write(temporary.join("payload"), vec![1; 8192])?;
    // Durable evidence and provider history must survive applied GC byte for byte.
    let mut retained = BTreeMap::new();
    for name in [
        "prompt.txt",
        "launch.json",
        "stdout.log",
        "stderr.log",
        "result.md",
        "process.json",
        "process-exit.json",
        "settlement.json",
        "terminal-pending.json",
        "terminal-commit.json",
        "release.json",
        "provider-history/candidate-0/sessions/session.jsonl",
        "provider-history/candidate-0/thread_history_1.sqlite",
    ] {
        let path = attempt.join(name);
        fs::create_dir_all(path.parent().ok_or("retained parent")?)?;
        if !path.exists() {
            fs::write(&path, format!("retained fixture: {name}\n\0"))?;
        }
        retained.insert(path.clone(), fs::read(path)?);
    }
    drop(store);
    let db = rusqlite::Connection::open(rig.state.join("run.sqlite3"))?;
    db.execute(
        "UPDATE attempts SET tmp_retained=1 WHERE id=?1",
        [id.to_string()],
    )?;
    let preview = ok(&rig, &["gc", "--dry-run", "--json"])?;
    let removal = &preview["data"]["provider_state"]["attempt_tmp"][0];
    assert_eq!(removal["path"], temporary.to_str().ok_or("tmp path")?);
    let bytes = removal["bytes"].as_u64().ok_or("tmp bytes")?;
    assert!(bytes >= 8192);
    assert!(temporary.exists());
    let text = command(&rig, &["gc", "--dry-run"])?;
    assert_eq!(code(&text), 0);
    let text = String::from_utf8(text.stdout.bytes)?;
    assert!(text.contains("would remove attempt tmp"), "{text}");
    assert!(
        text.contains(temporary.to_str().ok_or("tmp path")?),
        "{text}"
    );
    assert!(text.contains(&format!("{bytes} bytes")), "{text}");
    assert!(temporary.exists());
    let applied = ok(&rig, &["gc", "--apply", "--json"])?;
    assert_eq!(
        applied["data"]["provider_state"]["attempt_tmp"][0],
        *removal
    );
    assert!(!temporary.exists());
    for (path, bytes) in retained {
        assert_eq!(
            fs::read(&path)?,
            bytes,
            "{} changed during GC",
            path.display()
        );
    }
    let retained: bool = db.query_row(
        "SELECT tmp_retained FROM attempts WHERE id=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    assert!(!retained);
    Ok(())
}

fn dispatch_refuses_low_disk_writers() -> Result {
    let rig = repository()?;
    ok(&rig, &["worktree", "create", "low-disk", "--json"])?;
    let (roots, _) = writer_context(&rig)?;
    let settings = provenant_config::read(&roots)?;
    assert_eq!(settings.gc.free_floor_bytes(rig.state.as_path())?, 1);
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='18446744073709551615B'\n",
    )?;
    let prompt = rig.root.join("prompt.md");
    fs::write(&prompt, "hello")?;
    let output = command(
        &rig,
        &[
            "dispatch",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--model",
            "codex/id",
            "--mode",
            "worktree-write",
            "--worktree",
            ".worktrees/low-disk",
            "--operation-id",
            "low-disk",
            "--json",
        ],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 1, "{value}");
    assert_eq!(value["status"], "refused", "{value}");
    let message = value["error"]["message"].as_str().ok_or("message")?;
    assert!(
        message.contains("available")
            && message.contains("floor 18446744073709551615")
            && message.contains("provenant gc"),
        "{message}"
    );
    assert!(!rig.state.join("runs").exists());
    let reader = ok(
        &rig,
        &[
            "dispatch",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--model",
            "codex/id",
            "--operation-id",
            "low-disk-reader",
            "--json",
        ],
    )?;
    assert_eq!(reader["status"], "ok", "{reader}");
    assert!(rig.state.join("runs").exists());
    Ok(())
}

fn resume_refuses_low_disk_writer() -> Result {
    use provenant_run::AttemptReader;
    let rig = repository()?;
    ok(&rig, &["worktree", "create", "resume-disk", "--json"])?;
    let (_, context) = writer_context(&rig)?;
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let input = writer_run(
        provenant_run::WriterRequest {
            project: rig.project.clone(),
            worktree: rig.project.join(".worktrees/resume-disk"),
        },
        "resume-disk-prior",
        211,
        "prior",
    )?;
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    // Publish a synthetic provider exit through the normal immutable-record API.
    let exit = provenant_supervisor::custody::ExitObservation {
        publication: provenant_supervisor::custody::PublicationState::Durable,
        scope: provenant_supervisor::custody::Scope {
            run: input.run_id,
            attempt: id,
            generation: 0,
        },
        termination: Some(provenant_os::exec::Termination::Exit(0)),
        never_spawned: false,
        stop_reason: None,
        delivered_signals: vec![],
        reaped: vec![],
        omitted_reaped: 0,
        spared: vec![],
        omitted_spared: 0,
        warnings: vec![],
        omitted_warnings: 0,
        terminal_observed: true,
        unresolved_cleanup: false,
        unresolved_pids: vec![],
        settlement_evidence:
            provenant_supervisor::custody::SettlementEvidence::DarwinCensusAndCustodyLease,
        stdout: Default::default(),
        stderr: Default::default(),
    };
    let attempt = rig
        .state
        .join(format!("runs/{}/attempts/{id}", input.run_id));
    let mut evidence = serde_json::to_value(&exit)?;
    evidence["schema_version"] = 1.into();
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempt.join("process-exit.json"))?;
    file.write_all(&serde_json::to_vec(&evidence)?)?;
    drop(file);
    store.publish_terminal(
        id,
        store.record(id, &call)?.revision,
        &provenant_run::TerminalPublication {
            outcome: provenant_run::Outcome::Succeeded,
            started_at: Some(input.created_at),
            finished_at: clock.wall_now()?,
            provider_ms: Some(1),
            reason: None,
            result: "prior result",
            complete: true,
            redacted: false,
            resolved_route: Some(fixture_route()),
            applied_route: Some(fixture_route()),
            observed_route: Some(fixture_route()),
            usage: Default::default(),
            session: Some("fixture-session".into()),
        },
        &exit,
        &call,
    )?;
    drop(store);
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='18446744073709551615B'\n",
    )?;
    let prompt = rig.root.join("resume.txt");
    fs::write(&prompt, "continue")?;
    let output = command(
        &rig,
        &[
            "resume",
            &id.to_string(),
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--operation-id",
            "resume-disk-low",
            "--json",
        ],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 1, "{value}");
    assert_eq!(value["status"], "refused", "{value}");
    assert_eq!(value["error"]["code"], "REFUSED", "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .ok_or("message")?
            .contains("provenant gc"),
        "{value}"
    );
    let store = open_writer_store(&context)?;
    // The CLI continuation belongs to its actor, rather than the fixture actor.
    let db = rusqlite::Connection::open(rig.state.join("run.sqlite3"))?;
    let run: String = db.query_row(
        "SELECT run FROM operations WHERE project=?1 AND operation=?2",
        rusqlite::params![context.project_id.to_string(), "resume-disk-low"],
        |row| row.get(0),
    )?;
    let run = provenant_core::RunId::parse(&run)?;
    let attempts = store
        .attempts(
            provenant_run::AttemptFilter {
                run_id: Some(run),
                task_id: None,
            },
            &call,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(attempts.len(), 1);
    let refused = store.record(attempts[0].attempt_id, &call)?;
    assert!(matches!(
        refused.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Rejected,
            ..
        }
    ));
    assert!(refused.applied_route.is_none());
    assert!(
        !rig.state
            .join(format!(
                "runs/{}/attempts/{}/launch.json",
                run, attempts[0].attempt_id
            ))
            .exists()
    );
    Ok(())
}

fn gc_disk_alert_is_once_per_dip_and_manual_preview_does_not_latch() -> Result {
    let rig = repository()?;
    ok(&rig, &["worktree", "create", "idle", "--json"])?;
    let set_floor = |floor: &str| -> Result {
        fs::write(
            rig.state.join("config.toml"),
            format!("schema_version=1\n[gc]\nfree_floor='{floor}'\nalert_below='1B'\n"),
        )?;
        Ok(())
    };
    set_floor("18446744073709551615B")?;
    let dry = ok(&rig, &["gc", "--dry-run", "--json"])?;
    assert_eq!(dry["data"]["alerts"].as_array().ok_or("alerts")?.len(), 1);
    assert!(!rig.state.join("gc/low-disk.json").exists());
    let first = ok(&rig, &["gc", "--apply", "--json"])?;
    assert_eq!(first["data"]["alerts"].as_array().ok_or("alerts")?.len(), 1);
    let again = ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(
        again["data"]["alerts"]
            .as_array()
            .ok_or("alerts")?
            .is_empty()
    );
    set_floor("0B")?;
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(!rig.state.join("gc/low-disk.json").exists());
    set_floor("18446744073709551615B")?;
    let second = ok(&rig, &["gc", "--apply", "--json"])?;
    assert_eq!(
        second["data"]["alerts"].as_array().ok_or("alerts")?.len(),
        1
    );
    let (roots, context) = writer_context(&rig)?;
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let backoff = provenant_fabric_store::ThreadWait;
    let request = provenant_fabric_store::Request {
        clock: &clock,
        budget: provenant_core::WaitBudget::cli(clock.mono_now(), 5)?,
        cancel: &cancel,
        backoff: &backoff,
    };
    let store = provenant_fabric_store::Store::open(&roots.state, &request)?;
    let events = store.events(context.project_id, 0, 100, &request)?;
    assert_eq!(
        events
            .items
            .iter()
            .filter(|event| event.record.entity.as_deref() == Some("gc.low_disk"))
            .count(),
        2
    );
    Ok(())
}
