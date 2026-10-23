use super::*;
use crate::clock::SystemClock;
use std::error::Error;
use std::os::unix::fs::PermissionsExt;

type TestResult = std::result::Result<(), Box<dyn Error>>;
#[test]
fn exit_publication_bounds_escaped_facts_and_counts_every_omission() -> TestResult {
    let identity = ProcessIdentity {
        boot: "ffffffff-ffff-ffff-ffff-ffffffffffff".into(),
        pid: i64::MAX,
        native_start: u64::MAX,
        group_id: i64::MAX,
        session_id: i64::MAX,
    };
    let exit = ProcessExit {
        schema_version: 1,
        scope: scope(),
        termination: Some(Termination::Exit(0)),
        never_spawned: false,
        stop_reason: None,
        delivered_signals: vec![i64::from(libc::SIGTERM), i64::from(libc::SIGKILL)],
        reaped: vec![identity.clone(); 16],
        omitted_reaped: 2,
        spared: vec![identity.clone(); 16],
        omitted_spared: 3,
        failed_deliveries: vec![
            FailedSignalDelivery {
                identity: Some(identity),
                reason: StopReason::PostTerminalCleanup,
                signal: i64::from(libc::SIGKILL),
                failure: crate::machine::DeliveryFailure::PermissionDenied,
            };
            16
        ],
        omitted_failed_deliveries: 4,
        terminal_observed: true,
        unresolved_cleanup: true,
        settlement_evidence: SettlementEvidence::Unresolved,
        stdout: CaptureTotals::default(),
        stderr: CaptureTotals::default(),
        warnings: vec!["\u{0001}".repeat(160); 24],
        omitted_warnings: 5,
    };
    assert!(serde_json::to_vec(&exit)?.len() as u64 > EXIT_LIMIT);
    let bytes = exit.bounded_bytes()?;
    assert!(bytes.len() as u64 <= EXIT_LIMIT);
    assert_eq!(bytes, exit.bounded_bytes()?);
    let stored: serde_json::Value = serde_json::from_slice(&bytes)?;
    for (list, omitted, expected) in [
        ("reaped", "omitted_reaped", 18),
        ("spared", "omitted_spared", 19),
        ("failed_deliveries", "omitted_failed_deliveries", 20),
        ("warnings", "omitted_warnings", 29),
    ] {
        let retained = stored
            .get(list)
            .and_then(serde_json::Value::as_array)
            .ok_or("missing bounded list")?
            .len() as u64;
        let omitted = stored
            .get(omitted)
            .and_then(serde_json::Value::as_u64)
            .ok_or("missing omission count")?;
        assert_eq!(retained + omitted, expected);
    }
    assert_eq!(
        stored.get("unresolved_cleanup"),
        Some(&serde_json::Value::Bool(true))
    );
    Ok(())
}
fn scope() -> Scope {
    Scope {
        run: RunId::from_bytes([1; 16]),
        attempt: AttemptId::from_bytes([2; 16]),
        generation: 1,
    }
}
fn acquire(root: &std::path::Path) -> Result<WriterLease> {
    let clock = SystemClock::new();
    crate::fixture::acquire_writer(
        Directory::open_anchor(root)?,
        scope(),
        WaitBudget::cli(clock.mono_now(), 1).map_err(|_| CustodyError::InvalidRecord)?,
        &CancelToken::new(),
        &clock,
    )
}
#[test]
fn missing_or_malformed_reservation_does_not_reopen_a_writer() -> TestResult {
    let root = tempfile::tempdir()?;
    let writer = acquire(root.path())?;
    drop(writer);
    std::fs::remove_file(root.path().join("reservation.json"))?;
    assert!(matches!(acquire(root.path()), Err(CustodyError::Fenced)));
    std::fs::write(root.path().join("reservation.json"), b"{}")?;
    assert!(matches!(acquire(root.path()), Err(CustodyError::Fenced)));
    let clock = SystemClock::new();
    assert!(matches!(
        acquire_recovery(
            Directory::open_anchor(root.path())?,
            WaitBudget::cli(clock.mono_now(), 1)?,
            &CancelToken::new(),
            &clock
        ),
        Err(CustodyError::InvalidRecord)
    ));
    Ok(())
}
struct Authority(bool);
impl ReconcileAuthority for Authority {
    fn permits(&self, _: &ReconcileDecision) -> Result<bool> {
        Ok(self.0)
    }
}
fn decision(target: Scope, disposition: ReconcileDisposition) -> ReconcileDecision {
    ReconcileDecision {
        schema_version: 1,
        actor: ActorId::from_bytes([7; 16]),
        scope: target,
        evidence: Digest::from_bytes([9; 32]),
        disposition,
    }
}
#[test]
fn reconcile_requires_matching_authority_and_records_before_release() -> TestResult {
    let root = tempfile::tempdir()?;
    let recovery = acquire(root.path())?.into_recovery()?;
    assert!(
        recovery
            .reconcile(
                decision(scope(), ReconcileDisposition::AcceptLostCustody),
                &Authority(false)
            )
            .is_err()
    );
    assert!(!root.path().join("reconcile.json").exists());
    let mut stale = scope();
    stale.generation += 1;
    assert!(
        recovery
            .reconcile(
                decision(stale, ReconcileDisposition::AcceptLostCustody),
                &Authority(true)
            )
            .is_err()
    );
    let _receipt = recovery.reconcile(
        decision(scope(), ReconcileDisposition::RetainFence),
        &Authority(true),
    )?;
    assert!(root.path().join("reconcile.json").is_file());
    assert!(root.path().join("reservation.json").is_file());
    assert!(
        recovery
            .reconcile(
                decision(scope(), ReconcileDisposition::AcceptLostCustody),
                &Authority(true)
            )
            .is_err()
    );
    Ok(())
}
#[test]
fn settlement_checks_exact_attempt_and_both_digests_before_scratch() -> TestResult {
    let root = tempfile::tempdir()?;
    let attempt = tempfile::tempdir()?;
    let unknown = tempfile::tempdir()?;
    let mut writer = acquire(root.path())?;
    let meta = Directory::open_anchor(attempt.path())?.metadata()?;
    writer.reservation.launch_state = LaunchState::NeverSpawned;
    writer.reservation.unresolved_cleanup = false;
    writer.reservation.census_incomplete = false;
    writer.reservation.attempt_device = Some(meta.device);
    writer.reservation.attempt_inode = Some(meta.inode);
    require_durable(write_record(
        &writer.directory,
        "reservation.json",
        &writer.reservation,
        PublishMode::Replace,
    )?)?;
    let recovery = writer.into_recovery()?;
    assert!(
        recovery
            .verify_settlement(&Directory::open_anchor(unknown.path())?)
            .is_err()
    );
    let held = Directory::open_anchor(attempt.path())?;
    held.mkdir(OsStr::new("quarantine"), 0o700)?;
    let quarantine = held.open_verified_directory(OsStr::new("quarantine"))?;
    let terminal = serde_json::to_vec(
        &serde_json::json!({"schema_version":1,"scope":scope(),"kind":"canonical_terminal"}),
    )?;
    let manifest = serde_json::to_vec(
        &serde_json::json!({"schema_version":1,"scope":scope(),"kind":"quarantine_manifest"}),
    )?;
    require_durable(held.atomic_write(
        OsStr::new("terminal-commit.json"),
        ".test-",
        &terminal,
        PublishMode::NoReplace,
    )?)?;
    require_durable(quarantine.atomic_write(
        OsStr::new("manifest.json"),
        ".test-",
        &manifest,
        PublishMode::NoReplace,
    )?)?;
    let mut release = ReleaseReceipt {
        schema_version: 1,
        scope: scope(),
        terminal_digest: provenant_records::CanonicalV1::file_digest(&terminal),
        quarantine_digest: provenant_records::CanonicalV1::file_digest(b"wrong"),
    };
    require_durable(write_record(
        &held,
        "release.json",
        &release,
        PublishMode::NoReplace,
    )?)?;
    assert!(recovery.verify_settlement(&held).is_err());
    release.quarantine_digest = provenant_records::CanonicalV1::file_digest(&manifest);
    require_durable(write_record(
        &held,
        "release.json",
        &release,
        PublishMode::Replace,
    )?)?;
    let commit = recovery.verify_settlement(&held)?;
    let scratch_dir = tempfile::tempdir()?;
    std::fs::set_permissions(scratch_dir.path(), std::fs::Permissions::from_mode(0o700))?;
    let mut scratch = crate::scratch::Scratch::new(Directory::open_anchor(scratch_dir.path())?)?;
    scratch.create(OsStr::new("generated"), b"temporary")?;
    std::fs::write(scratch_dir.path().join("transcript"), b"retain")?;
    recovery.settle(commit, &mut scratch)?;
    assert!(!scratch_dir.path().join("generated").exists());
    assert!(scratch_dir.path().join("transcript").exists());
    assert!(!root.path().join("reservation.json").exists());
    assert!(root.path().join("settlement.json").exists());
    drop(acquire(root.path())?);
    Ok(())
}
#[test]
fn two_recoverers_cannot_hold_the_same_writer_exclusion() -> TestResult {
    let root = tempfile::tempdir()?;
    drop(acquire(root.path())?);
    let clock = SystemClock::new();
    let first = acquire_recovery(
        Directory::open_anchor(root.path())?,
        WaitBudget::cli(clock.mono_now(), 1)?,
        &CancelToken::new(),
        &clock,
    )?;
    let second = acquire_recovery(
        Directory::open_anchor(root.path())?,
        WaitBudget::cli(provenant_core::MonoTime::from_micros(0), 1)?,
        &CancelToken::new(),
        &clock,
    );
    assert!(matches!(
        second,
        Err(CustodyError::Filesystem(FsError::LockTimeout { .. }))
    ));
    drop(first);
    Ok(())
}

