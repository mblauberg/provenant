//! Doctor diagnoses the same custody boundary as admission without repairing it.
use super::lifecycle_tests::TestContext;
use super::*;
use crate::test_support::Fixture;
use std::{error::Error, fs};

#[test]
fn doctor_reports_five_worktree_states_and_one_repair_without_mutation()
-> Result<(), Box<dyn Error>> {
    for scenario in ["dirty", "stale", "held", "unregistered", "detached"] {
        let fixture = Fixture::new()?;
        let test = TestContext::new(&fixture)?;
        let context = test.context();
        let cancel = CancelToken::new();
        let path = fixture.root.join(format!("doctor-{scenario}"));
        if scenario != "unregistered" {
            fixture.run(&["worktree", "add", "--detach", path.to_str().ok_or("path")?])?;
            if scenario != "detached" {
                fixture.run(&[
                    "-C",
                    path.to_str().ok_or("path")?,
                    "switch",
                    "-c",
                    "doctor-fixture",
                ])?;
            }
        }
        let mut lease = None;
        let holder = AttemptId::from_bytes([42; 16]);
        match scenario {
            "dirty" => fs::write(path.join("uncommitted"), "preserve this work")?,
            "stale" => fs::remove_dir_all(&path)?,
            "held" => {
                let slot = writer_directory(context.project_state, &path)?;
                lease = Some(provenant_supervisor::fixture::acquire_writer(
                    slot,
                    provenant_supervisor::custody::Scope {
                        attempt: holder,
                        generation: 1,
                    },
                    WaitBudget::nonblocking(context.clock.mono_now()),
                    &cancel,
                    context.clock,
                )?);
            }
            _ => {}
        }
        let before = context.facts.writer_inventory(
            context.repo,
            context.executor,
            context.utility,
            context.utility_budget,
            &cancel,
        )?;
        let report = doctor(&context, &path, &cancel)?;
        let (expected, repair_suffix) = match scenario {
            "dirty" => (
                WorktreeDiagnosisState::Dirty,
                "stash push --include-untracked",
            ),
            "stale" => (WorktreeDiagnosisState::StaleRegistration, "worktree prune"),
            "held" => (
                WorktreeDiagnosisState::LiveWriter,
                "provenant status --wait",
            ),
            "unregistered" => (
                WorktreeDiagnosisState::Unregistered,
                "provenant worktree create BRANCH",
            ),
            "detached" => (WorktreeDiagnosisState::Detached, "switch -c BRANCH"),
            _ => unreachable!(),
        };
        assert_eq!(report.state, expected, "{scenario}: {report:?}");
        assert!(!report.reason.is_empty());
        assert!(report.repair.ends_with(repair_suffix), "{report:?}");
        assert!(!report.repair.contains('\n'), "one repair command");
        assert_eq!(
            context.facts.writer_inventory(
                context.repo,
                context.executor,
                context.utility,
                context.utility_budget,
                &cancel,
            )?,
            before,
            "doctor must not prune, attach, clean or register the worktree"
        );
        match scenario {
            "dirty" => assert_eq!(
                fs::read_to_string(path.join("uncommitted"))?,
                "preserve this work"
            ),
            "held" => {
                assert_eq!(report.holder, Some(holder));
                assert!(matches!(
                    cache_writer_probe(context.project_state, &path),
                    Err(Occupancy::Held)
                ));
            }
            "stale" | "unregistered" => assert!(!path.exists()),
            _ => {}
        }
        drop(lease);
    }
    Ok(())
}

