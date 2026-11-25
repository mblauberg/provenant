use super::*;
use provenant_core::Clock;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
// Spec: docs/specs/run-settlement.md#continuation
fn resume_policy_uses_current_project_configuration() -> TestResult {
    use provenant_records::policy::{Source, Strength};
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = crate::tests::input();
    crate::tests::rekey(&mut run);
    store.create(&run, &call)?;
    std::fs::write(
        context.state_root.as_path().join("config.toml"),
        format!(
            "schema_version = 1\nprofile = 'economy'\n[domains.policy.projects.\"{}\".rules]\nWRITE_SCOPE = 'refuse'\n",
            context.project_id,
        ),
    )?;
    let policy = store.resume_policy(run.attempt_id, &call)?;
    assert_eq!(policy.rule("WRITE_SCOPE").rule.strength, Strength::Refuse);
    assert_eq!(policy.rule("WRITE_SCOPE").source, Source::Project);
    Ok(())
}

#[test]
// Spec: docs/specs/run-settlement.md#continuation
fn resume_re_resolves_policy_from_current_project_configuration() -> TestResult {
    use provenant_records::policy::{Source, Strength};
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    store.create(&run, &call)?;
    store.withdraw(
        run.attempt_id,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Cancelled,
        "fixture",
        clock.wall_now()?,
        &call,
    )?;
    std::fs::write(
        context.state_root.as_path().join("config.toml"),
        format!(
            "schema_version = 1\nprofile = 'economy'\n[domains.policy.projects.\"{}\".rules]\nWRITE_SCOPE = 'warn'\n",
            context.project_id,
        ),
    )?;
    let current = provenant_config::policy::read(
        store.writer_registry(),
        &context.project_id.to_string(),
        &Default::default(),
    );
    assert_eq!(current.rule("WRITE_SCOPE").rule.strength, Strength::Warn);
    assert_eq!(current.rule("WRITE_SCOPE").source, Source::Project);
    let next = Resume {
        prior: run.attempt_id,
        expected_generation: 0,
        operation_id: "policy-resume".parse()?,
        attempt_id: AttemptId::from_bytes([9; 16]),
        prompt: "Continue".into(),
        created_at: clock.wall_now()?,
        extend: Vec::new(),
    };
    store.resume(&next, &call)?;
    let policy = store.resume_policy(next.attempt_id, &call)?;
    assert_eq!(policy.rule("WRITE_SCOPE").rule.strength, Strength::Warn);
    assert_eq!(policy.rule("WRITE_SCOPE").source, Source::Project);
    Ok(())
}

#[test]
fn missing_terminal_attempt_directory_does_not_abort_reconcile()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let record = store.record(id, &call)?;
    store.commit_lost(RunStore::scope(&record), "fixture", &call)?;
    store.set_temporary_retained(id, true, &call)?;
    let path = context.state_root.as_path().join(format!("attempts/{id}"));
    std::fs::rename(path, temp.path().join("missing-attempt"))?;
    store.reconcile_with_observer(
        run.attempt_id,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    Ok(())
}

