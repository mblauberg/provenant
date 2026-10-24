use super::*;
use provenant_records::ResolvedRoute;
use serde::Deserialize;

macro_rules! fixture {
    ($file:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/providers/",
            $file
        ))
    };
}
const EXIT: ExitObservation = ExitObservation {
    code: Some(0),
    timed_out: false,
};
fn replay(
    adapter: &dyn Adapter,
    bytes: &str,
    exit: ExitObservation,
) -> Result<(Vec<ProviderEvent>, ProviderOutcome), ProviderError> {
    let mut state = adapter.stream(None)?;
    let mut events = Vec::new();
    // Split inside UTF-8 and JSON tokens, not just at event boundaries.
    for chunk in bytes.as_bytes().chunks(17) {
        events.extend(adapter.observe(&mut state, chunk)?);
    }
    Ok((events, adapter.finish(state, exit)?))
}
#[test]
fn system_notifications_without_models_do_not_poison_streams() -> Result<(), ProviderError> {
    for adapter in [&claude::Claude as &dyn Adapter, &cursor::Cursor] {
        let stream = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
            "{\"type\":\"system\",\"subtype\":\"compact_boundary\",\"session_id\":\"s\"}\n",
            "{\"type\":\"system\",\"subtype\":\"api_retry\",\"session_id\":\"s\"}\n",
            "{\"type\":\"system\",\"subtype\":\"hook_response\",\"session_id\":\"s\"}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\",\"usage\":{\"inputTokens\":1,\"outputTokens\":2}}\n",
        );
        assert_eq!(
            replay(adapter, stream, EXIT)?.1.status(),
            OutcomeStatus::Success
        );
    }
    Ok(())
}
#[test]
fn error_results_need_neither_answer_nor_usage() -> Result<(), ProviderError> {
    for adapter in [&claude::Claude as &dyn Adapter, &cursor::Cursor] {
        for subtype in ["error_max_turns", "error_during_execution"] {
            let raw = format!(
                "{{\"type\":\"result\",\"subtype\":\"{subtype}\",\"is_error\":true,\"session_id\":\"s\"}}\n"
            );
            let (events, outcome) = replay(adapter, &raw, EXIT)?;
            assert!(events.contains(&ProviderEvent::Error));
            assert_eq!(outcome.status(), OutcomeStatus::Failed);
        }
    }
    Ok(())
}
#[test]
fn synthetic_and_api_error_messages_cannot_change_answering_identity() -> Result<(), ProviderError>
{
    let adapter = claude::Claude;
    let raw = fixture!("claude/direct-model.jsonl");
    let route = route("claude", "claude-haiku-4-5-20251001");
    let continuation = adapter.continuation(&replay(&adapter, raw, EXIT)?.1, &route, "grants")?;
    for (model, error, api_error) in [
        ("<synthetic>", "\"authentication_failed\"", false),
        ("error-model", "\"authentication_failed\"", false),
        ("error-model", "null", true),
        ("<synthetic>", "null", false),
    ] {
        let message = format!(
            "{{\"type\":\"assistant\",\"session_id\":\"FIXTURE_SESSION_IDENTIFIER_004\",\"error\":{error},\"is_api_error_message\":{api_error},\"message\":{{\"role\":\"assistant\",\"model\":\"{model}\",\"content\":[]}}}}\n"
        );
        let mut resumed = adapter.stream(Some(&continuation))?;
        let events = adapter.observe(&mut resumed, message.as_bytes())?;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ProviderEvent::Model(_)))
        );
        if error != "null" {
            assert_eq!(adapter.account(&resumed)?, AccountDetection::AuthRequired);
        }
        assert_ne!(
            adapter.finish(resumed, EXIT)?.status(),
            OutcomeStatus::Malformed
        );
        let (_, fresh) = replay(&adapter, &(message + raw), EXIT)?;
        assert_eq!(fresh.model(), Some(route.model.as_str()));
    }
    Ok(())
}
#[test]
fn recovered_transport_retry_succeeds_but_turn_failure_stays_failed() -> Result<(), ProviderError> {
    // Synthetic R9 replay: records recovery semantics, not CLI activation.
    for (message, failure, expected) in [
        ("Reconnecting… 1/5", "", OutcomeStatus::Success),
        (
            "Reconnecting… 1/5",
            "{\"type\":\"turn.failed\",\"error\":{\"message\":\"permanent\"}}\n",
            OutcomeStatus::Failed,
        ),
        ("invalid request", "", OutcomeStatus::Failed),
    ] {
        let raw = format!(
            "{{\"type\":\"error\",\"message\":\"{message}\"}}\n{failure}{}",
            fixture!("codex/success.jsonl")
        );
        assert_eq!(replay(&codex::Codex, &raw, EXIT)?.1.status(), expected);
    }
    Ok(())
}
#[test]
fn terminal_answer_and_nonzero_exit_remain_separate_facts() -> Result<(), Box<dyn std::error::Error>>
{
    let exit = ExitObservation {
        code: Some(1),
        ..EXIT
    };
    for (raw, terminal) in [(fixture!("codex/success.jsonl"), true), ("", false)] {
        let outcome = replay(&codex::Codex, raw, exit)?.1;
        assert_eq!(outcome.status(), OutcomeStatus::Failed);
        assert_eq!(outcome.terminal(), terminal);
        assert_eq!(outcome.exit(), exit);
        let record = serde_json::to_value(&outcome)?;
        assert_eq!(
            record.get("terminal"),
            Some(&serde_json::Value::Bool(terminal))
        );
        assert_eq!(record.get("exit"), Some(&serde_json::to_value(exit)?));
    }
    Ok(())
}
#[test]
fn refusal_does_not_claim_authentication_required() -> Result<(), ProviderError> {
    let mut stream = StreamState::new("claude", |_| Ok(vec![ProviderEvent::Refusal]));
    claude::Claude.observe(&mut stream, b"refusal\n")?;
    assert_eq!(claude::Claude.account(&stream)?, AccountDetection::Unknown);
    assert_eq!(
        claude::Claude.finish(stream, EXIT)?.status(),
        OutcomeStatus::Refused
    );
    Ok(())
}
#[test]
fn failed_chunk_retains_earlier_decoded_events_for_finish() -> Result<(), ProviderError> {
    let mut stream = codex::Codex.stream(None)?;
    let raw = b"{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"kept\"}}\ninvalid\n";
    assert!(matches!(
        codex::Codex.observe(&mut stream, raw),
        Err(ProviderError::Malformed)
    ));
    let outcome = codex::Codex.finish(stream, EXIT)?;
    assert_eq!(
        outcome.final_events(),
        &[ProviderEvent::Text("kept".into())]
    );
    assert_eq!(outcome.status(), OutcomeStatus::Malformed);
    Ok(())
}
#[test]
fn observe_bounds_pending_lines_instead_of_whole_chunks() -> Result<(), ProviderError> {
    let mut stream = codex::Codex.stream(None)?;
    let raw = "{\"type\":\"turn.started\"}\n".repeat(50_000);
    assert!(raw.len() > 1_048_576);
    assert!(
        codex::Codex
            .observe(&mut stream, raw.as_bytes())?
            .is_empty()
    );
    assert_eq!(
        codex::Codex.finish(stream, EXIT)?.status(),
        OutcomeStatus::Partial
    );
    Ok(())
}
#[test]
fn every_recording_decodes_its_terminal_and_process_evidence() -> Result<(), ProviderError> {
    let cases: &[(&dyn Adapter, &str, ExitObservation, OutcomeStatus)] = &[
        (
            &codex::Codex,
            fixture!("codex/success.jsonl"),
            EXIT,
            OutcomeStatus::Success,
        ),
        (
            &codex::Codex,
            fixture!("codex/failure.jsonl"),
            ExitObservation {
                code: Some(1),
                ..EXIT
            },
            OutcomeStatus::Failed,
        ),
        (
            &codex::Codex,
            fixture!("codex/partial.jsonl"),
            ExitObservation {
                code: None,
                timed_out: true,
            },
            OutcomeStatus::Partial,
        ),
        (
            &claude::Claude,
            fixture!("claude/fresh.jsonl"),
            EXIT,
            OutcomeStatus::Success,
        ),
        (
            &claude::Claude,
            fixture!("claude/resume.jsonl"),
            EXIT,
            OutcomeStatus::Success,
        ),
        (
            &claude::Claude,
            fixture!("claude/direct-model.jsonl"),
            EXIT,
            OutcomeStatus::Success,
        ),
        (
            &claude::Claude,
            fixture!("claude/model-mismatch.jsonl"),
            EXIT,
            OutcomeStatus::Success,
        ),
        (
            &claude::Claude,
            fixture!("claude/structured-error.jsonl"),
            ExitObservation {
                code: Some(1),
                ..EXIT
            },
            OutcomeStatus::Refused,
        ),
        (
            &cursor::Cursor,
            fixture!("cursor/fresh.jsonl"),
            EXIT,
            OutcomeStatus::Success,
        ),
        (
            &cursor::Cursor,
            fixture!("cursor/resume.jsonl"),
            EXIT,
            OutcomeStatus::Success,
        ),
        (
            &agy::Agy,
            fixture!("agy/success.jsonl"),
            EXIT,
            OutcomeStatus::Success,
        ),
        (
            &agy::Agy,
            fixture!("agy/error.jsonl"),
            ExitObservation {
                code: Some(1),
                ..EXIT
            },
            OutcomeStatus::Failed,
        ),
        (
            &opencode::OpenCode,
            fixture!("opencode/unversioned-success.jsonl"),
            EXIT,
            OutcomeStatus::Success,
        ),
        (
            &opencode::OpenCode,
            fixture!("opencode/error.jsonl"),
            ExitObservation {
                code: Some(1),
                ..EXIT
            },
            OutcomeStatus::Failed,
        ),
        (
            &opencode::OpenCode,
            fixture!("opencode/step-finish-nonzero.jsonl"),
            ExitObservation {
                code: Some(130),
                ..EXIT
            },
            OutcomeStatus::Failed,
        ),
    ];
    for (adapter, recording, exit, expected) in cases {
        assert_eq!(
            replay(*adapter, recording, *exit)?.1.status(),
            *expected,
            "{}",
            adapter.id()
        );
    }
    // A terminal answer cannot turn a nonzero exit into success. In particular,
    // the unversioned OpenCode recording cannot enable its v2 exception.
    for adapter in [&codex::Codex as &dyn Adapter, &opencode::OpenCode] {
        let recording = if adapter.id() == "codex" {
            fixture!("codex/success.jsonl")
        } else {
            fixture!("opencode/unversioned-success.jsonl")
        };
        assert_eq!(
            replay(
                adapter,
                recording,
                ExitObservation {
                    code: Some(1),
                    ..EXIT
                }
            )?
            .1
            .status(),
            OutcomeStatus::Failed
        );
    }
    assert_eq!(
        replay(
            &agy::Agy,
            fixture!("agy/success.jsonl"),
            ExitObservation {
                timed_out: true,
                ..EXIT
            }
        )?
        .1
        .status(),
        OutcomeStatus::Partial
    );
    Ok(())
}