#[test]
fn doctor_unknown_preserves_census_and_custody_scope_and_evidence() -> Result<(), Box<dyn Error>> {
    struct Incomplete;
    impl WorkerObserver for Incomplete {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .map(|root| {
                    (
                        root.clone(),
                        Occupancy::census_unknown(
                            root.clone(),
                            "fixture descriptors unreadable for pid 24",
                        ),
                    )
                })
                .collect()
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = WorktreeContext {
        workers: &Incomplete,
        ..test.context()
    };
    let cancel = CancelToken::new();
    let path = fixture.root.join("doctor-unknown");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "doctor-unknown",
        path.to_str().ok_or("path")?,
    ])?;
    let census = doctor(&context, &path, &cancel)?;
    assert_eq!(census.state, WorktreeDiagnosisState::Unknown);
    assert_eq!(census.source, Some("census"));
    assert_eq!(
        serde_json::to_value(&census.scope)?,
        serde_json::to_value(StructuredPathWire::from_path(&path)?)?
    );
    assert_eq!(
        census.evidence.as_deref(),
        Some("fixture descriptors unreadable for pid 24")
    );
    assert!(census.repair.starts_with("provenant worktree doctor "));

    struct CensusHeld;
    impl WorkerObserver for CensusHeld {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Held))
                .collect()
        }
    }
    let census_held = doctor(
        &WorktreeContext {
            workers: &CensusHeld,
            ..test.context()
        },
        &path,
        &cancel,
    )?;
    assert_eq!(census_held.state, WorktreeDiagnosisState::Unknown);
    assert_eq!(census_held.source, Some("census"));
    assert_eq!(census_held.holder, None);
    assert_eq!(
        serde_json::to_value(&census_held.scope)?,
        serde_json::to_value(StructuredPathWire::from_path(&path)?)?
    );
    assert!(
        census_held
            .evidence
            .as_deref()
            .is_some_and(|evidence| evidence.contains("without a held writer lease"))
    );

    let verified = verify_worktree(context.repo, &path)?;
    let owner_path = verified
        .repo()
        .private_git_dir()
        .join("provenant-owner.json");
    fs::write(&owner_path, "malformed owner record")?;
    let malformed = doctor(&test.context(), &path, &cancel)?;
    assert_eq!(malformed.state, WorktreeDiagnosisState::Unknown);
    assert_eq!(malformed.source, Some("custody"));
    assert_eq!(
        serde_json::to_value(&malformed.scope)?,
        serde_json::to_value(StructuredPathWire::from_path(&path)?)?
    );
    assert!(
        malformed
            .evidence
            .as_deref()
            .is_some_and(|evidence| !evidence.is_empty())
    );
    assert!(malformed.repair.starts_with("provenant worktree doctor "));
    assert_eq!(fs::read_to_string(&owner_path)?, "malformed owner record");
    fs::remove_file(owner_path)?;

    // Custody uncertainty takes priority over an incomplete advisory census.
    let slot = writer_directory(context.project_state, &path)?;
    let scope = provenant_supervisor::custody::Scope {
        attempt: AttemptId::from_bytes([52; 16]),
        generation: 1,
    };
    let lease = provenant_supervisor::fixture::acquire_writer(
        slot.try_clone()?,
        scope,
        WaitBudget::nonblocking(context.clock.mono_now()),
        &cancel,
        context.clock,
    )?;
    let retained = slot.current_path()?.join(scope.attempt.to_string());
    fs::write(retained.join("import-retained.json"), "retained")?;
    let reservation = fs::read(retained.join("reservation.json"))?;
    drop(lease);
    let custody = doctor(&context, &path, &cancel)?;
    assert_eq!(custody.state, WorktreeDiagnosisState::Unknown);
    assert_eq!(custody.source, Some("custody"));
    assert_eq!(
        serde_json::to_value(&custody.scope)?,
        serde_json::to_value(StructuredPathWire::from_path(&path)?)?
    );
    assert!(
        custody
            .evidence
            .as_deref()
            .is_some_and(|evidence| evidence.contains("retained import fence"))
    );
    assert!(custody.repair.starts_with("provenant worktree doctor "));
    assert_eq!(fs::read(retained.join("reservation.json"))?, reservation);
    assert_eq!(
        fs::read(retained.join("import-retained.json"))?,
        b"retained"
    );
    Ok(())
}

#[test]
fn doctor_reports_an_observed_bystander_in_a_free_worktree() -> Result<(), Box<dyn Error>> {
    struct Observed;
    impl WorkerObserver for Observed {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Observed { process: None }))
                .collect()
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = WorktreeContext {
        workers: &Observed,
        ..test.context()
    };
    let path = fixture.root.join("observed");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "observed",
        path.to_str().ok_or("path")?,
    ])?;
    let report = doctor(&context, &path, &CancelToken::new())?;
    assert_eq!(report.state, WorktreeDiagnosisState::Observed);
    assert_eq!(report.holder, None);
    assert!(report.repair.starts_with("provenant worktree doctor "));
    Ok(())
}