// Spec: docs/specs/run-settlement.md#guardian-loss-proof
#[test]
fn tick_queues_unknown_and_consumes_prepared_guardian_loss_proof()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_supervisor::census::{Census, CensusError, ProcessObservation};
    struct Gone(String);
    impl ProcessObserver for Gone {
        fn boot_identity(&self) -> std::result::Result<String, CensusError> {
            Ok(self.0.clone())
        }
        fn observe(&self, _: i64) -> ProcessObservation {
            ProcessObservation::Absent
        }
        fn census(&self, _: usize) -> std::result::Result<Census, CensusError> {
            Census::from_observations([], [], [], true, false, 64)
        }
    }
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
    let input = crate::tests::input();
    let id = input.attempt_id;
    store.create(&input, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let record = store.record(id, &call)?;
    let scope = RunStore::scope(&record);
    let attempt = store.attempt_dir(id, false)?;
    let lease = custody::acquire_reader(
        attempt.try_clone()?,
        scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    let evidence = custody::attempt_evidence(&attempt, scope)?;
    let native_custody = attempt.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let guardian = evidence.try_lock(
        std::ffi::OsStr::new("guardian.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let identity = |meta: provenant_os::fs::Metadata| serde_json::json!({"device": meta.device, "inode": meta.inode});
    let raw = evidence.read_bounded(std::ffi::OsStr::new("reservation.json"), RECORD_LIMIT)?;
    let mut reservation: serde_json::Value = serde_json::from_slice(&raw)?;
    reservation["launch_state"] = "handing_off".into();
    reservation["attempt_device"] = attempt.metadata()?.device.into();
    reservation["attempt_inode"] = attempt.metadata()?.inode.into();
    let observer = Gone(reservation["boot"].as_str().ok_or("boot")?.to_owned());
    evidence
        .atomic_write(
            std::ffi::OsStr::new("reservation.json"),
            ".test-",
            &serde_json::to_vec(&reservation)?,
            provenant_os::fs::PublishMode::Replace,
        )?
        .require_durable()?;
    crate::artifacts::seal(
        &evidence,
        "guardian-lock.json",
        &serde_json::to_vec(&serde_json::json!({
            "schema_version": 2, "scope": scope, "guardian_process": null,
            "attempt": identity(attempt.metadata()?), "guardian": identity(guardian.metadata()?),
            "writer": identity(attempt.metadata_at(std::ffi::OsStr::new("writer.lock"), false)?),
            "custody": identity(native_custody.metadata()?),
            "holder": identity(evidence.metadata_at(std::ffi::OsStr::new("holder.lock"), false)?)
        }))?,
        RECORD_LIMIT,
    )?;
    // Neither stale process checkpoints nor PID values establish loss.
    let queued = std::cell::Cell::new(0);
    store.reap_with_census_and_spawn(&call, &observer, Some(&observer.census(64)?), |_, _| {
        queued.set(queued.get() + 1);
        Ok(())
    })?;
    assert_eq!(queued.get(), 1);
    assert_eq!(
        store.settle_holderless(&record, &call, &observer)?,
        None,
        "held original locks veto settlement"
    );
    drop(guardian);
    drop(native_custody);
    drop(lease);
    assert_eq!(
        store.settle_holderless(&record, &call, &observer)?,
        Some(Reconciled::Finished)
    );
    assert!(
        matches!(store.record(id, &call)?.state, AttemptState::Terminal { outcome: Outcome::Lost, ref reason, .. } if reason.as_ref().is_some_and(|reason| reason.contains("guardian lost")))
    );
    assert!(!custody::reservation_present(&attempt, scope)?);
    Ok(())
}

#[test]
fn reader_refusal_published_before_recovery_exclusion_is_not_lost()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let record = store.record(id, &call)?;
    let scope = RunStore::scope(&record);
    let attempt = store.attempt_dir(id, false)?;
    let path = context.state_root.as_path().join(format!("attempts/{id}"));
    drop(custody::acquire_reader(
        attempt.try_clone()?,
        scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    let reservation = path.join(id.to_string()).join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    value["launch_state"] = "spawned".into();
    value["boot"] = "darwin:00000000-0000-0000-0000-000000000000".into();
    value["guardian"] = serde_json::json!({
        "boot": "darwin:00000000-0000-0000-0000-000000000000",
        "pid": 123, "native_start": 1, "group_id": 123, "session_id": 123
    });
    let meta = attempt.metadata()?;
    value["attempt_device"] = meta.device.into();
    value["attempt_inode"] = meta.inode.into();
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    crate::artifacts::seal(&attempt, "launch.json", b"{}", RECORD_LIMIT)?;
    // Simulate the guardian's refusal while it owns writer exclusion, in
    // the precise gap after the first holder check and before acquisition.
    let hook_path = path.clone();
    let hook_reservation = reservation.clone();
    HOLDERLESS_BEFORE_EXCLUSION.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let dir = provenant_os::fs::Directory::open_anchor(&hook_path).unwrap();
            let _lease = dir
                .try_lock(
                    std::ffi::OsStr::new("writer.lock"),
                    provenant_os::fs::LockMode::Exclusive,
                )
                .unwrap();
            crate::artifacts::seal(&dir, "process-exit.json", b"refusal", RECORD_LIMIT).unwrap();
            let mut current: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&hook_reservation).unwrap()).unwrap();
            current["launch_state"] = "settling".into();
            current["unresolved_cleanup"] = false.into();
            current["census_incomplete"] = false.into();
            std::fs::write(&hook_reservation, serde_json::to_vec(&current).unwrap()).unwrap();
        }))
    });
    assert_eq!(
        store.settle_holderless(&record, &call, &SystemProcessObserver)?,
        None
    );
    assert_eq!(store.record(id, &call)?, record);
    assert!(reservation.exists());
    let recovery = custody::acquire_recovery(attempt, scope, call.lock_budget(), &cancel, &clock)?;
    assert_eq!(recovery.observe(), RecoveryState::Absent);
    Ok(())
}

