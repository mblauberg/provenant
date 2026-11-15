use super::*;

#[test]
fn graph_quality_reports_missing_checkpoint_without_failing()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, ProjectId, WallTime};
    use provenant_records::{AttemptFact, AttemptOutcome, Usage};
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let inputs = provenant_test_support::root_inputs(&base)?;
    let root = provenant_core::StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let project = ProjectId::from_bytes([3; 16]);
    let fact = AttemptFact {
        routing_context: Some(Default::default()),
        context: Some(provenant_records::run::WorkContext {
            graph_id: Some("retired".into()),
            ..Default::default()
        }),
        attempt_id: AttemptId::from_bytes([1; 16]),
        key: provenant_records::WorkKey::new(project, "retired", "node", 1)?,
        attempt_number: 1,
        project_id: project,
        terminal_state: AttemptOutcome::Succeeded,
        cause_source: provenant_records::run::CauseSource::Unavailable,
        settlement_reason: None,
        account_identity: provenant_records::AccountIdentity::unknown(),
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
            json!({"id":"repair", "state":"succeeded", "attempts":[{"launched":true,"key":{"graph_id":"g","node_id":"repair","route_index":0, "attempt": 1},"route":{"alias":"utility"},"state":"terminal","launch_actor":"actor"}], "round":{"loop_id":"loop","round":2,"node_id":"repair","until":false}}),
        ),
        node(
            json!({"id":"configured_only", "state":"pending", "attempts":[], "round":{"loop_id":"loop","round":3,"node_id":"configured_only","until":false}}),
        ),
    ];
    assert_eq!(graph_quality_counts(&checkpoint_nodes), (1, 1, 1));
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
fn host_delivery_limit_cli_projection_preserves_its_typed_fact()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::diagnostic::{DeliveryCommand, DiagnosticFact};
    for (tool, command, expected_command) in [
        ("git", "worktree list", DeliveryCommand::GitWorktreeList),
        ("gh", "pr", DeliveryCommand::GhPr),
    ] {
        let limit = provenant_os::exec::DEFAULT_CAPTURE_LIMIT;
        let failure = super::Fail::from(provenant_run::RunError::DeliveryLimit {
            stage: provenant_records::run::WriterDeliveryStage::Integrate,
            tool,
            command: command.into(),
            limit,
        });
        assert_eq!(failure.kind, ErrorKind::Io);
        let detail = failure.projection.as_ref().expect("typed detail");
        assert_eq!(detail.code, "IO");
        let Some(DiagnosticFact::DeliveryLimit(fact)) = detail.fact.as_ref() else {
            panic!("missing delivery fact: {detail:?}");
        };
        assert_eq!(
            fact.stage,
            provenant_records::run::WriterDeliveryStage::Integrate
        );
        assert_eq!(fact.command, expected_command);
        assert_eq!(fact.limit, limit as u64);
        assert_eq!(detail.detail.as_deref(), Some(fact.detail().as_str()));

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
        assert!(actual.contains("IO"));
        assert!(actual.contains(command));
    }
    let stage_failure = super::Fail::from(provenant_run::RunError::Delivery(
        provenant_records::run::WriterDeliveryStage::Prepare,
    ));
    assert!(matches!(
        stage_failure
            .projection
            .as_ref()
            .and_then(|detail| detail.fact.as_ref()),
        Some(DiagnosticFact::DeliveryStage(
            provenant_records::run::WriterDeliveryStage::Prepare
        ))
    ));
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
fn chair_cache_share_uses_only_reported_cache_reads() {
    use provenant_telemetry::{ChairModelUsage, ConfiguredPrices, usage::ChairSession};
    let prices = provenant_telemetry::validate_prices(&ConfiguredPrices {
        schema_version: 1,
        id: "empty".into(),
        reviewed_at: "2026-10-29".into(),
        units: "usd_microdollars_per_million_tokens".into(),
        max_review_age_days: None,
        discrepancy_threshold_bps: None,
        models: Vec::new(),
    })
    .unwrap();
    let chair = |usage| ChairSession {
        adapter: "claude".into(),
        account_identity: provenant_telemetry::usage::AccountIdentity::known(
            "claude",
            "work",
            "fixture-epoch",
        ),
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
    let report = |read| {
        provenant_telemetry::stats_with_chair([], [chair(usage(read))], &prices, Default::default())
            .unwrap()
    };
    let cache_row = |report: &provenant_telemetry::StatsReport| {
        report
            .cache
            .by_model
            .iter()
            .find(|row| {
                row.source == "chair" && row.model.as_deref() == Some("claude/claude-opus-5-5")
            })
            .expect("chair model cache row")
    };

    let reported = report(Some(90));
    assert_eq!(reported.scope, "host_chair_sessions");
    let row = cache_row(&reported);
    assert_eq!(row.cache.reporting, 1);
    assert_eq!(row.cache.missing, 0);
    assert_eq!(row.cache.input_tokens, 100);
    assert_eq!(row.cache.read_tokens, 90);
    assert_eq!(row.cache.read_share_basis_points, Some(9_000));
    assert_eq!(reported.chair.expect("chair report").sessions, 1);

    let absent = report(None);
    let row = cache_row(&absent);
    assert_eq!(row.cache.reporting, 0);
    assert_eq!(row.cache.missing, 1);
    assert_eq!(row.cache.read_share_basis_points, None);
}
#[test]
fn stats_scope_and_reasoning_fields_follow_typed_inputs() {
    let project = provenant_core::ProjectId::from_bytes([1; 16]);
    let scope = stats_scope_line(project, 2);
    assert!(scope.contains(&project.to_string()));
    assert!(scope.contains('2'));
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
    assert!(reasoning_line(&totals).is_none());
    totals.usage.reasoning.known = 42;
    assert!(reasoning_line(&totals).is_some());
    totals.usage.reasoning.missing = 3;
    assert!(reasoning_line(&totals).is_some());
}
#[test]
fn bare_word_model_is_rejected_with_invalid_status() -> Result<(), Box<dyn std::error::Error>> {
    let Err(bare) = super::requested(Some("junior"), None, None) else {
        panic!("bare-word model accepted");
    };
    assert_eq!(bare.status, Status::Invalid);
    assert_eq!(bare.kind, ErrorKind::Invalid);
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
