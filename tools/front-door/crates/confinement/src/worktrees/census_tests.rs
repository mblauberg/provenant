//! Worker census and native observation tests.
use super::census::{unreadable_cohort_pids, worker_evidence_from_census};
use super::census_excusal::PolicyCwdObserver;
use super::lifecycle_tests::*;
use super::*;
use crate::test_support::Fixture;
use provenant_supervisor::census::ObservationIssue;
use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;

struct DelegatingWriterObserver(Occupancy);
impl WorkerObserver for DelegatingWriterObserver {
    fn observe_roots(&self, _: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        panic!("delegation must use observe_roots_with_details")
    }
    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        (
            roots
                .iter()
                .cloned()
                .map(|root| (root, self.0.clone()))
                .collect(),
            roots
                .iter()
                .cloned()
                .map(|root| (root, "census detail".into()))
                .collect(),
        )
    }
}

#[test]
fn observe_writer_tree_censuses_present_and_removed_trees() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let present = temp.path().join("present-writer-tree");
    fs::create_dir(&present)?;
    for tree in [present, temp.path().join("removed-writer-tree")] {
        assert_eq!(
            super::census::observe_writer_tree(&DelegatingWriterObserver(Occupancy::Held), &tree),
            (Occupancy::Held, Some("census detail".into()))
        );
    }
    Ok(())
}

#[test]
fn an_absent_root_places_paths_lexically() -> Result<(), Box<dyn Error>> {
    use super::census::{absent_root_contains, occupancy_path_could_be_under};
    use super::census_excusal::PlacementEvidence;
    use std::path::Path;
    let temp = tempfile::tempdir()?;
    let removed = temp.path().join("removed-writer-tree");
    // A process left in the removed tree keeps its deleted path.
    let inside = removed.join("src");
    let sibling = temp.path().join("removed-writer-tree-2");
    assert_eq!(absent_root_contains(&inside, &removed), Some(true));
    assert_eq!(absent_root_contains(&removed, &removed), Some(true));
    assert_eq!(absent_root_contains(&sibling, &removed), Some(false));
    assert_eq!(absent_root_contains(Path::new("/"), &removed), Some(false));
    assert!(occupancy_path_could_be_under(&inside, &removed));
    assert!(!occupancy_path_could_be_under(Path::new("/"), &removed));
    let roots = [removed.clone()];
    assert!(PlacementEvidence::from_path(&inside, &roots).blocks_excusal());
    assert!(PlacementEvidence::from_path(Path::new("/"), &roots).outside_every_root());
    // A present root is placed by identity, and a dangling symlink stays
    // undecidable.
    fs::create_dir(&removed)?;
    assert_eq!(absent_root_contains(&inside, &removed), None);
    let link = temp.path().join("dangling-writer-tree");
    std::os::unix::fs::symlink(temp.path().join("missing-target"), &link)?;
    assert_eq!(absent_root_contains(&link.join("src"), &link), None);
    assert!(occupancy_path_could_be_under(Path::new("/"), &link));
    assert!(PlacementEvidence::from_path(Path::new("/"), &[link]).blocks_excusal());
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)] // A real child keeps the kernel's view of a deleted cwd.
fn a_process_left_in_a_removed_tree_is_placed_inside_it() -> Result<(), Box<dyn Error>> {
    use provenant_os::pid::{CwdObservation, CwdObserver, SystemCwdObserver};
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("removed-writer-tree");
    fs::create_dir_all(root.join("src"))?;
    // A cwd at the removed root itself carries Linux's " (deleted)" marker; one
    // below it keeps a removed suffix either way.
    let children = [root.clone(), root.join("src")]
        .iter()
        .map(|cwd| {
            Ok(Child(
                std::process::Command::new("/bin/sh")
                    .args(["-c", "read line"])
                    .current_dir(cwd)
                    .stdin(std::process::Stdio::piped())
                    .spawn()?,
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    fs::remove_dir_all(&root)?;
    for child in &children {
        let CwdObservation::Path(cwd) = SystemCwdObserver.cwd(i64::from(child.0.id()))? else {
            return Err("the child's cwd was not observable".into());
        };
        assert_eq!(
            super::census::absent_root_contains(&cwd, &root),
            Some(true),
            "{cwd:?}"
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn a_removed_root_spelled_through_the_data_firmlink_places_by_identity()
-> Result<(), Box<dyn Error>> {
    use super::census::absent_root_contains;
    let temp = tempfile::tempdir()?;
    let canonical = fs::canonicalize(temp.path())?;
    let data = std::path::Path::new("/System/Volumes/Data").join(canonical.strip_prefix("/")?);
    if !data.is_dir() {
        return Ok(());
    }
    let root = data.join("removed-writer-tree");
    for spelling in [canonical.as_path(), temp.path()] {
        let inside = spelling.join("removed-writer-tree/src");
        assert_eq!(
            absent_root_contains(&inside, &root),
            Some(true),
            "{inside:?}"
        );
    }
    assert_eq!(
        absent_root_contains(&canonical.join("other/src"), &root),
        Some(false)
    );
    Ok(())
}

#[test]
fn an_unreadable_path_elsewhere_cannot_hold_a_removed_root() -> Result<(), Box<dyn Error>> {
    use super::census::occupancy_path_could_be_under;
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let anchor = temp.path().join("worktrees");
    let root = anchor.join("removed-writer-tree");
    let elsewhere = temp.path().join("daemon-store");
    let sibling = anchor.join("locked-sibling");
    for locked in [&elsewhere, &sibling] {
        fs::create_dir_all(locked.join("restricted/stream"))?;
        fs::set_permissions(locked.join("restricted"), fs::Permissions::from_mode(0o000))?;
    }
    // A symlink loop cannot be split either, and could alias the root.
    let looped = temp.path().join("loop");
    std::os::unix::fs::symlink("loop", &looped)?;
    let unreadable = fs::metadata(elsewhere.join("restricted/stream")).is_err();
    let verdicts = [
        occupancy_path_could_be_under(&elsewhere.join("restricted/stream/local"), &root),
        occupancy_path_could_be_under(&sibling.join("restricted/stream/local"), &root),
        occupancy_path_could_be_under(&root.join("src/file"), &root),
        occupancy_path_could_be_under(&looped.join("file"), &root),
    ];
    for locked in [&elsewhere, &sibling] {
        fs::set_permissions(locked.join("restricted"), fs::Permissions::from_mode(0o755))?;
    }
    if !unreadable {
        // Running as a superuser: nothing is unreadable, so nothing to pin.
        return Ok(());
    }
    // Neither locked directory is an ancestor of the removed root; a path left
    // in the removed tree, or one through a symlink, still could be under it.
    assert_eq!(verdicts, [false, false, true, true]);
    Ok(())
}

#[test]
fn session_root_that_appears_during_the_census_is_unknown() -> Result<(), Box<dyn Error>> {
    struct Creating(PathBuf);
    impl WorkerObserver for Creating {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            fs::create_dir(&self.0).expect("create the root mid-census");
            roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Available))
                .collect()
        }
    }
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("appears");
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_unix_seconds(0)?,
        provenant_core::MonoTime::from_micros(1),
    );
    let observer = Creating(root.clone());
    let session = CensusSession::new(&observer, &clock);
    assert!(session.observe_roots(std::slice::from_ref(&root))[&root].is_unknown());
    Ok(())
}

struct EmptyWriterObserver;
impl WorkerObserver for EmptyWriterObserver {
    fn observe_roots(&self, _: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        BTreeMap::new()
    }
    fn observe_roots_with_details(&self, _: &[PathBuf]) -> WorkerObservations {
        (BTreeMap::new(), BTreeMap::new())
    }
}

#[test]
fn observe_writer_tree_missing_census_entry_is_unknown() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let present = temp.path().join("present-writer-tree");
    fs::create_dir(&present)?;
    assert_eq!(
        super::census::observe_writer_tree(&EmptyWriterObserver, &present),
        (
            Occupancy::census_unknown(present, "worker census did not return the requested tree"),
            None
        )
    );
    Ok(())
}

#[test]
fn session_reuses_evidence_and_refreshes_at_epoch_boundaries() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct ToggleObserver(AtomicBool, AtomicBool);
    impl WorkerObserver for ToggleObserver {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| {
                    (
                        root,
                        if self.0.load(Ordering::SeqCst) {
                            Occupancy::Observed { process: None }
                        } else {
                            Occupancy::Available
                        },
                    )
                })
                .collect()
        }
        fn epoch_policy(&self) -> Option<CensusEpochPolicy> {
            Some(CensusEpochPolicy {
                exclude_ancestors: self.1.load(Ordering::SeqCst),
                ..CensusEpochPolicy::default()
            })
        }
    }
    let fixture = Fixture::new()?;
    let a = fixture.root.join("session-a");
    let b = fixture.root.join("session-b");
    fs::create_dir(&a)?;
    fs::create_dir(&b)?;
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_unix_seconds(0)?,
        provenant_core::MonoTime::from_micros(42),
    );
    let observer = ToggleObserver(AtomicBool::new(false), AtomicBool::new(false));
    let session = CensusSession::new(&observer, &clock);
    assert!(session.observe_roots(std::slice::from_ref(&a))[&a].is_available());
    observer.0.store(true, Ordering::SeqCst);
    assert!(
        session
            .observe_roots_with_details(std::slice::from_ref(&a))
            .0[&a]
            .is_available()
    );
    assert_eq!(session.observed_at(), Some(clock.mono_now()));
    let expanded = session.observe_roots(&[a.clone(), b.clone()]);
    assert!(expanded[&a].is_observed());
    assert!(expanded[&b].is_observed());
    observer.0.store(false, Ordering::SeqCst);
    assert!(session.observe_roots_for_land(std::slice::from_ref(&a))[&a].is_available());
    session.invalidate();
    observer.0.store(true, Ordering::SeqCst);
    assert!(session.observe_roots_for_land(std::slice::from_ref(&a))[&a].is_observed());
    observer.0.store(false, Ordering::SeqCst);
    fs::rename(&a, fixture.root.join("session-old"))?;
    fs::create_dir(&a)?;
    assert!(session.observe_roots_for_land(std::slice::from_ref(&a))[&a].is_available());
    let second_command = CensusSession::new(&observer, &clock);
    assert!(second_command.observe_roots(std::slice::from_ref(&a))[&a].is_available());
    observer.0.store(true, Ordering::SeqCst);
    observer.1.store(true, Ordering::SeqCst);
    assert!(second_command.observe_roots(std::slice::from_ref(&a))[&a].is_observed());
    Ok(())
}

#[test]
fn session_acquires_with_captured_policy_during_aba() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Changing {
        policy_b: AtomicBool,
    }
    impl WorkerObserver for Changing {
        fn observe_roots(&self, _: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            panic!("acquisition must use the captured policy")
        }
        fn epoch_policy(&self) -> Option<CensusEpochPolicy> {
            Some(CensusEpochPolicy {
                exclude_ancestors: self.policy_b.load(Ordering::SeqCst),
                ..CensusEpochPolicy::default()
            })
        }
        fn observe_captured_policy(
            &self,
            roots: &[PathBuf],
            _: bool,
            policy: &CensusEpochPolicy,
        ) -> WorkerEvidence {
            self.policy_b.store(true, Ordering::SeqCst);
            let state = if policy.exclude_ancestors {
                Occupancy::Available
            } else {
                Occupancy::Observed { process: None }
            };
            self.policy_b.store(false, Ordering::SeqCst);
            WorkerEvidence {
                states: roots
                    .iter()
                    .cloned()
                    .map(|root| (root, state.clone()))
                    .collect(),
                ..WorkerEvidence::default()
            }
        }
    }
    let fixture = Fixture::new()?;
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_unix_seconds(0)?,
        provenant_core::MonoTime::from_micros(1),
    );
    let observer = Changing {
        policy_b: AtomicBool::new(false),
    };
    let session = CensusSession::new(&observer, &clock);
    assert!(
        session.observe_roots(std::slice::from_ref(&fixture.root))[&fixture.root].is_observed()
    );
    assert!(
        session.observe_roots(std::slice::from_ref(&fixture.root))[&fixture.root].is_observed()
    );
    Ok(())
}

#[test]
fn session_refusal_uses_decision_evidence_and_a_stably_absent_root_keeps_its_verdict()
-> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Sequenced(AtomicUsize);
    impl WorkerObserver for Sequenced {
        fn observe_roots(&self, _: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            panic!("must acquire evidence together")
        }
        fn observe_evidence(&self, roots: &[PathBuf], _: bool) -> WorkerEvidence {
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            let root = roots[0].clone();
            WorkerEvidence {
                states: BTreeMap::from([(
                    root.clone(),
                    if n == 0 {
                        Occupancy::Observed { process: None }
                    } else {
                        Occupancy::Available
                    },
                )]),
                holders: BTreeMap::from([(
                    root.clone(),
                    ObservedProcess {
                        pid: 123,
                        command: Some("first".into()),
                        kind: ProcessEvidenceKind::WorkingDirectory,
                        cwd: Some(root.clone()),
                    },
                )]),
                held: BTreeMap::from([(
                    root.clone(),
                    vec![ObservedProcess {
                        pid: 123,
                        command: Some("first".into()),
                        kind: ProcessEvidenceKind::WorkingDirectory,
                        cwd: Some(root.clone()),
                    }],
                )]),
                taints: BTreeMap::from([(
                    root.clone(),
                    TaintCause::LiveWriter(ObservedProcess {
                        pid: 123,
                        command: Some("first".into()),
                        kind: ProcessEvidenceKind::WorkingDirectory,
                        cwd: Some(root.clone()),
                    }),
                )]),
                details: BTreeMap::from([(root, "first observation".into())]),
                held_incomplete: false,
            }
        }
    }
    let fixture = Fixture::new()?;
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_unix_seconds(0)?,
        provenant_core::MonoTime::from_micros(1),
    );
    let observer = Sequenced(AtomicUsize::new(0));
    let session = CensusSession::new(&observer, &clock);
    let root = fixture.root.clone();
    let (states, details) = session.observe_roots_with_details(std::slice::from_ref(&root));
    assert!(states[&root].is_observed());
    assert!(details.contains_key(&root));
    assert_eq!(session.live_process(&root).unwrap().pid, 123);
    let (_, taints) = session.observe_roots_with_taints(std::slice::from_ref(&root));
    assert!(matches!(&taints[&root], TaintCause::LiveWriter(p) if p.pid == 123));
    // A root absent before and after the census keeps the census verdict;
    // placement, not the session, decides what an absent root holds.
    let missing = root.join("missing");
    assert!(session.observe_roots(std::slice::from_ref(&missing))[&missing].is_available());
    assert!(session.observe_roots(std::slice::from_ref(&root))[&root].is_available());
    Ok(())
}

#[test]
fn command_session_is_reused_across_worktree_contexts() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting(AtomicUsize);
    impl WorkerObserver for Counting {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            self.0.fetch_add(1, Ordering::SeqCst);
            roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Available))
                .collect()
        }
    }
    let fixture = Fixture::new()?;
    let state = fixture._temp.path().join("command-state");
    provenant_os::fixture::private_state_dir(&state)?;
    let mut input = provenant_core::RootInputs::new(fixture.home.clone(), fixture.root.clone());
    input.state_dir = Some(state);
    let roots = input.resolve(&provenant_os::fs::SystemPathResolver)?;
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_unix_seconds(0)?,
        provenant_core::MonoTime::from_micros(1),
    );
    let observer = Counting(AtomicUsize::new(0));
    let session = CensusSession::new(&observer, &clock);
    for _ in 0..2 {
        super::context::with_context_using_workers(
            &roots,
            &fixture.home,
            &fixture.root,
            &clock,
            &cancel(),
            &session,
            |context| {
                let roots = std::slice::from_ref(&fixture.root);
                assert!(context.workers.observe_roots(roots)[&fixture.root].is_available());
                assert!(
                    context.workers.observe_roots_with_details(roots).0[&fixture.root]
                        .is_available()
                );
                Ok(())
            },
        )?;
    }
    assert_eq!(observer.0.load(Ordering::SeqCst), 1);
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn native_full_census_places_clean_merged_worktree_and_same_user_sleep()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test = TestContext::new(&fixture)?;
    let created = create(
        &test.context(),
        WorktreeCreate {
            branch: "full-census-merged".into(),
            base: None,
        },
        &cancel(),
    )?;
    let root = canonicalize(&created.path)?;
    let current = refused_fact(100);
    let clean = InjectedCensus {
        census: provenant_supervisor::census::Census::from_observations(
            [current.clone()],
            [],
            [],
            true,
            false,
            32,
        )?,
        cwds: BTreeMap::new(),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };
    assert!(clean.observe_roots(std::slice::from_ref(&root))[&root].is_available());

    let mut sleep = refused_fact(200);
    sleep.command = Some("sleep".into());
    let occupied = InjectedCensus {
        census: provenant_supervisor::census::Census::from_observations(
            [current.clone(), sleep],
            [],
            [],
            true,
            false,
            32,
        )?,
        cwds: BTreeMap::from([(200, CwdObservation::Path(root.clone()))]),
        current,
        platform_uids: BTreeMap::new(),
    };
    let states = occupied.observe_roots(std::slice::from_ref(&root));
    assert!(matches!(
        &states[&root],
        Occupancy::Observed {
            process: Some(ObservedProcess {
                pid: 200,
                kind: ProcessEvidenceKind::WorkingDirectory,
                ..
            })
        }
    ));
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn native_unreadable_windowserver_allows_cleanup_of_a_merged_clean_tree()
-> Result<(), Box<dyn Error>> {
    use provenant_os::pid::{
        OccupancyObservation, ProcessUids, SealedSystemImage, UnreadablePolicyExcusal,
    };
    use provenant_supervisor::census::{Census, ObservationIssue};

    // Model a foreign sealed system process with an unreadable cwd. The native
    // process-observer smoke test is covered separately below.
    struct WindowServerWorkers {
        census: Census,
        current: ProcessFact,
        system_uids: ProcessUids,
    }
    impl ProcessObserver for WindowServerWorkers {
        fn boot_identity(&self) -> Result<String, provenant_supervisor::census::CensusError> {
            Ok("fixture-boot".into())
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            match self.census.process(pid) {
                ProcessLookup::Present(fact) => ProcessObservation::Present(fact.clone()),
                ProcessLookup::Absent => ProcessObservation::Absent,
                ProcessLookup::Unknown => ProcessObservation::Unknown(ObservationIssue::Malformed),
            }
        }
        fn census(&self, _: usize) -> Result<Census, provenant_supervisor::census::CensusError> {
            Ok(self.census.clone())
        }
    }
    impl CwdObserver for WindowServerWorkers {
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(if pid == 200 {
                CwdObservation::PermissionDenied
            } else {
                CwdObservation::Gone
            })
        }
        fn process_uids(&self, pid: i64) -> Option<ProcessUids> {
            (pid == 200).then_some(self.system_uids)
        }
        fn occupancy(&self, _: i64) -> OccupancyObservation {
            OccupancyObservation {
                paths: Vec::new(),
                complete: true,
                gap: None,
            }
        }
        fn sealed_system_image(&self, pid: i64) -> Option<SealedSystemImage> {
            (pid == 200).then_some(SealedSystemImage {
                setuid_transition: false,
            })
        }
        fn unreadable_policy_uids(
            &self,
            pid: i64,
            _: &[CensusIgnoreEntry],
        ) -> Result<UnreadablePolicyExcusal, provenant_os::pid::UnreadablePolicyFailure> {
            (pid == 200)
                .then_some(UnreadablePolicyExcusal {
                    uids: self.system_uids,
                })
                .ok_or_else(|| "no fixture policy evidence".into())
        }
    }
    impl WorkerObserver for WindowServerWorkers {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            observe_worker_roots(roots, self, self, &self.current, true).0
        }
        fn requires_complete_census(&self) -> bool {
            true
        }
    }
    let current = refused_fact(100);
    let mut windowserver = refused_fact(200);
    windowserver.command = Some("WindowServer".into());
    let system_uid = if provenant_os::fs::effective_uid() == 0 {
        1
    } else {
        0
    };
    let system_uids = ProcessUids {
        real: system_uid,
        effective: system_uid,
        saved: system_uid,
    };
    let workers = WindowServerWorkers {
        census: Census::from_observations(
            [current.clone(), windowserver],
            [],
            [],
            true,
            false,
            32,
        )?,
        current,
        system_uids,
    };
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let created = create(
        &test_context.context(),
        WorktreeCreate {
            branch: "landed".into(),
            base: None,
        },
        &cancel(),
    )?;
    fs::set_permissions(&created.path, fs::Permissions::from_mode(0o700))?;
    assert!(
        workers.observe_roots(std::slice::from_ref(&created.path))[&created.path].is_available()
    );
    let result = remove_with_port(
        &test_context,
        &WorktreeContext {
            workers: &workers,
            ..test_context.context()
        },
        WorktreeRemove::Name {
            name: "landed".into(),
            discard: false,
            integration: None,
            expected_head: None,
        },
        &cancel(),
    );
    assert!(
        result.is_ok(),
        "fixture daemon should be excluded: {result:?}"
    );
    assert!(!created.path.exists());
    assert!(!branch_exists(&fixture, "landed"));
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)] // A real child provides the cwd observation fixture.
fn system_observer_names_the_process_inside_a_tree() -> Result<(), Box<dyn Error>> {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::new()?;
    let path = canonicalize(fixture._temp.path())?.join("named-cwd");
    fs::create_dir(&path)?;
    let child = Child(
        std::process::Command::new("/bin/sh")
            .args(["-c", "read line"])
            .current_dir(&path)
            .stdin(std::process::Stdio::piped())
            .spawn()?,
    );
    let pid = i64::from(child.0.id());
    assert!(matches!(
        live_refusal(&SystemWorkerObserver, &path),
        WorktreeError::LiveProcess(process) if process.pid == pid
    ));
    Ok(())
}

