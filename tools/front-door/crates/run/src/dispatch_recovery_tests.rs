use super::*;

#[test]
fn reconcile_recovers_writer_when_device_numbers_change_after_reboot() -> TestResult {
    recover_rebooted_writer_inner(false, false, true, NEW_BOOT, false, true, false)
}
#[test]
fn reconcile_rebooted_slot_handoff_after_device_numbers_change() -> TestResult {
    let observer = RebootObserver(NEW_BOOT);
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = input();
    let worktree = temp.path().join("lane");
    std::fs::create_dir(&worktree)?;
    std::fs::write(worktree.join("unfinished.rs"), "keep my work")?;
    run.request.task.writer = Some(WriterRequest {
        extra: Default::default(),
        project: temp.path().to_path_buf(),
        worktree: worktree.clone(),
    });
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store
        .claim(id, &at_micros(100), &call)?
        .ok_or("not admitted")?;
    let scope = RunStore::scope(&store.record(id, &call)?);
    let attempt = store.attempt_dir(id, false)?;
    let custody = store
        .writer_registry
        .join("worktrees")
        .join(context.project_id.to_string())
        .join(provenant_confinement::worktrees::state_dir_name(&worktree));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&custody)?;
    let slot = Directory::open_anchor(&custody)?;
    drop(provenant_supervisor::fixture::acquire_writer(
        slot.try_clone()?,
        scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    let child_reservation = custody.join(id.to_string()).join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&child_reservation)?)?;
    let old_device = slot.metadata()?.device + 1;
    value["boot"] = OLD_BOOT.into();
    value["launch_state"] = "handing_off".into();
    value["reservation_device"] = old_device.into();
    value["reservation_inode"] = slot.metadata()?.inode.into();
    value["attempt_device"] = old_device.into();
    value["attempt_inode"] = attempt.metadata()?.inode.into();
    std::fs::write(
        custody.join("reservation.json"),
        serde_json::to_vec(&value)?,
    )?;
    std::fs::remove_file(&child_reservation)?;

    assert_eq!(
        store.reconcile_with_observer(run.attempt_id, &Fake::default(), &call, &observer)?,
        vec![(id, Reconciled::Finished)]
    );
    assert_eq!(terminal(&store, id, &call)?, Outcome::Lost);
    assert!(store.writer_fences()?.is_empty());
    assert!(!custody.join("reservation.json").exists());
    assert!(!child_reservation.exists());
    assert_eq!(
        std::fs::read_to_string(worktree.join("unfinished.rs"))?,
        "keep my work"
    );
    assert!(artifacts::read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_none());
    Ok(())
}
#[test]
fn changed_device_numbers_on_same_boot_keep_writer_custody() -> TestResult {
    recover_rebooted_writer_inner(false, false, true, OLD_BOOT, false, true, false)
}
#[test]
fn reconcile_settles_a_normal_terminal_attempt_after_reboot() -> TestResult {
    let observer = RebootObserver(NEW_BOOT);
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let attempt = store.attempt_dir(id, false)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    let record = store.finish(id, &attempt, &observed, &Fake::default(), &call)?;
    let path = context.state_root.as_path().join(format!("attempts/{id}"));
    // Recreate the reservation retained by a crash after the canonical commit.
    drop(provenant_supervisor::fixture::acquire_writer(
        attempt.try_clone()?,
        observed.scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    let reservation = path
        .join(observed.scope.attempt.to_string())
        .join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let meta = attempt.metadata()?;
    value["boot"] = "darwin:00000000-0000-0000-0000-000000000000".into();
    value["guardian"] = serde_json::json!({"boot":value["boot"], "pid":123, "native_start":1, "group_id":123, "session_id":123});
    value["launch_state"] = "spawned".into();
    value["attempt_device"] = meta.device.into();
    value["attempt_inode"] = meta.inode.into();
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    artifacts::subdir(&attempt, std::ffi::OsStr::new("scratch"), true)?;
    // Recreate the private tmp root with production permissions, independent of umask.
    artifacts::subdir(&attempt, std::ffi::OsStr::new("tmp"), true)?;
    let temporary = path.join("tmp/candidate-0");
    std::fs::create_dir_all(&temporary)?;
    std::fs::write(temporary.join("payload.lock"), b"build output")?;
    store.set_temporary_retained(id, true, &call)?;
    let manifest_path = path.join("quarantine/manifest.json");
    let manifest = std::fs::read(&manifest_path)?;
    std::fs::write(&manifest_path, b"invalid manifest")?;
    assert!(
        matches!(store.reconcile_with_observer(run.attempt_id, &Fake::default(), &call, &observer)?.as_slice(),
        [(attempt, Reconciled::Uncertain { .. })] if *attempt == id)
    );
    assert!(
        reservation.exists(),
        "failed verification retains the reservation"
    );
    assert!(temporary.exists(), "uncertain custody retains tmp");
    assert_eq!(store.record(id, &call)?, record);
    std::fs::write(manifest_path, manifest)?;
    assert_eq!(
        store.reconcile_with_observer(run.attempt_id, &Fake::default(), &call, &observer)?,
        vec![(id, Reconciled::Finished)]
    );
    let cleanup = controls::REBOOT_TEMPORARY_CLEANUP.with(|result| result.borrow_mut().take());
    assert_eq!(cleanup, Some(Ok(Some(Reconciled::TemporaryRemoved))));
    assert!(
        !path.join("scratch").exists(),
        "normal settlement removes empty launch staging"
    );
    assert!(
        path.join("release.json").exists(),
        "normal settlement verifies release evidence"
    );
    assert!(!reservation.exists());
    assert!(!path.join("tmp").exists(), "released tmp is removed");
    assert_eq!(store.record(id, &call)?, record);
    Ok(())
}
#[test]
fn cancel_recovers_a_writer_from_a_previous_boot() -> TestResult {
    recover_rebooted_writer(true, false, true, NEW_BOOT)
}
#[test]
fn reconcile_retries_reboot_release_after_the_terminal_commit() -> TestResult {
    recover_rebooted_writer(false, true, true, NEW_BOOT)
}
#[test]
fn cancel_retries_reboot_release_after_the_terminal_commit() -> TestResult {
    recover_rebooted_writer(true, true, true, NEW_BOOT)
}
#[test]
fn reconcile_retries_reboot_release_for_a_reader() -> TestResult {
    recover_rebooted_writer(false, true, false, NEW_BOOT)
}
#[test]
fn unreserved_cancel_keeps_unidentified_provider_uncertain() -> TestResult {
    placed_writer_without_reservation(true, 0)
}
#[test]
fn unreserved_reconcile_keeps_unidentified_provider_uncertain() -> TestResult {
    placed_writer_without_reservation(false, 0)
}
#[test]
fn unreserved_live_guardian_keeps_attempt_open() -> TestResult {
    placed_writer_without_reservation(true, 1)?;
    placed_writer_without_reservation(false, 1)
}
#[test]
fn unreserved_live_provider_keeps_attempt_open() -> TestResult {
    placed_writer_without_reservation(true, 3)?;
    placed_writer_without_reservation(false, 3)
}
#[test]
fn unreserved_recorded_process_tree_before_reboot_is_lost() -> TestResult {
    placed_writer_without_reservation(true, 2)?;
    placed_writer_without_reservation(false, 2)
}
#[test]
fn stale_reservation_before_reboot_is_reclaimed() -> TestResult {
    for cancel_run in [true, false] {
        placed_writer_with_missing_or_stale_reservation(RecoveryCase {
            cancel_run,
            evidence: 2,
            reservation: ReservationCase::Stale,
            removed: false,
        })?;
    }
    Ok(())
}
#[test]
fn removed_worktree_before_reboot_is_lost() -> TestResult {
    for cancel_run in [true, false] {
        placed_writer_with_missing_or_stale_reservation(RecoveryCase {
            cancel_run,
            evidence: 2,
            reservation: ReservationCase::Missing,
            removed: true,
        })?;
    }
    recover_rebooted_writer_removed_worktree()
}
#[test]
fn stale_reservation_with_current_boot_process_is_retained() -> TestResult {
    placed_writer_with_missing_or_stale_reservation(RecoveryCase {
        cancel_run: false,
        evidence: 1,
        reservation: ReservationCase::Stale,
        removed: false,
    })
}
#[test]
fn newer_generation_reservation_is_retained() -> TestResult {
    placed_writer_with_missing_or_stale_reservation(RecoveryCase {
        cancel_run: false,
        evidence: 2,
        reservation: ReservationCase::Newer,
        removed: false,
    })
}
#[test]
fn different_attempt_reservation_is_retained() -> TestResult {
    placed_writer_with_missing_or_stale_reservation(RecoveryCase {
        cancel_run: false,
        evidence: 2,
        reservation: ReservationCase::DifferentAttempt,
        removed: false,
    })
}
#[test]
fn same_boot_with_no_live_holder_settles_lost() -> TestResult {
    recover_rebooted_writer(false, false, true, OLD_BOOT)
}
#[test]
fn same_boot_with_unresolved_custody_stays_uncertain() -> TestResult {
    recover_rebooted_writer_inner(false, false, true, OLD_BOOT, false, false, true)
}
#[test]
fn cancel_settles_an_attempt_with_no_live_holder() -> TestResult {
    recover_rebooted_writer(true, false, true, OLD_BOOT)
}
#[test]
fn reader_with_no_live_holder_settles_lost() -> TestResult {
    recover_rebooted_writer(false, false, false, OLD_BOOT)
}
#[test]
fn reconcile_sealed_never_spawned_exit_without_a_stop_request() -> TestResult {
    use provenant_supervisor::machine::StopReason;
    for (reason, expected) in [
        (None, Outcome::Rejected),
        (Some(StopReason::WallDeadline), Outcome::TimedOut),
        (Some(StopReason::ExplicitCancellation), Outcome::Cancelled),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let record = store.record(id, &call)?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let mut observed = exit();
        observed.scope = scope;
        observed.never_spawned = true;
        observed.termination = None;
        observed.stop_reason = reason;
        observed.settlement_evidence =
            provenant_supervisor::custody::SettlementEvidence::NeverSpawned;
        seal_exit(&store, &observed)?;
        artifacts::seal(
            &attempt,
            "launch.json",
            &serde_json::to_vec(
                &serde_json::json!({"schema_version": 1, "scope": scope, "requester": context.actor_id}),
            )?,
            RECORD_LIMIT,
        )?;
        assert_eq!(
            store.reconcile_with_observer(
                run.attempt_id,
                &Fake::default(),
                &call,
                &provenant_supervisor::census::SystemProcessObserver
            )?,
            vec![(id, Reconciled::Finished)]
        );
        assert_eq!(terminal(&store, id, &call)?, expected);
        assert_eq!(store.record(id, &call)?.provider_launched, Some(false));
    }
    Ok(())
}

#[test]
fn reconcile_finishes_a_durable_exit_and_withdraws_a_launch_that_never_spawned() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
        read_only_spacing_ms: 0,
        ..AdmissionPolicy::default()
    });
    let (first, second) = (input(), lane(8, false));
    store.create(&first, &call)?;
    store.create(&second, &call)?;
    let [exited, orphan] = [first.attempt_id, second.attempt_id];

    // The guardian sealed an exit but died before its terminal commit.
    store.mark_starting(
        &store.record(exited, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store
        .claim(exited, &at_micros(100), &call)?
        .ok_or("not admitted")?;
    let dir = store.attempt_dir(exited, false)?;
    artifacts::seal(&dir, "stdout.log", DONE_ANSWER.as_bytes(), CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope.attempt = exited;
    observed.scope.generation = 1;
    let summary = serde_json::json!({"schema_version": 1, "scope": observed.scope, "requester": provenant_core::ActorId::from_bytes([2; 16])});
    artifacts::seal(
        &dir,
        "launch.json",
        &serde_json::to_vec(&summary)?,
        RECORD_LIMIT,
    )?;
    seal_exit(&store, &observed)?;

    // A handoff whose reservation never reached a guardian.
    store.mark_starting(
        &store.record(orphan, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let scope = provenant_supervisor::custody::Scope {
        attempt: orphan,
        generation: 1,
    };
    drop(provenant_supervisor::fixture::acquire_writer(
        store.attempt_dir(orphan, false)?,
        scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?);

    let fake = Fake {
        observed: Some(route()),
        ..Fake::default()
    };
    let observer = provenant_supervisor::census::SystemProcessObserver;
    assert_eq!(
        store.reconcile_with_observer(exited, &fake, &call, &observer)?,
        vec![(exited, Reconciled::Finished)]
    );
    assert_eq!(
        store.reconcile_with_observer(orphan, &fake, &call, &observer)?,
        vec![(orphan, Reconciled::Withdrawn)]
    );
    assert_eq!(terminal(&store, exited, &call)?, Outcome::Succeeded);
    assert!(artifacts::read_sealed(&dir, "release.json", RECORD_LIMIT)?.is_some());
    assert_eq!(terminal(&store, orphan, &call)?, Outcome::Rejected);
    for id in [exited, orphan] {
        assert!(
            store
                .reconcile_with_observer(id, &fake, &call, &observer)?
                .is_empty()
        );
    }
    Ok(())
}
#[test]
fn reconcile_settles_a_writer_attempt_whose_worktree_was_removed() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = input();
    run.request.task.writer = Some(WriterRequest {
        extra: Default::default(),
        project: temp.path().join("checkout"),
        worktree: temp.path().join("removed-lane"),
    });
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store
        .claim(id, &at_micros(100), &call)?
        .ok_or("not admitted")?;
    let attempt = store.attempt_dir(id, false)?;
    let scope = provenant_supervisor::custody::Scope {
        attempt: id,
        generation: 1,
    };
    // The guardian reserved the writer and sealed an exit; its worktree is gone.
    let custody = context
        .state_root
        .as_path()
        .join("worktrees")
        .join(context.project_id.to_string())
        .join("lane");
    std::fs::create_dir_all(&custody)?;
    drop(provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&custody)?,
        scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    let meta = attempt.metadata()?;
    let reservation = custody
        .join(scope.attempt.to_string())
        .join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let fields = value.as_object_mut().ok_or("reservation")?;
    fields.insert("launch_state".into(), "never_spawned".into());
    fields.insert("unresolved_cleanup".into(), false.into());
    fields.insert("census_incomplete".into(), false.into());
    fields.insert("attempt_device".into(), meta.device.into());
    fields.insert("attempt_inode".into(), meta.inode.into());
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    artifacts::seal(&attempt, "stdout.log", b"answer", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = scope;
    let summary = serde_json::json!({"schema_version": 1, "scope": scope, "requester": provenant_core::ActorId::from_bytes([2; 16])});
    artifacts::seal(
        &attempt,
        "launch.json",
        &serde_json::to_vec(&summary)?,
        RECORD_LIMIT,
    )?;
    seal_exit(&store, &observed)?;

    let fake = Fake {
        observed: Some(route()),
        ..Fake::default()
    };
    let found = store.reconcile_with_observer(
        run.attempt_id,
        &fake,
        &call,
        &provenant_supervisor::census::SystemProcessObserver,
    )?;
    assert_eq!(found, vec![(id, Reconciled::Finished)]);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Lost);
    assert!(store.writer_fences()?.is_empty());
    assert!(
        store
            .reconcile_with_observer(
                run.attempt_id,
                &fake,
                &call,
                &provenant_supervisor::census::SystemProcessObserver
            )?
            .is_empty()
    );
    Ok(())
}
#[test]
fn reconcile_withdraws_a_start_that_died_before_reserving() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let found = store.reconcile_with_observer(
        run.attempt_id,
        &Fake::default(),
        &call,
        &provenant_supervisor::census::SystemProcessObserver,
    )?;
    assert_eq!(found, vec![(id, Reconciled::Withdrawn)]);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Rejected);
    Ok(())
}
#[test]
fn cancelling_a_start_that_holds_only_a_writer_fence_frees_the_writer() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let custody = context
        .state_root
        .as_path()
        .join("worktrees")
        .join(context.project_id.to_string())
        .join("lane");
    std::fs::create_dir_all(&custody)?;
    let held = provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&custody)?,
        RunStore::scope(&store.record(id, &call)?),
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    assert_eq!(store.writer_fences()?.len(), 1);
    // The launcher is the holder: cancel asks it to stop and cannot yet settle.
    assert!(!matches!(
        store.cancel(id, "stop", &call)?,
        crate::CancelReceipt::Cancelled
    ));
    // Once the launcher has let go, a repeat cancel settles and frees the writer.
    drop(held);
    assert_eq!(
        store.cancel(id, "stop", &call)?,
        crate::CancelReceipt::Cancelled
    );
    assert_eq!(terminal(&store, id, &call)?, Outcome::Cancelled);
    assert!(store.writer_fences()?.is_empty());
    Ok(())
}
#[test]
fn repeat_cancel_while_the_launcher_holds_keeps_one_request_and_settles_on_release() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let slot = context
        .state_root
        .as_path()
        .join("worktrees")
        .join(context.project_id.to_string())
        .join("lane");
    std::fs::create_dir_all(&slot)?;
    // The launcher holds the worktree writer while it prepares the lane.
    let held = provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&slot)?,
        RunStore::scope(&store.record(id, &call)?),
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Requested);
    let attempt = store.attempt_dir(id, false)?;
    let first = artifacts::read_sealed(&attempt, "cancel.json", RECORD_LIMIT)?
        .ok_or("cancel request missing")?;
    // A repeat while the launcher still holds the writer neither settles the
    // lane nor writes a second request.
    assert_eq!(
        store.cancel(id, "stop", &call)?,
        CancelReceipt::AlreadyRequested
    );
    assert_eq!(
        artifacts::read_sealed(&attempt, "cancel.json", RECORD_LIMIT)?.as_deref(),
        Some(first.as_slice())
    );
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Starting { .. }
    ));
    assert_eq!(store.writer_fences()?.len(), 1);
    // Once the launcher has let go, the next repeat settles and frees the writer.
    drop(held);
    assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Cancelled);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Cancelled);
    assert!(store.writer_fences()?.is_empty());
    assert_eq!(
        store.cancel(id, "stop", &call)?,
        CancelReceipt::AlreadyTerminal
    );
    Ok(())
}
#[test]
fn a_cancelled_unlaunched_start_frees_the_worktree_writer_for_the_next_run() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let first = input();
    let first_id = first.attempt_id;
    store.create(&first, &call)?;
    store.mark_starting(
        &store.record(first_id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let slot = context
        .state_root
        .as_path()
        .join("worktrees")
        .join(context.project_id.to_string())
        .join("lane");
    std::fs::create_dir_all(&slot)?;
    let first_scope = RunStore::scope(&store.record(first_id, &call)?);
    let held = provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&slot)?,
        first_scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;

    let mut next = input();
    next.attempt_id = AttemptId::from_bytes([9; 16]);
    next.operation_id = provenant_core::OperationId::parse("run-test-next")?;
    next.attempt_id = AttemptId::from_bytes([10; 16]);
    crate::tests::rekey(&mut next);
    store.create(&next, &call)?;
    let next_id = next.attempt_id;
    store.mark_starting(
        &store.record(next_id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let next_scope = RunStore::scope(&store.record(next_id, &call)?);
    // The next run takes the writer the way a launch does: fenced, without waiting.
    let take_writer = |directory: Directory| {
        store.acquire_writer_fenced(
            directory,
            next_scope,
            provenant_supervisor::custody::WriterBinding::read_only(),
            &|directory, scope, binding| {
                provenant_supervisor::custody::acquire_writer(
                    directory,
                    scope,
                    binding,
                    provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                    call.cancel,
                    call.clock,
                )
            },
            &call,
        )
    };
    assert!(
        take_writer(Directory::open_anchor(&slot)?).is_err(),
        "a live launcher still holds the writer"
    );
    // The first launcher dies before the provider launches; cancel settles
    // the attempt and frees its writer and fence before returning.
    drop(held);
    assert_eq!(
        store.cancel(first_id, "stop", &call)?,
        CancelReceipt::Cancelled
    );
    assert_eq!(terminal(&store, first_id, &call)?, Outcome::Cancelled);
    assert!(store.writer_fences()?.is_empty());
    let lease = match take_writer(Directory::open_anchor(&slot)?) {
        Ok(lease) => lease,
        Err(_) => return Err("the next run was refused after the cancel".into()),
    };
    drop(lease);
    Ok(())
}
#[test]
fn read_only_exceptions_are_recorded_and_temporary_paths_are_private_per_attempt() -> TestResult {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let (prepared, other) = (input(), lane(8, false));
    store.create(&prepared, &call)?;
    store.create(&other, &call)?;
    let first = prepared.attempt_id;
    let second = other.attempt_id;
    let first_tmp = store.temporary_path(first, &call)?;
    let second_tmp = store.temporary_path(second, &call)?;
    assert_ne!(first_tmp, second_tmp);
    assert!(
        first_tmp.starts_with(
            context
                .state_root
                .as_path()
                .join("attempts")
                .join(first.to_string())
        )
    );
    assert_eq!(
        std::fs::metadata(&first_tmp)?.permissions().mode() & 0o777,
        0o700
    );
    let observed = provenant_core::GitOid::Sha1([7; 20]);
    let write_grants = vec![
        WriteGrantRecord {
            extra: Default::default(),
            source: WriteSource::WriteRoot,
            path: provenant_records::AbsolutePath::new(
                &context.state_root.as_path().join("shared"),
            )?,
            scope: PathScope::Subtree,
        },
        WriteGrantRecord {
            extra: Default::default(),
            source: WriteSource::StateWrite,
            path: provenant_records::AbsolutePath::new(
                &context
                    .state_root
                    .as_path()
                    .join(std::ffi::OsString::from_vec(b"session-\xff".to_vec())),
            )?,
            scope: PathScope::Exact,
        },
    ];
    store.mark_starting(
        &store.record(first, &call)?,
        &route(),
        &[".agent-run/locks".into()],
        &admission::AppliedWriteGrants {
            grants: write_grants.clone(),
            git_grants_digest: Some(provenant_core::Digest::from_bytes([7; 32])),
        },
        Some(observed),
        &call,
    )?;
    drop(store);
    let store = RunStore::open(&context, &call)?;
    assert_eq!(
        store.record(first, &call)?.read_only_writable,
        [".agent-run/locks"]
    );
    assert!(store.record(second, &call)?.read_only_writable.is_empty());
    assert_eq!(store.record(first, &call)?.write_grants, write_grants);
    assert!(store.record(second, &call)?.write_grants.is_empty());
    // The host-observed head of a read-only lane persists; unobserved stays absent.
    assert_eq!(store.record(first, &call)?.head, Some(observed));
    assert_eq!(store.record(second, &call)?.head, None);
    Ok(())
}