#[test]
fn a_read_only_probe_cannot_make_concurrent_admission_writer_busy() -> Result<(), Box<dyn Error>> {
    use std::sync::{Arc, Barrier, mpsc};
    use std::time::Duration;
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture.root.join("probe-race");
    let slot = writer_directory(context.project_state, &path)?;
    drop(slot.try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?);
    let probe = provenant_supervisor::custody::writer_observation_guard(&slot)?;
    let raw_probe = slot.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?;
    let barrier = Arc::new(Barrier::new(2));
    let entered = barrier.clone();
    let writer_slot = slot.try_clone()?;
    let (send, receive) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let clock = provenant_os::clock::SystemClock::new();
        entered.wait();
        let result = provenant_supervisor::fixture::acquire_writer(
            writer_slot,
            provenant_supervisor::custody::Scope {
                attempt: AttemptId::from_bytes([92; 16]),
                generation: 1,
            },
            WaitBudget::nonblocking(clock.mono_now()),
            &CancelToken::new(),
            &clock,
        );
        let _ = send.send(result);
    });
    barrier.wait();
    assert!(
        matches!(
            receive.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "an admitting writer must wait for the observer, not return WriterBusy"
    );
    drop(raw_probe);
    drop(probe);
    let lease = receive.recv_timeout(Duration::from_secs(5))??;
    assert_eq!(
        disk_writer_state(context.project_state, &path),
        Occupancy::Held
    );
    drop(lease);
    thread.join().map_err(|_| "writer panicked")?;
    assert_eq!(
        disk_writer_state(context.project_state, &path),
        Occupancy::Available,
        "a stale preparation record cannot substitute for a released lease"
    );
    Ok(())
}

#[test]
fn legacy_gate_upgrade_and_missing_lock_preserve_custody_evidence() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture.root.join("legacy-probe");
    let slot = writer_directory(context.project_state, &path)?;
    drop(slot.try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?);
    let gate_path = slot.current_path()?.join("writer-probe.lock");
    fs::remove_file(&gate_path)?;
    assert!(matches!(
        disk_writer_state(context.project_state, &path),
        Occupancy::Unknown { .. }
    ));
    assert!(
        !gate_path.exists(),
        "read-only observation cannot initialise legacy state"
    );
    prepare_writer_probe(context.project_state, &path)?;
    assert!(gate_path.is_file());
    assert_eq!(
        disk_writer_state(context.project_state, &path),
        Occupancy::Available
    );
    // Retained import evidence remains a fence even without an exclusion inode.
    fs::remove_file(slot.current_path()?.join(WRITER_LOCK))?;
    fs::write(slot.current_path()?.join("reservation.json"), "retained")?;
    fs::write(
        slot.current_path()?.join("import-retained.json"),
        "retained",
    )?;
    assert!(
        matches!(disk_writer_state(context.project_state, &path), Occupancy::Unknown { evidence, .. }
        if evidence.contains("retained import fence"))
    );
    assert!(
        matches!(cache_writer_probe(context.project_state, &path), Err(Occupancy::Unknown { evidence, .. })
        if evidence.contains("retained import fence"))
    );
    Ok(())
}

#[test]
fn recovery_wait_releases_the_observer_gate_while_a_writer_is_held() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::custody::{self, CustodyError};
    use std::sync::mpsc;
    use std::time::Duration;
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture.root.join("recovery-wait");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "recovery-wait",
        path.to_str().ok_or("path")?,
    ])?;
    let slot = writer_directory(context.project_state, &path)?;
    let scope = custody::Scope {
        attempt: AttemptId::from_bytes([102; 16]),
        generation: 1,
    };
    let lease = provenant_supervisor::fixture::acquire_writer(
        slot.try_clone()?,
        scope,
        WaitBudget::nonblocking(context.clock.mono_now()),
        &CancelToken::new(),
        context.clock,
    )?;
    let cancel = CancelToken::new();
    let worker_cancel = cancel.clone();
    let (waiting, waited) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        provenant_supervisor::fixture::on_writer_lock_wait(move || {
            waiting.send(()).expect("waiting observer");
            resumed
                .recv_timeout(Duration::from_secs(5))
                .expect("resume recovery");
        });
        let clock = provenant_os::clock::SystemClock::new();
        custody::acquire_recovery(
            slot,
            scope,
            WaitBudget::cli(clock.mono_now(), 5).expect("recovery budget"),
            &worker_cancel,
            &clock,
        )
    });
    // The recovery has attempted the actual held writer lease and is paused
    // between retries. Doctor must still acquire its observation gate.
    waited.recv_timeout(Duration::from_secs(5))?;
    let report = doctor(&context, &path, &CancelToken::new())?;
    assert_eq!(report.state, WorktreeDiagnosisState::LiveWriter);
    assert_eq!(report.holder, Some(scope.attempt));
    cancel.cancel();
    resume.send(())?;
    assert!(matches!(
        worker.join().map_err(|_| "recovery worker panicked")?,
        Err(CustodyError::Filesystem(FsError::Cancelled { .. }))
    ));
    assert!(matches!(
        cache_writer_probe(context.project_state, &path),
        Err(Occupancy::Held)
    ));
    drop(lease);
    Ok(())
}