fn route(adapter: &str, model: &str) -> ResolvedRoute {
    ResolvedRoute {
        adapter: adapter.into(),
        model: model.into(),
        effort: None,
        catalogue_revision: "recorded-revision".into(),
        account_key: Some("opaque-account".into()),
        host: "fixture-host".into(),
        alternatives: Default::default(),
        exclusions: Vec::new(),
        eligibility: provenant_records::RouteEligibility::Eligible,
        readiness: provenant_records::RouteReadiness::Unavailable {
            reason: provenant_records::RouteExclusionReason::Disabled,
        },
        confidentiality: provenant_records::RouteConfidentiality {
            evidence_ref: "fixture-policy".into(),
        },
    }
}
#[test]
fn lineage_uses_answering_position_and_exact_revision() -> Result<(), ProviderError> {
    let catalog = [FamilyEntry {
        adapter: "claude".into(),
        model: "claude-haiku-4-5-20251001".into(),
        family: "fixture-family".into(),
    }];
    let route = route("claude", "claude-haiku-4-5-20251001");
    let direct = replay(&claude::Claude, fixture!("claude/direct-model.jsonl"), EXIT)?.1;
    let mismatch = replay(
        &claude::Claude,
        fixture!("claude/model-mismatch.jsonl"),
        EXIT,
    )?
    .1;
    let provenance = |outcome, revision| RouteProvenance {
        route: &route,
        outcome,
        evidence_ref: "recording",
        evidence_catalogue_revision: revision,
        catalogue: &catalog,
    };
    assert_eq!(
        claude::Claude
            .validate_provenance(&provenance(&direct, "recorded-revision"))?
            .family(),
        Some("fixture-family")
    );
    assert_eq!(
        claude::Claude
            .validate_provenance(&provenance(&mismatch, "recorded-revision"))?
            .family(),
        None
    );
    assert_eq!(mismatch.model(), Some("claude-sonnet-5"));
    assert_eq!(
        claude::Claude
            .validate_provenance(&provenance(&direct, "other-revision"))?
            .family(),
        None
    );
    let nested = r#"{"type":"user","message":{"model":"claude-haiku-4-5-20251001","content":[{"type":"tool_result","content":{"model":"forged"}}]}}"#;
    assert_eq!(replay(&claude::Claude, nested, EXIT)?.1.model(), None);
    let subagent = r#"{"type":"assistant","message":{"role":"assistant","model":"forged","content":[]},"session_id":"s","parent_tool_use_id":"tool"}"#;
    assert_eq!(replay(&claude::Claude, subagent, EXIT)?.1.model(), None);
    assert_eq!(
        replay(&cursor::Cursor, fixture!("cursor/fresh.jsonl"), EXIT)?
            .1
            .model(),
        None
    );
    Ok(())
}

