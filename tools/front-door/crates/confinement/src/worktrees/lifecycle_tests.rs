//! Worktree lifecycle, removal, creation and GC tests.
pub(super) fn test_uids(uid: u32) -> ProcessUids {
    ProcessUids {
        real: uid,
        effective: uid,
        saved: uid,
    }
}

pub(super) fn builtin_audit_has_only_rejections(
    conditions: impl IntoIterator<Item = Option<&'static str>>,
) -> bool {
    conditions.into_iter().all(|condition| condition.is_some())
}

use super::*;

use crate::test_support::{
    Fixture, add_poisoned_gitlink, assert_raw_git_runs_poison_filter, fixture_env, run_fixture_git,
};
use crate::utility::{GitMutation, execute_git_mutation};

use provenant_core::Clock;

use provenant_os::clock::SystemClock;

use provenant_os::fs::Directory;

use std::error::Error;

use std::fs;

use std::os::unix::fs::PermissionsExt;

pub(super) struct FreeWorkers;

#[test]
fn streaming_inventory_reports_the_total_before_checked_rows() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let linked = create(
        &context,
        WorktreeCreate {
            branch: "streamed".into(),
            base: None,
        },
        &cancel(),
    )?;
    let mut events = Vec::new();
    let inventory = list_streaming(&context, &cancel(), |total, row| {
        events.push((total, row.map(|row| row.path.clone())));
    })?;
    assert_eq!(events.first(), Some(&(2, None)));
    assert_eq!(events.len(), 3);
    assert!(events.iter().skip(1).all(|(total, _)| *total == 2));
    assert!(
        events
            .iter()
            .any(|(_, path)| path.as_ref() == Some(&linked.path))
    );
    assert_eq!(inventory.len(), 2);
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods, clippy::print_stderr)] // Timing fixture around the real GC preview.
fn gc_preview_reuses_one_census_for_many_worktrees() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, CensusError, ProcessIdentity};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting {
        calls: AtomicUsize,
        current: ProcessFact,
    }
    impl ProcessObserver for Counting {
        fn boot_identity(&self) -> Result<String, CensusError> {
            Ok("fixture-boot".into())
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            if pid == self.current.identity.pid {
                ProcessObservation::Present(self.current.clone())
            } else {
                ProcessObservation::Absent
            }
        }
        fn census(&self, _: usize) -> Result<Census, CensusError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Census::from_observations([self.current.clone()], [], [], true, false, 32)
        }
    }
    impl CwdObserver for Counting {
        fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
            provenant_os::pid::OccupancyObservation {
                paths: vec![],
                complete: true,
                gap: None,
            }
        }
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(CwdObservation::Gone)
        }
    }
    impl WorkerObserver for Counting {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            super::census::observe_worker_roots(roots, self, self, &self.current, false).0
        }
    }
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".git/info/exclude"), "target/\n")?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    for index in 0..40 {
        let path = fixture.root.join(format!("foreign-{index}"));
        fixture.run(&["worktree", "add", "--detach", path.to_str().ok_or("path")?])?;
        fs::create_dir(path.join("target"))?;
        fs::write(path.join("target/artifact"), "output")?;
    }
    let mut lanes = list(&context, &cancel())?;
    for lane in &mut lanes {
        lane.writer = WriterState::Unknown;
    }
    struct CountingGit<'a> {
        inner: &'a dyn Executor,
        listings: AtomicUsize,
    }
    impl Executor for CountingGit<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            if options
                .argv
                .windows(2)
                .any(|pair| pair[0] == "worktree" && pair[1] == "list")
            {
                self.listings.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.execute(options)
        }
    }
    let observer = Counting {
        calls: AtomicUsize::new(0),
        current: ProcessFact {
            identity: ProcessIdentity {
                boot: "fixture-boot".into(),
                pid: 100,
                native_start: 100,
                group_id: 100,
                session_id: 100,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        },
    };
    let session = CensusSession::new(&observer, &test.clock);
    let git = CountingGit {
        inner: context.executor,
        listings: AtomicUsize::new(0),
    };
    context.workers = &session;
    context.executor = &git;
    let started = std::time::Instant::now();
    let report = collect(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &["target".into()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    eprintln!(
        "GC preview collection for 40 worktrees: {:?}",
        started.elapsed()
    );
    assert_eq!(observer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(git.listings.load(Ordering::SeqCst), 1);
    assert_eq!(report.idle_output.len(), 40, "{report:?}");
    assert!(
        report
            .idle_output
            .iter()
            .all(|entry| !entry.removed && entry.reason == "would_remove")
    );
    Ok(())
}

#[test]
fn repo_facts_resolves_a_moved_ref_fresh() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let facts = RepoFacts::default();
    fixture.run(&["branch", "facts-ref", "HEAD"])?;
    let name = GitRefName::branch("facts-ref")?;
    let initial = facts.resolve_commit(&context, name.clone(), &cancel())?;
    fixture.run(&["commit", "--allow-empty", "-m", "move facts ref"])?;
    fixture.run(&["branch", "-f", "facts-ref", "HEAD"])?;
    let moved = facts.resolve_commit(&context, name, &cancel())?;
    assert_ne!(moved, initial);
    assert_eq!(moved, fixture.head()?);
    assert_eq!(
        facts.resolve_revision(&context, GitRevision::new("facts-ref")?, &cancel())?,
        moved
    );
    assert_eq!(
        facts.resolve_revision(&context, GitRevision::new("facts-ref~1")?, &cancel())?,
        initial
    );
    assert_eq!(
        facts.symbolic_revision(&context, GitRevision::new("facts-ref")?, &cancel())?,
        Some("refs/heads/facts-ref".into())
    );
    context.facts.capture_inventory(&context, &cancel())?;
    read_worktrees(&context, &cancel())?;
    assert!(context.facts.take_inventory(&context).is_none());
    let inventory_limit = provenant_os::exec::DEFAULT_CAPTURE_LIMIT;
    assert!(RepoFacts::with_limits(inventory_limit, 0).is_err());
    assert!(RepoFacts::with_limits(0, provenant_os::git::REPO_FACT_SCALAR_LIMIT).is_err());
    let limited = RepoFacts::with_limits(inventory_limit, 1)?;
    assert!(matches!(
        limited.resolve_commit(&context, GitRefName::branch("facts-ref")?, &cancel()),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    assert!(matches!(
        limited.resolve_revision(&context, GitRevision::new("facts-ref")?, &cancel()),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    assert!(matches!(
        limited.symbolic_revision(&context, GitRevision::new("facts-ref")?, &cancel()),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    let limited_inventory = RepoFacts::with_limits(1, provenant_os::git::REPO_FACT_SCALAR_LIMIT)?;
    assert!(matches!(
        limited_inventory.capture_inventory(&context, &cancel()),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    Ok(())
}

#[test]
fn gc_does_not_inspect_status_of_out_of_scope_worktrees() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct CountingGit<'a> {
        inner: &'a dyn Executor,
        calls: AtomicUsize,
    }
    impl Executor for CountingGit<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.execute(options)
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    for index in 0..100 {
        let path = fixture.root.join(format!("foreign-{index}"));
        fixture.run(&["worktree", "add", "--detach", path.to_str().ok_or("path")?])?;
    }
    let git = CountingGit {
        inner: context.executor,
        calls: AtomicUsize::new(0),
    };
    context.executor = &git;
    let facts = RepoFacts::default();
    let lanes = list_for_gc(&context, &facts, &cancel())?;
    let report = collect_lazy(
        &context,
        &facts,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    assert!(report.worktrees.removed.is_empty());
    // A single command-scoped inventory serves both phases of a preview.
    assert_eq!(git.calls.load(Ordering::SeqCst), 1);
    let lanes = list_for_gc(&context, &facts, &cancel())?;
    collect_lazy(
        &context,
        &facts,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| false,
            no_live_lanes: &|_| false,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    // Applying discards the cached snapshot and reads again under custody.
    assert_eq!(git.calls.load(Ordering::SeqCst), 3);
    Ok(())
}

#[test]
fn gc_lazy_inventory_still_refuses_a_dirty_candidate() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let lane = create(
        &context,
        WorktreeCreate {
            branch: "dirty-candidate".into(),
            base: None,
        },
        &cancel(),
    )?;
    fs::write(lane.path.join("untracked"), "keep")?;
    let facts = RepoFacts::default();
    let lanes = list_for_gc(&context, &facts, &cancel())?;
    let report = collect_lazy(
        &context,
        &facts,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    assert!(
        report
            .worktrees
            .retained
            .iter()
            .any(|entry| entry.branch == "dirty-candidate" && entry.reason == RetainReason::Dirty)
    );
    assert!(lane.path.join("untracked").exists());
    Ok(())
}

#[test]
fn gc_lazy_candidate_is_retained_when_its_git_probe_fails() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct RejectConfig<'a> {
        inner: &'a dyn Executor,
        rejected: AtomicUsize,
    }
    impl Executor for RejectConfig<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            if options.argv.iter().any(|arg| arg == "config") {
                self.rejected.fetch_add(1, Ordering::SeqCst);
                return Err(provenant_os::exec::ExecError::InvalidLimit);
            }
            self.inner.execute(options)
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let lane = create(
        &context,
        WorktreeCreate {
            branch: "probe-failure".into(),
            base: None,
        },
        &cancel(),
    )?;
    let git = RejectConfig {
        inner: context.executor,
        rejected: AtomicUsize::new(0),
    };
    context.executor = &git;
    let facts = RepoFacts::default();
    let lanes = list_for_gc(&context, &facts, &cancel())?;
    let report = collect_lazy(
        &context,
        &facts,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    assert!(git.rejected.load(Ordering::SeqCst) > 0);
    assert!(lane.path.exists());
    assert!(report.worktrees.removed.is_empty());
    assert!(
        report
            .worktrees
            .retained
            .iter()
            .any(|entry| entry.branch == "probe-failure")
    );
    Ok(())
}

#[test]
fn gc_lazy_candidate_never_runs_nested_git_filters() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let lane = create(
        &context,
        WorktreeCreate {
            branch: "lazy-gitlink".into(),
            base: None,
        },
        &cancel(),
    )?;
    let marker = fixture._temp.path().join("lazy-filter-ran");
    let poison = add_poisoned_gitlink(&fixture, &lane.path, "sub", &marker)?;
    assert_raw_git_runs_poison_filter(&fixture, &poison)?;
    poison.arm()?;
    let facts = RepoFacts::default();
    let lanes = list_for_gc(&context, &facts, &cancel())?;
    let report = collect_lazy(
        &context,
        &facts,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    assert!(!marker.exists());
    assert!(
        report
            .worktrees
            .retained
            .iter()
            .any(|entry| entry.branch == "lazy-gitlink" && entry.reason == RetainReason::Dirty)
    );
    assert!(lane.path.exists());
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)] // Compare fixture mtimes to the filesystem clock.
fn gc_unknown_writer_only_prunes_idle_unheld_output() -> Result<(), Box<dyn Error>> {
    for scenario in [
        "preview", "idle", "aged", "unowned", "recent", "index", "live", "unknown",
    ] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), "target/\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore output"])?;
        let test = TestContext::new(&fixture)?;
        let mut context = test.context();
        let path = fixture._temp.path().join("foreign-output");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "foreign-output",
            path.to_str().ok_or("path")?,
        ])?;
        fs::create_dir(path.join("target"))?;
        fs::write(path.join("target/artifact"), "output")?;
        fs::write(path.join("untracked"), "source")?;
        if matches!(scenario, "index" | "aged") {
            let old = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
            let tree = verify_worktree(context.repo, &path)?;
            for item in [
                path.join("target/artifact"),
                path.join("target"),
                path.join("untracked"),
                path.join(".gitignore"),
                path.clone(),
                tree.repo().private_git_dir().join("HEAD"),
            ] {
                fs::File::open(item)?.set_times(fs::FileTimes::new().set_modified(old))?;
            }
            if scenario == "aged" {
                fs::File::open(tree.repo().private_git_dir().join("index"))?
                    .set_times(fs::FileTimes::new().set_modified(old))?;
            }
        }
        let mut lane = list(&context, &cancel())?
            .into_iter()
            .find(|lane| lane.path == path)
            .ok_or("missing tree")?;
        if scenario != "unowned" {
            lane.writer = WriterState::Unknown;
        } else {
            assert_eq!(lane.owner, OwnerStatus::Unowned);
            assert_eq!(lane.writer, WriterState::Free);
        }
        let live = NamedLive;
        let unknown = UnknownWorkers;
        if scenario == "live" {
            context.workers = &live;
        }
        if scenario == "unknown" {
            context.workers = &unknown;
        }
        let report = collect(
            &context,
            &[lane],
            &WorktreeGcPolicy {
                upstream: None,
                terminal: &|_| false,
                no_live_lanes: &|_| true,
                regenerable: &["target".into()],
                keep: &[],
                written_before_ns: match scenario {
                    "recent" => 0,
                    "index" | "aged" => (std::time::SystemTime::now()
                        - std::time::Duration::from_secs(3600))
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos() as i128,
                    _ => i128::MAX,
                },
                dry_run: scenario == "preview",
                foreign_merged_clean: false,
            },
            &cancel(),
        )?;
        assert!(path.is_dir(), "{scenario}");
        assert_eq!(fs::read_to_string(path.join("untracked"))?, "source");
        assert_eq!(
            path.join("target/artifact").exists(),
            !matches!(scenario, "idle" | "aged" | "unowned"),
            "{scenario}"
        );
        if scenario == "preview" {
            assert!(
                report.idle_output.iter().any(
                    |entry| entry.path == path.join("target") && entry.reason == "would_remove"
                )
            );
        }
        if matches!(scenario, "idle" | "aged" | "unowned") {
            assert!(
                report
                    .idle_output
                    .iter()
                    .any(|entry| entry.path == path.join("target") && entry.removed)
            );
        }
        if matches!(scenario, "recent" | "index" | "live" | "unknown") {
            assert!(report.idle_output.iter().all(|entry| !entry.removed));
        }
    }
    Ok(())
}

#[test]
fn gc_native_output_preview_creates_no_writer_state() -> Result<(), Box<dyn Error>> {
    for existing_slot in [false, true] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), ".worktrees/\ntarget/\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore build output"])?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let path = fixture.root.join(".worktrees/native");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "native",
            path.to_str().ok_or("path")?,
        ])?;
        fs::create_dir(path.join("target"))?;
        fs::write(path.join("target/artifact"), "cached output")?;
        let slot = context
            .project_state
            .current_path()?
            .join(state_dir_name(&path));
        if existing_slot {
            writer_directory(context.project_state, &path)?;
        }
        assert!(
            cache_writer_probe(context.project_state, &path)
                .map_err(|_| "probe failed")?
                .is_none()
        );
        let lanes = list(&context, &cancel())?;
        let regenerable = ["target".into()];
        let policy = WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &regenerable,
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        };
        let preview = collect(&context, &lanes, &policy, &cancel())?;
        assert_eq!(preview.idle_output.len(), 1);
        assert_eq!(preview.idle_output[0].reason, "would_remove");
        assert!(path.join("target/artifact").is_file());
        assert_eq!(slot.exists(), existing_slot);
        assert!(!slot.join(WRITER_LOCK).exists());
        let applied = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                dry_run: false,
                ..policy
            },
            &cancel(),
        )?;
        assert_eq!(applied.idle_output.len(), 1);
        assert!(!path.join("target").exists());
        assert!(slot.join(WRITER_LOCK).is_file());
        let held = cache_writer_guard(context.project_state, &path).map_err(|_| "guard failed")?;
        assert!(matches!(
            cache_writer_probe(context.project_state, &path),
            Err(WorkerState::Busy)
        ));
        drop(held);
    }
    Ok(())
}

