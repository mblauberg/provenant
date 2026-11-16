//! Doctor diagnoses the same custody boundary as admission without repairing it.
use super::lifecycle_tests::TestContext;
use super::*;
use crate::test_support::Fixture;
use std::{error::Error, fs};

#[test]
fn every_declared_repair_kind_renders_one_command() {
    for kind in WorktreeRepairKind::ALL {
        match kind {
            WorktreeRepairKind::RetryDiagnosis
            | WorktreeRepairKind::CreateWorktree
            | WorktreeRepairKind::PruneRegistration
            | WorktreeRepairKind::WaitForWriter
            | WorktreeRepairKind::PreserveChanges
            | WorktreeRepairKind::InspectGit
            | WorktreeRepairKind::CreateBranch => {}
        }
        let repair = kind.render(Path::new("/repo"), Path::new("/repo/worktree"));
        assert!(!repair.is_empty());
        assert!(!repair.contains('\n'));
    }
}

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
                    provenant_records::custody::Scope {
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
        let (expected, expected_repair) = match scenario {
            "dirty" => (
                WorktreeDiagnosisState::Dirty,
                WorktreeRepairKind::PreserveChanges,
            ),
            "stale" => (
                WorktreeDiagnosisState::StaleRegistration,
                WorktreeRepairKind::PruneRegistration,
            ),
            "held" => (
                WorktreeDiagnosisState::LiveWriter,
                WorktreeRepairKind::WaitForWriter,
            ),
            "unregistered" => (
                WorktreeDiagnosisState::Unregistered,
                WorktreeRepairKind::CreateWorktree,
            ),
            "detached" => (
                WorktreeDiagnosisState::Detached,
                WorktreeRepairKind::CreateBranch,
            ),
            _ => unreachable!(),
        };
        assert_eq!(report.state, expected, "{scenario}: {report:?}");
        assert_eq!(report.repair_kind(), expected_repair);
        assert!(!report.reason.is_empty());
        assert!(!report.repair.is_empty());
        assert!(!report.repair.contains('\n'));
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
    assert_eq!(census.repair_kind(), WorktreeRepairKind::RetryDiagnosis);
    assert_eq!(census.source, Some("census"));
    assert_eq!(
        census.scope.as_ref(),
        Some(&StructuredPathWire::from_path(&path)?)
    );
    assert!(
        census
            .evidence
            .as_deref()
            .is_some_and(|evidence| !evidence.is_empty())
    );
    assert!(!census.repair.is_empty());

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
    assert_eq!(
        census_held.repair_kind(),
        WorktreeRepairKind::RetryDiagnosis
    );
    assert_eq!(census_held.source, Some("census"));
    assert_eq!(census_held.holder, None);
    assert_eq!(
        census_held.scope.as_ref(),
        Some(&StructuredPathWire::from_path(&path)?)
    );
    assert!(
        census_held
            .evidence
            .as_deref()
            .is_some_and(|evidence| !evidence.is_empty())
    );

    let verified = verify_worktree(context.repo, &path)?;
    let owner_path = verified
        .repo()
        .private_git_dir()
        .join("provenant-owner.json");
    fs::write(&owner_path, "malformed owner record")?;
    let malformed = doctor(&test.context(), &path, &cancel)?;
    assert_eq!(malformed.state, WorktreeDiagnosisState::Unknown);
    assert_eq!(malformed.repair_kind(), WorktreeRepairKind::RetryDiagnosis);
    assert_eq!(malformed.source, Some("custody"));
    assert_eq!(
        malformed.scope.as_ref(),
        Some(&StructuredPathWire::from_path(&path)?)
    );
    assert!(
        malformed
            .evidence
            .as_deref()
            .is_some_and(|evidence| !evidence.is_empty())
    );
    assert!(!malformed.repair.is_empty());
    assert_eq!(fs::read_to_string(&owner_path)?, "malformed owner record");
    fs::remove_file(owner_path)?;

    // Custody uncertainty takes priority over an incomplete advisory census.
    let slot = writer_directory(context.project_state, &path)?;
    let scope = provenant_records::custody::Scope {
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
    assert_eq!(custody.repair_kind(), WorktreeRepairKind::RetryDiagnosis);
    assert_eq!(custody.source, Some("custody"));
    assert_eq!(
        custody.scope.as_ref(),
        Some(&StructuredPathWire::from_path(&path)?)
    );
    assert!(
        custody
            .evidence
            .as_deref()
            .is_some_and(|evidence| !evidence.is_empty())
    );
    assert!(!custody.repair.is_empty());
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
    assert_eq!(report.repair_kind(), WorktreeRepairKind::RetryDiagnosis);
    assert_eq!(report.holder, None);
    assert!(!report.repair.is_empty());
    Ok(())
}

#[test]
fn a_read_only_probe_yields_busy_without_publishing_a_reservation() -> Result<(), Box<dyn Error>> {
    // Spec: docs/specs/supervisor.md#writer-reservation-and-settlement
    // Contract (Q4): an observation gate is tried once. A writer that meets a
    // probe is refused as busy at once and leaves no record; after the probe
    // the same writer is admitted. Its reservation then belongs to settlement.
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture.root.join("probe-race");
    let slot = writer_directory(context.project_state, &path)?;
    drop(slot.try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?);
    let clock = provenant_os::clock::SystemClock::new();
    let admit = |slot: &provenant_os::fs::Directory| -> Result<_, Box<dyn Error>> {
        Ok(provenant_supervisor::fixture::acquire_writer(
            slot.try_clone()?,
            provenant_records::custody::Scope {
                attempt: AttemptId::from_bytes([92; 16]),
                generation: 1,
            },
            WaitBudget::nonblocking(clock.mono_now()),
            &CancelToken::new(),
            &clock,
        ))
    };
    let probe = provenant_supervisor::custody::writer_observation_guard(&slot)?;
    let refused = admit(&slot)?;
    assert!(
        matches!(
            refused,
            Err(provenant_supervisor::custody::CustodyError::Busy)
        ),
        "{refused:?}"
    );
    drop(probe);
    assert_eq!(
        disk_writer_state(context.project_state, &path),
        Occupancy::Available,
        "a refused admission leaves no record"
    );
    let lease = admit(&slot)??;
    assert_eq!(
        disk_writer_state(context.project_state, &path),
        Occupancy::Held
    );
    drop(lease);
    Ok(())
}

#[test]
fn interrupted_writer_slot_initialisation_can_retry_without_repairing_a_published_slot()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture.root.join("interrupted-slot");
    FAIL_WRITER_GATE_INITIALISATION.with(|failure| failure.set(true));
    assert!(matches!(
        writer_directory(context.project_state, &path),
        Err(WorktreeError::UnknownWriter)
    ));
    assert!(
        context
            .project_state
            .metadata_at(&state_dir_name(&path), false)
            .unwrap_err()
            .is_not_found()
    );
    let slot = writer_directory(context.project_state, &path)?;
    drop(provenant_supervisor::custody::writer_observation_guard(
        &slot,
    )?);
    Ok(())
}