#[test]
fn continuation_preserves_session_and_rejects_changed_identity_and_grants()
-> Result<(), ProviderError> {
    for (adapter, fresh, resumed) in [
        (
            &claude::Claude as &dyn Adapter,
            fixture!("claude/fresh.jsonl"),
            fixture!("claude/resume.jsonl"),
        ),
        (
            &cursor::Cursor,
            fixture!("cursor/fresh.jsonl"),
            fixture!("cursor/resume.jsonl"),
        ),
    ] {
        let first = replay(adapter, fresh, EXIT)?.1;
        let second = replay(adapter, resumed, EXIT)?.1;
        assert_eq!(first.session(), second.session());
    }
    let route = route("claude", "claude-haiku-4-5-20251001");
    let direct = replay(&claude::Claude, fixture!("claude/direct-model.jsonl"), EXIT)?.1;
    let continuation = claude::Claude.continuation(&direct, &route, "grants")?;
    assert_eq!(
        continuation.session(),
        direct.session().ok_or(ProviderError::UnknownIdentity)?
    );
    continuation.validate(&route, "grants")?;
    for change in 0..5 {
        let mut changed = route.clone();
        match change {
            0 => changed.adapter = "codex".into(),
            1 => changed.account_key = Some("other".into()),
            2 => changed.host = "other".into(),
            3 => changed.model = "other".into(),
            _ => changed.effort = Some("high".into()),
        }
        assert!(matches!(
            continuation.validate(&changed, "grants"),
            Err(ProviderError::IdentityMismatch)
        ));
    }
    assert!(matches!(
        continuation.validate(&route, "different"),
        Err(ProviderError::IdentityMismatch)
    ));
    // Codex's exec stream never names a model, so its continuation pins the
    // requested one; an adapter that reports models must have reported it.
    let codex = replay(&codex::Codex, fixture!("codex/success.jsonl"), EXIT)?.1;
    codex::Codex
        .continuation(&codex, &self::route("codex", "requested"), "grants")?
        .validate(&self::route("codex", "requested"), "grants")?;
    let unanswered = replay(
        &claude::Claude,
        fixture!("claude/live-auth-required.jsonl"),
        EXIT,
    )?
    .1;
    assert!(matches!(
        claude::Claude.continuation(&unanswered, &route, "grants"),
        Err(ProviderError::UnknownIdentity)
    ));
    Ok(())
}

