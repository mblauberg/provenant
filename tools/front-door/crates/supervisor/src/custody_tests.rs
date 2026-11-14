use super::*;
use provenant_os::clock::SystemClock;
use std::error::Error;
use std::os::unix::fs::PermissionsExt;

#[test]
fn admission_clock_error_stays_an_admission_refusal() {
    let error = CustodyError::from(AdmissionError::Clock);
    assert!(matches!(
        error,
        CustodyError::Admission(AdmissionError::Clock)
    ));
    assert!(!error.to_string().is_empty());
}

type TestResult = std::result::Result<(), Box<dyn Error>>;

/// The persisted attempt shape is pinned independently of the current
/// serializer; only physical directory identities vary between installations.
#[test]
fn current_attempt_reservation_preserves_uncommitted_work() -> TestResult {
    let root = tempfile::tempdir()?;
    let path = root.path().join("writer");
    let worktree = root.path().join("worktree");
    std::fs::create_dir(&path)?;
    std::fs::create_dir(&worktree)?;
    provenant_os::fixture::run_git(&worktree, &["init", "-q"])?;
    provenant_os::fixture::run_git(&worktree, &["checkout", "-qb", "lane"])?;
    std::fs::write(worktree.join("uncommitted.txt"), b"original")?;
    provenant_os::fixture::run_git(&worktree, &["add", "uncommitted.txt"])?;
    provenant_os::fixture::run_git(&worktree, &["commit", "-qm", "base"])?;
    let base = String::from_utf8(provenant_os::fixture::run_git(
        &worktree,
        &["rev-parse", "HEAD"],
    )?)?;
    std::fs::write(worktree.join("uncommitted.txt"), b"uncommitted work")?;
    let scope = scope();
    let evidence_path = path.join(scope.attempt.to_string());
    std::fs::create_dir(&evidence_path)?;
    let slot = Directory::open_anchor(&path)?;
    let evidence = Directory::open_anchor(&evidence_path)?;
    let tree = Directory::open_anchor(&worktree)?;
    let git = Directory::open_anchor(&worktree.join(".git"))?;
    let evidence_identity = evidence.metadata()?;
    let tree_identity = tree.metadata()?;
    let git_identity = git.metadata()?;
    let fixture = r#"{"schema_version":1,"scope":{"attempt":"@ATTEMPT@","generation":1},"launch_state":"prepared","attempt_device":null,"attempt_inode":null,"reservation_device":@SLOT_DEVICE@,"reservation_inode":@SLOT_INODE@,"binding":{"kind":"worktree","worktree_device":@TREE_DEVICE@,"worktree_inode":@TREE_INODE@,"git_device":@GIT_DEVICE@,"git_inode":@GIT_INODE@,"branch":"refs/heads/lane","base":"@BASE@"},"boot":"linux:00000000-0000-0000-0000-000000000001","guardian":null,"provider":null,"unresolved_cleanup":true,"descendants":[],"census_incomplete":true,"omitted_descendants":0}"#
    .replace("@ATTEMPT@", &scope.attempt.to_string())
    .replace("@SLOT_DEVICE@", &evidence_identity.device.to_string())
    .replace("@SLOT_INODE@", &evidence_identity.inode.to_string())
    .replace("@TREE_DEVICE@", &tree_identity.device.to_string())
    .replace("@TREE_INODE@", &tree_identity.inode.to_string())
    .replace("@GIT_DEVICE@", &git_identity.device.to_string())
    .replace("@GIT_INODE@", &git_identity.inode.to_string())
    .replace("@BASE@", base.trim());
    let reservation_path = evidence_path.join("reservation.json");
    let marker_path = evidence_path.join("import-retained.json");
    std::fs::write(&reservation_path, fixture.as_bytes())?;
    std::fs::write(&marker_path, b"retained import evidence")?;
    let decoded: Reservation = read_record(&evidence, "reservation.json", RECORD_LIMIT)?;
    validate_anchor(&evidence, &decoded)?;
    assert_eq!(decoded.scope, scope);
    assert!(matches!(
        decoded.binding,
        StoredWriterBinding::Worktree { .. }
    ));
    assert!(reservation_present(&slot, scope)?);
    assert!(matches!(acquire(&path), Err(CustodyError::Fenced)));
    assert_eq!(std::fs::read(&reservation_path)?, fixture.as_bytes());
    assert_eq!(std::fs::read(&marker_path)?, b"retained import evidence");
    assert_eq!(
        std::fs::read(worktree.join("uncommitted.txt"))?,
        b"uncommitted work"
    );
    assert_eq!(
        provenant_os::fixture::run_git(&worktree, &["status", "--porcelain"])?,
        b" M uncommitted.txt\n"
    );
    Ok(())
}