#[test]
fn worker_census_places_a_process_in_a_deleted_directory_by_its_nearest_ancestor()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, CensusError, ProcessIdentity};
    struct Processes(Census);
    impl ProcessObserver for Processes {
        fn boot_identity(&self) -> Result<String, CensusError> {
            Ok("boot".into())
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            match self.0.process(pid) {
                ProcessLookup::Present(fact) => ProcessObservation::Present(fact.clone()),
                _ => ProcessObservation::Absent,
            }
        }
        fn census(&self, _: usize) -> Result<Census, CensusError> {
            Ok(self.0.clone())
        }
    }
    struct Cwd(PathBuf);
    impl CwdObserver for Cwd {
        fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
            provenant_os::pid::OccupancyObservation {
                paths: vec![],
                complete: true,
                gap: None,
            }
        }
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(CwdObservation::Path(self.0.clone()))
        }
    }
    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    let outside = canonicalize(fixture._temp.path())?;
    let fact = |pid: i64, session_id| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: session_id,
            session_id,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let current = fact(100, 100);
    let processes = Processes(Census::from_observations(
        [current.clone(), fact(200, 200)],
        [],
        [],
        true,
        false,
        32,
    )?);
    let observe = |cwd: PathBuf| {
        observe_worker_roots(
            std::slice::from_ref(&root),
            &processes,
            &Cwd(cwd),
            &current,
            false,
        )
        .0[&root]
            .clone()
    };
    // A deleted directory elsewhere is unrelated to the root.
    assert!(observe(outside.join("gone").join("deeper")).is_available());
    // A deleted subdirectory of the root still holds the process inside it.
    assert!(observe(root.join("gone").join("deeper")).is_observed());
    Ok(())
}

#[test]
fn land_worker_census_excludes_verified_ancestors_and_the_callers_children()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, CensusError, ProcessIdentity};
    struct Processes(Census);
    impl ProcessObserver for Processes {
        fn boot_identity(&self) -> Result<String, CensusError> {
            Ok("boot".into())
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            match self.0.process(pid) {
                ProcessLookup::Present(fact) => ProcessObservation::Present(fact.clone()),
                _ => ProcessObservation::Absent,
            }
        }
        fn census(&self, _: usize) -> Result<Census, CensusError> {
            Ok(self.0.clone())
        }
    }
    struct Cwds(BTreeMap<i64, PathBuf>);
    impl CwdObserver for Cwds {
        fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
            provenant_os::pid::OccupancyObservation {
                paths: vec![],
                complete: true,
                gap: None,
            }
        }
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(self
                .0
                .get(&pid)
                .cloned()
                .map(CwdObservation::Path)
                .unwrap_or(CwdObservation::Gone))
        }
    }
    let fixture = Fixture::new()?;
    let root = fixture.root.clone();
    let outside = fixture._temp.path().to_path_buf();
    let fact = |pid, parent_pid, session_id| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: session_id,
            session_id,
        },
        parent_pid,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let current = fact(100, 90, 10);
    let facts = vec![
        fact(1, 0, 1),
        fact(10, 1, 10),
        fact(80, 10, 10),
        fact(90, 80, 10),
        current.clone(),
        fact(95, 80, 10),
        fact(110, 100, 10),
        fact(200, 1, 200),
    ];
    for complete in [true, false] {
        let processes = Processes(Census::from_observations(
            facts.clone(),
            [],
            [],
            complete,
            false,
            32,
        )?);
        let mut cwds = Cwds(
            facts
                .iter()
                .map(|fact| {
                    (
                        fact.identity.pid,
                        if [10, 80, 90, 100].contains(&fact.identity.pid) {
                            root.clone()
                        } else {
                            outside.clone()
                        },
                    )
                })
                .collect(),
        );
        let observe = |cwds: &Cwds, ancestors| {
            observe_worker_roots(
                std::slice::from_ref(&root),
                &processes,
                cwds,
                &current,
                ancestors,
            )
            .0[&root]
                .clone()
        };
        assert!(observe(&cwds, false).is_observed());
        let state = observe(&cwds, true);
        assert_eq!(state.is_available(), complete);
        // Above-session ancestors, siblings and independent sessions remain
        // live, even when the census is incomplete.
        for pid in [1, 95, 200] {
            cwds.0.insert(pid, root.clone());
            assert!(observe(&cwds, true).is_observed(), "pid {pid}");
            cwds.0.insert(pid, outside.clone());
        }
        // The caller's verified child is part of the caller, not an
        // independent writer: a probe the caller spawned must not make the
        // worktree live even while it is still in the listing.
        let before_child = observe(&cwds, true);
        cwds.0.insert(110, root.clone());
        assert_eq!(observe(&cwds, true), before_child);
    }
    // Descendant exclusion is scoped to run admission, matching ancestor
    // exclusion: an ordinary census still names the caller's child as a
    // writer, while a land census treats it as part of the caller.
    {
        let processes = Processes(Census::from_observations(
            facts.clone(),
            [],
            [],
            true,
            false,
            32,
        )?);
        let mut cwds = Cwds(
            facts
                .iter()
                .map(|fact| (fact.identity.pid, outside.clone()))
                .collect(),
        );
        cwds.0.insert(110, root.clone()); // verified child of the caller
        let observe = |ancestors| {
            observe_worker_roots(
                std::slice::from_ref(&root),
                &processes,
                &cwds,
                &current,
                ancestors,
            )
            .0[&root]
                .clone()
        };
        assert!(
            observe(false).is_observed(),
            "ordinary census must name the caller's child"
        );
        assert!(
            observe(true).is_available(),
            "land census must exclude the caller's child"
        );
    }
    for ambiguity in [
        "equal_stamp",
        "younger_parent",
        "missing_hop",
        "cycle",
        "reused_current",
    ] {
        let mut ambiguous = facts.clone();
        match ambiguity {
            "equal_stamp" => {
                ambiguous
                    .iter_mut()
                    .find(|fact| fact.identity.pid == 90)
                    .ok_or("parent")?
                    .identity
                    .native_start = 100
            }
            "younger_parent" => {
                ambiguous
                    .iter_mut()
                    .find(|fact| fact.identity.pid == 90)
                    .ok_or("parent")?
                    .identity
                    .native_start = 101
            }
            "missing_hop" => ambiguous.retain(|fact| fact.identity.pid != 90),
            "cycle" => {
                ambiguous
                    .iter_mut()
                    .find(|fact| fact.identity.pid == 80)
                    .ok_or("parent")?
                    .parent_pid = 90
            }
            "reused_current" => {
                ambiguous
                    .iter_mut()
                    .find(|fact| fact.identity.pid == 100)
                    .ok_or("current")?
                    .identity
                    .native_start += 1
            }
            _ => unreachable!(),
        }
        let processes = Processes(Census::from_observations(
            ambiguous,
            [],
            [],
            true,
            false,
            32,
        )?);
        let mut cwds = Cwds(
            facts
                .iter()
                .map(|fact| (fact.identity.pid, outside.clone()))
                .collect(),
        );
        let observe = |cwds: &Cwds| {
            observe_worker_roots(
                std::slice::from_ref(&root),
                &processes,
                cwds,
                &current,
                true,
            )
            .0[&root]
                .clone()
        };
        // An ambiguous ancestry excludes no unverified ancestor, so it
        // says nothing about a root no process is inside.
        assert!(observe(&cwds).is_available(), "{ambiguity}");
        // The session leader is never verified once the walk stops, so
        // inside the root it still makes the root live.
        cwds.0.insert(10, root.clone());
        assert!(observe(&cwds).is_observed(), "{ambiguity}");
    }
    Ok(())
}

// Spec: docs/specs/supervisor.md#unreadable-processes-need-positive-non-occupancy-evidence
#[test]
fn a_vanished_descriptor_query_is_absent_but_live_eprem_stays_unknown() -> Result<(), Box<dyn Error>>
{
    use provenant_os::pid::OccupancyGap;
    use provenant_supervisor::census::{Census, CensusError, ProcessIdentity};

    struct GapObserver {
        census: Census,
        outside: PathBuf,
        gap: Option<OccupancyGap>,
    }
    impl ProcessObserver for GapObserver {
        fn boot_identity(&self) -> Result<String, CensusError> {
            Ok("boot".into())
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            match self.census.process(pid) {
                ProcessLookup::Present(fact) => ProcessObservation::Present(fact.clone()),
                _ => ProcessObservation::Absent,
            }
        }
        fn census(&self, _: usize) -> Result<Census, CensusError> {
            Ok(self.census.clone())
        }
    }
    impl CwdObserver for GapObserver {
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(CwdObservation::Path(self.outside.clone()))
        }
        fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
            provenant_os::pid::OccupancyObservation {
                paths: Vec::new(),
                complete: false,
                gap: self.gap,
            }
        }
    }

    let temp = tempfile::tempdir()?;
    let worktree = temp.path().join("worktree");
    provenant_os::fixture::private_state_dir(&worktree)?;
    let root = canonicalize(&worktree)?;
    let outside = canonicalize(temp.path())?;
    let fact = |pid: i64| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: 10,
            session_id: 10,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let current = fact(100);
    let state_for = |gap: OccupancyGap| -> Result<Occupancy, Box<dyn Error>> {
        let census =
            Census::from_observations([current.clone(), fact(200)], [], [], true, false, 32)?;
        let observer = GapObserver {
            census,
            outside: outside.clone(),
            gap: Some(gap),
        };
        Ok(observe_worker_roots(
            std::slice::from_ref(&root),
            &observer,
            &observer,
            &current,
            true,
        )
        .0[&root]
            .clone())
    };
    // ESRCH says the process exited mid-census: it holds nothing and cannot
    // make the worktree unknown.
    assert!(state_for(OccupancyGap::Vanished)?.is_available());
    // EPERM on a still-listed process is a real permission failure, so the
    // data-loss boundary keeps the worktree Unknown.
    assert!(state_for(OccupancyGap::DescriptorsDenied)?.is_unknown());
    Ok(())
}

/// The native census over injected process and directory observations,
/// with landing's demand for a complete census.
struct InjectedCensus {
    census: provenant_supervisor::census::Census,
    cwds: BTreeMap<i64, CwdObservation>,
    current: ProcessFact,
    platform_uids: BTreeMap<i64, ProcessUids>,
}

// Spec: docs/specs/disk.md#scratch-and-cache-custody
#[test]
fn unreadable_process_without_candidate_paths_only_blocks_the_root_it_occupies()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fixture = Fixture::new()?;
    let first = fixture._temp.path().join("candidate-a");
    let second = fixture._temp.path().join("candidate-b");
    fs::create_dir(&first)?;
    fs::create_dir(&second)?;
    let roots = [canonicalize(&first)?, canonicalize(&second)?];
    let current = ProcessFact {
        identity: ProcessIdentity {
            boot: "eperm-candidate-scope".into(),
            pid: 100,
            native_start: 100,
            group_id: 100,
            session_id: 100,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let census = Census::from_observations(
        [current.clone()],
        [(200, ObservationIssue::PermissionDenied)],
        [],
        true,
        false,
        32,
    )?;
    let observer = |cwd| InjectedCensus {
        census: census.clone(),
        cwds: BTreeMap::from([(200, cwd)]),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };

    let inside_first = observer(CwdObservation::Path(roots[0].clone()));
    let states = observe_worker_roots(&roots, &inside_first, &inside_first, &current, false).0;
    assert!(states[&roots[0]].is_observed());
    assert!(states[&roots[1]].is_available());

    let unreadable_outside = observer(CwdObservation::PermissionDenied);
    let mut warnings = Vec::new();
    let (states, _, _, _) = observe_worker_holders_with_taints_and_audit(
        &roots,
        &unreadable_outside,
        &unreadable_outside,
        &current,
        false,
        &mut |pid, condition| warnings.push((pid, condition)),
        &[],
    );
    assert!(states.values().all(Occupancy::is_available));
    assert_eq!(
        warnings,
        vec![(Some(200), Some(UNREADABLE_NO_CANDIDATE_PATH))]
    );
    assert!(mark_unreadable_process_warning("eperm-test", 200));
    assert!(!mark_unreadable_process_warning("eperm-test", 200));
    Ok(())
}

/// A real configured-entry matcher: canonical executable path, exact team id
/// and the live signing signal. A similar-looking path or wrong team never
/// matches, so a test cannot pass on a stubbed boolean.
fn match_configured_entry(
    executable: &Path,
    entries: &[CensusIgnoreEntry],
    platform_signed: bool,
    team_id: Option<&str>,
) -> Option<CensusIgnoreEntry> {
    let canonical = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
    entries
        .iter()
        .find(|entry| {
            entry.is_valid()
                && std::fs::canonicalize(&entry.path).unwrap_or_else(|_| entry.path.clone())
                    == canonical
                && if entry.team_id == "platform" {
                    platform_signed
                } else {
                    team_id == Some(entry.team_id.as_str())
                }
        })
        .cloned()
}

/// Typed signals for one unreadable process. Only typed observer evidence is
/// consulted: the `process_details` diagnostic string is never parsed.
struct UnreadableSignals {
    uids: Option<ProcessUids>,
    platform_signed: bool,
    /// The real configured entries the census must thread to the matcher.
    entries: Vec<CensusIgnoreEntry>,
    /// The live kernel team id used by non-`platform` entries.
    team_id: Option<String>,
    executable: Option<PathBuf>,
    cwd: CwdObservation,
    occupancy_paths: Vec<PathBuf>,
}

impl CwdObserver for UnreadableSignals {
    fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
        Ok(self.cwd.clone())
    }
    fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
        provenant_os::pid::OccupancyObservation {
            paths: self.occupancy_paths.clone(),
            complete: false,
            gap: Some(provenant_os::pid::OccupancyGap::DescriptorsDenied),
        }
    }
    fn process_uids(&self, _: i64) -> Option<ProcessUids> {
        self.uids
    }
    fn platform_signed_image(&self, _: i64) -> Option<provenant_os::pid::PlatformSignedImage> {
        self.platform_signed
            .then_some(provenant_os::pid::PlatformSignedImage)
    }
    /// Match the entries the census actually threaded, on the canonical
    /// executable path plus team id and live signing signal, exactly as the
    /// host matcher does. A similar-looking path or wrong team never matches.
    fn configured_unreadable_image(
        &self,
        _: i64,
        entries: &[CensusIgnoreEntry],
    ) -> Option<CensusIgnoreEntry> {
        match_configured_entry(
            self.executable.as_deref()?,
            entries,
            self.platform_signed,
            self.team_id.as_deref(),
        )
    }
    fn unreadable_policy_uids(
        &self,
        _: i64,
        _: &[CensusIgnoreEntry],
    ) -> Result<
        provenant_os::pid::UnreadablePolicyExcusal,
        provenant_os::pid::UnreadablePolicyFailure,
    > {
        self.uids
            .filter(|uids| uids.all_system())
            .map(|uids| provenant_os::pid::UnreadablePolicyExcusal { uids })
            .ok_or_else(|| "image policy evidence unavailable".into())
    }
    fn executable(&self, _: i64) -> Option<PathBuf> {
        self.executable.clone()
    }
    fn process_details(&self, _: i64) -> Option<String> {
        Some("path=/usr/libexec/UserEventAgent, ruid=0, team_id=platform".into())
    }
}

/// One census outcome: the root state and detail, the unplaceable audit
/// conditions and the typed warnings raised while observing it.
struct UnreadableOutcome {
    state: Occupancy,
    detail: String,
    audit: Vec<(Option<i64>, Option<&'static str>)>,
    warnings: Vec<provenant_records::warning::Warning>,
}

/// Run one census with pid 200 unreadable beside `root`.
fn observe_unreadable(
    root: &Path,
    signals: &UnreadableSignals,
) -> Result<UnreadableOutcome, Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ObservationIssue};
    let current = refused_fact(100);
    let processes = InjectedCensus {
        census: Census::from_observations(
            [current.clone()],
            [(200, ObservationIssue::PermissionDenied)],
            [],
            true,
            false,
            32,
        )?,
        cwds: BTreeMap::new(),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };
    let roots = [root.to_path_buf()];
    let (observed, warnings) = provenant_records::warning::capture(|| {
        let mut audit = Vec::new();
        let (states, _, details, _) = observe_worker_holders_with_taints_and_audit(
            &roots,
            &processes,
            signals,
            &current,
            false,
            &mut |pid, condition| audit.push((pid, condition)),
            &signals.entries,
        );
        (
            states[&roots[0]].clone(),
            details.get(&roots[0]).cloned().unwrap_or_default(),
            audit,
        )
    });
    Ok(UnreadableOutcome {
        state: observed.0,
        detail: observed.1,
        audit: observed.2,
        warnings,
    })
}