#[test]
fn retained_import_fences_a_released_writer_slot() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture.root.join(".worktrees/native");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "native",
        path.to_str().ok_or("path")?,
    ])?;
    let slot = writer_directory(context.project_state, &path)?;
    let scope = provenant_supervisor::custody::Scope {
        run: provenant_core::RunId::from_bytes([11; 16]),
        attempt: provenant_core::AttemptId::from_bytes([12; 16]),
        generation: 1,
    };
    let lease = provenant_supervisor::fixture::acquire_writer(
        slot.try_clone()?,
        scope,
        WaitBudget::nonblocking(context.clock.mono_now()),
        &cancel(),
        context.clock,
    )?;
    let attempt = slot.current_path()?.join(scope.attempt.to_string());
    let reservation = fs::read(attempt.join("reservation.json"))?;
    fs::write(attempt.join("import-retained.json"), "retained")?;
    drop(lease);
    let listing = provenant_supervisor::custody::list_writer_reservations(context.project_state)?;
    assert!(listing.issues.is_empty());
    assert!(
        listing
            .reservations
            .iter()
            .any(|record| record.scope == scope)
    );

    assert!(matches!(
        cache_writer_probe(context.project_state, &path),
        Err(WorkerState::Busy)
    ));
    assert!(matches!(
        cache_writer_guard(context.project_state, &path),
        Err(WorkerState::Busy)
    ));
    let lane = list(&context, &cancel())?
        .into_iter()
        .find(|lane| lane.path == path)
        .ok_or("lane")?;
    assert_eq!(lane.writer, WriterState::Busy);
    let tree = verify_worktree(&fixture.repo, &path)?;
    let new_owner = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("retained-import-claim".parse()?),
        branch: "native".into(),
        base: lane.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
    };
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &new_owner,
            lane.head,
            None,
            false,
            &cancel()
        ),
        Err(WorktreeError::LockBusy)
    ));
    assert!(load_owner(&tree)?.is_none());
    assert_eq!(fs::read(attempt.join("reservation.json"))?, reservation);
    assert_eq!(fs::read(attempt.join("import-retained.json"))?, b"retained");
    let entries = read_worktrees(&context, &cancel())?;
    let entry = entries
        .iter()
        .find(|entry| entry.path == path)
        .ok_or("entry")?;
    assert!(matches!(
        lock_worktree(&context, entry, &cancel(), false),
        Err(WorktreeError::LockBusy)
    ));
    assert!(matches!(
        lock_worktree(&context, entry, &cancel(), true),
        Err(WorktreeError::LockBusy)
    ));
    fs::create_dir(path.join("target"))?;
    fs::write(path.join("target/artifact"), "retained")?;
    let lanes = list(&context, &cancel())?;
    let regenerable = ["target".into()];
    let policy = WorktreeGcPolicy {
        upstream: None,
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable: &regenerable,
        keep: &[],
        written_before_ns: i128::MAX,
        dry_run: false,
        foreign_merged_clean: false,
    };
    collect(&context, &lanes, &policy, &cancel())?;
    assert!(path.join("target/artifact").is_file());

    // Settlement can retain historical evidence after clearing the reservation.
    fs::remove_file(attempt.join("reservation.json"))?;
    assert!(cache_writer_guard(context.project_state, &path).is_ok());
    Ok(())
}

#[test]
fn git_inventory_accepts_unregistered_and_recreated_worktrees() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture.root.join(".worktrees/native");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "native",
        path.to_str().ok_or("path")?,
    ])?;
    let name = state_dir_name(&path);
    assert!(
        context
            .project_state
            .open_verified_directory(&name)
            .is_err()
    );
    let inventory = || {
        Ok(
            run_git(&context, &fixture.repo, GitRead::WorktreeList, &cancel())?
                .stdout
                .bytes,
        )
    };
    let bind = |branch: &str| -> Result<_, WorktreeError> {
        writer_custody(
            context.project_state,
            &verify_worktree(&fixture.repo, &path)?,
            &GitRefName::branch(branch).map_err(|_| WorktreeError::InvalidRequest)?,
            fixture.head().map_err(|_| WorktreeError::InvalidOutput)?,
            inventory,
        )
    };
    assert_eq!(
        list(&context, &cancel())?
            .iter()
            .find(|tree| tree.path == path)
            .ok_or("tree")?
            .writer,
        WriterState::Free
    );
    assert!(
        context
            .project_state
            .open_verified_directory(&name)
            .is_err(),
        "listing creates no state"
    );
    let (state, _) = bind("native")?;
    assert!(
        state
            .metadata_at(OsStr::new("identity.json"), false)
            .is_err()
    );
    assert!(matches!(bind("other"), Err(WorktreeError::IdentityChanged)));
    // Legacy identity and registry reservations are never membership gates.
    fs::write(
        state.current_path()?.join("identity.json"),
        b"invalid old schema and inode",
    )?;
    fs::write(
        state.current_path()?.join("reservation.json"),
        b"legacy registry data",
    )?;
    fixture.run(&["worktree", "remove", path.to_str().ok_or("path")?])?;
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "recreated",
        path.to_str().ok_or("path")?,
    ])?;
    bind("recreated")?;
    let outcome = remove(
        &context,
        WorktreeRemove::Name {
            name: "recreated".into(),
            discard: false,
            integration: None,
            expected_head: None,
        },
        &cancel(),
    )?;
    assert_eq!(outcome.removed.len(), 1);
    // Reusing a removed path has no tombstone/schema/branch refusal.
    create(
        &context,
        WorktreeCreate {
            branch: "native".into(),
            base: None,
        },
        &cancel(),
    )?;
    Ok(())
}

#[test]
fn writer_lock_busy_is_distinct_from_observed_live_process() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "busy".into(),
            base: None,
        },
        &cancel(),
    )?;
    let state = writer_directory(context.project_state, &info.path)?;
    let before = state.metadata_at(OsStr::new(WRITER_LOCK), false)?;
    let held = state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?;
    let request = || WorktreeRemove::Name {
        name: "busy".into(),
        discard: false,
        integration: None,
        expected_head: None,
    };
    let error = remove(&context, request(), &cancel())
        .err()
        .ok_or("removed busy tree")?;
    assert!(matches!(error, WorktreeError::LockBusy));
    assert_eq!(
        list(&context, &cancel())?
            .iter()
            .find(|tree| tree.path == info.path)
            .ok_or("tree")?
            .writer,
        WriterState::Busy
    );
    let report = collect(
        &context,
        &list(&context, &cancel())?,
        &WorktreeGcPolicy {
            upstream: Some(fixture.head()?),
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    assert!(
        report
            .worktrees
            .retained
            .iter()
            .any(|tree| tree.reason == RetainReason::LockBusy)
    );
    drop(held);
    let live = WorktreeContext {
        workers: &NamedLive,
        ..test.context()
    };
    assert!(matches!(
        remove(&live, request(), &cancel()),
        Err(WorktreeError::LiveProcess(_))
    ));
    remove(&context, request(), &cancel())?;
    assert!(before.same_identity(&state.metadata_at(OsStr::new(WRITER_LOCK), false)?));
    Ok(())
}

#[test]
fn foreign_gc_removes_recent_declared_output() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture._temp.path().join("foreign");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "foreign",
        path.to_str().ok_or("path")?,
    ])?;
    fs::create_dir(path.join("target"))?;
    fs::write(path.join("target/artifact"), "recent output")?;
    let lanes = list(&context, &cancel())?;
    let report = collect(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            upstream: Some(fixture.head()?),
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &["target".into()],
            keep: &[],
            written_before_ns: 0,
            dry_run: false,
            foreign_merged_clean: true,
        },
        &cancel(),
    )?;
    assert_eq!(report.worktrees.removed.len(), 1);
    assert!(!path.exists());
    assert!(!branch_exists(&fixture, "foreign"));
    Ok(())
}

#[test]
fn source_fast_forward_advances_clean_tree_and_preserves_ignored_output()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore build output"])?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "source-writer".into(),
            base: None,
        },
        &cancel(),
    )?;
    fs::create_dir(info.path.join("target"))?;
    fs::write(info.path.join("target/cache"), "retained output")?;
    fs::write(fixture.root.join("source-file"), "pinned source")?;
    fixture.run(&["add", "source-file"])?;
    fixture.run(&["commit", "-m", "combined source"])?;
    let target = fixture.head()?;
    let hooks = fixture.repo.common_git_dir().join("hooks");
    fs::create_dir_all(&hooks)?;
    fs::write(hooks.join("post-merge"), "#!/bin/sh\n: > hook-ran\n")?;
    fs::set_permissions(hooks.join("post-merge"), fs::Permissions::from_mode(0o700))?;
    let session = CensusSession::new(&test.workers, &test.clock);
    context.workers = &session;
    fast_forward(&context, &info.path, target, &cancel())?;
    assert_eq!(
        session.observed_at(),
        None,
        "mutation must invalidate the custody census"
    );
    let actual = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing source writer")?;
    assert_eq!(actual.head, target);
    assert_eq!(actual.dirty, WorktreeCondition::Ignored);
    assert_eq!(
        fs::read_to_string(info.path.join("source-file"))?,
        "pinned source"
    );
    assert_eq!(
        fs::read_to_string(info.path.join("target/cache"))?,
        "retained output"
    );
    assert!(!info.path.join("hook-ran").exists());
    fast_forward(&context, &info.path, target, &cancel())?;
    Ok(())
}

#[test]
fn batch_worktree_on_a_private_branch_follows_an_integration_ref_by_fast_forward()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let start = fixture.head()?;
    fixture.run(&["branch", "batch/graph"])?;
    let batch = create(
        &context,
        WorktreeCreate {
            branch: "batch/work".into(),
            base: None,
        },
        &cancel(),
    )?;
    let lane = create(
        &context,
        WorktreeCreate {
            branch: "lane".into(),
            base: None,
        },
        &cancel(),
    )?;
    run_in_worktree(
        &fixture,
        &lane.path,
        &["commit", "--allow-empty", "-m", "lane work"],
    )?;
    let entry = |path: &Path| {
        list(&context, &cancel())?
            .into_iter()
            .find(|entry| entry.path == path)
            .ok_or_else(|| Box::<dyn Error>::from("missing worktree"))
    };
    let lane_head = entry(&lane.path)?.head;
    // Delivery advances the integration ref without touching any checkout.
    fixture.run(&[
        "update-ref",
        "refs/heads/batch/graph",
        &lane_head.to_string(),
        &start.to_string(),
    ])?;
    let before = entry(&batch.path)?;
    assert_eq!(before.head, start);
    assert_eq!(before.dirty, WorktreeCondition::Clean);
    fast_forward(&context, &batch.path, lane_head, &cancel())?;
    let after = entry(&batch.path)?;
    assert_eq!(after.head, lane_head);
    assert_eq!(after.dirty, WorktreeCondition::Clean);
    assert_eq!(after.branch.as_deref(), Some("refs/heads/batch/work"));
    Ok(())
}

#[test]
fn fallback_writer_after_a_partial_commit_continues_past_the_pin() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "source-writer".into(),
            base: None,
        },
        &cancel(),
    )?;
    let pin = fixture.head()?;
    fast_forward(&context, &info.path, pin, &cancel())?;
    run_in_worktree(
        &fixture,
        &info.path,
        &["commit", "--allow-empty", "-m", "partial work"],
    )?;
    let ahead = read_worktrees(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing writer")?
        .head;
    assert_ne!(ahead, pin);
    fast_forward(&context, &info.path, pin, &cancel())?;
    let after = read_worktrees(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing writer")?
        .head;
    assert_eq!(after, ahead);
    Ok(())
}

#[test]
fn source_fast_forward_refuses_dirty_diverged_or_leased_tree() -> Result<(), Box<dyn Error>> {
    for scenario in ["dirty", "diverged", "leased"] {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "source-writer".into(),
                base: None,
            },
            &cancel(),
        )?;
        fixture.run(&["commit", "--allow-empty", "-m", "combined source"])?;
        let target = fixture.head()?;
        let state = context
            .project_state
            .open_verified_directory(&state_dir_name(&info.path))?;
        let lease = if scenario == "leased" {
            Some(state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?)
        } else {
            None
        };
        if scenario == "dirty" {
            fs::write(info.path.join("keep"), "uncommitted")?;
        } else if scenario == "diverged" {
            run_in_worktree(
                &fixture,
                &info.path,
                &["commit", "--allow-empty", "-m", "diverged writer"],
            )?;
        }
        let before = read_worktrees(&context, &cancel())?
            .into_iter()
            .find(|entry| entry.path == info.path)
            .ok_or("missing writer")?
            .head;
        let error = fast_forward(&context, &info.path, target, &cancel())
            .err()
            .ok_or_else(|| format!("{scenario} source writer advanced"))?;
        match scenario {
            "dirty" => {
                assert!(matches!(error, WorktreeError::Dirty(ref paths) if paths.contains("keep")))
            }
            "diverged" => assert!(
                matches!(error, WorktreeError::SourceDiverged { current, target: requested } if current == before && requested == target)
            ),
            "leased" => assert!(matches!(error, WorktreeError::LockBusy)),
            _ => unreachable!(),
        }
        let after = read_worktrees(&context, &cancel())?
            .into_iter()
            .find(|entry| entry.path == info.path)
            .ok_or("missing writer")?
            .head;
        assert_eq!(after, before, "{scenario}");
        if scenario == "dirty" {
            assert_eq!(fs::read_to_string(info.path.join("keep"))?, "uncommitted");
        }
        drop(lease);
    }
    Ok(())
}

#[test]
fn source_fast_forward_preserves_ignored_files_that_become_tracked() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "cache\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore local cache"])?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "source-writer".into(),
            base: None,
        },
        &cancel(),
    )?;
    fs::write(info.path.join("cache"), "keep ignored content")?;
    fs::write(fixture.root.join("cache"), "tracked source content")?;
    fixture.run(&["add", "-f", "cache"])?;
    fixture.run(&["commit", "-m", "track cache path"])?;
    let target = fixture.head()?;
    assert!(matches!(
        fast_forward(&context, &info.path, target, &cancel()),
        Err(WorktreeError::SourceAdvanceFailed { target: requested }) if requested == target
    ));
    let after = read_worktrees(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing writer")?
        .head;
    assert_eq!(after, info.head);
    assert_eq!(
        fs::read_to_string(info.path.join("cache"))?,
        "keep ignored content"
    );
    Ok(())
}

#[test]
fn occupancy_keeps_scoped_census_evidence_and_held_lease_priority() -> Result<(), Box<dyn Error>> {
    struct Incomplete;
    struct Contended;
    impl WorkerObserver for Contended {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|path| (path, WorkerState::Busy))
                .collect()
        }
    }
    impl WorkerObserver for Incomplete {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|path| (path, WorkerState::Unknown))
                .collect()
        }
        fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
            (
                self.observe_roots(roots),
                roots
                    .iter()
                    .cloned()
                    .map(|path| (path, "fixture census incomplete".into()))
                    .collect(),
            )
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "occupancy-evidence".into(),
            base: None,
        },
        &cancel(),
    )?;
    context.workers = &Incomplete;
    let observation = list_with_observation(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.info.path == info.path)
        .ok_or("missing worktree")?;
    assert!(matches!(
        observation.occupancy,
        Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Census }
            if scope == info.path && evidence == "fixture census incomplete"
    ));
    let _held = cache_writer_guard(context.project_state, &info.path).map_err(|_| "lease")?;
    let observation = list_with_observation(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.info.path == info.path)
        .ok_or("missing worktree")?;
    assert_eq!(observation.occupancy, Occupancy::Held);
    drop(_held);
    context.workers = &Contended;
    let observation = list_with_observation(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.info.path == info.path)
        .ok_or("missing worktree")?;
    assert!(matches!(
        observation.occupancy,
        Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Census }
            if scope == info.path && evidence.contains("without a held writer lease")
    ));
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let owner = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("occupancy-evidence".parse()?),
        branch: "occupancy-evidence".into(),
        base: info.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
    };
    save_lane_owner_when_free(&context, &tree, &owner, info.head, None, false, &cancel())?;
    assert_eq!(load_owner(&tree)?, Some(owner));
    fixture.run(&["worktree", "lock", info.path.to_str().ok_or("path")?])?;
    let observation = list_with_observation(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.info.path == info.path)
        .ok_or("missing worktree")?;
    assert!(matches!(
        observation.occupancy,
        Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Custody }
            if scope == info.path && evidence.contains("locked")
    ));
    Ok(())
}

#[test]
fn held_writer_lease_takes_precedence_over_live_census() -> Result<(), Box<dyn Error>> {
    struct Live;
    impl WorkerObserver for Live {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|path| (path, WorkerState::Live))
                .collect()
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "lease-priority".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let attempt = provenant_core::AttemptId::from_bytes([42; 16]);
    let owner = WorktreeOwner {
        attempt_id: Some(attempt),
        operation_id: Some("lease-priority".parse()?),
        branch: "lease-priority".into(),
        base: info.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
    };
    save_owner(&tree, &owner)?;
    let _held = cache_writer_guard(context.project_state, &info.path).map_err(|_| "lease")?;
    context.workers = &Live;
    let listed = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing worktree")?;
    assert_eq!(listed.writer, WriterState::Busy);
    assert_eq!(
        load_owner(&tree)?.and_then(|owner| owner.attempt_id),
        Some(attempt)
    );
    let next = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("next-lease".parse()?),
        ..owner.clone()
    };
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &next,
            info.head,
            Some(&owner),
            true,
            &cancel()
        ),
        Err(WorktreeError::LockBusy)
    ));
    drop(_held);
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &next,
            info.head,
            Some(&owner),
            false,
            &cancel()
        ),
        Err(WorktreeError::LiveWriter | WorktreeError::LiveProcess(_))
    ));
    save_lane_owner_when_free(
        &context,
        &tree,
        &next,
        info.head,
        Some(&owner),
        true,
        &cancel(),
    )?;
    assert_eq!(load_owner(&tree)?, Some(next));
    Ok(())
}