#[test]
fn slot_root_reservations_are_not_adopted_as_attempt_evidence() -> TestResult {
    let root = tempfile::tempdir()?;
    let path = root.path().join("writer");
    std::fs::create_dir(&path)?;
    let attempt_path = path.join(scope().attempt.to_string());
    std::fs::create_dir(&attempt_path)?;
    let slot = Directory::open_anchor(&path)?;
    let root_record = path.join("reservation.json");
    std::fs::write(&root_record, br#"{"schema_version":1,"slot":"old"}"#)?;

    let clock = SystemClock;
    assert!(
        acquire_recovery(
            slot,
            scope(),
            WaitBudget::nonblocking(clock.mono_now()),
            &CancelToken::new(),
            &clock,
        )
        .is_err()
    );
    assert!(root_record.exists());
    assert!(!attempt_path.join("reservation.json").exists());
    Ok(())
}

#[test]
fn reservation_listing_isolates_an_inaccessible_slot() -> TestResult {
    let root = tempfile::tempdir()?;
    let blocked = root.path().join("blocked");
    let good = root.path().join("good");
    std::fs::create_dir(&blocked)?;
    std::fs::create_dir(&good)?;
    drop(acquire(&good)?);
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o0))?;
    let result = list_writer_reservations(&Directory::open_anchor(root.path())?);
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700))?;
    let listing = result?;
    assert_eq!(listing.reservations.len(), 1);
    assert_eq!(listing.reservations[0].scope, scope());
    assert_eq!(listing.issues.len(), 1);
    assert_eq!(
        listing.issues[0].reservation,
        PathBuf::from("blocked/reservation.json")
    );
    Ok(())
}

#[test]
fn admission_uses_flock_and_retains_predecessor_evidence() -> TestResult {
    let root = tempfile::tempdir()?;
    let path = root.path().join("writer");
    std::fs::create_dir(&path)?;
    let slot = Directory::open_anchor(&path)?;
    for name in ["ownership.json", "settlement.json"] {
        std::fs::write(path.join(name), b"retired malformed registry record")?;
    }
    let first = acquire(&path)?;
    let first_path = path
        .join(scope().attempt.to_string())
        .join("reservation.json");
    let first_bytes = std::fs::read(&first_path)?;
    let next = Scope {
        attempt: AttemptId::from_bytes([4; 16]),
        generation: 1,
    };
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let admit = || {
        crate::fixture::acquire_writer(
            slot.try_clone()?,
            next,
            WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )
    };
    assert!(matches!(admit(), Err(CustodyError::Busy)));
    drop(first);
    let second = admit()?;
    assert!(matches!(
        slot.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive),
        Err(FsError::LockTimeout { .. })
    ));
    assert_eq!(std::fs::read(&first_path)?, first_bytes);
    let listing = list_writer_reservations(&Directory::open_anchor(root.path())?)?;
    assert_eq!(listing.reservations.len(), 2);
    assert!(
        listing
            .reservations
            .iter()
            .any(|record| record.scope == scope())
    );
    assert!(
        listing
            .reservations
            .iter()
            .any(|record| record.scope == next)
    );
    second.withdraw_unlaunched()?;
    assert_eq!(std::fs::read(&first_path)?, first_bytes);
    let recovery = acquire_recovery(
        slot.try_clone()?,
        scope(),
        WaitBudget::nonblocking(clock.mono_now()),
        &cancel,
        &clock,
    )?;
    assert_eq!(recovery.scope(), scope());
    assert!(matches!(admit(), Err(CustodyError::Busy)));
    recovery.withdraw_prepared()?;
    admit()?.withdraw_unlaunched()?;
    Ok(())
}