fn warning_names_process(warnings: &[provenant_records::warning::Warning], pid: i64) -> bool {
    warnings
        .iter()
        .any(|warning| warning.subject.as_deref() == Some(format!("process {pid}").as_str()))
}

fn warning_names_path(warnings: &[provenant_records::warning::Warning], path: &str) -> bool {
    warnings
        .iter()
        .any(|warning| warning.message.contains(path))
}

/// The listed-uid policy excuses an unplaceable occupancy path when every
/// credential is a service uid foreign to the root owner. This pins the path
/// placement failure only: a root whose own metadata is unreadable fails closed
/// (see `unreadable_root_metadata_keeps_the_root_unknown`). The warning names
/// the pid and the executable path.
#[test]
fn listed_unplaceable_descriptor_process_frees_the_root_with_a_warning()
-> Result<(), Box<dyn Error>> {
    use provenant_os::fs::effective_uid;
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-launchd");
    fs::create_dir(&root)?;
    let blocker = root.join("blocker");
    fs::write(&blocker, b"x")?;
    let service = if effective_uid() == 0 { 1 } else { 0 };
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: service,
            effective: service,
            saved: service,
        }),
        platform_signed: false,
        entries: Vec::new(),
        team_id: None,
        executable: Some(PathBuf::from("/sbin/launchd")),
        cwd: CwdObservation::PermissionDenied,
        occupancy_paths: vec![blocker.join("descriptor")],
    };
    let UnreadableOutcome {
        state,
        detail,
        audit,
        warnings,
    } = observe_unreadable(&root, &signals)?;
    assert!(state.is_available(), "{state:?}: {detail}");
    assert!(
        audit.iter().any(|(pid, condition)| *pid == Some(200)
            && condition == &Some("listed service uid cannot write a lane checkout")),
        "{audit:?}"
    );
    assert!(warning_names_process(&warnings, 200), "{warnings:?}");
    assert!(
        warning_names_path(&warnings, "/sbin/launchd"),
        "{warnings:?}"
    );
    Ok(())
}

/// The configured knob extends the built-in defaults to every failure class.
/// An owner-uid process with a configured canonical path and team id is excused
/// even though the listed-uid policy alone would not apply. The matcher below
/// exercises the real entries threaded through the census, not a stubbed flag.
#[test]
fn configured_entry_excuses_unplaceable_descriptor_process() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-configured-path");
    fs::create_dir(&root)?;
    let blocker = root.join("blocker");
    fs::write(&blocker, b"x")?;
    let owner = provenant_os::fs::effective_uid();
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: owner,
            effective: owner,
            saved: owner,
        }),
        platform_signed: true,
        entries: vec![CensusIgnoreEntry {
            path: PathBuf::from("/usr/libexec/UserEventAgent"),
            team_id: "platform".into(),
            started_before: None,
        }],
        team_id: None,
        executable: Some(PathBuf::from("/usr/libexec/UserEventAgent")),
        cwd: CwdObservation::PermissionDenied,
        occupancy_paths: vec![blocker.join("descriptor")],
    };
    let UnreadableOutcome {
        state,
        detail,
        warnings,
        ..
    } = observe_unreadable(&root, &signals)?;
    assert!(state.is_available(), "{state:?}: {detail}");
    assert!(warning_names_process(&warnings, 200), "{warnings:?}");
    assert!(
        warning_names_path(&warnings, "/usr/libexec/UserEventAgent"),
        "{warnings:?}"
    );
    Ok(())
}

/// The counterpart to `configured_entry_with_unreadable_root_metadata_keeps_root_unknown`:
/// with every root readable, a configured entry does excuse a process whose own
/// occupancy path cannot be placed. The two causes must not be folded together.
#[test]
fn configured_entry_excuses_unplaceable_process_path_with_readable_roots()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("configured-unplaceable-readable-root");
    fs::create_dir(&root)?;
    let blocker = root.join("blocker");
    fs::write(&blocker, b"x")?;
    let owner = provenant_os::fs::effective_uid();
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: owner,
            effective: owner,
            saved: owner,
        }),
        platform_signed: true,
        entries: vec![CensusIgnoreEntry {
            path: PathBuf::from("/usr/libexec/UserEventAgent"),
            team_id: "platform".into(),
            started_before: None,
        }],
        team_id: None,
        executable: Some(PathBuf::from("/usr/libexec/UserEventAgent")),
        cwd: CwdObservation::PermissionDenied,
        // A descriptor beneath a regular file cannot be placed by directory
        // identity, while the root's own metadata stays readable.
        occupancy_paths: vec![blocker.join("descriptor")],
    };
    let outcome = observe_unreadable(&root, &signals)?;
    assert!(
        outcome.state.is_available(),
        "{:?}: {}",
        outcome.state,
        outcome.detail
    );
    assert!(
        outcome
            .audit
            .iter()
            .any(|(pid, condition)| *pid == Some(200)
                && condition == &Some("configured unreadable image exception")),
        "{:?}",
        outcome.audit
    );
    Ok(())
}

/// A configured entry whose canonical path does not match the executable must
/// not excuse: the matcher exercises the real entries threaded by the census.
#[test]
fn mismatched_configured_entry_does_not_excuse() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-wrong-configured-entry");
    fs::create_dir(&root)?;
    let blocker = root.join("blocker");
    fs::write(&blocker, b"x")?;
    let owner = provenant_os::fs::effective_uid();
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: owner,
            effective: owner,
            saved: owner,
        }),
        platform_signed: true,
        entries: vec![CensusIgnoreEntry {
            path: PathBuf::from("/usr/libexec/UserEventAgent"),
            team_id: "platform".into(),
            started_before: None,
        }],
        team_id: None,
        executable: Some(PathBuf::from("/usr/bin/true")),
        cwd: CwdObservation::PermissionDenied,
        occupancy_paths: vec![blocker.join("descriptor")],
    };
    let outcome = observe_unreadable(&root, &signals)?;
    assert!(
        outcome.state.is_unknown(),
        "{:?}: {}",
        outcome.state,
        outcome.detail
    );
    assert!(
        !warning_names_process(&outcome.warnings, 200),
        "{:?}",
        outcome.warnings
    );
    Ok(())
}

/// A setuid-root platform binary launched by the owner has a real owner uid but
/// elevated effective and saved uids. All three must be owner uids, so it is
/// not an owner daemon and must still block.
#[test]
fn setuid_owner_platform_daemon_with_denied_descriptors_blocks() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-setuid-daemon");
    fs::create_dir(&root)?;
    let owner = provenant_os::fs::effective_uid();
    let elevated = if owner == 0 { 1 } else { 0 };
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: owner,
            effective: elevated,
            saved: elevated,
        }),
        platform_signed: true,
        entries: Vec::new(),
        team_id: None,
        executable: Some(PathBuf::from("/usr/libexec/UserEventAgent")),
        cwd: CwdObservation::Path(PathBuf::from("/")),
        occupancy_paths: Vec::new(),
    };
    let outcome = observe_unreadable(&root, &signals)?;
    assert!(
        outcome.state.is_unknown(),
        "{:?}: {}",
        outcome.state,
        outcome.detail
    );
    assert!(
        !warning_names_process(&outcome.warnings, 200),
        "{:?}",
        outcome.warnings
    );
    Ok(())
}

/// A root whose own metadata cannot be read cannot be proven foreign to a
/// listed service uid. The excusal fails closed and the root stays Unknown.
#[test]
fn unreadable_root_metadata_keeps_the_root_unknown() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let not_a_dir = fixture.root.join("root-metadata-not-a-dir");
    fs::write(&not_a_dir, b"x")?;
    let root = not_a_dir.join("child");
    let service = if provenant_os::fs::effective_uid() == 0 {
        1
    } else {
        0
    };
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: service,
            effective: service,
            saved: service,
        }),
        platform_signed: false,
        entries: Vec::new(),
        team_id: None,
        executable: Some(PathBuf::from("/sbin/launchd")),
        cwd: CwdObservation::PermissionDenied,
        occupancy_paths: vec![root.join("descriptor")],
    };
    let outcome = observe_unreadable(&root, &signals)?;
    assert!(
        outcome.state.is_unknown(),
        "{:?}: {}",
        outcome.state,
        outcome.detail
    );
    assert!(
        !warning_names_process(&outcome.warnings, 200),
        "{:?}",
        outcome.warnings
    );
    Ok(())
}

/// A configured entry must not excuse a process when a root's own metadata is
/// unreadable: placement for that root is undecidable, so it fails closed for
/// every arm, configured included, and the root stays Unknown.
#[test]
fn configured_entry_with_unreadable_root_metadata_keeps_root_unknown() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let not_a_dir = fixture.root.join("configured-root-metadata-not-a-dir");
    fs::write(&not_a_dir, b"x")?;
    let root = not_a_dir.join("child");
    let owner = provenant_os::fs::effective_uid();
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: owner,
            effective: owner,
            saved: owner,
        }),
        platform_signed: true,
        entries: vec![CensusIgnoreEntry {
            path: PathBuf::from("/usr/libexec/UserEventAgent"),
            team_id: "platform".into(),
            started_before: None,
        }],
        team_id: None,
        executable: Some(PathBuf::from("/usr/libexec/UserEventAgent")),
        cwd: CwdObservation::PermissionDenied,
        occupancy_paths: vec![root.join("descriptor")],
    };
    let outcome = observe_unreadable(&root, &signals)?;
    assert!(
        outcome.state.is_unknown(),
        "{:?}: {}",
        outcome.state,
        outcome.detail
    );
    assert!(
        !warning_names_process(&outcome.warnings, 200),
        "{:?}",
        outcome.warnings
    );
    Ok(())
}

/// A configured process whose readable cwd is inside the root is positive
/// placement and must not be excused when its descriptors are EPERM. This
/// drives the facts loop (a listed fact, not an unknown pid) so it pins the
/// placement-before-excusal structure: the configured arm never `continue`s.
#[test]
fn configured_entry_with_cwd_inside_root_and_denied_descriptors_blocks()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::Census;
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-configured-inside");
    fs::create_dir(&root)?;
    let current = refused_fact(100);
    let processes = InjectedCensus {
        census: Census::from_observations(
            [current.clone(), refused_fact(200)],
            [],
            [],
            true,
            false,
            32,
        )?,
        cwds: BTreeMap::from([(200, CwdObservation::Path(root.clone()))]),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };
    let executable = PathBuf::from("/usr/libexec/UserEventAgent");
    let entries = [CensusIgnoreEntry {
        path: executable.clone(),
        team_id: "platform".into(),
        started_before: None,
    }];
    struct ConfiguredInside<'a> {
        inner: &'a InjectedCensus,
        executable: &'a Path,
    }
    impl ProcessObserver for ConfiguredInside<'_> {
        fn boot_identity(&self) -> Result<String, provenant_supervisor::census::CensusError> {
            self.inner.boot_identity()
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            self.inner.observe(pid)
        }
        fn census(
            &self,
            limit: usize,
        ) -> Result<provenant_supervisor::census::Census, provenant_supervisor::census::CensusError>
        {
            self.inner.census(limit)
        }
    }
    impl CwdObserver for ConfiguredInside<'_> {
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            self.inner.cwd(pid)
        }
        fn process_uids(&self, pid: i64) -> Option<ProcessUids> {
            self.inner.process_uids(pid)
        }
        fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
            provenant_os::pid::OccupancyObservation {
                paths: Vec::new(),
                complete: false,
                gap: Some(provenant_os::pid::OccupancyGap::DescriptorsDenied),
            }
        }
        fn configured_unreadable_image(
            &self,
            _: i64,
            entries: &[CensusIgnoreEntry],
        ) -> Option<CensusIgnoreEntry> {
            match_configured_entry(self.executable, entries, true, None)
        }
    }
    let observer = ConfiguredInside {
        inner: &processes,
        executable: &executable,
    };
    let roots = [root.clone()];
    let (states, _, details, _) = observe_worker_holders_with_taints_and_audit(
        &roots,
        &observer,
        &observer,
        &current,
        false,
        &mut |_, _| {},
        &entries,
    );
    assert!(
        states[&root].is_observed(),
        "{:?}: {:?}",
        states[&root],
        details.get(&root)
    );
    Ok(())
}

/// An owner-uid process with EPERM-unreadable descriptors is excused only when
/// it is platform-signed, lives in an approved daemon directory and has a
/// readable cwd placed outside every scoped root by directory identity. A cwd
/// of `/` is outside a root under `/Users`: textual ancestry of an ancestor is
/// not occupancy evidence.
#[test]
fn owner_platform_daemon_with_root_cwd_is_excused() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-platform-daemon");
    fs::create_dir(&root)?;
    let owner = provenant_os::fs::effective_uid();
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: owner,
            effective: owner,
            saved: owner,
        }),
        platform_signed: true,
        entries: Vec::new(),
        team_id: None,
        executable: Some(PathBuf::from("/usr/libexec/UserEventAgent")),
        cwd: CwdObservation::Path(PathBuf::from("/")),
        occupancy_paths: Vec::new(),
    };
    let UnreadableOutcome {
        state,
        detail,
        warnings,
        ..
    } = observe_unreadable(&root, &signals)?;
    assert!(state.is_available(), "{state:?}: {detail}");
    assert!(warning_names_process(&warnings, 200), "{warnings:?}");
    assert!(
        warning_names_path(&warnings, "/usr/libexec/UserEventAgent"),
        "{warnings:?}"
    );
    Ok(())
}

/// The same platform daemon shape under /bin is a user tool, never excused.
#[test]
fn owner_platform_daemon_under_bin_still_blocks() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-bin-tool");
    fs::create_dir(&root)?;
    let owner = provenant_os::fs::effective_uid();
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: owner,
            effective: owner,
            saved: owner,
        }),
        platform_signed: true,
        entries: Vec::new(),
        team_id: None,
        executable: Some(PathBuf::from("/bin/zsh")),
        cwd: CwdObservation::Path(PathBuf::from("/tmp")),
        occupancy_paths: Vec::new(),
    };
    let UnreadableOutcome {
        state,
        detail,
        warnings,
        ..
    } = observe_unreadable(&root, &signals)?;
    assert!(state.is_unknown(), "{state:?}: {detail}");
    assert!(detail.contains("process 200"), "{detail}");
    assert!(!warning_names_process(&warnings, 200), "{warnings:?}");
    Ok(())
}

/// A platform daemon whose cwd the kernel refuses cannot prove it is outside
/// the root, so it still blocks.
#[test]
fn owner_platform_daemon_with_unreadable_cwd_still_blocks() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-daemon-cwd");
    fs::create_dir(&root)?;
    let owner = provenant_os::fs::effective_uid();
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: owner,
            effective: owner,
            saved: owner,
        }),
        platform_signed: true,
        entries: Vec::new(),
        team_id: None,
        executable: Some(PathBuf::from("/usr/libexec/UserEventAgent")),
        cwd: CwdObservation::PermissionDenied,
        occupancy_paths: Vec::new(),
    };
    let UnreadableOutcome {
        state,
        detail,
        warnings,
        ..
    } = observe_unreadable(&root, &signals)?;
    assert!(state.is_unknown(), "{state:?}: {detail}");
    assert!(detail.contains("process 200"), "{detail}");
    assert!(!warning_names_process(&warnings, 200), "{warnings:?}");
    Ok(())
}

/// A readable cwd inside the root is positive occupancy evidence and always
/// wins over a platform-signed image.
#[test]
fn owner_platform_daemon_with_cwd_inside_root_still_blocks() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-daemon-inside");
    fs::create_dir(&root)?;
    let owner = provenant_os::fs::effective_uid();
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: owner,
            effective: owner,
            saved: owner,
        }),
        platform_signed: true,
        entries: Vec::new(),
        team_id: None,
        executable: Some(PathBuf::from("/usr/libexec/UserEventAgent")),
        cwd: CwdObservation::Path(root.clone()),
        occupancy_paths: Vec::new(),
    };
    let UnreadableOutcome {
        state,
        detail,
        warnings,
        ..
    } = observe_unreadable(&root, &signals)?;
    assert!(state.is_observed(), "{state:?}: {detail}");
    assert!(detail.contains("process 200"), "{detail}");
    assert!(!warning_names_process(&warnings, 200), "{warnings:?}");
    Ok(())
}

/// A human uid is never excused through the listed-uid policy, even when the
/// image and cwd signals match the built-in platform-daemon default.
#[test]
fn human_uid_platform_process_still_blocks() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-human-uid");
    fs::create_dir(&root)?;
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: 500,
            effective: 500,
            saved: 500,
        }),
        platform_signed: true,
        entries: Vec::new(),
        team_id: None,
        executable: Some(PathBuf::from("/usr/libexec/UserEventAgent")),
        cwd: CwdObservation::Path(PathBuf::from("/tmp")),
        occupancy_paths: Vec::new(),
    };
    let UnreadableOutcome {
        state,
        detail,
        warnings,
        ..
    } = observe_unreadable(&root, &signals)?;
    assert!(state.is_unknown(), "{state:?}: {detail}");
    assert!(detail.contains("process 200"), "{detail}");
    assert!(!warning_names_process(&warnings, 200), "{warnings:?}");
    Ok(())
}

/// A same-user process whose readable executable is inside the worktree could
/// still be its writer. The executable is injected as an occupancy path so this
/// pins real executable placement, not just an unused signal.
#[test]
fn unreadable_same_user_process_inside_the_worktree_still_blocks() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-inside-process");
    fs::create_dir(&root)?;
    let own = provenant_os::fs::effective_uid();
    let executable = root.join("writer");
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: own,
            effective: own,
            saved: own,
        }),
        platform_signed: false,
        entries: Vec::new(),
        team_id: None,
        executable: Some(executable.clone()),
        cwd: CwdObservation::PermissionDenied,
        occupancy_paths: vec![executable],
    };
    let UnreadableOutcome {
        state,
        detail,
        warnings,
        ..
    } = observe_unreadable(&root, &signals)?;
    assert!(state.is_observed(), "{state:?}: {detail}");
    assert!(detail.contains("process 200"), "{detail}");
    assert!(!warning_names_process(&warnings, 200), "{warnings:?}");
    Ok(())
}

/// A readable executable and cwd outside the root are not enough on their own:
/// the broad "different ruid" and "outside paths" excusals were removed, so an
/// unsigned user tool with EPERM descriptors still blocks.
#[test]
fn unreadable_unsigned_user_tool_outside_root_still_blocks() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-outside-process");
    fs::create_dir(&root)?;
    let own = provenant_os::fs::effective_uid();
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: own,
            effective: own,
            saved: own,
        }),
        platform_signed: false,
        entries: Vec::new(),
        team_id: None,
        executable: Some(PathBuf::from("/usr/bin/true")),
        cwd: CwdObservation::Path(PathBuf::from("/tmp")),
        occupancy_paths: Vec::new(),
    };
    let UnreadableOutcome {
        state,
        detail,
        warnings,
        ..
    } = observe_unreadable(&root, &signals)?;
    assert!(state.is_unknown(), "{state:?}: {detail}");
    assert!(detail.contains("process 200"), "{detail}");
    assert!(!warning_names_process(&warnings, 200), "{warnings:?}");
    Ok(())
}

