use super::*;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

// Spec: docs/specs/run-settlement.md#settlement
#[test]
fn a_writer_lock_holder_with_a_live_settlement_owner_is_never_escalated() {
    let owner = |live: Option<bool>| move || live;
    let unreachable = || -> Option<bool> { panic!("guardian decides") };
    assert_eq!(
        reservation_may_release(false, Some(true), unreachable),
        (true, "guardian live")
    );
    assert_eq!(
        reservation_may_release(false, None, unreachable),
        (true, "guardian unobservable")
    );
    // Another attempt's settle worker can hold the lock after its
    // guardian exits; an unreadable owner is not proof of absence.
    assert_eq!(
        reservation_may_release(false, Some(false), owner(Some(true))),
        (true, "guardian exited, settlement owner live")
    );
    assert_eq!(
        reservation_may_release(false, Some(false), owner(None)),
        (true, "guardian exited, settlement owner unknown")
    );
    assert_eq!(
        reservation_may_release(false, Some(false), owner(Some(false))),
        (false, "guardian exited, no settlement owner")
    );
    // The waiter owns its own reservation's settlement.
    assert_eq!(
        reservation_may_release(true, Some(false), unreachable),
        (false, "guardian exited")
    );
}

#[test]
fn interrupted_writer_preparation_projects_typed_rejection()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = crate::tests::input();
    run.request.task.writer = Some(WriterRequest {
        extra: Default::default(),
        custody_project: Some(crate::tests::writer_project()),
        project: temp.path().join("checkout"),
        worktree: temp.path().join("checkout/.worktrees/interrupted"),
    });
    store.create(&run, &call)?;
    let id = run.attempt_id;
    let starting = store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
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
        .join("interrupted");
    std::fs::create_dir_all(&custody)?;
    // Preparation reserved the writer, then its submitter died before a
    // guardian handoff. No provider preparation refusal was observed.
    drop(provenant_supervisor::fixture::acquire_writer(
        provenant_os::fs::Directory::open_anchor(&custody)?,
        RunStore::scope(&starting),
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    let found = store.reconcile_with_observer(
        run.attempt_id,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    assert!(
        matches!(
            found.as_slice(),
            [(attempt, Reconciled::WriterLeaseReleased { locks })]
                if *attempt == id
                    && locks.iter().any(|lock| lock.ends_with("interrupted/writer.lock"))
        ),
        "{found:?}"
    );
    assert!(store.writer_fences()?.is_empty());
    let terminal = store.record(id, &call)?;
    let AttemptState::Terminal {
        cause,
        terminal_note,
        outcome,
        ..
    } = &terminal.state
    else {
        return Err("interrupted preparation did not settle".into());
    };
    assert_eq!(*outcome, Outcome::Rejected);
    assert_eq!(*cause, Some(CauseKind::LaunchHandoffRefused));
    assert_eq!(*terminal_note, None);
    let projected = provenant_records::wire::lane::LaneRow::project(&terminal, Default::default());
    assert_eq!(
        projected.reason.as_deref(),
        Some(CauseKind::LaunchHandoffRefused.detail())
    );
    Ok(())
}

#[test]
fn settled_writer_issues_are_discovered_only_in_their_persisted_slot() -> TestResult {
    use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
    use provenant_core::Clock as _;
    let fixture = PrepareFixture::new()?;
    let context = registered_landed_writer_fixture(&fixture)?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    let mut run = crate::tests::input();
    let writer = fixture.writer.clone();
    run.request.task.writer = Some(writer.clone());
    store.create(&run, &call)?;
    let starting = store.mark_starting(
        &store.record(run.attempt_id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.withdraw(
        run.attempt_id,
        starting.state.clone(),
        Outcome::Rejected,
        "fixture",
        clock.wall_now()?,
        &call,
    )?;
    store.set_temporary_retained(run.attempt_id, false, &call)?;
    crate::dispatch_tests::finish_reactions(&store)?;
    assert!(
        store.open_attempts_readonly(10, &call)?.is_empty(),
        "pending effects: {:?}, receipts: {:?}",
        store.pending_effect_attempts(run.attempt_id, &call)?,
        store.record(run.attempt_id, &call)?.settlement_receipts
    );

    let slot = store.writer_slot(&writer)?;
    std::fs::create_dir_all(&slot)?;
    drop(provenant_supervisor::fixture::acquire_writer(
        provenant_os::fs::Directory::open_anchor(&slot)?,
        RunStore::scope(&starting),
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    let reservation = slot
        .join(run.attempt_id.to_string())
        .join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    value["reservation_device"] =
        (value["reservation_device"].as_u64().ok_or("device")? + 1).into();
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    let listing = store.writer_fence_listing()?;
    assert!(listing.reservations.is_empty());
    let issue = listing
        .issues
        .iter()
        .find(|issue| issue.reservation == reservation)
        .ok_or("issue")?;
    assert!(store.writer_issue_candidate(issue, &call).is_some());
    let foreign = provenant_supervisor::custody::ReservationIssue {
        reservation: context
            .state_root
            .as_path()
            .join("worktrees")
            .join(provenant_core::ProjectId::from_bytes([99; 16]).to_string())
            .join(slot.file_name().ok_or("slot")?)
            .join(run.attempt_id.to_string())
            .join("reservation.json"),
        reason: "foreign fixture".into(),
        kind: provenant_supervisor::custody::ReservationIssueKind::Other,
    };
    assert!(store.writer_issue_candidate(&foreign, &call).is_none());
    assert_eq!(
        store.open_attempts_readonly(10, &call)?,
        vec![run.attempt_id]
    );
    let mut queued = false;
    store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
        queued = true;
        Ok(())
    })?;
    assert!(
        queued,
        "the async control must queue the settled writer issue"
    );
    let found = store.reconcile_with_observer(
        run.attempt_id,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    assert!(found.iter().any(|(id, _)| *id == run.attempt_id));
    assert!(matches!(
        store.record(run.attempt_id, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    Ok(())
}

#[test]
fn related_unreadable_slots_block_only_their_release_report() {
    let reservation =
        std::path::Path::new("/state/worktrees/project/slot/attempt/reservation.json");
    for related in [
        "/state/worktrees/project/slot/attempt/reservation.json",
        "/state/worktrees/project/slot/reservation.json",
        "/state/worktrees/project/slot",
        "/state/worktrees/project",
    ] {
        assert!(reservation_issue_blocks_report(
            reservation,
            std::path::Path::new(related)
        ));
    }
    for unrelated in [
        "/state/worktrees/project/other/reservation.json",
        "/state/worktrees/project/slot/other-attempt/reservation.json",
        "/state/worktrees/other-project/slot/reservation.json",
    ] {
        assert!(!reservation_issue_blocks_report(
            reservation,
            std::path::Path::new(unrelated)
        ));
    }
}

#[test]
fn reconcile_keeps_a_live_worktree_lease_and_its_reservation() -> TestResult {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = crate::tests::input();
    run.request.task.writer = Some(WriterRequest {
        extra: Default::default(),
        custody_project: Some(crate::tests::writer_project()),
        project: temp.path().join("checkout"),
        worktree: temp.path().join("checkout/.worktrees/live"),
    });
    store.create(&run, &call)?;
    let starting = store.mark_starting(
        &store.record(run.attempt_id, &call)?,
        &crate::tests::route(),
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
        .join("live");
    std::fs::create_dir_all(&custody)?;
    let lease = provenant_supervisor::fixture::acquire_writer(
        provenant_os::fs::Directory::open_anchor(&custody)?,
        RunStore::scope(&starting),
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    let reservation = custody
        .join(starting.attempt_id.to_string())
        .join("reservation.json");
    let before = std::fs::read(&reservation)?;

    let found = store.reconcile_with_observer(
        starting.attempt_id,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;

    assert!(matches!(
        found.as_slice(),
        [(attempt, Reconciled::Uncertain { .. } | Reconciled::Held)]
            if *attempt == starting.attempt_id
    ));
    assert_eq!(std::fs::read(&reservation)?, before);
    assert!(
        store
            .writer_fences()?
            .iter()
            .any(|fence| fence.scope == RunStore::scope(&starting))
    );
    drop(lease);
    Ok(())
}

#[test]
fn reconcile_journal_errors_are_reported_as_uncertain()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for failure in ["clear", "blocked", "listing"] {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let registry = temp.path().join("registry");
        std::fs::create_dir(&registry)?;
        if failure == "listing" {
            std::fs::write(registry.join("worktrees"), b"not a directory")?;
        }
        let store = RunStore::open(&context, &call)?.with_writer_registry(&registry);
        let run = crate::tests::input();
        let bad = run.attempt_id;
        store.create(&run, &call)?;
        if failure == "blocked" {
            store.mark_starting(
                &store.record(bad, &call)?,
                &crate::tests::route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            crate::artifacts::seal(
                &store.attempt_dir(bad, false)?,
                "launch.json",
                b"{}",
                RECORD_LIMIT,
            )?;
        }
        // Make journal I/O fail without relying on disk capacity or permissions.
        std::fs::create_dir(
            context
                .state_root
                .as_path()
                .join(format!("attempts/{bad}/settlement-progress.json")),
        )?;
        let found = store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert_eq!(found.len(), 1, "{failure}: {found:?}");
        assert!(
            matches!(
                &found[0],
                (id, Reconciled::Uncertain {
                    reason: UncertainReason::ReconcileFailed { .. }
                }) if *id == bad
            ),
            "{failure}: {found:?}"
        );
    }
    Ok(())
}

#[test]
fn reconcile_keeps_unrelated_orphan_fences_outside_its_attempt_scope()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    store.create(&run, &call)?;
    let orphan = AttemptId::from_bytes([7; 16]);
    let slot = context
        .state_root
        .as_path()
        .join("worktrees")
        .join(context.project_id.to_string())
        .join("orphan");
    std::fs::create_dir_all(&slot)?;
    drop(custody::acquire_reader(
        provenant_os::fs::Directory::open_anchor(&slot)?,
        Scope {
            attempt: orphan,
            generation: 1,
        },
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    assert!(matches!(
        store.record(orphan, &call),
        Err(RunError::NotFound)
    ));
    store.attempt_dir(orphan, true)?;
    assert_eq!(store.writer_fences()?.len(), 1);
    let found = store.reconcile_with_observer(
        run.attempt_id,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    assert_eq!(found, vec![(run.attempt_id, Reconciled::Queued)]);
    let queued = store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
        panic!("queued work and an orphan fence need no recovery worker")
    })?;
    assert_eq!(queued, vec![(run.attempt_id, Reconciled::Queued)]);
    assert_eq!(store.writer_fences()?[0].scope.attempt, orphan);
    let orphan_result = store.reconcile_with_observer(
        orphan,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    assert!(
        matches!(orphan_result.as_slice(), [(id, Reconciled::Uncertain { .. })] if *id == orphan)
    );
    Ok(())
}

#[test]
fn unreadable_writer_listing_still_judges_known_attempts()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::DirBuilderExt;

    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let registry = std::fs::canonicalize(temp.path())?.join("registry");
    // claim() also uses this as the private admission-ledger root.
    // Its mode must not depend on the test runner's umask.
    std::fs::DirBuilder::new().mode(0o700).create(&registry)?;
    std::fs::write(registry.join("worktrees"), b"not a directory")?;
    let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?
        .with_writer_registry(&registry);
    let run = crate::tests::input();
    store.create(&run, &call)?;
    let found = store.reconcile_with_observer(
        run.attempt_id,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    assert_eq!(found, [(run.attempt_id, Reconciled::Queued)]);
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let directory = store.attempt_dir(id, false)?;
    crate::artifacts::seal(&directory, "stdout.log", b"answer", RECORD_LIMIT)?;
    let mut exit = crate::tests::exit();
    exit.scope = RunStore::scope(&running);
    crate::artifacts::seal(
        &directory,
        "launch.json",
        &serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "scope": exit.scope, "requester": store.context.actor_id,
        }))?,
        RECORD_LIMIT,
    )?;
    crate::dispatch_tests::seal_exit(&store, &exit)?;
    let found = store.reconcile_with_observer(
        run.attempt_id,
        &crate::dispatch_tests::Fake::default(),
        &call,
        &SystemProcessObserver,
    )?;
    assert_eq!(found, [(id, Reconciled::Finished)]);
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Terminal { .. }
    ));
    Ok(())
}

#[test]
fn uncertain_reasons_serialise_by_kind_and_keep_their_text() {
    let retained = Reconciled::TemporaryRetained {
        path: "/tmp".into(),
        reason: TemporaryReason::new(TemporaryReasonKind::ReservationUnreadable, "detail"),
    };
    assert_eq!(serde_json::to_value(retained).unwrap()["reason"], "detail");
    assert_eq!(
        serde_json::to_value(UncertainReason::RebootProofUnavailable {
            error: "proof failed".into(),
            cause: RebootProofCause::Filesystem,
        })
        .unwrap(),
        serde_json::json!({ "kind": "reconcile_failed", "error": "proof failed" })
    );
    let reservation = std::path::PathBuf::from("/state/custody/reservation.json");
    let found = Reconciled::uncertain(UncertainReason::WriterCustodyUnconfirmed {
        reservation: reservation.clone(),
    });
    let value = serde_json::to_value(&found).unwrap();
    assert_eq!(value["state"], "uncertain");
    assert_eq!(value["reason"]["kind"], "writer_custody_unconfirmed");
    assert_eq!(
        value["reason"]["reservation"],
        "/state/custody/reservation.json"
    );
    let Reconciled::Uncertain { reason } = found else {
        unreachable!()
    };
    assert!(matches!(
        reason,
        UncertainReason::WriterCustodyUnconfirmed { .. }
    ));
    let unit = serde_json::to_value(UncertainReason::GuardianNotObservable).unwrap();
    assert_eq!(
        unit,
        serde_json::json!({ "kind": "guardian_not_observable" })
    );
    let custody = Reconciled::custody_unavailable(&"lock is held");
    assert_eq!(
        custody,
        Reconciled::Uncertain {
            reason: UncertainReason::CustodyUnavailable {
                error: "lock is held".into()
            }
        }
    );
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn steered_claude_without_a_session_starts_a_fresh_turn() -> TestResult {
    use std::os::unix::fs::DirBuilderExt;

    let temp = tempfile::tempdir()?;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(temp.path().join("state"))?;
    let mut clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = crate::tests::input();
    if let provenant_records::run::RequestedRoute::Model {
        provider, model, ..
    } = &mut run.request.task.route
    {
        *provider = "claude".into();
        *model = "claude/claude-sonnet-5-5".into();
    }
    let mut route = crate::tests::route();
    route.provider = Some("claude".into());
    route.model = Some("claude/claude-sonnet-5-5".into());
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &route,
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    assert!(running.session.is_none());
    store.steer(
        id,
        OperationId::parse("steer-no-session")?,
        "Apply this instruction.",
        &call,
    )?;
    let queued = store.record(id, &call)?;
    assert!(matches!(queued.state, AttemptState::Running { .. }));
    assert!(queued.cancellation_disposition.is_none());
    let mut exit = crate::tests::exit();
    exit.scope = RunStore::scope(&running);
    crate::dispatch_tests::seal_exit(&store, &exit)?;
    let mut publication = crate::tests::publication();
    // A finished task has no follow-up (#3987). Keep this writer unsettled so
    // the test still reaches the no-session continuation path it pins.
    publication.fact.outcome = Outcome::Uncommitted;
    publication.resolved_route = Some(route.clone());
    publication.applied_route = Some(route.clone());
    publication.observed_route = Some(route.clone());
    store.publish_terminal(id, running.revision, &publication, &exit, &call)?;
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Uncommitted,
            cause: None,
            ..
        }
    ));
    clock.advance(provenant_core::DurationMicros::from_micros(100))?;
    let call = crate::tests::call(&clock, &cancel);
    assert!(matches!(
        store.prepare_steer_continuation(id, &call),
        Err(RunError::SteerSessionUnavailable)
    ));
    let child = store
        .prepare_fresh_steer_continuation(id, &call)?
        .ok_or("fresh successor")?;
    let child_record = store.record(child, &call)?;
    assert_eq!(child_record.continuation.as_deref(), Some("fresh"));
    let prompt = store.artifact_bytes(&child_record.prompt, RECORD_LIMIT)?;
    assert!(
        String::from_utf8(prompt)?
            .contains("Queued steering instructions:\nApply this instruction.")
    );
    store.mark_starting(&child_record, &route, &[], &Default::default(), None, &call)?;
    let successor = store
        .claim(child, &clock, &call)?
        .ok_or("fresh successor did not start")?;
    assert!(matches!(successor.state, AttemptState::Running { .. }));
    assert_eq!(store.record(id, &call)?.superseded_by, Some(child));
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn steer_replays_are_idempotent_and_natural_settlement_adopts_all_intents() -> TestResult {
    use provenant_supervisor::census::ProcessObservation;

    let temp = tempfile::tempdir()?;
    let mut clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let mut running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;

    // Deliberately reverse lexical operation-ID order: accepted sequence,
    // not the idempotency key, owns the continuation prompt order.
    let first_operation = OperationId::parse("steer-z-first")?;
    let second_operation = OperationId::parse("steer-a-second")?;
    let first = store.steer(
        id,
        first_operation.clone(),
        "Inspect the failing path.",
        &call,
    )?;
    let second = store.steer(
        id,
        second_operation,
        "Prioritise the smallest repair.",
        &call,
    )?;
    assert!(matches!(
        store.steer(
            id,
            first_operation.clone(),
            "Replace the original intent.",
            &call
        ),
        Err(RunError::OperationConflict)
    ));
    let replay = store.steer(id, first_operation, "Inspect the failing path.", &call)?;
    let message_id = match first {
        SteerReceipt::Queued { message_id } => message_id,
        receipt => return Err(format!("expected initial queue, got {receipt:?}").into()),
    };
    let second_message = match second {
        SteerReceipt::Queued { message_id } => message_id,
        receipt => return Err(format!("expected queued receipt, got {receipt:?}").into()),
    };
    assert_ne!(message_id, second_message);
    assert_eq!(
        replay,
        SteerReceipt::AlreadyQueued { message_id },
        "replaying one operation returns its original message ID"
    );
    let intents = store.steer_intent(id, &call)?;
    assert_eq!(
        intents
            .iter()
            .map(|intent| intent.text.as_str())
            .collect::<Vec<_>>(),
        [
            "Inspect the failing path.",
            "Prioritise the smallest repair."
        ]
    );
    let queued_record = store.record(id, &call)?;
    let queued_status = queued_record.steering.as_ref().ok_or("steering status")?;
    assert_eq!(
        queued_status.state,
        provenant_records::run::SteeringState::Queued
    );
    assert_eq!(queued_status.operations.len(), 2);
    assert!(queued_status.successor.is_none());
    assert!(
        serde_json::to_value(&queued_record)?
            .get("steering")
            .is_none()
    );

    // The provider exits cleanly after intent persistence. Settlement adopts it.
    let mut exit = crate::tests::exit();
    exit.scope = RunStore::scope(&running);
    crate::dispatch_tests::seal_exit(&store, &exit)?;
    let mut publication = crate::tests::publication();
    publication.session = Some("fixture-session".into());
    // A writer that leaves retained edits naturally continues into its queued
    // turn. A finished (`Succeeded`) writer never creates a follow-up (#3987).
    publication.fact.outcome = Outcome::Uncommitted;
    running = store.publish_terminal(id, running.revision, &publication, &exit, &call)?;
    assert!(matches!(
        running.state,
        AttemptState::Terminal {
            outcome: Outcome::Uncommitted,
            cause: None,
            ..
        }
    ));

    // The successor is timestamped from the terminal predecessor. Move
    // the fixture clock to that settlement time before admitting it.
    clock.advance(provenant_core::DurationMicros::from_micros(100))?;
    let call = crate::tests::call(&clock, &cancel);

    let child = store
        .prepare_steer_continuation(id, &call)?
        .ok_or("natural settlement did not adopt durable steer")?;
    let replayed_child = store
        .prepare_steer_continuation(id, &call)?
        .ok_or("continuation replay lost its child")?;
    assert_eq!(child, replayed_child);
    assert_eq!(store.record(id, &call)?.superseded_by, Some(child));
    let child_prompt = crate::artifacts::read_sealed(
        &store.attempt_dir(child, false)?,
        provenant_records::AttemptFile::Prompt.name(),
        RECORD_LIMIT,
    )?
    .ok_or("continuation prompt")?;
    let expected_prompt = format!(
        "{}Inspect the failing path.\n\nPrioritise the smallest repair.",
        STEER_PROMPT_PREFIX
    );
    assert_eq!(child_prompt, expected_prompt.as_bytes());
    let child_count: i64 = store.db.query_row(
        "SELECT COUNT(*) FROM attempts WHERE continues=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(
        child_count, 1,
        "settlement replay must not create a second child"
    );
    store.record_steer_launch_result(
        id,
        child,
        &[Started::Skipped {
            attempt: child,
            reason: "fixture skip".into(),
        }],
        &call,
    )?;
    let queued_status = store
        .record(id, &call)?
        .steering
        .ok_or("queued steering status")?;
    assert_eq!(
        queued_status.state,
        provenant_records::run::SteeringState::Queued
    );
    assert_eq!(queued_status.successor, Some(child));
    assert_eq!(store.steer_intent(id, &call)?.len(), 2);
    store.record_steer_launch_result(id, child, &[], &call)?;
    store.record_steer_launch_result(
        id,
        child,
        &[Started::Unknown {
            attempt: child,
            error: "fixture uncertain handoff".into(),
        }],
        &call,
    )?;
    assert_eq!(
        store.record(id, &call)?.steering.as_ref().unwrap().state,
        provenant_records::run::SteeringState::Queued
    );
    assert_eq!(store.prepare_steer_continuation(id, &call)?, Some(child));
    let child_count: i64 = store.db.query_row(
        "SELECT COUNT(*) FROM attempts WHERE continues=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(child_count, 1, "transient launch receipts reuse one child");
    store.record_steer_launch_result(id, child, &[Started::Launched(child)], &call)?;
    let admitted_status = store
        .record(id, &call)?
        .steering
        .ok_or("admitted steering status")?;
    assert_eq!(
        admitted_status.state,
        provenant_records::run::SteeringState::Queued,
        "launch admission does not prove that the provider resumed its session"
    );
    assert_eq!(admitted_status.successor, Some(child));
    assert_eq!(admitted_status.cause, None);

    let route = crate::tests::route();
    let expected_route_digest = steer_route_digest(&route)?;
    assert_eq!(expected_route_digest.len(), 71);
    assert!(expected_route_digest.starts_with("sha256:"));
    let queued_child = store.record(child, &call)?;
    let starting =
        store.mark_starting(&queued_child, &route, &[], &Default::default(), None, &call)?;
    let expectation = crate::ContinuationExpectation {
        session: "fixture-session".into(),
    };
    let starting = store.record_starting_launch(
        &starting,
        &route,
        Some(&expectation),
        &[],
        &Default::default(),
        None,
        0,
        &Default::default(),
        &call,
    )?;
    assert_eq!(starting.applied_route.as_ref(), Some(&route));
    let saved_expectation = store
        .continuation_expectation(child, &call)?
        .ok_or("continuation expectation missing")?;
    assert_eq!(saved_expectation.session, "fixture-session");
    let stored_expectation_digest: String = store.db.query_row(
        "SELECT route_digest FROM continuation_expectations WHERE attempt=?1",
        [child.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(stored_expectation_digest, expected_route_digest);
    let child_running = store
        .claim(child, &clock, &call)?
        .ok_or("child not admitted")?;
    let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
        ProcessObservation::Present(fact) if !fact.zombie => {
            provenant_os::capture::CaptureSourceIdentity {
                attempt: child.to_string(),
                generation: RunStore::scope(&child_running).generation,
                boot: fact.identity.boot,
                pid: fact.identity.pid,
                native_start: fact.identity.native_start,
            }
        }
        observation => {
            return Err(format!("test process identity unavailable: {observation:?}").into());
        }
    };
    let applied = child_running
        .applied_route
        .as_ref()
        .ok_or("child applied route")?;
    assert!(store.record_provider_session_ready(
        child,
        applied,
        "fixture-session",
        &source,
        &call,
    )?);
    let stored_ready_digest: String = store.db.query_row(
        "SELECT route_digest FROM provider_session_ready WHERE attempt=?1",
        [child.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(stored_ready_digest, expected_route_digest);
    let resumed_status = store
        .record(id, &call)?
        .steering
        .ok_or("resumed steering status")?;
    assert_eq!(
        resumed_status.state,
        provenant_records::run::SteeringState::Resumed
    );
    assert_eq!(resumed_status.successor, Some(child));
    assert_eq!(resumed_status.cause, None);
    let retained = store.steer_ledger(id, &call)?;
    assert_eq!(retained.len(), 2);
    assert!(retained.iter().all(|(_, resolved)| *resolved));

    let mut child_exit = crate::tests::exit();
    child_exit.scope = RunStore::scope(&child_running);
    crate::dispatch_tests::seal_exit(&store, &child_exit)?;
    let mut child_publication = crate::tests::publication();
    child_publication.started_at = child_running.started_at;
    child_publication.session = Some("fixture-session".into());
    store.publish_terminal(
        child,
        child_running.revision,
        &child_publication,
        &child_exit,
        &call,
    )?;
    let final_status = store
        .record(id, &call)?
        .steering
        .ok_or("final resumed steering status")?;
    assert_eq!(
        final_status.state,
        provenant_records::run::SteeringState::Resumed
    );
    let (live_ack, terminal_final): (i64, i64) = store.db.query_row(
        "SELECT live_ack,terminal_final FROM steer_continuations WHERE attempt=?1",
        [id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((live_ack, terminal_final), (1, 1));

    store.db.execute(
        "DELETE FROM provider_session_ready WHERE attempt=?1",
        [child.to_string()],
    )?;
    store.db.execute(
        "DELETE FROM continuation_expectations WHERE attempt=?1",
        [child.to_string()],
    )?;
    assert!(
        store.continuation_expectation(child, &call).is_err(),
        "a resumed child cannot be decoded as fresh after its expectation is lost"
    );
    assert_eq!(
        store.record(id, &call)?.steering.unwrap().state,
        provenant_records::run::SteeringState::Resumed,
        "the saved disposition does not depend on retained child proof rows"
    );
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn a_finished_task_creates_no_steer_follow_up() -> TestResult {
    let temp = tempfile::tempdir()?;
    let mut clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    store.steer(
        id,
        OperationId::parse("steer-after-finish")?,
        "Inspect the failing path.",
        &call,
    )?;
    let mut exit = crate::tests::exit();
    exit.scope = RunStore::scope(&running);
    crate::dispatch_tests::seal_exit(&store, &exit)?;
    let mut publication = crate::tests::publication();
    publication.session = Some("fixture-session".into());
    let finished = store.publish_terminal(id, running.revision, &publication, &exit, &call)?;
    assert!(matches!(
        finished.state,
        AttemptState::Terminal {
            outcome: Outcome::Succeeded,
            ..
        }
    ));

    clock.advance(provenant_core::DurationMicros::from_micros(100))?;
    let call = crate::tests::call(&clock, &cancel);
    assert_eq!(
        store.prepare_steer_continuation(id, &call)?,
        None,
        "a finished task must not spawn a follow-up attempt (#3987)"
    );
    let record = store.record(id, &call)?;
    assert_eq!(record.superseded_by, None);
    let status = record.steering.ok_or("steering status")?;
    assert_eq!(
        status.state,
        provenant_records::run::SteeringState::NotContinued
    );
    assert_eq!(
        status.cause,
        Some(provenant_records::run::SteeringCause::TerminalStop)
    );
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn terminal_proof_revises_live_resume_without_losing_acknowledgement() -> TestResult {
    use provenant_supervisor::census::ProcessObservation;

    #[derive(Clone, Copy)]
    enum Case {
        Refused,
        MissingSession,
        MismatchedSession,
        LiveThenTerminalMismatch,
        LiveThenTerminalIncomplete,
        LiveThenTerminalIncompleteAllowed,
        LiveThenTerminalSteeredEof,
        LiveThenTerminalExplicitCancelEof,
        LiveThenTerminalEofIncomplete,
        LiveThenTerminalEofConflict,
        AccountMismatch,
        ModelMismatch,
        PoolParentResumes,
    }
    for case in [
        Case::Refused,
        Case::MissingSession,
        Case::MismatchedSession,
        Case::LiveThenTerminalMismatch,
        Case::LiveThenTerminalIncomplete,
        Case::LiveThenTerminalIncompleteAllowed,
        Case::LiveThenTerminalSteeredEof,
        Case::LiveThenTerminalExplicitCancelEof,
        Case::LiveThenTerminalEofIncomplete,
        Case::LiveThenTerminalEofConflict,
        Case::AccountMismatch,
        Case::ModelMismatch,
        Case::PoolParentResumes,
    ] {
        let temp = tempfile::tempdir()?;
        let mut clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        let parent = run.attempt_id;
        let mut route = crate::tests::route();
        route.account_key = Some("fixture-account".into());
        if matches!(case, Case::PoolParentResumes) {
            route.source = Some("pool".into());
            route.account_source = Some(provenant_records::run::AccountSource::Automatic);
            route.pool_selection = Some(provenant_records::RouteSelectionReceipt {
                selection_rule: provenant_records::route_selection::SelectionRule::WeightedDraw,
                considered_exclusions: Vec::new(),
                exploration: provenant_records::route_selection::Exploration {
                    extra: Default::default(),
                    selected: false,
                    percent: 0,
                    enabled: false,
                    source: "fixture".into(),
                },
                weight_source: provenant_records::route_selection::WeightSource::Catalogue,
                account_load: None,
                extra: Default::default(),
                pool: "junior".into(),
                candidates: vec![],
                roll: 0,
                total_weight: 1,
                pick: "fixture/pick".into(),
                reason: None,
                redraws: vec![],
            });
        }
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(parent, &call)?,
            &route,
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let parent_running = store
            .claim(parent, &clock, &call)?
            .ok_or("parent not admitted")?;
        store.steer(
            parent,
            OperationId::parse("steer-proof-boundary")?,
            "Continue from the bound provider session.",
            &call,
        )?;
        let mut parent_exit = crate::tests::exit();
        parent_exit.scope = RunStore::scope(&parent_running);
        crate::dispatch_tests::seal_exit(&store, &parent_exit)?;
        let mut parent_publication = crate::tests::publication();
        // A finished task has no follow-up (#3987). This test pins how a
        // terminal proof revises the successor's live acknowledgement, so the
        // parent must stay continuable: leave retained edits for its queued
        // turn instead of publishing success.
        parent_publication.fact.outcome = Outcome::Uncommitted;
        parent_publication.session = Some("fixture-session".into());
        parent_publication.resolved_route = Some(route.clone());
        parent_publication.applied_route = Some(route.clone());
        parent_publication.observed_route = Some(route.clone());
        store.publish_terminal(
            parent,
            parent_running.revision,
            &parent_publication,
            &parent_exit,
            &call,
        )?;
        clock.advance(provenant_core::DurationMicros::from_micros(100))?;
        let call = crate::tests::call(&clock, &cancel);
        let child = store
            .prepare_steer_continuation(parent, &call)?
            .ok_or("steer successor missing")?;

        if matches!(case, Case::Refused) {
            let queued = store.record(child, &call)?;
            store.withdraw(
                child,
                queued.state.clone(),
                Outcome::Rejected,
                "fixture preparation refusal",
                WallTime::from_micros(300).unwrap(),
                &call,
            )?;
            store.record_steer_launch_result(
                parent,
                child,
                &[Started::Refused {
                    attempt: child,
                    reason: "fixture preparation refusal".into(),
                    kind: RefusalKind::Preparation,
                }],
                &call,
            )?;
            let status = store
                .record(parent, &call)?
                .steering
                .ok_or("refused steering status")?;
            assert_eq!(
                status.state,
                provenant_records::run::SteeringState::NotContinued
            );
            assert_eq!(status.successor, Some(child));
            assert_eq!(
                status.cause,
                Some(provenant_records::run::SteeringCause::ContinuationRefused)
            );
        } else {
            let mismatch = matches!(case, Case::AccountMismatch | Case::ModelMismatch);
            let child_route = if matches!(case, Case::AccountMismatch) {
                let mut changed = route.clone();
                changed.account_key = Some("other-account".into());
                changed
            } else if matches!(case, Case::ModelMismatch) {
                let mut changed = route.clone();
                changed.model = Some("other-model".into());
                changed
            } else if matches!(case, Case::PoolParentResumes) {
                let mut resumed = route.clone();
                resumed.source = Some("continuation".into());
                resumed.account_source = Some(provenant_records::run::AccountSource::Continuation);
                resumed.pool_selection = None;
                resumed
            } else {
                route.clone()
            };
            let starting = store.mark_starting(
                &store.record(child, &call)?,
                &child_route,
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let expectation = crate::ContinuationExpectation {
                session: "fixture-session".into(),
            };
            let launch = store.record_starting_launch(
                &starting,
                &child_route,
                Some(&expectation),
                &[],
                &Default::default(),
                None,
                0,
                &Default::default(),
                &call,
            );
            if mismatch {
                assert!(matches!(launch, Err(RunError::SteerSessionMismatch)));
                let status = store
                    .record(parent, &call)?
                    .steering
                    .ok_or("account-mismatch steering status")?;
                assert_eq!(status.state, provenant_records::run::SteeringState::Queued);
                assert_eq!(status.successor, Some(child));
                assert_eq!(status.cause, None);
                assert!(store.continuation_expectation(child, &call).is_err());
                continue;
            }
            let starting = launch?;
            if matches!(case, Case::PoolParentResumes) {
                assert_eq!(
                    store
                        .continuation_expectation(child, &call)?
                        .ok_or("pool continuation expectation missing")?
                        .session,
                    "fixture-session"
                );
            }
            let child_running = store
                .claim(child, &clock, &call)?
                .ok_or("child not admitted")?;
            store.record_steer_launch_result(parent, child, &[Started::Launched(child)], &call)?;
            assert_eq!(
                store.record(parent, &call)?.steering.unwrap().state,
                provenant_records::run::SteeringState::Queued
            );
            let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
                ProcessObservation::Present(fact) if !fact.zombie => {
                    provenant_os::capture::CaptureSourceIdentity {
                        attempt: child.to_string(),
                        generation: RunStore::scope(&child_running).generation,
                        boot: fact.identity.boot,
                        pid: fact.identity.pid,
                        native_start: fact.identity.native_start,
                    }
                }
                observation => {
                    return Err(
                        format!("test process identity unavailable: {observation:?}").into(),
                    );
                }
            };
            let applied = child_running
                .applied_route
                .as_ref()
                .ok_or("child applied route")?;
            let live_ack = matches!(
                case,
                Case::LiveThenTerminalMismatch
                    | Case::LiveThenTerminalIncomplete
                    | Case::LiveThenTerminalSteeredEof
                    | Case::LiveThenTerminalExplicitCancelEof
                    | Case::LiveThenTerminalEofIncomplete
                    | Case::LiveThenTerminalEofConflict
                    | Case::PoolParentResumes
            );
            if live_ack {
                assert!(store.record_provider_session_ready(
                    child,
                    applied,
                    "fixture-session",
                    &source,
                    &call,
                )?);
                assert_eq!(
                    store.record(parent, &call)?.steering.unwrap().state,
                    provenant_records::run::SteeringState::Resumed,
                    "a verified live session acknowledgement is reported promptly"
                );
                let (saved_live_ack, saved_terminal_final): (i64, i64) = store.db.query_row(
                    "SELECT live_ack,terminal_final FROM steer_continuations WHERE attempt=?1",
                    [parent.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!((saved_live_ack, saved_terminal_final), (1, 0));
            }
            let mut exit = crate::tests::exit();
            exit.scope = RunStore::scope(&child_running);
            let mut publication = crate::tests::publication();
            publication.started_at = child_running.started_at;
            publication.resolved_route = Some(child_route.clone());
            publication.applied_route = Some(child_route.clone());
            publication.observed_route = Some(child_route.clone());
            let (expected_state, expected_cause) = match case {
                Case::MissingSession => {
                    exit.stop_reason = Some(StopReason::Steered);
                    exit.cancel_delivered_before_exit = Some(true);
                    exit.provider_identity_eof_fragment = true;
                    publication.fact.outcome = Outcome::Cancelled;
                    publication.fact.cause = Some(CauseKind::Steered);
                    publication.result = "continued attempt stopped before session proof";
                    publication.session = None;
                    (
                        provenant_records::run::SteeringState::NotContinued,
                        Some(provenant_records::run::SteeringCause::SessionUnavailable),
                    )
                }
                Case::MismatchedSession => {
                    exit.provider_identity_conflict = true;
                    publication.fact.outcome = Outcome::Failed;
                    publication.fact.cause = Some(CauseKind::IdentityMismatch);
                    publication.result = "provider session did not match its continuation";
                    publication.fact.complete = false;
                    publication.session = Some("different-session".into());
                    (
                        provenant_records::run::SteeringState::NotContinued,
                        Some(provenant_records::run::SteeringCause::SessionMismatch),
                    )
                }
                Case::LiveThenTerminalMismatch => {
                    exit.provider_identity_conflict = true;
                    publication.fact.outcome = Outcome::Failed;
                    publication.fact.cause = Some(CauseKind::IdentityMismatch);
                    publication.result = "terminal session contradicted live acknowledgement";
                    publication.fact.complete = false;
                    publication.session = Some("different-session".into());
                    (
                        provenant_records::run::SteeringState::NotContinued,
                        Some(provenant_records::run::SteeringCause::SessionMismatch),
                    )
                }
                Case::LiveThenTerminalIncomplete => {
                    exit.provider_identity_evidence_incomplete = true;
                    publication.fact.outcome = Outcome::Unsettled;
                    publication.fact.reason =
                        Some("terminal provider evidence was incomplete".into());
                    publication.result = "terminal provider evidence was incomplete";
                    publication.fact.complete = false;
                    publication.session = None;
                    (
                        provenant_records::run::SteeringState::NotContinued,
                        Some(provenant_records::run::SteeringCause::SessionProofIncomplete),
                    )
                }
                Case::LiveThenTerminalIncompleteAllowed => {
                    exit.provider_identity_evidence_incomplete = true;
                    publication.fact.outcome = Outcome::Succeeded;
                    publication.session = Some("fixture-session".into());
                    (provenant_records::run::SteeringState::Resumed, None)
                }
                Case::LiveThenTerminalSteeredEof | Case::LiveThenTerminalExplicitCancelEof => {
                    exit.stop_reason = Some(if matches!(case, Case::LiveThenTerminalSteeredEof) {
                        StopReason::Steered
                    } else {
                        StopReason::ExplicitCancellation
                    });
                    exit.cancel_delivered_before_exit = Some(true);
                    exit.provider_identity_eof_fragment = true;
                    publication.fact.outcome = Outcome::Cancelled;
                    publication.fact.cause = matches!(case, Case::LiveThenTerminalSteeredEof)
                        .then_some(CauseKind::Steered);
                    publication.fact.reason = Some(
                        "confirmed intentional stop ended at an incomplete final frame".into(),
                    );
                    publication.result =
                        "provider session identity remained valid before the final fragment";
                    publication.fact.complete = false;
                    publication.session = Some("fixture-session".into());
                    (provenant_records::run::SteeringState::Resumed, None)
                }
                Case::LiveThenTerminalEofIncomplete => {
                    exit.stop_reason = Some(StopReason::Steered);
                    exit.cancel_delivered_before_exit = Some(true);
                    exit.provider_identity_eof_fragment = true;
                    exit.provider_identity_evidence_incomplete = true;
                    publication.fact.outcome = Outcome::Unsettled;
                    publication.fact.reason =
                        Some("provider identity evidence was incomplete".into());
                    publication.result = "provider identity evidence was incomplete";
                    publication.fact.complete = false;
                    publication.session = None;
                    (
                        provenant_records::run::SteeringState::NotContinued,
                        Some(provenant_records::run::SteeringCause::SessionProofIncomplete),
                    )
                }
                Case::LiveThenTerminalEofConflict => {
                    exit.stop_reason = Some(StopReason::Steered);
                    exit.cancel_delivered_before_exit = Some(true);
                    exit.provider_identity_eof_fragment = true;
                    exit.provider_identity_conflict = true;
                    publication.fact.outcome = Outcome::Failed;
                    publication.fact.cause = Some(CauseKind::IdentityMismatch);
                    publication.result = "provider identity conflicted before the final fragment";
                    publication.fact.complete = false;
                    publication.session = Some("fixture-session".into());
                    (
                        provenant_records::run::SteeringState::NotContinued,
                        Some(provenant_records::run::SteeringCause::SessionMismatch),
                    )
                }
                Case::PoolParentResumes => {
                    publication.session = Some("fixture-session".into());
                    (provenant_records::run::SteeringState::Resumed, None)
                }
                Case::AccountMismatch | Case::ModelMismatch => unreachable!(),
                Case::Refused => unreachable!(),
            };
            crate::dispatch_tests::seal_exit(&store, &exit)?;
            store.publish_terminal(child, child_running.revision, &publication, &exit, &call)?;
            let status = store
                .record(parent, &call)?
                .steering
                .ok_or("terminal steering status")?;
            assert_eq!(status.state, expected_state);
            assert_eq!(status.successor, Some(child));
            assert_eq!(status.cause, expected_cause);
            let (saved_live_ack, saved_terminal_final): (i64, i64) = store.db.query_row(
                "SELECT live_ack,terminal_final FROM steer_continuations WHERE attempt=?1",
                [parent.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(saved_live_ack, if live_ack { 1_i64 } else { 0_i64 });
            assert_eq!(saved_terminal_final, 1);
            assert_eq!(store.prepare_steer_continuation(parent, &call)?, None);
            assert!(
                store
                    .steer_ledger(parent, &call)?
                    .iter()
                    .any(|(intent, resolved)| intent.text
                        == "Continue from the bound provider session."
                        && *resolved)
            );
            assert!(starting.applied_route.is_some());
        }
    }
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn clean_exit_with_incomplete_identity_evidence_settles_the_result() -> TestResult {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let attempt = store.attempt_dir(id, false)?;
    crate::artifacts::seal(&attempt, "stdout.log", b"valid result", CAPTURE_LIMIT)?;
    let mut exit = crate::tests::exit();
    exit.scope = RunStore::scope(&running);
    exit.provider_identity_evidence_incomplete = true;
    crate::dispatch_tests::seal_exit(&store, &exit)?;
    let fake = crate::dispatch_tests::Fake::with_observed(
        Some(crate::tests::route()),
        Some(Outcome::Succeeded),
    );
    let finished = store.finish(id, &attempt, &exit, &fake, &call)?;
    assert!(matches!(
        finished.state,
        AttemptState::Terminal {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn sealed_live_identity_conflict_fences_a_queued_successor() -> TestResult {
    {
        let (conflict, incomplete, outcome, cause) = (
            true,
            false,
            Outcome::Failed,
            Some(CauseKind::IdentityMismatch),
        );
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let operation = OperationId::parse("steer-identity-conflict")?;
        store.steer(id, operation, "Keep the existing session.", &call)?;

        let attempt = store.attempt_dir(id, false)?;
        crate::artifacts::seal(
            &attempt,
            "stdout.log",
            b"apparently successful",
            CAPTURE_LIMIT,
        )?;
        let mut exit = crate::tests::exit();
        exit.scope = RunStore::scope(&running);
        exit.provider_identity_conflict = conflict;
        exit.provider_identity_evidence_incomplete = incomplete;
        crate::dispatch_tests::seal_exit(&store, &exit)?;
        let fake = crate::dispatch_tests::Fake::with_observed(
            Some(crate::tests::route()),
            Some(Outcome::Succeeded),
        );
        let finished = store.finish(id, &attempt, &exit, &fake, &call)?;
        assert!(matches!(
            finished.state,
            AttemptState::Terminal {
                outcome: actual_outcome,
                cause: actual_cause,
                ..
            } if actual_outcome == outcome && actual_cause == cause
        ));
        assert_eq!(store.prepare_steer_continuation(id, &call)?, None);
        assert_eq!(store.record(id, &call)?.superseded_by, None);
        let ledger = store.steer_ledger(id, &call)?;
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].0.text.as_str(), "Keep the existing session.");
        assert!(ledger[0].1, "a non-continuable stop resolves the intent");
        assert_eq!(
            store
                .steering_status(id, &call)?
                .ok_or("steering status missing")?
                .state,
            provenant_records::run::SteeringState::NotContinued
        );
        assert_eq!(
            store
                .steering_status(id, &call)?
                .ok_or("steering status missing")?
                .cause,
            Some(provenant_records::run::SteeringCause::TerminalStop)
        );
    }
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn eof_fragment_only_continues_after_a_confirmed_intentional_stop() -> TestResult {
    use provenant_supervisor::census::ProcessObservation;

    let cases = [
        (
            Some(StopReason::Steered),
            Some(true),
            false,
            false,
            Outcome::Cancelled,
            Some(CauseKind::Steered),
            true,
        ),
        (
            Some(StopReason::ExplicitCancellation),
            Some(true),
            false,
            false,
            Outcome::Cancelled,
            None,
            false,
        ),
        (
            None,
            Some(false),
            false,
            false,
            Outcome::Unsettled,
            None,
            false,
        ),
        (
            Some(StopReason::Steered),
            Some(false),
            false,
            false,
            Outcome::Unsettled,
            None,
            false,
        ),
        (
            Some(StopReason::Steered),
            Some(true),
            false,
            true,
            Outcome::Cancelled,
            Some(CauseKind::Steered),
            true,
        ),
        (
            Some(StopReason::Steered),
            Some(true),
            true,
            false,
            Outcome::Failed,
            Some(CauseKind::IdentityMismatch),
            false,
        ),
    ];
    for (index, (reason, delivered, conflict, incomplete, outcome, cause, resumes)) in
        cases.into_iter().enumerate()
    {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let operation = OperationId::parse(&format!("steer-eof-{index}"))?;
        assert!(matches!(
            store.steer(id, operation, "Continue from the validated session.", &call)?,
            SteerReceipt::Queued { .. }
        ));
        let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) if !fact.zombie => {
                provenant_os::capture::CaptureSourceIdentity {
                    attempt: id.to_string(),
                    generation: RunStore::scope(&running).generation,
                    boot: fact.identity.boot,
                    pid: fact.identity.pid,
                    native_start: fact.identity.native_start,
                }
            }
            observation => {
                return Err(format!("test process identity unavailable: {observation:?}").into());
            }
        };
        let route = running
            .applied_route
            .as_ref()
            .ok_or("applied route")?
            .clone();
        assert!(store.record_provider_session_ready(id, &route, "live-session", &source, &call,)?);
        if conflict {
            assert!(store.record_provider_identity_conflict(id, &route, &source, &call)?);
        }

        let attempt = store.attempt_dir(id, false)?;
        let scope = RunStore::scope(&running);
        let mut lease = custody::acquire_reader(
            attempt.try_clone()?,
            scope,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        let provider = provenant_supervisor::census::ProcessIdentity {
            boot: source.boot.clone(),
            pid: source.pid,
            native_start: source.native_start,
            group_id: source.pid,
            session_id: source.pid,
        };
        provenant_supervisor::fixture::record_spawned_provider(
            &mut lease,
            &attempt,
            provider.clone(),
            provider,
        )?;
        let recovery = lease.into_recovery()?;
        crate::artifacts::seal(
                &attempt,
                "stdout.log",
                b"{\"event\":\"init\",\"conversation_id\":\"live-session\",\"init\":{\"model\":\"expected-model\"}}\n{\"event\":",
                CAPTURE_LIMIT,
            )?;
        let mut exit = crate::tests::exit();
        exit.scope = RunStore::scope(&running);
        exit.stop_reason = reason;
        exit.cancel_delivered_before_exit = delivered;
        exit.provider_identity_conflict = conflict;
        exit.provider_identity_evidence_incomplete = incomplete;
        exit.provider_identity_eof_fragment = true;
        exit.terminal_observed = true;
        exit.settlement_evidence = custody::SettlementEvidence::DarwinCensusAndCustodyLease;
        crate::dispatch_tests::seal_exit(&store, &exit)?;
        let mut fake =
            crate::dispatch_tests::Fake::with_observed(Some(route), Some(Outcome::Succeeded));
        fake.session = Some("live-session".into());
        let finished =
            store.finish_with_recovery_for_fixture(id, &attempt, &exit, &recovery, &fake, &call)?;
        assert!(matches!(
            finished.state,
            AttemptState::Terminal {
                outcome: actual_outcome,
                cause: actual_cause,
                ..
            } if actual_outcome == outcome && actual_cause == cause
        ));

        let child = store.prepare_steer_continuation(id, &call)?;
        assert_eq!(child.is_some(), resumes);
        if let Some(child) = child {
            assert_eq!(store.record(id, &call)?.superseded_by, Some(child));
            let status = store.steering_status(id, &call)?.ok_or("steering status")?;
            assert_eq!(status.state, provenant_records::run::SteeringState::Queued);
            assert_eq!(status.successor, Some(child));
        } else {
            assert_eq!(store.record(id, &call)?.superseded_by, None);
        }
    }
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn steer_session_readiness_binds_scope_process_and_immutable_route_identity() -> TestResult {
    use provenant_supervisor::census::ProcessObservation;

    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
        ProcessObservation::Present(fact) if !fact.zombie => {
            provenant_os::capture::CaptureSourceIdentity {
                attempt: id.to_string(),
                generation: RunStore::scope(&running).generation,
                boot: fact.identity.boot,
                pid: fact.identity.pid,
                native_start: fact.identity.native_start,
            }
        }
        observation => {
            return Err(format!("test process identity unavailable: {observation:?}").into());
        }
    };
    let route = running
        .applied_route
        .as_ref()
        .ok_or("applied route")?
        .clone();
    assert!(store.record_provider_session_ready(
        id,
        &route,
        "fixture-live-session",
        &source,
        &call,
    )?);
    let mut warning_only_change = route.clone();
    warning_only_change.availability_warning = Some("later warning".into());
    assert!(store.record_provider_session_ready(
        id,
        &warning_only_change,
        "fixture-live-session",
        &source,
        &call,
    )?);
    let ready = store
        .steer_session_ready(id, &call)?
        .ok_or("ready session missing")?;
    assert_eq!(ready.session.as_deref(), Some("fixture-live-session"));
    assert_eq!(ready.generation, source.generation);
    assert!(!ready.conflict);
    assert!(!ready.terminal_confirmed);
    assert!(store.record_provider_session_ready(
        id,
        &route,
        "different-live-session",
        &source,
        &call,
    )?);
    assert!(
        store
            .steer_session_ready(id, &call)?
            .is_some_and(|ready| ready.conflict)
    );

    let mut other_account = route.clone();
    other_account.account_key = Some("different-account".into());
    assert!(!store.record_provider_session_ready(
        id,
        &other_account,
        "fixture-live-session",
        &source,
        &call,
    )?);
    let mut other_process = source.clone();
    other_process.native_start = other_process.native_start.saturating_add(1);
    assert!(!store.record_provider_session_ready(
        id,
        &route,
        "fixture-live-session",
        &other_process,
        &call,
    )?);
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
// Invariant: a steer to a running lane is queued for the next turn and never
// stops or cancels the attempt, with or without a retained provider session.
#[test]
fn a_steer_to_a_running_lane_queues_the_turn_without_stopping_the_attempt() -> TestResult {
    use provenant_supervisor::census::ProcessObservation;

    for with_session in [false, true] {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let route = running
            .applied_route
            .as_ref()
            .ok_or("applied route")?
            .clone();

        if with_session {
            // A validated live session is the case that used to request a
            // `Steered` interrupt as soon as the steer was accepted.
            let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
                ProcessObservation::Present(fact) if !fact.zombie => {
                    provenant_os::capture::CaptureSourceIdentity {
                        attempt: id.to_string(),
                        generation: RunStore::scope(&running).generation,
                        boot: fact.identity.boot,
                        pid: fact.identity.pid,
                        native_start: fact.identity.native_start,
                    }
                }
                observation => {
                    return Err(
                        format!("test process identity unavailable: {observation:?}").into(),
                    );
                }
            };
            assert!(store.record_provider_session_ready(
                id,
                &route,
                "fixture-session",
                &source,
                &call
            )?);
            assert!(store.steer_session_available(id, &call)?);
        } else {
            // A Claude lane with no retained session still queues the turn;
            // readiness is evidence for delivery, not a gate on acceptance.
            assert!(!store.steer_session_available(id, &call)?);
        }

        let text = "Finish the scoped check at the next turn.";
        let operation = OperationId::parse(&format!("steer-running-{with_session}"))?;
        assert!(matches!(
            store.steer(id, operation, text, &call)?,
            SteerReceipt::Queued { .. }
        ));

        assert!(
            matches!(store.record(id, &call)?.state, AttemptState::Running { .. }),
            "a steer must leave the running attempt running"
        );
        let attempt = store.attempt_dir(id, false)?;
        assert!(
            crate::artifacts::read_sealed(&attempt, "stop-intent.json", RECORD_LIMIT)?.is_none(),
            "a steer must never write a stop intent"
        );
        let intents = store.steer_intent(id, &call)?;
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].text, text);
    }
    Ok(())
}

#[test]
fn explicit_cancel_after_delivered_steer_keeps_the_stop_and_blocks_successor() -> TestResult {
    use provenant_supervisor::census::ProcessObservation;

    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let text = "Keep the existing session and finish the scoped check.";
    assert!(matches!(
        store.steer(
            id,
            OperationId::parse("steer-before-explicit-cancel")?,
            text,
            &call,
        )?,
        SteerReceipt::Queued { .. }
    ));
    let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
        ProcessObservation::Present(fact) if !fact.zombie => {
            provenant_os::capture::CaptureSourceIdentity {
                attempt: id.to_string(),
                generation: RunStore::scope(&running).generation,
                boot: fact.identity.boot,
                pid: fact.identity.pid,
                native_start: fact.identity.native_start,
            }
        }
        observation => {
            return Err(format!("test process identity unavailable: {observation:?}").into());
        }
    };
    let route = running
        .applied_route
        .as_ref()
        .ok_or("applied route")?
        .clone();
    assert!(store.record_provider_session_ready(id, &route, "fixture-session", &source, &call,)?);

    let attempt = store.attempt_dir(id, false)?;
    assert!(
        crate::artifacts::read_sealed(&attempt, "stop-intent.json", RECORD_LIMIT)?.is_none(),
        "a steer never interrupts the running attempt, even with a live session"
    );
    // Model a steered stop delivered independently of the steer (a legacy
    // intent still awaiting settlement). What this test pins is that a later
    // explicit cancel keeps the first delivery fact and blocks continuation.
    provenant_supervisor::custody::request_stop(
        attempt.try_clone()?,
        RunStore::scope(&running),
        context.actor_id,
        StopReason::Steered,
    )?;
    let steered: provenant_supervisor::custody::StopIntent = crate::db::decode(
        &crate::artifacts::read_sealed(&attempt, "stop-intent.json", RECORD_LIMIT)?
            .ok_or("steered stop intent missing")?,
    )?;
    assert_eq!(steered.reason, StopReason::Steered);
    assert!(matches!(
        store.cancel(id, "owner cancellation after steer", &call)?,
        CancelReceipt::Requested
    ));
    let explicit: provenant_supervisor::custody::StopIntent = crate::db::decode(
        &crate::artifacts::read_sealed(&attempt, "stop-intent.json", RECORD_LIMIT)?
            .ok_or("explicit cancel request missing")?,
    )?;
    assert_eq!(explicit.reason, StopReason::ExplicitCancellation);
    assert_eq!(explicit.scope, RunStore::scope(&running));
    assert_eq!(explicit.requester, context.actor_id);

    // The provider was already interrupted by the first Steered signal.
    // Its sealed reason and delivery fact remain unchanged after the later
    // explicit cancellation request replaced cancel.json.
    let mut exit = crate::tests::exit();
    exit.scope = RunStore::scope(&running);
    exit.stop_reason = Some(StopReason::Steered);
    exit.cancel_delivered_before_exit = Some(true);
    crate::dispatch_tests::seal_exit(&store, &exit)?;
    let mut publication = crate::tests::publication();
    publication.fact.outcome = Outcome::Cancelled;
    publication.fact.cause = Some(CauseKind::Steered);
    publication.resolved_route = Some(route.clone());
    publication.applied_route = Some(route.clone());
    publication.observed_route = Some(route);
    publication.session = Some("fixture-session".into());
    let terminal = store.publish_terminal(id, running.revision, &publication, &exit, &call)?;
    assert_eq!(
        terminal.cancellation_disposition,
        Some(provenant_records::run::CancellationDisposition::Unknown),
        "the Steered delivery fact does not prove delivery of the later cancel"
    );
    assert!(matches!(
        terminal.state,
        AttemptState::Terminal {
            outcome: Outcome::Cancelled,
            cause: Some(CauseKind::Steered),
            ..
        }
    ));
    assert_eq!(
        store.prepare_steer_continuation(id, &call)?,
        None,
        "the accepted explicit stop must not launch a same-session successor"
    );
    assert_eq!(store.record(id, &call)?.superseded_by, None);
    let status = store
        .steering_status(id, &call)?
        .ok_or("steering status missing")?;
    assert_eq!(
        status.state,
        provenant_records::run::SteeringState::NotContinued
    );
    assert_eq!(
        status.cause,
        Some(provenant_records::run::SteeringCause::TerminalStop)
    );
    let ledger = store.steer_ledger(id, &call)?;
    assert_eq!(ledger.len(), 1);
    assert_eq!(
        ledger[0].0.text, text,
        "accepted text stays private and durable"
    );
    assert!(ledger[0].1, "a non-continuable stop resolves the intent");
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn graph_cancel_and_steer_admission_have_one_ordered_winner() -> TestResult {
    for admission_first in [false, true] {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        let parent = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(parent, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let running = store
            .claim(parent, &clock, &call)?
            .ok_or("parent not admitted")?;
        assert!(matches!(
            store.steer(
                parent,
                OperationId::parse("steer-cancel-admission-order")?,
                "Continue this task in its existing session.",
                &call,
            )?,
            SteerReceipt::Queued { .. }
        ));
        let mut exit = crate::tests::exit();
        exit.scope = RunStore::scope(&running);
        crate::dispatch_tests::seal_exit(&store, &exit)?;
        let mut publication = crate::tests::publication();
        // A finished task has no follow-up (#3987). Keep the parent
        // continuable so this test still exercises the ordering race between
        // admitting its queued successor and cancelling the graph.
        publication.fact.outcome = Outcome::Uncommitted;
        publication.started_at = running.started_at;
        publication.finished_at = call.clock.wall_now()?;
        publication.session = Some("fixture-session".into());
        store.publish_terminal(parent, running.revision, &publication, &exit, &call)?;

        if admission_first {
            let child = store
                .prepare_steer_continuation(parent, &call)?
                .ok_or("continuation was not admitted")?;
            let (cancelled, receipt) =
                store.cancel_graph(parent, "cancel current graph continuation", &call)?;
            assert_eq!(cancelled, child);
            assert_eq!(receipt, CancelReceipt::Cancelled);
            assert!(matches!(
                store.record(child, &call)?.state,
                AttemptState::Terminal {
                    outcome: Outcome::Cancelled,
                    ..
                }
            ));
            assert_eq!(
                store.record(parent, &call)?.steering.unwrap().state,
                provenant_records::run::SteeringState::NotContinued,
                "a cancelled, never-launched child closes the pending intent"
            );
        } else {
            let (cancelled, receipt) =
                store.cancel_graph(parent, "cancel pending continuation", &call)?;
            assert_eq!(cancelled, parent);
            assert_eq!(receipt, CancelReceipt::AlreadyTerminal);
            let attempt = store.attempt_dir(parent, false)?;
            let request: provenant_supervisor::custody::StopIntent = crate::db::decode(
                &crate::artifacts::read_sealed(&attempt, "stop-intent.json", RECORD_LIMIT)?
                    .ok_or("terminal graph cancel intent was not persisted")?,
            )?;
            assert_eq!(request.scope, RunStore::scope(&running));
            assert_eq!(request.requester, context.actor_id);
            assert_eq!(request.reason, StopReason::ExplicitCancellation);
            // The stop resolves the steering at once, cause shown; it
            // never waits on a continuation reaction that may be parked
            // behind the steered writer's custody release.
            let resolved = store.record(parent, &call)?.steering.ok_or("steering")?;
            assert_eq!(
                resolved.state,
                provenant_records::run::SteeringState::NotContinued
            );
            assert_eq!(
                resolved.cause,
                Some(provenant_records::run::SteeringCause::TerminalStop)
            );
            assert_eq!(
                store.prepare_steer_continuation(parent, &call)?,
                None,
                "the serialized gate observes a terminal graph cancel"
            );
            assert_eq!(store.record(parent, &call)?.superseded_by, None);
            let status = store.record(parent, &call)?.steering.unwrap();
            assert_eq!(
                status.state,
                provenant_records::run::SteeringState::NotContinued
            );
            assert_eq!(
                status.cause,
                Some(provenant_records::run::SteeringCause::TerminalStop)
            );
        }
    }
    Ok(())
}

// Spec: docs/specs/run.md#stop-intent
#[test]
fn stop_settles_a_sealed_running_attempt_without_a_holder() -> TestResult {
    use provenant_core::Clock as _;
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = crate::tests::input();
    run.created_at = clock.wall_now()?;
    run.request.task.writer = Some(WriterRequest {
        extra: Default::default(),
        custody_project: Some(context.project_id),
        project: temp.path().join("checkout"),
        worktree: temp.path().join("checkout/.worktrees/settling"),
    });
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let running = store
        .claim(id, &clock, &call)?
        .ok_or("attempt not admitted")?;
    let attempt = store.attempt_dir(id, false)?;
    crate::artifacts::seal(&attempt, "stdout.log", b"captured answer", CAPTURE_LIMIT)?;
    crate::artifacts::seal(&attempt, "handoff.json", b"captured handoff", RECORD_LIMIT)?;
    // The provider sealed its own exit while the record stayed Running. This,
    // not a hand-written settling row, is what status reports as settling.
    let mut exit = crate::tests::exit();
    exit.scope = RunStore::scope(&running);
    crate::dispatch_tests::seal_exit(&store, &exit)?;
    assert_eq!(
        attempt_presence(context.state_root.as_path(), &store.record(id, &call)?),
        Some(AttemptPresence::Settling),
        "a sealed Running attempt must be the settling status sees"
    );

    let (cancelled, receipt) = store.cancel_graph(id, "stop the vanished provider", &call)?;
    assert_eq!(cancelled, id);
    assert_eq!(receipt, CancelReceipt::Cancelled);
    let settled = store.record(id, &call)?;
    assert!(
        matches!(
            &settled.state,
            AttemptState::Terminal {
                outcome: Outcome::Cancelled,
                ..
            }
        ),
        "stop must settle a vanished provider as cancelled: {settled:?}"
    );
    assert_eq!(
        settled.terminal_basis,
        Some(TerminalBasis::ProcessExit)
    );
    assert_eq!(
        settled.cancellation_disposition,
        Some(CancellationDisposition::Unknown),
        "a stop with no delivery proof must not claim effectiveness"
    );
    assert!(
        settled.exit_evidence.is_some(),
        "the sealed exit was dropped from the terminal decision"
    );
    // Every captured artifact survives the terminal commit.
    assert_eq!(
        crate::artifacts::read_sealed(&attempt, "stdout.log", CAPTURE_LIMIT)?.as_deref(),
        Some(b"captured answer".as_slice())
    );
    assert_eq!(
        crate::artifacts::read_sealed(&attempt, "handoff.json", RECORD_LIMIT)?.as_deref(),
        Some(b"captured handoff".as_slice())
    );
    assert!(
        crate::artifacts::read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some(),
        "sealed exit was discarded"
    );
    assert_eq!(store.replay_terminal(id, &call)?.state, settled.state);
    assert_eq!(
        store.cancel_graph(id, "stop the vanished provider again", &call)?,
        (id, CancelReceipt::AlreadyTerminal)
    );
    Ok(())
}

// Spec: docs/specs/run.md#stop-intent
#[test]
fn stop_asks_a_live_holder_to_settle_its_sealed_exit() -> TestResult {
    use provenant_core::Clock as _;
    use provenant_supervisor::census::ProcessObservation;
    use provenant_supervisor::custody::HolderPresence;
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = crate::tests::input();
    run.created_at = clock.wall_now()?;
    run.request.task.writer = Some(WriterRequest {
        extra: Default::default(),
        custody_project: Some(context.project_id),
        project: temp.path().join("checkout"),
        worktree: temp.path().join("checkout/.worktrees/held"),
    });
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let running = store
        .claim(id, &clock, &call)?
        .ok_or("attempt not admitted")?;
    let attempt = store.attempt_dir(id, false)?;
    let mut exit = crate::tests::exit();
    exit.scope = RunStore::scope(&running);
    crate::dispatch_tests::seal_exit(&store, &exit)?;
    let ProcessObservation::Present(fact) =
        SystemProcessObserver.observe(i64::from(std::process::id()))
    else {
        return Err("test process identity unavailable".into());
    };
    crate::artifacts::seal(
        &attempt,
        "process.json",
        &serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "scope": RunStore::scope(&running),
            "publication_pending": false,
            "census_complete": true,
            "guardian": fact.identity,
        }))?,
        RECORD_LIMIT,
    )?;
    assert_eq!(
        custody::observe_holder(&attempt, RunStore::scope(&running), &SystemProcessObserver),
        HolderPresence::Live,
        "the fixture must prove a live holder before asserting the stop path"
    );
    assert_eq!(
        store.cancel_graph(id, "stop the held attempt", &call)?,
        (id, CancelReceipt::Requested),
        "a live holder owns settlement and must be asked to stop"
    );
    assert!(matches!(
        store.record(id, &call)?.state,
        AttemptState::Running { .. }
    ));
    Ok(())
}

// Spec: docs/specs/run-settlement.md#settlement
#[test]
fn writer_lock_contention_waits_without_blocking_settlement() -> TestResult {
    use provenant_core::Clock as _;
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let diagnostic = || -> std::result::Result<String, Box<dyn std::error::Error>> {
        Ok(crate::settlement::diagnostic_for(
            context.state_root.as_path(),
            &store.record(id, &call)?,
            Some(clock.wall_now()?),
        )
        .ok_or("settlement diagnostic")?)
    };
    let busy = Reconciled::Uncertain {
        reason: UncertainReason::WriterLockBusy {
            lock: "/slot/writer.lock".into(),
            holder: "attempt X (guardian live, reserved 2s ago)".into(),
            holder_live: true,
        },
    };
    for _ in 0..=crate::settlement::LIMIT {
        assert_eq!(
            store.track_reconcile_progress(id, busy.clone(), &call),
            busy
        );
    }
    let reason = diagnostic()?;
    assert!(!reason.contains("settlement held"), "{reason}");
    assert!(reason.contains("retry pending"), "{reason}");
    assert!(reason.contains("held by attempt X"), "{reason}");
    // A real failure still spends the budget and blocks at the limit.
    let failed = Reconciled::Uncertain {
        reason: UncertainReason::ReconcileFailed {
            error: "custody reattach failed".into(),
        },
    };
    for _ in 0..crate::settlement::LIMIT {
        store.track_reconcile_progress(id, failed.clone(), &call);
    }
    assert!(diagnostic()?.contains("settlement held"));
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn steered_writer_continuation_waits_for_custody_and_stop_resolves_it() -> TestResult {
    use crate::delivery_tests::{PrepareFixture, registered_landed_writer_fixture};
    use provenant_records::run::{SteeringCause, SteeringState};
    for stop in [false, true] {
        let fixture = PrepareFixture::new()?;
        let execution = registered_landed_writer_fixture(&fixture)?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&execution, &call)?;
        let mut run = crate::tests::input();
        run.request.task.writer = Some(fixture.writer.clone());
        let parent = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(parent, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let running = store
            .claim(parent, &clock, &call)?
            .ok_or("writer not admitted")?;
        assert!(matches!(
            store.steer(
                parent,
                OperationId::parse("steer-retained-writer")?,
                "Continue in the retained worktree.",
                &call,
            )?,
            SteerReceipt::Queued { .. }
        ));
        let mut exit = crate::tests::exit();
        exit.scope = RunStore::scope(&running);
        crate::dispatch_tests::seal_exit(&store, &exit)?;
        let mut publication = crate::tests::publication();
        // A finished task has no follow-up (#3987). Leave retained edits so
        // the queued turn still reaches the custody gate this test pins.
        publication.fact.outcome = Outcome::Uncommitted;
        publication.started_at = running.started_at;
        publication.finished_at = call.clock.wall_now()?;
        publication.session = Some("fixture-session".into());
        store.publish_terminal(parent, running.revision, &publication, &exit, &call)?;

        // Settlement has not released the retained worktree's custody.
        let attempt = store.attempt_dir(parent, false)?;
        let custody = attempt.try_lock(
            std::ffi::OsStr::new(provenant_records::AttemptFile::CustodyLock.name()),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        assert!(matches!(
            store.prepare_steer_continuation(parent, &call),
            Err(RunError::SteerAwaitingCustody)
        ));
        let waiting = store.record(parent, &call)?;
        assert!(
            waiting
                .steering
                .as_ref()
                .is_some_and(|steering| steering.state == SteeringState::Queued),
            "a continuation waiting on custody keeps the accepted steering"
        );
        assert_eq!(waiting.superseded_by, None);

        if stop {
            let (cancelled, receipt) =
                store.cancel_graph(parent, "stop the steered writer", &call)?;
            assert_eq!(cancelled, parent);
            assert_eq!(receipt, CancelReceipt::AlreadyTerminal);
            let resolved = store.record(parent, &call)?.steering.ok_or("steering")?;
            assert_eq!(resolved.state, SteeringState::NotContinued);
            assert_eq!(resolved.cause, Some(SteeringCause::TerminalStop));
            drop(custody);
            assert_eq!(store.prepare_steer_continuation(parent, &call)?, None);
            assert_eq!(store.record(parent, &call)?.superseded_by, None);
        } else {
            drop(custody);
            let child = store
                .prepare_steer_continuation(parent, &call)?
                .ok_or("continuation not admitted after custody release")?;
            assert_eq!(store.record(parent, &call)?.superseded_by, Some(child));
        }
    }
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn steer_session_disagreement_keeps_accepted_text_and_refuses_fresh_fallback() -> TestResult {
    use provenant_supervisor::census::ProcessObservation;

    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let mut running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let operation = OperationId::parse("steer-session-disagreement")?;
    let accepted = store.steer(id, operation, "Keep this input.", &call)?;
    assert!(matches!(accepted, SteerReceipt::Queued { .. }));
    let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
        ProcessObservation::Present(fact) if !fact.zombie => {
            provenant_os::capture::CaptureSourceIdentity {
                attempt: id.to_string(),
                generation: RunStore::scope(&running).generation,
                boot: fact.identity.boot,
                pid: fact.identity.pid,
                native_start: fact.identity.native_start,
            }
        }
        observation => {
            return Err(format!("test process identity unavailable: {observation:?}").into());
        }
    };
    let route = running
        .applied_route
        .as_ref()
        .ok_or("applied route")?
        .clone();
    assert!(store.record_provider_session_ready(id, &route, "live-session", &source, &call,)?);
    assert!(store.record_provider_identity_conflict(id, &route, &source, &call)?);
    let terminal_provider = provenant_supervisor::census::ProcessIdentity {
        boot: source.boot.clone(),
        pid: source.pid,
        native_start: source.native_start,
        group_id: source.pid,
        session_id: source.pid,
    };
    store.confirm_steer_terminal_process(
        id,
        RunStore::scope(&running),
        Some(&terminal_provider),
        &call,
    )?;
    assert!(
        store
            .steer_session_ready(id, &call)?
            .is_some_and(|ready| { ready.conflict && ready.terminal_confirmed })
    );
    let mut exit = crate::tests::exit();
    exit.scope = RunStore::scope(&running);
    crate::dispatch_tests::seal_exit(&store, &exit)?;
    let mut publication = crate::tests::publication();
    // A finished task has no follow-up (#3987). Keep the session disagreement
    // reachable by leaving retained edits for the queued turn.
    publication.fact.outcome = Outcome::Uncommitted;
    publication.session = Some("live-session".into());
    running = store.publish_terminal(id, running.revision, &publication, &exit, &call)?;
    assert!(matches!(running.state, AttemptState::Terminal { .. }));
    assert!(matches!(
        store.prepare_steer_continuation(id, &call),
        Err(RunError::SteerSessionMismatch)
    ));
    assert!(matches!(
        store.prepare_fresh_steer_continuation(id, &call),
        Err(RunError::SteerSessionMismatch)
    ));
    store.record_steer_not_continued(
        id,
        provenant_records::run::SteeringCause::SessionMismatch,
        &call,
    )?;
    let ledger = store.steer_ledger(id, &call)?;
    assert_eq!(ledger[0].0.text, "Keep this input.");
    assert!(
        ledger[0].1,
        "the original private text remains durably stored"
    );
    let status = store.record(id, &call)?.steering.ok_or("steering status")?;
    assert_eq!(
        status.state,
        provenant_records::run::SteeringState::NotContinued
    );
    assert_eq!(
        status.cause,
        Some(provenant_records::run::SteeringCause::SessionMismatch)
    );
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn first_frame_identity_mismatch_without_session_event_fences_natural_steer() -> TestResult {
    use provenant_supervisor::census::ProcessObservation;

    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let mut running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let accepted = store.steer(
        id,
        OperationId::parse("steer-first-frame-mismatch")?,
        "Keep the accepted work, then verify its provider identity.",
        &call,
    )?;
    assert!(matches!(accepted, SteerReceipt::Queued { .. }));
    let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
        ProcessObservation::Present(fact) if !fact.zombie => {
            provenant_os::capture::CaptureSourceIdentity {
                attempt: id.to_string(),
                generation: RunStore::scope(&running).generation,
                boot: fact.identity.boot,
                pid: fact.identity.pid,
                native_start: fact.identity.native_start,
            }
        }
        observation => {
            return Err(format!("test process identity unavailable: {observation:?}").into());
        }
    };
    let route = running
        .applied_route
        .as_ref()
        .ok_or("applied route")?
        .clone();
    // Agy may reject its first init frame on Model after decoding Session;
    // the adapter then returns neither event. The host still records a
    // route/process-bound fence for that exact admitted candidate.
    assert!(store.record_provider_identity_conflict(id, &route, &source, &call)?);
    let ready = store
        .steer_session_ready(id, &call)?
        .ok_or("identity conflict row missing")?;
    assert!(ready.conflict);
    assert!(ready.session.is_none());
    assert!(!ready.terminal_confirmed);

    let mut exit = crate::tests::exit();
    exit.scope = RunStore::scope(&running);
    crate::dispatch_tests::seal_exit(&store, &exit)?;
    let mut publication = crate::tests::publication();
    // A finished task has no follow-up (#3987). Leave retained edits so the
    // first-frame identity fence is still evaluated for the queued turn.
    publication.fact.outcome = Outcome::Uncommitted;
    publication.session = Some("session-from-final-decoder".into());
    running = store.publish_terminal(id, running.revision, &publication, &exit, &call)?;
    assert!(matches!(running.state, AttemptState::Terminal { .. }));
    assert!(matches!(
        store.prepare_steer_continuation(id, &call),
        Err(RunError::SteerSessionMismatch)
    ));
    assert_eq!(
        store.steer_intent(id, &call)?[0].text,
        "Keep the accepted work, then verify its provider identity."
    );
    Ok(())
}

// Spec: docs/specs/fabric.md#session-ready-steering
#[test]
fn steer_stop_conflict_preserves_each_durable_intent_and_receipt() -> TestResult {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;

    let accepted_operation = OperationId::parse("steer-accepted")?;
    let accepted = store.steer(
        id,
        accepted_operation.clone(),
        "Keep this instruction.",
        &call,
    )?;
    let accepted_message = match accepted {
        SteerReceipt::Queued { message_id } => message_id,
        receipt => return Err(format!("expected accepted receipt, got {receipt:?}").into()),
    };
    assert!(matches!(
        store.cancel(id, "owner cancellation", &call)?,
        CancelReceipt::Requested
    ));
    assert_eq!(
        store.steer(id, accepted_operation, "Keep this instruction.", &call,)?,
        SteerReceipt::AlreadyQueued {
            message_id: accepted_message
        }
    );
    let later_message = match store.steer(
        id,
        OperationId::parse("steer-after-stop")?,
        "Keep this second instruction too.",
        &call,
    )? {
        SteerReceipt::Queued { message_id } => message_id,
        receipt => return Err(format!("expected durable queue, got {receipt:?}").into()),
    };
    assert_ne!(later_message, accepted_message);
    let intents = store.steer_intent(id, &call)?;
    assert_eq!(intents.len(), 2);
    assert_eq!(intents[0].text, "Keep this instruction.");
    assert_eq!(intents[1].text, "Keep this second instruction too.");
    Ok(())
}

#[test]
// Spec: docs/specs/supervisor.md#attempt-silence-is-reported-action-is-opt-in
fn stall_age_uses_output_or_start_and_requires_live_presence() -> TestResult {
    let temp = tempfile::tempdir()?;
    let clock = crate::tests::clock();
    let cancel = provenant_core::CancelToken::new();
    let call = crate::tests::call(&clock, &cancel);
    let context = crate::tests::context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = crate::tests::input();
    store.create(&run, &call)?;
    store.mark_starting(
        &store.record(run.attempt_id, &call)?,
        &crate::tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    // Silence is reported only for a running attempt with live custody; a
    // starting attempt has not handed off to a provider yet.
    let mut record = store
        .claim(run.attempt_id, &clock, &call)?
        .ok_or("not admitted")?;
    let last = WallTime::from_micros(1_000_000)?;
    let now = WallTime::from_micros(32_000_000)?;
    record.last_output_at = Some(last);
    record.limits = Some(provenant_records::LaneLimits {
        stall_after_seconds: Some(provenant_records::Limit::new(
            30,
            provenant_records::Layer::Host,
            provenant_records::LimitStrength::Default,
        )),
        ..Default::default()
    });
    assert_eq!(stall_after_seconds(&record), 30);
    assert_eq!(stall_fact(&record, now), Some((30, last)));
    assert!(stalled_at(&record, now, true));
    // An unobserved or absent writer is a custody problem, not silence.
    assert!(!stalled_at(&record, now, false));
    // A provider that never emits output is measured from its start, not queue.
    record.last_output_at = None;
    record.started_at = Some(last);
    assert_eq!(stall_fact(&record, now), Some((30, last)));
    assert!(stalled_at(&record, now, true));
    record.started_at = None;
    assert_eq!(stall_fact(&record, now), None);
    // Zero disables the report rather than stalling every live attempt.
    record.last_output_at = Some(last);
    record.limits = Some(provenant_records::LaneLimits {
        stall_after_seconds: Some(provenant_records::Limit::new(
            0,
            provenant_records::Layer::Host,
            provenant_records::LimitStrength::Default,
        )),
        ..Default::default()
    });
    assert!(!stalled_at(&record, now, true));
    Ok(())
}
