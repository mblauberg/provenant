use super::*;

#[test]
fn rejected_resume_remains_visible_and_addressable() -> TestResult {
    struct RouteMismatch;
    impl Providers for RouteMismatch {
        fn prepare(
            &self,
            _: &TaskRequest,
            _: &str,
            _: &LaunchPaths,
            prior: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, crate::PrepareRefusal> {
            assert!(prior.is_some());
            Err(crate::fixture_refusal(
                "route differs from the prior attempt's provider, model or account",
            ))
        }
        fn decode(
            &self,
            _: &RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &std::path::Path,
        ) -> std::result::Result<Decoded, String> {
            unreachable!()
        }
    }
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let initial = input();
    store.create(&initial, &call)?;
    let refusal = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start(initial.run_id, &refusal, &|_, _| unreachable!(), &call)?;
    let resumed = Resume {
        extend: Vec::new(),
        prior: initial.attempts[0].attempt_id,
        expected_generation: 0,
        operation_id: "rejected-resume".parse()?,
        run_id: RunId::from_bytes([20; 16]),
        attempt_id: AttemptId::from_bytes([21; 16]),
        prompt: "answer".into(),
        created_at: WallTime::from_micros(100)?,
    };
    store.resume(&resumed, &call)?;
    let started = store.start(
        resumed.run_id,
        &RouteMismatch,
        &|_, _| unreachable!(),
        &call,
    )?;
    let [
        Started::Refused {
            attempt,
            kind: RefusalKind::Preparation,
            reason,
        },
    ] = started.as_slice()
    else {
        return Err("resume must be refused during preparation".into());
    };
    assert_eq!(*attempt, resumed.attempt_id);
    let reader =
        RunStore::open_read_only(&context(temp.path())?, &call)?.ok_or("run store missing")?;
    let lanes = reader.lanes(
        &LaneQuery {
            targets: vec![],
            all: true,
            limit: 10,
            after: 0,
            newest_first: false,
            owner: None,
            unseen_after: None,
        },
        &call,
    )?;
    let rejected = lanes
        .lanes
        .iter()
        .find(|lane| lane.attempt_id == resumed.attempt_id)
        .ok_or("rejected resume missing from lanes")?;
    assert!(matches!(
        rejected.state,
        AttemptState::Terminal {
            outcome: Outcome::Rejected,
            ..
        }
    ));
    assert!(
        matches!(&rejected.state, AttemptState::Terminal { reason: Some(recorded), .. } if recorded == reason)
    );
    let output = Service::new(&reader).output(
        resumed.attempt_id,
        ArtifactPart::Result,
        ByteRange::default(),
        &call,
    )?;
    assert!(output.bytes.is_empty());
    Ok(())
}
#[test]
fn resumed_starting_lane_has_private_directories_before_checkpoint() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let original = input();
    store.create(&original, &call)?;
    let prior = original.attempts[0].attempt_id;
    let fake = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start(original.run_id, &fake, &|_, _| unreachable!(), &call)?;
    let resumed = Resume {
        extend: Vec::new(),
        prior,
        expected_generation: 0,
        operation_id: provenant_core::OperationId::parse("resume-private")?,
        run_id: RunId::from_bytes([50; 16]),
        attempt_id: AttemptId::from_bytes([51; 16]),
        prompt: "answer".into(),
        created_at: WallTime::from_micros(100)?,
    };
    store.resume(&resumed, &call)?;
    let record = store.record(resumed.attempt_id, &call)?;
    store.mark_starting(&record, &route(), &[], &Default::default(), None, &call)?;
    let directory = store.attempt_dir(resumed.run_id, resumed.attempt_id, false)?;
    assert!(
        matches!(directory.metadata_at("process.json".as_ref(), false), Err(error) if error.is_not_found())
    );
    let reader = RunStore::open_read_only(&context, &call)?.ok_or(RunError::NotFound)?;
    assert_eq!(
        reader.record(resumed.attempt_id, &call)?.state,
        AttemptState::Starting {
            extra: Default::default()
        }
    );
    let state = context.state_root.as_path();
    for path in [
        state.join("runs"),
        state.join("runs").join(resumed.run_id.to_string()),
        state
            .join("runs")
            .join(resumed.run_id.to_string())
            .join("attempts"),
        state
            .join("runs")
            .join(resumed.run_id.to_string())
            .join("attempts")
            .join(resumed.attempt_id.to_string()),
    ] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::symlink_metadata(path)?.permissions().mode() & 0o077,
            0
        );
    }
    Ok(())
}
#[test]
fn resume_continues_a_terminal_attempt_once_with_its_route() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut run = input();
    run.request.chair = Some(ChairBinding {
        extra: Default::default(),
        session: None,
        host: provenant_core::HostId::from_bytes([30; 16]),
        actor: provenant_core::ActorId::from_bytes([31; 16]),
    });
    let outer_parent = AttemptId::from_bytes([19; 16]);
    run.request.lane_depth = Some(1);
    run.request.parent_attempt = Some(outer_parent);
    run.request.tasks[0].account = Some("fixture-account".into());
    store.create(&run, &call)?;
    let prior = run.attempts[0].attempt_id;
    let resume = |operation: &str, expected_generation, byte| -> Result<Resume> {
        Ok(Resume {
            extend: Vec::new(),
            prior,
            expected_generation,
            operation_id: provenant_core::OperationId::parse(operation)
                .map_err(|_| RunError::Validation("operation"))?,
            run_id: provenant_core::RunId::from_bytes([byte; 16]),
            attempt_id: AttemptId::from_bytes([byte + 1; 16]),
            prompt: "answer".into(),
            created_at: WallTime::from_micros(100).map_err(|_| RunError::Validation("time"))?,
        })
    };
    assert!(matches!(
        store.resume(&resume("r1", 0, 20)?, &call),
        Err(RunError::Nonterminal)
    ));
    let fake = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
    assert_eq!(fake.resumed.get(), None);

    let continued = store.resume(&resume("r1", 0, 20)?, &call)?;
    assert_eq!(continued.parent, Some(prior));
    assert_eq!(
        store.request(continued.run_id, &call)?.chair,
        run.request.chair
    );
    assert_eq!(
        store.request(continued.run_id, &call)?.tasks[0]
            .account
            .as_deref(),
        Some("fixture-account")
    );
    assert_eq!(store.resume(&resume("r1", 0, 20)?, &call)?, continued);
    let mut changed = resume("r1", 0, 20)?;
    changed.prompt = "other".into();
    assert!(matches!(
        store.resume(&changed, &call),
        Err(RunError::OperationConflict)
    ));
    assert!(matches!(
        store.resume(&resume("r2", 0, 30)?, &call),
        Err(RunError::GenerationConflict {
            current_generation: 1
        })
    ));
    assert!(matches!(
        store.resume(&resume("r2", 1, 30)?, &call),
        Err(RunError::Conflict)
    ));
    let next = store.pending_attempts(continued.run_id, &call)?[0].attempt_id;
    let record = store.record(next, &call)?;
    assert_eq!(record.retry_of, Some(prior));
    assert_eq!(record.attempt_number, 2);
    assert_eq!(
        record.requested_route,
        store.record(prior, &call)?.requested_route
    );
    store.start(continued.run_id, &fake, &|_, _| unreachable!(), &call)?;
    assert_eq!(fake.resumed.get(), Some(prior));
    Ok(())
}
#[test]
fn nested_continuation_uses_host_derived_ids() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut child = input();
    child.request.parent_attempt = Some(AttemptId::from_bytes([9; 16]));
    child.request.lane_depth = Some(2);
    store.create(&child, &call)?;
    store.refuse_queued(child.run_id, "completed", &call)?;
    let mut resume = Resume {
        extend: Vec::new(),
        prior: child.attempts[0].attempt_id,
        expected_generation: 0,
        operation_id: provenant_core::OperationId::parse("nested-continuation")?,
        run_id: RunId::from_bytes([30; 16]),
        attempt_id: AttemptId::from_bytes([31; 16]),
        prompt: "answer".into(),
        created_at: clock.wall_now()?,
    };
    let continued = store.resume_fresh(&resume, "fixture", &call)?;
    let request = store.request(continued.run_id, &call)?;
    assert_eq!(request.child_attempt_id_version, Some(1));
    assert_eq!(
        Some(continued.run_id),
        correlation::child_run_id(store.context.project_id, &request, &resume.operation_id)?
    );
    assert_ne!(continued.run_id, resume.run_id);
    let attempt = store.pending_attempts(continued.run_id, &call)?.remove(0);
    assert_eq!(
        Some(attempt.attempt_id),
        correlation::child_attempt_id(
            store.context.project_id,
            continued.run_id,
            &request,
            attempt.task_id
        )?
    );
    resume.run_id = RunId::from_bytes([40; 16]);
    resume.attempt_id = AttemptId::from_bytes([41; 16]);
    assert_eq!(
        store.resume_fresh(&resume, "fixture", &call)?.run_id,
        continued.run_id
    );
    Ok(())
}
#[test]
fn nested_continuation_preflight_does_not_reuse_crashed_staging() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut child = input();
    child.request.parent_attempt = Some(AttemptId::from_bytes([9; 16]));
    child.request.lane_depth = Some(2);
    store.create(&child, &call)?;
    store.refuse_queued(child.run_id, "completed", &call)?;
    let resume = Resume {
        extend: Vec::new(),
        prior: child.attempts[0].attempt_id,
        expected_generation: 0,
        operation_id: "crashed-continuation".parse()?,
        run_id: RunId::from_bytes([30; 16]),
        attempt_id: AttemptId::from_bytes([31; 16]),
        prompt: "answer".into(),
        created_at: clock.wall_now()?,
    };
    let mut canonical = child.request.clone();
    canonical.child_attempt_id_version = Some(1);
    canonical.parent = Some(resume.prior);
    canonical.fresh_continuation = Some("fixture".into());
    canonical.tasks[0].prompt_digest = CanonicalV1::file_digest(resume.prompt.as_bytes());
    let expected =
        correlation::child_run_id(store.context.project_id, &canonical, &resume.operation_id)?
            .unwrap();
    let stale = store
        .context
        .state_root
        .as_path()
        .join(format!("runs/{expected}"));
    std::fs::DirBuilder::new().mode(0o700).create(&stale)?;
    std::fs::write(stale.join("retained"), "stale staging")?;
    let (continued, _) = store.resume_fresh_prepared(&resume, "fixture", None, &Ready, &call)?;
    assert_eq!(continued.run_id, expected);
    assert_eq!(
        std::fs::read_to_string(stale.join("retained"))?,
        "stale staging"
    );
    Ok(())
}
#[test]
fn resume_replay_finishes_a_prepared_continuation() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let prior = run.attempts[0].attempt_id;
    let fake = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
    let resume = |operation: &str, expected_generation, byte| -> Result<Resume> {
        Ok(Resume {
            extend: Vec::new(),
            prior,
            expected_generation,
            operation_id: provenant_core::OperationId::parse(operation)
                .map_err(|_| RunError::Validation("operation"))?,
            run_id: provenant_core::RunId::from_bytes([byte; 16]),
            attempt_id: AttemptId::from_bytes([byte + 1; 16]),
            prompt: "answer".into(),
            created_at: WallTime::from_micros(100).map_err(|_| RunError::Validation("time"))?,
        })
    };
    let original = resume("r1", 0, 20)?;
    let continued = store.resume(&original, &call)?;
    let mut operation = store
        .operation(&original.operation_id, &call)?
        .ok_or(RunError::NotFound)?;
    operation.state = OperationState::Prepared;
    store.db.execute(
        "UPDATE attempts SET operation_record=?1 WHERE project=?2 AND actor=?3 AND operation=?4",
        rusqlite::params![
            crate::db::bytes(&operation)?,
            store.context.project_id.to_string(),
            store.context.actor_id.to_string(),
            original.operation_id.as_str(),
        ],
    )?;

    let replayed = store.resume(&resume("r1", 0, 40)?, &call)?;
    assert_eq!(replayed.run_id, provenant_core::RunId::from_bytes([20; 16]));
    assert_eq!(replayed, continued);
    assert_eq!(
        store
            .operation(&original.operation_id, &call)?
            .ok_or(RunError::NotFound)?
            .state,
        OperationState::Accepted
    );
    let mut changed = resume("r1", 0, 40)?;
    changed.prompt = "other".into();
    assert!(matches!(
        store.resume(&changed, &call),
        Err(RunError::OperationConflict)
    ));
    assert!(matches!(
        store.resume(&resume("r2", 0, 30)?, &call),
        Err(RunError::GenerationConflict {
            current_generation: 1
        })
    ));
    assert!(matches!(
        store.resume(&resume("r2", 1, 30)?, &call),
        Err(RunError::Conflict)
    ));
    Ok(())
}