/// With neither a readable executable nor a readable cwd, a same-user process
/// could be inside, so the census keeps the root unknown and names its PID.
#[test]
fn unreadable_same_user_process_without_readable_paths_still_blocks() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let root = fixture.root.join("unreadable-no-paths-process");
    fs::create_dir(&root)?;
    let own = provenant_os::fs::effective_uid();
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: own,
            effective: own,
            saved: own,
        }),
        platform_signed: false,
        entries: Vec::new(),
        team_id: None,
        executable: None,
        cwd: CwdObservation::PermissionDenied,
        occupancy_paths: Vec::new(),
    };
    let UnreadableOutcome {
        state,
        detail,
        warnings,
        ..
    } = observe_unreadable(&root, &signals)?;
    assert!(state.is_unknown(), "{state:?}: {detail}");
    assert!(detail.contains("process 200"), "{detail}");
    assert!(!warning_names_process(&warnings, 200), "{warnings:?}");
    Ok(())
}

// Spec: docs/specs/disk.md#scratch-and-cache-custody
#[test]
fn unreadable_warning_counts_only_processes_that_could_own_the_checkout()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("eperm-cohort");
    fs::create_dir(&root)?;
    let owner = provenant_os::fs::effective_uid();
    let foreign_uid = if owner == 0 { 501 } else { 0 };
    let own_uids = ProcessUids {
        real: owner,
        effective: owner,
        saved: owner,
    };
    let foreign_uids = ProcessUids {
        real: foreign_uid,
        effective: foreign_uid,
        saved: foreign_uid,
    };
    let current = refused_fact(100);
    let census = provenant_supervisor::census::Census::from_observations(
        [current.clone()],
        [
            (200, ObservationIssue::PermissionDenied),
            (201, ObservationIssue::PermissionDenied),
            (202, ObservationIssue::PermissionDenied),
        ],
        [],
        true,
        false,
        32,
    )?;
    let observer = InjectedCensus {
        census,
        cwds: BTreeMap::from([
            (200, CwdObservation::PermissionDenied),
            (201, CwdObservation::PermissionDenied),
            (202, CwdObservation::PermissionDenied),
        ]),
        current,
        platform_uids: BTreeMap::from([(200, foreign_uids), (201, own_uids)]),
    };
    let roots = [root];
    // A foreign uid cannot hold the same-user checkout, so it is not unknown
    // custody for this warning. Unreadable credentials stay in the cohort.
    assert_eq!(
        unreadable_cohort_pids(&BTreeSet::from([200]), &observer, &roots),
        BTreeSet::new()
    );
    assert_eq!(
        unreadable_cohort_pids(&BTreeSet::from([200, 201, 202]), &observer, &roots),
        BTreeSet::from([201, 202])
    );
    Ok(())
}

impl ProcessObserver for InjectedCensus {
    fn boot_identity(&self) -> Result<String, provenant_supervisor::census::CensusError> {
        Ok("boot".into())
    }
    fn observe(&self, pid: i64) -> ProcessObservation {
        match self.census.process(pid) {
            ProcessLookup::Present(fact) => ProcessObservation::Present(fact.clone()),
            _ => ProcessObservation::Absent,
        }
    }
    fn census(
        &self,
        _: usize,
    ) -> Result<provenant_supervisor::census::Census, provenant_supervisor::census::CensusError>
    {
        Ok(self.census.clone())
    }
}

impl CwdObserver for InjectedCensus {
    fn process_uids(&self, pid: i64) -> Option<ProcessUids> {
        self.platform_uids.get(&pid).copied()
    }

    fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
        provenant_os::pid::OccupancyObservation {
            paths: vec![],
            complete: true,
            gap: None,
        }
    }
    fn unreadable_policy_uids(
        &self,
        pid: i64,
        _: &[CensusIgnoreEntry],
    ) -> Result<
        provenant_os::pid::UnreadablePolicyExcusal,
        provenant_os::pid::UnreadablePolicyFailure,
    > {
        self.platform_uids
            .get(&pid)
            .copied()
            .filter(|uids| uids.all_system())
            .map(|uids| provenant_os::pid::UnreadablePolicyExcusal { uids })
            .ok_or_else(|| "image policy evidence unavailable".into())
    }
    fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
        Ok(self.cwds.get(&pid).cloned().unwrap_or(CwdObservation::Gone))
    }
}

impl WorkerObserver for InjectedCensus {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        observe_worker_roots(roots, self, self, &self.current, true).0
    }
    fn requires_complete_census(&self) -> bool {
        true
    }
    fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
        observe_worker_roots(roots, self, self, &self.current, true)
    }
}

fn refused_fact(pid: i64) -> provenant_supervisor::census::ProcessFact {
    use provenant_supervisor::census::ProcessIdentity;
    ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    }
}

#[test]
fn occupancy_is_placed_per_root_and_partial_results_remain_conservative()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    struct FixtureOccupancy {
        cwd: PathBuf,
        paths: Vec<PathBuf>,
        complete: bool,
    }
    impl CwdObserver for FixtureOccupancy {
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(CwdObservation::Path(self.cwd.clone()))
        }
        fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
            provenant_os::pid::OccupancyObservation {
                paths: self.paths.clone(),
                complete: self.complete,
                gap: None,
            }
        }
    }
    let fixture = Fixture::new()?;
    let base = canonicalize(fixture._temp.path())?;
    let a = base.join("a");
    let b = base.join("b");
    fs::create_dir(&a)?;
    fs::create_dir(&b)?;
    let file = a.join("open");
    fs::write(&file, "open")?;
    let fact = |pid| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let current = fact(100);
    let processes = InjectedCensus {
        census: Census::from_observations([current.clone(), fact(200)], [], [], true, false, 32)?,
        cwds: BTreeMap::new(),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };
    let roots = vec![a.clone(), b.clone()];
    let observe = |paths, complete| {
        struct FixtureObserver<'a> {
            processes: &'a InjectedCensus,
            cwd: FixtureOccupancy,
            current: &'a ProcessFact,
        }
        impl WorkerObserver for FixtureObserver<'_> {
            fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
                self.observe_evidence(roots, false).states
            }
            fn observe_evidence(&self, roots: &[PathBuf], _: bool) -> WorkerEvidence {
                let (states, holders, details) =
                    observe_worker_holders(roots, self.processes, &self.cwd, self.current, false);
                WorkerEvidence {
                    states,
                    holders,
                    details,
                    ..WorkerEvidence::default()
                }
            }
        }
        let observer = FixtureObserver {
            processes: &processes,
            cwd: FixtureOccupancy {
                cwd: base.clone(),
                paths,
                complete,
            },
            current: &current,
        };
        let clock = provenant_core::TestClock::new(
            provenant_core::WallTime::from_unix_seconds(0).expect("valid fixture time"),
            provenant_core::MonoTime::from_micros(1),
        );
        CensusSession::new(&observer, &clock).observe_roots(&roots)
    };
    // The resolved ancestor places an unresolvable cwd under a.
    let unplaceable = a.join("cwd-loop");
    std::os::unix::fs::symlink("cwd-loop", &unplaceable)?;
    let states = observe_worker_roots(
        &roots,
        &processes,
        &FixtureOccupancy {
            cwd: unplaceable,
            paths: vec![],
            complete: true,
        },
        &current,
        false,
    )
    .0;
    assert!(states[&a].is_unknown());
    assert!(states[&b].is_available());
    let free = observe(vec![], true);
    assert!(free[&a].is_available());
    assert!(free[&b].is_available());
    let live = observe(vec![file.clone()], true);
    assert!(live[&a].is_observed());
    assert!(live[&b].is_available());
    let partial = observe(vec![file], false);
    assert!(partial[&a].is_observed());
    assert!(matches!(
        &partial[&b],
        Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Census }
            if scope == &b && !evidence.is_empty()
    ));

    // A symlink loop makes canonicalisation fail without relying on permissions.
    let outside = base.join("loop");
    std::os::unix::fs::symlink("loop", &outside)?;
    let outside_states = observe(vec![outside], true);
    assert!(outside_states[&a].is_unknown());
    assert!(outside_states[&b].is_unknown());
    let inside = a.join("loop");
    std::os::unix::fs::symlink("loop", &inside)?;
    let alias = if let Ok(suffix) = inside.strip_prefix("/private") {
        Path::new("/").join(suffix)
    } else {
        Path::new("/private").join(inside.strip_prefix("/")?)
    };
    let alias_root = alias.parent().unwrap().to_path_buf();
    let (alias_states, alias_details) = observe_worker_roots(
        &[alias_root.clone(), b.clone()],
        &processes,
        &FixtureOccupancy {
            cwd: base.clone(),
            paths: vec![inside.clone()],
            complete: true,
        },
        &current,
        false,
    );
    assert!(alias_states[&alias_root].is_unknown());
    assert!(alias_states[&b].is_available());
    assert!(alias_details[&alias_root].contains(&inside.display().to_string()));
    {
        let path = inside;
        let (states, details) = observe_worker_roots(
            &roots,
            &processes,
            &FixtureOccupancy {
                cwd: base.clone(),
                paths: vec![path.clone()],
                complete: true,
            },
            &current,
            false,
        );
        assert!(states[&a].is_unknown());
        assert!(states[&b].is_available());
        assert!(details[&a].contains(&path.display().to_string()));
    }
    for path in [PathBuf::new(), PathBuf::from("relative")] {
        let states = observe(vec![path], true);
        assert!(states[&a].is_unknown());
        assert!(states[&b].is_unknown());
    }
    Ok(())
}

#[test]
fn occupancy_failures_retain_every_root_and_positive_occupants_stay_live()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    use std::cell::Cell;
    struct FixtureOccupancy {
        base: PathBuf,
        paths: Vec<PathBuf>,
        complete: bool,
        supported: bool,
    }
    impl CwdObserver for FixtureOccupancy {
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(CwdObservation::Path(self.base.clone()))
        }
        fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
            if !self.supported {
                return provenant_os::pid::OccupancyObservation::default();
            }
            provenant_os::pid::OccupancyObservation {
                paths: self.paths.clone(),
                complete: self.complete,
                gap: None,
            }
        }
    }
    /// The second and later queries of one pid see a different process.
    struct Reused<'a>(&'a InjectedCensus, Cell<u32>);
    impl ProcessObserver for Reused<'_> {
        fn boot_identity(&self) -> Result<String, provenant_supervisor::census::CensusError> {
            self.0.boot_identity()
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            let observed = self.0.observe(pid);
            if pid != 200 {
                return observed;
            }
            self.1.set(self.1.get() + 1);
            match observed {
                // The census listing and first recheck agree; later checks do not.
                ProcessObservation::Present(mut fact) if self.1.get() > 1 => {
                    fact.identity.native_start += 1;
                    ProcessObservation::Present(fact)
                }
                other => other,
            }
        }
        fn census(&self, max: usize) -> Result<Census, provenant_supervisor::census::CensusError> {
            self.0.census(max)
        }
    }
    let fixture = Fixture::new()?;
    let base = canonicalize(fixture._temp.path())?;
    let a = base.join("a");
    let b = base.join("b");
    fs::create_dir(&a)?;
    fs::create_dir(&b)?;
    let fact = |pid: i64| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let current = fact(100);
    let processes = InjectedCensus {
        census: Census::from_observations([current.clone(), fact(200)], [], [], true, false, 32)?,
        cwds: BTreeMap::new(),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };
    let roots = vec![a.clone(), b.clone()];
    let occupancy = |paths: Vec<PathBuf>, complete, supported| FixtureOccupancy {
        base: base.clone(),
        paths,
        complete,
        supported,
    };
    let states = |observer: &dyn ProcessObserver, cwd: &FixtureOccupancy| {
        observe_worker_roots(&roots, observer, cwd, &current, false).0
    };
    // An executable inside one root, with the other root provably unrelated.
    let live = states(&processes, &occupancy(vec![b.join("bin/tool")], true, true));
    assert!(live[&a].is_available());
    assert!(live[&b].is_observed());
    // A reused pid cannot prove a snapshot belongs to the census process.
    let reused = states(
        &Reused(&processes, Cell::new(0)),
        &occupancy(vec![], true, true),
    );
    assert!(reused[&a].is_unknown());
    assert!(reused[&b].is_unknown());
    // An unsupported query (default observation) is not an empty one.
    let unsupported = states(&processes, &occupancy(vec![], true, false));
    assert!(unsupported[&a].is_unknown());
    assert!(unsupported[&b].is_unknown());
    // A positive occupant stays live beside incompleteness elsewhere.
    let partial = states(&processes, &occupancy(vec![a.join("x")], false, true));
    assert!(partial[&a].is_observed());
    assert!(partial[&b].is_unknown());
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn native_worker_census_places_both_data_firmlink_spellings() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fixture = Fixture::new()?;
    let resolved = canonicalize(&fixture.root)?;
    let root = resolved
        .strip_prefix("/System/Volumes/Data")
        .unwrap_or(&resolved);
    let data = Path::new("/System/Volumes/Data").join(root.strip_prefix("/")?);
    assert_ne!(root, data);
    let fact = |pid| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: Some("occupant".into()),
    };
    let mut observer = InjectedCensus {
        census: Census::from_observations([fact(100), fact(200)], [], [], true, false, 32)?,
        cwds: BTreeMap::new(),
        current: fact(100),
        platform_uids: BTreeMap::new(),
    };
    let sibling = root.with_file_name("outside");
    fs::create_dir(&sibling)?;
    fs::create_dir(root.join("child"))?;
    for (registered, reported) in [
        (root.to_path_buf(), data.clone()),
        (data, root.to_path_buf()),
    ] {
        for cwd in [
            reported.clone(),
            reported.join("child"),
            reported.join("deleted/descendant"),
            sibling.clone(),
        ] {
            observer.cwds.insert(200, CwdObservation::Path(cwd.clone()));
            let (states, holders, _) = observe_worker_holders(
                std::slice::from_ref(&registered),
                &observer,
                &observer,
                &observer.current,
                false,
            );
            let inside = cwd != sibling;
            assert_eq!(
                states[&registered].is_observed(),
                inside,
                "root={registered:?}, cwd={cwd:?}"
            );
            assert_eq!(
                holders.get(&registered).map(|holder| holder.pid),
                inside.then_some(200)
            );
        }
    }
    Ok(())
}

#[test]
fn unreadable_platform_policy_rechecks_identity_and_preserves_other_unknowns()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, CensusError, ProcessIdentity};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    let fact = |pid| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let observer = InjectedCensus {
        census: Census::from_observations(
            [fact(100), fact(200), fact(300)],
            [],
            [],
            true,
            false,
            32,
        )?,
        cwds: BTreeMap::from([
            (200, CwdObservation::PermissionDenied),
            (300, CwdObservation::Unsupported),
        ]),
        current: fact(100),
        platform_uids: BTreeMap::from([(
            200,
            test_uids(if provenant_os::fs::effective_uid() == 88 {
                89
            } else {
                88
            }),
        )]),
    };
    assert!(observer.observe_roots(std::slice::from_ref(&root))[&root].is_unknown());
    struct Racing<'a> {
        observer: &'a InjectedCensus,
        calls: AtomicUsize,
        race_at: usize,
    }
    impl ProcessObserver for Racing<'_> {
        fn boot_identity(&self) -> Result<String, CensusError> {
            self.observer.boot_identity()
        }
        fn census(&self, limit: usize) -> Result<Census, CensusError> {
            self.observer.census(limit)
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            let mut result = self.observer.observe(pid);
            if self.calls.fetch_add(1, Ordering::SeqCst) == self.race_at
                && let ProcessObservation::Present(fact) = &mut result
            {
                fact.identity.native_start += 1;
            }
            result
        }
    }
    for race_at in [0, 1] {
        let processes = Racing {
            observer: &observer,
            calls: AtomicUsize::new(0),
            race_at,
        };
        assert!(matches!(
            observe_same_process(&processes, &observer, &fact(200)),
            NativeCwd::Unknown(_)
        ));
    }
    let mut observer = observer;
    observer
        .cwds
        .insert(300, CwdObservation::Path(root.clone()));
    assert!(observer.observe_roots(std::slice::from_ref(&root))[&root].is_observed());
    // Identity lookup failure never licenses exclusion or placement.
    let unreadable = root.join("dangling-root");
    std::os::unix::fs::symlink(root.join("missing-target"), &unreadable)?;
    assert!(observer.observe_roots(std::slice::from_ref(&unreadable))[&unreadable].is_unknown());
    // A removed root is decided: the system process is excused against this
    // user's uid and the readable process lies outside it.
    let removed = root.join("removed-root");
    assert!(observer.observe_roots(std::slice::from_ref(&removed))[&removed].is_available());
    // A process left inside the removed tree still holds it.
    observer
        .cwds
        .insert(300, CwdObservation::Path(removed.join("src")));
    assert!(!observer.observe_roots(std::slice::from_ref(&removed))[&removed].is_available());
    Ok(())
}

#[test]
fn worker_census_audit_scopes_eperm_warning_and_retains_unsupported_cwd_unknown()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let fact = |pid: i64| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let observer = InjectedCensus {
        census: Census::from_observations(
            [fact(1), fact(100), fact(200)],
            [],
            [],
            true,
            false,
            32,
        )?,
        cwds: BTreeMap::from([
            (1, CwdObservation::PermissionDenied),
            (200, CwdObservation::Unsupported),
        ]),
        current: fact(100),
        platform_uids: BTreeMap::new(),
    };
    let mut audit = BTreeMap::new();
    // Spec: docs/specs/disk.md#scratch-and-cache-custody. Complete empty
    // occupancy scopes an EPERM warning to that process; unsupported cwd
    // evidence still protects the candidate.
    let (states, _, details) = observe_worker_holders_with_audit(
        std::slice::from_ref(&root),
        &observer,
        &observer,
        &observer.current,
        false,
        &mut |pid, condition| {
            audit.insert(pid, condition);
        },
    );
    assert!(states[&root].is_unknown());
    assert!(details[&root].contains("process 200"));
    assert!(!details[&root].contains("process 1"));
    assert_eq!(audit.len(), 2);
    let unreadable_without_candidate_path = audit.get(&Some(1)).copied().flatten();
    assert_eq!(
        unreadable_without_candidate_path,
        Some(UNREADABLE_NO_CANDIDATE_PATH)
    );
    let unsupported_cwd = audit.get(&Some(200)).copied().flatten();
    assert!(unsupported_cwd.is_some());
    assert_ne!(unsupported_cwd, Some(UNREADABLE_NO_CANDIDATE_PATH));
    assert!(builtin_audit_has_only_rejections(audit.values().copied()));
    // An unplaceable record with fully admissible built-in evidence must
    // fail acceptance, even when other records have real refusals.
    struct ListedOnlyFailure;
    impl CwdObserver for ListedOnlyFailure {
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(CwdObservation::PermissionDenied)
        }
        fn unreadable_policy_uids(
            &self,
            _: i64,
            _: &[CensusIgnoreEntry],
        ) -> Result<
            provenant_os::pid::UnreadablePolicyExcusal,
            provenant_os::pid::UnreadablePolicyFailure,
        > {
            Err(provenant_os::pid::UnreadablePolicyFailure {
                condition: "stable signing team required",
                builtin_condition: None,
                configurable: false,
            })
        }
    }
    audit.clear();
    let (states, _, _) = observe_worker_holders_with_audit(
        std::slice::from_ref(&root),
        &observer,
        &ListedOnlyFailure,
        &observer.current,
        false,
        &mut |pid, condition| {
            audit.insert(pid, condition);
        },
    );
    assert!(states[&root].is_unknown());
    assert_eq!(audit.len(), 2);
    assert!(!builtin_audit_has_only_rejections(audit.values().copied()));
    Ok(())
}