/// A Lost read-only review may ignore its stale reservation only after the
/// custody lock is free, even when reboot recovery could not acquire it.
// Spec: docs/specs/run-settlement.md#outcome-before-release
#[test]
fn lost_read_only_review_keeps_reservation_fenced_by_live_custody_lock() -> TestResult {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let input = crate::tests::input();
    let id = input.attempt_id;
    store.create(&input, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let mut record = store.record(id, &call)?;
    record.state = AttemptState::Terminal {
        extra: Default::default(),
        terminal_cause: None,
        terminal_note: None,
        stop: None,
        outcome: Outcome::Lost,
        exit_code: None,
        finished_at: record.queued_at,
        reason: Some("guardian lost".into()),
    };
    record.provider_launched = Some(true);
    let scope = RunStore::scope(&record);
    let attempt = store.attempt_dir(id, false)?;
    drop(custody::acquire_reader(
        attempt.try_clone()?,
        scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    assert!(custody::reservation_present(&attempt, scope)?);
    let custody_lock = attempt.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;

    let result = store.reconcile_terminal_temporary(&record, &call, &SystemProcessObserver)?;

    assert!(
        matches!(result, Some(Reconciled::Uncertain { .. })),
        "a live custody lock keeps the terminal review fenced: {result:?}"
    );
    assert!(custody::reservation_present(&attempt, scope)?);
    drop(custody_lock);
    Ok(())
}

#[test]
fn reboot_settlement_error_propagates() -> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_supervisor::census::{Census, CensusError, ProcessObservation, ProcessObserver};
    struct Reboot;
    impl ProcessObserver for Reboot {
        fn boot_identity(&self) -> std::result::Result<String, CensusError> {
            Ok("darwin:11111111-1111-1111-1111-111111111111".into())
        }
        fn observe(&self, _: i64) -> ProcessObservation {
            unreachable!()
        }
        fn census(&self, _: usize) -> std::result::Result<Census, CensusError> {
            unreachable!()
        }
    }
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let record = store.record(id, &call)?;
    let attempt = store.attempt_dir(id, false)?;
    let scope = RunStore::scope(&record);
    attempt.atomic_write(std::ffi::OsStr::new("process.json"), ".checkpoint-", &serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "scope": scope, "publication_pending": false,
            "guardian": { "boot": "darwin:00000000-0000-0000-0000-000000000000", "pid": 100, "native_start": 1, "group_id": 100, "session_id": 100 },
            "provider": { "boot": "darwin:00000000-0000-0000-0000-000000000000", "pid": 101, "native_start": 2, "group_id": 101, "session_id": 100 }
        }))?, provenant_os::fs::PublishMode::Replace)?;
    let mut next = record.clone();
    next.generation += 1;
    next.revision += 1;
    store.write(&call, |tx| {
        crate::admission::update_attempt(tx, &record, &next)
    })?;
    let result = store.settle_rebooted(&record, &call, &Reboot);
    assert!(
        matches!(
            result,
            Err(RunError::Validation("process observation scope"))
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn unavailable_reboot_evidence_never_proves_a_reboot() -> TestResult {
    for evidence in [
        RebootEvidence::UnreadableExit,
        RebootEvidence::CorruptReservation,
    ] {
        unavailable_reboot_evidence(evidence)?;
    }
    Ok(())
}

#[test]
fn claim_records_the_runtime_deadline_and_a_host_cap_tightens_it()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for (cap, seconds) in [(0, 3600), (600, 600)] {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        assert_eq!(store.record(id, &call)?.runtime_deadline, None);
        store.mark_starting_with_priority(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            None,
            &crate::limits::deadline_only(cap),
            &call,
        )?;
        let claimed = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let started = claimed.started_at.ok_or("no start")?;
        let expected =
            started.checked_add(provenant_core::DurationMicros::from_seconds(seconds)?)?;
        assert_eq!(claimed.runtime_deadline, Some(expected));
    }
    Ok(())
}

#[test]
fn reap_queues_recovery_for_running_and_cancelled_starting_attempts_only()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let attempt = store.attempt_dir(id, false)?;
    let launcher = attempt.try_lock(
        std::ffi::OsStr::new("launcher.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let spawned = std::cell::Cell::new(0);
    let spawn = |_: &[std::ffi::OsString], _: &std::path::Path| {
        spawned.set(spawned.get() + 1);
        Ok(())
    };
    assert_eq!(
        store.reap_with(&call, spawn)?,
        0,
        "an uncancelled start waits"
    );
    let record = store.record(id, &call)?;
    let scope = RunStore::scope(&record);
    let owner = record.verified_owner.unwrap_or(context.actor_id);
    assert_eq!(
        custody::request_stop(
            attempt.try_clone()?,
            scope,
            owner,
            StopReason::ExplicitCancellation,
        )?,
        StopReceipt::Requested
    );
    assert_eq!(
        store.reap_with(&call, spawn)?,
        1,
        "a cancelled start is reaped"
    );
    assert_eq!(spawned.get(), 1);
    drop(launcher);
    store.reconcile_with_observer(
        run.attempt_id,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Cancelled,
            ..
        }
    ));
    Ok(())
}

