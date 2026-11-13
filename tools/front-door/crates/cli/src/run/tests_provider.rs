use super::*;
use provenant_records::CanonicalV1;

#[test]
fn capacity_guardian_context_records_and_reports_config_failure()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::DirBuilderExt;
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    std::fs::DirBuilder::new().mode(0o700).create(&state)?;
    let context = run::HolderContext {
        schema_version: 1,
        roots: run::RootSeed {
            home: temp.path().join("home"),
            checkout: temp.path().join("checkout"),
            state_root: state.clone(),
        },
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        attempt_id: AttemptId::from_bytes([4; 16]),
        host_delivery: None,
        launch: None,
        sealed: None,
    };
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-sol".into()),
        account_key: Some("key".into()),
        account_label: Some("label".into()),
        ..Default::default()
    };
    let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let registry = Registry::default();
    let decode = || {
        registry.decode_captures(
            &route,
            stream,
            &[],
            &exit,
            Path::new("/nonexistent"),
            Some(&context),
        )
    };
    std::fs::write(state.join("config.toml"), "invalid = [")?;
    let failed = decode()?;
    assert_eq!(failed.outcome, run::Outcome::ModelUnavailable);
    assert!(
        failed
            .reason
            .unwrap_or_default()
            .contains("capacity cooldown configuration unavailable")
    );
    std::fs::write(
        state.join("config.toml"),
        "schema_version = 1\nprofile = 'balanced'\n[domains.routing]\ncapacity_cooldown_seconds = 0\n",
    )?;
    let disabled = decode()?;
    assert_eq!(
        disabled.reason.as_deref(),
        Some(
            "provider capacity: the selected model is overloaded; wait or choose an eligible route"
        )
    );
    assert!(!state.join("quota").exists());
    std::fs::remove_file(state.join("config.toml"))?;
    decode()?;
    let record = provenant_quota::AccountRecord::decode(&std::fs::read(
        state.join("quota/key/codex.json"),
    )?)?;
    assert_eq!(record.capacity_cooldowns.len(), 1);
    let before = std::fs::read(state.join("quota/key/codex.json"))?;
    let mut nested = context.clone();
    nested.roots.state_root = state.join("children/child");
    nested.launch = Some(run::HostLaunchContext {
        graph_directory: None,
        configuration_root: state.clone(),
        cwd: temp.path().join("checkout"),
        executables: Default::default(),
        executable_search_path: Vec::new(),
        git: PathBuf::from("/usr/bin/git"),
        default_account_directories: Default::default(),
        credential_roots: Default::default(),
        github_roots: Default::default(),
        provider_path_environment: Default::default(),
        cargo_build_jobs: None,
        #[cfg(feature = "fixture")]
        fixture_state_write: false,
    });
    let nested_result = registry.decode_captures(
        &route,
        stream,
        &[],
        &exit,
        Path::new("/nonexistent"),
        Some(&nested),
    )?;
    assert_eq!(nested_result.outcome, run::Outcome::ModelUnavailable);
    assert_eq!(std::fs::read(state.join("quota/key/codex.json"))?, before);
    assert!(!nested.roots.state_root.join("quota").exists());
    Ok(())
}
#[test]
fn claude_and_opencode_overload_reach_the_decoded_outcome() {
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    for (provider, model, stream) in [
        (
            "claude",
            "claude/claude-sonnet-5-5",
            r#"{"type":"result","session_id":"s","subtype":"error_during_execution","is_error":true,"api_error_status":529,"result":"Overloaded"}"#,
        ),
        (
            "opencode",
            "opencode/anthropic/test",
            r#"{"type":"error","sessionID":"ses_x","error":{"type":"provider.internal","status":529,"message":"Overloaded"}}"#,
        ),
    ] {
        let route = run::RouteRecord {
            provider: Some(provider.into()),
            model: Some(model.into()),
            ..Default::default()
        };
        let decoded = Registry::default()
            .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
        assert!(
            decoded
                .reason
                .as_deref()
                .unwrap()
                .starts_with("provider capacity: the selected model is overloaded")
        );
        assert!(decoded.session.is_some());
    }
}
#[test]
fn codex_rate_limit_reaches_the_decoded_outcome() {
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6-luna".into()),
        ..Default::default()
    };
    let registry = Registry::default();
    for (message, _expected) in [
        (
            "exceeded retry limit, last status: 429 Too Many Requests".to_owned(),
            "exceeded retry limit, last status: 429 Too Many Requests".to_owned(),
        ),
        (
            "429 Too Many Requests TOKEN_SENTINEL /private/source \u{1b} ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
            "429 Too Many Requests for [REDACTED]".into(),
        ),
        (
            format!("429 Too Many Requests {}", "☀".repeat(3000)),
            format!("429 Too Many Requests {}", "☀".repeat(2723)),
        ),
    ] {
        let stream = [
            json!({"type":"thread.started","thread_id":"01234567-89ab-cdef-0123-456789abcdef"}),
            json!({"type":"error","message":message}),
            json!({"type":"turn.failed","error":{"message":message}}),
        ]
        .map(|frame| format!("{frame}\n"))
        .concat();
        let decoded = registry
            .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::RateLimited);
        let reason = decoded.reason.as_deref().unwrap();
        assert!(reason.contains("rate limited"));
        assert!(reason.contains("wait"));
        assert!(!reason.contains(&message));
        assert!(!reason.contains("ghp_"));
        assert!(!reason.contains("TOKEN_SENTINEL"));
        assert!(!reason.contains("/private/source"));
        assert!(!reason.contains('\u{1b}'));
        assert!(reason.len() <= 1024);
    }
}
#[test]
fn agy_denials_preserve_answers_and_prior_failures() {
    let route = run::RouteRecord {
        provider: Some("agy".into()),
        model: Some("agy/gemini-3.8-flash".into()),
        ..Default::default()
    };
    let fixture = include_str!("../../../../tests/fixtures/providers/agy/denied-result.jsonl")
        .replace("RunCommand", "TOKEN_SENTINEL/private/source");
    let recovered = fixture.replace("\"response\":\"\"", "\"response\":\"Recovered answer\"");
    let registry = Registry::default();
    let decoded = registry
        .decode(
            &route,
            recovered.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    assert_eq!(decoded.result, "Recovered answer");
    assert_eq!(
        decoded.reason.as_deref(),
        Some("provider denied an action; check account access and inspect the result")
    );
    let decoded = registry
        .decode(
            &route,
            fixture.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    // A refusal the provider reports with no answer is its own failed state,
    // never `succeeded`, and names the denied action (adapter-validated).
    assert_eq!(decoded.outcome, run::Outcome::PermissionDenied);
    assert_eq!(
        decoded.reason.as_deref(),
        Some(
            "provider denied an action; check account access and inspect the result; denied action: unknown action"
        )
    );
    let mut failed_exit = clean_exit();
    failed_exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = registry
        .decode(
            &route,
            fixture.as_bytes(),
            &failed_exit,
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::Failed);
    assert_eq!(decoded.reason.as_deref(), Some("provider failed"));
}
#[test]
fn codex_rollout_error_reaches_the_decoded_reason() {
    let exit = clean_exit();
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6-luna".into()),
        ..Default::default()
    };
    let registry = Registry {
        roots: None,
        home: None,
        assets: None,
        cwd: None,
        state_root: None,
        project_id: None,
        host: None,
        cancel: None,
        repo_facts: Default::default(),
    };
    let stream = [
        json!({"type":"thread.started","thread_id":"01234567-89ab-cdef-0123-456789abcdef"}),
        json!({"type":"item.completed","item":{"type":"agent_message","text":"OK"}}),
        json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}),
    ]
    .map(|frame| format!("{frame}\n"))
    .concat();
    let decoded = registry
        .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    assert_eq!(decoded.result, "OK");
    assert_eq!(
        decoded.session.as_deref(),
        Some("01234567-89ab-cdef-0123-456789abcdef")
    );
    assert!(decoded.observed_route.is_none());
    assert_eq!(
        decoded.reason.as_deref(),
        Some("Codex rollout identity could not be verified; inspect the local session evidence")
    );
}
#[test]
fn reviewer_receipts_cover_provenance_and_resolution_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let mut record: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1, "attempt_id": AttemptId::from_bytes([1; 16]),
        "key": {"project": provenant_core::ProjectId::from_bytes([3; 16]), "graph": "receipt", "node": "test", "attempt": 1},
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "attempt_number": 1, "revision": 1, "generation": 0,
        "requested_route": {"kind":"model","provider":"codex","model":"codex/gpt-6.1-sol"},
        "applied_route": {"provider":"codex","model":"codex/gpt-6.1-sol","effort":"high","account_label":null},
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]), "queued_at": now,
        "state": {"state":"terminal","outcome":"lost","exit_code":null,"finished_at":now,"reason":null},
        "usage": {}, "prompt": {"path":{"encoding":"utf8","path":"prompt.txt"}, "digest":CanonicalV1::file_digest(b""),"bytes":0,"complete":true,"redacted":false}
    }))?;
    let (data, line) = reviewer_receipt(&record, false).unwrap();
    assert_eq!(data["resolved_from"], "explicit");
    assert_eq!(data["reviewer"]["provenance"], "unknown");
    assert_eq!(data["reviewer"]["actor_id"], "unknown");
    // One grep-able line naming role, applied model and effort, family,
    // resolution source and the attempt; the text form leads with it.
    let receipt = data["receipt"].as_str().ok_or("receipt line")?;
    assert!(!receipt.contains('\n'), "{receipt}");
    assert!(receipt.starts_with("Reviewer: "), "{receipt}");
    for part in [
        "codex/gpt-6.1-sol@high".to_owned(),
        "openai".to_owned(),
        "explicit".to_owned(),
        record.key.to_string(),
        record.attempt_id.to_string(),
    ] {
        assert!(receipt.contains(&part), "{part}: {receipt}");
    }
    assert!(line.starts_with(&format!("{receipt}\n")), "{line}");
    assert!(
        line.ends_with(
            "Reviewer provenance: unknown, actor unknown, model unknown, family unknown\n"
        )
    );
    record.verified_owner = Some(ActorId::from_bytes([6; 16]));
    let unobserved = reviewer_receipt(&record, false).unwrap().0;
    assert_eq!(unobserved["reviewer"]["provenance"], "unknown");
    assert_ne!(unobserved["reviewer"]["actor_id"], "unknown");
    record.observed_route = record.applied_route.clone();
    let observed_fact = run::fact(record.clone())?.reviewer.ok_or("reviewer fact")?;
    let observed = reviewer_receipt(&record, false).unwrap();
    assert_eq!(observed.0["reviewer"]["provenance"], "observed");
    assert_eq!(
        observed.0["reviewer"]["actor_id"],
        observed_fact.actor_id.to_string()
    );
    assert_eq!(observed.0["reviewer"]["family"], "openai");
    assert!(observed.1.contains(&format!(
        "Reviewer provenance: observed, actor {},",
        observed_fact.actor_id
    )));
    record.applied_route.as_mut().unwrap().model = Some("gpt-6.1-sol".into());
    let (writer_data, writer_line) = reviewer_receipt(&record, true).unwrap();
    assert_eq!(writer_data["role"], "writer");
    assert_eq!(writer_data["family"], "openai");
    assert!(writer_line.starts_with("Writer: codex/gpt-6.1-sol@high (openai;"));
    record.applied_route.as_mut().unwrap().effort = None;
    assert!(
        reviewer_receipt(&record, false)
            .unwrap()
            .1
            .contains("@unknown")
    );
    record.applied_route.as_mut().unwrap().model = Some("codex/uncatalogued".into());
    let unknown_family = reviewer_receipt(&record, false).unwrap().0;
    assert_eq!(unknown_family["family"], "unknown");
    assert_eq!(unknown_family["resolved_from"], "unknown");
    record.skipped_routes.push(serde_json::from_value(
        json!({"route":"codex/gpt-6.1-sol","kind":"unavailable","detail":"unavailable"}),
    )?);
    assert_eq!(
        reviewer_receipt(&record, false).unwrap().0["resolved_from"],
        "unknown"
    );
    record.applied_route.as_mut().unwrap().model = Some("codex/gpt-6.1-sol".into());
    record.applied_route.as_mut().unwrap().pool_selection = Some(serde_json::from_value(
        json!({"pool":"junior","candidates":[],"roll":0,"total_weight":1,"pick":"codex/gpt-6.1-sol"}),
    )?);
    // An unavailable candidate is still a pool selection, not a fallback.
    assert_eq!(
        reviewer_receipt(&record, false).unwrap().0["resolved_from"],
        "pool junior"
    );
    record.applied_route.as_mut().unwrap().model = Some("codex/other".into());
    assert_eq!(
        reviewer_receipt(&record, false).unwrap().0["resolved_from"],
        "unknown"
    );
    record.applied_route = None;
    record.observed_route = None;
    let unknown = reviewer_receipt(&record, false).unwrap().0;
    for field in ["adapter", "model", "effort", "family", "resolved_from"] {
        assert_eq!(unknown[field], "unknown", "{field}");
    }
    record.applied_route = Some(Default::default());
    assert_eq!(
        reviewer_receipt(&record, false).unwrap().0["model"],
        "unknown"
    );
    record.state = run::AttemptState::Running {
        extra: Default::default(),
    };
    let refused = reviewer_receipt(&record, false).unwrap_err();
    assert_eq!(refused.status, Status::Refused);
    assert_eq!(refused.kind, ErrorKind::Nonterminal);
    Ok(())
}
#[test]
fn opencode_without_a_session_record_observes_no_model() {
    let route = run::RouteRecord {
        provider: Some("opencode".into()),
        model: Some("opencode/opencode/glm-5.3-flash-free".into()),
        ..Default::default()
    };
    let registry = Registry::default();
    let stream = [
        json!({"type":"text","sessionID":"ses_a","part":{"text":"OK"}}),
        json!({"type":"step_finish","sessionID":"ses_a","part":{"reason":"stop"}}),
    ]
    .map(|frame| format!("{frame}\n"))
    .concat();
    let decoded = registry
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/nonexistent"),
        )
        .unwrap();
    assert_eq!(decoded.result, "OK");
    assert!(decoded.observed_route.is_none());
    assert_eq!(
        decoded.reason.as_deref(),
        Some("OpenCode session identity could not be verified; inspect the local session evidence")
    );
}
#[test]
fn claude_session_reaches_the_decoded_attempt() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5".into()),
        ..Default::default()
    };
    let stream = include_str!("../../../../tests/fixtures/providers/claude/fresh.jsonl");
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(
        decoded.session.as_deref(),
        Some("FIXTURE_SESSION_IDENTIFIER_003")
    );
}
#[test]
fn agy_repeated_answer_is_captured_once() {
    let answer = "# Review\n\n```findings\n{\"schema_version\":1,\"findings\":[{\"id\":\"F1\",\"severity\":\"low\",\"file\":\"a.rs\",\"summary\":\"issue\",\"status\":\"open\"}]}\n```\nVERDICT: PASS\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```";
    let route = run::RouteRecord {
        provider: Some("agy".into()),
        ..Default::default()
    };
    let decode = |step: &str, response: &str| {
        let stream = format!(
            "{}\n{}\n{}\n",
            json!({"event":"init","conversation_id":"s","init":{"model":"m"}}),
            json!({"event":"step_update","step_update":{"conversation_id":"s","step_type":"agent_response","text_delta":step}}),
            json!({"event":"result","result":{"conversation_id":"s","status":"SUCCESS","response":response,"usage":{"input_tokens":1,"output_tokens":1}}})
        );
        Registry::default()
            .decode(
                &route,
                stream.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap()
    };
    for decoded in [
        decode(&format!("{answer}\n"), answer),
        decode("", &format!("  {answer}\n\n{answer}  ")),
    ] {
        assert_eq!(decoded.outcome, run::Outcome::Succeeded);
        assert_eq!(decoded.result, answer);
        let mut capture = provenant_records::result_capture::ResultCapture::new(None);
        capture.push(decoded.result.as_bytes());
        let captured = capture.finish();
        assert!(
            provenant_records::handoff::Handoff::from_turns(&decoded.result, &[])
                .unwrap()
                .is_some()
        );
        assert_eq!(captured.verdict.as_deref(), Some("VERDICT: PASS"));
        assert_eq!(captured.declarations, 1);
        assert_eq!(decoded.result.matches("```findings").count(), 1);
    }
    for response in [
        format!("{answer}\nextra"),
        format!(
            "{answer}\n{}",
            answer.replace("\"note\":\"done\"", "\"note\":\"different\"")
        ),
    ] {
        let decoded = decode("", &response);
        assert_eq!(decoded.result, response);
        assert!(provenant_records::handoff::Handoff::from_turns(&decoded.result, &[]).is_err());
    }
}
#[test]
fn repeated_answer_from_non_agy_adapter_is_unchanged() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        ..Default::default()
    };
    let stream = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\\nanswer\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n"
    );
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.result, "answer\nanswer");
}
#[test]
fn a_claude_reply_to_a_late_notification_keeps_the_final_handoff() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        ..Default::default()
    };
    let handoff = "Report.\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"finished\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```";
    let result = |text: &str| json!({"type":"result","subtype":"success","is_error":false,"session_id":"s","result":text,"usage":{"input_tokens":1,"output_tokens":1}});
    let tool_use = json!({"type":"assistant","message":{"role":"assistant","model":"m","content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]},"session_id":"s","parent_tool_use_id":null});
    let notification = json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"<task-notification>done</task-notification>"}]},"session_id":"s","parent_tool_use_id":null});
    let decode = |late: &[&serde_json::Value]| {
        let mut stream = format!(
            "{}\n{}\n{}\n",
            json!({"type":"system","subtype":"init","session_id":"s"}),
            tool_use,
            result(handoff)
        );
        for event in late {
            stream.push_str(&format!("{event}\n"));
        }
        stream.push_str(&format!("{}\n", result("The wait was redundant.")));
        Registry::default()
            .decode(
                &route,
                stream.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap()
    };
    let decoded = decode(&[&notification]);
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    // Each turn starts on its own line, so the fence stays intact.
    assert_eq!(
        decoded.result,
        format!("{handoff}\nThe wait was redundant.")
    );
    assert!(provenant_records::handoff::Handoff::from_turns(&decoded.result, &[]).is_err());
    assert_eq!(decoded.handoff().unwrap().unwrap().note, "finished");
    // A tool run after the declaration leaves the lane without one.
    let decoded = decode(&[&notification, &tool_use]);
    assert_eq!(decoded.handoff(), Ok(None));
}
#[test]
fn claude_usage_records_cache_read_and_cache_write() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5".into()),
        ..Default::default()
    };
    let stream = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\",\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":40,\"cache_creation\":{\"ephemeral_1h_input_tokens\":25}}}\n",
    );
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.usage.uncached_input, Some(10));
    assert_eq!(decoded.usage.output_excluding_reasoning, Some(20));
    assert_eq!(decoded.usage.cache_read, Some(30));
    assert_eq!(decoded.usage.cache_write, Some(40));
    assert_eq!(decoded.usage.cache_write_1h, Some(25));
    assert_eq!(decoded.usage.source.as_deref(), Some("provider"));
}
#[test]
fn a_claude_success_after_a_tool_result_with_a_wide_integer_is_published() {
    // Shape of #3111: a sub-agent stream whose tool result echoes a JSON Schema
    // bound of u64::MAX printed as a float, then a complete successful result.
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5".into()),
        ..Default::default()
    };
    let decode = |stream: &str| {
        Registry::default()
            .decode(
                &route,
                stream.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap()
    };
    let head = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
        "{\"type\":\"system\",\"subtype\":\"task_started\",\"session_id\":\"s\"}\n",
        "{\"type\":\"assistant\",\"session_id\":\"s\",\"parent_tool_use_id\":null,\"message\":{\"model\":\"claude-sonnet-5\",\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"ToolSearch\",\"input\":{}}]}}\n",
        "{\"type\":\"user\",\"parent_tool_use_id\":null,\"session_id\":\"s\",\"message\":{\"role\":\"user\",\"content\":[{\"tool_use_id\":\"t\",\"type\":\"tool_result\",\"content\":\"listed\"}]},\"tool_use_result\":{\"tools\":[{\"inputSchema\":{\"properties\":{\"generation\":{\"maximum\":18446744073709552000,\"minimum\":0,\"type\":\"integer\"}}}}]}}\n",
    );
    let decoded = decode(&format!(
        "{head}{}",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\",\"stop_reason\":\"end_turn\",\"terminal_reason\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":20}}\n",
    ));
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    assert_eq!(decoded.result, "answer");
    assert_eq!(decoded.reason, None);
    assert_eq!(decoded.usage.uncached_input, Some(10));
    assert_eq!(decoded.usage.output_excluding_reasoning, Some(20));
    assert_eq!(decoded.usage.source.as_deref(), Some("provider"));
    // A final frame that is genuinely malformed still settles as malformed.
    let decoded = decode(&format!(
        "{head}{}",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"ans\n",
    ));
    assert_eq!(decoded.outcome, run::Outcome::Failed);
    assert!(!decoded.complete);
    assert!(decoded.result.is_empty());
    assert_eq!(
        decoded.reason.as_deref(),
        Some("provider protocol is malformed")
    );
}
#[test]
fn a_claude_success_that_hit_its_token_limit_is_flagged_not_failed() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5".into()),
        ..Default::default()
    };
    let stream = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"half\",\"stop_reason\":\"max_tokens\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}\n",
    );
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.partial_signals, [run::PartialSignal::ProviderStop]);
    assert_eq!(
        decoded.reason.as_deref(),
        Some("provider stopped early; inspect the result and resume if needed")
    );
    assert_ne!(decoded.outcome, run::Outcome::Failed);
}
#[test]
fn streamed_deltas_stay_joined_without_a_message_boundary() {
    let route = run::RouteRecord {
        provider: Some("agy".into()),
        ..Default::default()
    };
    let delta = |text: &str| {
        json!({"event":"step_update","step_update":{"conversation_id":"s","step_type":"agent_response","text_delta":text}})
            .to_string()
    };
    let stream = [
        json!({"event":"init","conversation_id":"s","init":{"model":"m"}}).to_string(),
        delta("VERDICT"),
        delta(": PA"),
        delta("SS"),
    ]
    .join("\n");
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.result, "VERDICT: PASS");
}
#[test]
fn a_handoff_in_its_own_codex_message_is_read_after_prose() {
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-terra".into()),
        ..Default::default()
    };
    let handoff = "```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"n\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```";
    let message = |text: &str| {
        serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":text}})
            .to_string()
    };
    let stream = [
        r#"{"type":"thread.started","thread_id":"t"}"#.to_owned(),
        message("Checking the commit."),
        message("VERDICT: PASS"),
        message(handoff),
        r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}"#.to_owned(),
    ]
    .join("\n");
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    let mut capture = provenant_records::result_capture::ResultCapture::new(Some("VERDICT: PASS"));
    capture.push(decoded.result.as_bytes());
    let captured = capture.finish();
    let declared = provenant_records::handoff::Handoff::from_turns(&decoded.result, &[])
        .expect("handoff parses")
        .expect("handoff found");
    assert_eq!(declared.note, "n");
    assert_eq!(captured.verdict.as_deref(), Some("VERDICT: PASS"));
    assert!(
        decoded
            .result
            .starts_with("Checking the commit.\nVERDICT: PASS\n")
    );
}
#[test]
fn codex_usage_records_cache_read_apart_from_uncached_input() {
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-terra".into()),
        ..Default::default()
    };
    let stream = concat!(
        "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"answer\"}}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":80,\"output_tokens\":7}}\n",
    );
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.usage.uncached_input, Some(20));
    assert_eq!(decoded.usage.cache_read, Some(80));
    assert_eq!(decoded.usage.cache_write, None);
    assert_eq!(decoded.usage.output_excluding_reasoning, Some(7));
}