#[test]
fn worker_census_detail_reports_incomplete_listing() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    let current = ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid: 100,
            native_start: 100,
            group_id: 100,
            session_id: 100,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let observer = InjectedCensus {
        census: Census::from_observations([current.clone()], [], [], false, false, 32)?,
        cwds: BTreeMap::new(),
        current,
        platform_uids: BTreeMap::new(),
    };
    let mut audit = Vec::new();
    let (states, _, details) = observe_worker_holders_with_audit(
        std::slice::from_ref(&root),
        &observer,
        &observer,
        &observer.current,
        false,
        &mut |pid, condition| audit.push((pid, condition)),
    );
    assert!(states[&root].is_unknown());
    assert!(details[&root].contains("incomplete"));
    assert_eq!(audit.len(), 1);
    assert!(audit[0].1.is_some());
    Ok(())
}

#[test]
fn unplaceable_path_failures_retain_process_image_diagnostics() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    struct DetailedCwd(PathBuf);
    impl CwdObserver for DetailedCwd {
        fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
            provenant_os::pid::OccupancyObservation {
                paths: vec![],
                complete: true,
                gap: None,
            }
        }
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(CwdObservation::Path(self.0.clone()))
        }
        fn process_details(&self, _: i64) -> Option<String> {
            Some("path=/kernel/image, euid=501, cs_flags=0x20000001".into())
        }
    }
    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    let fact = |pid| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let observer = InjectedCensus {
        census: Census::from_observations([fact(100), fact(200)], [], [], true, false, 32)?,
        cwds: BTreeMap::new(),
        current: fact(100),
        platform_uids: BTreeMap::new(),
    };
    let loop_path = root.join("loop");
    std::os::unix::fs::symlink("loop", &loop_path)?;
    let file_path = root.join("file");
    fs::write(&file_path, "not a directory")?;
    for path in [loop_path, file_path] {
        let (states, details) = observe_worker_roots(
            std::slice::from_ref(&root),
            &observer,
            &DetailedCwd(path),
            &observer.current,
            false,
        );
        assert!(states[&root].is_unknown());
        let detail = &details[&root];
        for field in [
            "process 200:",
            "path=/kernel/image",
            "euid=501",
            "cs_flags=0x20000001",
        ] {
            assert!(detail.contains(field), "missing {field}: {detail}");
        }
    }
    Ok(())
}

#[test]
fn ancestor_exclusion_is_limited_to_run_admission() -> Result<(), Box<dyn Error>> {
    struct Probe;
    impl WorkerObserver for Probe {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Observed { process: None }))
                .collect()
        }
        fn observe_roots_with_policy(
            &self,
            roots: &[PathBuf],
            _: &[CensusIgnoreEntry],
            exclude: bool,
        ) -> WorkerObservations {
            (
                roots
                    .iter()
                    .cloned()
                    .map(|root| {
                        (
                            root,
                            if exclude {
                                Occupancy::Available
                            } else {
                                Occupancy::Observed { process: None }
                            },
                        )
                    })
                    .collect(),
                BTreeMap::new(),
            )
        }
        fn live_process_with_policy(
            &self,
            root: &Path,
            _: &[CensusIgnoreEntry],
            exclude: bool,
        ) -> Option<ObservedProcess> {
            (!exclude).then(|| ObservedProcess {
                pid: 90,
                command: Some("shell".into()),
                kind: ProcessEvidenceKind::WorkingDirectory,
                cwd: Some(root.to_path_buf()),
            })
        }
    }
    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    let inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let ordinary = CensusPolicyWorkers::from_roots(&Probe, &roots)?;
    let run = CensusPolicyWorkers::for_run(&Probe, &roots)?;
    assert!(ordinary.observe_roots(std::slice::from_ref(&root))[&root].is_observed());
    assert_eq!(ordinary.live_process(&root).unwrap().pid, 90);
    assert!(run.observe_roots(std::slice::from_ref(&root))[&root].is_available());
    assert!(run.live_process(&root).is_none());
    assert!(ordinary.observe_roots_for_land(std::slice::from_ref(&root))[&root].is_available());
    Ok(())
}

#[test]
fn host_policy_is_scoped_to_config_roots_and_preserves_injected_refusal()
-> Result<(), Box<dyn Error>> {
    struct Probe(std::sync::Mutex<Vec<Vec<CensusIgnoreEntry>>>);
    impl WorkerObserver for Probe {
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
        fn observe_roots_with_policy(
            &self,
            roots: &[PathBuf],
            entries: &[CensusIgnoreEntry],
            for_land: bool,
        ) -> WorkerObservations {
            assert!(for_land);
            self.0.lock().unwrap().push(entries.to_vec());
            (self.observe_roots(roots), BTreeMap::new())
        }
    }
    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    let state = fixture._temp.path().join("host-config");
    provenant_os::fixture::private_state_dir(&state)?;
    fs::write(
        state.join("config.toml"),
        "schema_version=1\n[knobs]\n'confinement.census_ignore_unreadable'=[{path='/Library/Daemon',team_id='TEAM123'}]\n",
    )?;
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(state);
    let configured_roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    inputs.state_dir = Some(fixture._temp.path().join("empty-config"));
    let empty_roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let probe = Probe(Default::default());
    let first = CensusPolicyWorkers::from_roots(&probe, &configured_roots)?;
    assert!(first.observe_roots_for_land(std::slice::from_ref(&root))[&root].is_unknown());
    let second = CensusPolicyWorkers::from_roots(&first, &empty_roots)?;
    assert!(second.observe_roots_for_land(std::slice::from_ref(&root))[&root].is_unknown());
    let seen = probe.0.lock().unwrap();
    assert_eq!(
        seen[0],
        vec![CensusIgnoreEntry {
            path: "/Library/Daemon".into(),
            team_id: "TEAM123".into(),
            started_before: None,
        }]
    );
    assert_eq!(
        seen[1],
        vec![CensusIgnoreEntry {
            path: "/usr/bin/login".into(),
            team_id: "platform".into(),
            started_before: Some(provenant_os::pid::StartedBefore::Root),
        }],
        "policy must not leak across roots or wrappers and unset config gets bundled login default"
    );
    Ok(())
}

/// The production configured-image matcher reads live macOS signing state and
/// cannot be driven positively in a unit test, so exercise the wiring instead:
/// `PolicyCwdObserver` must hand its non-empty configured entries to the matcher
/// it delegates to. A regression that drops those entries fails here.
#[test]
fn policy_cwd_observer_forwards_configured_entries_to_the_matcher() -> Result<(), Box<dyn Error>> {
    struct Recording(std::sync::Mutex<Vec<CensusIgnoreEntry>>);
    impl CwdObserver for Recording {
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(CwdObservation::PermissionDenied)
        }
        fn configured_unreadable_image(
            &self,
            _: i64,
            entries: &[CensusIgnoreEntry],
        ) -> Option<CensusIgnoreEntry> {
            *self.0.lock().unwrap() = entries.to_vec();
            entries.first().cloned()
        }
    }
    let entries = [CensusIgnoreEntry {
        path: PathBuf::from("/usr/libexec/UserEventAgent"),
        team_id: "TEAM123".into(),
        started_before: None,
    }];
    let recorder = Recording(std::sync::Mutex::new(Vec::new()));
    let policy = PolicyCwdObserver::with_inner(&entries, &recorder);
    // Call the real trait method with an empty caller argument: the policy must
    // still hand its own non-empty entries to the matcher.
    assert_eq!(
        CwdObserver::configured_unreadable_image(&policy, 200, &[]),
        Some(entries[0].clone())
    );
    assert_eq!(*recorder.0.lock().unwrap(), entries.to_vec());
    Ok(())
}

#[test]
fn listed_process_is_excluded_only_after_eperm_and_never_for_the_owner()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    struct Listed {
        uid: u32,
        denied: bool,
    }
    impl CwdObserver for Listed {
        fn process_uids(&self, _: i64) -> Option<ProcessUids> {
            Some(test_uids(self.uid))
        }

        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            if self.denied {
                Ok(CwdObservation::PermissionDenied)
            } else {
                Err(provenant_os::pid::PidError::Io {
                    pid: pid as i32,
                    source: std::io::Error::other("cwd failure"),
                })
            }
        }
        fn unreadable_policy_uids(
            &self,
            _: i64,
            _: &[CensusIgnoreEntry],
        ) -> Result<
            provenant_os::pid::UnreadablePolicyExcusal,
            provenant_os::pid::UnreadablePolicyFailure,
        > {
            assert!(self.denied, "non-EPERM must never consult listing policy");
            Ok(provenant_os::pid::UnreadablePolicyExcusal {
                uids: test_uids(self.uid),
            })
        }
    }
    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    let fact = |pid| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let observer = InjectedCensus {
        census: Census::from_observations([fact(100), fact(200)], [], [], true, false, 32)?,
        cwds: BTreeMap::new(),
        current: fact(100),
        platform_uids: BTreeMap::new(),
    };
    let owner = fs::metadata(&root)?.uid();
    for (name, uid, denied, expected_available) in [
        (
            "accepted foreign listing",
            if owner == 88 { 89 } else { 88 },
            true,
            true,
        ),
        ("owner uid refused", owner, true, false),
        ("human credentials refused", 500, true, false),
        ("non-EPERM refused", owner.wrapping_add(1), false, false),
    ] {
        assert_eq!(
            observe_worker_roots(
                std::slice::from_ref(&root),
                &observer,
                &Listed { uid, denied },
                &observer.current,
                false
            )
            .0[&root]
                .is_available(),
            expected_available,
            "{name}"
        );
    }
    Ok(())
}

// Spec: docs/specs/disk.md#scratch-and-cache-custody
#[test]
fn unreadable_process_without_candidate_paths_is_scoped_independently_of_uid()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    let fact = |pid: i64| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let owner = fs::metadata(&root)?.uid();
    let mut observer = InjectedCensus {
        census: Census::from_observations([fact(100), fact(200)], [], [], true, false, 32)?,
        cwds: BTreeMap::from([(200, CwdObservation::PermissionDenied)]),
        current: fact(100),
        platform_uids: BTreeMap::new(),
    };
    for (name, evidence) in [
        ("root daemon", Some(0)),
        (
            "foreign service daemon",
            Some(if owner == 88 { 89 } else { 88 }),
        ),
        ("last system account", Some(499)),
        ("first human account", Some(500)),
        ("worktree owner", Some(owner)),
        ("missing image/signing/uid evidence", None),
    ] {
        observer.platform_uids = evidence
            .map(|uid| BTreeMap::from([(200, test_uids(uid))]))
            .unwrap_or_default();
        assert!(
            observer.observe_roots(std::slice::from_ref(&root))[&root].is_available(),
            "{name}"
        );
    }
    for (name, uids) in [
        (
            "owner real uid",
            ProcessUids {
                real: owner,
                effective: 0,
                saved: 0,
            },
        ),
        (
            "owner saved uid",
            ProcessUids {
                real: 0,
                effective: 0,
                saved: owner,
            },
        ),
        (
            "human real uid",
            ProcessUids {
                real: 500,
                effective: 0,
                saved: 0,
            },
        ),
        (
            "human saved uid",
            ProcessUids {
                real: 0,
                effective: 0,
                saved: 500,
            },
        ),
    ] {
        observer.platform_uids = BTreeMap::from([(200, uids)]);
        assert!(
            observer.observe_roots(std::slice::from_ref(&root))[&root].is_available(),
            "{name}"
        );
    }
    observer.platform_uids = BTreeMap::from([(
        200,
        ProcessUids {
            real: 0,
            effective: 88,
            saved: 0,
        },
    )]);
    assert!(observer.observe_roots(std::slice::from_ref(&root))[&root].is_available());
    let roots = [root.clone(), PathBuf::from("/")];
    let states = observer.observe_roots(&roots);
    assert!(states.values().all(Occupancy::is_available));
    // A denied fact cannot borrow the cwd exception without a pinned identity.
    observer.census = Census::from_observations(
        [fact(100)],
        [(200, ObservationIssue::PermissionDenied)],
        [],
        true,
        false,
        32,
    )?;
    observer.platform_uids = BTreeMap::from([(200, test_uids(88))]);
    assert!(
        observer.observe_roots(std::slice::from_ref(&root))[&root].is_available(),
        "undescribed process"
    );
    Ok(())
}

#[test]
fn cwd_query_errors_never_consult_the_daemon_policy() -> Result<(), Box<dyn Error>> {
    struct FailedCwd;
    impl CwdObserver for FailedCwd {
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Err(provenant_os::pid::PidError::Io {
                pid: pid as i32,
                source: std::io::Error::from_raw_os_error(5),
            })
        }
        fn unreadable_policy_uids(
            &self,
            _: i64,
            _: &[CensusIgnoreEntry],
        ) -> Result<
            provenant_os::pid::UnreadablePolicyExcusal,
            provenant_os::pid::UnreadablePolicyFailure,
        > {
            panic!("non-EPERM cwd failure reached daemon policy")
        }
    }
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fact = ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid: 200,
            native_start: 200,
            group_id: 200,
            session_id: 200,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let observer = InjectedCensus {
        census: Census::from_observations([fact.clone()], [], [], true, false, 32)?,
        cwds: BTreeMap::new(),
        current: fact.clone(),
        platform_uids: BTreeMap::new(),
    };
    assert!(matches!(
        observe_same_process(&observer, &FailedCwd, &fact),
        NativeCwd::Unknown(_)
    ));
    assert!(matches!(
        observe_same_process(&observer, &observer, &fact),
        NativeCwd::Unknown(_)
    ));
    Ok(())
}