fn starting_store() -> std::result::Result<
    (
        tempfile::TempDir,
        RunStore,
        provenant_core::AttemptId,
        AttemptId,
    ),
    Box<dyn std::error::Error>,
> {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    Ok((temp, store, run.attempt_id, id))
}

#[test]
fn a_live_launcher_fences_unlaunched_reconciliation_until_owner_exits()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let (_temp, store, _run, id) = starting_store()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let original = store.record(id, &call)?;
    let attempt = store.attempt_dir(id, false)?;
    let launcher = attempt.try_lock(
        std::ffi::OsStr::new("launcher.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;

    assert_eq!(
        store.settle_unlaunched(id, &original, &call)?,
        Reconciled::Held
    );
    assert_eq!(store.record(id, &call)?, original);

    drop(launcher);
    assert_eq!(
        store.settle_unlaunched(id, &original, &call)?,
        Reconciled::Withdrawn
    );
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    Ok(())
}

#[test]
fn cancelling_an_attempt_that_never_launched_settles_it_before_returning()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let (_temp, store, _run, id) = starting_store()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Cancelled);
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Cancelled,
            ..
        }
    ));
    assert_eq!(
        store.cancel(id, "stop", &call)?,
        CancelReceipt::AlreadyTerminal
    );
    Ok(())
}