#[test]
fn lane_owner_reassignment_holds_custody_and_preserves_adopted_owners() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "lane-owner".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let mut owner = WorktreeOwner {
        attempt_id: Some(provenant_core::AttemptId::from_bytes([4; 16])),
        operation_id: Some("first-owner".parse()?),
        branch: "lane-owner".into(),
        base: info.head.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
    };
    save_lane_owner_when_free(&context, &tree, &owner, info.head, None, false, &cancel())?;
    let state = context
        .project_state
        .open_verified_directory(&state_dir_name(&info.path))?;
    let held = state.lock(
        OsStr::new(WRITER_LOCK),
        LockMode::Exclusive,
        WaitBudget::nonblocking(context.clock.mono_now()),
        &cancel(),
        context.clock,
    )?;
    let next = WorktreeOwner {
        attempt_id: None,
        operation_id: Some("next-owner".parse()?),
        ..owner.clone()
    };
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &next,
            info.head,
            Some(&owner),
            true,
            &cancel()
        ),
        Err(WorktreeError::LockBusy)
    ));
    assert_eq!(load_owner(&tree)?, Some(owner.clone()));
    drop(held);
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &next,
            info.head,
            Some(&owner),
            false,
            &cancel()
        ),
        Err(WorktreeError::WriterBusy { holder }) if holder == owner.attempt_id
    ));
    assert_eq!(load_owner(&tree)?, Some(owner.clone()));
    let pending = WorktreeOwner {
        attempt_id: None,
        ..owner.clone()
    };
    save_owner(&tree, &pending)?;
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &next,
            info.head,
            Some(&pending),
            false,
            &cancel()
        ),
        Err(WorktreeError::WriterBusy { holder: None })
    ));
    assert_eq!(load_owner(&tree)?, Some(pending));
    save_owner(&tree, &owner)?;
    fs::write(info.path.join("dirty"), "keep")?;
    let error = save_lane_owner_when_free(
        &context,
        &tree,
        &next,
        info.head,
        Some(&owner),
        true,
        &cancel(),
    )
    .err()
    .ok_or("dirty worktree owner saved")?;
    assert!(matches!(&error, WorktreeError::Dirty(paths) if paths.contains("dirty")));
    assert!(error.to_string().contains("dirty"));
    fs::remove_file(info.path.join("dirty"))?;
    // Another writer moves the branch after the caller observed and validated
    // `info.head`: the stale observation must not be published.
    run_in_worktree(
        &fixture,
        &info.path,
        &["commit", "--allow-empty", "-m", "moved after observation"],
    )?;
    assert!(matches!(
        save_lane_owner_when_free(
            &context,
            &tree,
            &next,
            info.head,
            Some(&owner),
            true,
            &cancel()
        ),
        Err(WorktreeError::IdentityChanged)
    ));
    assert_eq!(load_owner(&tree)?, Some(owner.clone()));
    let head = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == info.path)
        .ok_or("missing lane")?
        .head;
    assert_ne!(head, info.head);
    save_lane_owner_when_free(&context, &tree, &next, head, Some(&owner), true, &cancel())?;
    let stale = WorktreeOwner {
        operation_id: Some("stale-owner".parse()?),
        ..next.clone()
    };
    assert!(matches!(
        save_lane_owner_when_free(&context, &tree, &stale, head, Some(&owner), true, &cancel()),
        Err(WorktreeError::IdentityChanged)
    ));
    assert_eq!(load_owner(&tree)?, Some(next.clone()));
    owner.origin = WorktreeOrigin::Adopted;
    owner.label = Some("manual".into());
    save_owner(&tree, &owner)?;
    assert!(
        save_lane_owner_when_free(&context, &tree, &next, head, Some(&owner), false, &cancel())
            .is_err()
    );
    assert_eq!(load_owner(&tree)?, Some(owner));
    Ok(())
}

#[test]
fn ignored_paths_are_accepted_and_dirty_paths_are_named_in_error() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.root.join(".git/info/exclude"),
        "target/\n.provenant/\n",
    )?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "ignored-test".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let owner = WorktreeOwner {
        attempt_id: Some(provenant_core::AttemptId::from_bytes([7; 16])),
        operation_id: Some("op-ignored".parse()?),
        branch: "ignored-test".into(),
        base: info.head.to_string(),
        host: provenant_core::HostId::from_bytes([8; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
    };

    // 1. Acceptance 1: A worktree containing only ignored paths (such as target/
    // or .provenant/scratch/graphs/) is accepted as clean.
    fs::create_dir_all(info.path.join("target/debug"))?;
    fs::write(info.path.join("target/debug/output.bin"), "binary")?;
    fs::create_dir_all(info.path.join(".provenant/scratch/graphs"))?;
    fs::write(
        info.path.join(".provenant/scratch/graphs/preset-1.json"),
        "{}",
    )?;
    save_lane_owner_when_free(&context, &tree, &owner, info.head, None, false, &cancel())?;
    assert_eq!(load_owner(&tree)?, Some(owner.clone()));

    // 2. Acceptance 2: A dirty worktree's error names its paths.
    let next_owner = WorktreeOwner {
        attempt_id: Some(provenant_core::AttemptId::from_bytes([9; 16])),
        operation_id: Some("op-dirty".parse()?),
        ..owner.clone()
    };
    fs::write(info.path.join("uncommitted.rs"), "fn uncommitted() {}")?;
    let error = save_lane_owner_when_free(
        &context,
        &tree,
        &next_owner,
        info.head,
        Some(&owner),
        true,
        &cancel(),
    )
    .err()
    .ok_or("dirty tree owner saved")?;
    match &error {
        WorktreeError::Dirty(paths) => {
            assert!(
                paths.contains("uncommitted.rs"),
                "expected path in dirty list: {paths}"
            );
        }
        other => panic!("expected WorktreeError::Dirty, got: {other}"),
    }
    let error_text = error.to_string();
    assert!(
        error_text.contains("worktree is dirty: uncommitted.rs"),
        "{error_text}"
    );
    let with_repair = error.with_repair();
    assert!(
        with_repair.contains("worktree is dirty: uncommitted.rs")
            && with_repair.contains("run `git -C PATH status`"),
        "{with_repair}"
    );

    // Verify up to 12 dirty paths listed and remaining counted.
    for n in 0..15 {
        fs::write(info.path.join(format!("dirty_{n:02}.txt")), "dirty")?;
    }
    let many_error = save_lane_owner_when_free(
        &context,
        &tree,
        &next_owner,
        info.head,
        Some(&owner),
        true,
        &cancel(),
    )
    .err()
    .ok_or("many dirty tree owner saved")?;
    match &many_error {
        WorktreeError::Dirty(paths) => {
            assert!(
                paths.contains("and 4 more"),
                "expected count of remaining paths: {paths}"
            );
        }
        other => panic!("expected WorktreeError::Dirty, got: {other}"),
    }

    Ok(())
}

#[test]
fn owned_cleanup_rechecks_regenerated_output_and_keep_paths() -> Result<(), Box<dyn Error>> {
    for keep in [false, true] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".git/info/exclude"), "build/\nsaved/\n")?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "regenerated".into(),
                base: None,
            },
            &cancel(),
        )?;
        let attempt = provenant_core::AttemptId::from_bytes([7; 16]);
        let tree = verify_worktree(&fixture.repo, &info.path)?;
        save_owner(
            &tree,
            &WorktreeOwner {
                attempt_id: Some(attempt),
                operation_id: Some("regenerated-create".parse()?),
                branch: "regenerated".into(),
                base: info.head.to_string(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: context.clock.wall_now()?,
                origin: WorktreeOrigin::Lane,
                label: None,
            },
        )?;
        let output = info.path.join("saved/new");
        fs::create_dir_all(info.path.join("build"))?;
        fs::write(info.path.join("build/old"), "initial output")?;
        let calls = std::cell::Cell::new(0);
        let terminal = |_: &Path| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                // Recreate ignored output after collection, before final removal.
                assert!(!info.path.join("build/old").exists());
                assert!(fs::create_dir_all(output.parent().unwrap_or(&info.path)).is_ok());
                assert!(fs::write(&output, "regenerated output").is_ok());
            }
            true
        };
        let kept = if keep {
            vec!["saved/new".into()]
        } else {
            vec![]
        };
        let policy = WorktreeGcPolicy {
            upstream: Some(info.head),
            terminal: &terminal,
            no_live_lanes: &|_| true,
            regenerable: &["build".into(), "saved".into()],
            keep: &kept,
            written_before_ns: i128::MAX,
            foreign_merged_clean: false,
            dry_run: false,
        };
        let result = prune_owned(
            &context,
            &info.path,
            attempt,
            info.head,
            info.head,
            &policy,
            &cancel(),
        );
        assert_eq!(
            calls.get(),
            if keep { 2 } else { 3 },
            "terminality is rechecked after output preparation"
        );
        if keep {
            assert!(matches!(result, Err(WorktreeError::IgnoredPaths(_))));
            assert_eq!(fs::read_to_string(&output)?, "regenerated output");
        } else {
            assert_eq!(result?.removed.len(), 1);
            assert!(!info.path.exists());
        }
    }
    Ok(())
}

#[test]
fn owned_cleanup_refuses_dirty_unmerged_and_adopted_trees() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let base = fixture.head()?;
    let info = create(
        &context,
        WorktreeCreate {
            branch: "owned".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(&fixture.repo, &info.path)?;
    let attempt = provenant_core::AttemptId::from_bytes([7; 16]);
    let mut owner = WorktreeOwner {
        attempt_id: Some(attempt),
        operation_id: Some("owned-create".parse()?),
        branch: "owned".into(),
        base: base.to_string(),
        host: provenant_core::HostId::from_bytes([3; 16]),
        created_at: context.clock.wall_now()?,
        origin: WorktreeOrigin::Lane,
        label: None,
    };
    save_owner(&tree, &owner)?;
    let policy = WorktreeGcPolicy {
        upstream: Some(base),
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable: &[],
        keep: &[],
        written_before_ns: i128::MAX,
        foreign_merged_clean: false,
        dry_run: false,
    };
    fs::write(info.path.join("dirty"), "keep")?;
    assert!(
        prune_owned(
            &context,
            &info.path,
            attempt,
            base,
            base,
            &policy,
            &cancel()
        )
        .is_err()
    );
    fs::remove_file(info.path.join("dirty"))?;
    owner.origin = WorktreeOrigin::Adopted;
    save_owner(&tree, &owner)?;
    assert!(
        prune_owned(
            &context,
            &info.path,
            attempt,
            base,
            base,
            &policy,
            &cancel()
        )
        .is_err()
    );
    owner.origin = WorktreeOrigin::Lane;
    save_owner(&tree, &owner)?;
    let denied = WorktreeGcPolicy {
        no_live_lanes: &|_| false,
        ..policy
    };
    assert!(
        prune_owned(
            &context,
            &info.path,
            attempt,
            base,
            base,
            &denied,
            &cancel()
        )
        .is_err()
    );
    assert!(info.path.exists());
    fixture.run(&[
        "-C",
        info.path.to_str().ok_or("path")?,
        "commit",
        "--allow-empty",
        "-m",
        "unmerged lane commit",
    ])?;
    let head = list(&context, &cancel())?
        .into_iter()
        .find(|lane| lane.path == info.path)
        .ok_or("lane missing")?
        .head;
    assert!(
        prune_owned(
            &context,
            &info.path,
            attempt,
            head,
            base,
            &policy,
            &cancel()
        )
        .is_err()
    );
    let pushed_policy = WorktreeGcPolicy {
        upstream: None,
        ..policy
    };
    let removed = prune_owned(
        &context,
        &info.path,
        attempt,
        head,
        head,
        &pushed_policy,
        &cancel(),
    )?;
    assert_eq!(removed.removed.len(), 1);
    assert!(!info.path.exists());
    assert!(removed.pruned_refs.is_empty());
    assert!(
        removed
            .retained
            .iter()
            .any(|retained| retained.branch == "owned")
    );
    assert!(read_branch_tips(&context, &cancel())?.contains_key(&GitRefName::branch("owned")?));
    Ok(())
}

impl WorkerObserver for FreeWorkers {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        roots
            .iter()
            .cloned()
            .map(|root| (root, WorkerState::Free))
            .collect()
    }
}

/// A worker observer that sees one named process inside every root.
pub(super) struct NamedLive;

impl WorkerObserver for NamedLive {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        roots
            .iter()
            .cloned()
            .map(|root| (root, WorkerState::Live))
            .collect()
    }

    fn live_process(&self, _root: &Path) -> Option<ObservedProcess> {
        Some(ObservedProcess {
            pid: 4242,
            command: Some("zsh".into()),
            kind: ProcessEvidenceKind::WorkingDirectory,
            cwd: None,
        })
    }
}

pub(super) struct UnknownWorkers;

#[test]
fn remove_registered_detached_worktree() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "detached".into(),
            base: None,
        },
        &cancel(),
    )?;
    run_in_worktree(&fixture, &info.path, &["checkout", "--detach"])?;
    let removed = remove(
        &context,
        WorktreeRemove::Name {
            name: "detached".into(),
            discard: false,
            integration: None,
            expected_head: Some(info.head),
        },
        &cancel(),
    )?;
    assert_eq!(removed.removed.len(), 1);
    assert_eq!(removed.removed[0].branch, None);
    assert_eq!(removed.removed[0].head, info.head);
    assert!(!info.path.exists());
    assert!(removed.pruned_refs.is_empty());
    assert!(read_branch_tips(&context, &cancel())?.contains_key(&GitRefName::branch("detached")?));
    Ok(())
}

#[test]
fn detached_removal_keeps_dirty_unmerged_and_live_work() -> Result<(), Box<dyn Error>> {
    for scenario in ["dirty", "unmerged", "live", "changed-head"] {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let mut context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "detached".into(),
                base: None,
            },
            &cancel(),
        )?;
        run_in_worktree(&fixture, &info.path, &["checkout", "--detach"])?;
        match scenario {
            "dirty" => fs::write(info.path.join("notes"), "preserve")?,
            "unmerged" | "changed-head" => run_in_worktree(
                &fixture,
                &info.path,
                &["commit", "--allow-empty", "-m", "detached work"],
            )?,
            "live" => context.workers = &NamedLive,
            _ => {}
        }
        let request = |discard| WorktreeRemove::Name {
            name: "detached".into(),
            discard,
            integration: None,
            expected_head: (scenario == "changed-head").then_some(info.head),
        };
        let error = remove(&context, request(false), &cancel())
            .err()
            .ok_or("removed protected tree")?;
        assert!(
            match scenario {
                "dirty" =>
                    matches!(error, WorktreeError::Dirty(ref paths) if paths.contains("notes")),
                "unmerged" => matches!(error, WorktreeError::DetachedUnmerged { .. }),
                "live" => matches!(error, WorktreeError::LiveProcess(_)),
                "changed-head" => matches!(error, WorktreeError::IdentityChanged),
                _ => false,
            },
            "{scenario}: {error}"
        );
        assert!(error.repair().is_some(), "{scenario}: {error}");
        assert!(info.path.exists());
        if scenario == "unmerged" {
            let removed = remove(&context, request(true), &cancel())?;
            assert_eq!(removed.removed.len(), 1);
            assert_ne!(removed.removed[0].head, info.head);
            assert!(removed.pruned_refs.is_empty());
            assert!(!info.path.exists());
        }
    }
    Ok(())
}

impl WorkerObserver for UnknownWorkers {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        roots
            .iter()
            .cloned()
            .map(|root| (root, WorkerState::Unknown))
            .collect()
    }
}

pub(super) struct TestContext<'a> {
    pub(super) fixture: &'a Fixture,
    pub(super) facts: RepoFacts,
    pub(super) binding: ProjectBinding,
    pub(super) project_state: Directory,
    pub(super) workers: FreeWorkers,
    pub(super) clock: SystemClock,
    pub(super) lock_budget: WaitBudget,
    pub(super) utility_budget: UtilityBudget,
    pub(super) mutation_budget: UtilityBudget,
}