// Spec: docs/specs/disk.md#scratch-and-cache-custody
#[test]
fn unreadable_cwd_without_candidate_paths_does_not_block_removal() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let cancellation = cancel();
    let fact = |pid: i64| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let current = fact(100);
    let occupant = fact(200);
    let observer = InjectedCensus {
        census: Census::from_observations([current.clone(), occupant], [], [], true, false, 32)?,
        cwds: BTreeMap::from([(200, CwdObservation::PermissionDenied)]),
        current,
        platform_uids: BTreeMap::new(),
    };
    struct DeniedDescriptors<'a>(&'a InjectedCensus);
    impl ProcessObserver for DeniedDescriptors<'_> {
        fn boot_identity(&self) -> Result<String, provenant_supervisor::census::CensusError> {
            self.0.boot_identity()
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            self.0.observe(pid)
        }
        fn census(
            &self,
            limit: usize,
        ) -> Result<provenant_supervisor::census::Census, provenant_supervisor::census::CensusError>
        {
            self.0.census(limit)
        }
    }
    impl CwdObserver for DeniedDescriptors<'_> {
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            self.0.cwd(pid)
        }
        fn process_uids(&self, pid: i64) -> Option<ProcessUids> {
            self.0.process_uids(pid)
        }
        fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
            provenant_os::pid::OccupancyObservation {
                paths: vec![],
                complete: false,
                gap: Some(provenant_os::pid::OccupancyGap::DescriptorsDenied),
            }
        }
        fn unreadable_policy_uids(
            &self,
            pid: i64,
            entries: &[CensusIgnoreEntry],
        ) -> Result<
            provenant_os::pid::UnreadablePolicyExcusal,
            provenant_os::pid::UnreadablePolicyFailure,
        > {
            self.0.unreadable_policy_uids(pid, entries)
        }
    }
    impl WorkerObserver for DeniedDescriptors<'_> {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            observe_worker_roots(roots, self, self, &self.0.current, true).0
        }
        fn requires_complete_census(&self) -> bool {
            true
        }
        fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
            observe_worker_roots(roots, self, self, &self.0.current, true)
        }
    }
    let created = create(
        &test_context.context(),
        WorktreeCreate {
            branch: "landed".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    // Complete occupancy finds no path under the worktree, so the EPERM cwd
    // does not retain this candidate.
    fs::set_permissions(&created.path, fs::Permissions::from_mode(0o755))?;
    let denied = DeniedDescriptors(&observer);
    assert!(denied.observe_roots(std::slice::from_ref(&created.path))[&created.path].is_unknown());
    assert!(matches!(
        remove_with_port(
            &test_context,
            &WorktreeContext {
                workers: &denied,
                ..test_context.context()
            },
            WorktreeRemove::Name {
                name: "landed".to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation,
        ),
        Err(WorktreeError::IncompleteCensus { .. })
    ));
    assert!(created.path.exists());
    remove_with_port(
        &test_context,
        &WorktreeContext {
            workers: &observer,
            ..test_context.context()
        },
        WorktreeRemove::Name {
            name: "landed".to_owned(),
            discard: false,
            integration: None,
            expected_head: None,
        },
        &cancellation,
    )?;
    assert!(!created.path.exists());
    Ok(())
}

// Spec: docs/specs/disk.md#scratch-and-cache-custody
#[test]
fn only_a_placed_foreign_process_allows_pruning_a_merged_clean_tree() -> Result<(), Box<dyn Error>>
{
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "-M", "main"])?;
    let test_context = TestContext::new(&fixture)?;
    let cancellation = cancel();
    let fact = |pid: i64| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let outside = canonicalize(fixture._temp.path())?;
    // The caller, a readable bystander, a process whose directory the
    // kernel will not reveal, and a listed PID it will not describe.
    let census = |unknown: ObservationIssue| {
        Census::from_observations(
            [fact(100), fact(200), fact(300)],
            [(400, unknown)],
            [],
            true,
            false,
            32,
        )
    };
    let observer = |unknown, inside: Option<&Path>| -> Result<_, Box<dyn Error>> {
        let mut cwds = BTreeMap::from([
            (200, CwdObservation::Path(outside.clone())),
            (300, CwdObservation::PermissionDenied),
        ]);
        if let Some(path) = inside {
            cwds.insert(200, CwdObservation::Path(path.to_path_buf()));
        }
        Ok(InjectedCensus {
            census: census(unknown)?,
            cwds,
            current: fact(100),
            platform_uids: BTreeMap::new(),
        })
    };
    let remove_named = |workers: &InjectedCensus, name: &str| {
        remove_with_port(
            &test_context,
            &WorktreeContext {
                workers,
                ..test_context.context()
            },
            WorktreeRemove::Name {
                name: name.to_owned(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancellation,
        )
    };
    let created = create(
        &test_context.context(),
        WorktreeCreate {
            branch: "landed".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    // Make the permission evidence independent of the test runner's umask.
    fs::set_permissions(&created.path, fs::Permissions::from_mode(0o755))?;

    // Uncertainty that could place a process anywhere keeps the tree.
    let unplaced = observer(ObservationIssue::Io, None)?;
    assert!(
        unplaced.observe_roots(std::slice::from_ref(&created.path))[&created.path].is_unknown()
    );
    assert!(matches!(
        remove_named(&unplaced, "landed"),
        Err(WorktreeError::IncompleteCensus { .. })
    ));
    // A readable process inside the tree still keeps it.
    let live = observer(ObservationIssue::PermissionDenied, Some(&created.path))?;
    assert!(matches!(
        remove_named(&live, "landed"),
        Err(WorktreeError::LiveWriter)
    ));
    assert!(created.path.exists());

    // An unreadable process with no candidate cwd or descriptor does not taint
    // this root, even when its identity cannot be read from the census.
    let foreign = observer(ObservationIssue::PermissionDenied, None)?;
    assert!(
        foreign.observe_roots(std::slice::from_ref(&created.path))[&created.path].is_available()
    );
    assert!(created.path.exists());

    // A complete cwd observation outside the tree also leaves the candidate free.
    let placed = InjectedCensus {
        census: Census::from_observations(
            [fact(100), fact(200), fact(300), fact(400)],
            [],
            [],
            true,
            false,
            32,
        )?,
        cwds: [200, 300, 400]
            .into_iter()
            .map(|pid| (pid, CwdObservation::Path(outside.clone())))
            .collect(),
        ..foreign
    };
    let listed = list(
        &WorktreeContext {
            workers: &placed,
            ..test_context.context()
        },
        &cancellation,
    )?;
    assert!(
        listed
            .iter()
            .any(|entry| entry.path == created.path && entry.occupancy.is_available())
    );
    remove_named(&placed, "landed")?;
    assert!(!created.path.exists());
    Ok(())
}

#[test]
fn nested_worker_protects_all_containing_worktrees() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let outer = fixture.root.join("outer");
    let nested = outer.join("nested");
    let sibling = fixture.root.join("sibling");
    fs::create_dir_all(nested.join("src"))?;
    fs::create_dir(&sibling)?;
    let mut states = BTreeMap::from([
        (outer.clone(), Occupancy::Available),
        (nested.clone(), Occupancy::Available),
        (sibling.clone(), Occupancy::Available),
    ]);
    record_live_path(&mut states, &nested.join("src"));
    assert!(states[&outer].is_observed());
    assert!(states[&nested].is_observed());
    assert!(states[&sibling].is_available());
    Ok(())
}

mod writer_cohort {
    use super::*;
    use provenant_os::pid::{OccupancyGap, OccupancyObservation};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Observations {
        cwd: CwdObservation,
        uids: Vec<Option<ProcessUids>>,
        credential_reads: AtomicUsize,
        occupancy: Vec<OccupancyObservation>,
        attempts: AtomicUsize,
    }
    impl CwdObserver for Observations {
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(self.cwd.clone())
        }
        fn process_uids(&self, _: i64) -> Option<ProcessUids> {
            let index = self.credential_reads.fetch_add(1, Ordering::SeqCst);
            self.uids[index.min(self.uids.len() - 1)]
        }
        fn occupancy(&self, _: i64) -> OccupancyObservation {
            let index = self.attempts.fetch_add(1, Ordering::SeqCst);
            self.occupancy[index.min(self.occupancy.len() - 1)].clone()
        }
        // No image-policy implementation: a system updater need not satisfy
        // the old platform-signature or sealed-volume exception.
    }

    fn incomplete(gap: OccupancyGap) -> OccupancyObservation {
        OccupancyObservation {
            paths: vec![],
            complete: false,
            gap: Some(gap),
        }
    }
    fn complete() -> OccupancyObservation {
        OccupancyObservation {
            paths: vec![],
            complete: true,
            gap: None,
        }
    }
    fn observations(cwd: CwdObservation, uids: Option<ProcessUids>) -> Observations {
        Observations {
            cwd,
            uids: vec![uids],
            credential_reads: AtomicUsize::new(0),
            occupancy: vec![complete()],
            attempts: AtomicUsize::new(0),
        }
    }
    fn processes() -> Result<InjectedCensus, Box<dyn Error>> {
        Ok(InjectedCensus {
            census: provenant_supervisor::census::Census::from_observations(
                [refused_fact(100), refused_fact(200)],
                [],
                [],
                true,
                false,
                32,
            )?,
            cwds: BTreeMap::new(),
            current: refused_fact(100),
            platform_uids: BTreeMap::new(),
        })
    }
    struct Workers<'a>(&'a InjectedCensus, &'a Observations);
    impl WorkerObserver for Workers<'_> {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            observe_worker_roots(roots, self.0, self.1, &self.0.current, false).0
        }
        fn requires_complete_census(&self) -> bool {
            true
        }
    }

    #[test]
    fn unreadable_foreign_system_process_allows_merged_worktree_and_branch_cleanup()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-M", "main"])?;
        let context = TestContext::new(&fixture)?;
        let created = create(
            &context.context(),
            WorktreeCreate {
                branch: "landed".into(),
                base: None,
            },
            &cancel(),
        )?;
        let owner = fs::metadata(&created.path)?.uid();
        // Root is foreign on a user-owned checkout. A root-run test must not
        // excuse a process with the checkout owner's uid, so use a service uid.
        let system_uid = if owner == 0 { 88 } else { 0 };
        let mut observation = observations(
            CwdObservation::PermissionDenied,
            Some(test_uids(system_uid)),
        );
        observation.occupancy = vec![incomplete(OccupancyGap::DescriptorsDenied)];
        let processes = processes()?;
        let workers = Workers(&processes, &observation);
        assert!(
            workers.observe_roots(std::slice::from_ref(&created.path))[&created.path]
                .is_available()
        );
        remove_with_port(
            &context,
            &WorktreeContext {
                workers: &workers,
                ..context.context()
            },
            WorktreeRemove::Name {
                name: "landed".into(),
                discard: false,
                integration: None,
                expected_head: None,
            },
            &cancel(),
        )?;
        assert!(!created.path.exists());
        assert!(!branch_exists(&fixture, "landed"));
        Ok(())
    }

    #[test]
    fn transient_user_occupancy_gaps_converge_and_keep_earlier_positive_paths()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let root = canonicalize(&fixture.root)?;
        let outside = canonicalize(fixture._temp.path())?;
        let owner = fs::metadata(&root)?.uid();
        let processes = processes()?;
        for gap in [OccupancyGap::Churn, OccupancyGap::DescriptorsUnavailable] {
            for held in [false, true] {
                let mut observation = observations(
                    CwdObservation::Path(outside.clone()),
                    Some(test_uids(owner)),
                );
                let mut first = incomplete(gap);
                if held {
                    first.paths.push(root.join("held-file"));
                }
                observation.occupancy = vec![first, complete()];
                // Exercise the same budget-bound path used by landing retries.
                let mut held_processes = BTreeMap::new();
                let (states, holders, _, _) = observe_worker_holders_while(
                    std::slice::from_ref(&root),
                    &processes,
                    &observation,
                    &processes.current,
                    false,
                    &mut |_, _| {},
                    &mut held_processes,
                    Some(&|| true),
                    &[],
                );
                assert_eq!(states[&root].is_observed(), held, "{gap:?}");
                assert_eq!(observation.attempts.load(Ordering::SeqCst), 2);
                assert_eq!(holders.contains_key(&root), held);
            }
        }
        Ok(())
    }

    // Spec: docs/specs/disk.md#scratch-and-cache-custody
    #[test]
    fn denied_descriptor_observation_remains_unknown_until_occupancy_is_complete()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let root = canonicalize(&fixture.root)?;
        let outside = canonicalize(fixture._temp.path())?;
        let owner = fs::metadata(&root)?.uid();
        let system_uid = if owner == 0 { 88 } else { 0 };
        let system = test_uids(system_uid);
        let processes = processes()?;
        for uids in [
            Some(test_uids(owner)),
            None,
            Some(test_uids(500)),
            Some(ProcessUids {
                real: owner,
                ..system
            }),
            Some(ProcessUids {
                effective: owner,
                ..system
            }),
            Some(ProcessUids {
                saved: owner,
                ..system
            }),
        ] {
            let mut observation = observations(CwdObservation::PermissionDenied, uids);
            observation.occupancy = vec![incomplete(OccupancyGap::DescriptorsDenied)];
            let states =
                Workers(&processes, &observation).observe_roots(std::slice::from_ref(&root));
            assert!(states[&root].is_unknown(), "{uids:?}");
        }
        // Even a readable outside cwd cannot excuse an unreadable open file.
        let mut observation = observations(CwdObservation::Path(outside), Some(test_uids(owner)));
        observation.occupancy = vec![incomplete(OccupancyGap::DescriptorsUnavailable)];
        assert!(
            Workers(&processes, &observation).observe_roots(std::slice::from_ref(&root))[&root]
                .is_unknown()
        );
        assert_eq!(observation.attempts.load(Ordering::SeqCst), 4);

        for cwd_inside in [false, true] {
            let mut observation = observations(
                if cwd_inside {
                    CwdObservation::Path(root.clone())
                } else {
                    CwdObservation::PermissionDenied
                },
                Some(system),
            );
            observation.occupancy = vec![OccupancyObservation {
                paths: if cwd_inside {
                    vec![]
                } else {
                    vec![root.join("held-file")]
                },
                ..incomplete(OccupancyGap::DescriptorsDenied)
            }];
            assert!(
                Workers(&processes, &observation).observe_roots(std::slice::from_ref(&root))[&root]
                    .is_observed()
            );
        }
        let mut changing = observations(CwdObservation::PermissionDenied, Some(system));
        changing.uids.push(Some(test_uids(owner)));
        assert!(
            Workers(&processes, &changing).observe_roots(std::slice::from_ref(&root))[&root]
                .is_unknown()
        );
        Ok(())
    }
}

/// What the census concludes for one process beside a candidate root.
mod narrowing {
    use super::*;
    use provenant_os::pid::{OccupancyGap, OccupancyObservation, SealedSystemImage};
    use provenant_supervisor::census::{Census, ProcessIdentity};

    struct Scripted {
        cwd: CwdObservation,
        occupancy: OccupancyObservation,
        sealed: Option<SealedSystemImage>,
        kernel_thread: bool,
    }
    impl CwdObserver for Scripted {
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(self.cwd.clone())
        }
        fn occupancy(&self, _: i64) -> OccupancyObservation {
            self.occupancy.clone()
        }
        fn sealed_system_image(&self, _: i64) -> Option<SealedSystemImage> {
            self.sealed
        }
        fn kernel_thread(&self, _: i64) -> bool {
            self.kernel_thread
        }
    }

    fn gap(gap: OccupancyGap) -> OccupancyObservation {
        OccupancyObservation {
            paths: vec![],
            complete: false,
            gap: Some(gap),
        }
    }

    const SYSTEM: Option<SealedSystemImage> = Some(SealedSystemImage {
        setuid_transition: false,
    });
    const SETUID: Option<SealedSystemImage> = Some(SealedSystemImage {
        setuid_transition: true,
    });

    type Observed = (Occupancy, String, Vec<(Option<i64>, Option<&'static str>)>);

    /// Observe one process (pid 200) beside the root; returns state, detail
    /// and the audited (pid, condition) pairs.
    fn observe(scripted: Scripted, zombie: bool) -> Result<Observed, Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let base = canonicalize(fixture._temp.path())?;
        let root = base.join("root");
        fs::create_dir(&root)?;
        std::os::unix::fs::symlink(LOOP, base.join(LOOP))?;
        let mut scripted = scripted;
        for path in &mut scripted.occupancy.paths {
            if path.as_os_str() == LOOP {
                *path = base.join(LOOP);
            }
        }
        let scripted = Scripted {
            cwd: match scripted.cwd {
                CwdObservation::Path(path) if path.as_path() == std::path::Path::new("inside") => {
                    CwdObservation::Path(root.clone())
                }
                CwdObservation::Path(_) => CwdObservation::Path(base.clone()),
                other => other,
            },
            ..scripted
        };
        let fact = |pid: i64, zombie| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: pid,
                session_id: pid,
            },
            parent_pid: 1,
            zombie,
            cpu_micros: None,
            command: None,
        };
        let current = fact(100, false);
        let processes = InjectedCensus {
            census: Census::from_observations(
                [current.clone(), fact(200, zombie)],
                [],
                [],
                true,
                false,
                32,
            )?,
            cwds: BTreeMap::new(),
            current: current.clone(),
            platform_uids: BTreeMap::new(),
        };
        let mut audit = Vec::new();
        let mut held_processes = BTreeMap::new();
        let (states, _, details, _) = observe_worker_holders_while(
            std::slice::from_ref(&root),
            &processes,
            &scripted,
            &current,
            false,
            &mut |pid, condition| audit.push((pid, condition)),
            &mut held_processes,
            None,
            &[],
        );
        Ok((
            states[&root].clone(),
            details.get(&root).cloned().unwrap_or_default(),
            audit,
        ))
    }

    /// Stands for a path that cannot be canonicalised; `observe` makes it a
    /// symlink to itself.
    const LOOP: &str = "occupancy-loop";

    fn path_scripted(
        occupancy: OccupancyObservation,
        sealed: Option<SealedSystemImage>,
    ) -> Scripted {
        Scripted {
            cwd: CwdObservation::Path(PathBuf::new()),
            occupancy,
            sealed,
            kernel_thread: false,
        }
    }

    /// Wraps a scripted observer; the process exits (as `exit`) once its
    /// occupancy has been queried, however often it is observed.
    struct ExitsOnOccupancy<'a> {
        processes: &'a InjectedCensus,
        scripted: &'a Scripted,
        exit: ProcessObservation,
        exited: std::sync::atomic::AtomicBool,
    }
    impl ProcessObserver for ExitsOnOccupancy<'_> {
        fn boot_identity(&self) -> Result<String, provenant_supervisor::census::CensusError> {
            self.processes.boot_identity()
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            if pid == 200 && self.exited.load(std::sync::atomic::Ordering::SeqCst) {
                return self.exit.clone();
            }
            self.processes.observe(pid)
        }
        fn census(
            &self,
            max: usize,
        ) -> Result<provenant_supervisor::census::Census, provenant_supervisor::census::CensusError>
        {
            self.processes.census(max)
        }
    }
    impl CwdObserver for ExitsOnOccupancy<'_> {
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            self.scripted.cwd(pid)
        }
        fn occupancy(&self, pid: i64) -> OccupancyObservation {
            self.exited.store(true, std::sync::atomic::Ordering::SeqCst);
            self.scripted.occupancy(pid)
        }
        fn sealed_system_image(&self, pid: i64) -> Option<SealedSystemImage> {
            self.scripted.sealed_system_image(pid)
        }
        fn kernel_thread(&self, pid: i64) -> bool {
            self.scripted.kernel_thread(pid)
        }
    }

    /// Root state when pid 200, with its cwd and an open file inside the root,
    /// becomes `exit` during its occupancy query.
    fn exits_during_occupancy(
        exit: impl Fn(ProcessFact) -> ProcessObservation,
    ) -> Result<Occupancy, Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let root = canonicalize(fixture._temp.path())?.join("root");
        fs::create_dir(&root)?;
        let fact = |pid: i64| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: pid,
                session_id: pid,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let current = fact(100);
        let processes = InjectedCensus {
            census: Census::from_observations(
                [current.clone(), fact(200)],
                [],
                [],
                true,
                false,
                32,
            )?,
            cwds: BTreeMap::new(),
            current: current.clone(),
            platform_uids: BTreeMap::new(),
        };
        let scripted = Scripted {
            cwd: CwdObservation::Path(root.clone()),
            occupancy: OccupancyObservation {
                paths: vec![root.join("open-file")],
                complete: false,
                gap: Some(OccupancyGap::DescriptorsUnavailable),
            },
            sealed: None,
            kernel_thread: false,
        };
        let observer = ExitsOnOccupancy {
            processes: &processes,
            scripted: &scripted,
            exit: exit(fact(200)),
            exited: std::sync::atomic::AtomicBool::new(false),
        };
        let (states, _, _) = observe_worker_holders_with_audit(
            std::slice::from_ref(&root),
            &observer,
            &observer,
            &current,
            false,
            &mut |_, _| {},
        );
        Ok(states[&root].clone())
    }

    #[test]
    fn a_process_that_exits_during_occupancy_inspection_frees_its_root()
    -> Result<(), Box<dyn Error>> {
        assert!(exits_during_occupancy(|_| ProcessObservation::Absent)?.is_available());
        assert!(
            exits_during_occupancy(|fact| ProcessObservation::Present(ProcessFact {
                zombie: true,
                ..fact
            }))?
            .is_available()
        );
        // A different process that reused the PID proves nothing.
        assert_ne!(
            exits_during_occupancy(|fact| ProcessObservation::Present(ProcessFact {
                identity: ProcessIdentity {
                    native_start: 999,
                    ..fact.identity
                },
                ..fact
            }))?,
            Occupancy::Available
        );
        Ok(())
    }

    #[test]
    fn occupancy_gap_exceptions_depend_on_positive_placement_and_image_policy()
    -> Result<(), Box<dyn Error>> {
        let descriptor_gaps = [
            OccupancyGap::DescriptorsDenied,
            OccupancyGap::DescriptorsUnavailable,
            OccupancyGap::Churn,
            OccupancyGap::LimitExceeded,
        ];
        let mut cases = Vec::new();
        for gap_kind in descriptor_gaps {
            for sealed in [None, SYSTEM] {
                cases.push((
                    gap_kind,
                    sealed,
                    CwdObservation::Path(PathBuf::from("outside")),
                    false,
                ));
            }
            cases.push((
                gap_kind,
                None,
                CwdObservation::Path(PathBuf::from("inside")),
                true,
            ));
        }
        for gap_kind in [
            OccupancyGap::ExecutableUnavailable,
            OccupancyGap::Unobserved,
        ] {
            cases.push((
                gap_kind,
                SYSTEM,
                CwdObservation::Path(PathBuf::from("outside")),
                false,
            ));
        }
        for gap_kind in [
            OccupancyGap::DescriptorsDenied,
            OccupancyGap::DescriptorsUnavailable,
        ] {
            cases.push((gap_kind, SETUID, CwdObservation::PermissionDenied, false));
        }
        for sealed in [None, SYSTEM] {
            cases.push((
                OccupancyGap::Unobserved,
                sealed,
                CwdObservation::PermissionDenied,
                false,
            ));
        }

        for (gap_kind, sealed, cwd, expected_observed) in cases {
            let mut scripted = path_scripted(gap(gap_kind), sealed);
            scripted.cwd = cwd;
            let (state, _, _) = observe(scripted, false)?;
            assert_eq!(state.is_observed(), expected_observed, "{gap_kind:?}");
        }
        Ok(())
    }

    #[test]
    fn zombies_and_kernel_threads_are_skipped() -> Result<(), Box<dyn Error>> {
        let (state, _, _) = observe(
            Scripted {
                cwd: CwdObservation::Gone,
                occupancy: gap(OccupancyGap::ExecutableUnavailable),
                sealed: None,
                kernel_thread: false,
            },
            true,
        )?;
        assert!(state.is_available());
        let (state, _, _) = observe(
            Scripted {
                cwd: CwdObservation::Gone,
                occupancy: gap(OccupancyGap::ExecutableUnavailable),
                sealed: None,
                kernel_thread: true,
            },
            false,
        )?;
        assert!(state.is_available());
        // The same process that is neither stays unknown.
        let (state, _, _) = observe(
            Scripted {
                cwd: CwdObservation::Gone,
                occupancy: gap(OccupancyGap::ExecutableUnavailable),
                sealed: None,
                kernel_thread: false,
            },
            false,
        )?;
        assert!(state.is_unknown());
        Ok(())
    }

    #[test]
    fn occupancy_path_placement_failures_are_audited_as_unplaceable() -> Result<(), Box<dyn Error>>
    {
        let (state, detail, audit) = observe(
            path_scripted(
                OccupancyObservation {
                    paths: vec![PathBuf::new()],
                    complete: true,
                    gap: None,
                },
                None,
            ),
            false,
        )?;
        assert!(state.is_unknown());
        assert!(
            detail.starts_with("process 200: occupancy path unavailable"),
            "{detail}"
        );
        assert_eq!(
            audit,
            vec![(Some(200), Some("occupancy path placement required"))]
        );
        Ok(())
    }

    #[test]
    fn policy_workers_control_ancestor_exclusion() -> Result<(), Box<dyn Error>> {
        use provenant_supervisor::census::{Census, CensusError, ProcessIdentity};
        struct Processes(Census);
        impl ProcessObserver for Processes {
            fn boot_identity(&self) -> Result<String, CensusError> {
                Ok("boot".into())
            }
            fn observe(&self, pid: i64) -> ProcessObservation {
                match self.0.process(pid) {
                    ProcessLookup::Present(fact) => ProcessObservation::Present(fact.clone()),
                    _ => ProcessObservation::Absent,
                }
            }
            fn census(&self, _: usize) -> Result<Census, CensusError> {
                Ok(self.0.clone())
            }
        }
        struct Cwds(BTreeMap<i64, PathBuf>);
        impl CwdObserver for Cwds {
            fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
                provenant_os::pid::OccupancyObservation {
                    paths: vec![],
                    complete: true,
                    gap: None,
                }
            }
            fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
                Ok(self
                    .0
                    .get(&pid)
                    .cloned()
                    .map(CwdObservation::Path)
                    .unwrap_or(CwdObservation::Gone))
            }
        }
        let fixture = Fixture::new()?;
        let root = fixture.root.clone();
        let fact = |pid, parent_pid, session_id| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: session_id,
                session_id,
            },
            parent_pid,
            zombie: false,
            cpu_micros: None,
            command: Some("shell".into()),
        };
        let current = fact(100, 90, 10);
        let parent = fact(90, 10, 10);
        let leader = fact(10, 1, 10);
        let facts = vec![leader, parent, current.clone()];
        let processes = Processes(Census::from_observations(facts, [], [], true, false, 32)?);
        let cwds = Cwds(BTreeMap::from([(90, root.clone()), (100, root.clone())]));

        // With ancestor exclusion (default policy), parent shell inside root is excluded: Free.
        let (states_with_ancestors, _, _) = observe_worker_holders(
            std::slice::from_ref(&root),
            &processes,
            &cwds,
            &current,
            true,
        );
        assert!(states_with_ancestors[&root].is_available());

        // Without ancestor exclusion, parent shell inside root is detected: Live.
        let (states_no_ancestors, holders_no_ancestors, _) = observe_worker_holders(
            std::slice::from_ref(&root),
            &processes,
            &cwds,
            &current,
            false,
        );
        assert!(states_no_ancestors[&root].is_observed());
        let holder = holders_no_ancestors.get(&root).unwrap();
        assert_eq!(holder.pid, 90);
        assert_eq!(holder.command.as_deref(), Some("shell"));
        assert_eq!(holder.cwd.as_ref(), Some(&root));

        // Foreign process inside root is still detected even with ancestor exclusion: Live.
        let foreign = fact(200, 1, 200);
        let facts_with_foreign = vec![fact(10, 1, 10), fact(90, 10, 10), current.clone(), foreign];
        let processes_foreign = Processes(Census::from_observations(
            facts_with_foreign,
            [],
            [],
            true,
            false,
            32,
        )?);
        let cwds_foreign = Cwds(BTreeMap::from([
            (90, root.clone()),
            (100, root.clone()),
            (200, root.clone()),
        ]));
        let (states_foreign, holders_foreign, _) = observe_worker_holders(
            std::slice::from_ref(&root),
            &processes_foreign,
            &cwds_foreign,
            &current,
            true,
        );
        assert!(states_foreign[&root].is_observed());
        let foreign_holder = holders_foreign.get(&root).unwrap();
        assert_eq!(foreign_holder.pid, 200);
        assert_eq!(foreign_holder.cwd.as_ref(), Some(&root));
        Ok(())
    }
}

