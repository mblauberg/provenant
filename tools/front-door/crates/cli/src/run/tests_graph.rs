use super::*;

#[test]
fn cancel_wait_preserves_silence_as_non_success_without_timeout() {
    let wait = json!({
        "lanes": [{"state":"running"}],
        "condition_met": false,
        "timed_out": false,
        "reason": "provider silent; lane attempt-1 is still running"
    });
    let mut data = json!({});
    let mut status = Status::Ok;
    apply_cancel_wait(&mut data, &wait, false, &mut status);
    assert_eq!(status, Status::Failed);
    assert_eq!(data["timed_out"], false);
    assert_eq!(data["condition_met"], false);
    assert_eq!(data["reason"], wait["reason"]);
    assert_eq!(data["lanes"], wait["lanes"]);
}
#[test]
fn graph_quality_reports_missing_checkpoint_without_failing()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, OperationId, ProjectId, TaskId, WallTime};
    use provenant_records::{AttemptFact, AttemptOutcome, Usage};
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("project"));
    inputs.state_dir = Some(base.clone());
    let root = provenant_core::StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let project = ProjectId::from_bytes([3; 16]);
    let fact = AttemptFact {
        context: Some(provenant_records::run::WorkContext {
            graph_id: Some("retired".into()),
            ..Default::default()
        }),
        attempt_id: AttemptId::from_bytes([1; 16]),
        task_id: TaskId::from_bytes([2; 16]),
        attempt_number: 1,
        operation_id: OperationId::parse("quality-test")?,
        project_id: project,
        terminal_state: AttemptOutcome::Succeeded,
        exit_code: Some(0),
        queued_at: WallTime::from_micros(0)?,
        started_at: None,
        provider_ms: None,
        finished_at: WallTime::from_micros(1)?,
        resolved_route: None,
        usage: Usage::default(),
        reviewer: None,
        delivery_id: None,
        subject: None,
        head: None,
        output_refs: vec![],
        requested_route: None,
        applied_route: None,
        observed_route: None,
        verified_owner: None,
        lineage: vec![],
        tier: None,
        template: None,
        parent_node: None,
        slot_wait_ms: None,
        reason: None,
        trace: Default::default(),
        transcript: None,
    };
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let call = run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let rows =
        graph_quality_rows(&[&fact], None, &call, &root, project).map_err(|_| "quality report")?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["graph_id"], "retired");
    assert_eq!(rows[0]["checkpoint"], "missing");
    Ok(())
}
#[test]
fn graph_quality_counts_only_executed_repair_rounds_and_leaf_findings() {
    use provenant_fabric_runtime::graph::NodeProgress;
    let node = |value| serde_json::from_value::<NodeProgress>(value).unwrap();
    let findings = json!({
        "schema_version": 1,
        "findings": [{"id":"F1", "severity":"P2", "file":"a.rs", "line":1, "summary":"finding", "status":"open"}],
        "warnings": []
    });
    let checkpoint_nodes = vec![
        node(
            json!({"id":"composite", "state":"succeeded", "children":["child"], "attempts":[], "outputs":findings}),
        ),
        node(json!({"id":"child", "state":"succeeded", "attempts":[], "outputs":findings})),
        node(
            json!({"id":"initial", "state":"succeeded", "attempts":[], "round":{"loop_id":"loop","round":1,"node_id":"initial","until":false}, "gate":{"passed":true}}),
        ),
        node(
            json!({"id":"repair", "state":"succeeded", "attempts":[{"launched":true,"key":{"graph_id":"g","node_id":"repair","route_index":0},"route":{"alias":"utility"},"state":"terminal","launch_actor":"actor"}], "round":{"loop_id":"loop","round":2,"node_id":"repair","until":false}}),
        ),
        node(
            json!({"id":"configured_only", "state":"pending", "attempts":[], "round":{"loop_id":"loop","round":3,"node_id":"configured_only","until":false}}),
        ),
    ];
    assert_eq!(graph_quality_counts(&checkpoint_nodes), (1, 1, 1));
}
#[test]
fn host_native_attempts_do_not_resume_explicitly() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|error| error.message)?;
    let store = trace_store(&temp, &call)?;
    let input = trace_run(44, AttemptId::from_bytes([45; 16]), clock.wall_now()?)?;
    store.create(&input, &call)?;
    let mut record = store.record(input.attempts[0].attempt_id, &call)?;
    record.applied_route = Some(run::RouteRecord {
        extra: [("host_native".into(), json!("ci-fixer"))].into(),
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5-5".into()),
        ..Default::default()
    });
    record.state = run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::Succeeded,
        exit_code: Some(0),
        finished_at: clock.wall_now()?,
        reason: None,
    };
    record.session = Some("native-session".into());
    let registry = Registry::default();
    let error = registry
        .check_continuable(&record)
        .err()
        .ok_or("host resume accepted")?;
    assert_eq!(error.kind, ErrorKind::ContinuationUnsupported);
    Ok(())
}
#[test]
fn unsafe_store_path_reason_survives_cli_conversion() {
    let failure = super::Fail::from(provenant_run::RunError::UnsafePath {
        path: "runs/example/process.json".into(),
        check: provenant_run::PathCheck::Links,
    });
    assert!(failure.message.contains("runs/example/process.json"));
    assert!(failure.message.contains("link count"));
}
#[test]
fn host_delivery_limit_keeps_its_text_as_typed_io_detail() {
    for (tool, command, text) in [
        (
            "git",
            "worktree list",
            "host writer delivery failed at Integrate: git worktree list output exceeded the 8388608-byte host limit",
        ),
        (
            "gh",
            "pr",
            "host writer delivery failed at Integrate: gh pr output exceeded the 8388608-byte host limit",
        ),
    ] {
        let failure = super::Fail::from(provenant_run::RunError::DeliveryLimit {
            stage: provenant_records::run::WriterDeliveryStage::Integrate,
            tool,
            command: command.into(),
            limit: provenant_os::exec::DEFAULT_CAPTURE_LIMIT,
        });
        assert_eq!(failure.message, text);
        assert_eq!(failure.kind, ErrorKind::Io);
        let detail = failure.projection.expect("typed detail");
        assert!(matches!(
            detail.fact,
            Some(provenant_records::diagnostic::DiagnosticFact::DeliveryLimit(_))
        ));
        assert_eq!(detail.code, "IO");
        assert_eq!(detail.detail.as_deref(), Some(text));
    }
    let failure = super::Fail::from(provenant_run::RunError::Delivery(
        provenant_records::run::WriterDeliveryStage::Prepare,
    ));
    let detail = failure.projection.expect("stage detail");
    assert!(matches!(
        detail.fact,
        Some(provenant_records::diagnostic::DiagnosticFact::DeliveryStage(_))
    ));
    assert_eq!(detail.detail.as_deref(), Some(failure.message.as_str()));
}
#[test]
fn cli_io_projection_omits_untrusted_sources_in_both_formats()
-> Result<(), Box<dyn std::error::Error>> {
    for source in [
        "TOKEN_SENTINEL\u{1b}".to_owned(),
        format!("TOKEN_SENTINEL{}", "é".repeat(2048)),
    ] {
        let failure = super::Fail::from(run::RunError::Io(std::io::Error::other(source)));
        let reply = super::reply("run", Err(failure));
        for mode in [
            crate::render::RenderMode::Human {
                text: &reply.human,
                summary: None,
                read_only: false,
            },
            crate::render::RenderMode::Json,
        ] {
            let mut output = Vec::new();
            crate::render::render(&reply.result, mode, &mut output)?;
            let text = String::from_utf8(output)?;
            assert!(text.contains("IO"));
            assert!(!text.contains("TOKEN_SENTINEL"));
            assert!(!text.chars().any(|c| c.is_control() && c != '\n'));
            assert!(text.len() < 4096);
        }
        assert!(reply.result.error.as_ref().unwrap().repair.is_some());
    }
    Ok(())
}
#[test]
fn host_delivery_limit_cli_error_matches_golden() -> Result<(), Box<dyn std::error::Error>> {
    let failure = super::Fail::from(provenant_run::RunError::DeliveryLimit {
        stage: provenant_records::run::WriterDeliveryStage::Integrate,
        tool: "git",
        command: "worktree list".into(),
        limit: provenant_os::exec::DEFAULT_CAPTURE_LIMIT,
    });
    assert_eq!(failure.kind, ErrorKind::Io);
    let reply = super::reply("run", Err(failure));
    let mut rendered = Vec::new();
    crate::render::render(
        &reply.result,
        crate::render::RenderMode::Human {
            text: &reply.human,
            summary: reply.summary.as_ref(),
            read_only: reply.read_only,
        },
        &mut rendered,
    )?;
    let actual = String::from_utf8(rendered)?;
    for part in [
        "IO",
        "Integrate",
        "git",
        &provenant_os::exec::DEFAULT_CAPTURE_LIMIT.to_string(),
    ] {
        assert!(actual.contains(part), "{actual}");
    }
    Ok(())
}
#[test]
fn stats_summary_reports_cost_coverage() {
    use provenant_telemetry::{CostTotal, Totals};
    let mut totals = Totals {
        attempts: 3,
        usage: Default::default(),
        cache: Default::default(),
        cost: CostTotal {
            cost_basis: "api_equivalent_estimate",
            subscription_attempts: 0,
            complete: None,
            known_lower_bound: None,
            priced_attempts: 0,
            missing_attempts: 3,
        },
        outcomes: Default::default(),
    };
    assert_eq!(totals_cache_read_share(&totals), None);
    totals.usage.uncached_input.known = 10;
    totals.usage.cache_read.known = 90;
    assert_eq!(totals_cache_read_share(&totals), Some(0.9));
    totals.usage.cache_write.missing = 1;
    assert_eq!(totals_cache_read_share(&totals), None);
}
#[test]
fn stats_text_lists_cache_share_only_where_a_provider_reported_it() {
    use provenant_telemetry::{ChairModelUsage, ConfiguredPrices, usage::ChairSession};
    let prices = provenant_telemetry::validate_prices(&ConfiguredPrices {
        schema_version: 1,
        id: "empty".into(),
        reviewed_at: "2026-10-29".into(), // timebomb: allow fixed price-table review date, never compared with the clock
        units: "usd_microdollars_per_million_tokens".into(),
        max_review_age_days: None,
        discrepancy_threshold_bps: None,
        models: Vec::new(),
    })
    .unwrap();
    let chair = |usage| ChairSession {
        adapter: "claude".into(),
        account: "work".into(),
        session: "s".into(),
        project: None,
        config: None,
        usage,
        unread: None,
    };
    let usage = |read| {
        vec![ChairModelUsage {
            model: "claude-opus-5-5".into(),
            usage: provenant_records::Usage {
                uncached_input: Some(10),
                cache_read: read,
                cache_write: Some(0),
                cache_write_1h: None,
                output_excluding_reasoning: Some(1),
                reasoning: None,
            },
        }]
    };
    let report = |usage| {
        let report =
            provenant_telemetry::stats_with_chair([], [chair(usage)], &prices, Default::default())
                .unwrap();
        stats_summary(&report)
    };
    let text = report(usage(Some(90)));
    assert!(
            text.contains(
                "cache read by source [chair]: 90.0% of 100 input tokens (1 records reported, 0 did not)\n"
            ),
            "{text}"
        );
    assert!(
        text.contains("cache read by model [chair] claude claude/claude-opus-5-5: 90.0%"),
        "{text}"
    );
    assert!(
        text.contains("chair: 1 sessions on this host (0 unread)"),
        "{text}"
    );
    let text = report(usage(None));
    assert!(!text.contains("cache read"), "{text}");
}
#[test]
fn stats_text_names_its_scope_and_reports_reasoning_only_when_present() {
    let project = provenant_core::ProjectId::from_bytes([1; 16]);
    let scope = stats_scope_line(project, 2);
    assert!(
        scope.starts_with(&format!(
            "scope: project {project}, finished attempts only (2 queued or running not counted)"
        )),
        "{scope}"
    );
    assert!(scope.contains("other projects are excluded"), "{scope}");
    let mut totals = provenant_telemetry::Totals {
        attempts: 1,
        usage: Default::default(),
        cache: Default::default(),
        cost: provenant_telemetry::CostTotal {
            cost_basis: "api_equivalent_estimate",
            subscription_attempts: 0,
            complete: None,
            known_lower_bound: None,
            priced_attempts: 0,
            missing_attempts: 1,
        },
        outcomes: Default::default(),
    };
    assert_eq!(reasoning_line(&totals), None);
    totals.usage.reasoning.known = 42;
    assert_eq!(
        reasoning_line(&totals).as_deref(),
        Some("reasoning tokens: 42\n")
    );
    totals.usage.reasoning.missing = 3;
    assert_eq!(
        reasoning_line(&totals).as_deref(),
        Some("reasoning tokens: >=42 (3 unknown)\n")
    );
}
#[test]
fn displaced_fallback_reports_unavailable_history() {
    let history = super::fallback_history(Path::new(env!("CARGO_MANIFEST_DIR")), "HEAD", true);
    assert_eq!(
        history,
        "[history unavailable: prior worktree is missing or reassigned]"
    );
}
#[test]
fn submit_names_the_fix_for_a_bare_word_model_and_unsupported_effort()
-> Result<(), Box<dyn std::error::Error>> {
    let Err(bare) = super::requested(Some("junior"), None, None) else {
        panic!("bare-word model accepted");
    };
    assert_eq!(bare.status, Status::Invalid);
    assert!(
        bare.message.ends_with("did you mean --route junior?"),
        "{}",
        bare.message
    );
    Ok(())
}
#[test]
fn contention_reply_names_the_operation_to_retry() -> Result<(), Box<dyn std::error::Error>> {
    let id = OperationId::parse("retry-lane")?;
    let mut reply = super::reply(
        "run",
        Err(Fail::new(
            Status::Refused,
            ErrorKind::Contention,
            "the lane is being admitted",
        )),
    );
    super::operation_reply(&mut reply, &id);
    let repair = reply
        .result
        .error
        .as_ref()
        .and_then(|error| error.repair.as_deref());
    assert_eq!(
        repair,
        Some("retry the same command with --operation-id retry-lane")
    );
    Ok(())
}
#[test]
fn pruning_intent_replays_only_for_its_missing_worktree() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let tree = temp.path().join("owned");
    let receipt = serde_json::json!({"state": "pruning", "worktree": tree});
    assert!(super::worktree_was_pruned(&receipt, &tree));
    assert!(!super::worktree_was_pruned(
        &receipt,
        &temp.path().join("other")
    ));
    std::fs::create_dir(&tree)?;
    assert!(!super::worktree_was_pruned(&receipt, &tree));
    std::fs::remove_dir(&tree)?;
    std::os::unix::fs::symlink(temp.path().join("missing-target"), &tree)?;
    assert!(!super::worktree_was_pruned(&receipt, &tree));
    Ok(())
}
#[test]
fn retry_reuses_the_retained_brief_and_accepts_a_replacement()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    let prepared = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
    let prior = prepared.attempts[0].attempt_id;
    store.create(&prepared, &call)?;
    let mut roots = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    roots.state_dir = Some(root.join("state"));
    let context = run::ExecutionContext {
        state_root: roots.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    let session = Session { store, context };
    let mut command = crate::Resume {
        attempt: crate::Attempt(Target {
            selection: None,
            run: None,
            task: None,
            attempt: Some(prior),
        }),
        operation_id: OperationId::parse("retry-brief")?,
        node: None,
        brief: None,
        extend: vec![],
        no_fallback: false,
        prompt: crate::Prompt {
            prompt_file: None,
            prompt_stdin: false,
        },
        wait: None,
    };
    assert_eq!(
        resume::retry_prompt(&command, &session, prior, &call).map_err(|f| f.message)?,
        "p"
    );
    command.brief = Some("repair the parser".into());
    assert_eq!(
        resume::retry_prompt(&command, &session, prior, &call).map_err(|f| f.message)?,
        "repair the parser"
    );
    Ok(())
}
#[test]
fn refused_operations_are_rendered_without_an_attempt() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = trace_store(&temp, &call)?;
    let input = trace_run(
        30,
        AttemptId::from_bytes([2; 16]),
        now(&clock).map_err(|e| e.message)?,
    )?;
    store.record_refusal(
        &input,
        ErrorKind::Unsupported.diagnostic(),
        Some("writer".into()),
        &call,
    )?;
    let mut data = json!({"lanes": []});
    let mut human = String::new();
    assert!(
        status::append_refused_operations(&store, 20, &mut data, &mut human, &call)
            .map_err(|e| e.message)?
    );
    assert_eq!(
        data["refused_operations"][0]["operation_id"],
        input.operation_id.as_str()
    );
    assert_eq!(
        data["refused_operations"][0]["refusal"]["code"],
        "UNSUPPORTED"
    );
    assert!(human.contains(input.operation_id.as_str()));
    assert!(data["lanes"].as_array().is_some_and(Vec::is_empty));
    Ok(())
}
#[test]
fn cloud_job_survives_reopening_owned_attempt() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_providers::cloud::{
        CloudProvider, CloudResult, CloudSubmission, RemoteJob, RemoteState,
    };
    struct Fake {
        running: bool,
    }
    impl CloudProvider for Fake {
        type Error = ();
        fn check_environment(&mut self, _: CloudSubmission<'_>) -> Result<(), ()> {
            Ok(())
        }
        fn submit(&mut self, request: CloudSubmission<'_>) -> Result<String, ()> {
            assert_eq!(request.job.commit, "a".repeat(40));
            Ok("remote-id".into())
        }
        fn observe(&mut self, job: &RemoteJob) -> Result<RemoteState, ()> {
            assert_eq!(job.remote_id, "remote-id");
            assert_eq!(job.repository, "repository");
            assert_eq!(job.commit, "a".repeat(40));
            if self.running {
                return Ok(RemoteState::Running);
            }
            Ok(RemoteState::Finished {
                result: CloudResult {
                    job_id: job.job_id.clone(),
                    nonce: job.nonce.clone(),
                    commit: "a".repeat(40),
                    exit_status: 0,
                    tests_run: vec!["targeted".into()],
                    tests_failed: vec![],
                    log_location: "log".into(),
                    observed_model: Some("small".into()),
                    branch: None,
                    head: None,
                },
            })
        }
        fn cancel(&mut self, _: &RemoteJob) -> Result<bool, ()> {
            Ok(false)
        }
    }
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|e| e.message)?;
    let store = trace_store(&temp, &call)?;
    let mut input = trace_run(
        40,
        AttemptId::from_bytes([1; 16]),
        now(&clock).map_err(|e| e.message)?,
    )?;
    input.request.parent_attempt = None;
    input.request.tasks[0].head = Some("a".repeat(40));
    input.request.tasks[0].cloud = Some(provenant_records::run::CloudPlacement {
        target: "target".into(),
        provider: "fake".into(),
        environment: "env".into(),
        repository: "repository".into(),
        branch: "main".into(),
        model: "small".into(),
        effort: None,
        deadline_seconds: 3600,
        work_branch: None,
    });
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let target = crate::domains::CloudTarget {
        provider: "fake".into(),
        environment: "env".into(),
        model: "small".into(),
        effort: None,
        repository: "repository".into(),
        branch: "main".into(),
        deadline_seconds: 3600,
    };
    let job = cloud_owner::prepare_job(id, "target", &target, &"a".repeat(40), &clock)?;
    assert_eq!(job.job_id, id.to_string());
    assert_eq!(job.nonce.len(), 32);
    assert_ne!(
        job.nonce,
        cloud_owner::prepare_job(id, "target", &target, &"a".repeat(40), &clock)?.nonce
    );
    let mut wrong_commit = job.clone();
    wrong_commit.commit = "b".repeat(40);
    assert!(
        cloud_owner::submit(
            &mut Fake { running: true },
            &store,
            id,
            wrong_commit,
            "run tests",
            &call
        )
        .is_err()
    );
    let submitted = cloud_owner::submit(
        &mut Fake { running: true },
        &store,
        id,
        job,
        "run tests",
        &call,
    )?;
    drop(store);
    let store = trace_store(&temp, &call)?;
    let cancelled = cloud_owner::cancel(&mut Fake { running: true }, &store, id, &call)?;
    assert_eq!(cancelled.remote_id, submitted.remote_id);
    assert_eq!(cancelled.state, RemoteState::CancellationUnconfirmed);
    drop(store);
    let store = trace_store(&temp, &call)?;
    let running = cloud_owner::reconcile(&mut Fake { running: true }, &store, id, &call)?;
    assert_eq!(running.state, RemoteState::CancellationUnconfirmed);
    // A crash can happen after the remote result is saved but before its run
    // event is recorded; reconciliation repairs that publication.
    cloud_store::AttemptCloudStore::with_attempt(
        &store,
        id,
        false,
        &call,
        |cloud_store| -> Result<(), String> {
            let mut job = cloud_store
                .load()
                .map_err(|error| error.to_string())?
                .ok_or("missing cloud job")?;
            provenant_providers::cloud::reconcile(
                &mut Fake { running: false },
                cloud_store,
                &mut job,
            )
            .map_err(|error| format!("{error:?}"))?;
            Ok(())
        },
    )??;
    assert!(!cloud_owner::finished_run(&store, input.run_id, &call)?);
    let finished = cloud_owner::reconcile(&mut Fake { running: false }, &store, id, &call)?;
    assert!(matches!(finished.state, RemoteState::Finished { .. }));
    assert!(cloud_owner::finished_run(&store, input.run_id, &call)?);
    let event = store.last_event_sequence(id, &call)?;
    let _ = cloud_owner::reconcile(&mut Fake { running: false }, &store, id, &call)?;
    assert_eq!(store.last_event_sequence(id, &call)?, event);
    let mut lanes = vec![store.record(id, &call)?];
    let root = std::fs::canonicalize(temp.path())?;
    let mut roots = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    roots.state_dir = Some(root.join("state"));
    let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
    cloud_owner::hydrate(&roots, &store, &mut lanes, &call)?;
    assert_eq!(status::lane_display_state(&lanes[0]), "cloud_passed");
    assert!(condition_met(&lanes, Condition::All));
    let mut data = json!({});
    let mut human = String::new();
    status::attach_cloud_jobs(&store, &lanes, &mut data, &mut human, &call)
        .map_err(|error| error.message)?;
    assert_eq!(data["cloud_jobs"][0]["state"]["state"], "finished");
    assert_eq!(data["cloud_jobs"][0]["passed"], true);
    assert!(human.contains("advisory pass"));
    let mut setup_failed = finished;
    setup_failed.state = RemoteState::SetupFailed {
        detail: "environment setup failed".into(),
    };
    let diagnostic = cloud_owner::view(&setup_failed);
    assert_eq!(diagnostic["target"], "target");
    assert_eq!(diagnostic["environment"], "env");
    assert!(
        diagnostic["remedy"]
            .as_str()
            .is_some_and(|text| text.contains("repair"))
    );
    Ok(())
}