impl<'a> TestContext<'a> {
    pub(super) fn new(fixture: &'a Fixture) -> Result<Self, Box<dyn Error>> {
        let binding_state = fixture._temp.path().join("bindings");
        fs::create_dir(&binding_state)?;
        let mut inputs =
            provenant_core::RootInputs::new(fixture.home.clone(), fixture.root.clone());
        inputs.state_dir = Some(binding_state);
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(fixture.repo.common_git_dir())?,
        )?;
        let state_root = fixture._temp.path().join("worktrees");
        fs::create_dir(&state_root)?;
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700))?;
        let state_path = state_root.join(binding.project_id().to_string());
        fs::create_dir(&state_path)?;
        fs::set_permissions(&state_path, fs::Permissions::from_mode(0o700))?;
        let project_state = Directory::open_anchor(&state_path)?;
        let clock = SystemClock;
        let lock_budget = WaitBudget::cli(clock.mono_now(), 5)?;
        Ok(Self {
            fixture,
            facts: RepoFacts::default(),
            binding,
            project_state,
            workers: FreeWorkers,
            clock,
            lock_budget,
            utility_budget: fixture.budget()?,
            mutation_budget: fixture.budget()?,
        })
    }

    pub(super) fn context(&self) -> WorktreeContext<'_> {
        WorktreeContext {
            home: &self.fixture.home,
            repo: &self.fixture.repo,
            binding: &self.binding,
            project_state: &self.project_state,
            executor: &self.fixture.executor,
            utility: &self.fixture.runtime,
            facts: &self.facts,
            workers: &self.workers,
            clock: &self.clock,
            utility_budget: self.utility_budget,
            mutation_budget: self.mutation_budget,
            lock_budget: self.lock_budget,
        }
    }
}

pub(super) fn cancel() -> CancelToken {
    CancelToken::new()
}

pub(super) fn run_in_worktree(
    fixture: &Fixture,
    path: &Path,
    args: &[&str],
) -> Result<(), Box<dyn Error>> {
    let env = fixture_env(&fixture.home, fixture._temp.path());
    let path = path.to_str().ok_or("non-UTF8 worktree path")?;
    let mut git_args = vec!["-C", path];
    git_args.extend_from_slice(args);
    run_fixture_git(
        &fixture.executor,
        &fixture.git,
        &fixture.root,
        &env,
        &git_args,
    )?;
    Ok(())
}

/// Runs `git worktree add` to completion, then reports it timed out: the
/// tree and branch exist, as when a slow checkout is killed part-way.
pub(super) struct TimesOutWorktreeAdd<'a>(&'a provenant_os::exec::SystemExecutor);

pub(super) struct ObservesFetch<'a> {
    executor: &'a dyn Executor,
    seen: std::sync::atomic::AtomicBool,
}

impl Executor for ObservesFetch<'_> {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
        if options.argv.iter().any(|arg| arg == "fetch") {
            self.seen.store(true, std::sync::atomic::Ordering::Relaxed);
            assert!(
                options
                    .argv
                    .iter()
                    .any(|arg| arg == "--no-auto-maintenance")
            );
        }
        self.executor.execute(options)
    }
}

impl Executor for TimesOutWorktreeAdd<'_> {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
        let adds = options.argv.iter().any(|arg| arg == "add");
        let mut outcome = self.0.execute(options)?;
        if adds {
            outcome.timed_out = true;
            outcome.termination = Termination::Signal(15);
        }
        Ok(outcome)
    }
}

pub(super) fn branch_exists(fixture: &Fixture, name: &str) -> bool {
    fixture
        .run(&["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")])
        .is_ok()
}

pub(super) fn gc_merged_worktree(
    fixture: &Fixture,
    context: &WorktreeContext<'_>,
    name: &str,
) -> Result<WorktreeInfo, Box<dyn Error>> {
    let created = create(
        context,
        WorktreeCreate {
            branch: name.to_owned(),
            base: None,
        },
        &cancel(),
    )?;
    run_in_worktree(
        fixture,
        &created.path,
        &["commit", "--allow-empty", "-m", name],
    )?;
    fixture.run(&["merge", "--no-ff", name, "-m", "merge GC fixture"])?;
    list(context, &cancel())?
        .into_iter()
        .find(|lane| lane.path == created.path)
        .ok_or_else(|| "missing GC fixture worktree".into())
}

#[test]
fn ignored_paths_refusal_names_regenerable_setting() {
    let message = WorktreeError::IgnoredPaths("target, .build".into()).with_repair();
    assert!(message.contains("target, .build"));
    assert!(message.contains("gc.projects.<id>.regenerable"));
    assert!(message.contains("provenant fabric identity"));
}

#[test]
fn gc_idle_output_requires_ignored_idle_and_free_writer() -> Result<(), Box<dyn Error>> {
    for scenario in ["idle", "recent", "lease", "tracked", "untracked", "lane"] {
        let fixture = Fixture::new()?;
        fs::write(
            fixture.root.join(".gitignore"),
            if scenario == "untracked" {
                ".env\n"
            } else {
                "target/\n.env\n"
            },
        )?;
        if scenario == "tracked" {
            fs::create_dir(fixture.root.join("target"))?;
            fs::write(fixture.root.join("target/source"), "tracked")?;
            fixture.run(&["add", "-f", "target/source"])?;
        }
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "output rules"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let created = create(
            &context,
            WorktreeCreate {
                branch: "idle-output".into(),
                base: None,
            },
            &cancel(),
        )?;
        fs::create_dir_all(created.path.join("target"))?;
        fs::write(created.path.join("target/cache"), "output")?;
        let lanes = list(&context, &cancel())?;
        if scenario == "untracked" {
            // Call the pruning owner directly so the early dirty-lane gate
            // cannot mask the non-ignored output protection.
            let entries = read_worktrees(&context, &cancel())?;
            let entry = entries
                .iter()
                .find(|entry| entry.path == created.path)
                .ok_or("worktree")?;
            let locked = lock_worktree_for_mutation(&context, entry, &cancel())?;
            let refused = gc::prepare_removal(
                &context,
                locked.verified.repo(),
                &["target".into()],
                &[],
                &cancel(),
            );
            assert!(
                matches!(refused, Err(WorktreeError::IgnoredPaths(ref paths)) if paths.contains("target"))
            );
            assert_eq!(
                fs::read_to_string(created.path.join("target/cache"))?,
                "output"
            );
        }
        let state = context
            .project_state
            .open_verified_directory(&state_dir_name(&created.path))?;
        let lease = if scenario == "lease" {
            Some(state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?)
        } else {
            None
        };
        let report = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                foreign_merged_clean: false,
                upstream: None,
                terminal: &|_| false,
                no_live_lanes: &|_| scenario != "lane",
                regenerable: &["target".into()],
                keep: &[],
                written_before_ns: if scenario == "recent" { 0 } else { i128::MAX },
                dry_run: false,
            },
            &cancel(),
        )?;
        assert_eq!(
            created.path.join("target/cache").exists(),
            scenario != "idle",
            "{scenario}"
        );
        assert_eq!(
            report.idle_output.iter().filter(|row| row.removed).count(),
            usize::from(scenario == "idle"),
            "{scenario}"
        );
        assert!(created.path.is_dir());
        drop(lease);
    }
    Ok(())
}

#[test]
fn gc_protects_nested_repositories_under_ignored_directories() -> Result<(), Box<dyn Error>> {
    for linked in [false, true] {
        for parent_output in [false, true] {
            let fixture = Fixture::new()?;
            fs::write(fixture.root.join(".gitignore"), ".claude/\ntarget/\n")?;
            fixture.run(&["add", ".gitignore"])?;
            fixture.run(&["commit", "-m", "ignore native worktrees"])?;
            let test_context = TestContext::new(&fixture)?;
            let context = test_context.context();
            let created = create(
                &context,
                WorktreeCreate {
                    branch: "nested-host".into(),
                    base: None,
                },
                &cancel(),
            )?;
            let parent = created.path.join(if parent_output {
                "target"
            } else {
                ".claude/worktrees"
            });
            fs::create_dir_all(&parent)?;
            let nested = parent.join("native");
            if linked {
                fixture.run(&[
                    "worktree",
                    "add",
                    "-b",
                    "nested-native",
                    nested.to_str().ok_or("path")?,
                ])?;
            } else {
                fixture.run(&[
                    "clone",
                    fixture.root.to_str().ok_or("root")?,
                    nested.to_str().ok_or("path")?,
                ])?;
            }
            fs::create_dir(nested.join("build"))?;
            fs::write(nested.join("build/source"), "committed")?;
            run_in_worktree(&fixture, &nested, &["add", "-f", "build/source"])?;
            run_in_worktree(
                &fixture,
                &nested,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.test",
                    "commit",
                    "-m",
                    "nested source",
                ],
            )?;
            fs::write(nested.join("build/source"), "uncommitted edit")?;
            let report = collect(
                &context,
                &list(&context, &cancel())?,
                &WorktreeGcPolicy {
                    upstream: None,
                    terminal: &|_| false,
                    no_live_lanes: &|_| true,
                    regenerable: &["build".into(), "target".into()],
                    keep: &[],
                    written_before_ns: i128::MAX,
                    dry_run: false,
                    foreign_merged_clean: false,
                },
                &cancel(),
            )?;
            assert_eq!(
                fs::read_to_string(nested.join("build/source"))?,
                "uncommitted edit"
            );
            assert!(report.idle_output.iter().all(|entry| !entry.removed));
        }
    }
    Ok(())
}

#[test]
fn remove_declared_ignored_output_but_names_undeclared_env() -> Result<(), Box<dyn Error>> {
    for secret in [false, true] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), "target/\n.env\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "output rules"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let created = create(
            &context,
            WorktreeCreate {
                branch: "remove-output".into(),
                base: None,
            },
            &cancel(),
        )?;
        fs::create_dir(created.path.join("target"))?;
        fs::write(created.path.join("target/artifact"), "output")?;
        if secret {
            fs::write(created.path.join(".env"), "private")?;
        }
        let result = remove_with_output(
            &context,
            WorktreeRemove::Name {
                name: "remove-output".into(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancel(),
            &provenant_config::ProjectGcSettings {
                regenerable: vec!["target".into()],
                ..Default::default()
            },
        );
        if secret {
            let error = result.err().ok_or("removed ignored secret")?;
            assert!(error.to_string().contains(".env"), "{error}");
            assert_eq!(fs::read_to_string(created.path.join(".env"))?, "private");
            assert!(
                created.path.join("target/artifact").exists(),
                "preview must refuse before pruning"
            );
        } else {
            assert_eq!(result?.removed.len(), 1);
            assert!(!created.path.exists());
        }
    }
    Ok(())
}

#[test]
fn gc_foreign_merged_clean_is_opt_in_immediate_and_requires_complete_census()
-> Result<(), Box<dyn Error>> {
    for scenario in [
        "default",
        "recent",
        "unknown",
        "unmerged",
        "live-lane",
        "dirty",
        "preview",
        "apply",
    ] {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let mut context = test_context.context();
        let path = fixture._temp.path().join("native");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "native",
            path.to_str().ok_or("path")?,
        ])?;
        if scenario == "unmerged" {
            run_in_worktree(
                &fixture,
                &path,
                &["commit", "--allow-empty", "-m", "unmerged"],
            )?;
        }
        if scenario == "dirty" {
            fs::write(path.join("notes"), "keep")?;
        }
        let unknown = UnknownWorkers;
        if scenario == "unknown" {
            context.workers = &unknown;
        }
        let lanes = list(&context, &cancel())?;
        let report = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                foreign_merged_clean: scenario != "default",
                upstream: Some(fixture.head()?),
                terminal: &|_| false,
                no_live_lanes: &|_| scenario != "live-lane",
                regenerable: &[],
                keep: &[],
                written_before_ns: if scenario == "recent" { 0 } else { i128::MAX },
                dry_run: scenario == "preview",
            },
            &cancel(),
        )?;
        assert_eq!(
            report.worktrees.removed.len(),
            usize::from(matches!(scenario, "preview" | "apply" | "recent")),
            "{scenario}: {report:?}"
        );
        let expected = match scenario {
            "unknown" | "live-lane" => Some(RetainReason::UnknownWriter),
            "unmerged" => Some(RetainReason::NotMerged),
            "dirty" => Some(RetainReason::Dirty),
            _ => None,
        };
        assert_eq!(
            report.worktrees.retained,
            expected
                .into_iter()
                .map(|reason| RetainedBranch {
                    branch: "native".into(),
                    reason,
                    detail: (scenario == "unknown")
                        .then(|| "worker census did not prove the tree free".into()),
                })
                .collect::<Vec<_>>(),
            "{scenario}"
        );
        assert_eq!(
            path.exists(),
            !matches!(scenario, "apply" | "recent"),
            "{scenario}"
        );
        assert_eq!(
            context
                .project_state
                .open_verified_directory(&state_dir_name(&path))
                .is_ok(),
            !matches!(scenario, "default" | "preview" | "live-lane" | "unmerged"),
            "exclusion state after {scenario}"
        );
        assert_eq!(
            branch_exists(&fixture, "native"),
            !matches!(scenario, "apply" | "recent")
        );
        if scenario == "preview" {
            assert_eq!(report.worktrees.pruned_refs, vec!["native"]);
        }
    }
    Ok(())
}

#[test]
fn gc_output_pruning_holds_writer_but_not_repository_lock() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&fixture, &context, "gc-output-lock")?;
    let target = lane.path.join("target");
    fs::create_dir(&target)?;
    fs::write(target.join("artifact"), "output")?;
    lane.dirty = WorktreeCondition::Ignored;
    let state = context
        .project_state
        .open_verified_directory(&state_dir_name(&lane.path))?;
    let observations = std::cell::Cell::new(None);
    let calls = std::cell::Cell::new(0);
    let terminal = |_: &Path| {
        let count = calls.get() + 1;
        calls.set(count);
        if count == 1 {
            let repository_free = context
                .project_state
                .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                .is_ok();
            let writer_held = matches!(
                state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
                Err(FsError::LockTimeout { .. })
            );
            observations.set(Some((repository_free, writer_held)));
        }
        true
    };
    let report = collect(
        &context,
        &[lane],
        &WorktreeGcPolicy {
            foreign_merged_clean: false,
            upstream: None,
            terminal: &terminal,
            no_live_lanes: &|_| true,
            regenerable: &["target".to_owned()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert_eq!(observations.get(), Some((true, true)));
    assert!(!target.exists());
    assert_eq!(report.output.len(), 1);
    assert!(report.output[0].removed);
    Ok(())
}

#[test]
fn gc_foreign_rechecks_changes_before_removal() -> Result<(), Box<dyn Error>> {
    for change in ["ignored", "head"] {
        let fixture = Fixture::new()?;
        fs::write(fixture.root.join(".gitignore"), ".env\n")?;
        fixture.run(&["add", ".gitignore"])?;
        fixture.run(&["commit", "-m", "ignore local environment"])?;
        let test_context = TestContext::new(&fixture)?;
        let context = test_context.context();
        let path = fixture._temp.path().join("native-recheck");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "native-recheck",
            path.to_str().ok_or("path")?,
        ])?;
        let lanes = list(&context, &cancel())?;
        let changed = std::cell::Cell::new(false);
        let calls = std::cell::Cell::new(0);
        let no_live = |_: &Path| {
            let count = calls.get() + 1;
            calls.set(count);
            let repository_held = context
                .project_state
                .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                .is_err();
            if !changed.get()
                && ((change == "ignored" && repository_held) || (change == "head" && count == 3))
            {
                changed.set(true);
                if change == "ignored" {
                    fs::write(path.join(".env"), "new secret").unwrap();
                } else {
                    run_in_worktree(
                        &fixture,
                        &path,
                        &["commit", "--allow-empty", "-m", "concurrent work"],
                    )
                    .unwrap();
                }
            }
            true
        };
        let report = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                upstream: Some(fixture.head()?),
                terminal: &|_| false,
                no_live_lanes: &no_live,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: false,
                foreign_merged_clean: true,
            },
            &cancel(),
        )?;
        assert!(changed.get());
        assert!(path.is_dir());
        assert!(report.worktrees.removed.is_empty());
        assert_eq!(
            report.worktrees.retained,
            vec![RetainedBranch {
                branch: "native-recheck".into(),
                reason: if change == "ignored" {
                    RetainReason::Dirty
                } else {
                    RetainReason::Changed
                },
                detail: None,
            }],
            "{report:?}"
        );
        if change == "ignored" {
            assert_eq!(fs::read_to_string(path.join(".env"))?, "new secret");
        }
    }
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)] // A real child provides the cwd observation fixture.
pub(super) fn gc_foreign_retains_a_process_with_cwd_inside_the_worktree()
-> Result<(), Box<dyn Error>> {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    struct CwdWorker(ProcessFact);
    impl WorkerObserver for CwdWorker {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            let mut states = roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Free))
                .collect();
            match observe_same_process(&SystemProcessObserver, &SystemCwdObserver, &self.0) {
                NativeCwd::Path(path) => {
                    record_live_path(&mut states, &path);
                }
                _ => {
                    for state in states.values_mut() {
                        *state = WorkerState::Unknown;
                    }
                }
            }
            states
        }
    }
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let mut context = test_context.context();
    let path = fixture._temp.path().join("native-cwd");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "native-cwd",
        path.to_str().ok_or("path")?,
    ])?;
    fs::create_dir(path.join("src"))?;
    // Obtain a clean listing before the native process starts.
    let lanes = list(&context, &cancel())?;
    let child = Child(
        std::process::Command::new("/bin/sh")
            .args(["-c", "read line"])
            .current_dir(path.join("src"))
            .stdin(std::process::Stdio::piped())
            .spawn()?,
    );
    let ProcessObservation::Present(fact) = SystemProcessObserver.observe(i64::from(child.0.id()))
    else {
        return Err("child process unobservable".into());
    };
    let worker = CwdWorker(fact);
    assert_eq!(
        worker.observe_roots(std::slice::from_ref(&path)).get(&path),
        Some(&WorkerState::Live)
    );
    context.workers = &worker;
    let report = collect(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            upstream: Some(fixture.head()?),
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
            foreign_merged_clean: true,
        },
        &cancel(),
    )?;
    assert_eq!(
        report.worktrees.retained,
        vec![RetainedBranch {
            branch: "native-cwd".into(),
            reason: RetainReason::LiveWriter,
            detail: None,
        }]
    );
    assert!(report.worktrees.removed.is_empty());
    assert!(path.is_dir());
    Ok(())
}