#[test]
fn missing_observer_gate_refuses_without_creating_a_gate() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let path = fixture.root.join("missing-gate-probe");
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
        "read-only observation cannot create a missing gate"
    );
    assert!(matches!(
        cache_writer_probe(context.project_state, &path),
        Err(Occupancy::Unknown { .. })
    ));
    assert!(
        !gate_path.exists(),
        "observation cannot repair a missing gate"
    );

    Ok(())
}

#[test]
fn retained_import_evidence_remains_a_fence_without_an_exclusion_inode()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();

    // Retained import evidence remains a fence even without an exclusion inode.
    let retained_path = fixture.root.join("retained-probe");
    let retained = writer_directory(context.project_state, &retained_path)?;
    drop(retained.try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?);
    fs::remove_file(retained.current_path()?.join(WRITER_LOCK))?;
    let attempt = retained
        .current_path()?
        .join(AttemptId::from_bytes([53; 16]).to_string());
    fs::create_dir(&attempt)?;
    fs::write(attempt.join("reservation.json"), "retained")?;
    fs::write(attempt.join("import-retained.json"), "retained")?;
    assert!(matches!(
        disk_writer_state(context.project_state, &retained_path),
        Occupancy::Unknown {
            source: OccupancyUnknownSource::Custody,
            ..
        }
    ));
    assert!(matches!(
        cache_writer_probe(context.project_state, &retained_path),
        Err(Occupancy::Unknown {
            source: OccupancyUnknownSource::Custody,
            ..
        })
    ));
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
    let scope = provenant_records::custody::Scope {
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
    assert_eq!(report.repair_kind(), WorktreeRepairKind::WaitForWriter);
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