#[test]
fn unplaceable_aliased_cwd_does_not_taint_unrelated_worktrees() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let affected = temp.path().join("affected");
    let unrelated = temp.path().join("unrelated");
    fs::create_dir(&affected)?;
    fs::create_dir(&unrelated)?;
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&affected, &alias)?;
    let non_directory = alias.join("file");
    fs::write(&non_directory, b"")?;
    let mut states = BTreeMap::from([
        (affected.clone(), Occupancy::Available),
        (unrelated.clone(), Occupancy::Available),
    ]);
    let mut reported = false;
    record_live_path_with_audit(&mut states, &non_directory.join("cwd"), &mut || {
        reported = true;
    });
    assert!(reported);
    assert!(states[&affected].is_unknown());
    assert!(states[&unrelated].is_available());
    Ok(())
}

#[test]
fn affirmative_evidence_of_different_root_does_not_taint_this_one() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let this_root = temp.path().join("this-root");
    let other_root = temp.path().join("other-root");
    fs::create_dir(&this_root)?;
    fs::create_dir(&other_root)?;
    let inside_other = other_root.join("sub-dir");
    fs::create_dir(&inside_other)?;
    let mut states = BTreeMap::from([
        (this_root.clone(), Occupancy::Available),
        (other_root.clone(), Occupancy::Available),
    ]);
    let mut unplaceable_reported = false;
    let placed = record_live_path_with_audit(&mut states, &inside_other, &mut || {
        unplaceable_reported = true;
    });
    assert!(!unplaceable_reported);
    assert_eq!(placed, vec![other_root.clone()]);
    assert!(states[&this_root].is_available());
    assert!(states[&other_root].is_observed());
    Ok(())
}

#[test]
fn wholly_unplaceable_process_taints_every_root_fail_closed() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let root_a = temp.path().join("root-a");
    let root_b = temp.path().join("root-b");
    fs::create_dir(&root_a)?;
    fs::create_dir(&root_b)?;

    // Relative path cannot safely rule out any root.
    let mut states = BTreeMap::from([
        (root_a.clone(), Occupancy::Available),
        (root_b.clone(), Occupancy::Available),
    ]);
    let mut reported = false;
    let placed = record_live_path_with_audit(&mut states, Path::new("relative/path"), &mut || {
        reported = true;
    });
    assert!(reported);
    assert!(placed.is_empty());
    assert!(states[&root_a].is_unknown());
    assert!(states[&root_b].is_unknown());

    // Empty path is similarly wholly unplaceable and fails closed.
    let mut states_empty = BTreeMap::from([
        (root_a.clone(), Occupancy::Available),
        (root_b.clone(), Occupancy::Available),
    ]);
    let mut reported_empty = false;
    let placed_empty = record_live_path_with_audit(&mut states_empty, Path::new(""), &mut || {
        reported_empty = true;
    });
    assert!(reported_empty);
    assert!(placed_empty.is_empty());
    assert!(states_empty[&root_a].is_unknown());
    assert!(states_empty[&root_b].is_unknown());
    Ok(())
}

#[test]
fn live_observer_without_process_details_preserves_unattributed_writer() {
    struct LiveObserver;
    impl WorkerObserver for LiveObserver {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Observed { process: None }))
                .collect()
        }
    }

    let root = PathBuf::from("/worktree");
    let (states, taints) = LiveObserver.observe_roots_with_taints(std::slice::from_ref(&root));
    assert!(states[&root].is_observed());
    assert_eq!(taints[&root], TaintCause::UnattributedLiveWriter);
    assert!(!taints[&root].is_contention());
}

#[test]
fn census_observation_and_declared_reasons_produce_typed_taints() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fixture = Fixture::new()?;
    let base = canonicalize(fixture._temp.path())?;
    let live_root = base.join("live-root");
    let unknown_root = base.join("unknown-root");
    let free_root = base.join("free-root");
    fs::create_dir(&live_root)?;
    fs::create_dir(&unknown_root)?;
    fs::create_dir(&free_root)?;

    let fact = |pid: i64| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: Some("test-cmd".into()),
    };
    let current = fact(100);
    let live_proc = fact(200);
    let unknown_proc = fact(300);

    let processes = InjectedCensus {
        census: Census::from_observations(
            [current.clone(), live_proc.clone(), unknown_proc.clone()],
            [],
            [],
            true,
            false,
            32,
        )?,
        cwds: BTreeMap::new(),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };

    struct MockCwdObserver {
        live_dir: PathBuf,
        unreadable_file: PathBuf,
    }
    impl CwdObserver for MockCwdObserver {
        fn occupancy(&self, _pid: i64) -> provenant_os::pid::OccupancyObservation {
            provenant_os::pid::OccupancyObservation {
                paths: vec![],
                complete: true,
                gap: None,
            }
        }
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            if pid == 200 {
                Ok(CwdObservation::Path(self.live_dir.clone()))
            } else if pid == 300 {
                Ok(CwdObservation::Path(self.unreadable_file.clone()))
            } else {
                Ok(CwdObservation::Gone)
            }
        }
    }

    let unreadable_file = unknown_root.join("file-not-dir");
    fs::write(&unreadable_file, b"not a dir")?;

    let cwd_obs = MockCwdObserver {
        live_dir: live_root.clone(),
        unreadable_file: unreadable_file.join("sub"),
    };

    let roots = vec![live_root.clone(), unknown_root.clone(), free_root.clone()];
    let (states, taints) =
        observe_worker_root_taints(&roots, &processes, &cwd_obs, &current, false);

    assert!(states[&live_root].is_observed());
    assert!(states[&unknown_root].is_unknown());
    assert!(states[&free_root].is_available());

    let live_taint = &taints[&live_root];
    assert!(!live_taint.is_contention());
    assert!(!live_taint.is_uncertainty());
    assert!(matches!(live_taint, TaintCause::LiveWriter(p) if p.pid == 200));

    let unknown_taint = &taints[&unknown_root];
    assert!(!unknown_taint.is_contention());
    assert!(unknown_taint.is_uncertainty());
    assert!(matches!(unknown_taint, TaintCause::Uncertainty(_)));

    let lock = TaintCause::LockContention;
    assert!(lock.is_contention());
    assert!(!lock.is_uncertainty());

    for reason in UncertaintyReason::ALL {
        let cause = match reason {
            UncertaintyReason::CensusUnavailable
            | UncertaintyReason::UnstableProcessIdentity
            | UncertaintyReason::CwdUnreadable
            | UncertaintyReason::OccupancyIncomplete
            | UncertaintyReason::PathUnplaceable
            | UncertaintyReason::ListingIncomplete
            | UncertaintyReason::PolicyRejected
            | UncertaintyReason::RootUnreadable => TaintCause::Uncertainty(*reason),
        };
        assert!(!cause.is_contention());
        assert!(cause.is_uncertainty());
    }

    assert!(!taints.contains_key(&free_root));

    Ok(())
}

#[test]
fn census_refusal_reports_the_declared_process_evidence_kind() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};

    struct EvidenceObserver {
        tree_root: PathBuf,
        cwd: PathBuf,
        executable: PathBuf,
        held: PathBuf,
        kind: ProcessEvidenceKind,
    }
    impl CwdObserver for EvidenceObserver {
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            if pid != 200 {
                return Ok(CwdObservation::Gone);
            }
            Ok(if self.kind == ProcessEvidenceKind::WorkingDirectory {
                CwdObservation::Path(self.tree_root.clone())
            } else {
                CwdObservation::Path(self.cwd.clone())
            })
        }
        fn executable(&self, pid: i64) -> Option<PathBuf> {
            (pid == 200).then(|| self.executable.clone())
        }
        fn occupancy(&self, pid: i64) -> provenant_os::pid::OccupancyObservation {
            // The system observer reports the running image among the
            // occupancy paths, as it does any other held file.
            let held = match self.kind {
                ProcessEvidenceKind::OtherHeldPath => Some(self.held.clone()),
                ProcessEvidenceKind::ExecutableImage => Some(self.executable.clone()),
                _ => None,
            };
            if let (200, Some(held)) = (pid, held) {
                provenant_os::pid::OccupancyObservation {
                    paths: vec![held],
                    complete: true,
                    gap: None,
                }
            } else {
                provenant_os::pid::OccupancyObservation::default()
            }
        }
    }

    struct EvidenceWorkers<'a> {
        processes: &'a InjectedCensus,
        observer: EvidenceObserver,
    }
    impl WorkerObserver for EvidenceWorkers<'_> {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            observe_worker_roots(
                roots,
                self.processes,
                &self.observer,
                &self.processes.current,
                false,
            )
            .0
        }
        fn live_process(&self, root: &Path) -> Option<ObservedProcess> {
            let (_, holders, _) = observe_worker_holders(
                std::slice::from_ref(&root.to_path_buf()),
                self.processes,
                &self.observer,
                &self.processes.current,
                false,
            );
            holders.get(root).cloned()
        }
    }

    for kind in [
        ProcessEvidenceKind::WorkingDirectory,
        ProcessEvidenceKind::ExecutableImage,
        ProcessEvidenceKind::OtherHeldPath,
    ] {
        let fixture = Fixture::new()?;
        let base = canonicalize(fixture._temp.path())?;
        let root = base.join("root");
        let external = base.join("outside");
        fs::create_dir(&root)?;
        fs::create_dir(&external)?;
        let image = root.join("image");
        let held = if kind == ProcessEvidenceKind::OtherHeldPath {
            root.join("held-data")
        } else {
            image.clone()
        };
        fs::write(&image, b"executable image")?;
        fs::write(&held, b"held file")?;
        #[cfg(unix)]
        fs::set_permissions(&image, fs::Permissions::from_mode(0o755))?;
        let executable = if kind == ProcessEvidenceKind::ExecutableImage {
            #[cfg(unix)]
            {
                let alias = external.join("image-alias");
                std::os::unix::fs::symlink(&image, &alias)?;
                alias
            }
            #[cfg(not(unix))]
            {
                image.clone()
            }
        } else {
            external.join("different-image")
        };
        let fact = |pid| ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: pid,
                session_id: pid,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let current = fact(100);
        let worker = fact(200);
        let processes = InjectedCensus {
            census: Census::from_observations([current.clone(), worker], [], [], true, false, 32)?,
            cwds: BTreeMap::new(),
            current,
            platform_uids: BTreeMap::new(),
        };
        let observer = EvidenceWorkers {
            processes: &processes,
            observer: EvidenceObserver {
                tree_root: root.clone(),
                cwd: external,
                executable,
                held,
                kind,
            },
        };
        let refusal = live_refusal(&observer, &root);
        assert!(
            matches!(
                &refusal,
                WorktreeError::LiveProcess(ObservedProcess { pid: 200, kind: actual, .. })
                    if *actual == kind
            ),
            "{kind:?}: {refusal:?}"
        );
    }
    Ok(())
}

#[test]
fn census_keeps_holder_and_evidence_together_across_processes() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let temp = tempfile::tempdir()?;
    let root = canonicalize(temp.path())?;
    let held = root.join("held");
    fs::write(&held, b"held")?;
    let outside = tempfile::tempdir()?;
    let fact = |pid: i64| ProcessFact {
        identity: ProcessIdentity {
            boot: "boot".into(),
            pid,
            native_start: pid as u64,
            group_id: pid,
            session_id: pid,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: Some(format!("process-{pid}")),
    };
    let current = fact(100);
    let processes = InjectedCensus {
        census: Census::from_observations(
            [current.clone(), fact(200), fact(300)],
            [],
            [],
            true,
            false,
            32,
        )?,
        cwds: BTreeMap::new(),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };
    struct Observer {
        root: PathBuf,
        held: PathBuf,
        outside: PathBuf,
    }
    impl CwdObserver for Observer {
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(CwdObservation::Path(if pid == 300 {
                self.root.clone()
            } else {
                self.outside.clone()
            }))
        }
        fn occupancy(&self, pid: i64) -> provenant_os::pid::OccupancyObservation {
            provenant_os::pid::OccupancyObservation {
                paths: if pid == 200 {
                    vec![self.held.clone()]
                } else {
                    vec![]
                },
                complete: true,
                gap: None,
            }
        }
    }
    let (states, holders, _, taints) = observe_worker_holders_with_taints_and_audit(
        std::slice::from_ref(&root),
        &processes,
        &Observer {
            root: root.clone(),
            held,
            outside: outside.path().to_path_buf(),
        },
        &current,
        false,
        &mut |_, _| {},
        &[],
    );
    let holder = &holders[&root];
    assert!(matches!(
        &states[&root],
        Occupancy::Observed { process: Some(process) } if process == holder
    ));
    assert_eq!(holder.pid, 200);
    assert_eq!(holder.kind, ProcessEvidenceKind::OtherHeldPath);
    assert!(
        matches!(&taints[&root], TaintCause::LiveWriter(p) if p.pid == 200 && p.kind == ProcessEvidenceKind::OtherHeldPath)
    );
    Ok(())
}

#[test]
fn bounded_session_keeps_policy_fence_and_positive_holders()
-> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct ChangingPolicy {
        changed: AtomicBool,
        observed: bool,
    }
    impl WorkerObserver for ChangingPolicy {
        fn epoch_policy(&self) -> Option<CensusEpochPolicy> {
            (!self.changed.load(Ordering::SeqCst)).then(CensusEpochPolicy::default)
        }
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            self.changed.store(true, Ordering::SeqCst);
            roots
                .iter()
                .cloned()
                .map(|root| {
                    (
                        root,
                        if self.observed {
                            Occupancy::Observed { process: None }
                        } else {
                            Occupancy::Available
                        },
                    )
                })
                .collect()
        }
    }
    let temp = tempfile::tempdir()?;
    let roots = [temp.path().to_path_buf()];
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_unix_seconds(0)?,
        provenant_core::MonoTime::from_micros(1),
    );
    for observed in [false, true] {
        let workers = ChangingPolicy {
            changed: AtomicBool::new(false),
            observed,
        };
        let session = CensusSession::new(&workers, &clock);
        let (states, _) = session
            .observe_roots_while(&roots, &|| true)
            .ok_or("census")?;
        if observed {
            assert!(states[&roots[0]].is_observed());
        } else {
            assert!(states[&roots[0]].is_unknown());
        }
    }
    Ok(())
}

#[test]
fn held_report_uses_one_bounded_census_and_only_reports() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Dwellers(AtomicUsize);
    impl WorkerObserver for Dwellers {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Observed { process: None }))
                .collect()
        }
        fn observe_evidence_while(
            &self,
            roots: &[PathBuf],
            _: bool,
            keep_going: &dyn Fn() -> bool,
        ) -> Option<WorkerEvidence> {
            self.0.fetch_add(1, Ordering::SeqCst);
            if !keep_going() {
                return None;
            }
            let mut held = HeldProcesses::new();
            for root in roots {
                held.insert(
                    root.clone(),
                    vec![
                        ObservedProcess::cwd(101, Some("zsh -c burn".into())),
                        ObservedProcess::executable(202, Some("/usr/bin/yes".into())),
                    ],
                );
            }
            Some(WorkerEvidence {
                states: roots
                    .iter()
                    .cloned()
                    .map(|root| (root, Occupancy::Observed { process: None }))
                    .collect(),
                held,
                ..WorkerEvidence::default()
            })
        }
    }
    let temp = tempfile::tempdir()?;
    let workers = Dwellers(AtomicUsize::new(0));
    let roots = vec![temp.path().join("lane")];
    let report = held_report(&workers, &roots, &|| true);
    assert_eq!(workers.0.load(Ordering::SeqCst), 1, "one census per pass");
    assert!(!report.incomplete);
    assert_eq!(report.processes.len(), 2);
    assert!(
        report
            .processes
            .iter()
            .any(|row| row.pid == 101 && row.command.as_deref() == Some("zsh -c burn"))
    );
    assert!(report.processes.iter().any(|row| row.pid == 202));
    for row in &report.processes {
        let line = row.advisory();
        assert!(line.contains("report only"), "{line}");
        assert!(line.contains(&row.pid.to_string()), "{line}");
        assert!(line.contains(&row.evidence.to_string()), "{line}");
    }
    // A cut-short census reports incompleteness rather than an empty result.
    let cut = held_report(&workers, &roots, &|| false);
    assert!(cut.incomplete);
    assert!(cut.processes.is_empty());
    Ok(())
}

