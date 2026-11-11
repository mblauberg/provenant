use super::*;

#[test]
fn status_shows_recorded_capability_origins() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
    let store = trace_store(&temp, &call)?;
    let mut input = trace_run(
        1,
        AttemptId::from_bytes([9; 16]),
        WallTime::from_micros(100)?,
    )?;
    input.request.tasks[0].access.database_ipc = true;
    input.request.tasks[0].access.extra.insert(
        "capability_sources".into(),
        json!([{"name":"postgres","source":"project default"}]),
    );
    store.create(&input, &call)?;
    let row = store.record(input.attempts[0].attempt_id, &call)?;
    let mut roots = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    roots.state_dir = Some(root.join("state"));
    let context = run::ExecutionContext {
        state_root: roots.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    let session = Session { store, context };
    let (mut data, _) = render_lanes(std::slice::from_ref(&row), &[], None, &root, None);
    super::status::attach_capability_sources(
        &session,
        std::slice::from_ref(&row),
        &mut data,
        &call,
    );
    assert_eq!(
        data["lanes"][0]["capabilities"],
        json!([{"name":"postgres","source":"project default"}])
    );
    Ok(())
}
#[test]
fn a_nested_lane_reads_the_host_ledger_when_it_opens_read_only()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let host = root.join("state");
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(host.join("children").join("lane"));
    let context = run::ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: provenant_core::ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    // The store must exist to be opened; the writable open creates it.
    run::RunStore::open(&context, &call)?;
    let store = read_only_store(&context, &host, &call)
        .map_err(|f| f.message)?
        .ok_or("nested store")?;
    assert_eq!(store.writer_registry(), host);
    assert_ne!(store.writer_registry(), context.state_root.as_path());
    Ok(())
}
#[test]
fn trace_chain_follows_parents_and_ends_at_a_parent_this_store_lacks()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    let missing = AttemptId::from_bytes([9; 16]);
    let prepared =
        |n: u8, parent_attempt: AttemptId| trace_run(n, parent_attempt, clock.wall_now()?);
    let first = prepared(10, missing)?;
    let second = prepared(20, first.attempts[0].attempt_id)?;
    store.create(&first, &call)?;
    store.create(&second, &call)?;
    let leaf = store.record(second.attempts[0].attempt_id, &call)?;
    let chain = trace_chain(&store, leaf, &call)?;
    let attempts: Vec<_> = chain.iter().map(|t| t.attempt).collect();
    assert_eq!(
        attempts,
        [
            Some(second.attempts[0].attempt_id),
            Some(first.attempts[0].attempt_id),
            Some(missing)
        ]
    );
    assert_eq!(chain[1].run, Some(first.run_id));
    assert_eq!(
        chain[2],
        provenant_records::Trace {
            attempt: Some(missing),
            ..Default::default()
        }
    );
    let line = trace_line(&chain[2]);
    assert_eq!(line.lines().count(), 1);
    assert!(line.contains("host=- ") && line.contains(&format!("attempt={missing} ")));
    Ok(())
}
#[test]
fn refused_provider_continuation_keeps_a_durable_attempt() -> Result<(), Box<dyn std::error::Error>>
{
    struct Refusing {
        prior: Option<AttemptId>,
    }
    impl run::Providers for Refusing {
        fn prepare(
            &self,
            _: &run::TaskRequest,
            _: &str,
            _: &run::LaunchPaths,
            resume: Option<&run::AttemptRecord>,
        ) -> Result<run::PreparedLaunch, run::PrepareRefusal> {
            assert_eq!(resume.map(|prior| prior.attempt_id), self.prior);
            Err("fixture continuation refused; run provenant run".into())
        }
        fn decode(
            &self,
            _: &run::RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &Path,
        ) -> Result<run::Decoded, String> {
            unreachable!("a refused launch never reaches a provider")
        }
    }
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
    let store = trace_store(&temp, &call)?;
    let prior = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
    store.create(&prior, &call)?;
    store.start(
        prior.run_id,
        &Refusing { prior: None },
        &|_, _| unreachable!(),
        &call,
    )?;
    let prior_id = prior.attempts[0].attempt_id;
    let mut resumed = trace_run(20, prior_id, clock.wall_now()?)?;
    resumed.request.parent = Some(prior_id);
    resumed.request.tasks[0].task_id = prior.attempts[0].task_id;
    resumed.attempts[0].task_id = prior.attempts[0].task_id;
    store.create(&resumed, &call)?;
    let started = store.start(
        resumed.run_id,
        &Refusing {
            prior: Some(prior_id),
        },
        &|_, _| unreachable!(),
        &call,
    )?;
    let [run::Started::Refused { attempt, .. }] = started.as_slice() else {
        return Err("continuation must be refused".into());
    };
    assert_eq!(*attempt, resumed.attempts[0].attempt_id);
    // Reopen the store: every ID printed by the refusal must remain
    // addressable after the command exits.
    drop(store);
    let reopened = trace_store(&temp, &call)?;
    let record = run::Service::new(&reopened).status(*attempt, &call)?;
    assert_eq!(record.run_id, resumed.run_id);
    assert!(matches!(
        record.state,
        run::AttemptState::Terminal { outcome: run::Outcome::Rejected, reason: Some(ref reason), .. }
            if reason.contains("fixture continuation refused")
    ));
    Ok(())
}
#[test]
fn grants_lines_list_each_applied_grant_and_stay_empty_for_older_attempts()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    let prepared = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
    store.create(&prepared, &call)?;
    let mut record = store.record(prepared.attempts[0].attempt_id, &call)?;
    assert_eq!(grants_lines(&record), "");
    record.write_grants = vec![run::WriteGrantRecord {
        extra: Default::default(),
        source: run::WriteSource::WriteRoot,
        path: provenant_records::AbsolutePath::new(std::path::Path::new("/work/shared"))?,
        scope: run::PathScope::Subtree,
    }];
    record.read_only_writable = vec!["docs".into()];
    record.git_grants_digest = Some(provenant_core::Digest::from_bytes([6; 32]));
    let lines = grants_lines(&record);
    let mut lines = lines.lines();
    assert_eq!(
        lines.next(),
        Some("grant write source=write_root scope=subtree path=/work/shared")
    );
    assert_eq!(lines.next(), Some("grant read_only_writable path=docs"));
    assert!(
        lines
            .next()
            .is_some_and(|l| l.starts_with("grant git digest="))
    );
    assert_eq!(lines.next(), None);
    Ok(())
}
#[test]
fn grants_lines_report_incomplete_tool_closure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    let prepared = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
    store.create(&prepared, &call)?;
    let mut record = store.record(prepared.attempts[0].attempt_id, &call)?;
    record.applied_route = Some(run::RouteRecord {
        extra: [(
            "tool_closure".into(),
            serde_json::json!([{
                "tool": "rg", "outcome": "incomplete", "reason": "missing", "reasons": ["missing"]
            }]),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    assert!(
        grants_lines(&record).contains("warning tool_closure_incomplete tool=rg reason=missing\n")
    );
    Ok(())
}
#[test]
fn trace_chain_keeps_every_ancestor_of_a_long_chain() -> Result<(), Box<dyn std::error::Error>> {
    const LEN: u8 = 70;
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    let missing = AttemptId::from_bytes([9; 16]);
    let mut parent = missing;
    let mut ids = Vec::new();
    for link in 1..=LEN {
        // Each run uses three bytes of ID space, so step by three.
        let prepared = trace_run(3 * link, parent, clock.wall_now()?)?;
        store.create(&prepared, &call)?;
        parent = prepared.attempts[0].attempt_id;
        ids.push(parent);
    }
    let leaf = store.record(parent, &call)?;
    let chain = trace_chain(&store, leaf, &call)?;
    let attempts: Vec<_> = chain.iter().filter_map(|t| t.attempt).collect();
    ids.reverse();
    ids.push(missing);
    assert_eq!(attempts, ids);
    Ok(())
}
#[test]
fn trace_chain_fails_when_parents_loop() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    // Attempt IDs are n + 2, so 12 and 22 name each other.
    let first = trace_run(10, AttemptId::from_bytes([22; 16]), clock.wall_now()?)?;
    let second = trace_run(20, AttemptId::from_bytes([12; 16]), clock.wall_now()?)?;
    store.create(&first, &call)?;
    store.create(&second, &call)?;
    let leaf = store.record(second.attempts[0].attempt_id, &call)?;
    let Err(error) = trace_chain(&store, leaf, &call) else {
        return Err("a looping chain must fail".into());
    };
    assert!(matches!(error, TraceError::Loop(attempt) if attempt == second.attempts[0].attempt_id));
    Ok(())
}
#[test]
fn check_continuable_refuses_nonterminal_before_session() -> Result<(), Box<dyn std::error::Error>>
{
    for state in ["running", "queued"] {
        let prior: run::AttemptRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": RunId::from_bytes([2; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "resume-nonterminal",
            "attempt_number": 1,
            "revision": 1,
            "generation": 0,
            "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "queued_at": WallTime::from_micros(100)?,
            "state": {"state":state},
            "usage": {},
            "prompt": {
                "path": {"encoding":"utf8","path":"prompt.txt"},
                "digest": CanonicalV1::file_digest(b""),
                "bytes": 0,
                "complete": true,
                "redacted": false
            }
        }))?;
        let result = Registry::default().check_continuable(&prior);
        assert!(
            matches!(result, Err(fail) if fail.kind == ErrorKind::Nonterminal && fail.status == Status::Refused),
            "{state} attempt must be refused as nonterminal"
        );
    }
    Ok(())
}
#[test]
fn cancel_and_reconcile_report_leftover_paths_and_reasons_without_terminal_controls() {
    let state = run::Reconciled::TemporaryRetained {
        path: PathBuf::from("/state/runs/run/attempts/attempt/tmp"),
        reason: "permission denied\n\x1b[31mretry later\x1b[0m".into(),
    };
    let rendered = reconcile_summary(&state);
    assert!(rendered.contains("/state/runs/run/attempts/attempt/tmp"));
    assert!(rendered.contains("permission denied"));
    assert!(rendered.contains("retry later"));
    assert!(!rendered.chars().any(char::is_control));

    let state = run::Reconciled::Uncertain {
        reason: run::UncertainReason::ReconcileFailed {
            error: "writer fence listing failed".into(),
        },
    };
    assert!(reconcile_summary(&state).contains("writer fence listing failed"));

    let data =
        json!({"writer_reservation_errors": [{"reason": "legacy reservation has invalid anchor"}]});
    let blocked = reconcile_reply(
        Ok((
            data.clone(),
            "writer reservation: legacy reservation has invalid anchor\n".into(),
        )),
        true,
    );
    assert_eq!(blocked.result.status, Status::Failed);
    assert_eq!(blocked.result.data, data);
    let error = blocked
        .result
        .error
        .expect("failed reconcile must explain its blocker");
    assert_eq!(error.code, "RECONCILE_INCOMPLETE");
    assert!(error.message.contains("invalid anchor"));
}
#[test]
fn local_lanes_read_and_show_capacity_cooldowns() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let route = run::RouteRecord {
        extra: Default::default(),
        provider: Some("codex".into()),
        model: Some("codex/model".into()),
        effort: None,
        account_label: Some("work".into()),
        account_key: Some("key".into()),
        source: None,
        pool_selection: None,
    };
    let now = SystemClock::new().wall_now()?.as_timestamp().as_second();
    crate::quota::persist_capacity_cooldown(&state, &route, now, 600)?;
    let cooldowns = read_lane_cooldowns(&state);
    let (data, human) = render_lanes(&[], &[], None, &state, lane_cooldown_view(&cooldowns));
    assert_eq!(data["capacity_cooldowns"][0]["model"], "model");
    assert_eq!(data["capacity_cooldowns"][0]["account"]["label"], "work");
    assert!(
        data["capacity_cooldowns"][0]["remaining_seconds"]
            .as_i64()
            .is_some_and(|seconds| seconds > 0 && seconds <= 600)
    );
    assert!(human.contains("capacity cooldown codex/model [work]:"));
    Ok(())
}