#[test]
fn a_starting_attempt_whose_launcher_died_is_reaped_and_settles()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let (_temp, store, run, id) = starting_store()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let attempt = store.attempt_dir(id, false)?;
    let spawn = |_: &[std::ffi::OsString], _: &std::path::Path| Ok(());
    // A live launcher holds the attempt's writer lock.
    let launcher = attempt.lock(
        std::ffi::OsStr::new("writer.lock"),
        provenant_os::fs::LockMode::Exclusive,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    assert_eq!(store.reap_with(&call, spawn)?, 0, "a live launcher waits");
    drop(launcher);
    assert_eq!(
        store.reap_with(&call, spawn)?,
        1,
        "a dead launcher is reaped"
    );
    store.reconcile_with_observer(
        run,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Terminal { .. }
    ));
    Ok(())
}

#[test]
fn reaping_settles_a_launcher_lost_before_any_writer_lock_exists()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let (_temp, store, run, id) = starting_store()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let attempt = store.attempt_dir(id, false)?;
    assert!(
        matches!(attempt.metadata_at(std::ffi::OsStr::new("writer.lock"), false),
            Err(error) if error.is_not_found())
    );
    let launcher = attempt.try_lock(
        std::ffi::OsStr::new("launcher.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    assert_eq!(store.reap_with(&call, |_, _| Ok(()))?, 0);
    drop(launcher);
    assert_eq!(store.reap_with(&call, |_, _| Ok(()))?, 1);
    store.reconcile_with_observer(
        run,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    Ok(())
}

#[test]
fn reaping_settles_an_old_start_with_no_launcher_or_writer_lock()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let (_temp, store, run, id) = starting_store()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let attempt = store.attempt_dir(id, false)?;
    for name in ["launcher.lock", "writer.lock"] {
        assert!(
            matches!(attempt.metadata_at(std::ffi::OsStr::new(name), false),
                Err(error) if error.is_not_found())
        );
    }
    assert_eq!(store.reap_with(&call, |_, _| Ok(()))?, 1);
    store.reconcile_with_observer(
        run,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    Ok(())
}

#[test]
fn a_handed_off_launcher_is_not_reaped_as_an_unlaunched_attempt()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let (_temp, store, _run, id) = starting_store()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let attempt = store.attempt_dir(id, false)?;
    // The launcher no longer holds writer.lock after handing custody to
    // the guardian. Its durable launch evidence rules out prelaunch death.
    drop(attempt.try_lock(
        std::ffi::OsStr::new("writer.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?);
    crate::artifacts::seal(&attempt, "launch.json", b"{}", RECORD_LIMIT)?;
    assert!(!store.launcher_gone(&store.record(id, &call)?, &attempt, true, &call));
    assert_eq!(
        store.reap_with(&call, |_, _| panic!(
            "handed-off launcher must not be reaped"
        ))?,
        0
    );
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Starting { .. }
    ));
    Ok(())
}

#[test]
fn launcher_death_is_proved_in_its_worktree_fence()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let (_temp, store, run, id) = starting_store()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let record = store.record(id, &call)?;
    let attempt = store.attempt_dir(id, false)?;
    let slot = store
        .writer_registry
        .join("worktrees")
        .join(record.project_id.to_string())
        .join("lane");
    std::fs::create_dir_all(&slot)?;
    let held = provenant_supervisor::fixture::acquire_writer(
        provenant_os::fs::Directory::open_anchor(&slot)?,
        RunStore::scope(&record),
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    assert_eq!(store.writer_fences()?.len(), 1);
    // The attempt itself has no lock; only its fenced slot can prove life.
    assert!(!store.launcher_gone(&record, &attempt, true, &call));
    assert_eq!(
        store.reap_with(&call, |_, _| panic!("live fence holder must not be reaped"))?,
        0
    );
    drop(held);
    assert!(store.launcher_gone(&record, &attempt, false, &call));
    assert_eq!(store.reap_with(&call, |_, _| Ok(()))?, 1);
    store.reconcile_with_observer(
        run,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    assert!(store.writer_fences()?.is_empty());
    Ok(())
}

#[test]
fn a_running_attempt_without_process_checkpoint_stays_unknown()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Spec: docs/specs/supervisor.md#read-only-writer-presence
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let record = store.record(id, &call)?;
    assert_eq!(
        attempt_presence(context.state_root.as_path(), &record),
        Some(AttemptPresence::Unknown),
        "a missing process checkpoint cannot prove that the whole writer tree is gone"
    );
    assert!(!holder_absent(context.state_root.as_path(), &record));
    let spawned = std::cell::Cell::new(0);
    let reaped = store.reap_with(&call, |_, _| {
        spawned.set(spawned.get() + 1);
        Ok(())
    })?;
    // docs/specs/run-settlement.md#guardian-loss-proof: an unknown holder is
    // queued for recovery, which settles only on positive proof of loss;
    // the presence itself stays unknown and never reads as absent.
    assert_eq!((reaped, spawned.get()), (1, 1));
    assert!(!holder_absent(context.state_root.as_path(), &record));
    Ok(())
}

#[test]
fn a_sealed_exit_stays_settling_when_the_shared_census_is_unavailable()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Spec: docs/specs/supervisor.md#read-only-writer-presence
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let record = store.record(id, &call)?;
    let attempt = provenant_os::fs::Directory::open_anchor(
        &context.state_root.as_path().join(format!("attempts/{id}")),
    )?;
    crate::artifacts::seal(&attempt, "process-exit.json", b"settled exit", RECORD_LIMIT)?;

    assert_eq!(
        attempt_presence_with_unavailable_census(context.state_root.as_path(), &record),
        Some(AttemptPresence::Settling),
        "a failed shared census must not mask durable exit proof"
    );
    Ok(())
}

#[test]
fn tick_reaping_queues_settling_and_unattended_but_never_live_attempts() -> TestResult {
    // Spec: docs/specs/supervisor.md#tick-driven-settlement
    use provenant_supervisor::census::{
        Census, CensusError, ProcessFact, ProcessIdentity, ProcessObservation,
    };
    struct Observer(ProcessFact, Census);
    impl ProcessObserver for Observer {
        fn boot_identity(&self) -> std::result::Result<String, CensusError> {
            Ok(self.0.identity.boot.clone())
        }
        fn observe(&self, _: i64) -> ProcessObservation {
            ProcessObservation::Present(self.0.clone())
        }
        fn census(&self, _: usize) -> std::result::Result<Census, CensusError> {
            panic!("tick must reuse its shared census")
        }
    }
    let fact = ProcessFact {
        identity: ProcessIdentity {
            boot: "fixture-current-boot".into(),
            pid: 123,
            native_start: 1,
            group_id: 123,
            session_id: 123,
        },
        parent_pid: 1,
        zombie: false,
        cpu_micros: None,
        command: None,
    };
    let census = Census::from_observations([fact.clone()], [], [], true, false, 64)?;
    let observer = Observer(fact.clone(), census);
    for (presence, expected_spawns) in [
        (AttemptPresence::Settling, 1),
        (AttemptPresence::Unattended, 1),
        (AttemptPresence::Live, 0),
        (AttemptPresence::Unknown, 1),
    ] {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let record = store.record(id, &call)?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        if presence == AttemptPresence::Settling {
            crate::artifacts::seal(&attempt, "process-exit.json", b"{}", RECORD_LIMIT)?;
        } else if presence != AttemptPresence::Unknown {
            let mut identity = fact.identity.clone();
            if presence == AttemptPresence::Unattended {
                identity.boot = "fixture-previous-boot".into();
            }
            crate::artifacts::seal(
                &attempt,
                "process.json",
                &serde_json::to_vec(&serde_json::json!({
                    "schema_version": 1, "scope": scope, "guardian": identity,
                    "publication_pending": false, "census_complete": true,
                }))?,
                RECORD_LIMIT,
            )?;
        }
        assert_eq!(
            attempt_presence_with_observer_and_census(
                context.state_root.as_path(),
                &record,
                &observer,
                &observer.1
            ),
            Some(presence)
        );
        let spawned = std::cell::Cell::new(0);
        for _ in 0..2 {
            store.reap_with_census_and_spawn(&call, &observer, Some(&observer.1), |_, _| {
                spawned.set(spawned.get() + 1);
                Ok(())
            })?;
        }
        assert_eq!(
            spawned.get(),
            expected_spawns,
            "{presence:?}: repeated ticks coalesce workers"
        );
    }
    Ok(())
}

#[test]
fn only_a_running_attempt_past_its_deadline_and_grace_is_overdue()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let deadline = provenant_core::Clock::wall_now(&clock)?;
    let mut record: AttemptRecord = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "attempt_id": provenant_core::AttemptId::from_bytes([1; 16]),
        "key": {
            "project": provenant_core::ProjectId::from_bytes([3; 16]),
            "graph": "overdue", "node": "test", "attempt": 1
        },
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "attempt_number": 1, "revision": 1, "generation": 0,
        "requested_route": {"kind":"model", "provider":"p", "model":"m"},
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": deadline,
        "state": {"state":"running"},
        "usage": {}, "trace": {},
        "read_only_writable": [], "protected_paths": [],
        "prompt": {"path":{"encoding":"utf8", "path":"prompt.txt"},
            "digest": provenant_records::CanonicalV1::file_digest(b""), "bytes":0, "complete":true, "redacted":false}
    }))?;
    record.runtime_deadline = Some(deadline);
    let overdue = |clock: &provenant_core::TestClock, record: &AttemptRecord| {
        past_runtime_deadline(record, &crate::tests::call(clock, &cancel))
    };
    assert!(
        !overdue(&clock, &record),
        "at the deadline the guardian acts"
    );
    clock.advance(provenant_core::DurationMicros::from_seconds(
        DEADLINE_GRACE_SECONDS - 1,
    )?)?;
    assert!(!overdue(&clock, &record));
    clock.advance(provenant_core::DurationMicros::from_seconds(1)?)?;
    assert!(overdue(&clock, &record));
    record.runtime_deadline = None;
    assert!(!overdue(&clock, &record), "no recorded deadline, no stop");
    record.runtime_deadline = Some(deadline);
    record.state = AttemptState::Starting {
        extra: Default::default(),
    };
    assert!(!overdue(&clock, &record));
    Ok(())
}