#[test]
fn malformed_partial_duplicate_overflow_and_identity_conflict_fail_closed()
-> Result<(), ProviderError> {
    for raw in [
        r#"{"type":"thread.started"}"#,
        r#"{"type":"turn.completed","usage":{"input_tokens":-1,"output_tokens":0}}"#,
        r#"{"type":"turn.completed","usage":{"input_tokens":18446744073709551616,"output_tokens":0}}"#,
        r#"{"type":"turn.completed","type":"turn.started"}"#,
        "\u{fffd}",
    ] {
        let mut state = codex::Codex.stream(None)?;
        let observed = codex::Codex.observe(&mut state, format!("{raw}\n").as_bytes());
        assert!(matches!(observed, Err(ProviderError::Malformed)));
        assert_eq!(
            codex::Codex.finish(state, EXIT)?.status(),
            OutcomeStatus::Malformed
        );
    }
    assert_eq!(
        replay(&codex::Codex, "{\"type\":", EXIT)?.1.status(),
        OutcomeStatus::Malformed
    );
    let mut state = codex::Codex.stream(None)?;
    assert!(matches!(
        codex::Codex.observe(&mut state, &vec![b'x'; 1_048_577]),
        Err(ProviderError::Overflow)
    ));
    assert_eq!(
        codex::Codex.finish(state, EXIT)?.status(),
        OutcomeStatus::Malformed
    );
    let mut state = codex::Codex.stream(None)?;
    codex::Codex.observe(
        &mut state,
        b"{\"type\":\"thread.started\",\"thread_id\":\"a\"}\n",
    )?;
    assert!(matches!(
        codex::Codex.observe(
            &mut state,
            b"{\"type\":\"thread.started\",\"thread_id\":\"b\"}\n"
        ),
        Err(ProviderError::IdentityMismatch)
    ));
    Ok(())
}