#[test]
fn cloud_saved_transport_ignores_later_host_configuration() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_providers::cloud::{CloudProvider, RemoteState};
    use provenant_providers::cloud_cli::{CloudCli, CodexCloud};
    #[derive(Default)]
    struct FakeCli(Vec<(String, Vec<String>)>);
    impl CloudCli for FakeCli {
        fn run(
            &mut self,
            program: &str,
            args: &[&str],
            stdin: Option<&str>,
        ) -> Result<String, String> {
            assert!(stdin.is_none());
            self.0
                .push((program.into(), args.iter().map(|s| (*s).into()).collect()));
            Ok(r#"{"thread":{"id":"thread-1","status":{"active":{}}},"turns":{"data":{"values":[{"id":"turn-1","status":"TURN_STATUS_IN_PROGRESS"}]}}}"#.into())
        }
    }
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    std::fs::create_dir_all(root.join("state"))?;
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let target = crate::domains::CloudTarget {
        provider: "codex".into(),
        environment: "env".into(),
        model: "requested".into(),
        effort: None,
        repository: "repository".into(),
        branch: "main".into(),
        deadline_seconds: 3600,
    };
    let mut job = cloud_owner::prepare_job(
        AttemptId::from_bytes([41; 16]),
        "target",
        &target,
        &"a".repeat(40),
        &SystemClock::new(),
    )?;
    job.remote_id = "thread-1".into();
    job.state = RemoteState::Running;
    job.cloud_transport = provenant_config::CodexCloudTransport::Thread;
    job.cloud_command = Some("first-thread-client".into());
    for command in ["first-thread-client", "replacement-thread-client"] {
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            format!(
                "schema_version=1\n[providers.codex.cloud]\ntransport='thread'\ncommand='{command}'\n"
            ),
        )?;
        // Later host config applies to new submissions only; the job keeps its saved command.
        let mut provider = CodexCloud::new(
            FakeCli::default(),
            cloud_owner::cloud_settings(&roots, &job.provider)?,
        );
        assert_eq!(provider.observe(&job)?, RemoteState::Running);
        assert_eq!(
            provider.cli.0,
            [(
                "first-thread-client".into(),
                vec!["status".into(), "thread-1".into()]
            )]
        );
    }
    Ok(())
}
