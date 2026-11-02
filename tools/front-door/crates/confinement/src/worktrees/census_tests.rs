//! Worker census and native observation tests.
use super::lifecycle_tests::*;
use super::*;
use crate::test_support::Fixture;
use provenant_supervisor::census::ObservationIssue;
use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;

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
    let complete = provenant_config::read(&host_roots)?.census_host_list_complete()?;
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
    assert_eq!(
        SystemCwdObserver.sealed_platform_uids(pid),
        None,
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
            "SKIP: clean-Free assertion: confinement.census_host_list_complete=false; host exceptions are not declared complete; land cleanup may report the first unlisted daemon: {first}"
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
        if std::env::var("PROVENANT_CONFINEMENT_REQUIRED").as_deref() == Ok("1") {
            Err(reason.into())
        } else {
            eprintln!("SKIP: {reason}");
            Ok(())
        }
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
            NativeCwd::PolicyExcluded(uids) if uids.all_system() && uids.all_differ_from(provenant_os::fs::effective_uid())
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
    fn sealed_platform_uids(&self, pid: i64) -> Option<ProcessUids> {
        self.platform_uids.get(&pid).copied()
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
        observe_worker_roots(
            &roots,
            &processes,
            &Occupancy {
                cwd: base.clone(),
                paths,
                complete,
            },
            &current,
            false,
        )
        .0
    };
    let free = observe(vec![], true);
    assert_eq!(free[&a], WorkerState::Free);
    assert_eq!(free[&b], WorkerState::Free);
    let live = observe(vec![file.clone()], true);
    assert_eq!(live[&a], WorkerState::Live);
    assert_eq!(live[&b], WorkerState::Free);
    let partial = observe(vec![file], false);
    assert_eq!(partial[&a], WorkerState::Live);
    assert_eq!(partial[&b], WorkerState::Unknown);
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
        ) -> Result<ProcessUids, provenant_os::pid::UnreadablePolicyFailure> {
            Err(provenant_os::pid::UnreadablePolicyFailure {
                condition: "stable signing team required",
                builtin_condition: None,
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
    assert_eq!(details[&root], "process listing truncated or incomplete");
    assert_eq!(
        audit,
        vec![(None, Some("complete process listing required"))]
    );
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
        fn ignored_unreadable_uids(&self, _: i64, _: &[CensusIgnoreEntry]) -> Option<ProcessUids> {
            assert!(self.denied, "non-EPERM must never consult listing policy");
            Some(test_uids(self.uid))
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
        fn sealed_platform_uids(&self, _: i64) -> Option<ProcessUids> {
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