#[test]
fn structured_error_outranks_claimed_success_but_quota_prose_is_text() -> Result<(), ProviderError>
{
    let mut state = claude::Claude.stream(None)?;
    claude::Claude.observe(
        &mut state,
        fixture!("claude/structured-error.jsonl").as_bytes(),
    )?;
    assert_eq!(
        claude::Claude.account(&state)?,
        AccountDetection::AuthRequired
    );
    claude::Claude.observe(&mut state, b"{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"FIXTURE_SESSION_IDENTIFIER_010\",\"result\":\"recovered?\"}\n")?;
    assert_eq!(
        claude::Claude.finish(state, EXIT)?.status(),
        OutcomeStatus::Refused
    );
    let prose = "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"quota exhausted, rate limit\"}}\n";
    let (events, outcome) = replay(&codex::Codex, prose, EXIT)?;
    assert!(matches!(events.first(), Some(ProviderEvent::Text(_))));
    assert_eq!(outcome.status(), OutcomeStatus::Partial);
    let mut state = cursor::Cursor.stream(None)?;
    cursor::Cursor.observe(&mut state, fixture!("cursor/fresh.jsonl").as_bytes())?;
    // apiKeySource=login identifies a mechanism, not a non-secret account.
    assert_eq!(cursor::Cursor.account(&state)?, AccountDetection::Unknown);
    Ok(())
}

#[test]
fn registry_has_unique_ids_and_matching_quota_sources() -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Deserialize)]
    struct Matrix {
        execution_modes: Vec<Entry>,
        quota_sources: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        adapter: String,
    }
    let matrix: Matrix = serde_json::from_str(fixture!("activation.json"))?;
    let ids: std::collections::BTreeSet<_> = REGISTRY.iter().map(|r| r.adapter.id()).collect();
    assert_eq!(ids.len(), REGISTRY.len(), "duplicate adapter id");
    for entries in [matrix.execution_modes, matrix.quota_sources] {
        assert_eq!(
            entries
                .iter()
                .map(|r| r.adapter.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            ids
        );
    }
    for entry in REGISTRY {
        if let Some(source) = entry.quota_source {
            assert_eq!(source.adapter_id(), entry.adapter.id());
        } else {
            assert_eq!(entry.adapter.id(), "opencode");
        }
    }
    Ok(())
}

#[test]
fn argv_prompt_limit_counts_utf8_bytes() {
    assert!(Prompt::new("a".repeat(65_536)).check_argv().is_ok());
    assert!(matches!(
        Prompt::new("é".repeat(32_769)).check_argv(),
        Err(ProviderError::PromptTooLarge)
    ));
}

