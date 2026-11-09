use super::*;

#[test]
fn blocked_parent_reclaim_preserves_work_and_reports_admission_wait() -> TestResult {
    child_wait_lease_cycle(true)
}
#[test]
fn child_stores_share_the_host_ledger_and_recovery_frees_a_lease() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let now = at_micros(100);
    let base = std::fs::canonicalize(temp.path())?;
    let host = base.join("host");
    let mut stores = Vec::new();
    std::fs::create_dir_all(&host)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        host.join("config.toml"),
        "schema_version=1\n[domains.fabric.lanes]\nmax_lanes = 32\nmax_project_lanes = 32\n",
    )?;
    for name in ["first", "second"] {
        std::fs::create_dir(base.join(name))?;
        let mut store =
            RunStore::open(&context(&base.join(name))?, &call)?.with_writer_registry(&host);
        store.set_admission_policy(AdmissionPolicy {
            startup_spacing_ms: 0,
            read_only_spacing_ms: 0,
            ..AdmissionPolicy::default()
        });
        stores.push(store);
    }
    // Each store fills half the project's host-wide limit, in runs that stay
    // inside one batch's concurrency.
    let per_run = PROJECT_ACTIVE / 4;
    let batches = [
        batch(1, per_run),
        batch(4, per_run),
        batch(2, per_run),
        batch(5, per_run),
    ];
    for (store, run) in stores.iter().flat_map(|s| [s, s]).zip(&batches) {
        store.create(run, &call)?;
        for attempt in &run.attempts {
            store.mark_starting(
                &store.record(attempt.attempt_id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            store
                .claim(attempt.attempt_id, &now, &call)?
                .ok_or("within the host limit")?;
        }
    }
    // Counted per store, the second store would admit another attempt.
    let extra = batch(3, 1);
    let waiting = extra.attempts[0].attempt_id;
    stores[1].create(&extra, &call)?;
    stores[1].mark_starting(
        &stores[1].record(waiting, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    assert!(
        stores[1].claim(waiting, &now, &call)?.is_none(),
        "both child stores count against one project limit"
    );
    // The first attempt's owner dies; recovery commits its terminal from the
    // sealed exit, as reconcile does, and that frees its host lease.
    let dead = batches[0].attempts[0].attempt_id;
    let dir = stores[0].attempt_dir(batches[0].run_id, dead, false)?;
    artifacts::seal(&dir, "stdout.log", b"partial", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope.run = batches[0].run_id;
    observed.scope.attempt = dead;
    observed.scope.generation = 1;
    observed.termination = Some(Termination::Signal(9));
    seal_exit(&stores[0], &observed)?;
    let fake = Fake {
        refuse: false,
        observed: Some(route()),
        ..Fake::default()
    };
    stores[0].finish(dead, &dir, &observed, &fake, &call)?;
    let admitted = stores[1]
        .claim(waiting, &now, &call)?
        .ok_or("recovered lease not freed")?;
    assert_eq!(
        admitted.state,
        AttemptState::Running {
            extra: Default::default()
        }
    );
    Ok(())
}
#[test]
fn prepare_refusal_is_terminal_without_custody() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let fake = Fake {
        refuse: true,
        observed: None,
        ..Fake::default()
    };
    let started = store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
    assert!(
        matches!(&started[..], [Started::Refused { reason, .. }] if reason.contains("disabled"))
    );
    let prompt = fake.prompt.borrow();
    let path = store.context.state_root.as_path().join(format!(
        "runs/{}/attempts/{}",
        run.run_id, run.attempts[0].attempt_id
    ));
    assert!(prompt.contains(&path.join("result.md").display().to_string()));
    assert!(prompt.contains(&path.join("process-exit.json").display().to_string()));
    assert!(prompt.contains("only durable outputs"));
    assert!(prompt.contains("Files in $TMPDIR are discarded when the attempt ends."));
    assert!(prompt.contains("Provider history and handoff progress are kept."));
    assert!(prompt.ends_with("exact prompt"));
    let fact = store.attempt(run.attempts[0].attempt_id, &call)?;
    assert_eq!(
        fact.terminal_state,
        provenant_records::AttemptOutcome::Rejected
    );
    assert!(fact.started_at.is_none());
    assert_eq!(store.run(run.run_id, &call)?.closure, Closure::Closed);
    assert!(
        store
            .start(run.run_id, &fake, &|_, _| unreachable!(), &call)?
            .is_empty()
    );
    Ok(())
}
#[test]
fn missing_stored_prompt_ends_the_attempt_rejected_with_the_cause() -> TestResult {
    assert_unreadable_prompt_rejected(|prompt| {
        std::fs::remove_file(prompt)?;
        Ok(())
    })
}
#[test]
fn mode_zero_stored_prompt_ends_the_attempt_rejected_with_the_cause() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    assert_unreadable_prompt_rejected(|prompt| {
        std::fs::set_permissions(prompt, std::fs::Permissions::from_mode(0o000))?;
        Ok(())
    })
}
#[test]
fn holder_release_reclaims_temporary_after_verified_settlement() -> TestResult {
    holder_release_temporary_fixture(true)
}
#[test]
fn holder_release_retains_temporary_when_verification_fails() -> TestResult {
    holder_release_temporary_fixture(false)
}
#[test]
fn publication_holder_commits_decoded_success_only_with_exit_zero_and_observed_identity()
-> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
        read_only_spacing_ms: 0,
        ..AdmissionPolicy::default()
    });
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let now = at_micros(100);
    let fake = Fake {
        refuse: false,
        observed: Some(route()),
        ..Fake::default()
    };
    for (index, code) in [(0, 0), (1, 1)] {
        let id = batch.attempts[index].attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &now, &call)?.ok_or("not admitted")?;
        let dir = store.attempt_dir(batch.run_id, id, false)?;
        artifacts::seal(&dir, "stdout.log", DONE_ANSWER.as_bytes(), CAPTURE_LIMIT)?;
        let mut observed = exit();
        observed.scope.attempt = id;
        observed.scope.generation = 1;
        observed.terminal_observed = false;
        observed.termination = Some(Termination::Exit(code));
        if code == 0 {
            observed.cancel_delivered_before_exit = None;
            artifacts::seal(
                &dir,
                "cancel.json",
                &crate::db::bytes(&serde_json::json!({
                    "schema_version": 1, "scope": observed.scope,
                    "requester": store.context.actor_id, "request_id": "legacy-late-cancel",
                "reason": provenant_supervisor::machine::StopReason::ExplicitCancellation
                }))?,
                RECORD_LIMIT,
            )?;
        }
        seal_exit(&store, &observed)?;
        let record = store.finish(id, &dir, &observed, &fake, &call)?;
        if code == 0 {
            assert_eq!(
                record.cancellation_disposition,
                Some(CancellationDisposition::Unknown)
            );
        }
        let AttemptState::Terminal { outcome, .. } = record.state else {
            return Err("not terminal".into());
        };
        assert_eq!(
            outcome,
            if code == 0 {
                Outcome::Succeeded
            } else {
                Outcome::Failed
            }
        );
        assert_eq!(store.finish(id, &dir, &observed, &fake, &call)?, record);
        provenant_supervisor::custody::publish_release(&dir, observed.scope)?;
        provenant_supervisor::custody::publish_release(&dir, observed.scope)?;
    }
    assert_eq!(fake.decoded.get(), 2);
    let fact = store.attempt(batch.attempts[0].attempt_id, &call)?;
    assert_eq!(
        fact.output_refs[0].digest,
        CanonicalV1::file_digest(DONE_ANSWER.as_bytes())
    );
    assert_eq!(store.run(batch.run_id, &call)?.closure, Closure::Closed);
    Ok(())
}
#[test]
fn never_spawned_exit_is_a_refusal_with_its_reason() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let mut observed = exit();
    observed.scope.generation = 1;
    observed.never_spawned = true;
    observed.termination = None;
    observed.terminal_observed = false;
    observed.warnings = vec!["admission refused".into()];
    observed.launch_refusal = Some(provenant_records::diagnostic::ResourceFact::LanePorts {
        blocks: 1,
        origin: provenant_records::diagnostic::PortSettingSource::Configuration,
    });
    observed.settlement_evidence = provenant_supervisor::custody::SettlementEvidence::NeverSpawned;
    seal_exit(&store, &observed)?;
    let dir = store.attempt_dir(run.run_id, id, false)?;
    let fake = Fake {
        refuse: false,
        observed: None,
        ..Fake::default()
    };
    store.finish(id, &dir, &observed, &fake, &call)?;
    let fact = store.attempt(id, &call)?;
    assert_eq!(
        fact.terminal_state,
        provenant_records::AttemptOutcome::Rejected
    );
    assert_eq!(fact.reason.as_deref(), Some("admission refused"));
    let record = store.record(id, &call)?;
    let (_, reason) = provenant_records::wire::lane::LaneRow::state_and_reason(&record.state);
    let reason = reason.ok_or("missing projected refusal")?;
    assert!(reason.contains("domains.fabric.lanes.lane_port_blocks"));
    assert!(reason.contains("host configuration"));
    assert_eq!(fake.decoded.get(), 0);
    Ok(())
}
#[test]
fn private_temporary_directories_are_removed_after_terminal_and_withdrawal() -> TestResult {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = two_tasks(2);
    store.create(&run, &call)?;
    let outside = temp.path().join("outside.txt");
    std::fs::write(&outside, "keep")?;

    let withdrawn = run.attempts[0].attempt_id;
    let withdrawn_tmp = store.temporary_path(withdrawn, &call)?;
    std::fs::write(withdrawn_tmp.join("provider.tmp"), "discard")?;
    symlink(&outside, withdrawn_tmp.join("outside-link"))?;
    store.withdraw(
        withdrawn,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Cancelled,
        "cancelled before start",
        WallTime::from_micros(150)?,
        &call,
    )?;
    assert!(!withdrawn_tmp.exists());

    let terminal = run.attempts[1].attempt_id;
    let terminal_tmp = store.temporary_path(terminal, &call)?;
    std::fs::write(terminal_tmp.join("provider.tmp"), "discard")?;
    symlink(&outside, terminal_tmp.join("outside-link"))?;
    let mut observed = exit();
    observed.scope.attempt = terminal;
    observed.termination = Some(Termination::Exit(1));
    seal_exit(&store, &observed)?;
    let publication = TerminalPublication {
        terminal_cause: None,
        terminal_note: None,
        outcome: Outcome::Failed,
        finished_at: WallTime::from_micros(160)?,
        started_at: Some(WallTime::from_micros(120)?),
        provider_ms: Some(1),
        reason: Some("fixture failure".into()),
        result: "",
        complete: true,
        redacted: false,
        resolved_route: Some(route()),
        applied_route: Some(route()),
        observed_route: Some(route()),
        usage: UsageRecord::default(),
        session: None,
        partial_signals: Vec::new(),
    };
    store.publish_terminal(terminal, 1, &publication, &observed, &call)?;
    assert!(!terminal_tmp.exists());
    assert_eq!(std::fs::read_to_string(outside)?, "keep");
    Ok(())
}
#[test]
fn writer_private_temporary_directories_are_removed_after_terminal_and_withdrawal() -> TestResult {
    for withdrawn in [false, true] {
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
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let scratch = artifacts::subdir(&attempt, std::ffi::OsStr::new("scratch"), true)?;
        let temporary = store.temporary_path(id, &call)?;
        let candidate = temporary.join("candidate-0");
        std::fs::create_dir(&candidate)?;
        std::fs::write(candidate.join("provider.tmp"), "discard")?;
        if withdrawn {
            store.withdraw(
                id,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "cancelled before start",
                WallTime::from_micros(150)?,
                &call,
            )?;
        } else {
            let observed = exit();
            seal_exit(&store, &observed)?;
            store.publish_terminal(id, 1, &super::tests::publication(), &observed, &call)?;
        }
        // The launch/guardian settlement edge removes empty probe staging.
        drop(scratch);
        crate::writer::settle_launch_staging(&attempt)?;
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
        assert!(
            attempt
                .metadata_at(std::ffi::OsStr::new("scratch"), false)
                .is_err_and(|e| e.is_not_found())
        );
        assert!(!temporary.exists());
    }
    Ok(())
}
#[test]
fn cancel_ends_queued_work_and_stops_admission_before_spawn() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let [queued, starting] = [batch.attempts[0].attempt_id, batch.attempts[1].attempt_id];
    let op = provenant_core::OperationId::parse("stop")?;
    store.bind_cancel(&op, &queued.to_string(), &call)?;
    store.bind_cancel(&op, &queued.to_string(), &call)?;
    assert!(matches!(
        store.bind_cancel(&op, &starting.to_string(), &call),
        Err(RunError::OperationConflict)
    ));

    assert_eq!(
        store.cancel(queued, "stop", &call)?,
        CancelReceipt::Cancelled
    );
    assert_eq!(terminal(&store, queued, &call)?, Outcome::Cancelled);
    assert!(store.attempt(queued, &call)?.started_at.is_none());
    assert_eq!(
        store.cancel(queued, "stop", &call)?,
        CancelReceipt::AlreadyTerminal
    );

    store.mark_starting(
        &store.record(starting, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    // Before the handoff publishes its launch summary, cancellation records
    // a durable request without waiting for custody or recovery.
    let system = provenant_os::clock::SystemClock::new();
    let spent = Request {
        clock: &system,
        cancel: &cancel,
    };
    // A held custody lock is a live handoff: cancel leaves it untouched.
    let scope = provenant_supervisor::custody::Scope {
        run: batch.run_id,
        attempt: starting,
        generation: 1,
    };
    let held = provenant_supervisor::fixture::acquire_writer(
        store.attempt_dir(batch.run_id, starting, false)?,
        scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    assert_eq!(
        store.cancel(starting, "stop", &spent)?,
        CancelReceipt::Requested
    );
    let dir = store.attempt_dir(batch.run_id, starting, false)?;
    let request: serde_json::Value = serde_json::from_slice(
        &artifacts::read_sealed(&dir, "cancel.json", RECORD_LIMIT)?
            .ok_or("cancel request missing")?,
    )?;
    assert_eq!(request["scope"], serde_json::to_value(scope)?);
    assert_eq!(
        store.record(starting, &call)?.state,
        AttemptState::Starting {
            extra: Default::default()
        }
    );
    assert!(
        provenant_supervisor::custody::attempt_evidence(&dir, scope)?
            .metadata_at(std::ffi::OsStr::new("reservation.json"), false)
            .is_ok()
    );
    drop(held);
    // The waiting guardian sees the actual request before admission.
    let host = HostFacts::new(100);
    assert!(matches!(
        store.admit(starting, &dir, &host, &clock, &cancel),
        Err(RunError::Cancelled)
    ));
    let mut observed = exit();
    observed.scope.attempt = starting;
    observed.scope.generation = 1;
    observed.never_spawned = true;
    observed.termination = None;
    observed.terminal_observed = false;
    observed.settlement_evidence = provenant_supervisor::custody::SettlementEvidence::NeverSpawned;
    observed.stop_reason = Some(provenant_supervisor::machine::StopReason::ExplicitCancellation);
    seal_exit(&store, &observed)?;
    store.finish(starting, &dir, &observed, &Fake::default(), &call)?;
    assert_eq!(terminal(&store, starting, &call)?, Outcome::Cancelled);
    assert_eq!(store.run(batch.run_id, &call)?.closure, Closure::Closed);
    Ok(())
}