#[test]
pub(super) fn gc_foreign_without_state_holds_writer_through_final_recheck()
-> Result<(), Box<dyn Error>> {
    for scenario in [
        "remove",
        "final-refusal",
        "contended-refusal",
        "cancelled-refusal",
    ] {
        let refuse = scenario != "remove";
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let mut context = test_context.context();
        let clock = provenant_core::TestClock::new(
            context.clock.wall_now()?,
            provenant_core::MonoTime::from_micros(0),
        );
        context.clock = &clock;
        context.lock_budget = WaitBudget::cli(clock.mono_now(), 1)?;
        let path = fixture._temp.path().join("native-no-state");
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            "native-no-state",
            path.to_str().ok_or("path")?,
        ])?;
        let lanes = list(&context, &cancel())?;
        let state_name = state_dir_name(&path);
        assert!(
            context
                .project_state
                .open_verified_directory(&state_name)
                .is_err()
        );
        let final_writer_held = std::cell::Cell::new(false);
        let holder = std::cell::RefCell::new(None);
        let collection_cancel = cancel();
        let no_live = |_: &Path| {
            if scenario == "contended-refusal"
                && let Ok(state) = context.project_state.open_verified_directory(&state_name)
                && matches!(
                    state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
                    Err(FsError::LockTimeout { .. })
                )
            {
                clock
                    .wait_until(provenant_core::MonoTime::from_micros(10_000_000))
                    .unwrap();
                let repository = context
                    .project_state
                    .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                    .unwrap();
                holder.replace(Some(repository));
                return false;
            }
            if matches!(
                context
                    .project_state
                    .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive),
                Err(FsError::LockTimeout { .. })
            ) {
                let Ok(state) = context.project_state.open_verified_directory(&state_name) else {
                    // A concurrent test's fork can briefly inherit the
                    // repository lock before this call establishes state.
                    return true;
                };
                final_writer_held.set(matches!(
                    state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive),
                    Err(FsError::LockTimeout { .. })
                ));
            }
            if scenario == "cancelled-refusal" && final_writer_held.get() {
                collection_cancel.cancel();
            }
            !refuse || !final_writer_held.get()
        };
        let report = collect(
            &context,
            &lanes,
            &WorktreeGcPolicy {
                upstream: Some(fixture.head()?),
                terminal: &|_| false,
                no_live_lanes: &no_live,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: false,
                foreign_merged_clean: true,
            },
            &collection_cancel,
        )?;
        if refuse {
            assert_eq!(
                report.worktrees.retained,
                vec![RetainedBranch {
                    branch: "native-no-state".into(),
                    reason: RetainReason::UnknownWriter,
                    detail: None,
                }]
            );
            assert!(path.is_dir());
        } else {
            assert_eq!(
                report
                    .worktrees
                    .removed
                    .iter()
                    .map(|tree| &tree.path)
                    .collect::<Vec<_>>(),
                vec![&path],
                "{report:?}"
            );
            assert!(!path.exists());
        }
        if let Some(holder) = holder.into_inner() {
            drop(holder);
        } else {
            assert!(final_writer_held.get());
        }
        assert!(
            context
                .project_state
                .open_verified_directory(&state_name)
                .is_ok(),
            "exclusion slot must remain after {scenario}"
        );
    }
    Ok(())
}

#[test]
fn gc_foreign_prunes_under_writer_then_locks_repository() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&fixture, &context, "foreign-output-lock")?;
    fs::create_dir(lane.path.join("target"))?;
    fs::write(lane.path.join("target/artifact"), "output")?;
    lane.writer = WriterState::Unknown;
    let state = context
        .project_state
        .open_verified_directory(&state_dir_name(&lane.path))?;
    let observations = std::cell::RefCell::new(Vec::new());
    let no_live_lanes = |_: &Path| {
        if state
            .try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
            .is_err()
        {
            observations.borrow_mut().push(
                context
                    .project_state
                    .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                    .is_ok(),
            );
        }
        true
    };
    let report = collect(
        &context,
        &[lane],
        &WorktreeGcPolicy {
            upstream: Some(fixture.head()?),
            terminal: &|_| false,
            no_live_lanes: &no_live_lanes,
            regenerable: &["target".into()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
            foreign_merged_clean: true,
        },
        &cancel(),
    )?;
    assert_eq!(report.worktrees.removed.len(), 1, "{report:?}");
    let observed = observations.borrow();
    assert_eq!(observed.first(), Some(&true));
    assert_eq!(observed.last(), Some(&false));
    Ok(())
}

#[test]
fn gc_failed_prune_retains_once_and_continues() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let first = gc_merged_worktree(&fixture, &context, "gc-prune-failed")?;
    let second = gc_merged_worktree(&fixture, &context, "gc-prune-next")?;
    let calls = std::cell::Cell::new(0);
    let terminal = |path: &Path| {
        if path == first.path {
            calls.set(calls.get() + 1);
            false
        } else {
            true
        }
    };
    let report = collect(
        &context,
        &[first.clone(), second.clone()],
        &WorktreeGcPolicy {
            foreign_merged_clean: false,
            upstream: Some(fixture.head()?),
            terminal: &terminal,
            no_live_lanes: &|_| true,
            regenerable: &["target".to_owned()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert!(
        calls.get() >= 2,
        "prune and removal both recheck terminality"
    );
    assert!(first.path.is_dir());
    assert!(branch_exists(&fixture, "gc-prune-failed"));
    assert_eq!(report.worktrees.retained.len(), 1, "{report:?}");
    assert_eq!(report.worktrees.retained[0].branch, "gc-prune-failed");
    assert_eq!(report.worktrees.removed.len(), 1);
    assert_eq!(report.worktrees.removed[0].path, second.path);
    assert_eq!(report.worktrees.pruned_refs, vec!["gc-prune-next"]);
    assert!(!second.path.exists());
    Ok(())
}

#[test]
fn gc_reclaims_unmanaged_output_but_keeps_worktree() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let unmanaged = fixture._temp.path().join("unmanaged");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "gc-unmanaged",
        unmanaged.to_str().ok_or("non-UTF8 worktree path")?,
    ])?;
    run_in_worktree(
        &fixture,
        &unmanaged,
        &["commit", "--allow-empty", "-m", "gc-unmanaged"],
    )?;
    fixture.run(&["merge", "--no-ff", "gc-unmanaged", "-m", "merge GC fixture"])?;
    let target = unmanaged.join("target");
    fs::create_dir(&target)?;
    fs::write(target.join("artifact"), "keep")?;
    let canonical = canonicalize(&unmanaged)?;
    let mut lane = list(&context, &cancel())?
        .into_iter()
        .find(|lane| lane.path == canonical)
        .ok_or("missing unmanaged fixture worktree")?;
    lane.dirty = WorktreeCondition::Ignored;
    let report = collect(
        &context,
        &[lane],
        &WorktreeGcPolicy {
            foreign_merged_clean: false,
            upstream: Some(fixture.head()?),
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &["target".to_owned()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert!(!target.join("artifact").exists());
    assert!(unmanaged.is_dir());
    assert!(branch_exists(&fixture, "gc-unmanaged"));
    assert!(report.worktrees.removed.is_empty());
    assert!(report.worktrees.retained.is_empty());
    assert!(report.output.is_empty());
    assert_eq!(report.idle_output.len(), 1);
    assert_eq!(report.idle_output[0].path, target);
    assert!(report.idle_output[0].removed);
    Ok(())
}

#[test]
fn gc_merged_tree_previews_and_removes_recent_declared_output() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "target/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore output"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&fixture, &context, "gc-recent-merged")?;
    fs::create_dir(lane.path.join("target"))?;
    fs::write(lane.path.join("target/artifact"), "recent")?;
    lane.dirty = WorktreeCondition::Ignored;
    let settings = WorktreeGcPolicy {
        upstream: Some(fixture.head()?),
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable: &["target".into()],
        keep: &[],
        written_before_ns: 0,
        dry_run: true,
        foreign_merged_clean: false,
    };
    let preview = collect(&context, &[lane.clone()], &settings, &cancel())?;
    assert_eq!(preview.worktrees.removed.len(), 1);
    assert!(branch_exists(&fixture, "gc-recent-merged"));
    assert!(lane.path.join("target/artifact").exists());
    let applied = collect(
        &context,
        &[lane.clone()],
        &WorktreeGcPolicy {
            dry_run: false,
            ..settings
        },
        &cancel(),
    )?;
    assert_eq!(applied.worktrees.removed.len(), 1);
    assert!(!lane.path.exists());
    assert!(!branch_exists(&fixture, "gc-recent-merged"));
    Ok(())
}

#[test]
fn gc_merge_proof_requires_containment_in_the_pinned_default() -> Result<(), Box<dyn Error>> {
    // A concurrent test's fork can briefly inherit a just-released lock
    // descriptor; rebuild the whole fixture rather than weaken zero-wait locks.
    for _ in 0..5 {
        match merge_proof_scenario() {
            Err(error) if format!("{error:?}").contains("LockTimeout") => {}
            result => return result,
        }
    }
    merge_proof_scenario()
}

fn merge_proof_scenario() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let new_lane = |name: &str| -> Result<WorktreeInfo, Box<dyn Error>> {
        let created = create(
            &context,
            WorktreeCreate {
                branch: name.to_owned(),
                base: None,
            },
            &cancel(),
        )?;
        fs::write(created.path.join(format!("{name}.txt")), name)?;
        run_in_worktree(&fixture, &created.path, &["add", "."])?;
        run_in_worktree(&fixture, &created.path, &["commit", "-m", name])?;
        list(&context, &cancel())?
            .into_iter()
            .find(|lane| lane.path == created.path)
            .ok_or_else(|| "missing lane".into())
    };
    let batch = new_lane("proof-batch")?;
    let via_batch = new_lane("proof-via-batch")?;
    let local_only = new_lane("proof-local-only")?;
    let squashed = new_lane("proof-squashed")?;
    let unmerged = new_lane("proof-unmerged")?;
    let advanced = new_lane("proof-advanced")?;
    // lane -> batch preserves ancestry; the batch itself lands later.
    run_in_worktree(
        &fixture,
        &batch.path,
        &[
            "merge",
            "--no-ff",
            "proof-via-batch",
            "-m",
            "lane into batch",
        ],
    )?;
    run_in_worktree(
        &fixture,
        &batch.path,
        &["merge", "--no-ff", "proof-local-only", "-m", "local lane"],
    )?;
    // A pin taken before the batch lands: the batch is merged only locally.
    let stale_pin = fixture.head()?;
    fixture.run(&["merge", "--no-ff", "proof-batch", "-m", "land batch"])?;
    fixture.run(&["merge", "--squash", "proof-squashed"])?;
    fixture.run(&["commit", "-m", "squash lane"])?;
    fixture.run(&["merge", "--no-ff", "proof-advanced", "-m", "land advanced"])?;
    let pin = fixture.head()?;
    // The tip moves after the lane list was read.
    run_in_worktree(
        &fixture,
        &advanced.path,
        &["commit", "--allow-empty", "-m", "after listing"],
    )?;
    let policy = |upstream, dry_run| WorktreeGcPolicy {
        foreign_merged_clean: false,
        upstream: Some(upstream),
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable: &[],
        keep: &[],
        written_before_ns: 0,
        dry_run,
    };
    let local = collect(
        &context,
        &[via_batch.clone(), local_only.clone()],
        &policy(stale_pin, false),
        &cancel(),
    )?;
    assert!(local.worktrees.removed.is_empty(), "{local:?}");
    assert!(
        local
            .worktrees
            .retained
            .iter()
            .all(|retained| matches!(retained.reason, RetainReason::NotMerged))
    );
    assert!(via_batch.path.is_dir() && branch_exists(&fixture, "proof-via-batch"));
    let lanes = [via_batch.clone(), squashed, unmerged, advanced.clone()];
    let preview = collect(&context, &lanes, &policy(pin, true), &cancel())?;
    let mut previewed = preview.worktrees.pruned_refs.clone();
    previewed.sort();
    assert_eq!(previewed, ["proof-via-batch"], "{preview:?}");
    assert!(branch_exists(&fixture, "proof-via-batch"));
    let applied = collect(&context, &lanes, &policy(pin, false), &cancel())?;
    let retained: BTreeMap<_, _> = applied
        .worktrees
        .retained
        .iter()
        .map(|retained| (retained.branch.as_str(), &retained.reason))
        .collect();
    assert!(!via_batch.path.exists());
    assert!(!branch_exists(&fixture, "proof-via-batch"));
    for name in ["proof-squashed", "proof-unmerged", "proof-advanced"] {
        assert!(branch_exists(&fixture, name), "{name}");
        assert!(retained.contains_key(name), "{name}: {applied:?}");
    }
    assert!(matches!(
        retained["proof-squashed"],
        RetainReason::NotMerged
    ));
    assert!(matches!(
        retained["proof-unmerged"],
        RetainReason::NotMerged
    ));
    // Advanced after the listing: the expected head no longer matches.
    assert!(matches!(retained["proof-advanced"], RetainReason::Changed));
    assert!(advanced.path.is_dir());
    Ok(())
}

#[test]
fn gc_managed_fallback_keeps_fresh_trees_at_the_tip_or_first_parent() -> Result<(), Box<dyn Error>>
{
    // Two ways a managed tree reaches the foreign fallback: no terminal lane,
    // or a writer reported unknown. Neither may bypass the managed protections.
    for entry in ["no-terminal-lane", "unknown-writer"] {
        for shape in ["at-tip", "first-parent"] {
            let fixture = Fixture::new()?;
            let test_context = TestContext::new(&fixture)?;
            let context = test_context.context();
            let name = format!("gc-fallback-{entry}-{shape}");
            let created = create(
                &context,
                WorktreeCreate {
                    branch: name.clone(),
                    base: None,
                },
                &cancel(),
            )?;
            if shape == "first-parent" {
                run_in_worktree(
                    &fixture,
                    &created.path,
                    &["commit", "--allow-empty", "-m", "lane"],
                )?;
                fixture.run(&["merge", "--ff-only", &name])?;
                fixture.run(&["commit", "--allow-empty", "-m", "main moves on"])?;
            }
            let mut lane = list(&context, &cancel())?
                .into_iter()
                .find(|lane| lane.path == created.path)
                .ok_or("missing lane")?;
            if entry == "unknown-writer" {
                lane.writer = WriterState::Unknown;
            }
            let report = collect(
                &context,
                &[lane],
                &WorktreeGcPolicy {
                    foreign_merged_clean: true,
                    upstream: Some(fixture.head()?),
                    terminal: &|_| entry == "unknown-writer",
                    no_live_lanes: &|_| true,
                    regenerable: &[],
                    keep: &[],
                    written_before_ns: i128::MAX,
                    dry_run: false,
                },
                &cancel(),
            )?;
            let expected = if shape == "at-tip" {
                RetainReason::AtIntegrationTip
            } else {
                RetainReason::NoUniqueCommit
            };
            assert_eq!(
                report.worktrees.retained,
                vec![RetainedBranch {
                    branch: name.clone(),
                    reason: expected,
                    detail: None,
                }],
                "{entry} {shape}: {report:?}"
            );
            assert!(report.worktrees.removed.is_empty(), "{entry} {shape}");
            assert!(created.path.is_dir() && branch_exists(&fixture, &name));
        }
    }
    Ok(())
}

/// Records whether the repository lock was held during each census.
struct LockProbeWorkers<'a> {
    project_state: &'a Directory,
    repository_held: std::sync::Mutex<Vec<bool>>,
}