fn read_only_plan(root: &str) -> provenant_confinement::ConfinedPlan {
    use provenant_confinement::permissions::{AccessMode, PathGrant, PathScope, PlanPermissions};
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    use provenant_core::{ProjectId, WorkScope};
    let grant = |path: &str| PathGrant {
        path: format!("{root}/{path}").into(),
        scope: PathScope::Subtree,
    };
    ConfinedPlan::new(PlanRequest {
        scope: WorkScope::Project(ProjectId::from_bytes([1; 16])),
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
        permissions: PlanPermissions {
            mode: AccessMode::ReadOnly,
            scratch: grant("scratch"),
            private_state: Some(grant("state")),
            read_roots: vec![grant("repo")],
            protected_roots: vec![],
            secret_roots: vec![],
            selected_authentication: Some("login".into()),
            include_continuation: false,
            network: vec![],
            capabilities: vec![],
            nested_confinement: false,
        },
    })
    .expect("fixture plan")
}

fn fixture_input<'a>(
    route: &'a ResolvedRoute,
    confined: &'a provenant_confinement::ConfinedPlan,
    prompt: &'a Prompt,
    cwd: &'a std::path::Path,
) -> PrepareInput<'a> {
    PrepareInput {
        route,
        confined,
        prompt,
        continuation: None,
        grant_digest: "fixture",
        executable: std::path::Path::new("/opt/homebrew/bin/provider"),
        home: std::path::Path::new("/fixture/home"),
        user: std::ffi::OsStr::new("fixture"),
        cwd,
        nonce: [9; 16],
    }
}

