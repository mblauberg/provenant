use super::*;

#[test]
fn fresh_resume_preserves_lineage_and_authority_without_provider_state() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut original = input();
    original.request.chair = Some(ChairBinding {
        extra: Default::default(),
        session: None,
        host: provenant_core::HostId::from_bytes([32; 16]),
        actor: provenant_core::ActorId::from_bytes([33; 16]),
    });
    store.create(&original, &call)?;
    let fake = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start(original.run_id, &fake, &|_, _| unreachable!(), &call)?;
    let prior = original.attempts[0].attempt_id;
    let mut resume = Resume {
        extend: Vec::new(),
        prior,
        expected_generation: 0,
        operation_id: "fresh-resume".parse()?,
        run_id: RunId::from_bytes([90; 16]),
        attempt_id: AttemptId::from_bytes([91; 16]),
        prompt: "Original prompt\nPrior result tail\nNew prompt".into(),
        created_at: WallTime::from_micros(100)?,
    };
    let run = store.resume_fresh(&resume, "retained Codex rollout missing", &call)?;
    assert_eq!(
        store.resume_fresh(&resume, "retained Codex rollout missing", &call)?,
        run
    );
    let request = store.request(run.run_id, &call)?;
    assert_eq!(request.parent, Some(prior));
    assert_eq!(request.chair, original.request.chair);
    assert_eq!(
        request.tasks[0].authority_digest,
        original.request.tasks[0].authority_digest
    );
    assert_eq!(request.tasks[0].writer, original.request.tasks[0].writer);
    assert_eq!(request.tasks[0].route, original.request.tasks[0].route);
    assert_eq!(
        request.fresh_continuation.as_deref(),
        Some("retained Codex rollout missing")
    );
    store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
    assert_eq!(fake.resumed.get(), None);
    let record = store.record(resume.attempt_id, &call)?;
    assert_eq!(record.retry_of, Some(prior));
    assert_eq!(record.continuation.as_deref(), Some("fresh"));
    resume.prompt = "different".into();
    assert!(matches!(
        store.resume_fresh(&resume, "retained Codex rollout missing", &call),
        Err(RunError::OperationConflict)
    ));
    Ok(())
}
#[test]
fn cancel_settles_a_prepared_attempt_nobody_holds() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let batch = two_tasks(1);
    store.create(&batch, &call)?;
    let orphan = batch.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(orphan, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let scope = provenant_supervisor::custody::Scope {
        run: batch.run_id,
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
    let system = provenant_os::clock::SystemClock::new();
    let spent = Request {
        clock: &system,
        cancel: &cancel,
    };
    // Nobody holds the reservation, so cancel settles it before returning
    // and the writer is free for the next run.
    assert_eq!(
        store.cancel(orphan, "stop", &spent)?,
        CancelReceipt::Cancelled
    );
    assert_eq!(terminal(&store, orphan, &call)?, Outcome::Cancelled);
    assert!(store.writer_fences()?.is_empty());
    assert_eq!(
        store.cancel(orphan, "stop", &spent)?,
        CancelReceipt::AlreadyTerminal
    );
    Ok(())
}
#[test]
fn reconcile_unreadable_reservation_only_retains_existing_temporary_data() -> TestResult {
    for has_temporary in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = two_tasks(2);
        store.create(&run, &call)?;
        let terminal = run.attempts[0].attempt_id;
        let sibling = run.attempts[1].attempt_id;
        let temporary = store.temporary_path(terminal, &call)?;
        if has_temporary {
            std::fs::write(temporary.join("keep.lock"), "keep")?;
        }
        store.withdraw(
            terminal,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(150)?,
            &call,
        )?;
        if !has_temporary {
            // A crash after removal can leave the cleanup hint uncleared.
            store.set_temporary_retained(terminal, true, &call)?;
        }
        let custody = context
            .state_root
            .as_path()
            .join("worktrees")
            .join(context.project_id.to_string())
            .join("corrupt")
            .join(terminal.to_string());
        std::fs::create_dir_all(&custody)?;
        std::fs::write(custody.join("reservation.json"), b"corrupt")?;
        assert_eq!(store.writer_fence_listing()?.issues.len(), 1);

        let found = store.reconcile_with_observer(
            run.run_id,
            &Fake::default(),
            &call,
            &provenant_supervisor::census::SystemProcessObserver,
        )?;
        if has_temporary {
            assert!(matches!(found.as_slice(), [
                (id, Reconciled::TemporaryRetained { path, reason }),
                (other, Reconciled::Queued),
            ] if *id == terminal && *other == sibling && *path == temporary
                && reason.kind == TemporaryReasonKind::ReservationUnreadable));
            assert!(temporary.join("keep.lock").exists());
        } else {
            assert_eq!(found, vec![(sibling, Reconciled::Queued)]);
        }
    }
    Ok(())
}
#[test]
fn reconcile_retries_terminal_temporary_removal() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    let temporary = store.temporary_path(id, &call)?;
    // Settlement protects lock names while custody may still be retained.
    // Reconcile must dispose of these build artefacts once custody clears.
    let incremental = temporary.join("candidate-0/target/debug/incremental/x");
    std::fs::create_dir_all(&incremental)?;
    std::fs::write(incremental.join("s-1.lock"), "discard")?;
    let observed = exit();
    seal_exit(&store, &observed)?;
    store.publish_terminal(id, 1, &super::tests::publication(), &observed, &call)?;
    assert_eq!(terminal(&store, id, &call)?, Outcome::Succeeded);
    assert!(temporary.is_dir());

    assert_eq!(store.open_runs(10, &call)?, vec![run.run_id]);
    assert_eq!(
        store.reconcile_with_observer(
            run.run_id,
            &Fake::default(),
            &call,
            &provenant_supervisor::census::SystemProcessObserver
        )?,
        vec![(id, Reconciled::TemporaryRemoved)]
    );
    assert!(!temporary.exists());
    finish_reactions(&store)?;
    assert!(store.open_runs(10, &call)?.is_empty());
    assert_eq!(terminal(&store, id, &call)?, Outcome::Succeeded);
    Ok(())
}
#[test]
fn reconcile_preserves_protected_locks_outside_temporary() -> TestResult {
    use provenant_os::fs::FsError;
    use provenant_os::fs::LockMode;
    use std::ffi::OsStr;

    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    let temporary = store.temporary_path(id, &call)?;
    std::fs::write(temporary.join("writer.lock"), "disposable payload")?;
    store.withdraw(
        id,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Cancelled,
        "fixture",
        WallTime::from_micros(150)?,
        &call,
    )?;
    let attempt = store.attempt_dir(id, false)?;
    let run_dir = store.run_dir(run.run_id, false)?;
    drop(attempt.try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)?);
    // The run lock may stay held while reconcile removes attempt payload.
    let _held = run_dir.try_lock(OsStr::new("publication.lock"), LockMode::Exclusive)?;
    let locks = [(&attempt, "writer.lock"), (&run_dir, "publication.lock")];
    let before = locks
        .iter()
        .map(|(dir, name)| dir.metadata_at(OsStr::new(name), false))
        .collect::<provenant_os::fs::Result<Vec<_>>>()?;

    assert_eq!(
        store.reconcile_with_observer(
            run.run_id,
            &Fake::default(),
            &call,
            &provenant_supervisor::census::SystemProcessObserver
        )?,
        vec![(id, Reconciled::TemporaryRemoved)]
    );
    assert!(!temporary.exists());
    for ((dir, name), identity) in locks.into_iter().zip(before) {
        assert!(matches!(
            dir.unlink(OsStr::new(name)),
            Err(FsError::ReservedLockName { .. })
        ));
        assert!(identity.same_identity(&dir.metadata_at(OsStr::new(name), false)?));
    }
    assert!(matches!(
        run_dir.try_lock(OsStr::new("publication.lock"), LockMode::Exclusive),
        Err(FsError::LockTimeout { .. })
    ));
    Ok(())
}
#[test]
fn reconcile_reports_temporary_removal_failures_and_continues() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = two_tasks(2);
    store.create(&run, &call)?;
    for new in &run.attempts {
        let temporary = store.temporary_path(new.attempt_id, &call)?;
        std::fs::write(temporary.join("payload.lock"), "discard")?;
        store.withdraw(
            new.attempt_id,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(150)?,
            &call,
        )?;
    }
    let retained = run.attempts[0].attempt_id;
    let removed = run.attempts[1].attempt_id;
    let temporary = store.temporary_path(retained, &call)?;
    std::fs::write(temporary.join("payload"), "retain")?;
    let _held = store.attempt_dir(retained, false)?.try_lock(
        std::ffi::OsStr::new("writer.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let removable = store.temporary_path(removed, &call)?;
    std::fs::write(removable.join("payload"), "discard")?;

    let found = store.reconcile_with_observer(
        run.run_id,
        &Fake::default(),
        &call,
        &provenant_supervisor::census::SystemProcessObserver,
    )?;
    assert!(matches!(found.as_slice(), [
        (id, Reconciled::TemporaryRetained { path, reason }),
        (other, Reconciled::TemporaryRemoved),
    ] if *id == retained && *other == removed && *path == temporary
        && reason.contains("writer.lock")));
    assert!(temporary.join("payload").exists());
    assert!(!removable.exists());
    assert_eq!(terminal(&store, retained, &call)?, Outcome::Cancelled);
    Ok(())
}
#[test]
fn reconcile_leaves_nonterminal_and_reserved_temporary_data() -> TestResult {
    for reserved in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let temporary = store.temporary_path(id, &call)?;
        std::fs::write(temporary.join("keep.lock"), "keep")?;
        if reserved {
            store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            store.withdraw(
                id,
                AttemptState::Starting {
                    extra: Default::default(),
                },
                Outcome::Rejected,
                "fixture",
                WallTime::from_micros(150)?,
                &call,
            )?;
        }
        // Also cover a reservation retained after its owner releases the lock.
        if reserved {
            drop(provenant_supervisor::fixture::acquire_writer(
                store.attempt_dir(id, false)?,
                RunStore::scope(&store.record(id, &call)?),
                call.lock_budget(),
                &cancel,
                &clock,
            )?);
        }
        std::fs::write(temporary.join("keep.lock"), "keep")?;
        let found = store.reconcile_with_observer(
            run.run_id,
            &Fake::default(),
            &call,
            &provenant_supervisor::census::SystemProcessObserver,
        )?;
        if reserved {
            assert!(
                matches!(found.as_slice(), [(attempt, Reconciled::TemporaryRetained { reason, .. })]
                if *attempt == id && reason.kind == TemporaryReasonKind::ReservationRetained)
            );
        } else {
            assert_eq!(found, vec![(id, Reconciled::Queued)]);
        }
        assert_eq!(
            std::fs::read_to_string(temporary.join("keep.lock"))?,
            "keep"
        );
    }
    Ok(())
}
#[test]
fn reconcile_terminal_writer_reservation_without_temporary() -> TestResult {
    for prepared in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let temporary = store.temporary_path(id, &call)?;
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
        let _held = if prepared {
            drop(held);
            None
        } else {
            Some(held)
        };
        store.withdraw(
            id,
            AttemptState::Starting {
                extra: Default::default(),
            },
            Outcome::Rejected,
            "fixture",
            WallTime::from_micros(150)?,
            &call,
        )?;
        assert!(!temporary.exists());
        assert_eq!(store.open_runs(10, &call)?, vec![run.run_id]);
        let found = store.reconcile_with_observer(
            run.run_id,
            &Fake::default(),
            &call,
            &provenant_supervisor::census::SystemProcessObserver,
        )?;
        if prepared {
            assert_eq!(found, vec![(id, Reconciled::Withdrawn)]);
            assert!(store.writer_fences()?.is_empty());
            finish_reactions(&store)?;
            assert!(store.open_runs(10, &call)?.is_empty());
        } else {
            assert!(
                matches!(found.as_slice(), [(attempt, Reconciled::Uncertain { reason })]
                if *attempt == id && matches!(reason, UncertainReason::WriterReservationRetained { .. }))
            );
            assert_eq!(store.writer_fences()?.len(), 1);
            assert_eq!(store.open_runs(10, &call)?, vec![run.run_id]);
        }
        assert!(!temporary.exists());
    }
    Ok(())
}
#[test]
fn reconcile_leaves_temporary_data_behind_a_writer_reservation() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = input();
    run.request.tasks[0].writer = Some(WriterRequest {
        extra: Default::default(),
        project: temp.path().join("checkout"),
        worktree: temp.path().join("checkout/.worktrees/lane"),
    });
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    let temporary = store.temporary_path(id, &call)?;
    let candidate = temporary.join("candidate-0");
    std::fs::create_dir(&candidate)?;
    std::fs::write(candidate.join("x.lock"), "keep")?;
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
    store.withdraw(
        id,
        AttemptState::Starting {
            extra: Default::default(),
        },
        Outcome::Rejected,
        "fixture",
        WallTime::from_micros(150)?,
        &call,
    )?;
    assert_eq!(std::fs::read_to_string(candidate.join("x.lock"))?, "keep");
    // An unreadable sibling must not hide this attempt's readable reservation.
    let corrupt = custody.with_file_name("corrupt").join(id.to_string());
    std::fs::create_dir_all(&corrupt)?;
    std::fs::write(corrupt.join("reservation.json"), b"corrupt")?;
    assert_eq!(store.writer_fence_listing()?.issues.len(), 1);
    assert!(
        matches!(store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &provenant_supervisor::census::SystemProcessObserver)?.as_slice(),
        [(attempt, Reconciled::Uncertain { reason: UncertainReason::WriterReservationRetained { .. } })]
        if *attempt == id)
    );
    assert_eq!(std::fs::read_to_string(candidate.join("x.lock"))?, "keep");
    // Reconcile can withdraw the prepared reservation after its lock is released.
    drop(held);
    assert!(matches!(
        store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &provenant_supervisor::census::SystemProcessObserver)?.as_slice(),
        [(attempt, Reconciled::Withdrawn)] if *attempt == id
    ));
    assert!(store.writer_fences()?.is_empty());
    assert!(candidate.join("x.lock").exists());
    // The unreadable sibling remains a separate barrier until removed.
    assert!(matches!(
        store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &provenant_supervisor::census::SystemProcessObserver)?.as_slice(),
        [(attempt, Reconciled::TemporaryRetained { .. })] if *attempt == id
    ));
    assert!(candidate.join("x.lock").exists());
    std::fs::remove_file(corrupt.join("reservation.json"))?;
    assert_eq!(
        store.reconcile_with_observer(
            run.run_id,
            &Fake::default(),
            &call,
            &provenant_supervisor::census::SystemProcessObserver
        )?,
        vec![(id, Reconciled::TemporaryRemoved)]
    );
    assert!(!temporary.exists());
    Ok(())
}
#[test]
fn reconcile_temporary_removal_never_follows_symlinks() -> TestResult {
    use std::os::unix::fs::symlink;
    for link_at_root in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let temporary = store.temporary_path(id, &call)?;
        std::fs::write(temporary.join("keep.lock"), "keep")?;
        store.withdraw(
            id,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(150)?,
            &call,
        )?;
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside)?;
        std::fs::write(outside.join("keep"), "keep")?;
        let attempt = store.attempt_dir(id, false)?;
        if link_at_root {
            std::fs::remove_dir_all(&temporary)?;
            symlink(&outside, &temporary)?;
        } else {
            std::fs::create_dir_all(temporary.join("candidate-0/target"))?;
            symlink(&outside, temporary.join("candidate-0/target/outside.lock"))?;
        }
        assert_eq!(store.open_runs(10, &call)?, vec![run.run_id]);
        assert_eq!(
            store.reconcile_with_observer(
                run.run_id,
                &Fake::default(),
                &call,
                &provenant_supervisor::census::SystemProcessObserver
            )?,
            vec![(id, Reconciled::TemporaryRemoved)]
        );
        assert!(std::fs::symlink_metadata(&temporary).is_err());
        assert_eq!(std::fs::read_to_string(outside.join("keep"))?, "keep");
        assert!(artifacts::read_sealed(&attempt, "result.md", RESULT_LIMIT)?.is_some());
        assert!(artifacts::read_sealed(&attempt, "prompt.txt", RECORD_LIMIT)?.is_some());
    }
    Ok(())
}
#[test]
fn reconcile_removes_owner_only_temporary_and_retains_shared_modes() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    for mode in [0o500, 0o750, 0o755] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let temporary = store.temporary_path(id, &call)?;
        std::fs::write(temporary.join("keep.lock"), "keep")?;
        store.withdraw(
            id,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(150)?,
            &call,
        )?;
        let nested = temporary.join("candidate-0/target");
        std::fs::create_dir_all(&nested)?;
        std::fs::write(nested.join("s-1.lock"), "keep")?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))?;
        let found = store.reconcile_with_observer(
            run.run_id,
            &Fake::default(),
            &call,
            &provenant_supervisor::census::SystemProcessObserver,
        )?;
        if mode & 0o077 == 0 {
            assert_eq!(found, vec![(id, Reconciled::TemporaryRemoved)]);
            assert!(std::fs::symlink_metadata(&temporary).is_err());
            continue;
        }
        assert!(
            matches!(found.as_slice(), [(attempt, Reconciled::TemporaryRetained { path, reason })]
            if *attempt == id && *path == temporary && reason.kind == TemporaryReasonKind::NotPrivate),
            "{found:?}"
        );
        assert_eq!(
            std::fs::metadata(&temporary)?.permissions().mode() & 0o7777,
            mode
        );
        assert_eq!(
            std::fs::read_to_string(temporary.join("keep.lock"))?,
            "keep"
        );
        assert_eq!(std::fs::read_to_string(nested.join("s-1.lock"))?, "keep");
    }
    Ok(())
}
#[test]
fn cancel_queued_work_without_an_attempt_directory() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    let attempt = context.state_root.as_path().join(format!("attempts/{id}"));
    // The queued transaction can survive a crash before directory publication.
    std::fs::remove_dir_all(attempt)?;
    assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Cancelled);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Cancelled);
    Ok(())
}
#[test]
fn reconcile_recovers_a_writer_from_a_previous_boot() -> TestResult {
    recover_rebooted_writer(false, false, true, NEW_BOOT)
}
