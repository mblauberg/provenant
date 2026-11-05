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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Counting(AtomicUsize, AtomicBool);
    impl WorkerObserver for Counting {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            self.0.fetch_add(1, Ordering::SeqCst);
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Free))
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
        provenant_os::clock::SystemClock.wall_now()?,
        provenant_core::MonoTime::from_micros(42),
    );
    let observer = Counting(AtomicUsize::new(0), AtomicBool::new(false));
    let session = CensusSession::new(&observer, &clock);
    assert_eq!(
        session.observe_roots(std::slice::from_ref(&a))[&a],
        WorkerState::Free
    );
    assert_eq!(
        session
            .observe_roots_with_details(std::slice::from_ref(&a))
            .0[&a],
        WorkerState::Free
    );
    assert_eq!(session.observed_at(), Some(clock.mono_now()));
    assert_eq!(observer.0.load(Ordering::SeqCst), 1);
    session.observe_roots(&[a.clone(), b.clone()]);
    session.observe_roots(std::slice::from_ref(&a));
    assert_eq!(observer.0.load(Ordering::SeqCst), 2);
    session.observe_roots_for_land(std::slice::from_ref(&a));
    session.invalidate();
    session.observe_roots_for_land(std::slice::from_ref(&a));
    fs::rename(&a, fixture.root.join("session-old"))?;
    fs::create_dir(&a)?;
    session.observe_roots_for_land(std::slice::from_ref(&a));
    assert_eq!(observer.0.load(Ordering::SeqCst), 5);
    let second_command = CensusSession::new(&observer, &clock);
    second_command.observe_roots(std::slice::from_ref(&a));
    assert_eq!(observer.0.load(Ordering::SeqCst), 6);
    observer.1.store(true, Ordering::SeqCst);
    second_command.observe_roots(std::slice::from_ref(&a));
    assert_eq!(observer.0.load(Ordering::SeqCst), 7);
    Ok(())
}