#[test]
fn worktree_settlement_retains_flock_until_recovery_is_dropped() -> TestResult {
    for read_only in [false, true] {
        let root = tempfile::tempdir()?;
        let tree = tempfile::tempdir()?;
        let git = tempfile::tempdir()?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let directory = Directory::open_anchor(root.path())?;
        let binding = if read_only {
            WriterBinding::read_only()
        } else {
            WriterBinding::from_verified(
                &Directory::open_anchor(tree.path())?,
                &Directory::open_anchor(git.path())?,
                "refs/heads/lane",
                GitOid::parse(&"1".repeat(40))?,
            )?
        };
        let mut writer = acquire_writer(
            directory.try_clone()?,
            scope(),
            binding,
            WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?;
        writer.reservation.launch_state = LaunchState::NeverSpawned;
        writer.reservation.unresolved_cleanup = false;
        writer.reservation.census_incomplete = false;
        let metadata = Directory::open_anchor(tree.path())?.metadata()?;
        writer.reservation.attempt_device = Some(metadata.device);
        writer.reservation.attempt_inode = Some(metadata.inode);
        Publication::require_durable(write_record(
            &writer.directory,
            "reservation.json",
            &writer.reservation,
            PublishMode::Replace,
        )?)?;
        let mut recovery = writer.into_recovery()?;
        recovery.prepare_settlement()?;
        let attempt_lock =
            || directory.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive);
        if read_only {
            drop(attempt_lock()?);
        } else {
            assert!(matches!(attempt_lock(), Err(FsError::LockTimeout { .. })));
        }
        drop(recovery);
        drop(attempt_lock()?);
    }
    Ok(())
}