impl WorkerObserver for LockProbeWorkers<'_> {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        let held = self
            .project_state
            .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
            .is_err();
        self.repository_held.lock().unwrap().push(held);
        roots
            .iter()
            .cloned()
            .map(|root| (root, WorkerState::Free))
            .collect()
    }
}

#[test]
fn gc_census_never_runs_under_the_repository_lock() -> Result<(), Box<dyn Error>> {
    for scenario in ["managed", "managed-preview", "foreign", "foreign-preview"] {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let mut context = test_context.context();
        let mut lane = gc_merged_worktree(&fixture, &context, "gc-census-lock")?;
        let probe = LockProbeWorkers {
            project_state: context.project_state,
            repository_held: Default::default(),
        };
        context.workers = &probe;
        let foreign = scenario.starts_with("foreign");
        if foreign {
            lane.writer = WriterState::Unknown;
        }
        let report = collect(
            &context,
            &[lane],
            &WorktreeGcPolicy {
                foreign_merged_clean: foreign,
                upstream: Some(fixture.head()?),
                terminal: &|_| !foreign,
                no_live_lanes: &|_| true,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run: scenario.ends_with("preview"),
            },
            &cancel(),
        )?;
        assert_eq!(report.worktrees.removed.len(), 1, "{scenario}: {report:?}");
        let observed = probe.repository_held.lock().unwrap().clone();
        assert!(!observed.is_empty(), "{scenario}");
        assert!(
            observed.iter().all(|held| !held),
            "{scenario}: census under the repository lock {observed:?}"
        );
    }
    Ok(())
}

#[test]
fn inventory_census_runs_after_repository_snapshot_lock_is_released() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let probe = LockProbeWorkers {
        project_state: context.project_state,
        repository_held: Default::default(),
    };
    context.workers = &probe;

    // `collect` and the CLI's project-GC discovery both obtain their candidate
    // set through this inventory path before applying GC policy.
    let inventory = list(&context, &cancel())?;
    assert!(!inventory.is_empty());
    assert_eq!(
        *probe.repository_held.lock().unwrap(),
        vec![false],
        "native census must run after the Git registration snapshot lock drops"
    );
    Ok(())
}

#[test]
fn locked_registration_stays_unknown_when_census_reports_live() -> Result<(), Box<dyn Error>> {
    struct LiveWorkers;
    impl WorkerObserver for LiveWorkers {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Live))
                .collect()
        }
    }

    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let path = fixture.root.join(".worktrees/locked-live");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "locked-live",
        path.to_str().ok_or("path")?,
    ])?;
    fixture.run(&["worktree", "lock", path.to_str().ok_or("path")?])?;
    context.workers = &LiveWorkers;

    let listed = list(&context, &cancel())?
        .into_iter()
        .find(|entry| entry.path == path)
        .ok_or("locked linked worktree missing from inventory")?;
    assert_eq!(listed.writer, WriterState::Unknown);
    Ok(())
}

/// Free until the `unknown_from`th call (1-based), then unobservable.
struct FlippingWorkers {
    calls: std::sync::atomic::AtomicUsize,
    unknown_from: usize,
}

impl WorkerObserver for FlippingWorkers {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let state = if call >= self.unknown_from {
            WorkerState::Unknown
        } else {
            WorkerState::Free
        };
        roots.iter().cloned().map(|root| (root, state)).collect()
    }

    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        let states = self.observe_roots(roots);
        let details = states
            .iter()
            .filter(|(_, state)| **state != WorkerState::Free)
            .map(|(root, _)| {
                (
                    root.clone(),
                    "process 968: cwd unreadable; policy_condition=approved daemon directory required; configure confinement.census_ignore_unreadable; path=\"/Library/Developer/CoreSimulator/Resources/bin/simdiskimaged\", team_id=platform".into(),
                )
            })
            .collect();
        (states, details)
    }
}

#[test]
fn removal_refreshes_session_after_output_preparation() -> Result<(), Box<dyn Error>> {
    struct AppearingAfterPreparation {
        artifact: PathBuf,
        calls: std::sync::atomic::AtomicUsize,
        complete_required: bool,
    }
    impl WorkerObserver for AppearingAfterPreparation {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let state = if self.artifact.exists() {
                WorkerState::Free
            } else {
                WorkerState::Live
            };
            roots.iter().cloned().map(|root| (root, state)).collect()
        }
        fn requires_complete_census(&self) -> bool {
            self.complete_required
        }
    }
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".git/info/exclude"), "target/\n")?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "late-worker".into(),
            base: None,
        },
        &cancel(),
    )?;
    let artifact = info.path.join("target/artifact");
    fs::create_dir(info.path.join("target"))?;
    fs::write(&artifact, "output")?;
    let observer = AppearingAfterPreparation {
        artifact: artifact.clone(),
        calls: Default::default(),
        complete_required: false,
    };
    let session = CensusSession::new(&observer, &test.clock);
    context.workers = &session;
    let result = remove_with_output(
        &context,
        WorktreeRemove::Name {
            name: "late-worker".into(),
            discard: false,
            integration: None,
            expected_head: None,
        },
        &cancel(),
        &provenant_config::ProjectGcSettings {
            regenerable: vec!["target".into()],
            ..Default::default()
        },
    );
    assert!(
        matches!(
            result,
            Err(WorktreeError::LiveWriter | WorktreeError::LiveProcess(_))
        ),
        "{result:?}"
    );
    assert!(
        !artifact.exists(),
        "output preparation must precede the late observation"
    );
    assert!(info.path.is_dir());
    assert!(observer.calls.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    Ok(())
}

#[test]
fn custody_refresh_rechecks_a_cached_free_observation() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Appearing(AtomicUsize);
    impl WorkerObserver for Appearing {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            let state = if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                WorkerState::Free
            } else {
                WorkerState::Live
            };
            roots.iter().cloned().map(|root| (root, state)).collect()
        }
        fn requires_complete_census(&self) -> bool {
            true
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let observer = Appearing(AtomicUsize::new(0));
    let session = CensusSession::new(&observer, &test.clock);
    assert_eq!(
        session.observe_roots(std::slice::from_ref(&fixture.root))[&fixture.root],
        WorkerState::Free
    );
    context.workers = &session;
    assert!(matches!(
        lifecycle::refresh_census_under_custody(&context, &fixture.root),
        Err(WorktreeError::LiveWriter | WorktreeError::LiveProcess(_))
    ));
    assert_eq!(observer.0.load(Ordering::SeqCst), 2);
    Ok(())
}

#[test]
fn removal_session_detects_worker_after_preview() -> Result<(), Box<dyn Error>> {
    struct Appearing {
        calls: std::sync::atomic::AtomicUsize,
        complete_required: bool,
    }
    impl WorkerObserver for Appearing {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let state = if call == 0 {
                WorkerState::Free
            } else {
                WorkerState::Live
            };
            roots.iter().cloned().map(|root| (root, state)).collect()
        }
        fn requires_complete_census(&self) -> bool {
            self.complete_required
        }
    }
    for complete_required in [false, true] {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let mut context = test.context();
        let info = create(
            &context,
            WorktreeCreate {
                branch: "worker-appears".into(),
                base: None,
            },
            &cancel(),
        )?;
        let observer = Appearing {
            calls: Default::default(),
            complete_required,
        };
        let session = CensusSession::new(&observer, &test.clock);
        assert_eq!(
            session.observe_roots(std::slice::from_ref(&info.path))[&info.path],
            WorkerState::Free
        );
        context.workers = &session;
        let error = remove(
            &context,
            WorktreeRemove::Name {
                name: "worker-appears".into(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancel(),
        )
        .err()
        .ok_or("removed occupied tree")?;
        assert!(
            matches!(
                error,
                WorktreeError::LiveWriter | WorktreeError::LiveProcess(_)
            ),
            "{error:?}"
        );
        assert!(info.path.is_dir());
        assert!(observer.calls.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    }
    Ok(())
}

#[test]
fn foreign_gc_session_requires_fresh_complete_census() -> Result<(), Box<dyn Error>> {
    struct BecomingUnknown(std::sync::atomic::AtomicUsize);
    impl WorkerObserver for BecomingUnknown {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            let call = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let state = if call == 0 {
                WorkerState::Free
            } else {
                WorkerState::Unknown
            };
            roots.iter().cloned().map(|root| (root, state)).collect()
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let path = fixture._temp.path().join("foreign-session");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "foreign-session",
        path.to_str().ok_or("path")?,
    ])?;
    let lanes = list(&context, &cancel())?;
    let observer = BecomingUnknown(Default::default());
    let session = CensusSession::new(&observer, &test.clock);
    assert_eq!(
        session.observe_roots(std::slice::from_ref(&path))[&path],
        WorkerState::Free
    );
    context.workers = &session;
    let report = collect(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            foreign_merged_clean: true,
            upstream: Some(fixture.head()?),
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert!(report.worktrees.removed.is_empty(), "{report:?}");
    assert!(path.is_dir());
    assert!(observer.0.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    Ok(())
}

#[test]
fn gc_session_detects_worker_after_preview() -> Result<(), Box<dyn Error>> {
    struct Appearing(std::sync::atomic::AtomicUsize);
    impl WorkerObserver for Appearing {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            let call = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let state = if call == 0 {
                WorkerState::Free
            } else {
                WorkerState::Live
            };
            roots.iter().cloned().map(|root| (root, state)).collect()
        }
    }
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let mut context = test_context.context();
    let lane = gc_merged_worktree(&fixture, &context, "gc-worker-appears")?;
    let observer = Appearing(Default::default());
    let session = CensusSession::new(&observer, &test_context.clock);
    assert_eq!(
        session.observe_roots(std::slice::from_ref(&lane.path))[&lane.path],
        WorkerState::Free
    );
    context.workers = &session;
    let report = collect(
        &context,
        std::slice::from_ref(&lane),
        &WorktreeGcPolicy {
            foreign_merged_clean: false,
            upstream: Some(fixture.head()?),
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert!(report.worktrees.removed.is_empty(), "{report:?}");
    assert!(lane.path.is_dir());
    assert!(observer.0.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    Ok(())
}

#[test]
fn gc_managed_removal_needs_a_fresh_free_census_under_custody() -> Result<(), Box<dyn Error>> {
    // Count the censuses of an uninterrupted removal, then make the last one
    // fail: a tree whose census turns unobservable after selection is kept.
    let mut total = usize::MAX;
    for unknown_from in [usize::MAX, 0] {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let mut context = test_context.context();
        let lane = gc_merged_worktree(&fixture, &context, "gc-census-flip")?;
        let upstream = fixture.head()?;
        let run = |context: &WorktreeContext<'_>| {
            collect(
                context,
                std::slice::from_ref(&lane),
                &WorktreeGcPolicy {
                    foreign_merged_clean: false,
                    upstream: Some(upstream),
                    terminal: &|_| true,
                    no_live_lanes: &|_| true,
                    regenerable: &[],
                    keep: &[],
                    written_before_ns: i128::MAX,
                    dry_run: false,
                },
                &cancel(),
            )
        };
        if total == usize::MAX {
            let counting = FlippingWorkers {
                calls: Default::default(),
                unknown_from,
            };
            let session = CensusSession::new(&counting, &test_context.clock);
            context.workers = &session;
            let report = run(&context)?;
            assert_eq!(report.worktrees.removed.len(), 1, "{report:?}");
            total = counting.calls.load(std::sync::atomic::Ordering::SeqCst);
            // The removal censuses come after the one that listed the lane.
            assert!(total >= 2, "{total}");
            continue;
        }
        let flipping = FlippingWorkers {
            calls: Default::default(),
            unknown_from: total,
        };
        let session = CensusSession::new(&flipping, &test_context.clock);
        context.workers = &session;
        let report = run(&context)?;
        assert!(report.worktrees.removed.is_empty(), "{report:?}");
        assert_eq!(
            report.worktrees.retained,
            vec![RetainedBranch {
                branch: "gc-census-flip".into(),
                reason: RetainReason::UnknownWriter,
                detail: Some("process 968: cwd unreadable; policy_condition=approved daemon directory required; configure confinement.census_ignore_unreadable; path=\"/Library/Developer/CoreSimulator/Resources/bin/simdiskimaged\", team_id=platform".into()),
            }]
        );
        assert!(lane.path.is_dir());
        assert!(branch_exists(&fixture, "gc-census-flip"));
    }
    Ok(())
}

#[test]
fn gc_preview_and_apply_both_retain_a_managed_tree_with_an_unknown_census()
-> Result<(), Box<dyn Error>> {
    for dry_run in [true, false] {
        let fixture = Fixture::new()?;
        let test_context = TestContext::new(&fixture)?;
        let mut context = test_context.context();
        let lane = gc_merged_worktree(&fixture, &context, "gc-census-unknown")?;
        let unknown = FlippingWorkers {
            calls: Default::default(),
            unknown_from: 0,
        };
        context.workers = &unknown;
        let report = collect(
            &context,
            std::slice::from_ref(&lane),
            &WorktreeGcPolicy {
                foreign_merged_clean: false,
                upstream: Some(fixture.head()?),
                terminal: &|_| true,
                no_live_lanes: &|_| true,
                regenerable: &[],
                keep: &[],
                written_before_ns: i128::MAX,
                dry_run,
            },
            &cancel(),
        )?;
        assert!(report.worktrees.removed.is_empty(), "{dry_run}: {report:?}");
        assert_eq!(
            report.worktrees.retained,
            vec![RetainedBranch {
                branch: "gc-census-unknown".into(),
                reason: RetainReason::UnknownWriter,
                detail: Some("process 968: cwd unreadable; policy_condition=approved daemon directory required; configure confinement.census_ignore_unreadable; path=\"/Library/Developer/CoreSimulator/Resources/bin/simdiskimaged\", team_id=platform".into()),
            }],
            "dry_run={dry_run}"
        );
        assert!(lane.path.is_dir());
    }
    Ok(())
}

#[test]
fn gc_dry_run_retains_merged_worktree_with_unknown_writer() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let lane = gc_merged_worktree(&fixture, &context, "gc-dry-unknown")?;
    let report = collect(
        &context,
        &[lane],
        &WorktreeGcPolicy {
            foreign_merged_clean: false,
            upstream: Some(fixture.head()?),
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
        },
        &cancel(),
    )?;
    assert_eq!(report.worktrees.retained.len(), 1);
    assert_eq!(
        report.worktrees.retained[0].reason,
        RetainReason::UnknownWriter
    );
    assert!(report.worktrees.removed.is_empty());
    Ok(())
}

#[test]
fn gc_dry_run_retains_preview_dirty_worktree() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".gitignore"), "cache/\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore cache"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let mut lane = gc_merged_worktree(&fixture, &context, "gc-dry-dirty")?;
    // Ignored content outside the regenerable output keeps the worktree
    // dirty once the preview has pruned.
    fs::create_dir(lane.path.join("cache"))?;
    fs::write(lane.path.join("cache/artifact"), "keep")?;
    lane.dirty = WorktreeCondition::Ignored;
    let report = collect(
        &context,
        &[lane.clone()],
        &WorktreeGcPolicy {
            foreign_merged_clean: false,
            upstream: Some(fixture.head()?),
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &["target".to_owned()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
        },
        &cancel(),
    )?;
    assert_eq!(report.worktrees.retained.len(), 1);
    assert_eq!(report.worktrees.retained[0].reason, RetainReason::Dirty);
    assert!(report.worktrees.removed.is_empty());
    assert!(lane.path.join("cache/artifact").is_file());
    Ok(())
}

/// Fail the first worktree-list read after a successful removal, precisely
/// where branch settlement rechecks other checkouts. Later reads succeed.
pub(super) struct FailsFirstGcSettlement<'a> {
    executor: &'a provenant_os::exec::SystemExecutor,
    removed: std::sync::atomic::AtomicBool,
    failed: std::sync::atomic::AtomicBool,
}

impl Executor for FailsFirstGcSettlement<'_> {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
        use std::sync::atomic::Ordering;
        let worktree = options.argv.iter().any(|arg| arg == "worktree");
        let remove = worktree && options.argv.iter().any(|arg| arg == "remove");
        let list = worktree && options.argv.iter().any(|arg| arg == "list");
        let mut outcome = self.executor.execute(options)?;
        if remove && outcome.termination == Termination::Exit(0) {
            self.removed.store(true, Ordering::SeqCst);
        } else if list
            && self.removed.load(Ordering::SeqCst)
            && !self.failed.swap(true, Ordering::SeqCst)
        {
            outcome.termination = Termination::Exit(1);
        }
        Ok(outcome)
    }
}