#[derive(Clone, Copy)]
enum RebootEvidence {
    UnreadableExit,
    CorruptReservation,
}

fn unavailable_reboot_evidence(evidence: RebootEvidence) -> TestResult {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let path = context.state_root.as_path().join(format!("attempts/{id}"));
    let evidence_dir = path.join(id.to_string());
    std::fs::create_dir_all(&evidence_dir)?;
    let reservation = evidence_dir.join("reservation.json");
    let cause = match evidence {
        RebootEvidence::CorruptReservation => {
            std::fs::write(&reservation, b"corrupt reservation")?;
            RebootProofCause::Other
        }
        RebootEvidence::UnreadableExit => {
            std::fs::create_dir(path.join("process-exit.json"))?;
            RebootProofCause::Filesystem
        }
    };
    let before = std::fs::read(&reservation).ok();
    assert!(matches!(store.settle_rebooted(
            &store.record(id, &call)?,
            &call,
            &SystemProcessObserver
        )?, Some(Reconciled::Uncertain { reason: UncertainReason::RebootProofUnavailable { cause: found, .. } })
            if found == cause));
    assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Requested);
    let request: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.join("stop-intent.json"))?)?;
    assert_eq!(
        request["scope"],
        serde_json::to_value(RunStore::scope(&store.record(id, &call)?))?
    );
    assert!(matches!(store.settle_rebooted(
            &store.record(id, &call)?,
            &call,
            &SystemProcessObserver
        )?, Some(Reconciled::Uncertain { reason: UncertainReason::RebootProofUnavailable { cause: found, .. } })
            if found == cause));
    assert_eq!(std::fs::read(&reservation).ok(), before);
    assert_eq!(
        store.record(id, &call)?.state,
        AttemptState::Running {
            extra: Default::default(),
            child_wait: None,
        }
    );
    Ok(())
}
