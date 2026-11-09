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

impl WorkerObserver for FreeWorkers {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        roots
            .iter()
            .cloned()
            .map(|root| (root, Occupancy::Available))
            .collect()
    }
}

/// A worker observer that sees one named process inside every root.
pub(super) struct NamedLive;

impl WorkerObserver for NamedLive {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        roots
            .iter()
            .cloned()
            .map(|root| (root, Occupancy::Observed { process: None }))
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

impl WorkerObserver for UnknownWorkers {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        roots
            .iter()
            .cloned()
            .map(|root| {
                (
                    root.clone(),
                    Occupancy::census_unknown(root, "fixture census unavailable"),
                )
            })
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
        provenant_os::fixture::private_state_dir(&binding_state)?;
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
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            let mut states = roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Available))
                .collect();
            match observe_same_process(&SystemProcessObserver, &SystemCwdObserver, &self.0) {
                NativeCwd::Path(path) => {
                    record_live_path(&mut states, &path);
                }
                _ => {
                    for (root, state) in &mut states {
                        *state =
                            Occupancy::census_unknown(root.clone(), "fixture process unavailable");
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
        Some(&Occupancy::Observed { process: None })
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
                let repository = context
                    .repo
                    .common_dir()
                    .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                    .unwrap();
                holder.replace(Some(repository));
                return false;
            }
            if matches!(
                context
                    .repo
                    .common_dir()
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

/// Records whether the repository lock was held during each census.
struct LockProbeWorkers<'a> {
    common: &'a Directory,
    repository_held: std::sync::Mutex<Vec<bool>>,
}

impl WorkerObserver for LockProbeWorkers<'_> {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        let held = self
            .common
            .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
            .is_err();
        self.repository_held.lock().unwrap().push(held);
        roots
            .iter()
            .cloned()
            .map(|root| (root, Occupancy::Available))
            .collect()
    }
}

/// Free until the `unknown_from`th call (1-based), then unobservable.
struct FlippingWorkers {
    calls: std::sync::atomic::AtomicUsize,
    unknown_from: usize,
}

impl WorkerObserver for FlippingWorkers {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        roots
            .iter()
            .cloned()
            .map(|root| {
                let state = if call >= self.unknown_from {
                    Occupancy::census_unknown(root.clone(), "fixture census unavailable")
                } else {
                    Occupancy::Available
                };
                (root, state)
            })
            .collect()
    }

    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        let states = self.observe_roots(roots);
        let details = states
            .iter()
            .filter(|(_, state)| !state.is_available())
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

/// Worktree creation and writer setup use one common-directory mutation owner.
#[test]
fn worktree_creation_excludes_writer_configuration_updates() -> Result<(), Box<dyn Error>> {
    struct MutationProbe<'a> {
        inner: &'a dyn Executor,
        common: &'a Directory,
        observed: std::sync::atomic::AtomicBool,
    }
    impl Executor for MutationProbe<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            if options.argv.iter().any(|word| word == "worktree")
                && options.argv.iter().any(|word| word == "add")
            {
                assert!(
                    matches!(
                        self.common
                            .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive),
                        Err(FsError::LockTimeout { .. })
                    ),
                    "worktree addition must exclude configuration writers"
                );
                self.observed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            self.inner.execute(options)
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    context.lock_budget = WaitBudget::nonblocking(context.clock.mono_now());
    let cancel = cancel();
    let setup = lock_repository(
        context.repo.common_dir(),
        LockMode::Exclusive,
        context.lock_budget,
        &cancel,
        context.clock,
    )?;
    let request = WorktreeCreate {
        branch: "shared-mutation-owner".into(),
        base: Some(fixture.head()?.to_string()),
    };
    assert!(matches!(
        create(&context, request.clone(), &cancel),
        Err(WorktreeError::RegistryLockTimeout)
    ));
    assert!(
        !fixture
            .root
            .join(".worktrees/shared-mutation-owner")
            .exists()
    );
    // A completed sibling setup legitimately invalidates our old config digest.
    fixture.run(&["config", "--local", "branch.sibling.remote", "origin"])?;
    drop(setup);
    let probe = MutationProbe {
        inner: context.executor,
        common: context.repo.common_dir(),
        observed: std::sync::atomic::AtomicBool::new(false),
    };
    context.executor = &probe;
    let created = create(&context, request, &cancel)?;
    assert!(created.path.is_dir());
    assert!(probe.observed.load(std::sync::atomic::Ordering::SeqCst));
    drop(
        context
            .repo
            .common_dir()
            .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)?,
    );
    Ok(())
}

mod gc_preview;
mod gc_pruning;
mod gc_safety;
mod ownership;
mod ref_ownership;
mod remote_creation;
mod worktree_operations;