#[test]
fn gc_settlement_error_retains_ref_and_continues() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let mut context = test_context.context();
    let first = gc_merged_worktree(&fixture, &context, "gc-settle-failed")?;
    let second = gc_merged_worktree(&fixture, &context, "gc-settle-next")?;
    let faulty = FailsFirstGcSettlement {
        executor: &fixture.executor,
        removed: std::sync::atomic::AtomicBool::new(false),
        failed: std::sync::atomic::AtomicBool::new(false),
    };
    context.executor = &faulty;
    let report = collect(
        &context,
        &[first.clone(), second.clone()],
        &WorktreeGcPolicy {
            foreign_merged_clean: false,
            upstream: Some(fixture.head()?),
            terminal: &|_| true,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
        },
        &cancel(),
    )?;
    assert_eq!(
        report
            .worktrees
            .removed
            .iter()
            .map(|tree| &tree.path)
            .collect::<Vec<_>>(),
        vec![&first.path, &second.path]
    );
    assert_eq!(report.worktrees.retained.len(), 1);
    assert_eq!(report.worktrees.retained[0].branch, "gc-settle-failed");
    assert_eq!(report.worktrees.pruned_refs, vec!["gc-settle-next"]);
    assert!(branch_exists(&fixture, "gc-settle-failed"));
    assert!(!first.path.exists());
    assert!(!second.path.exists());
    Ok(())
}

#[test]
fn timed_out_add_rolls_back_its_checkout_and_new_branch() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let faulty = TimesOutWorktreeAdd(&fixture.executor);
    let mut context = test_context.context();
    context.executor = &faulty;
    let request = || WorktreeCreate {
        branch: "feature/slow".to_owned(),
        base: Some("HEAD".to_owned()),
    };
    let failed = create(&context, request(), &cancel());
    let Err(WorktreeError::CreateFailed { cause, rolled_back }) = failed else {
        return Err(format!("expected CreateFailed, got {failed:?}").into());
    };
    assert!(rolled_back);
    assert!(matches!(
        *cause,
        WorktreeError::Utility(UtilityError::TimedOut)
    ));
    assert!(
        !fixture
            .root
            .join(WORKTREE_ROOT)
            .join("feature-slow")
            .exists()
    );
    assert!(!branch_exists(&fixture, "feature/slow"));
    let admin = fixture.repo.common_git_dir().join("worktrees");
    assert!(!admin.exists() || fs::read_dir(admin)?.next().is_none());
    // The retry that the leftover state used to block now succeeds.
    let retried = create(&test_context.context(), request(), &cancel())?;
    assert!(retried.path.is_dir());
    Ok(())
}

#[test]
fn timed_out_add_keeps_a_branch_it_did_not_create() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "feature/existing"])?;
    let test_context = TestContext::new(&fixture)?;
    let faulty = TimesOutWorktreeAdd(&fixture.executor);
    let mut context = test_context.context();
    context.executor = &faulty;
    let failed = create(
        &context,
        WorktreeCreate {
            branch: "feature/existing".to_owned(),
            base: None,
        },
        &cancel(),
    );
    assert!(matches!(
        failed,
        Err(WorktreeError::CreateFailed {
            rolled_back: true,
            ..
        })
    ));
    assert!(branch_exists(&fixture, "feature/existing"));
    assert!(
        !fixture
            .root
            .join(WORKTREE_ROOT)
            .join("feature-existing")
            .exists()
    );
    Ok(())
}

#[test]
fn create_fetches_a_remote_base_with_a_main_only_refspec() -> Result<(), Box<dyn Error>> {
    let source = Fixture::new()?;
    source.run(&["branch", "-M", "main"])?;
    source.run(&["checkout", "-b", "chore/cleanup-code-sweep"])?;
    source.run(&["commit", "--allow-empty", "-m", "remote-only commit"])?;
    let base = source.head()?;
    source.run(&["checkout", "main"])?;
    let mut fixture = Fixture::new()?;
    let clone = fixture._temp.path().join("clone");
    fixture.run(&[
        "clone",
        "--single-branch",
        "--branch",
        "main",
        source.root.to_str().ok_or("non-UTF8 source")?,
        clone.to_str().ok_or("non-UTF8 clone")?,
    ])?;
    fixture.root = clone;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let refspec = fixture.run(&["config", "--get-all", "remote.origin.fetch"])?;
    assert_eq!(
        refspec.stdout.bytes,
        b"+refs/heads/main:refs/remotes/origin/main\n"
    );
    assert!(
        fixture
            .run(&["for-each-ref", "refs/remotes/origin/chore/"])?
            .stdout
            .bytes
            .is_empty()
    );
    let test_context = TestContext::new(&fixture)?;

    let observes_fetch = ObservesFetch {
        executor: &fixture.executor,
        seen: std::sync::atomic::AtomicBool::new(false),
    };
    let mut context = test_context.context();
    context.executor = &observes_fetch;
    let created = create(
        &context,
        WorktreeCreate {
            branch: "chore/cleanup-code-sweep-2".to_owned(),
            base: Some("origin/chore/cleanup-code-sweep".to_owned()),
        },
        &cancel(),
    )?;

    assert_eq!(created.head, base);
    assert!(
        observes_fetch
            .seen
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    let tracking = fixture.run(&[
        "for-each-ref",
        "refs/remotes/origin/chore/cleanup-code-sweep",
    ])?;
    assert!(tracking.stdout.bytes.is_empty());
    assert_eq!(
        fixture
            .run(&["config", "--get-all", "remote.origin.fetch"])?
            .stdout
            .bytes,
        refspec.stdout.bytes
    );
    Ok(())
}

#[test]
fn remote_base_uses_exact_advertisement_instead_of_tracking_ref() -> Result<(), Box<dyn Error>> {
    let source = Fixture::new()?;
    source.run(&["branch", "-M", "main"])?;
    let parent = source.head()?;
    source.run(&["commit", "--allow-empty", "-m", "advertised tip"])?;
    let expected = source.head()?;
    let mut fixture = Fixture::new()?;
    let clone = fixture._temp.path().join("clone");
    fixture.run(&[
        "clone",
        source.root.to_str().ok_or("non-UTF8 source")?,
        clone.to_str().ok_or("non-UTF8 clone")?,
    ])?;
    fixture.root = clone;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    fixture.run(&["config", "user.name", "Fixture"])?;
    fixture.run(&["config", "user.email", "fixture@example.invalid"])?;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    fixture.run(&["commit", "--allow-empty", "-m", "different tracking tip"])?;
    let wrong = fixture.head()?;
    assert_ne!(wrong, expected);
    fixture.run(&["update-ref", "refs/remotes/origin/main", &wrong.to_string()])?;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let test_context = TestContext::new(&fixture)?;
    for alias in [
        "remotes/origin/main",
        "origin",
        "@{u}",
        "main@{upstream}",
        "@{push}",
    ] {
        assert_eq!(
            pin_revision(
                &test_context.context(),
                &RepoFacts::default(),
                alias,
                &cancel()
            )?,
            expected,
            "{alias}"
        );
    }
    for expression in [
        "origin/main~0",
        "remotes/origin/main^0",
        "@{u}~0",
        "main@{upstream}^{commit}",
        "@{push}^0",
    ] {
        assert_eq!(
            pin_revision(
                &test_context.context(),
                &RepoFacts::default(),
                expression,
                &cancel()
            )?,
            expected,
            "{expression}"
        );
    }
    // Every spelling of a tracking root verifies against the advertisement
    // and applies its suffix to that, never to the stale local tip (whose
    // first parent is `expected`, not `parent`).
    for root in [
        "origin/main",
        "remotes/origin/main",
        "refs/remotes/origin/main",
        "origin/HEAD",
        "remotes/origin/HEAD",
        "refs/remotes/origin/HEAD",
        "origin",
        "@{u}",
        "@{push}",
    ] {
        for (suffix, oid) in [
            ("", expected),
            ("~1", parent),
            ("^", parent),
            ("^{commit}", expected),
            ("~1^{commit}", parent),
        ] {
            assert_eq!(
                pin_revision(
                    &test_context.context(),
                    &RepoFacts::default(),
                    &format!("{root}{suffix}"),
                    &cancel()
                )?,
                oid,
                "{root}{suffix}"
            );
        }
    }
    let (_, integration) = pin_integration(&test_context.context(), "origin/main", &cancel())?;
    assert_eq!(integration, expected);
    let (_, default_integration) =
        pin_integration(&test_context.context(), "origin/HEAD", &cancel())?;
    assert_eq!(default_integration, expected);
    let created = create(
        &test_context.context(),
        WorktreeCreate {
            branch: "feature/verified-base".to_owned(),
            base: Some("origin/main".to_owned()),
        },
        &cancel(),
    )?;
    assert_eq!(created.head, expected);
    let tracking = fixture.run(&["rev-parse", "refs/remotes/origin/main"])?;
    assert_eq!(
        String::from_utf8(tracking.stdout.bytes)?.trim(),
        wrong.to_string()
    );
    Ok(())
}

#[test]
fn tracking_base_the_remote_cannot_vouch_for_fails_closed() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    let head = fixture.head()?;
    fixture.run(&[
        "remote",
        "add",
        "origin",
        fixture.root.to_str().ok_or("non-UTF8 root")?,
    ])?;
    // A tracking ref whose remote is not configured, and one whose remote
    // is spelled in another case: Git resolves both locally.
    fixture.run(&["update-ref", "refs/remotes/gone/main", &head.to_string()])?;
    fixture.run(&["update-ref", "refs/remotes/origin/main", &head.to_string()])?;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let test_context = TestContext::new(&fixture)?;
    let mut unverifiable = vec!["gone/main", "gone/main~1", "remotes/gone/main^{commit}"];
    // On a case-insensitive filesystem `Origin/main` finds origin/main.
    let probe = fixture._temp.path().join("CaseProbe");
    fs::write(&probe, b"")?;
    let insensitive = fixture._temp.path().join("caseprobe").exists();
    if insensitive {
        unverifiable.extend(["Origin/main", "Origin/main~1", "remotes/Origin/main"]);
    }
    for base in unverifiable {
        let error = pin_revision(
            &test_context.context(),
            &RepoFacts::default(),
            base,
            &cancel(),
        )
        .err()
        .ok_or_else(|| format!("{base} must not fall back to the local tracking OID"))?;
        assert!(
            matches!(&error, WorktreeError::BaseUnverified { base: named } if named == base),
            "{base}: {error}"
        );
    }
    Ok(())
}

#[test]
fn unresolved_base_error_names_the_base() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    fixture.run(&[
        "remote",
        "add",
        "origin",
        fixture.root.to_str().ok_or("non-UTF8 root")?,
    ])?;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let test_context = TestContext::new(&fixture)?;
    let error = create(
        &test_context.context(),
        WorktreeCreate {
            branch: "feature/missing-base".to_owned(),
            base: Some("origin/missing".to_owned()),
        },
        &cancel(),
    )
    .expect_err("missing remote branch must fail");
    assert!(matches!(
        error,
        WorktreeError::BaseNotFound { ref base } if base == "origin/missing"
    ));

    let error = create(
        &test_context.context(),
        WorktreeCreate {
            branch: "feature/missing-base".to_owned(),
            base: Some("unknown/branch".to_owned()),
        },
        &cancel(),
    )
    .expect_err("missing base must fail");
    assert_eq!(
        error.to_string(),
        "base unknown/branch did not resolve to a commit"
    );
    Ok(())
}

#[test]
fn create_skips_prunable_worktree_registrations() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let stale = create(
        &context,
        WorktreeCreate {
            branch: "scratch/stale".to_owned(),
            base: None,
        },
        &cancel(),
    )?;
    fs::remove_dir_all(&stale.path)?;
    let listing = fixture.run(&["worktree", "list", "--porcelain"])?;
    assert!(String::from_utf8_lossy(&listing.stdout.bytes).contains("prunable"));

    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/after-prunable".to_owned(),
            base: None,
        },
        &cancel(),
    )?;

    assert!(created.path.is_dir());
    assert!(!stale.path.exists());
    assert!(
        read_worktrees(&context, &cancel())?
            .iter()
            .any(|entry| entry.path == stale.path && entry.prunable)
    );
    Ok(())
}

#[test]
fn create_with_prunable_registration_for_same_branch() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let stale = create(
        &context,
        WorktreeCreate {
            branch: "scratch/stale".to_owned(),
            base: None,
        },
        &cancel(),
    )?;
    fs::remove_dir_all(&stale.path)?;
    let error = create(
        &context,
        WorktreeCreate {
            branch: "scratch/stale".to_owned(),
            base: None,
        },
        &cancel(),
    )
    .expect_err("stale registration must fail with prune hint");
    assert!(matches!(error, WorktreeError::StaleRegistration));
    assert!(error.to_string().contains("git worktree prune"), "{error}");

    fixture.run(&["commit", "--allow-empty", "-m", "advance HEAD"])?;
    let head = fixture.head()?;
    fixture.run(&["update-ref", "-d", "refs/heads/scratch/stale"])?;
    fixture.run(&["branch", "scratch/stale", &head.to_string()])?;
    let error_recreated = create(
        &context,
        WorktreeCreate {
            branch: "scratch/stale".to_owned(),
            base: None,
        },
        &cancel(),
    )
    .expect_err("recreated branch with stale registration must fail with prune hint");
    assert!(matches!(error_recreated, WorktreeError::StaleRegistration));
    Ok(())
}

#[test]
fn remove_reports_stale_registration_by_branch_and_directory() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let stale = create(
        &context,
        WorktreeCreate {
            branch: "scratch/stale".to_owned(),
            base: None,
        },
        &cancel(),
    )?;
    fs::remove_dir_all(&stale.path)?;
    for name in ["scratch/stale", "scratch-stale"] {
        let error = remove(
            &context,
            WorktreeRemove::Name {
                name: name.to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancel(),
        )
        .expect_err("stale registration must be reported");
        assert!(matches!(error, WorktreeError::StaleRegistration));
    }
    assert!(branch_exists(&fixture, "scratch/stale"));
    assert!(
        read_worktrees(&context, &cancel())?
            .iter()
            .any(|entry| entry.path == stale.path && entry.prunable)
    );
    Ok(())
}

#[test]
fn prune_treats_a_prunable_branch_as_not_checked_out() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let stale = create(
        &context,
        WorktreeCreate {
            branch: "scratch/stale".to_owned(),
            base: None,
        },
        &cancel(),
    )?;
    run_in_worktree(
        &fixture,
        &stale.path,
        &["commit", "--allow-empty", "-m", "stale work"],
    )?;
    fixture.run(&[
        "merge",
        "--no-ff",
        "scratch/stale",
        "-m",
        "merge stale work",
    ])?;
    fs::remove_dir_all(&stale.path)?;
    let entries = read_worktrees(&context, &cancel())?;
    let branch = GitRefName::branch("scratch/stale")?;
    assert!(matching_worktree(&entries, &branch, &fixture.root)?.is_none());
    assert!(!checked_out_elsewhere(&entries, &branch, None)?);
    assert!(!checked_out_elsewhere(
        &entries,
        &branch,
        Some(&fixture.root)
    )?);
    let integration = primary_integration(&context, &cancel())?.0;
    let result = remove(
        &context,
        WorktreeRemove::Merged {
            integration: integration.as_str().to_owned(),
            scope: PruneScope::Branches(vec!["scratch/stale".to_owned()]),
        },
        &cancel(),
    )?;
    assert_eq!(result.pruned_refs, vec!["scratch/stale"]);
    assert!(result.retained.is_empty());
    assert!(!branch_exists(&fixture, "scratch/stale"));
    Ok(())
}

#[test]
fn creation_refuses_a_shared_writable_worktree_parent() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fs::create_dir(fixture.root.join(WORKTREE_ROOT))?;
    fs::set_permissions(
        fixture.root.join(WORKTREE_ROOT),
        fs::Permissions::from_mode(0o777),
    )?;
    let test_context = TestContext::new(&fixture)?;
    assert!(matches!(
        create(
            &test_context.context(),
            WorktreeCreate {
                branch: "feature/refused".to_owned(),
                base: None
            },
            &cancel()
        ),
        Err(WorktreeError::UnknownWriter)
    ));
    assert!(
        !fixture
            .root
            .join(WORKTREE_ROOT)
            .join("feature-refused")
            .exists()
    );
    Ok(())
}

#[test]
fn worktree_operations_require_the_repo_project_binding() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let mut test_context = TestContext::new(&fixture)?;
    let other_fixture = Fixture::new()?;
    test_context.binding = TestContext::new(&other_fixture)?.binding;
    assert!(matches!(
        list(&test_context.context(), &cancel()),
        Err(WorktreeError::Repository(RepoError::InvalidRegistration))
    ));
    Ok(())
}