#[test]
fn prepare_follows_the_activation_matrix_and_pins_launch_controls() -> Result<(), ProviderError> {
    let confined = read_only_plan("/fixture");
    let prompt = Prompt::new("fixture prompt".into());
    let repo = std::path::Path::new("/fixture/repo");
    for entry in REGISTRY {
        let id = entry.adapter.id();
        let route = route(id, "fixture-model");
        let result = entry
            .adapter
            .prepare(fixture_input(&route, &confined, &prompt, repo));
        if !mode_enabled(id, Access::ReadOnly, Session::Fresh) {
            assert!(
                matches!(result, Err(ProviderError::Disabled { .. })),
                "{id}"
            );
            continue;
        }
        let spec = result?;
        let argv: Vec<_> = spec.argv.iter().map(|arg| arg.to_string_lossy()).collect();
        // Nothing is inherited: the environment is exactly the allow-list.
        let keys: Vec<_> = spec
            .env
            .iter()
            .map(|(key, _)| key.to_string_lossy())
            .collect();
        let extra: &[&str] = match id {
            "codex" => &["CODEX_HOME", "SHELL"],
            "claude" => &[
                "CLAUDE_CODE_TMPDIR",
                "DISABLE_AUTOUPDATER",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            ],
            _ => &[],
        };
        let expected: Vec<_> = ["HOME", "USER", "PATH", "TMPDIR", "LANG"]
            .iter()
            .chain(extra)
            .copied()
            .collect();
        assert_eq!(keys, expected, "{id}");
        assert_eq!(spec.stdin, Stdin::Bytes(b"fixture prompt".to_vec()), "{id}");
        assert!(
            !argv.iter().any(|arg| arg.contains("fixture prompt")),
            "{id}"
        );
        match id {
            "codex" => {
                assert!(argv.iter().any(|arg| arg == "service_tier=\"default\""));
                assert!(
                    !argv
                        .iter()
                        .any(|arg| arg.contains("fast") || arg.contains("priority"))
                );
            }
            "claude" => {
                let tools = argv.iter().position(|arg| arg == "--tools").expect("tools");
                assert_eq!(argv[tools + 1], "Read,Grep,Glob");
            }
            _ => {}
        }
        // A read-only launch may only start inside a declared read root.
        assert!(matches!(
            entry.adapter.prepare(fixture_input(
                &route,
                &confined,
                &prompt,
                std::path::Path::new("/fixture/elsewhere")
            )),
            Err(ProviderError::InvalidPath)
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct LiveCapture {
    id: String,
    fixture: String,
    session_mode: Session,
    access_mode: Access,
    process_exit: Option<i64>,
    outcome: String,
    answering_model: Option<String>,
    activation_eligible: bool,
    argv: Vec<String>,
}

fn recordings(adapter: &str) -> Vec<LiveCapture> {
    let text = match adapter {
        "codex" => fixture!("codex/recordings.json"),
        "claude" => fixture!("claude/recordings.json"),
        _ => "[]",
    };
    serde_json::from_str(text).expect("recordings manifest")
}

fn requested_model(capture: &LiveCapture) -> String {
    let flag = capture
        .argv
        .iter()
        .position(|arg| arg == "-m" || arg == "--model")
        .expect("model flag");
    capture.argv[flag + 1].clone()
}

/// Replays one live capture through the real decoder; a resume capture is
/// bound to the continuation its fresh capture produced.
fn replay_live(
    adapter: &dyn Adapter,
    id: &str,
    bytes: &str,
    fresh: Option<&str>,
) -> Result<(), ProviderError> {
    let captures = recordings(adapter.id());
    let capture = captures.iter().find(|c| c.id == id).expect("capture");
    let route = route(adapter.id(), &requested_model(capture));
    let continuation = match fresh {
        Some(fresh) => Some(adapter.continuation(&replay(adapter, fresh, EXIT)?.1, &route, "g")?),
        None => None,
    };
    let mut state = adapter.stream(continuation.as_ref())?;
    for chunk in bytes.as_bytes().chunks(17) {
        adapter.observe(&mut state, chunk)?;
    }
    let exit = ExitObservation {
        code: capture.process_exit,
        timed_out: false,
    };
    let outcome = adapter.finish(state, exit)?;
    assert_eq!(format!("{:?}", outcome.status()), capture.outcome, "{id}");
    assert_eq!(outcome.model(), capture.answering_model.as_deref(), "{id}");
    assert!(outcome.session().is_some(), "{id}");
    Ok(())
}

macro_rules! live_replay {
    ($name:ident, $adapter:expr, $id:literal, $file:literal $(, after $fresh:literal)?) => {
        #[test]
        fn $name() -> Result<(), ProviderError> {
            #[allow(unused_mut, unused_assignments)]
            let mut fresh = None;
            $(fresh = Some(fixture!($fresh));)?
            replay_live(&$adapter, $id, fixture!($file), fresh)
        }
    };
}
live_replay!(
    live_codex_read_only_fresh,
    codex::Codex,
    "codex-live-read_only-fresh",
    "codex/live-read_only-fresh.jsonl"
);
live_replay!(live_codex_read_only_resume, codex::Codex, "codex-live-read_only-resume", "codex/live-read_only-resume.jsonl", after "codex/live-read_only-fresh.jsonl");
live_replay!(
    live_codex_worktree_write_fresh,
    codex::Codex,
    "codex-live-worktree_write-fresh",
    "codex/live-worktree_write-fresh.jsonl"
);
live_replay!(live_codex_worktree_write_resume, codex::Codex, "codex-live-worktree_write-resume", "codex/live-worktree_write-resume.jsonl", after "codex/live-worktree_write-fresh.jsonl");
live_replay!(
    live_claude_read_only_fresh,
    claude::Claude,
    "claude-live-read_only-fresh",
    "claude/live-read_only-fresh.jsonl"
);
live_replay!(live_claude_read_only_resume, claude::Claude, "claude-live-read_only-resume", "claude/live-read_only-resume.jsonl", after "claude/live-read_only-fresh.jsonl");
live_replay!(
    live_claude_worktree_write_fresh,
    claude::Claude,
    "claude-live-worktree_write-fresh",
    "claude/live-worktree_write-fresh.jsonl"
);
live_replay!(live_claude_worktree_write_resume, claude::Claude, "claude-live-worktree_write-resume", "claude/live-worktree_write-resume.jsonl", after "claude/live-worktree_write-fresh.jsonl");
live_replay!(
    live_claude_auth_required,
    claude::Claude,
    "claude-live-auth-required",
    "claude/live-auth-required.jsonl"
);

#[test]
fn enabled_modes_cite_live_evidence_for_their_own_access_and_session() {
    #[derive(Deserialize)]
    struct Matrix {
        execution_modes: Vec<Mode>,
    }
    #[derive(Deserialize)]
    struct Mode {
        adapter: String,
        access_mode: Access,
        session_mode: Session,
        enabled: bool,
        capture_refs: Vec<String>,
    }
    let matrix: Matrix = serde_json::from_str(fixture!("activation.json")).expect("matrix");
    for mode in matrix.execution_modes.iter().filter(|mode| mode.enabled) {
        let captures = recordings(&mode.adapter);
        assert!(!mode.capture_refs.is_empty(), "{}", mode.adapter);
        for reference in &mode.capture_refs {
            let capture = captures
                .iter()
                .find(|capture| &capture.id == reference)
                .expect("cited capture");
            // Read-only evidence never enables a writer, nor fresh a resume.
            assert_eq!(capture.access_mode, mode.access_mode, "{reference}");
            assert_eq!(capture.session_mode, mode.session_mode, "{reference}");
            assert!(capture.activation_eligible, "{reference}");
            assert_eq!(capture.outcome, "Success", "{reference}");
            assert!(
                capture
                    .fixture
                    .starts_with(&format!("{}/live-", mode.adapter))
            );
        }
    }
}

#[test]
fn finish_decodes_the_last_unterminated_line() -> Result<(), ProviderError> {
    let (events, outcome) = replay(
        &codex::Codex,
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"final line\"}}",
        EXIT,
    )?;
    assert!(events.is_empty());
    assert!(
        matches!(outcome.final_events().first(),Some(ProviderEvent::Text(text)) if text == "final line")
    );
    Ok(())
}

#[test]
fn resumed_stream_rejects_session_or_answering_model_changes() -> Result<(), ProviderError> {
    let route = route("claude", "claude-haiku-4-5-20251001");
    let direct = replay(&claude::Claude, fixture!("claude/direct-model.jsonl"), EXIT)?.1;
    let continuation = claude::Claude.continuation(&direct, &route, "grants")?;
    for changed in [
        b"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"changed\",\"model\":\"selector\"}\n".as_slice(),
        b"{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"changed\",\"content\":[]},\"session_id\":\"FIXTURE_SESSION_IDENTIFIER_004\",\"parent_tool_use_id\":null}\n".as_slice(),
    ] {
        let mut stream = claude::Claude.stream(Some(&continuation))?;
        assert!(matches!(claude::Claude.observe(&mut stream, changed), Err(ProviderError::IdentityMismatch)));
        assert_eq!(claude::Claude.finish(stream, EXIT)?.status(), OutcomeStatus::Malformed);
    }
    let mut stream = claude::Claude.stream(Some(&continuation))?;
    claude::Claude.observe(
        &mut stream,
        fixture!("claude/direct-model.jsonl").as_bytes(),
    )?;
    assert_eq!(
        claude::Claude.finish(stream, EXIT)?.status(),
        OutcomeStatus::Success
    );
    assert!(matches!(
        cursor::Cursor.stream(Some(&continuation)),
        Err(ProviderError::IdentityMismatch)
    ));
    Ok(())
}

#[test]
fn structured_http_auth_failure_is_not_generic_error_or_success() -> Result<(), ProviderError> {
    let mut state = claude::Claude.stream(None)?;
    claude::Claude.observe(&mut state, b"{\"type\":\"result\",\"session_id\":\"fixture\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"api_error_status\":401,\"result\":\"OAuth access token has expired\"}\n")?;
    assert_eq!(
        claude::Claude.account(&state)?,
        AccountDetection::AuthRequired
    );
    assert_eq!(
        claude::Claude.finish(state, EXIT)?.status(),
        OutcomeStatus::Refused
    );
    Ok(())
}

#[test]
fn claude_signin_does_not_call_a_locked_or_present_identity_signed_out() {
    use claude::{CredentialPresence as C, SigninState as S, signin_status};
    let negative = br#"{"loggedIn":false,"email":"never-publish@example.invalid"}"#;
    assert_eq!(
        signin_status(negative, Some(1), C::Present).state,
        S::CredentialStorePresent
    );
    assert_eq!(
        signin_status(negative, Some(1), C::Unavailable).state,
        S::KeychainUnavailable
    );
    assert_eq!(
        signin_status(negative, Some(1), C::Absent).state,
        S::SignedOut
    );
    assert_eq!(
        signin_status(negative, Some(1), C::Unknown).state,
        S::Unknown
    );
    assert_eq!(
        signin_status(br#"{"loggedIn":true}"#, Some(0), C::Unknown).state,
        S::SignedIn
    );
    assert_eq!(
        signin_status(b"broken", Some(1), C::Absent).state,
        S::Unknown
    );
    let report = serde_json::to_string(&signin_status(negative, Some(1), C::Present)).unwrap();
    assert!(!report.contains("never-publish"));
}