#[test]
fn prelaunch_publication_failure_keeps_outcome_for_exact_retry() -> TestResult {
    let root = tempfile::tempdir()?;
    let attempt = tempfile::tempdir()?;
    let mut writer = acquire(root.path())?;
    let clock = SystemClock::new();
    let token = CancelToken::new();
    let publication = PublicationReservation::prepare(
        Directory::open_anchor(attempt.path())?,
        WaitBudget::cli(clock.mono_now(), 1)?,
        &token,
        &clock,
    )?;
    std::fs::create_dir(attempt.path().join("process-exit.json"))?;
    token.cancel();
    let failure = launch(
        OwnedLaunch {
            command: ProviderCommand {
                resolved_route: "fixture/provider".into(),
                permission_plan_digest: Digest::from_bytes([0; 32]),
                task: provenant_core::TaskId::from_bytes([3; 16]),
                requester: ActorId::from_bytes([4; 16]),
                protected_commands: vec![],
                config_environment: vec![],
                argv: vec!["/nonexistent-never-executed".into()],
                environment: crate::fixture::environment(root.path()),
                cwd: root.path().into(),
                stdin: vec![],
            },
            writer: &mut writer,
            handshake: WaitBudget::cli(clock.mono_now(), 1)?,
            cancel: &token,
            clock: &clock,
        },
        CapturePlan::default(),
        publication,
    )
    .err()
    .ok_or("cancelled launch succeeded")?;
    assert!(failure.retry_exit_publication().is_err());
    std::fs::remove_dir(attempt.path().join("process-exit.json"))?;
    assert!(matches!(
        failure.retry_exit_publication()?,
        Publication::Durable
    ));
    let before = std::fs::read(attempt.path().join("process-exit.json"))?;
    assert!(matches!(
        failure.retry_exit_publication()?,
        Publication::Durable
    ));
    assert_eq!(
        before,
        std::fs::read(attempt.path().join("process-exit.json"))?
    );
    assert!(root.path().join("reservation.json").is_file());
    Ok(())
}

