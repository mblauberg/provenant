use super::*;

#[test]
fn graph_quality_reports_missing_checkpoint_without_failing()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, ProjectId, WallTime};
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
        key: provenant_records::WorkKey::new(project, "retired", "node", 1)?,
        attempt_number: 1,
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