#[test]
fn clean_linked_worktree_reports_and_retains_in_progress_git_operations()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/sequencer".into(),
            base: None,
        },
        &cancellation,
    )?;
    let verified = verify_worktree(&fixture.repo, &created.path)?;
    check_writer_terminal(verified.repo())?;
    for (marker, directory) in [
        ("CHERRY_PICK_HEAD", false),
        ("REVERT_HEAD", false),
        ("REBASE_HEAD", false),
        ("rebase-merge", true),
        ("rebase-apply", true),
        ("MERGE_HEAD", false),
        ("sequencer", true),
    ] {
        let marker_path = verified.repo().private_git_dir().join(marker);
        if directory {
            fs::create_dir(&marker_path)?;
        } else {
            fs::write(&marker_path, format!("{}\n", created.head))?;
        }
        assert!(
            matches!(
                check_writer_terminal(verified.repo()),
                Err(WorktreeError::GitOperationInProgress)
            ),
            "{marker}"
        );
        if marker == "MERGE_HEAD" {
            let expected = fs::read(&marker_path)?;
            check_writer_terminal_with_merge(verified.repo(), &expected)?;
            assert!(matches!(
                check_writer_terminal_with_merge(verified.repo(), b"different"),
                Err(WorktreeError::GitOperationInProgress)
            ));
            let other = verified.repo().private_git_dir().join("CHERRY_PICK_HEAD");
            fs::write(&other, &expected)?;
            assert!(matches!(
                check_writer_terminal_with_merge(verified.repo(), &expected),
                Err(WorktreeError::GitOperationInProgress)
            ));
            fs::remove_file(other)?;
        }
        check_writer_terminal(&fixture.repo)?;
        let listed = list(&context, &cancellation)?;
        let entry = listed
            .iter()
            .find(|entry| entry.path == created.path)
            .ok_or("worktree missing")?;
        assert_eq!(
            entry.dirty,
            WorktreeCondition::GitOperationInProgress,
            "{marker}"
        );
        assert!(
            matches!(
                remove(
                    &context,
                    WorktreeRemove::Name {
                        name: "feature/sequencer".into(),
                        discard: false,
                        integration: None,
                        expected_head: None,
                    },
                    &cancellation,
                ),
                Err(WorktreeError::GitOperationInProgress)
            ),
            "{marker}"
        );
        assert!(created.path.is_dir());
        if directory {
            fs::remove_dir(&marker_path)?;
        } else {
            fs::remove_file(&marker_path)?;
        }
    }
    check_writer_terminal(verified.repo())?;
    let listed = list(&context, &cancellation)?;
    assert!(
        listed
            .iter()
            .any(|entry| entry.path == created.path && entry.dirty == WorktreeCondition::Clean)
    );
    remove(
        &context,
        WorktreeRemove::Name {
            name: "feature/sequencer".into(),
            discard: false,
            integration: None,
            expected_head: None,
        },
        &cancellation,
    )?;
    Ok(())
}

#[test]
fn create_refuses_aliases_of_local_and_remote_branches() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "Feature/Topic"])?;
    fixture.run(&["update-ref", "refs/remotes/origin/Remote/Topic", "HEAD"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    for (branch, existing) in [
        ("feature/topic", "refs/heads/Feature/Topic"),
        ("remote/topic", "refs/remotes/origin/Remote/Topic"),
    ] {
        let error = create(
            &context,
            WorktreeCreate {
                branch: branch.into(),
                base: None,
            },
            &cancel(),
        );
        assert!(
            matches!(error, Err(WorktreeError::BranchCollision { existing: ref found }) if found == existing),
            "unexpected result for {branch}: {error:?}"
        );
        assert!(
            !fixture
                .root
                .join(".worktrees")
                .join(branch.replace('/', "-"))
                .exists()
        );
    }
    Ok(())
}

#[test]
fn create_list_remove_keeps_branch_and_refuses_primary() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();

    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/lifecycle".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    assert!(
        created
            .path
            .starts_with(canonicalize(&fixture.root)?.join(".worktrees"))
    );
    assert_eq!(created.head, fixture.head()?);
    assert_eq!(created.dirty, WorktreeCondition::Clean);
    assert_eq!(created.writer, WriterState::Free);

    let listed = list(&context, &cancellation)?;
    assert!(listed.iter().any(|entry| {
        entry.path == created.path
            && entry.branch.as_deref() == Some("refs/heads/feature/lifecycle")
            && entry.dirty == WorktreeCondition::Clean
            && entry.writer == WriterState::Free
    }));
    assert!(matches!(
        remove(
            &context,
            WorktreeRemove::Name {
                name: "main".to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation
        ),
        Err(WorktreeError::Primary)
    ));

    let unknown_context = WorktreeContext {
        workers: &UnknownWorkers,
        ..test_context.context()
    };
    // An unobservable process with a free lease and a clean tree is removed.
    remove(
        &unknown_context,
        WorktreeRemove::Name {
            name: "feature/lifecycle".to_owned(),
            discard: false,
            integration: None,
            expected_head: None,
        },
        &cancellation,
    )?;
    assert!(!created.path.exists());
    Ok(())
}

#[test]
fn ignored_files_prevent_worktree_removal() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    fs::write(fixture.root.join(".gitignore"), "ignored.txt\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore fixture file"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/ignored".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    fs::write(created.path.join("ignored.txt"), "keep me\n")?;
    let condition = |cancellation: &CancelToken| -> Result<_, Box<dyn Error>> {
        Ok(list(&context, cancellation)?
            .into_iter()
            .find(|entry| entry.path == created.path)
            .map(|entry| entry.dirty))
    };
    assert_eq!(condition(&cancellation)?, Some(WorktreeCondition::Ignored));

    assert!(matches!(
        remove(
            &context,
            WorktreeRemove::Name {
                name: "feature/ignored".to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation,
        ),
        Err(WorktreeError::IgnoredPaths(ref paths)) if paths.contains("ignored.txt")
    ));
    assert_eq!(
        fs::read_to_string(created.path.join("ignored.txt"))?,
        "keep me\n"
    );
    fs::write(created.path.join("notes.txt"), "draft\n")?;
    assert_eq!(condition(&cancellation)?, Some(WorktreeCondition::Dirty));
    Ok(())
}

#[test]
fn named_removal_refuses_a_head_that_moved_after_the_callers_check() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/raced".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    // The caller verified the worktree at its creation head, then a clean
    // commit landed before removal started.
    let verified = created.head;
    run_in_worktree(
        &fixture,
        &created.path,
        &[
            "-c",
            "user.name=raced",
            "-c",
            "user.email=raced@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "after the check",
        ],
    )?;
    let branch = GitRefName::branch("feature/raced")?;
    let moved = read_branch_tips(&context, &cancellation)?
        .get(&branch)
        .copied()
        .ok_or("branch tip")?;
    assert_ne!(moved, verified);
    let request = |expected_head| WorktreeRemove::Name {
        name: "feature/raced".to_owned(),
        discard: false,
        integration: None,
        expected_head,
    };
    assert!(matches!(
        remove(&context, request(Some(verified)), &cancellation),
        Err(WorktreeError::IdentityChanged)
    ));
    assert!(created.path.exists(), "the checkout must survive");
    assert_eq!(
        read_branch_tips(&context, &cancellation)?.get(&branch),
        Some(&moved)
    );
    // Verifying the head it is really at lets the removal proceed.
    let outcome = remove(&context, request(Some(moved)), &cancellation)?;
    assert_eq!(outcome.removed.len(), 1);
    assert!(!created.path.exists());
    Ok(())
}

#[test]
fn merged_pruning_is_branch_scoped_and_keeps_unselected_refs() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let merged = create(
        &context,
        WorktreeCreate {
            branch: "feature/merged".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    fs::write(merged.path.join("proof.txt"), "merged fixture\n")?;
    run_in_worktree(&fixture, &merged.path, &["add", "proof.txt"])?;
    run_in_worktree(&fixture, &merged.path, &["commit", "-m", "feature commit"])?;
    fixture.run(&["merge", "--no-ff", "feature/merged", "-m", "merge feature"])?;

    let unselected = create(
        &context,
        WorktreeCreate {
            branch: "feature/unselected".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    let removed = remove(
        &context,
        WorktreeRemove::Merged {
            integration: "main".to_owned(),
            scope: PruneScope::Branches(vec!["feature/merged".to_owned()]),
        },
        &cancellation,
    )?;
    assert_eq!(removed.pruned_refs, vec!["feature/merged"]);
    assert_eq!(removed.removed.len(), 1);
    assert!(!merged.path.exists());
    assert!(unselected.path.exists());
    let tips = read_branch_tips(&context, &cancellation)?;
    assert!(!tips.contains_key(&GitRefName::branch("feature/merged")?));
    assert!(tips.contains_key(&GitRefName::branch("feature/unselected")?));
    Ok(())
}

#[test]
fn merged_pruning_keeps_old_ancestor_and_unmerged_ignored_files() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    fs::write(fixture.root.join(".gitignore"), "ignored.txt\n")?;
    fixture.run(&["add", ".gitignore"])?;
    fixture.run(&["commit", "-m", "ignore fixture file"])?;
    let old_base = fixture.head()?;
    fixture.run(&[
        "commit",
        "--allow-empty",
        "-m",
        "advance before branch creation",
    ])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let old = create(
        &context,
        WorktreeCreate {
            branch: "feature/old-ancestor".to_owned(),
            base: Some(old_base.to_string()),
        },
        &cancellation,
    )?;
    let unmerged = create(
        &context,
        WorktreeCreate {
            branch: "feature/unmerged".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    fs::write(unmerged.path.join("unique.txt"), "branch-only commit\n")?;
    run_in_worktree(&fixture, &unmerged.path, &["add", "unique.txt"])?;
    run_in_worktree(
        &fixture,
        &unmerged.path,
        &["commit", "-m", "unique branch commit"],
    )?;
    fs::write(old.path.join("ignored.txt"), "old branch data\n")?;
    fs::write(unmerged.path.join("ignored.txt"), "unmerged branch data\n")?;
    fixture.run(&["commit", "--allow-empty", "-m", "advance integration"])?;

    let outcome = remove(
        &context,
        WorktreeRemove::Merged {
            integration: "main".to_owned(),
            scope: PruneScope::Branches(vec![
                "feature/old-ancestor".to_owned(),
                "feature/unmerged".to_owned(),
            ]),
        },
        &cancellation,
    )?;
    assert!(outcome.removed.is_empty());
    assert!(outcome.pruned_refs.is_empty());
    assert_eq!(outcome.retained.len(), 2);
    assert!(outcome.retained.iter().any(|retained| {
        retained.branch == "feature/old-ancestor" && retained.reason == RetainReason::NoUniqueCommit
    }));
    assert!(old.path.exists());
    assert!(unmerged.path.exists());
    assert_eq!(
        fs::read_to_string(old.path.join("ignored.txt"))?,
        "old branch data\n"
    );
    assert_eq!(
        fs::read_to_string(unmerged.path.join("ignored.txt"))?,
        "unmerged branch data\n"
    );
    Ok(())
}

#[test]
fn create_accepts_uninitialised_gitlinks_and_still_rejects_initialised_ones()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let head = fixture.head()?;
    fixture.run(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{head},sub"),
    ])?;
    fixture.run(&["commit", "-m", "tracked but uninitialised gitlink"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let created = create(
        &context,
        WorktreeCreate {
            branch: "feature/uninitialised-gitlink".to_owned(),
            base: None,
        },
        &cancel(),
    )?;
    assert_eq!(created.dirty, WorktreeCondition::Clean);
    assert!(list(&context, &cancel())?.iter().any(|entry| {
        entry.branch.as_deref() == Some("refs/heads/feature/uninitialised-gitlink")
            && entry.dirty == WorktreeCondition::Clean
    }));

    let marker = fixture._temp.path().join("initialised-filter-ran");
    add_poisoned_gitlink(&fixture, &created.path, "sub", &marker)?;
    assert!(list(&context, &cancel())?.iter().any(|entry| {
        entry.branch.as_deref() == Some("refs/heads/feature/uninitialised-gitlink")
            && entry.dirty == WorktreeCondition::Unknown
    }));
    Ok(())
}

#[test]
fn gitlinks_make_remove_and_prune_require_review() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let direct = create(
        &context,
        WorktreeCreate {
            branch: "feature/direct-gitlink".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    let direct_marker = fixture._temp.path().join("direct-filter-ran");
    let direct_poison = add_poisoned_gitlink(&fixture, &direct.path, "sub", &direct_marker)?;
    assert_raw_git_runs_poison_filter(&fixture, &direct_poison)?;

    let merged = create(
        &context,
        WorktreeCreate {
            branch: "feature/merged-gitlink".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    fs::write(merged.path.join("merged.txt"), "merged feature\n")?;
    run_in_worktree(&fixture, &merged.path, &["add", "merged.txt"])?;
    run_in_worktree(&fixture, &merged.path, &["commit", "-m", "feature commit"])?;
    fixture.run(&[
        "merge",
        "--no-ff",
        "feature/merged-gitlink",
        "-m",
        "merge feature",
    ])?;
    let merged_marker = fixture._temp.path().join("merged-filter-ran");
    let merged_poison = add_poisoned_gitlink(&fixture, &merged.path, "sub", &merged_marker)?;
    assert_raw_git_runs_poison_filter(&fixture, &merged_poison)?;

    direct_poison.arm()?;
    merged_poison.arm()?;
    let listed = list(&context, &cancellation)?;
    assert!(
        !direct_marker.exists(),
        "worktree listing ran a nested clean filter"
    );
    assert!(
        !merged_marker.exists(),
        "worktree listing ran a nested clean filter"
    );
    for branch in ["feature/direct-gitlink", "feature/merged-gitlink"] {
        assert!(listed.iter().any(|entry| {
            entry.branch.as_deref() == Some(format!("refs/heads/{branch}").as_str())
                && entry.dirty == WorktreeCondition::Unknown
        }));
    }
    direct_poison.arm()?;
    merged_poison.arm()?;
    assert!(matches!(
        remove(
            &context,
            WorktreeRemove::Name {
                name: "feature/direct-gitlink".to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation,
        ),
        Err(WorktreeError::UnknownWriter)
    ));
    assert!(
        !direct_marker.exists(),
        "worktree removal ran a nested clean filter"
    );
    direct_poison.arm()?;
    merged_poison.arm()?;
    let pruned = remove(
        &context,
        WorktreeRemove::Merged {
            integration: "main".to_owned(),
            scope: PruneScope::Branches(vec!["feature/merged-gitlink".to_owned()]),
        },
        &cancellation,
    )?;
    assert!(pruned.removed.is_empty());
    assert!(pruned.pruned_refs.is_empty());
    assert_eq!(pruned.retained.len(), 1);
    assert_eq!(pruned.retained[0].reason, RetainReason::UnknownWriter);
    assert!(
        !merged_marker.exists(),
        "worktree pruning ran a nested clean filter"
    );
    assert!(direct.path.join("sub/.git").exists());
    assert!(merged.path.join("sub/.git").exists());
    assert!(!direct_marker.exists());
    assert!(!merged_marker.exists());
    Ok(())
}

#[test]
fn delete_ref_compare_and_swap_preserves_a_moved_ref() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let context = test_context.context();
    let cancellation = cancel();
    let stale_oid = fixture.head()?;
    fixture.run(&["branch", "feature/race"])?;
    fixture.run(&["commit", "--allow-empty", "-m", "advance integration"])?;
    let moved_oid = fixture.head()?;
    fixture.run(&["branch", "-f", "feature/race", &moved_oid.to_string()])?;

    let outcome = execute_git_mutation(
        &fixture.executor,
        &fixture.repo,
        &fixture.runtime,
        GitMutation::DeleteRef {
            name: GitRefName::branch("feature/race")?,
            expected_old_oid: stale_oid,
        },
        fixture.budget()?,
        &cancellation,
    )?;
    assert!(matches!(outcome.termination, Termination::Exit(code) if code != 0));
    let tips = read_branch_tips(&context, &cancellation)?;
    assert_eq!(
        tips.get(&GitRefName::branch("feature/race")?),
        Some(&moved_oid)
    );
    Ok(())
}

#[test]
fn detached_worktree_names_the_head_and_both_fixes() -> Result<(), Box<dyn std::error::Error>> {
    let head = GitOid::parse("0123456789abcdef0123456789abcdef01234567")?;
    let message = WorktreeError::DetachedWorktree { head }.to_string();
    for part in [
        "0123456789abcdef0123456789abcdef01234567",
        "git switch -c",
        "--mode read-only",
    ] {
        assert!(message.contains(part), "{message}");
    }
    Ok(())
}

#[test]
fn writer_busy_names_the_holder_only_when_known() {
    let holder = AttemptId::from_bytes([7; 16]);
    let known = WorktreeError::WriterBusy {
        holder: Some(holder),
    }
    .to_string();
    assert!(
        known.starts_with("worktree writer is busy, held by attempt "),
        "{known}"
    );
    assert_eq!(
        WorktreeError::WriterBusy { holder: None }.to_string(),
        "worktree writer is busy"
    );
}
