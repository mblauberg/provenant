use super::*;
use provenant_os::clock::SystemClock;
use std::error::Error;
use std::os::unix::fs::PermissionsExt;

type TestResult = std::result::Result<(), Box<dyn Error>>;
#[test]
fn persisted_custody_accepts_future_fields_without_changing_scope_checks() -> TestResult {
    let record = serde_json::json!({
        "schema_version": 1,
        "scope": {
            "run": scope().run,
            "attempt": scope().attempt,
            "generation": 1,
            "future_scope": {"value": true}
        },
        "binding": {"kind": "read_only", "future_binding": 42},
        "future_journal": [1, 2]
    });
    let journal: OwnershipJournal =
        decode_v1(serde_json::to_vec(&record)?.as_slice(), Limits::CONFIG)?;
    assert_eq!(journal.schema_version, 1);
    assert_eq!(journal.scope, scope());
    journal.binding.validate()?;
    let mut invalid = record;
    invalid["scope"]["generation"] = serde_json::json!("invalid");
    assert!(
        decode_v1::<OwnershipJournal, _>(serde_json::to_vec(&invalid)?.as_slice(), Limits::CONFIG)
            .is_err()
    );
    Ok(())
}

#[test]
fn capture_plan_without_silence_uses_registry_default() -> TestResult {
    let mut stored = serde_json::to_value(CapturePlan::default())?;
    stored
        .as_object_mut()
        .ok_or("capture plan object")?
        .remove("silence_ms");
    let plan: CapturePlan = serde_json::from_value(stored)?;
    assert_eq!(plan.silence_ms, 300_000);
    Ok(())
}

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
        priority: None,
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
        unresolved_pids: vec![],
        unresolved_identities: vec![],
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
fn acquire(root: &std::path::Path) -> Result<CustodyLease> {
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
    let path = root.path().join("writer");
    std::fs::create_dir(&path)?;
    let writer = acquire(&path)?;
    drop(writer);
    std::fs::remove_file(path.join("reservation.json"))?;
    assert!(matches!(acquire(&path), Err(CustodyError::Fenced)));
    std::fs::write(path.join("reservation.json"), b"{}")?;
    assert!(matches!(acquire(&path), Err(CustodyError::Fenced)));
    let clock = SystemClock::new();
    assert!(matches!(
        acquire_recovery(
            Directory::open_anchor(&path)?,
            WaitBudget::cli(clock.mono_now(), 1)?,
            &CancelToken::new(),
            &clock
        ),
        Err(CustodyError::InvalidRecord)
    ));
    let good = root.path().join("good");
    std::fs::create_dir(&good)?;
    let _good = acquire(&good)?;
    let listing = list_writer_reservations(&Directory::open_anchor(root.path())?)?;
    assert_eq!(listing.reservations.len(), 1);
    assert_eq!(listing.reservations[0].scope, scope());
    assert_eq!(listing.issues.len(), 1);
    assert_eq!(
        listing.issues[0].reservation,
        PathBuf::from("writer/reservation.json")
    );
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
    Publication::require_durable(write_record(
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
    Publication::require_durable(held.atomic_write(
        OsStr::new("terminal-commit.json"),
        ".test-",
        &terminal,
        PublishMode::NoReplace,
    )?)?;
    Publication::require_durable(quarantine.atomic_write(
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
    Publication::require_durable(write_record(
        &held,
        "release.json",
        &release,
        PublishMode::NoReplace,
    )?)?;
    assert!(recovery.verify_settlement(&held).is_err());
    release.quarantine_digest = provenant_records::CanonicalV1::file_digest(&manifest);
    Publication::require_durable(write_record(
        &held,
        "release.json",
        &release,
        PublishMode::Replace,
    )?)?;
    std::fs::write(attempt.path().join(crate::build_pause::PAUSE_FILE), b"{")?;
    let commit = recovery.verify_settlement(&held)?;
    let scratch_dir = tempfile::tempdir()?;
    std::fs::set_permissions(scratch_dir.path(), std::fs::Permissions::from_mode(0o700))?;
    let mut scratch =
        provenant_os::scratch::Scratch::new(Directory::open_anchor(scratch_dir.path())?)?;
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
fn corrupt_pause_journal_does_not_block_observation() -> TestResult {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join(crate::build_pause::PAUSE_FILE), b"{")?;
    let handle = CustodyHandle {
        directory: Directory::open_anchor(root.path())?,
        scope: scope(),
        requester: ActorId::from_bytes([7; 16]),
        held: false,
    };
    let clock = SystemClock::new();
    let observation = observe(
        &handle,
        WaitBudget::cli(clock.mono_now(), 1)?,
        &CancelToken::new(),
        &clock,
    )?;
    assert!(matches!(observation, Observation::Unknown));
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
                priority: 0,
                provider_lease: None,
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
                secret_input: None,
                holder_context: Vec::new(),
            },
            lease: &mut writer,
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

struct RebootWorld {
    boot: Option<&'static str>,
}
impl ProcessObserver for RebootWorld {
    fn boot_identity(&self) -> std::result::Result<String, crate::census::CensusError> {
        self.boot
            .map(str::to_owned)
            .ok_or(crate::census::CensusError::Unsupported)
    }
    fn observe(&self, _: i64) -> ProcessObservation {
        ProcessObservation::Unknown(crate::census::ObservationIssue::PermissionDenied)
    }
    fn census(
        &self,
        _: usize,
    ) -> std::result::Result<crate::census::Census, crate::census::CensusError> {
        Err(crate::census::CensusError::Unsupported)
    }
}

#[test]
fn reboot_recovery_requires_native_proof() -> TestResult {
    const BOOT: &str = "darwin:00000000-0000-0000-0000-000000000000";
    const NEW_BOOT: &str = "darwin:11111111-1111-1111-1111-111111111111";
    for (boot, expected) in [(Some(NEW_BOOT), true), (Some(BOOT), false), (None, false)] {
        let root = tempfile::tempdir()?;
        let attempt = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(attempt.path())?;
        let recovery = spawned_reboot_recovery(root.path(), &attempt, BOOT, 10, 20)?;
        let world = RebootWorld { boot };
        let result = recovery.after_reboot_with(&attempt, scope(), &world);
        if boot.is_none() {
            assert!(matches!(result, Err(CustodyError::Io(ref error))
                if error.to_string().contains("boot identity unavailable")));
            assert!(root.path().join("reservation.json").exists());
            continue;
        }
        let proof = result?;
        assert_eq!(proof.is_some(), expected, "boot={boot:?}");
        if let Some(proof) = proof {
            assert_eq!(proof.scope(), scope());
            proof.release()?;
            assert!(!root.path().join("reservation.json").exists());
        } else {
            assert!(root.path().join("reservation.json").exists());
        }
    }
    Ok(())
}

fn spawned_reboot_recovery(
    root: &std::path::Path,
    attempt: &Directory,
    boot: &str,
    guardian_start: u64,
    provider_start: u64,
) -> Result<Recovery> {
    let mut writer = acquire(root)?;
    let metadata = attempt.metadata()?;
    let identity = |pid, native_start| ProcessIdentity {
        boot: boot.into(),
        pid,
        native_start,
        group_id: pid,
        session_id: 100,
    };
    writer.reservation.boot = boot.into();
    writer.reservation.launch_state = LaunchState::Spawned;
    writer.reservation.guardian = Some(identity(100, guardian_start));
    writer.reservation.provider = Some(identity(101, provider_start));
    writer.reservation.attempt_device = Some(metadata.device);
    writer.reservation.attempt_inode = Some(metadata.inode);
    Publication::require_durable(write_record(
        &writer.directory,
        "reservation.json",
        &writer.reservation,
        PublishMode::Replace,
    )?)?;
    writer.into_recovery()
}

#[test]
fn reboot_recovery_requires_the_original_attempt_and_exclusive_custody() -> TestResult {
    enum Refusal {
        Scope,
        Attempt,
        Custody,
    }
    for refusal in [Refusal::Scope, Refusal::Attempt, Refusal::Custody] {
        let root = tempfile::tempdir()?;
        let original = tempfile::tempdir()?;
        let original = Directory::open_anchor(original.path())?;
        let replacement = tempfile::tempdir()?;
        let replacement = Directory::open_anchor(replacement.path())?;
        let recovery = spawned_reboot_recovery(
            root.path(),
            &original,
            "darwin:00000000-0000-0000-0000-000000000000",
            10,
            20,
        )?;
        let before = std::fs::read(root.path().join("reservation.json"))?;
        let mut target = scope();
        let mut attempt = &original;
        let _custody = match refusal {
            Refusal::Scope => {
                target.generation += 1;
                None
            }
            Refusal::Attempt => {
                attempt = &replacement;
                None
            }
            Refusal::Custody => {
                Some(original.try_lock(OsStr::new("custody.lock"), LockMode::Exclusive)?)
            }
        };
        let world = RebootWorld {
            boot: Some("darwin:11111111-1111-1111-1111-111111111111"),
        };
        let result = recovery.after_reboot_with(attempt, target, &world);
        assert!(result?.is_none(), "unavailable evidence is not proof");
        assert_eq!(std::fs::read(root.path().join("reservation.json"))?, before);
    }
    Ok(())
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