#[test]
fn session_acquires_with_captured_policy_during_aba() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Changing {
        policy_b: AtomicBool,
        acquisitions: AtomicUsize,
    }
    impl WorkerObserver for Changing {
        fn observe_roots(&self, _: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
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
            self.acquisitions.fetch_add(1, Ordering::SeqCst);
            self.policy_b.store(true, Ordering::SeqCst);
            let state = if policy.exclude_ancestors {
                WorkerState::Free
            } else {
                WorkerState::Live
            };
            self.policy_b.store(false, Ordering::SeqCst);
            WorkerEvidence {
                states: roots.iter().cloned().map(|root| (root, state)).collect(),
                ..WorkerEvidence::default()
            }
        }
    }
    let fixture = Fixture::new()?;
    let clock = provenant_core::TestClock::new(
        provenant_os::clock::SystemClock.wall_now()?,
        provenant_core::MonoTime::from_micros(1),
    );
    let observer = Changing {
        policy_b: AtomicBool::new(false),
        acquisitions: AtomicUsize::new(0),
    };
    let session = CensusSession::new(&observer, &clock);
    assert_eq!(
        session.observe_roots(std::slice::from_ref(&fixture.root))[&fixture.root],
        WorkerState::Live
    );
    assert_eq!(
        session.observe_roots(std::slice::from_ref(&fixture.root))[&fixture.root],
        WorkerState::Live
    );
    assert_eq!(observer.acquisitions.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn session_refusal_uses_decision_evidence_and_missing_root_is_unknown() -> Result<(), Box<dyn Error>>
{
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Sequenced(AtomicUsize);
    impl WorkerObserver for Sequenced {
        fn observe_roots(&self, _: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            panic!("must acquire evidence together")
        }
        fn observe_evidence(&self, roots: &[PathBuf], _: bool) -> WorkerEvidence {
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            let root = roots[0].clone();
            WorkerEvidence {
                states: BTreeMap::from([(
                    root.clone(),
                    if n == 0 {
                        WorkerState::Live
                    } else {
                        WorkerState::Free
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
        provenant_os::clock::SystemClock.wall_now()?,
        provenant_core::MonoTime::from_micros(1),
    );
    let observer = Sequenced(AtomicUsize::new(0));
    let session = CensusSession::new(&observer, &clock);
    let root = fixture.root.clone();
    let (states, details) = session.observe_roots_with_details(std::slice::from_ref(&root));
    assert_eq!(states[&root], WorkerState::Live);
    assert_eq!(details[&root], "first observation");
    assert_eq!(session.live_process(&root).unwrap().pid, 123);
    let (_, taints) = session.observe_roots_with_taints(std::slice::from_ref(&root));
    assert!(matches!(&taints[&root], TaintCause::LiveWriter(p) if p.pid == 123));
    assert_eq!(observer.0.load(Ordering::SeqCst), 1);
    let missing = root.join("missing");
    assert_eq!(
        session.observe_roots(std::slice::from_ref(&missing))[&missing],
        WorkerState::Unknown
    );
    let states = session.observe_roots(&[root.clone(), missing.clone()]);
    assert_eq!(states[&root], WorkerState::Free);
    assert_eq!(states[&missing], WorkerState::Unknown);
    Ok(())
}

#[test]
fn command_session_is_reused_across_worktree_contexts() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting(AtomicUsize);
    impl WorkerObserver for Counting {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            self.0.fetch_add(1, Ordering::SeqCst);
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Free))
                .collect()
        }
    }
    let fixture = Fixture::new()?;
    let state = fixture._temp.path().join("command-state");
    fs::create_dir(&state)?;
    let mut input = provenant_core::RootInputs::new(fixture.home.clone(), fixture.root.clone());
    input.state_dir = Some(state);
    let roots = input.resolve(&provenant_os::fs::SystemPathResolver)?;
    let clock = provenant_os::clock::SystemClock;
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
                assert_eq!(
                    context.workers.observe_roots(roots)[&fixture.root],
                    WorkerState::Free
                );
                assert_eq!(
                    context.workers.observe_roots_with_details(roots).0[&fixture.root],
                    WorkerState::Free
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
#[allow(clippy::print_stderr, clippy::disallowed_methods)] // Real process list and child are the acceptance fixture.
fn native_full_census_places_clean_merged_worktree_and_same_user_sleep()
-> Result<(), Box<dyn Error>> {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
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
    // Use the same host config selection and policy composition as runtime,
    // so native CI exercises host-owned exceptions rather than task knobs.
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME unavailable")?);
    let mut inputs = provenant_core::RootInputs::new(home, fixture.root.clone());
    inputs.environment_state_dir =
        std::env::var_os(provenant_core::STATE_DIRECTORY_ENV).map(PathBuf::from);
    let host_roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let complete = std::env::var("PROVENANT_CENSUS_HOST_LIST_COMPLETE").is_ok();
    let workers = CensusPolicyWorkers::from_roots(&SystemWorkerObserver, &host_roots)?;
    let ProcessObservation::Present(current) = current_process() else {
        return Err("native audit requires caller identity".into());
    };
    let mut unplaceable = BTreeMap::new();
    let mut incomplete = Vec::new();
    let (clean, _, clean_details) = observe_worker_holders_with_audit(
        std::slice::from_ref(&root),
        &SystemProcessObserver,
        &PolicyCwdObserver(&workers.entries),
        &current,
        true,
        &mut |pid, condition| {
            if let Some(pid) = pid {
                let detail = process_detail(
                    &SystemCwdObserver,
                    pid,
                    condition.unwrap_or("ADMISSIBLE under built-in policy"),
                );
                unplaceable.insert(pid, (condition, detail));
            } else {
                incomplete.push(condition.unwrap_or("unclassified census failure"));
            }
        },
    );
    let first = clean_details.get(&root).map_or("none", String::as_str);
    eprintln!(
        "FULL_CENSUS: clean merged worktree {:?}; first unplaceable detail: {first}",
        clean[&root]
    );
    for (_, detail) in unplaceable.values() {
        eprintln!("UNPLACEABLE: {detail}");
    }
    assert!(
        incomplete.is_empty(),
        "native census audit incomplete: {incomplete:?}"
    );
    assert!(
        builtin_audit_has_only_rejections(unplaceable.values().map(|(condition, _)| *condition)),
        "every unplaceable process must fail a named built-in condition"
    );
    let mut child = Child(
        std::process::Command::new("/bin/sleep")
            .arg("60")
            .current_dir(&root)
            .spawn()?,
    );
    assert!(child.0.try_wait()?.is_none());
    let pid = i64::from(child.0.id());
    assert!(
        SystemCwdObserver.unreadable_policy_uids(pid, &[]).is_err(),
        "same-user /bin/sleep cannot earn daemon exception"
    );
    let (occupied, occupied_details) =
        workers.observe_roots_for_land_with_details(std::slice::from_ref(&root));
    eprintln!(
        "FULL_CENSUS: same-user sleep {pid}: {:?}; placement detail: {:?}",
        occupied[&root],
        occupied_details.get(&root)
    );
    // A readable occupant is positively Live, a stronger refusal than
    // Unknown. Never weaken that existing refusal to satisfy the fixture.
    assert_eq!(occupied[&root], WorkerState::Live);
    assert_eq!(
        occupied_details[&root],
        format!("process {pid}: cwd inside root")
    );
    eprintln!(
        "ASSERTED: native full census audited {} unplaceable processes with named built-in failures; same-user sleep {pid} Live with placement detail",
        unplaceable.len()
    );
    if complete {
        assert_eq!(
            clean[&root],
            WorkerState::Free,
            "native clean full census: {first}"
        );
        eprintln!(
            "ASSERTED: native full census clean merged worktree Free (host list declared complete)"
        );
    } else {
        eprintln!(
            "SKIP: clean-Free assertion: PROVENANT_CENSUS_HOST_LIST_COMPLETE is unset; host exceptions are not declared complete; land cleanup may report the first unlisted daemon: {first}"
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
#[allow(clippy::print_stderr)] // Native fixture skips must explain missing host evidence.
fn native_unreadable_windowserver_allows_cleanup_of_a_merged_clean_tree()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, CensusError};

    // Select the real bystander to isolate its effect on cleanup, but
    // retain native identity rechecks and cwd observation.
    struct WindowServerWorkers {
        census: Census,
        current: ProcessFact,
    }
    impl ProcessObserver for WindowServerWorkers {
        fn boot_identity(&self) -> Result<String, CensusError> {
            SystemProcessObserver::boot_identity()
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            SystemProcessObserver.observe(pid)
        }
        fn census(&self, _: usize) -> Result<Census, CensusError> {
            Ok(self.census.clone())
        }
    }
    impl WorkerObserver for WindowServerWorkers {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            self.observe_roots_with_details(roots).0
        }
        fn observe_roots_with_details(&self, roots: &[PathBuf]) -> WorkerObservations {
            observe_worker_roots(roots, self, &SystemCwdObserver, &self.current, true)
        }
        fn requires_complete_census(&self) -> bool {
            true
        }
    }
    let unavailable = |reason: String| -> Result<(), Box<dyn Error>> {
        provenant_test_support::skip_sandbox_dependent_test(reason)?;
        Ok(())
    };
    let census = match SystemProcessObserver.census(65_536) {
        Ok(census) => census,
        Err(error) => {
            return unavailable(format!("native process census unavailable: {error}"));
        }
    };
    let Some(windowserver) = census
        .facts()
        .find(|fact| fact.command.as_deref() == Some("WindowServer") && !fact.zombie)
    else {
        return unavailable("live WindowServer not visible in native process census".into());
    };
    let pid = windowserver.identity.pid;
    match SystemCwdObserver.cwd(pid) {
        Ok(CwdObservation::PermissionDenied) => {}
        Ok(observation) => {
            return unavailable(format!(
                "WindowServer cwd is not unreadable: {observation:?}"
            ));
        }
        Err(error) => return unavailable(format!("WindowServer cwd query failed: {error}")),
    }
    let ProcessObservation::Present(current) =
        SystemProcessObserver.observe(i64::from(std::process::id()))
    else {
        return Err("native caller identity unavailable".into());
    };
    let workers = WindowServerWorkers {
        census: Census::from_observations(
            [current.clone(), windowserver.clone()],
            [],
            [],
            true,
            false,
            32,
        )?,
        current,
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
    let result = remove(
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
    assert!(result.is_ok(), "WindowServer {pid}: {result:?}");
    assert!(!created.path.exists());
    assert!(!branch_exists(&fixture, "landed"));
    assert!(
        matches!(
            observe_same_process(&SystemProcessObserver, &SystemCwdObserver, windowserver),
            NativeCwd::PolicyExcluded(excusal) if excusal.uids.all_system() && excusal.uids.all_differ_from(provenant_os::fs::effective_uid())
        ),
        "WindowServer must still be unreadable, running sealed platform code and foreign"
    );
    eprintln!(
        "ASSERTED: unreadable WindowServer {pid} allowed merged-clean land cleanup and branch deletion"
    );
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
    };
    // A deleted directory elsewhere is unrelated to the root.
    assert_eq!(
        observe(outside.join("gone").join("deeper")),
        WorkerState::Free
    );
    // A deleted subdirectory of the root still holds the process inside it.
    assert_eq!(observe(root.join("gone").join("deeper")), WorkerState::Live);
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
        };
        assert_eq!(observe(&cwds, false), WorkerState::Live);
        assert_eq!(
            observe(&cwds, true),
            if complete {
                WorkerState::Free
            } else {
                WorkerState::Unknown
            }
        );
        // Above-session ancestors, siblings, descendants and independent
        // sessions remain live, even when the census is incomplete.
        for pid in [1, 95, 110, 200] {
            cwds.0.insert(pid, root.clone());
            assert_eq!(observe(&cwds, true), WorkerState::Live, "pid {pid}");
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
        };
        // An ambiguous ancestry excludes no unverified ancestor, so it
        // says nothing about a root no process is inside.
        assert_eq!(observe(&cwds), WorkerState::Free, "{ambiguity}");
        // The session leader is never verified once the walk stops, so
        // inside the root it still makes the root live.
        cwds.0.insert(10, root.clone());
        assert_eq!(observe(&cwds), WorkerState::Live, "{ambiguity}");
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
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
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
    path: PathBuf,
    /// Pids that this observer reports as having an unreadable cwd; others
    /// are reported as an unobservable policy failure.
    unknown_pids: Vec<i64>,
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
        pid: i64,
        _: &[CensusIgnoreEntry],
    ) -> Result<
        provenant_os::pid::UnreadablePolicyExcusal,
        provenant_os::pid::UnreadablePolicyFailure,
    > {
        if self.unknown_pids.contains(&pid) {
            return Err("image policy evidence unavailable".into());
        }
        if !self.uids.all_system() {
            return Err("real/effective/saved system uids below 500 required".into());
        }
        Err(provenant_os::pid::UnreadablePolicyFailure {
            condition: "approved daemon directory required",
            builtin_condition: Some("approved daemon directory required"),
            configurable: true,
        })
    }
    fn process_details(&self, _: i64) -> Option<String> {
        Some(format!("path={:?}, team_id=TEAM123", self.path))
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
) -> Result<WorkerObservations, Box<dyn Error>> {
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
    Ok(observe_worker_roots(
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
fn configurable_census_refusal_names_signed_entry_key_path_and_team() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let root = canonicalize(&fixture.root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let cwd = |uids, path: &str| RefusedCwd {
        uids,
        path: path.into(),
        unknown_pids: vec![],
    };
    let path = "/Library/Developer/CoreSimulator/CoreSimulator.framework/Versions/A/Resources/bin/simdiskimaged";
    let (refused, details) = observe_refused(&root, &[200], &cwd(SYSTEM_UIDS, path))?;
    assert_eq!(refused[&root], WorkerState::Unknown);
    assert!(details[&root].contains("confinement.census_ignore_unreadable"));
    assert!(details[&root].contains(path));
    assert!(details[&root].contains("team_id=TEAM123"));

    let (user_owned, user_details) = observe_refused(
        &root,
        &[200],
        &cwd(
            ProcessUids {
                real: 0,
                effective: 501,
                saved: 0,
            },
            path,
        ),
    )?;
    assert_eq!(user_owned[&root], WorkerState::Unknown);
    assert!(
        !user_details[&root].contains("config_key="),
        "{}",
        user_details[&root]
    );
    Ok(())
}

#[test]
fn occupancy_is_placed_per_root_and_partial_results_remain_conservative()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    struct Occupancy {
        cwd: PathBuf,
        paths: Vec<PathBuf>,
        complete: bool,
    }
    impl CwdObserver for Occupancy {
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
            cwd: Occupancy,
            current: &'a ProcessFact,
        }
        impl WorkerObserver for FixtureObserver<'_> {
            fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
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
            cwd: Occupancy {
                cwd: base.clone(),
                paths,
                complete,
            },
            current: &current,
        };
        let clock = provenant_os::clock::SystemClock;
        CensusSession::new(&observer, &clock).observe_roots(&roots)
    };
    // The resolved ancestor places an unresolvable cwd under a.
    let unplaceable = a.join("cwd-loop");
    std::os::unix::fs::symlink("cwd-loop", &unplaceable)?;
    let states = observe_worker_roots(
        &roots,
        &processes,
        &Occupancy {
            cwd: unplaceable,
            paths: vec![],
            complete: true,
        },
        &current,
        false,
    )
    .0;
    assert_eq!(states[&a], WorkerState::Unknown);
    assert_eq!(states[&b], WorkerState::Free);
    let free = observe(vec![], true);
    assert_eq!(free[&a], WorkerState::Free);
    assert_eq!(free[&b], WorkerState::Free);
    let live = observe(vec![file.clone()], true);
    assert_eq!(live[&a], WorkerState::Live);
    assert_eq!(live[&b], WorkerState::Free);
    let partial = observe(vec![file], false);
    assert_eq!(partial[&a], WorkerState::Live);
    assert_eq!(partial[&b], WorkerState::Unknown);

    // A symlink loop makes canonicalisation fail without relying on permissions.
    let outside = base.join("loop");
    std::os::unix::fs::symlink("loop", &outside)?;
    let outside_states = observe(vec![outside], true);
    assert_eq!(outside_states[&a], WorkerState::Unknown);
    assert_eq!(outside_states[&b], WorkerState::Unknown);
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
        &Occupancy {
            cwd: base.clone(),
            paths: vec![inside.clone()],
            complete: true,
        },
        &current,
        false,
    );
    assert_eq!(alias_states[&alias_root], WorkerState::Unknown);
    assert_eq!(alias_states[&b], WorkerState::Free);
    assert!(alias_details[&alias_root].contains(&inside.display().to_string()));
    {
        let path = inside;
        let (states, details) = observe_worker_roots(
            &roots,
            &processes,
            &Occupancy {
                cwd: base.clone(),
                paths: vec![path.clone()],
                complete: true,
            },
            &current,
            false,
        );
        assert_eq!(states[&a], WorkerState::Unknown);
        assert_eq!(states[&b], WorkerState::Free);
        assert!(details[&a].contains(&path.display().to_string()));
    }
    for path in [PathBuf::new(), PathBuf::from("relative")] {
        let states = observe(vec![path], true);
        assert_eq!(states[&a], WorkerState::Unknown);
        assert_eq!(states[&b], WorkerState::Unknown);
    }
    Ok(())
}

#[test]
fn occupancy_failures_retain_every_root_and_positive_occupants_stay_live()
-> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    use std::cell::Cell;
    struct Occupancy {
        base: PathBuf,
        paths: Vec<PathBuf>,
        complete: bool,
        supported: bool,
    }
    impl CwdObserver for Occupancy {
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
    let occupancy = |paths: Vec<PathBuf>, complete, supported| Occupancy {
        base: base.clone(),
        paths,
        complete,
        supported,
    };
    let states = |observer: &dyn ProcessObserver, cwd: &Occupancy| {
        observe_worker_roots(&roots, observer, cwd, &current, false).0
    };
    // An executable inside one root, with the other root provably unrelated.
    let live = states(&processes, &occupancy(vec![b.join("bin/tool")], true, true));
    assert_eq!(live[&a], WorkerState::Free);
    assert_eq!(live[&b], WorkerState::Live);
    // A reused pid cannot prove a snapshot belongs to the census process.
    let reused = states(
        &Reused(&processes, Cell::new(0)),
        &occupancy(vec![], true, true),
    );
    assert_eq!(reused[&a], WorkerState::Unknown);
    assert_eq!(reused[&b], WorkerState::Unknown);
    // An unsupported query (default observation) is not an empty one.
    let unsupported = states(&processes, &occupancy(vec![], true, false));
    assert_eq!(unsupported[&a], WorkerState::Unknown);
    assert_eq!(unsupported[&b], WorkerState::Unknown);
    // A positive occupant stays live beside incompleteness elsewhere.
    let partial = states(&processes, &occupancy(vec![a.join("x")], false, true));
    assert_eq!(partial[&a], WorkerState::Live);
    assert_eq!(partial[&b], WorkerState::Unknown);
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
                states[&registered],
                if inside {
                    WorkerState::Live
                } else {
                    WorkerState::Free
                },
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
    assert_eq!(
        observer.observe_roots(std::slice::from_ref(&root))[&root],
        WorkerState::Unknown
    );
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
        assert!(
            matches!(observe_same_process(&processes, &observer, &fact(200)), NativeCwd::Unknown(reason) if reason == "identity race")
        );
    }
    let mut observer = observer;
    observer
        .cwds
        .insert(300, CwdObservation::Path(root.clone()));
    assert_eq!(
        observer.observe_roots(std::slice::from_ref(&root))[&root],
        WorkerState::Live
    );
    // Owner/identity lookup failure never licenses exclusion or placement.
    let missing = root.join("missing-root");
    assert_eq!(
        observer.observe_roots(std::slice::from_ref(&missing))[&missing],
        WorkerState::Unknown
    );
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
    assert_eq!(states[&root], WorkerState::Unknown);
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
    assert_eq!(states[&root], WorkerState::Unknown);
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
    assert_eq!(states[&root], WorkerState::Unknown);
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
        assert_eq!(states[&root], WorkerState::Unknown);
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
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Live))
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
                                WorkerState::Free
                            } else {
                                WorkerState::Live
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
    assert_eq!(
        ordinary.observe_roots(std::slice::from_ref(&root))[&root],
        WorkerState::Live
    );
    assert_eq!(ordinary.live_process(&root).unwrap().pid, 90);
    assert_eq!(
        run.observe_roots(std::slice::from_ref(&root))[&root],
        WorkerState::Free
    );
    assert!(run.live_process(&root).is_none());
    assert_eq!(
        ordinary.observe_roots_for_land(std::slice::from_ref(&root))[&root],
        WorkerState::Free
    );
    Ok(())
}

#[test]
fn host_policy_is_scoped_to_config_roots_and_preserves_injected_refusal()
-> Result<(), Box<dyn Error>> {
    struct Probe(std::sync::Mutex<Vec<Vec<CensusIgnoreEntry>>>);
    impl WorkerObserver for Probe {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Unknown))
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
    fs::create_dir(&state)?;
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
    assert_eq!(
        first.observe_roots_for_land(std::slice::from_ref(&root))[&root],
        WorkerState::Unknown
    );
    let second = CensusPolicyWorkers::from_roots(&first, &empty_roots)?;
    assert_eq!(
        second.observe_roots_for_land(std::slice::from_ref(&root))[&root],
        WorkerState::Unknown
    );
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
    for (name, uid, denied, expected) in [
        (
            "accepted foreign listing",
            if owner == 88 { 89 } else { 88 },
            true,
            WorkerState::Free,
        ),
        ("owner uid refused", owner, true, WorkerState::Unknown),
        ("human credentials refused", 500, true, WorkerState::Unknown),
        (
            "non-EPERM refused",
            owner.wrapping_add(1),
            false,
            WorkerState::Unknown,
        ),
    ] {
        assert_eq!(
            observe_worker_roots(
                std::slice::from_ref(&root),
                &observer,
                &Listed { uid, denied },
                &observer.current,
                false
            )
            .0[&root],
            expected,
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn denied_daemon_policy_distinguishes_system_owner_human_and_missing_evidence()
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
    for (name, evidence, expected) in [
        (
            "root daemon",
            Some(0),
            if owner == 0 {
                WorkerState::Unknown
            } else {
                WorkerState::Free
            },
        ),
        (
            "foreign service daemon",
            Some(if owner == 88 { 89 } else { 88 }),
            WorkerState::Free,
        ),
        (
            "last system account",
            Some(499),
            if owner == 499 {
                WorkerState::Unknown
            } else {
                WorkerState::Free
            },
        ),
        ("first human account", Some(500), WorkerState::Unknown),
        ("worktree owner", Some(owner), WorkerState::Unknown),
        (
            "missing image/signing/uid evidence",
            None,
            WorkerState::Unknown,
        ),
    ] {
        observer.platform_uids = evidence
            .map(|uid| BTreeMap::from([(200, test_uids(uid))]))
            .unwrap_or_default();
        assert_eq!(
            observer.observe_roots(std::slice::from_ref(&root))[&root],
            expected,
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
        assert_eq!(
            observer.observe_roots(std::slice::from_ref(&root))[&root],
            WorkerState::Unknown,
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
    assert_eq!(
        observer.observe_roots(std::slice::from_ref(&root))[&root],
        if [0, 88].contains(&owner) {
            WorkerState::Unknown
        } else {
            WorkerState::Free
        },
        "WindowServer real and saved uids of zero qualify for a foreign owner"
    );
    let roots = [root.clone(), PathBuf::from("/")];
    let states = observer.observe_roots(&roots);
    assert!(
        states.values().all(|state| *state == WorkerState::Unknown),
        "a credential matching any candidate root's owner refuses exclusion for all roots"
    );
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
    assert_eq!(
        observer.observe_roots(std::slice::from_ref(&root))[&root],
        WorkerState::Unknown,
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
fn unreadable_occupant_cwd_keeps_the_tree_for_removal() -> Result<(), Box<dyn Error>> {
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
    let created = create(
        &test_context.context(),
        WorktreeCreate {
            branch: "landed".to_owned(),
            base: None,
        },
        &cancellation,
    )?;
    // The occupant is same-uid, so its unreadable cwd cannot be excluded.
    fs::set_permissions(&created.path, fs::Permissions::from_mode(0o755))?;
    let refused = remove(
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
    );
    assert!(
        matches!(&refused, Err(WorktreeError::IncompleteCensus { detail }) if detail == "process 200: cwd unreadable; policy_condition=image policy evidence unavailable"),
        "{refused:?}"
    );
    assert!(created.path.exists());
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
        remove(
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
    assert_eq!(
        unplaced.observe_roots(std::slice::from_ref(&created.path))[&created.path],
        WorkerState::Unknown
    );
    assert!(matches!(
        remove_named(&unplaced, "landed"),
        Err(WorktreeError::IncompleteCensus { ref detail }) if detail == "process 400: Io"
    ));
    // A readable process inside the tree still keeps it.
    let live = observer(ObservationIssue::PermissionDenied, Some(&created.path))?;
    assert!(matches!(
        remove_named(&live, "landed"),
        Err(WorktreeError::LiveWriter)
    ));
    assert!(created.path.exists());

    // Foreign credentials and unwritable root permissions cannot prove
    // where an unreadable process has its cwd.
    let foreign = observer(ObservationIssue::PermissionDenied, None)?;
    assert_eq!(
        foreign.observe_roots(std::slice::from_ref(&created.path))[&created.path],
        WorkerState::Unknown
    );
    assert!(matches!(
        remove_named(&foreign, "landed"),
        Err(WorktreeError::IncompleteCensus { ref detail }) if detail == "process 400: PermissionDenied"
    ));
    assert!(created.path.exists());

    // A complete, identity-checked cwd observation outside the tree
    // provides the positive non-occupancy evidence credentials cannot.
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
            .any(|entry| entry.path == created.path && entry.writer == WriterState::Free)
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
        (outer.clone(), WorkerState::Free),
        (nested.clone(), WorkerState::Free),
        (sibling.clone(), WorkerState::Free),
    ]);
    record_live_path(&mut states, &nested.join("src"));
    assert_eq!(states[&outer], WorkerState::Live);
    assert_eq!(states[&nested], WorkerState::Live);
    assert_eq!(states[&sibling], WorkerState::Free);
    Ok(())
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

    type Observed = (
        WorkerState,
        String,
        Vec<(Option<i64>, Option<&'static str>)>,
    );

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
        let (states, _, details) = observe_worker_holders_with_audit(
            std::slice::from_ref(&root),
            &processes,
            &scripted,
            &current,
            false,
            &mut |pid, condition| audit.push((pid, condition)),
        );
        Ok((
            states[&root],
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
    ) -> Result<WorkerState, Box<dyn Error>> {
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
        Ok(states[&root])
    }

    #[test]
    fn a_process_that_exits_during_occupancy_inspection_frees_its_root()
    -> Result<(), Box<dyn Error>> {
        assert_eq!(
            exits_during_occupancy(|_| ProcessObservation::Absent)?,
            WorkerState::Free
        );
        assert_eq!(
            exits_during_occupancy(|fact| ProcessObservation::Present(ProcessFact {
                zombie: true,
                ..fact
            }))?,
            WorkerState::Free
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
            WorkerState::Free
        );
        Ok(())
    }

    #[test]
    fn a_sealed_system_image_with_a_wholly_denied_descriptor_listing_is_excused_by_design()
    -> Result<(), Box<dyn Error>> {
        let (state, _, audit) = observe(
            path_scripted(gap(OccupancyGap::DescriptorsDenied), SYSTEM),
            false,
        )?;
        assert_eq!(state, WorkerState::Free);
        assert!(audit.is_empty());
        Ok(())
    }

    #[test]
    fn refused_descriptors_on_a_non_system_image_stay_unknown_with_pid_and_reason()
    -> Result<(), Box<dyn Error>> {
        let (state, detail, audit) = observe(
            path_scripted(gap(OccupancyGap::DescriptorsDenied), None),
            false,
        )?;
        assert_eq!(state, WorkerState::Unknown);
        assert!(
            detail.starts_with("process 200: occupancy incomplete: descriptors unreadable (EPERM)")
        );
        // An incomplete occupancy is not an image-policy rejection.
        assert!(audit.is_empty());
        Ok(())
    }

    #[test]
    fn other_gaps_are_never_excused_even_for_a_sealed_system_image() -> Result<(), Box<dyn Error>> {
        for gap_kind in [
            OccupancyGap::ExecutableUnavailable,
            OccupancyGap::DescriptorsUnavailable,
            OccupancyGap::LimitExceeded,
            OccupancyGap::Unobserved,
        ] {
            let (state, detail, _) = observe(path_scripted(gap(gap_kind), SYSTEM), false)?;
            assert_eq!(state, WorkerState::Unknown, "{gap_kind:?}");
            assert!(
                detail.contains(&format!("occupancy incomplete: {}", gap_kind.describe())),
                "{detail}"
            );
        }
        Ok(())
    }

    #[test]
    fn refused_descriptors_with_an_unreadable_cwd_are_not_excused() -> Result<(), Box<dyn Error>> {
        // The cwd is unknown, so the outside-cwd condition is unmet.
        let (state, _, _) = observe(
            Scripted {
                cwd: CwdObservation::Unsupported,
                occupancy: gap(OccupancyGap::DescriptorsDenied),
                sealed: SYSTEM,
                kernel_thread: false,
            },
            false,
        )?;
        assert_eq!(state, WorkerState::Unknown);
        Ok(())
    }

    #[test]
    fn a_setuid_sealed_image_with_a_refused_cwd_is_non_occupying() -> Result<(), Box<dyn Error>> {
        // /usr/bin/login: ruid=501, euid=0, cwd refused by the transition.
        let (state, _, audit) = observe(
            Scripted {
                cwd: CwdObservation::PermissionDenied,
                occupancy: gap(OccupancyGap::DescriptorsDenied),
                sealed: SETUID,
                kernel_thread: false,
            },
            false,
        )?;
        assert_eq!(state, WorkerState::Free);
        assert!(audit.is_empty());
        Ok(())
    }

    #[test]
    fn a_setuid_sealed_image_still_reports_positively_observed_paths_and_other_gaps()
    -> Result<(), Box<dyn Error>> {
        let (state, detail, _) = observe(
            Scripted {
                cwd: CwdObservation::PermissionDenied,
                occupancy: gap(OccupancyGap::DescriptorsUnavailable),
                sealed: SETUID,
                kernel_thread: false,
            },
            false,
        )?;
        assert_eq!(state, WorkerState::Unknown);
        assert!(detail.contains("descriptors unavailable"), "{detail}");
        Ok(())
    }

    #[test]
    fn a_refused_cwd_on_a_non_setuid_or_non_system_image_stays_unknown()
    -> Result<(), Box<dyn Error>> {
        for sealed in [None, SYSTEM] {
            let (state, detail, _) = observe(
                Scripted {
                    cwd: CwdObservation::PermissionDenied,
                    occupancy: OccupancyObservation {
                        paths: vec![],
                        complete: true,
                        gap: None,
                    },
                    sealed,
                    kernel_thread: false,
                },
                false,
            )?;
            assert_eq!(state, WorkerState::Unknown, "{sealed:?}");
            assert!(
                detail.starts_with("process 200: cwd unreadable"),
                "{detail}"
            );
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
        assert_eq!(state, WorkerState::Free);
        let (state, _, _) = observe(
            Scripted {
                cwd: CwdObservation::Gone,
                occupancy: gap(OccupancyGap::ExecutableUnavailable),
                sealed: None,
                kernel_thread: true,
            },
            false,
        )?;
        assert_eq!(state, WorkerState::Free);
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
        assert_eq!(state, WorkerState::Unknown);
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
        assert_eq!(state, WorkerState::Unknown);
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
        assert_eq!(states_with_ancestors[&root], WorkerState::Free);

        // Without ancestor exclusion, parent shell inside root is detected: Live.
        let (states_no_ancestors, holders_no_ancestors, _) = observe_worker_holders(
            std::slice::from_ref(&root),
            &processes,
            &cwds,
            &current,
            false,
        );
        assert_eq!(states_no_ancestors[&root], WorkerState::Live);
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
        assert_eq!(states_foreign[&root], WorkerState::Live);
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
        (affected.clone(), WorkerState::Free),
        (unrelated.clone(), WorkerState::Free),
    ]);
    let mut reported = false;
    record_live_path_with_audit(&mut states, &non_directory.join("cwd"), &mut || {
        reported = true;
    });
    assert!(reported);
    assert_eq!(states[&affected], WorkerState::Unknown);
    assert_eq!(states[&unrelated], WorkerState::Free);
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
        (this_root.clone(), WorkerState::Free),
        (other_root.clone(), WorkerState::Free),
    ]);
    let mut unplaceable_reported = false;
    let placed = record_live_path_with_audit(&mut states, &inside_other, &mut || {
        unplaceable_reported = true;
    });
    assert!(!unplaceable_reported);
    assert_eq!(placed, vec![other_root.clone()]);
    assert_eq!(states[&this_root], WorkerState::Free);
    assert_eq!(states[&other_root], WorkerState::Live);
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
        (root_a.clone(), WorkerState::Free),
        (root_b.clone(), WorkerState::Free),
    ]);
    let mut reported = false;
    let placed = record_live_path_with_audit(&mut states, Path::new("relative/path"), &mut || {
        reported = true;
    });
    assert!(reported);
    assert!(placed.is_empty());
    assert_eq!(states[&root_a], WorkerState::Unknown);
    assert_eq!(states[&root_b], WorkerState::Unknown);

    // Empty path is similarly wholly unplaceable and fails closed.
    let mut states_empty = BTreeMap::from([
        (root_a.clone(), WorkerState::Free),
        (root_b.clone(), WorkerState::Free),
    ]);
    let mut reported_empty = false;
    let placed_empty = record_live_path_with_audit(&mut states_empty, Path::new(""), &mut || {
        reported_empty = true;
    });
    assert!(reported_empty);
    assert!(placed_empty.is_empty());
    assert_eq!(states_empty[&root_a], WorkerState::Unknown);
    assert_eq!(states_empty[&root_b], WorkerState::Unknown);
    Ok(())
}

#[test]
fn taint_causes_distinguish_contention_from_uncertainty() {
    let live = TaintCause::LiveWriter(ObservedProcess::cwd(1234, Some("worker".into())));
    assert!(!live.is_contention());
    assert!(!live.is_uncertainty());

    let lock = TaintCause::LockContention;
    assert!(lock.is_contention());
    assert!(!lock.is_uncertainty());

    let uncertainty_reasons = [
        UncertaintyReason::CensusUnavailable,
        UncertaintyReason::UnstableProcessIdentity,
        UncertaintyReason::CwdUnreadable,
        UncertaintyReason::OccupancyIncomplete,
        UncertaintyReason::PathUnplaceable,
        UncertaintyReason::ListingIncomplete,
        UncertaintyReason::PolicyRejected,
        UncertaintyReason::RootUnreadable,
    ];
    for reason in uncertainty_reasons {
        let cause = TaintCause::Uncertainty(reason);
        assert!(!cause.is_contention());
        assert!(cause.is_uncertainty());
    }
}

#[test]
fn live_observer_without_process_details_preserves_unattributed_writer() {
    struct LiveObserver;
    impl WorkerObserver for LiveObserver {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Live))
                .collect()
        }
    }

    let root = PathBuf::from("/worktree");
    let (states, taints) = LiveObserver.observe_roots_with_taints(std::slice::from_ref(&root));
    assert_eq!(states[&root], WorkerState::Live);
    assert_eq!(taints[&root], TaintCause::UnattributedLiveWriter);
    assert!(!taints[&root].is_contention());
    assert_eq!(taints[&root].to_string(), "live writer");
}

#[test]
fn census_observation_produces_typed_taints_for_contention_and_uncertainty()
-> Result<(), Box<dyn Error>> {
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

    assert_eq!(states[&live_root], WorkerState::Live);
    assert_eq!(states[&unknown_root], WorkerState::Unknown);
    assert_eq!(states[&free_root], WorkerState::Free);

    let live_taint = &taints[&live_root];
    assert!(!live_taint.is_contention());
    assert!(!live_taint.is_uncertainty());
    assert!(matches!(live_taint, TaintCause::LiveWriter(p) if p.pid == 200));

    let unknown_taint = &taints[&unknown_root];
    assert!(!unknown_taint.is_contention());
    assert!(unknown_taint.is_uncertainty());
    assert!(matches!(unknown_taint, TaintCause::Uncertainty(_)));

    assert!(!taints.contains_key(&free_root));

    Ok(())
}

#[test]
fn census_refusal_names_working_directory_evidence_kind() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fixture = Fixture::new()?;
    let root = canonicalize(fixture._temp.path())?.join("root-wd");
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
        command: Some("agent-shell".into()),
    };
    let current = fact(100);
    let worker = fact(200);

    let processes = InjectedCensus {
        census: Census::from_observations(
            [current.clone(), worker.clone()],
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

    struct WdCwdObserver(PathBuf);
    impl CwdObserver for WdCwdObserver {
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            if pid == 200 {
                Ok(CwdObservation::Path(self.0.clone()))
            } else {
                Ok(CwdObservation::Gone)
            }
        }
    }

    struct MockObserver<'a> {
        processes: &'a InjectedCensus,
        cwd_obs: WdCwdObserver,
    }
    impl WorkerObserver for MockObserver<'_> {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            observe_worker_roots(
                roots,
                self.processes,
                &self.cwd_obs,
                &self.processes.current,
                false,
            )
            .0
        }
        fn live_process(&self, root: &Path) -> Option<ObservedProcess> {
            let (_, holders, _) = observe_worker_holders(
                std::slice::from_ref(&root.to_path_buf()),
                self.processes,
                &self.cwd_obs,
                &self.processes.current,
                false,
            );
            holders.get(root).cloned()
        }
    }

    let observer = MockObserver {
        processes: &processes,
        cwd_obs: WdCwdObserver(root.clone()),
    };

    let refusal = live_refusal(&observer, &root);
    let WorktreeError::LiveProcess(observed) = &refusal else {
        panic!("expected LiveProcess, got {refusal:?}");
    };
    assert_eq!(observed.pid, 200);
    assert_eq!(observed.kind, ProcessEvidenceKind::WorkingDirectory);
    let message = refusal.to_string();
    assert!(
        message.contains("has its working directory inside the tree"),
        "expected working directory refusal message, got: {message}"
    );
    Ok(())
}