#[test]
fn persisted_custody_accepts_future_fields_without_changing_scope_checks() -> TestResult {
    let record = serde_json::json!({
        "schema_version": 1,
        "scope": {
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
    assert_eq!(plan.silence_ms, 1_800_000);
    assert!(!plan.silent(1_799_999, Some(0)));
    assert!(plan.silent(1_800_000, Some(0)));
    assert!(!plan.silent(1_800_000, Some(1)));
    assert!(plan.silent_for_ms(1_800_000, Some(0)).is_some());
    assert!(
        CapturePlan {
            silence_ms: 3_600_001,
            ..plan
        }
        .validate()
        .is_err()
    );
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
        cancel_delivered_before_exit: None,
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
        launch_refusal: None,
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
    std::fs::remove_file(
        path.join(scope().attempt.to_string())
            .join("reservation.json"),
    )?;
    assert!(matches!(acquire(&path), Err(CustodyError::Fenced)));
    std::fs::write(
        path.join(scope().attempt.to_string())
            .join("reservation.json"),
        b"{}",
    )?;
    assert!(matches!(acquire(&path), Err(CustodyError::Fenced)));
    let clock = SystemClock::new();
    assert!(matches!(
        acquire_recovery(
            Directory::open_anchor(&path)?,
            scope(),
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
        PathBuf::from("writer")
            .join(scope().attempt.to_string())
            .join("reservation.json")
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
    assert!(
        !root
            .path()
            .join(scope().attempt.to_string())
            .join("reconcile.json")
            .exists()
    );
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
    assert!(
        root.path()
            .join(scope().attempt.to_string())
            .join("reconcile.json")
            .is_file()
    );
    assert!(
        root.path()
            .join(scope().attempt.to_string())
            .join("reservation.json")
            .is_file()
    );
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
    let reservation_bytes = serde_json::to_vec(&writer.reservation)?;
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
    assert!(
        !root
            .path()
            .join(scope().attempt.to_string())
            .join("reservation.json")
            .exists()
    );
    assert!(
        root.path()
            .join(scope().attempt.to_string())
            .join("settlement.json")
            .exists()
    );
    // Simulate a crash after durable settlement but before reservation unlink.
    let evidence_path = root.path().join(scope().attempt.to_string());
    let evidence = Directory::open_anchor(&evidence_path)?;
    std::fs::write(evidence_path.join("reservation.json"), &reservation_bytes)?;
    let marker = ImportFence {
        schema_version: 1,
        scope: scope(),
        lane: provenant_records::WorkKey::new(
            provenant_core::ProjectId::from_bytes([1; 16]),
            "retained",
            "writer",
            1,
        )?,
        run_attempt: attempt.path().to_path_buf(),
    };
    let marker_bytes = serde_json::to_vec(&marker)?;
    std::fs::write(evidence_path.join("import-retained.json"), &marker_bytes)?;
    let journal_path = evidence_path.join("settlement.json");
    let journal = std::fs::read(&journal_path)?;
    let release_path = attempt.path().join("release.json");
    let release_bytes = std::fs::read(&release_path)?;
    let wrong_attempt = Scope {
        attempt: AttemptId::from_bytes([9; 16]),
        ..scope()
    };
    let wrong_generation = Scope {
        generation: scope().generation + 1,
        ..scope()
    };
    for bytes in [
        None,
        Some(b"{".to_vec()),
        Some(serde_json::to_vec(&SettlementJournal {
            schema_version: 1,
            scope: wrong_attempt,
        })?),
        Some(serde_json::to_vec(&SettlementJournal {
            schema_version: 1,
            scope: wrong_generation,
        })?),
    ] {
        match bytes {
            Some(bytes) => std::fs::write(&journal_path, bytes)?,
            None => std::fs::remove_file(&journal_path)?,
        }
        assert!(matches!(
            retained_import_decision(&evidence, scope())?,
            Some(CustodyDecision::Pending(_))
        ));
        assert!(acquire(root.path()).is_err());
        assert_eq!(
            std::fs::read(evidence_path.join("reservation.json"))?,
            reservation_bytes
        );
    }
    std::fs::write(&journal_path, &journal)?;
    for path in [
        &release_path,
        &attempt.path().join("terminal-commit.json"),
        &attempt.path().join("quarantine/manifest.json"),
    ] {
        let bytes = std::fs::read(path)?;
        std::fs::remove_file(path)?;
        assert!(matches!(
            retained_import_decision(&evidence, scope())?,
            Some(CustodyDecision::Pending(_))
        ));
        std::fs::write(path, b"{")?;
        assert!(matches!(
            retained_import_decision(&evidence, scope())?,
            Some(CustodyDecision::Pending(_))
        ));
        std::fs::write(path, bytes)?;
    }
    for wrong in [wrong_attempt, wrong_generation] {
        let mismatched = ImportFence {
            scope: wrong,
            ..marker.clone()
        };
        std::fs::write(
            evidence_path.join("import-retained.json"),
            serde_json::to_vec(&mismatched)?,
        )?;
        assert!(retained_import_decision(&evidence, scope()).is_err());
        assert!(acquire(root.path()).is_err());
    }
    let other_run = ImportFence {
        run_attempt: unknown.path().to_path_buf(),
        ..marker.clone()
    };
    std::fs::write(
        evidence_path.join("import-retained.json"),
        serde_json::to_vec(&other_run)?,
    )?;
    assert!(matches!(
        retained_import_decision(&evidence, scope())?,
        Some(CustodyDecision::Pending(_))
    ));
    std::fs::write(evidence_path.join("import-retained.json"), &marker_bytes)?;
    assert_eq!(
        retained_import_decision(&evidence, scope())?,
        Some(CustodyDecision::Settled(scope()))
    );
    let slot = Directory::open_anchor(root.path())?;
    assert!(pending_import(&slot)?.is_none());
    // Observation preserves all evidence. Admission alone completes removal.
    assert_eq!(
        std::fs::read(evidence_path.join("reservation.json"))?,
        reservation_bytes
    );
    let clock = SystemClock::new();
    drop(crate::fixture::acquire_writer(
        slot,
        wrong_attempt,
        WaitBudget::nonblocking(clock.mono_now()),
        &CancelToken::new(),
        &clock,
    )?);
    assert!(!evidence_path.join("reservation.json").exists());
    assert_eq!(
        std::fs::read(evidence_path.join("import-retained.json"))?,
        marker_bytes
    );
    assert_eq!(std::fs::read(release_path)?, release_bytes);
    assert_eq!(
        std::fs::read(scratch_dir.path().join("transcript"))?,
        b"retain"
    );
    Ok(())
}

#[test]
fn two_recoverers_cannot_hold_the_same_writer_exclusion() -> TestResult {
    let root = tempfile::tempdir()?;
    drop(acquire(root.path())?);
    let clock = SystemClock::new();
    let first = acquire_recovery(
        Directory::open_anchor(root.path())?,
        scope(),
        WaitBudget::cli(clock.mono_now(), 1)?,
        &CancelToken::new(),
        &clock,
    )?;
    let second = acquire_recovery(
        Directory::open_anchor(root.path())?,
        scope(),
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
                policy: Default::default(),
                profile_file: None,
                priority: 0,
                lane_ports: None,
                resolved_route: "fixture/provider".into(),
                permission_plan_digest: Digest::from_bytes([0; 32]),
                key: crate::custody::fixture_key(3),
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
    assert!(
        root.path()
            .join(scope().attempt.to_string())
            .join("reservation.json")
            .is_file()
    );
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
            assert!(
                root.path()
                    .join(scope().attempt.to_string())
                    .join("reservation.json")
                    .exists()
            );
            continue;
        }
        let proof = result?;
        assert_eq!(proof.is_some(), expected, "boot={boot:?}");
        if let Some(proof) = proof {
            assert_eq!(proof.scope(), scope());
            proof.release()?;
            assert!(
                !root
                    .path()
                    .join(scope().attempt.to_string())
                    .join("reservation.json")
                    .exists()
            );
        } else {
            assert!(
                root.path()
                    .join(scope().attempt.to_string())
                    .join("reservation.json")
                    .exists()
            );
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
        let before = std::fs::read(
            root.path()
                .join(scope().attempt.to_string())
                .join("reservation.json"),
        )?;
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
        assert_eq!(
            std::fs::read(
                root.path()
                    .join(scope().attempt.to_string())
                    .join("reservation.json")
            )?,
            before
        );
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

#[test]
fn failed_ownership_publication_reports_retained_reservation() -> TestResult {
    struct FailOwnership(PathBuf);
    impl ProcessObserver for FailOwnership {
        fn boot_identity(&self) -> std::result::Result<String, crate::census::CensusError> {
            std::fs::create_dir(&self.0).expect("create ownership obstruction");
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
    let root = tempfile::tempdir()?;
    let directory = Directory::open_anchor(root.path())?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let observer = FailOwnership(
        root.path()
            .join(scope().attempt.to_string())
            .join("ownership.json"),
    );
    let result = acquire_writer_with_observer(
        directory.try_clone()?,
        scope(),
        WriterBinding::read_only(),
        WaitBudget::nonblocking(clock.mono_now()),
        &cancel,
        &clock,
        &observer,
    );
    assert!(matches!(result, Err(CustodyError::ReservationRetained(_))));
    assert!(reservation_present(&directory, scope())?);
    Ok(())
}

#[test]
fn never_spawned_resource_fact_survives_exit_storage() -> TestResult {
    use provenant_records::diagnostic::{PortSettingSource, ResourceFact};
    let fact = ResourceFact::LanePorts {
        blocks: 2,
        origin: PortSettingSource::Configuration,
    };
    let mut exit = ProcessExit::never_spawned(scope(), None, vec![]);
    exit.launch_refusal = Some(fact);
    let stored: StoredExit = serde_json::from_slice(&serde_json::to_vec(&exit)?)?;
    assert_eq!(stored.launch_refusal, Some(fact));
    assert_eq!(exit.durable_observation().launch_refusal, Some(fact));
    Ok(())
}

#[test]
fn observation_gates_try_once_and_preserve_unknown_errors() -> TestResult {
    let root = tempfile::tempdir()?;
    let directory = Directory::open_anchor(root.path())?;
    let guard = writer_admission_guard(&directory)?;
    assert!(matches!(
        writer_observation_guard(&directory),
        Err(CustodyError::Busy)
    ));
    assert!(matches!(
        writer_admission_guard(&directory),
        Err(CustodyError::Busy)
    ));
    drop(guard);
    drop(writer_observation_guard(&directory)?);
    std::fs::remove_file(root.path().join("writer-probe.lock"))?;
    assert!(matches!(
        writer_observation_guard(&directory),
        Err(CustodyError::ObservationGate(_))
    ));
    Ok(())
}

#[test]
fn writer_exclusion_gate_contention_is_busy_independent_of_writer_budget() -> TestResult {
    let root = tempfile::tempdir()?;
    // The directory reports its canonical path (`/private/var/...` on macOS).
    let base = std::fs::canonicalize(root.path())?;
    let directory = Directory::open_anchor(&base)?;
    let _observer = directory.try_lock(OsStr::new("writer-probe.lock"), LockMode::Exclusive)?;
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_micros(0)?,
        provenant_core::MonoTime::from_micros(0),
    );
    let error = lock_writer_exclusion(
        &directory,
        WaitBudget::up_to(
            clock.mono_now(),
            provenant_core::DurationMicros::from_micros(10_000),
        ),
        &CancelToken::new(),
        &clock,
    )
    .expect_err("an observer yields busy without consuming the writer budget");
    assert!(matches!(error, CustodyError::Busy));

    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(matches!(
        lock_writer_exclusion(
            &directory,
            WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        ),
        Err(CustodyError::Filesystem(FsError::Cancelled { .. }))
    ));
    Ok(())
}

#[test]
fn writer_exclusion_distinct_gate_failure_remains_unknown() -> TestResult {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join("writer-probe.lock"))?;
    let directory = Directory::open_anchor(root.path())?;
    let clock = SystemClock::new();
    assert!(matches!(
        lock_writer_exclusion(
            &directory,
            WaitBudget::cli(clock.mono_now(), 1)?,
            &CancelToken::new(),
            &clock,
        ),
        Err(CustodyError::ObservationGate(_))
    ));
    Ok(())
}
