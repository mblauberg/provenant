//! Worker census and native observation tests.
use super::lifecycle_tests::*;
use super::*;
use crate::test_support::Fixture;
use provenant_supervisor::census::ObservationIssue;
use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;

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
fn session_refusal_uses_decision_evidence_and_missing_root_is_unknown() -> Result<(), Box<dyn Error>>
{
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
    let missing = root.join("missing");
    assert!(session.observe_roots(std::slice::from_ref(&missing))[&missing].is_unknown());
    let states = session.observe_roots(&[root.clone(), missing.clone()]);
    assert!(states[&root].is_available());
    assert!(states[&missing].is_unknown());
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
fn land_worker_census_excludes_only_ancestors_through_the_session_leader()
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
        // Above-session ancestors, siblings, descendants and independent
        // sessions remain live, even when the census is incomplete.
        for pid in [1, 95, 110, 200] {
            cwds.0.insert(pid, root.clone());
            assert!(observe(&cwds, true).is_observed(), "pid {pid}");
            cwds.0.insert(pid, outside.clone());
        }
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

/// A cwd observer whose queries are refused, with host policy answered from a
/// configured entry list the way the native observer does.
struct RefusedCwd {
    uids: ProcessUids,
}
impl CwdObserver for RefusedCwd {
    fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
        provenant_os::pid::OccupancyObservation {
            paths: vec![],
            complete: true,
            gap: None,
        }
    }
    fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
        Ok(CwdObservation::PermissionDenied)
    }
    fn unreadable_policy_uids(
        &self,
        _pid: i64,
        _: &[CensusIgnoreEntry],
    ) -> Result<
        provenant_os::pid::UnreadablePolicyExcusal,
        provenant_os::pid::UnreadablePolicyFailure,
    > {
        if !self.uids.all_system() {
            return Err("real/effective/saved system uids below 500 required".into());
        }
        Err(provenant_os::pid::UnreadablePolicyFailure {
            condition: "approved daemon directory required",
            builtin_condition: Some("approved daemon directory required"),
            configurable: true,
        })
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

fn observe_refused(
    root: &Path,
    pids: &[i64],
    cwd: &RefusedCwd,
) -> Result<WorkerTaintObservations, Box<dyn Error>> {
    use provenant_supervisor::census::Census;
    let current = refused_fact(100);
    let mut facts = vec![current.clone()];
    facts.extend(pids.iter().map(|pid| refused_fact(*pid)));
    let processes = InjectedCensus {
        census: Census::from_observations(facts, [], [], true, false, 32)?,
        cwds: BTreeMap::new(),
        current: current.clone(),
        platform_uids: BTreeMap::new(),
    };
    Ok(observe_worker_root_taints(
        std::slice::from_ref(&root.to_path_buf()),
        &processes,
        cwd,
        &current,
        false,
    ))
}

const SYSTEM_UIDS: ProcessUids = ProcessUids {
    real: 0,
    effective: 0,
    saved: 0,
};

#[test]
fn unreadable_census_refusal_stays_unknown_when_policy_rejects() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let configured_refusal = RefusedCwd { uids: SYSTEM_UIDS };
    let configured_failure = configured_refusal
        .unreadable_policy_uids(200, &[])
        .expect_err("the configured entry does not match this fixture");
    assert!(configured_failure.configurable);
    assert!(configured_failure.builtin_condition.is_some());
    let (refused, taints) = observe_refused(&root, &[200], &configured_refusal)?;
    assert!(refused[&root].is_unknown());
    assert_eq!(
        taints[&root],
        TaintCause::Uncertainty(UncertaintyReason::PolicyRejected)
    );

    let user_owned_refusal = RefusedCwd {
        uids: ProcessUids {
            real: 0,
            effective: 501,
            saved: 0,
        },
    };
    let user_owned_failure = user_owned_refusal
        .unreadable_policy_uids(200, &[])
        .expect_err("a user-owned process cannot use a system entry");
    assert!(!user_owned_failure.configurable);
    assert!(user_owned_failure.builtin_condition.is_some());
    let (user_owned, taints) = observe_refused(&root, &[200], &user_owned_refusal)?;
    assert!(user_owned[&root].is_unknown());
    assert_eq!(
        taints[&root],
        TaintCause::Uncertainty(UncertaintyReason::PolicyRejected)
    );
    Ok(())
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
    // Owner/identity lookup failure never licenses exclusion or placement.
    let missing = root.join("missing-root");
    assert!(observer.observe_roots(std::slice::from_ref(&missing))[&missing].is_unknown());
    Ok(())
}

#[test]
fn worker_census_audit_keeps_all_unplaceable_processes_and_first_detail()
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
    assert_eq!(
        details[&root],
        "process 1: cwd unreadable; policy_condition=image policy evidence unavailable"
    );
    assert_eq!(
        audit.keys().copied().collect::<Vec<_>>(),
        vec![Some(1), Some(200)]
    );
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
            team_id: "TEAM123".into()
        }]
    );
    assert!(
        seen[1].is_empty(),
        "policy must not leak across roots or wrappers"
    );
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
                let (states, holders, _, _) = observe_worker_holders_while(
                    std::slice::from_ref(&root),
                    &processes,
                    &observation,
                    &processes.current,
                    false,
                    &mut |_, _| {},
                    Some(&|| true),
                );
                assert_eq!(states[&root].is_observed(), held, "{gap:?}");
                assert_eq!(observation.attempts.load(Ordering::SeqCst), 2);
                assert_eq!(holders.contains_key(&root), held);
            }
        }
        Ok(())
    }

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
        let (states, _, details, _) = observe_worker_holders_while(
            std::slice::from_ref(&root),
            &processes,
            &scripted,
            &current,
            false,
            &mut |pid, condition| audit.push((pid, condition)),
            None,
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