#[test]
fn census_refusal_names_executable_image_evidence_kind() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fixture = Fixture::new()?;
    let base = canonicalize(fixture._temp.path())?;
    let root = base.join("root-exe");
    let external_cwd = base.join("tmp-cwd");
    fs::create_dir(&root)?;
    fs::create_dir(&external_cwd)?;

    // Create an executable image file inside the root checkout
    let exe_path = root.join("tools/front-door/target/debug/provenant");
    fs::create_dir_all(exe_path.parent().unwrap())?;
    fs::write(&exe_path, b"ELF or Mach-O binary content")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755))?;
    }

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
        command: Some("provenant-supervise".into()),
    };
    let current = fact(100);
    let worker = fact(200);

    let processes = InjectedCensus {
        census: Census::from_observations(
            [current.clone(), worker.clone()],
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

    struct ExeCwdObserver {
        cwd: PathBuf,
        exe: PathBuf,
        held: PathBuf,
    }
    impl CwdObserver for ExeCwdObserver {
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            if pid == 200 {
                // Cwd is outside the tree (e.g. temp dir)
                Ok(CwdObservation::Path(self.cwd.clone()))
            } else {
                Ok(CwdObservation::Gone)
            }
        }
        fn occupancy(&self, pid: i64) -> provenant_os::pid::OccupancyObservation {
            if pid == 200 {
                provenant_os::pid::OccupancyObservation {
                    paths: vec![self.held.clone()],
                    complete: true,
                    gap: None,
                }
            } else {
                provenant_os::pid::OccupancyObservation::default()
            }
        }
        fn executable(&self, pid: i64) -> Option<PathBuf> {
            if pid == 200 {
                Some(self.exe.clone())
            } else {
                None
            }
        }
    }

    struct MockObserver<'a> {
        processes: &'a InjectedCensus,
        cwd_obs: ExeCwdObserver,
    }
    impl WorkerObserver for MockObserver<'_> {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            observe_worker_roots(
                roots,
                self.processes,
                &self.cwd_obs,
                &self.processes.current,
                false,
            )
            .0
        }
        fn live_process(&self, root: &Path) -> Option<ObservedProcess> {
            let (_, holders, _) = observe_worker_holders(
                std::slice::from_ref(&root.to_path_buf()),
                self.processes,
                &self.cwd_obs,
                &self.processes.current,
                false,
            );
            holders.get(root).cloned()
        }
    }

    #[cfg(unix)]
    let executable_alias = {
        let alias = base.join("executable-alias");
        std::os::unix::fs::symlink(&exe_path, &alias)?;
        alias
    };
    #[cfg(not(unix))]
    let executable_alias = exe_path.clone();
    let observer = MockObserver {
        processes: &processes,
        cwd_obs: ExeCwdObserver {
            cwd: external_cwd,
            exe: executable_alias,
            held: exe_path,
        },
    };

    let refusal = live_refusal(&observer, &root);
    let WorktreeError::LiveProcess(observed) = &refusal else {
        panic!("expected LiveProcess, got {refusal:?}");
    };
    assert_eq!(observed.pid, 200);
    assert_eq!(observed.kind, ProcessEvidenceKind::ExecutableImage);
    let message = refusal.to_string();
    assert!(
        message.contains("has its executable image inside the tree"),
        "expected executable image refusal message, got: {message}"
    );
    Ok(())
}