/// Two foreign processes sharing one root are both named; a sibling in the
/// caller's session is not. This drives the real `observe_worker_holders_while`
/// path, where `holders` keeps only the first process per root.
#[test]
fn failed_process_listing_makes_held_report_incomplete() {
    use provenant_supervisor::census::{Census, CensusError, ProcessIdentity};
    struct FailedListing;
    impl ProcessObserver for FailedListing {
        fn boot_identity(&self) -> Result<String, CensusError> {
            Ok("boot".into())
        }
        fn observe(&self, _: i64) -> ProcessObservation {
            ProcessObservation::Absent
        }
        fn census(&self, _: usize) -> Result<Census, CensusError> {
            Err(CensusError::ProcessList(std::io::Error::other(
                "listing failed",
            )))
        }
    }
    impl CwdObserver for FailedListing {
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(CwdObservation::Gone)
        }
    }
    impl WorkerObserver for FailedListing {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Available))
                .collect()
        }
        fn observe_evidence_while(
            &self,
            roots: &[PathBuf],
            _: bool,
            keep_going: &dyn Fn() -> bool,
        ) -> Option<WorkerEvidence> {
            let current = ProcessFact {
                identity: ProcessIdentity {
                    boot: "boot".into(),
                    pid: 1,
                    native_start: 1,
                    group_id: 1,
                    session_id: 1,
                },
                parent_pid: 0,
                zombie: false,
                cpu_micros: None,
                command: None,
            };
            keep_going().then(|| {
                worker_evidence_from_census(
                    roots,
                    true,
                    self,
                    self,
                    &current,
                    Some(keep_going),
                    &[],
                )
            })
        }
    }
    let report = held_report(&FailedListing, &[PathBuf::from("/fixture")], &|| true);
    assert!(report.incomplete);
    assert!(report.processes.is_empty());
}

#[test]
fn held_report_names_every_foreign_dweller_and_excludes_the_callers_session()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let temp = tempfile::tempdir()?;
    let root = canonicalize(temp.path())?;
    for name in ["caller", "first", "second", "sibling"] {
        fs::create_dir(temp.path().join(name))?;
    }
    let fact = |pid: i64, session_id: i64, command: &str| ProcessFact {
        identity: ProcessIdentity {
            boot: "dwellers".into(),
            pid,
            native_start: pid as u64,
            group_id: session_id,
            session_id,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: Some(command.into()),
    };
    let current = fact(100, 100, "provenant");
    let census = Census::from_observations(
        [
            current.clone(),
            fact(201, 201, "zsh -c burn"),
            fact(202, 202, "yes"),
            fact(103, 100, "caller sibling"),
        ],
        [],
        [],
        true,
        false,
        32,
    )?;
    let observer = InjectedCensus {
        census,
        cwds: BTreeMap::from([
            (100, CwdObservation::Path(temp.path().join("caller"))),
            (103, CwdObservation::Path(temp.path().join("sibling"))),
            (201, CwdObservation::Path(temp.path().join("first"))),
            (202, CwdObservation::Path(temp.path().join("second"))),
        ]),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };
    // The existing census fixture includes both positive dwellers. Marking the
    // listing incomplete must retain those rows while exposing the gap.
    let partial = Census::from_observations(
        observer.census.facts().cloned().collect::<Vec<_>>(),
        [],
        [],
        false,
        false,
        32,
    )?;
    let partial_observer = InjectedCensus {
        census: partial,
        cwds: observer.cwds.clone(),
        current: current.clone(),
        platform_uids: observer.platform_uids.clone(),
    };
    let evidence = worker_evidence_from_census(
        std::slice::from_ref(&root),
        true,
        &partial_observer,
        &partial_observer,
        &current,
        Some(&|| true),
        &[],
    );
    assert!(evidence.held_incomplete);
    assert!(
        evidence.held[&root]
            .iter()
            .any(|process| process.pid == 201)
    );
    let mut held = HeldProcesses::new();
    let (states, _, _, _) = observe_worker_holders_while(
        std::slice::from_ref(&root),
        &observer,
        &observer,
        &current,
        true,
        &mut |_, _| {},
        &mut held,
        Some(&|| true),
        &[],
    );
    assert!(states[&root].is_observed());
    let rows = &held[&root];
    let pids: BTreeSet<_> = rows.iter().map(|row| row.pid).collect();
    assert_eq!(pids, BTreeSet::from([201, 202]), "{rows:?}");
    assert!(
        rows.iter()
            .any(|row| row.pid == 201 && row.command.as_deref() == Some("zsh -c burn"))
    );
    assert!(
        rows.iter()
            .any(|row| row.pid == 202 && row.command.as_deref() == Some("yes"))
    );
    Ok(())
}

/// EPERM processes are still aggregated into the held report, but the caller
/// is excluded even when the listing could not name it.
#[test]
fn held_report_aggregates_eperm_dwellers_and_excludes_the_caller() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let temp = tempfile::tempdir()?;
    let root = canonicalize(temp.path())?;
    for name in ["caller", "first", "second"] {
        fs::create_dir(temp.path().join(name))?;
    }
    let current = ProcessFact {
        identity: ProcessIdentity {
            boot: "eperm-dwellers".into(),
            pid: 100,
            native_start: 100,
            group_id: 100,
            session_id: 100,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: Some("provenant".into()),
    };
    let census = Census::from_observations(
        [],
        [
            (100, ObservationIssue::PermissionDenied),
            (201, ObservationIssue::PermissionDenied),
            (202, ObservationIssue::PermissionDenied),
        ],
        [],
        true,
        false,
        32,
    )?;
    let observer = InjectedCensus {
        census,
        cwds: BTreeMap::from([
            (100, CwdObservation::Path(temp.path().join("caller"))),
            (201, CwdObservation::Path(temp.path().join("first"))),
            (202, CwdObservation::Path(temp.path().join("second"))),
        ]),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };
    let mut held = HeldProcesses::new();
    let _ = observe_worker_holders_while(
        std::slice::from_ref(&root),
        &observer,
        &observer,
        &current,
        true,
        &mut |_, _| {},
        &mut held,
        None,
        &[],
    );
    let rows = &held[&root];
    let pids: BTreeSet<_> = rows.iter().map(|row| row.pid).collect();
    assert_eq!(pids, BTreeSet::from([201, 202]), "{rows:?}");
    assert!(
        rows.iter()
            .all(|row| row.kind == ProcessEvidenceKind::WorkingDirectory)
    );
    Ok(())
}

struct StartTimeSignals<'a> {
    inner: &'a UnreadableSignals,
    start_times: Vec<Option<provenant_core::WallTime>>,
    reads: std::sync::atomic::AtomicUsize,
}

impl CwdObserver for StartTimeSignals<'_> {
    fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
        self.inner.cwd(pid)
    }
    fn occupancy(&self, pid: i64) -> provenant_os::pid::OccupancyObservation {
        self.inner.occupancy(pid)
    }
    fn process_uids(&self, pid: i64) -> Option<ProcessUids> {
        self.inner.process_uids(pid)
    }
    fn platform_signed_image(&self, pid: i64) -> Option<provenant_os::pid::PlatformSignedImage> {
        self.inner.platform_signed_image(pid)
    }
    fn configured_unreadable_image(
        &self,
        pid: i64,
        entries: &[CensusIgnoreEntry],
    ) -> Option<CensusIgnoreEntry> {
        self.inner.configured_unreadable_image(pid, entries)
    }
    fn process_start_time(&self, _: i64) -> Option<provenant_core::WallTime> {
        if self.start_times.is_empty() {
            return None;
        }
        let index = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.start_times
            .get(index)
            .or_else(|| self.start_times.last())
            .copied()
            .flatten()
    }
    fn unreadable_policy_uids(
        &self,
        pid: i64,
        entries: &[CensusIgnoreEntry],
    ) -> Result<
        provenant_os::pid::UnreadablePolicyExcusal,
        provenant_os::pid::UnreadablePolicyFailure,
    > {
        self.inner.unreadable_policy_uids(pid, entries)
    }
    fn executable(&self, pid: i64) -> Option<PathBuf> {
        self.inner.executable(pid)
    }
    fn process_details(&self, pid: i64) -> Option<String> {
        self.inner.process_details(pid)
    }
}

type RootObservation = (
    BTreeMap<PathBuf, Occupancy>,
    Vec<provenant_records::warning::Warning>,
);

fn observe_unreadable_roots_with_starts(
    roots: &[PathBuf],
    signals: &UnreadableSignals,
    start_times: Vec<Option<provenant_core::WallTime>>,
) -> Result<RootObservation, Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ObservationIssue};
    let current = refused_fact(100);
    let processes = InjectedCensus {
        census: Census::from_observations(
            [current.clone()],
            [(200, ObservationIssue::PermissionDenied)],
            [],
            true,
            false,
            32,
        )?,
        cwds: BTreeMap::new(),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };
    let observer = StartTimeSignals {
        inner: signals,
        start_times,
        reads: std::sync::atomic::AtomicUsize::new(0),
    };
    let (states, warnings) = provenant_records::warning::capture(|| {
        observe_worker_holders_with_taints_and_audit(
            roots,
            &processes,
            &observer,
            &current,
            false,
            &mut |_, _| {},
            &signals.entries,
        )
        .0
    });
    Ok((states, warnings))
}

#[test]
fn started_before_root_requires_two_second_margin_and_stable_start_time()
-> Result<(), Box<dyn Error>> {
    use provenant_core::WallTime;
    use provenant_os::pid::{STARTED_BEFORE_MARGIN_MICROS, StartedBefore};

    let fixture = Fixture::new()?;
    let root = fixture.root.join("login-floor-present");
    fs::create_dir(&root)?;
    let birth = fs::metadata(&root)?
        .created()?
        .duration_since(std::time::UNIX_EPOCH)?;
    let birth = WallTime::from_micros(i64::try_from(birth.as_micros())?)?;
    let at_margin = WallTime::from_micros(birth.as_micros() - STARTED_BEFORE_MARGIN_MICROS)?;
    let too_recent = WallTime::from_micros(birth.as_micros() - STARTED_BEFORE_MARGIN_MICROS + 1)?;
    let much_earlier = WallTime::from_micros(birth.as_micros() - 10_000_000)?;
    let after_root = WallTime::from_micros(birth.as_micros() + 1_000_000)?;

    let owner = provenant_os::fs::effective_uid();
    let elevated = if owner == 0 { 1 } else { 0 };
    let signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: owner,
            effective: elevated,
            saved: elevated,
        }),
        platform_signed: true,
        entries: vec![CensusIgnoreEntry {
            path: PathBuf::from("/usr/bin/login"),
            team_id: "platform".into(),
            started_before: Some(StartedBefore::Root),
        }],
        team_id: None,
        executable: Some(PathBuf::from("/usr/bin/login")),
        cwd: CwdObservation::PermissionDenied,
        occupancy_paths: Vec::new(),
    };

    for allowed_start in [much_earlier, at_margin] {
        let (states, warnings) = observe_unreadable_roots_with_starts(
            std::slice::from_ref(&root),
            &signals,
            vec![Some(allowed_start)],
        )?;
        assert!(states[&root].is_available(), "{states:?}");
        assert!(warning_names_process(&warnings, 200), "{warnings:?}");
    }

    for blocked in [
        vec![Some(too_recent)],
        vec![Some(after_root)],
        vec![None],
        vec![Some(much_earlier), Some(at_margin)],
        vec![Some(much_earlier), None],
    ] {
        let (states, warnings) = observe_unreadable_roots_with_starts(
            std::slice::from_ref(&root),
            &signals,
            blocked.clone(),
        )?;
        assert!(
            states[&root].is_unknown(),
            "{blocked:?} should fail closed: {states:?}"
        );
        assert!(!warning_names_process(&warnings, 200), "{warnings:?}");
    }
    Ok(())
}

#[test]
fn started_before_root_fails_closed_when_any_root_absent_until_at_floor_supplied()
-> Result<(), Box<dyn Error>> {
    use provenant_core::WallTime;
    use provenant_os::pid::StartedBefore;

    let fixture = Fixture::new()?;
    let present = fixture.root.join("floor-present");
    fs::create_dir(&present)?;
    let removed = fixture.root.join("floor-removed");
    let floor = WallTime::from_micros(1_780_000_010_000_000)?;
    let old_start = WallTime::from_micros(1_780_000_005_000_000)?;
    let recent_start = WallTime::from_micros(1_780_000_009_000_000)?;

    let owner = provenant_os::fs::effective_uid();
    let elevated = if owner == 0 { 1 } else { 0 };
    let mut signals = UnreadableSignals {
        uids: Some(ProcessUids {
            real: owner,
            effective: elevated,
            saved: elevated,
        }),
        platform_signed: true,
        entries: vec![CensusIgnoreEntry {
            path: PathBuf::from("/usr/bin/login"),
            team_id: "platform".into(),
            started_before: Some(StartedBefore::Root),
        }],
        team_id: None,
        executable: Some(PathBuf::from("/usr/bin/login")),
        cwd: CwdObservation::PermissionDenied,
        occupancy_paths: Vec::new(),
    };

    // Removed root with StartedBefore::Root has no floor -> fails closed.
    let (states, _) = observe_unreadable_roots_with_starts(
        std::slice::from_ref(&removed),
        &signals,
        vec![Some(old_start)],
    )?;
    assert!(states[&removed].is_unknown(), "{states:?}");

    // Mixed [present, absent] roots: missing floor for any scoped root fails closed.
    let (mixed, _) = observe_unreadable_roots_with_starts(
        &[present.clone(), removed.clone()],
        &signals,
        vec![Some(old_start)],
    )?;
    assert!(mixed[&present].is_unknown(), "{mixed:?}");
    assert!(mixed[&removed].is_unknown(), "{mixed:?}");

    // Supplying At(floor) excuses only when start_time precedes floor by >= 2s.
    signals.entries[0].started_before = Some(StartedBefore::At(floor));
    let (excused, warnings) = observe_unreadable_roots_with_starts(
        std::slice::from_ref(&removed),
        &signals,
        vec![Some(old_start)],
    )?;
    assert!(excused[&removed].is_available(), "{excused:?}");
    assert!(warning_names_process(&warnings, 200), "{warnings:?}");

    let (rejected, _) = observe_unreadable_roots_with_starts(
        std::slice::from_ref(&removed),
        &signals,
        vec![Some(recent_start)],
    )?;
    assert!(rejected[&removed].is_unknown(), "{rejected:?}");
    Ok(())
}

#[test]
fn policy_workers_with_creation_floor_rewrites_root_entries_only_when_supplied()
-> Result<(), Box<dyn Error>> {
    use provenant_core::WallTime;
    use provenant_os::pid::StartedBefore;

    struct Probe(std::sync::Mutex<Vec<Vec<CensusIgnoreEntry>>>);
    impl WorkerObserver for Probe {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| (root.clone(), Occupancy::census_unknown(root, "fixture")))
                .collect()
        }
        fn observe_roots_with_policy(
            &self,
            roots: &[PathBuf],
            entries: &[CensusIgnoreEntry],
            _: bool,
        ) -> WorkerObservations {
            self.0.lock().unwrap().push(entries.to_vec());
            (self.observe_roots(roots), BTreeMap::new())
        }
    }

    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(fixture._temp.path().join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let floor = WallTime::from_micros(1_780_000_020_000_000)?;
    let probe = Probe(Default::default());

    let plain = CensusPolicyWorkers::from_roots(&probe, &roots)?;
    let _ = plain.observe_roots(std::slice::from_ref(&root));
    let floored = CensusPolicyWorkers::from_roots(&probe, &roots)?.with_creation_floor(floor);
    let _ = floored.observe_roots(std::slice::from_ref(&root));
    let clock = provenant_core::TestClock::new(floor, provenant_core::MonoTime::from_micros(0));
    let session = CensusSession::new(&floored, &clock);
    let _ = session.observe_roots(std::slice::from_ref(&root));

    let seen = probe.0.lock().unwrap();
    assert_eq!(seen[0][0].started_before, Some(StartedBefore::Root));
    assert_eq!(seen[1][0].started_before, Some(StartedBefore::At(floor)));
    assert_eq!(seen[2][0].started_before, Some(StartedBefore::At(floor)));
    Ok(())
}

#[test]
fn worktree_owner_for_path_matches_removed_checkout_by_directory_identity()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let canonical = canonicalize(&fixture.root)?;
    let common = canonical.join("repo/.git");
    let registration = common.join("worktrees/lane-1");
    fs::create_dir_all(&registration)?;
    let removed = canonical.join("trees/lane-1");
    let owner = super::owner::WorktreeOwner {
        attempt_id: Some(provenant_core::AttemptId::from_bytes([7; 16])),
        operation_id: Some("op-owner".parse()?),
        branch: "fix/3914".into(),
        base: "1".repeat(40),
        host: provenant_core::HostId::from_bytes([2; 16]),
        created_at: provenant_core::WallTime::from_micros(1_780_000_000_000_000)?,
        origin: super::owner::WorktreeOrigin::Lane,
        label: None,
        reference: None,
    };
    fs::write(
        registration.join("gitdir"),
        format!("{}/.git\n", removed.display()),
    )?;
    fs::write(
        registration.join("provenant-owner.json"),
        serde_json::to_vec(&owner)?,
    )?;

    assert_eq!(
        super::owner::worktree_owner_for_path(&common, &removed)?,
        Some(owner.clone())
    );
    // The registration's birth survives the checkout's removal and is the
    // creation floor an abandon uses (#3914).
    let registered = super::owner::worktree_registration_for_path(&common, &removed)?;
    assert_eq!(
        registered,
        Some((
            owner.clone(),
            super::census_excusal::birth_time(&registration)
        ))
    );
    #[cfg(target_os = "macos")]
    assert!(
        registered.is_some_and(|(_, born)| born.is_some()),
        "APFS reports a registration birth time"
    );
    #[cfg(unix)]
    {
        let alias = canonical.join("alias");
        std::os::unix::fs::symlink(&canonical, &alias)?;
        assert_eq!(
            super::owner::worktree_owner_for_path(&common, &alias.join("trees/lane-1"))?,
            Some(owner)
        );
    }
    assert_eq!(
        super::owner::worktree_owner_for_path(&common, &canonical.join("trees/other"))?,
        None
    );
    assert_eq!(
        super::owner::worktree_owner_for_path(&common, Path::new("trees/lane-1"))?,
        None
    );
    Ok(())
}