#[test]
fn contradictory_never_spawned_records_never_prove_absence() -> TestResult {
    let root = tempfile::tempdir()?;
    let writer = acquire(root.path())?;
    let mut record = writer.reservation.clone();
    record.descendants.push(ProcessIdentity {
        boot: record.boot.clone(),
        pid: 1,
        native_start: 1,
        group_id: 1,
        session_id: 1,
    });
    assert!(record.validate().is_err());
    assert_eq!(
        classify_reservation(&record, &crate::fixture::RecordedBootObserver),
        RecoveryState::Unknown
    );
    record.descendants.clear();
    record.omitted_descendants = 1;
    assert!(record.validate().is_err());
    record.omitted_descendants = 0;
    record.launch_state = LaunchState::NeverSpawned;
    record.attempt_device = Some(1);
    record.attempt_inode = Some(1);
    assert!(record.validate().is_err());
    record.unresolved_cleanup = false;
    record.census_incomplete = false;
    assert!(record.validate().is_ok());
    record.provider = Some(ProcessIdentity {
        boot: record.boot.clone(),
        pid: 1,
        native_start: 1,
        group_id: 1,
        session_id: 1,
    });
    assert!(record.validate().is_err());
    Ok(())
}

struct EmptyNativeWorld;
impl ProcessObserver for EmptyNativeWorld {
    fn boot_identity(&self) -> std::result::Result<String, crate::census::CensusError> {
        Ok("fixture-recorded-boot-v1".into())
    }
    fn observe(&self, _: i64) -> ProcessObservation {
        ProcessObservation::Absent
    }
    fn census(
        &self,
        _: usize,
    ) -> std::result::Result<crate::census::Census, crate::census::CensusError> {
        crate::census::Census::from_observations([], [], [], true, false, 1)
    }
}
#[test]
fn an_unrecorded_escape_cannot_be_cleared_by_an_empty_census() -> TestResult {
    let root = tempfile::tempdir()?;
    let writer = acquire(root.path())?;
    let mut record = writer.reservation.clone();
    record.launch_state = LaunchState::Spawned;
    record.attempt_device = Some(1);
    record.attempt_inode = Some(1);
    record.guardian = Some(ProcessIdentity {
        boot: record.boot.clone(),
        pid: 100,
        native_start: 1,
        group_id: 100,
        session_id: 100,
    });
    record.provider = Some(ProcessIdentity {
        boot: record.boot.clone(),
        pid: 101,
        native_start: 2,
        group_id: 101,
        session_id: 100,
    });
    record.census_incomplete = false;
    assert!(record.validate().is_ok());
    assert_eq!(
        classify_reservation(&record, &EmptyNativeWorld),
        RecoveryState::Unknown
    );
    record.unresolved_cleanup = false;
    assert_eq!(
        classify_reservation(&record, &EmptyNativeWorld),
        RecoveryState::Absent
    );
    record.census_incomplete = true;
    assert!(record.validate().is_err());
    assert_eq!(
        classify_reservation(&record, &EmptyNativeWorld),
        RecoveryState::Unknown
    );
    record.census_incomplete = false;
    record.omitted_descendants = 1;
    assert!(record.validate().is_err());
    assert_eq!(
        classify_reservation(&record, &EmptyNativeWorld),
        RecoveryState::Unknown
    );
    Ok(())
}