#[test]
fn census_refusal_names_other_held_path_evidence_kind() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, ProcessIdentity};
    let fixture = Fixture::new()?;
    let base = canonicalize(fixture._temp.path())?;
    let root = base.join("root-other");
    let external_cwd = base.join("tmp-cwd");
    fs::create_dir(&root)?;
    fs::create_dir(&external_cwd)?;

    // An executable-mode file held open is not necessarily the running image.
    let log_path = root.join("logs/output.log");
    fs::create_dir_all(log_path.parent().unwrap())?;
    fs::write(&log_path, b"log stream content")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&log_path, fs::Permissions::from_mode(0o755))?;
    }

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
        command: Some("log-writer".into()),
    };
    let current = fact(100);
    let worker = fact(200);

    let processes = InjectedCensus {
        census: Census::from_observations(
            [current.clone(), worker.clone()],
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

    struct OtherHeldPathCwdObserver {
        cwd: PathBuf,
        held_file: PathBuf,
    }
    impl CwdObserver for OtherHeldPathCwdObserver {
        fn cwd(&self, pid: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            if pid == 200 {
                Ok(CwdObservation::Path(self.cwd.clone()))
            } else {
                Ok(CwdObservation::Gone)
            }
        }
        fn executable(&self, pid: i64) -> Option<PathBuf> {
            (pid == 200).then(|| self.cwd.join("different-image"))
        }
        fn occupancy(&self, pid: i64) -> provenant_os::pid::OccupancyObservation {
            if pid == 200 {
                provenant_os::pid::OccupancyObservation {
                    paths: vec![self.held_file.clone()],
                    complete: true,
                    gap: None,
                }
            } else {
                provenant_os::pid::OccupancyObservation::default()
            }
        }
    }

    struct MockObserver<'a> {
        processes: &'a InjectedCensus,
        cwd_obs: OtherHeldPathCwdObserver,
    }
    impl WorkerObserver for MockObserver<'_> {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, WorkerState> {
            observe_worker_roots(
                roots,
                self.processes,
                &self.cwd_obs,
                &self.processes.current,
                false,
            )
            .0
        }
        fn live_process(&self, root: &Path) -> Option<ObservedProcess> {
            let (_, holders, _) = observe_worker_holders(
                std::slice::from_ref(&root.to_path_buf()),
                self.processes,
                &self.cwd_obs,
                &self.processes.current,
                false,
            );
            holders.get(root).cloned()
        }
    }

    let observer = MockObserver {
        processes: &processes,
        cwd_obs: OtherHeldPathCwdObserver {
            cwd: external_cwd,
            held_file: log_path,
        },
    };

    let refusal = live_refusal(&observer, &root);
    let WorktreeError::LiveProcess(observed) = &refusal else {
        panic!("expected LiveProcess, got {refusal:?}");
    };
    assert_eq!(observed.pid, 200);
    assert_eq!(observed.kind, ProcessEvidenceKind::OtherHeldPath);
    let message = refusal.to_string();
    assert!(
        message.contains("has its other held path inside the tree"),
        "expected other held path refusal message, got: {message}"
    );
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
    let (_, holders, _, taints) = observe_worker_holders_with_taints_and_audit(
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
    assert_eq!(holder.pid, 200);
    assert_eq!(holder.kind, ProcessEvidenceKind::OtherHeldPath);
    assert!(
        matches!(&taints[&root], TaintCause::LiveWriter(p) if p.pid == 200 && p.kind == ProcessEvidenceKind::OtherHeldPath)
    );
    Ok(())
}