#[test]
fn codex_last_turn_completed_is_the_session_total() {
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        ..Default::default()
    };
    let stream = concat!(
        "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":80,\"output_tokens\":7}}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":150,\"cached_input_tokens\":100,\"output_tokens\":12}}\n",
    );
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.usage.uncached_input, Some(50));
    assert_eq!(decoded.usage.cache_read, Some(100));
    assert_eq!(decoded.usage.output_excluding_reasoning, Some(12));
}
#[test]
fn command_exit_keeps_requested_route() {
    let route = run::RouteRecord {
        provider: Some("command".into()),
        model: None,
        ..Default::default()
    };
    let decoded = Registry::default()
        .decode(&route, b"output", &clean_exit(), Path::new("/unused"))
        .unwrap();
    let observed = decoded.observed_route.expect("observed route");
    assert_eq!(observed.provider.as_deref(), Some("command"));
    assert_eq!(observed.source.as_deref(), Some("command_exit"));
}
#[test]
fn claude_absent_usage_names_its_reason() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5".into()),
        ..Default::default()
    };
    let stream = "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n";
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.usage.uncached_input, None);
    assert_eq!(
        decoded.usage.source.as_deref(),
        Some("absent: no result event")
    );

    let stream_no_usage = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
        "{\"type\" : \"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\"}\n",
    );
    let decoded_no_usage = Registry::default()
        .decode(
            &route,
            stream_no_usage.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded_no_usage.usage.uncached_input, None);
    assert_eq!(
        decoded_no_usage.usage.source.as_deref(),
        Some("absent: result event has no usage")
    );

    let unknown = run::RouteRecord {
        provider: Some("unregistered-fixture".into()),
        ..Default::default()
    };
    let decoded_unknown = Registry::default()
        .decode(&unknown, b"", &clean_exit(), Path::new("/unused"))
        .unwrap();
    assert_eq!(decoded_unknown.usage.source, None);
}
#[test]
fn every_adapter_sign_in_failure_settles_auth_required() -> Result<(), Box<dyn std::error::Error>> {
    use run::Providers;
    let fixtures: &[(&str, &[u8])] = &[
        (
            "cursor",
            include_bytes!("../../../../tests/fixtures/providers/cursor/auth-required.stderr"),
        ),
        (
            "codex",
            include_bytes!("../../../../tests/fixtures/providers/codex/auth-required.stderr"),
        ),
        (
            "opencode",
            include_bytes!("../../../../tests/fixtures/providers/opencode/auth-required.stderr"),
        ),
        (
            "agy",
            include_bytes!("../../../../tests/fixtures/providers/agy/auth-required.stderr"),
        ),
    ];
    let temp = tempfile::tempdir()?;
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    for (adapter, stderr) in fixtures {
        let route = run::RouteRecord {
            provider: Some((*adapter).into()),
            model: Some(format!("{adapter}/example")),
            ..Default::default()
        };
        let decoded =
            Registry::default().decode_captures(&route, b"", stderr, &exit, temp.path(), None)?;
        assert_eq!(decoded.outcome, run::Outcome::AuthRequired, "{adapter}");
        assert!(
            decoded
                .reason
                .as_deref()
                .unwrap()
                .contains("provenant quota tick")
        );
    }
    Ok(())
}
#[test]
fn settled_auth_uses_only_the_selected_registered_account() -> Result<(), Box<dyn std::error::Error>>
{
    use run::Providers;
    let temp = tempfile::tempdir()?;
    let mut inputs =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
    inputs.state_dir = Some(temp.path().join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    std::fs::write(
        roots.state.as_path().join("config.toml"),
        "schema_version = 1\n[domains.accounts.entries.work]\nadapter = 'codex'\nlabel = 'work'\nkey = 'key'\ndirectory = '/fixture/work'\n",
    )?;
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/example".into()),
        account_key: Some("key".into()),
        ..Default::default()
    };
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = Registry {
        roots: Some(&roots),
        ..Default::default()
    }
    .decode_captures(
        &route,
        b"",
        include_bytes!("../../../../tests/fixtures/providers/codex/auth-required.stderr"),
        &exit,
        temp.path(),
        None,
    )?;
    assert_eq!(decoded.outcome, run::Outcome::AuthRequired);
    let reason = decoded.reason.unwrap();
    assert!(reason.contains("work [codex]"), "{reason}");
    assert!(reason.contains("codex login"), "{reason}");
    assert!(reason.contains("/fixture/work"), "{reason}");
    Ok(())
}
/// A Claude lane holds a launch-time copy of its account's token, so a 401
/// names the account and says the lane token was lost, not that it needs a login.
#[test]
fn claude_lane_auth_failure_reports_a_lost_lane_token_for_its_account()
-> Result<(), Box<dyn std::error::Error>> {
    use run::Providers;
    let temp = tempfile::tempdir()?;
    let mut inputs =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
    inputs.state_dir = Some(temp.path().join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    std::fs::write(
        roots.state.as_path().join("config.toml"),
        "schema_version = 1\n[domains.accounts.entries.pro]\nadapter = 'claude'\nlabel = 'pro'\nkey = 'key'\ndirectory = '/fixture/pro'\n",
    )?;
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/example".into()),
        account_key: Some("key".into()),
        ..Default::default()
    };
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = Registry {
        roots: Some(&roots),
        ..Default::default()
    }
    .decode_captures(
        &route,
        include_bytes!("../../../../tests/fixtures/providers/claude/live-auth-required.jsonl"),
        b"",
        &exit,
        temp.path(),
        None,
    )?;
    assert_eq!(decoded.outcome, run::Outcome::AuthRequired);
    assert_eq!(
        decoded.terminal_cause,
        Some(run::TerminalCause::LaneTokenLost)
    );
    let reason = decoded.reason.unwrap();
    assert!(reason.contains("pro [claude]"), "{reason}");
    assert!(
        reason.contains("lane's access token was revoked or expired"),
        "{reason}"
    );
    assert!(!reason.contains("claude login"), "{reason}");
    Ok(())
}
/// When a Claude lane ran on a registered `provenant-lane-token`, a 401
/// settles as a token problem without continuation, naming the account and
/// directing the owner to recreate it with `claude setup-token`.
#[test]
fn claude_lane_auth_failure_with_registered_token_settles_without_continuation()
-> Result<(), Box<dyn std::error::Error>> {
    use run::Providers;
    let temp = tempfile::tempdir()?;
    let mut inputs =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
    inputs.state_dir = Some(temp.path().join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    let account_dir = temp.path().join("pro");
    std::fs::create_dir_all(&account_dir)?;
    let token_path = account_dir.join(provenant_providers::claude::LANE_TOKEN_FILE);
    std::fs::write(&token_path, "fixture-lane-token\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::write(
        roots.state.as_path().join("config.toml"),
        format!(
            "schema_version = 1\n[domains.accounts.entries.pro]\nadapter = 'claude'\nlabel = 'pro'\nkey = 'key'\ndirectory = '{}'\n",
            account_dir.display()
        ),
    )?;
    let mut route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/example".into()),
        account_key: Some("key".into()),
        ..Default::default()
    };
    route.extra.insert("lane_token".into(), true.into());
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = Registry {
        roots: Some(&roots),
        ..Default::default()
    }
    .decode_captures(
        &route,
        include_bytes!("../../../../tests/fixtures/providers/claude/live-auth-required.jsonl"),
        b"",
        &exit,
        temp.path(),
        None,
    )?;
    assert_eq!(decoded.outcome, run::Outcome::AuthRequired);
    assert_eq!(decoded.terminal_cause, None);
    let reason = decoded.reason.unwrap();
    assert!(reason.contains("pro [claude]"), "{reason}");
    assert!(
        reason.contains("registered lane token was rejected"),
        "{reason}"
    );
    assert!(reason.contains("claude setup-token"), "{reason}");
    assert!(
        !reason.contains("lane's access token was revoked or expired"),
        "{reason}"
    );
    assert!(!reason.contains("claude login"), "{reason}");
    Ok(())
}
#[test]
fn successful_output_with_incidental_auth_stderr_stays_successful() {
    let route = run::RouteRecord {
        provider: Some("cursor".into()),
        model: Some("cursor/example".into()),
        ..Default::default()
    };
    let decoded = Registry::default()
        .decode_captures(
            &route,
            include_bytes!("../../../../tests/fixtures/providers/cursor/fresh.jsonl"),
            include_bytes!("../../../../tests/fixtures/providers/cursor/auth-required.stderr"),
            &clean_exit(),
            Path::new("/unused"),
            None,
        )
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
}
