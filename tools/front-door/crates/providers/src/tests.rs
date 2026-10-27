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
fn codex_retry_exhausted_429_is_rate_limited() -> Result<(), ProviderError> {
    for message in [
        "exceeded retry limit, last status: 429 Too Many Requests",
        "exceeded retry limit, last status: 429",
        "TOO MANY REQUESTS",
    ] {
        let raw = format!(
            "{{\"type\":\"error\",\"message\":\"{message}\"}}\n\
             {{\"type\":\"turn.failed\",\"error\":{{\"message\":\"{message}\"}}}}\n"
        );
        let (events, outcome) = replay(&codex::Codex, &raw, EXIT)?;
        assert_eq!(outcome.status(), OutcomeStatus::Refused);
        assert!(events.contains(&ProviderEvent::RateLimited {
            message: message.into(),
        }));
    }
    let recovered = format!(
        "{{\"type\":\"error\",\"message\":\"Reconnecting… 1/5 (429 Too Many Requests)\"}}\n{}",
        fixture!("codex/success.jsonl")
    );
    let (events, outcome) = replay(&codex::Codex, &recovered, EXIT)?;
    assert_eq!(outcome.status(), OutcomeStatus::Success);
    assert!(events.contains(&ProviderEvent::TransientError));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderEvent::RateLimited { .. }))
    );
    for message in [
        "invalid request",
        "request id: 429",
        "https://example.test/429",
    ] {
        let raw = format!("{{\"type\":\"error\",\"message\":\"{message}\"}}\n");
        let (events, outcome) = replay(&codex::Codex, &raw, EXIT)?;
        assert_eq!(outcome.status(), OutcomeStatus::Failed);
        assert_eq!(events, vec![ProviderEvent::Error]);
    }
    Ok(())
}
#[test]
fn agy_stream_denial_is_failure() -> Result<(), ProviderError> {
    let (events, outcome) = replay(&agy::Agy, fixture!("agy/denied-result.jsonl"), EXIT)?;
    assert_eq!(outcome.status(), OutcomeStatus::Failed);
    assert!(events.contains(&ProviderEvent::Denied {
        action: "RunCommand".into(),
    }));
    let unsafe_name = fixture!("agy/denied-result.jsonl").replace("RunCommand", "bad\\nname");
    let (events, outcome) = replay(&agy::Agy, &unsafe_name, EXIT)?;
    assert_eq!(outcome.status(), OutcomeStatus::Failed);
    assert!(events.contains(&ProviderEvent::Denied {
        action: "unknown action".into(),
    }));
    Ok(())
}
#[test]
fn agy_recovered_denial_is_non_fatal() -> Result<(), ProviderError> {
    let raw = fixture!("agy/denied-result.jsonl")
        .replace("\"response\":\"\"", "\"response\":\"Recovered answer\"");
    let (events, outcome) = replay(&agy::Agy, &raw, EXIT)?;
    assert_eq!(outcome.status(), OutcomeStatus::Success);
    assert!(events.contains(&ProviderEvent::Text("Recovered answer".into())));
    assert!(events.contains(&ProviderEvent::DeniedNote {
        action: "RunCommand".into()
    }));
    Ok(())
}
#[test]
fn agy_denials_accept_unexpected_shapes() -> Result<(), ProviderError> {
    for (denials, expected) in [
        (serde_json::json!(null), None),
        (serde_json::json!({"display_name": "RunCommand"}), None),
        (serde_json::json!([42]), Some("unknown action")),
    ] {
        let raw = serde_json::json!({"event":"result","result":{
            "conversation_id":"", "status":"SUCCESS", "response":"",
            "denied_actions":denials, "usage":{"input_tokens":0,"output_tokens":0}
        }})
        .to_string();
        let (events, outcome) = replay(&agy::Agy, &format!("{raw}\n"), EXIT)?;
        if let Some(action) = expected {
            assert_eq!(outcome.status(), OutcomeStatus::Failed);
            assert!(events.contains(&ProviderEvent::Denied {
                action: action.into()
            }));
        } else {
            assert_eq!(outcome.status(), OutcomeStatus::Success);
        }
    }
    Ok(())
}
#[test]
fn agy_denied_command_preserves_failed_response() -> Result<(), ProviderError> {
    // Synthetic stdout paired with the issue's recorded stderr diagnostic.
    let raw = serde_json::json!({"event":"result","result":{
        "conversation_id":"", "status":"ERROR", "response":"I could only verify part of the claim.",
        "error":"command permission denied", "usage":{"input_tokens":0,"output_tokens":0}
    }})
    .to_string();
    let (mut events, outcome) = replay(
        &agy::Agy,
        &raw,
        ExitObservation {
            code: Some(1),
            ..EXIT
        },
    )?;
    events.extend(outcome.final_events().iter().cloned());
    assert_eq!(outcome.status(), OutcomeStatus::Failed);
    assert!(events.contains(&ProviderEvent::Text(
        "I could only verify part of the claim.".into()
    )));
    assert_eq!(
        agy::Agy.failure_diagnostic(include_bytes!(
            "../../../tests/fixtures/providers/agy/command-denied.stderr"
        )),
        Some(r#"agy denied the "command" permission in headless mode"#.into())
    );
    for unrelated in [
        "a tool required the command permission",
        "jetski: unrelated failure",
        "jetski: no output produced \u{2014} a tool required the \"command\nforged\" permission that headless mode cannot prompt for, so it was auto-denied.",
        "jetski: no output produced \u{2014} a tool required the \"\" permission that headless mode cannot prompt for, so it was auto-denied.",
    ] {
        assert_eq!(agy::Agy.failure_diagnostic(unrelated.as_bytes()), None);
    }
    assert_eq!(
        codex::Codex.failure_diagnostic(include_bytes!(
            "../../../tests/fixtures/providers/agy/command-denied.stderr"
        )),
        None
    );
    Ok(())
}
#[test]
fn agy_individual_quota_is_quota_limited_until_its_stated_reset() -> Result<(), ProviderError> {
    let exit = ExitObservation {
        code: Some(1),
        ..EXIT
    };
    let result = |error: &str| {
        serde_json::json!({"event":"result","result":{
            "conversation_id":"","status":"ERROR","response":"","error":error,
            "usage":{"input_tokens":0,"output_tokens":0}}})
        .to_string()
            + "\n"
    };
    for (message, resets_in_seconds) in [
        (
            "429: Individual quota reached for gemini-3.8-flash. Resets in 7h23m.",
            Some(26_580),
        ),
        (
            "Individual quota reached. Resets in 2d 5m 30s",
            Some(173_130),
        ),
        ("Individual quota reached. Resets in 8d", None),
        ("Individual quota reached. Resets in 0m", None),
        ("INDIVIDUAL QUOTA REACHED", None),
        // The wait comes only from the closing suffix.
        ("Individual quota reached (Resets in 7d) for now", None),
        ("Individual quota reached. Resets in 7h23m, try later", None),
    ] {
        let (events, outcome) = replay(&agy::Agy, &result(message), exit)?;
        assert_eq!(outcome.status(), OutcomeStatus::Refused, "{message}");
        assert!(
            events.contains(&ProviderEvent::QuotaLimited {
                message: message.into(),
                pool: None,
                resets_in_seconds,
            }),
            "{message}"
        );
    }
    // A bare 429 may be a per-minute rate limit, not a spent quota, and only an
    // error that opens with the refusal is one.
    for message in [
        "429",
        "429 Too Many Requests",
        "RESOURCE_EXHAUSTED",
        "429 RESOURCE_EXHAUSTED: Quota exceeded for requests per minute",
        "upstream error",
        r#"Invalid prompt: "quota reached. Resets in 7d""#,
        r#"Invalid prompt: "Individual quota reached. Resets in 7d""#,
        "tool failed: Individual quota reached while reading notes, Resets in 7d",
        "4290: Individual quota reached. Resets in 7d",
    ] {
        let (events, outcome) = replay(&agy::Agy, &result(message), exit)?;
        assert_eq!(outcome.status(), OutcomeStatus::Failed, "{message}");
        assert!(events.contains(&ProviderEvent::Error), "{message}");
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

pub(super) fn route(adapter: &str, model: &str) -> ResolvedRoute {
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
        pool_selection: None,
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
fn codex_oversized_command_output_is_skipped_but_other_oversize_fails_closed()
-> Result<(), ProviderError> {
    // Shape of run 358cbf47 (#1514): a 1.17 MB command_execution item.
    let filler = "x".repeat(1_200_000);
    let big = format!(
        "{{\"type\":\"item.completed\",\"item\":{{\"id\":\"item_20\",\"type\":\"command_execution\",\"command\":\"/bin/zsh -lc 'cat src/main.rs'\",\"aggregated_output\":\"{filler}\",\"exit_code\":0,\"status\":\"completed\"}}}}\n"
    );
    let stream = format!(
        "{{\"type\":\"thread.started\",\"thread_id\":\"t1\"}}\n{big}{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":2}}}}\n"
    );
    let mut state = codex::Codex.stream(None)?;
    for chunk in stream.as_bytes().chunks(65_536) {
        codex::Codex.observe(&mut state, chunk)?;
    }
    assert_eq!(
        codex::Codex.finish(state, EXIT)?.status(),
        OutcomeStatus::Success
    );
    // An oversized agent_message still fails closed.
    let answer = format!(
        "{{\"type\":\"item.completed\",\"item\":{{\"id\":\"i\",\"type\":\"agent_message\",\"text\":\"{filler}\"}}}}\n"
    );
    let mut state = codex::Codex.stream(None)?;
    assert!(matches!(
        codex::Codex.observe(&mut state, answer.as_bytes()),
        Err(ProviderError::Overflow)
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
fn claude_answer_is_recorded_once() -> Result<(), ProviderError> {
    let (events, _) = replay(&claude::Claude, fixture!("claude/fresh.jsonl"), EXIT)?;
    let texts: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["FIXTURE_USER_OR_PROVIDER_TEXT_001"]);
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
    let ids: std::collections::BTreeSet<_> = REGISTRY.iter().map(|r| r.adapter.id()).collect();
    assert_eq!(ids.len(), REGISTRY.len(), "duplicate adapter id");
    assert_eq!(
        ids,
        provenant_records::ADAPTER_SPECS
            .iter()
            .map(|spec| spec.name)
            .collect()
    );
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

pub(super) fn read_only_plan(
    root: &str,
    authentication: Option<&str>,
) -> provenant_confinement::ConfinedPlan {
    use provenant_confinement::permissions::{
        AccessMode, NetworkAccess, PathGrant, PathScope, PlanPermissions,
    };
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
            read_only: None,
            mode: AccessMode::ReadOnly,
            scratch: grant("scratch"),
            private_state: Some(grant("state")),
            read_roots: vec![grant("repo")],
            protected_roots: vec![],
            secret_roots: vec![],
            selected_authentication: authentication.map(Into::into),
            include_continuation: false,
            write_roots: vec![],
            network: NetworkAccess::Provider,
            nested_confinement: false,
        },
    })
    .expect("fixture plan")
}

pub(super) fn fixture_access_plan(
    access: Access,
    authentication: Option<&str>,
    include_continuation: bool,
) -> provenant_confinement::ConfinedPlan {
    use provenant_confinement::permissions::{AccessMode, WorktreeWriteAuthority};
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let reader = read_only_plan("/fixture", authentication);
    let mut permissions = reader.permissions().clone();
    permissions.include_continuation = include_continuation;
    if access == Access::WorktreeWrite {
        let authority: WorktreeWriteAuthority = serde_json::from_value(serde_json::json!({
            "git_grants_digest": vec![0; 32],
            "cargo_cache": {"path": "/fixture/home/Library/Caches/provenant-lane-cargo/fixture", "scope": "subtree"}
        })).expect("fixture authority; no compilation is attempted");
        permissions.mode = AccessMode::WorktreeWrite(Box::new(authority));
    }
    ConfinedPlan::new(PlanRequest {
        scope: reader.scope(),
        permissions,
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
    .expect("fixture access plan")
}

const FIXTURE_TOKEN: &str = "fixture-access-token";

pub(super) fn fixture_token() -> crate::claude::OauthToken {
    let item = format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{FIXTURE_TOKEN}","refreshToken":"fixture-refresh","expiresAt":4000000000000}}}}"#
    );
    crate::claude::OauthToken::parse(item.as_bytes(), 1_000).expect("fixture token")
}

pub(super) fn fixture_input<'a>(
    route: &'a ResolvedRoute,
    confined: &'a provenant_confinement::ConfinedPlan,
    prompt: &'a Prompt,
    cwd: &'a std::path::Path,
    token: Option<&'a crate::claude::OauthToken>,
) -> PrepareInput<'a> {
    let executable = std::path::Path::new("/opt/homebrew/Cellar/provider/1.0/bin/provider");
    PrepareInput {
        route,
        confined,
        prompt,
        continuation: None,
        grant_digest: "fixture",
        executable,
        home: std::path::Path::new("/fixture/home"),
        user: std::ffi::OsStr::new("fixture"),
        host_var: &|_| None,
        account: None,
        token,
        cwd,
        git_context: false,
        nonce: [9; 16],
        provenant: crate::LaneRuntime {
            executable: std::path::Path::new("/fixture/provenant/bin/provenant"),
            git: std::path::Path::new("/fixture/git/bin/git"),
            state: std::path::Path::new("/fixture/state"),
            child_state: std::path::Path::new("/fixture/state/children/attempt"),
            home: std::path::Path::new("/fixture/home"),
            depth: 1,
            attempt: provenant_core::AttemptId::from_bytes([7; 16]),
            assets: std::path::Path::new("/fixture/provenant"),
        },
    }
}

#[test]
fn spilled_prompts_reach_argv_adapters_with_file_read_permissions() -> Result<(), ProviderError> {
    let file = Path::new("/fixture/attempt/prompt.md");
    let prompt = Prompt::from_file(file.to_owned());
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for entry in REGISTRY
            .iter()
            .filter(|entry| matches!(entry.adapter.id(), "cursor" | "agy"))
        {
            let id = entry.adapter.id();
            let authentication = match id {
                "cursor" => Some("login"),
                "agy" => Some("keychain"),
                _ => None,
            };
            let plan = fixture_access_plan(access, authentication, false);
            let mut route = route(id, "fixture-model");
            route.account_key = None;
            let spec = entry.adapter.prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                None,
            ))?;
            assert_eq!(spec.argv.last(), Some(&OsString::from(prompt.text())));
            if id == "agy" {
                assert!(
                    spec.argv
                        .windows(2)
                        .any(|pair| { pair[0] == "--add-dir" && pair[1] == "/fixture/attempt" })
                );
            }
            if id == "agy" && access == Access::ReadOnly {
                assert!(
                    spec.argv
                        .iter()
                        .any(|arg| arg == "--dangerously-skip-permissions")
                );
            }
        }
    }
    Ok(())
}

#[test]
fn stdin_adapters_receive_oversized_prompts_in_full() -> Result<(), ProviderError> {
    let prompt = Prompt::new(format!(
        "Durable output instructions\n\n{}",
        "é".repeat(75_000)
    ));
    let token = fixture_token();
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for entry in REGISTRY
            .iter()
            .filter(|entry| matches!(entry.adapter.id(), "claude" | "codex" | "opencode"))
        {
            let id = entry.adapter.id();
            let plan = fixture_access_plan(access, (id == "codex").then_some("login"), false);
            let mut route = route(id, "fixture-model");
            route.account_key = None;
            let spec = entry.adapter.prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                (id == "claude").then_some(&token),
            ))?;
            assert_eq!(spec.stdin, Stdin::Bytes(prompt.text().as_bytes().to_vec()));
            assert!(!spec.argv.iter().any(|arg| arg == "--add-dir"));
        }
    }
    Ok(())
}

#[test]
fn temporary_environment_follows_the_plan_for_readers_and_writers() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let token = fixture_token();
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for entry in REGISTRY {
            let id = entry.adapter.id();
            let authentication = match id {
                "codex" | "cursor" => Some("login"),
                "agy" => Some("keychain"),
                _ => None,
            };
            let plan = fixture_access_plan(access, authentication, false);
            let mut route = route(id, "fixture-model");
            route.account_key = None;
            let spec = entry.adapter.prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                (id == "claude").then_some(&token),
            ))?;
            for key in
                std::iter::once("TMPDIR").chain((id == "claude").then_some("CLAUDE_CODE_TMPDIR"))
            {
                assert_eq!(
                    spec.env
                        .iter()
                        .find(|(name, _)| name == key)
                        .map(|(_, value)| value),
                    Some(&plan.permissions().scratch.path.as_os_str().to_owned()),
                    "{id} {access:?} {key}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn claude_read_only_adds_external_git_dir_only() {
    let common_dir = Path::new("/fixture/repo/.git");
    let mut argv = Vec::new();
    claude::add_git_dirs(
        &mut argv,
        [common_dir],
        Path::new("/fixture/repo/.worktrees/x"),
    );
    assert_eq!(
        argv,
        [
            OsString::from("--add-dir"),
            common_dir.as_os_str().to_owned()
        ]
    );

    argv.clear();
    claude::add_git_dirs(&mut argv, [common_dir], Path::new("/fixture/repo"));
    assert!(argv.is_empty());
}

#[test]
fn claude_lanes_disable_commit_and_pr_attribution() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("claude", "fixture-model");
    let account = crate::identity::RegisteredAccount::new(
        "claude",
        "opaque-account",
        "/fixture/home/.claude".into(),
    )?;
    let token = fixture_token().for_account(account.key(), account.directory());
    let continuation = Continuation::retained("claude", "fixture-session", &route, "fixture")?;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for resume in [false, true] {
            let plan = fixture_access_plan(access, None, resume);
            let mut input = fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                Some(&token),
            );
            input.account = Some(&account);
            input.continuation = resume.then_some(&continuation);
            let spec = claude::Claude.prepare(input)?;
            let settings_path = spec
                .argv
                .windows(2)
                .find(|args| args[0] == "--settings")
                .map(|args| Path::new(&args[1]))
                .expect("lane settings argument");
            let bytes = spec
                .setup
                .iter()
                .find_map(|step| match step {
                    SetupStep::WriteFile { path, bytes } if path == settings_path => Some(bytes),
                    _ => None,
                })
                .expect("lane settings file");
            let settings: serde_json::Value =
                serde_json::from_slice(bytes).expect("lane settings JSON");
            assert_eq!(settings["attribution"]["commit"], "");
            assert_eq!(settings["attribution"]["pr"], "");
        }
    }
    Ok(())
}

#[test]
fn prepare_all_adapters_and_pin_launch_controls() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let repo = std::path::Path::new("/fixture/repo");
    let token = fixture_token();
    for entry in REGISTRY {
        let id = entry.adapter.id();
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        // Codex and Agy read a credential inside the sandbox.
        let authentication = match id {
            "codex" | "cursor" => Some("login"),
            "agy" => Some("keychain"),
            _ => None,
        };
        let confined = read_only_plan("/fixture", authentication);
        let token = (id == "claude").then_some(&token);
        let result = entry
            .adapter
            .prepare(fixture_input(&route, &confined, &prompt, repo, token));
        let spec = result?;
        // Only Agy runs its commands on a pseudo-terminal.
        assert_eq!(spec.profile.spec.allow_pseudo_tty, id == "agy");
        // Agy keeps its prompt on argv and has no lane runtime; its own test pins its launch.
        if matches!(id, "agy" | "cursor" | "opencode") {
            continue;
        }
        let argv: Vec<_> = spec.argv.iter().map(|arg| arg.to_string_lossy()).collect();
        // Nothing is inherited: the environment is exactly the allow-list.
        let keys: Vec<_> = spec
            .env
            .iter()
            .map(|(key, _)| key.to_string_lossy())
            .collect();
        let extra: &[&str] = match id {
            "codex" => &[
                "CARGO_HOME",
                "RUSTUP_HOME",
                "RUSTUP_AUTO_INSTALL",
                "CARGO_TARGET_DIR",
                "CARGO_BUILD_BUILD_DIR",
                "CARGO_NET_OFFLINE",
                "CARGO_CACHE_AUTO_CLEAN_FREQUENCY",
                "GIT_CONFIG_GLOBAL",
                "GIT_CONFIG_NOSYSTEM",
                "CODEX_HOME",
                "SHELL",
            ],
            "claude" => &[
                "CLAUDE_CODE_TMPDIR",
                "DISABLE_AUTOUPDATER",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            ],
            _ => &[],
        };
        let mut expected: Vec<_> = [
            "HOME",
            "USER",
            "PATH",
            "TMPDIR",
            "LANG",
            "PROVENANT_AGENT_LABEL",
            "PROVENANT_ROOT",
            "PROVENANT_GIT",
            "PROVENANT_LANE_DEPTH",
            "PROVENANT_PARENT_ATTEMPT_ID",
            "PROVENANT_RUN_STATE_DIR",
        ]
        .iter()
        .chain(extra)
        .copied()
        .collect();
        if cfg!(target_os = "macos")
            && id == "codex"
            && Path::new("/Library/Developer/CommandLineTools").is_dir()
        {
            let index = expected
                .iter()
                .position(|key| *key == "CARGO_CACHE_AUTO_CLEAN_FREQUENCY")
                .expect("cargo env")
                + 1;
            expected.splice(index..index, ["DEVELOPER_DIR", "SDKROOT"]);
        }
        if id == "codex"
            && ["/opt/homebrew", "/usr/local"]
                .iter()
                .any(|prefix| Path::new(prefix).join("bin/node").exists())
        {
            let index = expected
                .iter()
                .position(|key| *key == "GIT_CONFIG_GLOBAL")
                .expect("git env");
            expected.splice(
                index..index,
                [
                    "npm_config_cache",
                    "PNPM_HOME",
                    "npm_config_store_dir",
                    "pnpm_config_store_dir",
                    "npm_config_cache_dir",
                    "pnpm_config_cache_dir",
                    "COREPACK_HOME",
                ],
            );
        }
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
                assert!(argv.iter().any(|arg| arg == "--mcp-config"));
                assert!(argv.iter().any(|arg| arg == "--settings"));
                assert!(!argv.iter().any(|arg| arg == "--safe-mode"));
                assert!(!argv.iter().any(|arg| arg == "--add-dir"));
                assert!(
                    spec.env
                        .iter()
                        .all(|(key, _)| { key != "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD" })
                );
                let sources = argv
                    .iter()
                    .position(|arg| arg == "--setting-sources")
                    .expect("isolated settings");
                assert_eq!(argv[sources + 1], "");
                let tools = argv.iter().position(|arg| arg == "--tools").expect("tools");
                assert_eq!(argv[tools + 1], "Read,Grep,Glob");
                // The host's token is the only credential: no keychain or
                // other credential path is readable. Only Provenant and its hook helpers are added.
                assert!(
                    spec.env
                        .iter()
                        .all(|(key, _)| key != "CLAUDE_CODE_OAUTH_TOKEN")
                );
                let secret = spec.secret_input.as_ref().expect("token pipe");
                assert_eq!(
                    secret.descriptor_env,
                    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR"
                );
                assert_eq!(secret.bytes, FIXTURE_TOKEN.as_bytes());
                let allowed = argv
                    .iter()
                    .position(|arg| arg == "--allowedTools")
                    .expect("allow-list");
                assert_eq!(argv[allowed + 1], "mcp__fabric__*");
                assert!(spec.profile.spec.authentication.is_empty());
                assert!(
                    spec.profile
                        .spec
                        .process_executables
                        .iter()
                        .any(|grant| grant.path
                            == std::path::Path::new("/fixture/provenant/bin/provenant"))
                );
                assert!(matches!(
                    entry
                        .adapter
                        .prepare(fixture_input(&route, &confined, &prompt, repo, None)),
                    Err(ProviderError::UnsupportedPolicy)
                ));
                let with_login = read_only_plan("/fixture", Some("login"));
                assert!(matches!(
                    entry.adapter.prepare(fixture_input(
                        &route,
                        &with_login,
                        &prompt,
                        repo,
                        Some(&fixture_token())
                    )),
                    Err(ProviderError::UnsupportedPolicy)
                ));
            }
            _ => {}
        }
        // The package is the CLI's own install directory, never a shared
        // prefix whose var/ would become readable.
        let shared = PrepareInput {
            executable: std::path::Path::new("/opt/homebrew/bin/provider"),
            ..fixture_input(&route, &confined, &prompt, repo, token)
        };
        assert!(matches!(
            entry.adapter.prepare(shared),
            Err(ProviderError::InvalidPath)
        ));
        let package = std::path::Path::new("/opt/homebrew/Cellar/provider/1.0");
        // Host-detected Homebrew Node toolchain directories (launch::node_tools).
        let homebrew_node = |path: &Path| {
            ["/opt/homebrew", "/usr/local"].iter().any(|prefix| {
                [
                    "bin",
                    "sbin",
                    "Cellar",
                    "opt",
                    "lib",
                    "libexec",
                    "etc",
                    "share",
                    "include",
                    "Frameworks",
                ]
                .iter()
                .any(|name| path == Path::new(prefix).join(name))
            })
        };
        assert!(spec.profile.spec.runtime_reads.iter().all(|grant| {
            grant.path == package
                || homebrew_node(&grant.path)
                || (id == "codex"
                    && (grant.path.starts_with("/fixture/home")
                        || grant.path == Path::new("/Library/Developer/CommandLineTools")
                        || grant.path == Path::new("/private/etc/ssl")))
                || [
                    "/fixture/provenant/bin/provenant",
                    "/fixture/git/bin/git",
                    "/fixture/provenant/release.json",
                    "/fixture/state/config.toml",
                    "/fixture/state/host-id",
                    "/fixture/state/projects",
                    "/fixture/state/fabric",
                ]
                .iter()
                .any(|path| grant.path == std::path::Path::new(path))
        }));
        assert!(
            spec.profile
                .spec
                .tool_exec_roots
                .iter()
                .all(|grant| grant.path == package
                    || homebrew_node(&grant.path)
                    || !grant.path.starts_with("/opt"))
        );
        // A read-only launch may only start inside a declared read root.
        assert!(matches!(
            entry.adapter.prepare(fixture_input(
                &route,
                &confined,
                &prompt,
                std::path::Path::new("/fixture/elsewhere"),
                token,
            )),
            Err(ProviderError::InvalidPath)
        ));
    }
    Ok(())
}

#[test]
fn cursor_launches_fresh_and_resume_with_edits_only_for_writers() -> Result<(), ProviderError> {
    let prompt = Prompt::new("--literal prompt".into());
    let route = route("cursor", "fixture-model");
    let continuation = Continuation::retained("cursor", "fixture-session", &route, "fixture")?;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for resume in [false, true] {
            let plan = fixture_access_plan(access, Some("login"), resume);
            let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
            input.continuation = resume.then_some(&continuation);
            let spec = cursor::Cursor.prepare(input)?;
            let mut expected = vec![
                "/opt/homebrew/Cellar/provider/1.0/bin/provider",
                "--print",
                "--output-format",
                "stream-json",
                "--workspace",
                "/fixture/repo",
                "--trust",
                "--sandbox",
                "disabled",
            ];
            match access {
                Access::ReadOnly => expected.extend(["--mode", "ask"]),
                Access::WorktreeWrite => expected.push("--force"),
            }
            if resume {
                expected.extend(["--resume", "fixture-session"]);
            }
            expected.extend(["--model", "fixture-model", "--", "--literal prompt"]);
            assert_eq!(
                spec.argv,
                expected
                    .iter()
                    .map(std::ffi::OsString::from)
                    .collect::<Vec<_>>()
            );
            assert_eq!(spec.stdin, Stdin::Null);
            assert_eq!(spec.cwd, Path::new("/fixture/repo"));
        }
    }
    let plan = read_only_plan("/fixture", Some("login"));
    let prompt = Prompt::new("x".repeat(65_537));
    assert!(matches!(
        cursor::Cursor.prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            None
        )),
        Err(ProviderError::PromptTooLarge)
    ));
    Ok(())
}

#[test]
fn agy_includes_attempt_scratch_in_the_workspace() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("agy", "fixture-model");
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("keychain"), false);
        for git_context in [false, true] {
            let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
            input.git_context = git_context;
            let spec = agy::Agy.prepare(input)?;
            assert_eq!(
                spec.argv.windows(2).any(|pair| {
                    pair[0] == "--add-dir" && pair[1] == plan.permissions().scratch.path.as_os_str()
                }),
                git_context
            );
        }
    }
    Ok(())
}

#[test]
fn agy_turns_are_bounded_only_by_the_supervisor_deadline() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("agy", "fixture-model");
    let plan = fixture_access_plan(Access::ReadOnly, Some("keychain"), false);
    let spec = agy::Agy.prepare(fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo"),
        None,
    ))?;
    // Zero switches off agy's own print timeout, which would end a turn early.
    assert!(
        spec.argv
            .windows(2)
            .any(|pair| { pair[0] == "--print-timeout" && pair[1] == "0s" })
    );
    Ok(())
}

#[test]
fn agy_writers_exec_what_codex_writers_exec() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let repo = Path::new("/fixture/repo");
    let agy_route = route("agy", "fixture-model");
    let mut codex_route = route("codex", "fixture-model");
    codex_route.account_key = None;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("keychain"), false);
        let spec = agy::Agy.prepare(fixture_input(&agy_route, &plan, &prompt, repo, None))?;
        assert!(
            spec.env
                .iter()
                .any(|(key, value)| key == "SHELL" && value == "/bin/zsh")
        );
        assert_eq!(spec.profile.spec.process_executables.len(), 2);
        let exec = &spec.profile.spec.tool_exec_roots;
        if access == Access::ReadOnly {
            assert_eq!(
                exec,
                &vec![
                    crate::launch::subtree("/bin"),
                    crate::launch::subtree("/usr/bin"),
                    crate::launch::subtree("/usr/libexec")
                ]
            );
            assert!(spec.profile.spec.tool_writes.is_empty());
            assert!(plan.permissions().write_roots.is_empty());
            for (key, value) in [
                ("PATH", "/usr/bin:/bin"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
            ] {
                assert!(spec.env.iter().any(|(k, v)| k == key && v == value));
            }
            continue;
        }
        let plan = fixture_access_plan(access, Some("login"), false);
        let codex =
            codex::Codex.prepare(fixture_input(&codex_route, &plan, &prompt, repo, None))?;
        // Everything but Codex's own package directory.
        let package = Path::new("/opt/homebrew/Cellar/provider/1.0");
        let expected: Vec<_> = codex
            .profile
            .spec
            .tool_exec_roots
            .into_iter()
            .filter(|grant| grant.path != package)
            .collect();
        assert_eq!(exec, &expected);
        assert!(exec.iter().any(|grant| grant.path == Path::new("/bin")));
    }
    Ok(())
}

#[test]
fn agy_read_only_adds_read_roots_outside_the_working_directory() -> Result<(), ProviderError> {
    use provenant_confinement::permissions::{PathGrant, PathScope};
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("agy", "fixture-model");
    let reader = fixture_access_plan(Access::ReadOnly, Some("keychain"), false);
    let mut permissions = reader.permissions().clone();
    permissions.read_roots.push(PathGrant {
        path: "/fixture/build".into(),
        scope: PathScope::Subtree,
    });
    let plan = ConfinedPlan::new(PlanRequest {
        scope: reader.scope(),
        permissions,
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
    .expect("fixture read-only plan");
    let spec = agy::Agy.prepare(fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo/crates"),
        None,
    ))?;
    let added: Vec<_> = spec
        .argv
        .windows(2)
        .filter(|pair| pair[0] == "--add-dir")
        .map(|pair| pair[1].clone())
        .collect();
    assert_eq!(added, [OsString::from("/fixture/build")]);
    Ok(())
}

#[test]
fn claude_read_only_adds_read_roots_outside_the_working_directory() -> Result<(), ProviderError> {
    use provenant_confinement::permissions::{PathGrant, PathScope};
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let prompt = Prompt::new("fixture prompt".into());
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    let token = fixture_token();
    let reader = fixture_access_plan(Access::ReadOnly, None, false);
    let mut permissions = reader.permissions().clone();
    for path in ["/fixture/diffs", "/fixture/repo/crates/sub"] {
        permissions.read_roots.push(PathGrant {
            path: path.into(),
            scope: PathScope::Subtree,
        });
    }
    let plan = ConfinedPlan::new(PlanRequest {
        scope: reader.scope(),
        permissions,
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
    .expect("fixture read-only plan");
    let spec = claude::Claude.prepare(fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo/crates"),
        Some(&token),
    ))?;
    let added: Vec<_> = spec
        .argv
        .windows(2)
        .filter(|pair| pair[0] == "--add-dir")
        .map(|pair| pair[1].clone())
        .collect();
    // The fixture's own /fixture/repo root holds the working directory but
    // reaches beyond it, so it needs a flag; /fixture/repo/crates/sub sits
    // inside the working directory, so it does not.
    assert_eq!(
        added,
        [
            OsString::from("/fixture/repo"),
            OsString::from("/fixture/diffs")
        ]
    );
    Ok(())
}

#[test]
fn cursor_reads_and_execs_only_its_resolved_version_payload() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("cursor", "fixture-model");
    let payload = Path::new("/fixture/home/.local/share/cursor-agent/versions/2026.10.19-6bbf2c4");
    let launcher = payload.join("cursor-agent");
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("login"), false);
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.executable = &launcher;
        let spec = cursor::Cursor.prepare(input)?.profile.spec;
        assert_eq!(
            spec.process_executables,
            vec![crate::launch::exact(&launcher)]
        );
        assert!(
            spec.runtime_reads
                .contains(&crate::launch::subtree(payload))
        );
        assert_eq!(
            spec.tool_exec_roots,
            ["/bin", "/usr/bin"]
                .map(crate::launch::subtree)
                .into_iter()
                .chain([crate::launch::subtree(payload)])
                .collect::<Vec<_>>()
        );
    }
    Ok(())
}

#[test]
fn opencode_uses_user_config_and_accounts_with_read_only_permissions() -> Result<(), ProviderError>
{
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("opencode", "configured/model");
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for git_context in [false, true] {
            let plan = fixture_access_plan(access, None, false);
            let input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
            let spec = opencode::OpenCode.prepare(PrepareInput {
                git_context,
                ..input
            })?;
            let env = |key: &str| {
                spec.env
                    .iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value)
            };
            assert_eq!(
                env("HOME"),
                Some(&std::ffi::OsString::from("/fixture/home"))
            );
            let config_home = Path::new(env("XDG_CONFIG_HOME").expect("config home"));
            let data_home = Path::new(env("XDG_DATA_HOME").expect("data home"));
            assert!(!config_home.starts_with("/fixture/state"));
            assert!(!data_home.starts_with("/fixture/state"));
            if config_home.join("opencode").is_dir() {
                assert!(
                    spec.profile
                        .spec
                        .runtime_reads
                        .contains(&crate::launch::subtree(config_home.join("opencode")))
                );
            }
            assert!(
                spec.profile
                    .spec
                    .runtime_reads
                    .contains(&crate::launch::subtree(data_home.join("opencode")))
            );
            let auth = data_home.join("opencode/auth.json");
            assert!(spec.profile.spec.state_writes.iter().all(|grant| {
                match grant.scope {
                    provenant_confinement::permissions::PathScope::Exact => grant.path != auth,
                    provenant_confinement::permissions::PathScope::Subtree => {
                        !auth.starts_with(&grant.path)
                    }
                }
            }));
            let config: serde_json::Value = serde_json::from_str(
                &env("OPENCODE_CONFIG_CONTENT")
                    .expect("inline config")
                    .to_string_lossy(),
            )
            .expect("JSON config");
            let permission = if access == Access::ReadOnly {
                "deny"
            } else {
                "allow"
            };
            assert_eq!(config["permission"]["edit"], permission);
            assert_eq!(config["permission"]["bash"], permission);
            let scratch = format!("{}/**", plan.permissions().scratch.path.display());
            if git_context {
                assert_eq!(
                    config["permission"]["external_directory"],
                    serde_json::json!({ scratch.clone(): "allow" })
                );
                assert_eq!(config["permission"]["read"][&scratch], "allow");
            } else {
                assert!(config["permission"]["external_directory"].is_null());
                assert!(config["permission"]["read"].is_null());
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct LiveCapture {
    id: String,
    process_exit: Option<i64>,
    outcome: String,
    answering_model: Option<String>,
    argv: Vec<String>,
}

fn recordings(adapter: &str) -> Vec<LiveCapture> {
    let text = match adapter {
        "codex" => fixture!("codex/recordings.json"),
        "claude" => fixture!("claude/recordings.json"),
        "agy" => fixture!("agy/recordings.json"),
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
    // Codex streams carry no model; its recorded answering model comes from the rollout.
    let streamed = capture
        .answering_model
        .as_deref()
        .filter(|_| adapter.id() != "codex");
    assert_eq!(outcome.model(), streamed, "{id}");
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
    live_agy_read_only_fresh,
    agy::Agy,
    "agy-live-read_only-fresh",
    "agy/live-read_only-fresh.jsonl"
);
live_replay!(live_agy_read_only_resume, agy::Agy, "agy-live-read_only-resume", "agy/live-read_only-resume.jsonl", after "agy/live-read_only-fresh.jsonl");
live_replay!(
    live_agy_worktree_write_fresh,
    agy::Agy,
    "agy-live-worktree_write-fresh",
    "agy/live-worktree_write-fresh.jsonl"
);
live_replay!(live_agy_worktree_write_resume, agy::Agy, "agy-live-worktree_write-resume", "agy/live-worktree_write-resume.jsonl", after "agy/live-worktree_write-fresh.jsonl");
live_replay!(
    live_claude_auth_required,
    claude::Claude,
    "claude-live-auth-required",
    "claude/live-auth-required.jsonl"
);

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

use std::ffi::{OsStr, OsString};
use std::path::Path;

/// One recorded call: argv, environment and working directory.
type Call = (Vec<String>, Vec<String>, std::path::PathBuf);

/// Answers keychain reads from a script and records every call.
struct KeychainScript {
    items: std::sync::Mutex<std::collections::VecDeque<Vec<u8>>>,
    statuses: std::sync::Mutex<std::collections::VecDeque<Vec<u8>>>,
    calls: std::sync::Mutex<Vec<Call>>,
}

impl provenant_os::exec::Executor for KeychainScript {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
        let text = |items: &[OsString]| {
            items
                .iter()
                .map(|item| item.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let argv = text(options.argv);
        let env = options
            .env
            .unwrap_or_default()
            .iter()
            .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
            .collect();
        let stdout = if argv[0] == "/usr/bin/security" {
            self.items.lock().unwrap().pop_front().unwrap()
        } else if argv.get(1).is_some_and(|a| a == "auth") {
            self.statuses.lock().unwrap().pop_front().unwrap()
        } else {
            vec![]
        };
        self.calls
            .lock()
            .unwrap()
            .push((argv, env, options.cwd.to_owned()));
        Ok(provenant_os::exec::ExecOutcome {
            termination: provenant_os::exec::Termination::Exit(0),
            stdout: provenant_os::exec::CapturedStream {
                bytes: stdout,
                ..Default::default()
            },
            stderr: Default::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        })
    }
}

/// File-backed fake credentials; only the fake CLI refresh rewrites storage.
struct FileCredentialScript {
    script: KeychainScript,
    path: std::path::PathBuf,
}
impl FileCredentialScript {
    fn new(config: &Path, script: KeychainScript) -> Self {
        std::fs::create_dir_all(config).unwrap();
        let path = config.join(".credentials.json");
        if let Some(item) = script.items.lock().unwrap().pop_front() {
            std::fs::write(&path, item).unwrap();
        }
        Self { script, path }
    }
}
impl provenant_os::exec::Executor for FileCredentialScript {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
        assert_ne!(options.argv[0], OsStr::new("/usr/bin/security"));
        if options.argv.get(1).is_some_and(|arg| arg == "-p")
            && let Some(item) = self.script.items.lock().unwrap().pop_front()
        {
            std::fs::write(&self.path, item).unwrap();
        }
        self.script.execute(options)
    }
}

#[test]
fn agy_account_probe_logs_under_the_host_probe_root() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};

    struct Probe<'a>(&'a Path);
    impl Executor for Probe<'_> {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let log = options
                .argv
                .windows(2)
                .find(|pair| pair[0] == "--log-file")
                .map(|pair| Path::new(&pair[1]))
                .unwrap();
            let directory = log.parent().unwrap();
            assert_eq!(directory.parent(), Some(self.0));
            assert!(
                directory
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("probe-")
            );
            std::fs::write(
                log,
                b"OAuth: authenticated successfully as fixture@example.test\n",
            )
            .unwrap();
            Ok(ExecOutcome {
                termination: provenant_os::exec::Termination::Exit(0),
                stdout: provenant_os::exec::CapturedStream {
                    bytes: br#"{"status":"SUCCESS"}"#.to_vec(),
                    ..Default::default()
                },
                stderr: Default::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }
    let state = tempfile::tempdir().unwrap();
    let host_root = state.path().canonicalize().unwrap();
    let key = crate::accounts::detect(
        &Probe(&host_root),
        "agy",
        Path::new("/bin/agy"),
        Path::new("/home/test"),
        OsStr::new("test"),
        Path::new("/home/test/.agy"),
        &host_root,
    )
    .unwrap();
    assert!(key.starts_with("agy-google-"));
    assert_eq!(std::fs::read_dir(&host_root).unwrap().count(), 0);
}

#[test]
fn agy_account_probe_removes_its_log_directory_when_the_command_fails() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};

    struct Fail;
    impl Executor for Fail {
        fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            Err(ExecError::Busy)
        }
    }
    let state = tempfile::tempdir().unwrap();
    let host_root = state.path().canonicalize().unwrap();
    let result = crate::accounts::detect(
        &Fail,
        "agy",
        Path::new("/bin/agy"),
        Path::new("/home/test"),
        OsStr::new("test"),
        Path::new("/home/test/.agy"),
        &host_root,
    );
    assert!(matches!(result, Err(ProviderError::UnknownIdentity)));
    assert_eq!(std::fs::read_dir(&host_root).unwrap().count(), 0);
}

#[test]
fn agy_account_probe_reports_a_missing_probe_root_as_a_probe_failure() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Count(AtomicUsize);
    impl Executor for Count {
        fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ExecError::Busy)
        }
    }
    let state = tempfile::tempdir().unwrap();
    let host_root = state.path().canonicalize().unwrap();
    let missing = host_root.join("missing");
    let executor = Count(AtomicUsize::new(0));
    let error = crate::accounts::detect(
        &executor,
        "agy",
        Path::new("/bin/agy"),
        Path::new("/home/test"),
        OsStr::new("test"),
        Path::new("/home/test/.agy"),
        &missing,
    )
    .unwrap_err();
    assert!(matches!(&error, ProviderError::ProbeDirectory(_)));
    assert!(error.to_string().contains(missing.to_str().unwrap()));
    assert_eq!(executor.0.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_dir(&host_root).unwrap().count(), 0);
}

#[test]
fn registration_reports_probe_directory_failures_from_the_executor() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};

    struct Fail;
    impl Executor for Fail {
        fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            Err(ExecError::Io {
                operation: crate::host::PROBE_DIR_OPERATION,
                source: std::io::Error::other("fixture probe failure"),
            })
        }
    }
    let state = tempfile::tempdir().unwrap();
    let host_root = state.path().canonicalize().unwrap();
    for adapter in ["claude", "cursor", "agy"] {
        let error = crate::accounts::detect_with_org(
            &Fail,
            adapter,
            Path::new("/bin/provider"),
            Path::new("/home/test"),
            OsStr::new("test"),
            Path::new("/home/test/identity"),
            &host_root,
        )
        .unwrap_err();
        assert!(
            matches!(&error, ProviderError::ProbeDirectory(_)),
            "{adapter}: {error}"
        );
        assert!(error.to_string().contains("fixture probe failure"));
        assert_eq!(std::fs::read_dir(&host_root).unwrap().count(), 0);
    }
}

#[test]
fn registration_names_probe_directory_failures() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};
    use std::os::unix::fs::PermissionsExt;

    struct Never;
    impl Executor for Never {
        fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            panic!("the provider must not run without a private probe directory");
        }
    }
    let state = tempfile::tempdir().unwrap();
    let executor = crate::host::HostProbeExecutor::new(&Never, state.path()).unwrap();
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    for adapter in ["claude", "cursor", "agy"] {
        let error = crate::accounts::detect_with_org(
            &executor,
            adapter,
            Path::new("/bin/provider"),
            Path::new("/home/test"),
            OsStr::new("test"),
            Path::new("/home/test/identity"),
            state.path(),
        )
        .unwrap_err();
        assert!(matches!(&error, ProviderError::ProbeDirectory(_)));
        assert!(error.to_string().contains("private directory"), "{error}");
    }
}

#[test]
fn registration_keeps_other_executor_failures_as_unknown_identity() {
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};

    struct Fail {
        busy: bool,
    }
    impl Executor for Fail {
        fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            Err(if self.busy {
                ExecError::Busy
            } else {
                ExecError::Io {
                    operation: "spawn",
                    source: std::io::Error::other("provider unavailable"),
                }
            })
        }
    }
    let state = tempfile::tempdir().unwrap();
    for adapter in ["claude", "cursor", "agy"] {
        for busy in [false, true] {
            let result = crate::accounts::detect_with_org(
                &Fail { busy },
                adapter,
                Path::new("/bin/provider"),
                Path::new("/home/test"),
                OsStr::new("test"),
                Path::new("/home/test/identity"),
                state.path(),
            );
            assert!(matches!(result, Err(ProviderError::UnknownIdentity)));
            assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 0);
        }
    }
}

#[test]
fn host_preparation_never_uses_prepopulated_child_scratch_or_temporary_directories() {
    use crate::host::HostProbeExecutor;
    use provenant_os::exec::{ExecOptions, Executor};
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    // Check while the command is running, before the private cwd is removed.
    struct CheckCwd<'a> {
        host_root: &'a Path,
        script: &'a FileCredentialScript,
    }
    impl Executor for CheckCwd<'_> {
        fn execute(
            &self,
            options: ExecOptions<'_>,
        ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
            assert_eq!(options.cwd.parent(), Some(self.host_root));
            let metadata = std::fs::metadata(options.cwd).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::read_dir(options.cwd).unwrap().count(), 0);
            // A command may write settings. The next command must not see them.
            std::fs::create_dir(options.cwd.join(".claude")).unwrap();
            std::fs::write(options.cwd.join(".claude/settings.json"), b"{}").unwrap();
            self.script.execute(options)
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let host_root = temp.path().canonicalize().unwrap();
    let child_store = host_root.join("children/parent-attempt");
    let attempt = child_store.join("runs/run/attempts/attempt");
    let scratch = attempt.join("scratch");
    let candidate = attempt.join("tmp/candidate-0");
    for directory in [&scratch, &candidate] {
        std::fs::create_dir_all(directory.join(".claude")).unwrap();
        std::fs::write(
            directory.join(".claude/settings.json"),
            br#"{"hooks":{"SessionStart":[{"command":"child-controlled"}]}}"#,
        )
        .unwrap();
    }
    let now = 1_000_000_000_u64;
    let item = |expires| {
        format!(r#"{{"claudeAiOauth":{{"accessToken":"fixture-access","expiresAt":{expires}}}}}"#)
            .into_bytes()
    };
    let mut seen = std::collections::HashSet::new();
    for supplied_cwd in [&scratch, &candidate] {
        let script = KeychainScript {
            items: std::sync::Mutex::new(vec![item(now + 1000), item(now + 3_600_000)].into()),
            statuses: std::sync::Mutex::new(
                vec![br#"{"loggedIn":true,"email":"fixture@example.test"}"#.to_vec(); 3].into(),
            ),
            calls: Default::default(),
        };
        let config = host_root.join(".claude-pro");
        let script = FileCredentialScript::new(&config, script);
        let checked = CheckCwd {
            host_root: &host_root,
            script: &script,
        };
        let executor = HostProbeExecutor::new(&checked, &host_root).unwrap();
        let login = claude::Login {
            executable: Path::new("/opt/homebrew/bin/claude"),
            home: &host_root,
            user: OsStr::new("u"),
            config_dir: &config,
        };
        let key = crate::accounts::detect(
            &executor,
            "claude",
            login.executable,
            login.home,
            login.user,
            login.config_dir,
            &host_root,
        )
        .unwrap();
        let account =
            crate::identity::RegisteredAccount::new("claude", &key, login.config_dir.to_owned())
                .unwrap();
        claude::refresh_for_account(&executor, &login, &account, &|| now, now + 60_000).unwrap();
        let argv = [login.executable.into(), "--version".into()];
        executor
            .execute(ExecOptions::new(
                &argv,
                supplied_cwd,
                Duration::from_secs(4),
            ))
            .unwrap();
        let calls = script.script.calls.lock().unwrap();
        assert_eq!(calls.len(), 5, "status, refresh and version commands");
        for (_, _, cwd) in calls.iter() {
            assert!(!cwd.starts_with(&child_store));
            assert!(!cwd.exists(), "host cwd must be removed after each command");
            assert!(seen.insert(cwd.clone()), "host cwds must never be reused");
        }
        assert!(supplied_cwd.join(".claude/settings.json").is_file());
    }
}

#[test]
fn claude_concurrent_host_refreshes_do_not_reuse_a_rotating_token() {
    use crate::claude::{
        CredentialAuthority, Login, refresh_for_account_authorised, refresh_if_expiring,
    };
    use provenant_os::exec::{ExecError, ExecOptions, ExecOutcome, Executor};
    use std::sync::{Condvar, Mutex, mpsc};
    use std::time::Duration;

    // Model the CLI reading a single-use refresh token before exchanging it.
    // Overlapping exchanges revoke that token; a later reader sees the renewal.
    struct RotatingCli {
        script: FileCredentialScript,
        exchanges: Mutex<usize>,
        overlap: Condvar,
        entered: mpsc::Sender<()>,
    }
    impl Executor for RotatingCli {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            if options.argv.get(1).is_some_and(|arg| arg == "-p") {
                let mut exchanges = self.exchanges.lock().unwrap();
                *exchanges += 1;
                self.entered.send(()).unwrap();
                if *exchanges > 1 {
                    self.overlap.notify_all();
                    return Err(ExecError::Busy); // OAuth token revoked.
                }
                let _wait = self
                    .overlap
                    .wait_timeout_while(exchanges, Duration::from_secs(1), |count| *count == 1)
                    .unwrap();
            }
            self.script.execute(options)
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join(".claude");
    let now = 1_000_000_000_u64;
    let item = |access, refresh, expires| {
        format!(r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":{expires}}}}}"#).into_bytes()
    };
    let (entered, started) = mpsc::channel();
    let executor = RotatingCli {
        script: FileCredentialScript::new(
            &config,
            KeychainScript {
                items: Mutex::new(
                    vec![
                        item("fixture-old", "fixture-single-use", now + 1000),
                        item("fixture-renewed", "fixture-rotated", now + 3_600_000),
                    ]
                    .into(),
                ),
                statuses: Mutex::new(
                    vec![br#"{"loggedIn":true,"email":"fixture@example.test"}"#.to_vec(); 2].into(),
                ),
                calls: Default::default(),
            },
        ),
        exchanges: Mutex::new(0),
        overlap: Condvar::new(),
        entered,
    };
    let login = Login {
        executable: Path::new("/opt/homebrew/bin/claude"),
        home: temp.path(),
        user: OsStr::new("u"),
        config_dir: &config,
    };
    let key = crate::identity::fingerprint("fixture@example.test").unwrap();
    let account = crate::identity::RegisteredAccount::new("claude", &key, config.clone()).unwrap();
    let authority =
        CredentialAuthority::for_dispatch("fixture".into(), &login, now + 60_000).unwrap();
    std::thread::scope(|scope| {
        // Quota refresh and an authorised dispatch share the same account store.
        let quota =
            scope.spawn(|| refresh_if_expiring(&executor, &login, &|| now, now + 60_000, None));
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let dispatch = refresh_for_account_authorised(
            &executor,
            &login,
            &account,
            &|| now,
            now + 60_000,
            &authority,
        );
        quota.join().unwrap().expect("quota refresh succeeds");
        dispatch.expect("concurrent dispatch must not reuse the rotated refresh token");
    });
    assert_eq!(
        *executor.exchanges.lock().unwrap(),
        1,
        "only one exchange may consume the refresh token"
    );
}

#[test]
fn claude_login_refreshes_only_near_expiry_with_customisations_off_and_never_prints_the_token() {
    use crate::claude::{Login, refresh_if_expiring};
    let now = 1_000_000_000_u64;
    let item = |token: &str, expires: u64| {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"fixture-refresh","expiresAt":{expires}}}}}"#
        )
        .into_bytes()
    };
    let run_by = |items: Vec<Vec<u8>>, deadline: u64| {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(".claude");
        let script = FileCredentialScript::new(
            &config,
            KeychainScript {
                items: std::sync::Mutex::new(items.into()),
                statuses: Default::default(),
                calls: std::sync::Mutex::new(vec![]),
            },
        );
        let login = Login {
            executable: Path::new("/opt/homebrew/bin/claude"),
            home: temp.path(),
            user: OsStr::new("u"),
            config_dir: &config,
        };
        let result = refresh_if_expiring(&script, &login, &|| now, deadline, None);
        (result, script.script.calls.into_inner().unwrap())
    };
    let hour = 3_600_000;
    let run = |items: Vec<Vec<u8>>| run_by(items, now + hour);

    // A spent deadline runs nothing.
    let (token, calls) = run_by(vec![item("fixture-access", now + 1000)], now);
    assert!(token.is_err());
    assert!(calls.is_empty());

    // A live token is used as read: no refresh, and Debug never shows it.
    let (token, calls) = run(vec![item("fixture-access", now + hour)]);
    let token = token.expect("live token");
    let printed = format!("{token:?}");
    assert!(!printed.contains("fixture-access") && !printed.contains("fixture-refresh"));
    assert!(calls.is_empty());

    // Inside the window the CLI refreshes once, in the empty scratch directory
    // with hooks, MCP servers, slash commands and session files off.
    let expiring = item("fixture-access", now + 14 * 60 * 1000);
    let (token, calls) = run(vec![expiring.clone(), item("fixture-renewed", now + hour)]);
    token.expect("refreshed token");
    let (argv, env, cwd) = &calls[0];
    assert_eq!(argv[0], "/opt/homebrew/bin/claude");
    for flag in [
        "--safe-mode",
        "--strict-mcp-config",
        r#"{"disableAllHooks":true}"#,
        "--disable-slash-commands",
        "--no-session-persistence",
    ] {
        assert!(argv.iter().any(|a| a == flag), "{flag}");
    }
    assert_eq!(cwd, Path::new("/"));
    assert!(!env.iter().any(|v| v.starts_with("CLAUDE_CONFIG_DIR=")));
    assert_eq!(calls.len(), 1);

    // The CLI may keep a token inside the window; one with a minute left is
    // accepted, one about to expire is refused.
    let (token, _) = run(vec![expiring.clone(), expiring.clone()]);
    token.expect("still usable");
    let (token, _) = run(vec![expiring, item("fixture-access", now + 30_000)]);
    assert!(matches!(token, Err(ProviderError::CredentialUnavailable)));

    // An expired token is refreshed; it stays expired only if the CLI fails.
    let (token, calls) = run(vec![
        item("fixture-access", now),
        item("fixture-renewed", now + hour),
    ]);
    token.expect("expired token refreshed");
    assert_eq!(calls.len(), 1, "expired storage refreshes once");
    let (token, calls) = run(vec![
        item("fixture-access", now),
        item("fixture-access", now),
    ]);
    assert!(matches!(token, Err(ProviderError::CredentialExpired)));
    assert_eq!(calls.len(), 1);

    // Corrupt stored JSON cannot be repaired by refreshing.
    let (token, calls) = run(vec![b"not json: fixture-secret".to_vec()]);
    assert!(
        matches!(&token, Err(ProviderError::CredentialReadFailed(reason)) if reason == "stored credential JSON is invalid")
    );
    assert!(!token.unwrap_err().to_string().contains("fixture-secret"));
    assert!(
        calls.is_empty(),
        "corrupt file credentials must not refresh"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn claude_keychain_read_failure_does_not_spawn_refresh_probe() {
    use crate::claude::{Login, refresh_if_expiring};

    struct FailedKeychainRead(std::sync::Mutex<Vec<Call>>);

    impl provenant_os::exec::Executor for FailedKeychainRead {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
            let argv: Vec<String> = options
                .argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            self.0.lock().unwrap().push((
                argv.clone(),
                options
                    .env
                    .unwrap_or_default()
                    .iter()
                    .map(|(key, value)| {
                        format!("{}={}", key.to_string_lossy(), value.to_string_lossy())
                    })
                    .collect(),
                options.cwd.into(),
            ));
            Ok(provenant_os::exec::ExecOutcome {
                termination: if argv.first().is_some_and(|arg| arg == "/usr/bin/security") {
                    provenant_os::exec::Termination::Exit(1)
                } else {
                    provenant_os::exec::Termination::Exit(0)
                },
                stdout: Default::default(),
                stderr: Default::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }

    let executor = FailedKeychainRead(Default::default());
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join(".claude-work");
    std::fs::create_dir(&config).unwrap();
    let login = Login {
        executable: Path::new("/opt/homebrew/bin/claude"),
        home: temp.path(),
        user: OsStr::new("u"),
        config_dir: &config,
    };

    let result = refresh_if_expiring(&executor, &login, &|| 1_000_000, 2_000_000, None);

    assert!(matches!(
        result,
        Err(ProviderError::CredentialReadFailed(_))
    ));
    let calls = executor.0.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "a failed keychain read must not start a refresh"
    );
    assert_eq!(calls[0].0[0], "/usr/bin/security");
    use std::os::unix::ffi::OsStrExt;
    let suffix = crate::claude::keychain_suffix(config.as_os_str().as_bytes());
    assert_eq!(calls[0].0[3], format!("Claude Code-credentials-{suffix}"));
    assert!(
        calls[0]
            .1
            .contains(&format!("CLAUDE_CONFIG_DIR={}", config.display()))
    );
}

#[test]
fn codex_read_only_toolchain_uses_private_build_and_cargo_home_linked_to_shared_stores() {
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let plan = read_only_plan("/fixture", Some("login"));
    let prompt = Prompt::new("fixture".into());
    let spec = crate::codex::Codex
        .prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            std::path::Path::new("/fixture/repo"),
            None,
        ))
        .unwrap();
    let env = |key: &str| spec.env.iter().find(|(k, _)| k == key).unwrap().1.clone();
    assert_eq!(env("CARGO_TARGET_DIR"), "/fixture/scratch/target");
    assert_eq!(env("RUSTUP_HOME"), "/fixture/home/.rustup");
    assert_eq!(env("RUSTUP_AUTO_INSTALL"), "0");
    let paths = |grants: &[provenant_confinement::permissions::PathGrant]| {
        grants
            .iter()
            .map(|g| g.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    let reads = paths(&spec.profile.spec.runtime_reads);
    for dir in ["/fixture/home/.cargo", "/fixture/home/.rustup"] {
        assert!(reads.iter().any(|path| path == dir), "{dir}");
    }
    // Cargo downloads over TLS, so the system trust roots are readable.
    #[cfg(target_os = "macos")]
    assert!(reads.iter().any(|path| path == "/private/etc/ssl"));
    // Shared Cargo stores are never writable; Cargo downloads into its own
    // lane-private home. Installed binaries and toolchains stay read-only.
    assert!(spec.profile.spec.tool_writes.is_empty());
    assert_eq!(env("CARGO_HOME"), "/fixture/scratch/cargo-home");
    // The registry and Git checkouts are linked read-only, never granted.
    for name in ["registry", "git"] {
        assert!(
            spec.setup.iter().any(|step| matches!(
                step,
                crate::launch::SetupStep::Symlink { link, target }
                    if link == &std::path::Path::new("/fixture/scratch/cargo-home").join(name)
                        && target == &std::path::Path::new("/fixture/home/.cargo").join(name)
            )),
            "{name}"
        );
    }
    for dir in [
        "/fixture/home/.cargo/bin",
        "/fixture/home/.rustup/toolchains",
    ] {
        assert!(
            spec.profile
                .spec
                .tool_exec_roots
                .iter()
                .any(|g| g.path == std::path::Path::new(dir))
        );
    }
}

fn covers(grant: &provenant_confinement::permissions::PathGrant, path: &std::path::Path) -> bool {
    use provenant_confinement::permissions::PathScope;
    match grant.scope {
        PathScope::Exact => grant.path == path,
        PathScope::Subtree => path.starts_with(&grant.path),
    }
}

#[test]
fn cargo_build_dir_is_pinned_to_lane_target_for_each_access() -> Result<(), ProviderError> {
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    for (access, target) in [
        (Access::ReadOnly, "/fixture/scratch/target"),
        (
            Access::WorktreeWrite,
            "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/target",
        ),
    ] {
        let plan = fixture_access_plan(access, Some("login"), false);
        let launch = crate::codex::Codex.prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            None,
        ))?;
        for key in ["CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR"] {
            assert!(
                launch.env.iter().any(|(k, v)| k == key && v == target),
                "{access:?}: {key} must be {target}"
            );
        }
    }
    Ok(())
}

#[test]
fn writer_lint_target_is_a_lane_cache_sibling_of_the_build_target() -> Result<(), ProviderError> {
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let cache = "/fixture/home/Library/Caches/provenant-lane-cargo/fixture";
    let plan = fixture_access_plan(Access::WorktreeWrite, Some("login"), false);
    let launch = crate::codex::Codex.prepare(fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo"),
        None,
    ))?;
    // Lint-only cargo runs take their own build directory, so they do not
    // queue on the lock that test and build runs hold.
    let lint = format!("{cache}/target-lint");
    assert!(
        launch
            .env
            .iter()
            .any(|(k, v)| k == "PROVENANT_LANE_CARGO_LINT_TARGET" && v == lint.as_str()),
        "PROVENANT_LANE_CARGO_LINT_TARGET must be {lint}"
    );
    assert!(
        Path::new(&lint).starts_with(cache),
        "the lint target must sit inside the granted lane cache"
    );
    let read_only = fixture_access_plan(Access::ReadOnly, Some("login"), false);
    let launch = crate::codex::Codex.prepare(fixture_input(
        &route,
        &read_only,
        &prompt,
        Path::new("/fixture/repo"),
        None,
    ))?;
    assert!(
        launch
            .env
            .iter()
            .all(|(k, _)| k != "PROVENANT_LANE_CARGO_LINT_TARGET"),
        "read-only lanes have no lane cache to hold a lint target"
    );
    Ok(())
}

#[test]
fn no_lane_can_write_shared_toolchain_stores() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let shared = [
        ".cargo/registry",
        ".cargo/registry/src/index/crate-1.0/build.rs",
        ".cargo/git",
        ".cargo/git/checkouts/repo/build.rs",
        ".cargo/.package-cache",
        ".cargo/.global-cache",
        "Library/pnpm",
        "Library/pnpm/pnpm",
        "Library/Caches/pnpm",
        ".npm",
        ".npm/_cacache/content",
        ".cache/node",
        ".cache/node/corepack",
    ];
    for entry in REGISTRY {
        let id = entry.adapter.id();
        // Claude shares the same launch helpers; its account setup needs
        // identities, so its writer lane is covered by its own test.
        if id != "codex" {
            continue;
        }
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            let plan = fixture_access_plan(access, (id == "codex").then_some("login"), false);
            let launch = entry.adapter.prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                std::path::Path::new("/fixture/repo"),
                None,
            ))?;
            let spec = &launch.profile.spec;
            let writable: Vec<_> = spec
                .tool_writes
                .iter()
                .chain(&spec.state_writes)
                .chain(
                    spec.authentication
                        .iter()
                        .flat_map(|auth| &auth.in_place_writes),
                )
                .collect();
            for relative in shared {
                let path = std::path::Path::new("/fixture/home").join(relative);
                assert!(
                    !writable.iter().any(|grant| covers(grant, &path)),
                    "{id} {access:?}: {}",
                    path.display()
                );
            }
            let env = |key: &str| {
                launch
                    .env
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| std::path::PathBuf::from(v))
            };
            let cache = match access {
                Access::ReadOnly => std::path::PathBuf::from("/fixture/scratch"),
                _ => "/fixture/home/Library/Caches/provenant-lane-cargo/fixture".into(),
            };
            assert_eq!(env("CARGO_HOME"), Some(cache.join("cargo-home")));
            // Node variables appear only where a Homebrew Node exists.
            for (key, leaf) in [
                ("npm_config_cache", "npm"),
                ("PNPM_HOME", "pnpm-home"),
                ("npm_config_store_dir", "pnpm-store"),
                ("pnpm_config_store_dir", "pnpm-store"),
                ("npm_config_cache_dir", "pnpm-cache"),
                ("pnpm_config_cache_dir", "pnpm-cache"),
            ] {
                if let Some(value) = env(key) {
                    assert_eq!(value, cache.join(leaf), "{id} {access:?} {key}");
                }
            }
        }
    }
    Ok(())
}

#[test]
fn provider_recipes_cannot_write_user_configuration() -> Result<(), ProviderError> {
    let cases: &[(&str, &[&str])] = &[
        (
            "claude",
            &[
                ".claude/settings.json",
                ".claude/hooks/run",
                ".claude/CLAUDE.md",
                ".claude/skills/test/SKILL.md",
                ".claude/ide/state",
                ".claude/jobs/job",
                ".claude/state/file",
                ".claude/teams/team",
            ],
        ),
        (
            "codex",
            &[
                ".codex/config.toml",
                ".codex/rules/default.rules",
                ".codex/skills/test/SKILL.md",
                ".codex/hooks/run",
                ".codex/extensions/test",
            ],
        ),
        (
            "opencode",
            &[
                ".config/opencode/opencode.json",
                ".config/opencode/plugins/run.js",
                ".config/opencode/skills/test/SKILL.md",
            ],
        ),
        (
            "agy",
            &[
                ".gemini/settings.json",
                ".gemini/config/mcp_config.json",
                ".gemini/config/plugins/run",
                ".gemini/antigravity-cli/sidecars/run",
                ".gemini/antigravity-cli/plugin_data/file",
                ".gemini/antigravity-cli/knowledge/file",
            ],
        ),
        (
            "cursor",
            &[
                ".cursor/hooks.json",
                ".cursor/extensions/run",
                ".cursor/skills/test/SKILL.md",
                ".cursor/plans/plan",
                ".cursor/ai-tracking/file",
            ],
        ),
    ];
    let prompt = Prompt::new("fixture prompt".into());
    for &(id, forbidden) in cases {
        let adapter = REGISTRY
            .iter()
            .find(|entry| entry.adapter.id() == id)
            .expect("provider")
            .adapter;
        let recording = match id {
            "claude" => fixture!("claude/direct-model.jsonl"),
            "codex" => fixture!("codex/success.jsonl"),
            "cursor" => fixture!("cursor/fresh.jsonl"),
            "agy" => fixture!("agy/success.jsonl"),
            "opencode" => fixture!("opencode/unversioned-success.jsonl"),
            _ => return Err(ProviderError::UnknownIdentity),
        };
        let outcome = replay(adapter, recording, EXIT)?.1;
        let route = route(id, outcome.model().unwrap_or("fixture-model"));
        let account_dir = std::path::Path::new("/fixture/identities/opaque");
        let account = matches!(id, "claude" | "codex")
            .then(|| identity::RegisteredAccount::new(id, "opaque-account", account_dir.into()))
            .transpose()?;
        let account_token =
            (id == "claude").then(|| fixture_token().for_account("opaque-account", account_dir));
        // Synthetic continuation covers every adapter and mode.
        let continuation = Continuation::from_outcome(id, &outcome, &route, "fixture", false)?;
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            for session in [Session::Fresh, Session::Resume] {
                let plan = fixture_access_plan(
                    access,
                    match id {
                        "codex" | "cursor" => Some("login"),
                        "agy" => Some("keychain"),
                        _ => None,
                    },
                    session == Session::Resume,
                );
                let mut input = fixture_input(
                    &route,
                    &plan,
                    &prompt,
                    std::path::Path::new("/fixture/repo"),
                    account_token.as_ref(),
                );
                input.account = account.as_ref();
                input.continuation = (session == Session::Resume).then_some(&continuation);
                let launch = adapter.prepare(input)?;
                for relative in forbidden
                    .iter()
                    .copied()
                    .chain([".npm/_cacache/content", ".npm/_logs/log"])
                {
                    let path = std::path::Path::new("/fixture/home").join(relative);
                    let writable = launch
                        .profile
                        .spec
                        .state_writes
                        .iter()
                        .chain(
                            launch
                                .profile
                                .spec
                                .authentication
                                .iter()
                                .flat_map(|auth| &auth.in_place_writes),
                        )
                        .chain(std::iter::once(&plan.permissions().scratch))
                        .chain(&plan.permissions().private_state)
                        .any(|grant| covers(grant, &path));
                    assert!(!writable, "{id} {access:?} {session:?}: {}", path.display());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn agy_runs_in_a_private_home_that_links_only_the_login_keychain() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("agy", "gemini-3.8-flash");
    let cwd = std::path::Path::new("/fixture/repo");
    for (access, edits) in [(Access::ReadOnly, false), (Access::WorktreeWrite, true)] {
        let plan = fixture_access_plan(access, Some("keychain"), false);
        let spec = agy::Agy.prepare(fixture_input(&route, &plan, &prompt, cwd, None))?;
        let argv: Vec<_> = spec.argv.iter().map(|arg| arg.to_string_lossy()).collect();
        // Headless lanes auto-approve tools within the outer profile.
        assert!(
            argv.iter()
                .any(|arg| arg == "--dangerously-skip-permissions")
        );
        assert!(!argv.iter().any(|arg| arg == "--sandbox"));
        assert_eq!(spec.stdin, Stdin::Null);
        let home = spec
            .env
            .iter()
            .find(|(key, _)| key == "HOME")
            .map(|(_, value)| std::path::Path::new(value));
        assert_eq!(home, Some(std::path::Path::new("/fixture/state")));
        let keychain = std::path::Path::new("/fixture/home/Library/Keychains/login.keychain-db");
        assert!(spec.setup.contains(&SetupStep::Symlink {
            link: "/fixture/state/Library/Keychains/login.keychain-db".into(),
            target: keychain.into(),
        }));
        // Nothing of the user's Gemini state is granted, and the keychain is read-only.
        let spec = &spec.profile.spec;
        assert_eq!(spec.authentication.len(), 1);
        assert_eq!(
            spec.authentication[0].roots,
            [crate::launch::exact(keychain)]
        );
        assert!(spec.authentication[0].in_place_writes.is_empty());
        assert!(spec.state_writes.is_empty());
        // Writers read only the shared toolchains their commands run.
        assert_eq!(spec.runtime_reads.is_empty(), !edits);
        assert!(
            spec.runtime_reads
                .iter()
                .all(|grant| !grant.path.starts_with("/fixture/home/.gemini"))
        );
    }
    Ok(())
}

#[test]
fn opencode_free_limit_preserves_provider_message() -> Result<(), Box<dyn std::error::Error>> {
    // Synthetic protocol regression from #1216, not an activation recording.
    let message = "Free tier exhausted. See https://opencode.ai/go\\quota";
    for wrapped in [false, true] {
        for string_body in [false, true] {
            let body =
                serde_json::json!({"error":{"type":"FreeUsageLimitError","message":message}});
            let body = if string_body {
                serde_json::Value::String(body.to_string())
            } else {
                body
            };
            let event =
                serde_json::json!({"type":"provider.quota","status":429,"response":{"body":body}});
            let event = if wrapped {
                serde_json::json!({"type":"error","error":event})
            } else {
                event
            };
            let (events, outcome) = replay(
                &opencode::OpenCode,
                &(event.to_string() + "\n"),
                ExitObservation {
                    code: Some(1),
                    timed_out: false,
                },
            )?;
            assert_eq!(outcome.status(), OutcomeStatus::Refused);
            assert!(events.contains(&ProviderEvent::QuotaLimited {
                message: message.into(),
                pool: Some("opencode-free".into()),
                resets_in_seconds: None,
            }));
        }
    }
    for response in [
        serde_json::json!({}),
        serde_json::json!({"body":{"error":{"code":429,"message":"provider quota message"}}}),
    ] {
        let event = serde_json::json!({"type":"provider.quota","message":"provider quota message","response":response});
        let (events, outcome) = replay(&opencode::OpenCode, &(event.to_string() + "\n"), EXIT)?;
        assert_eq!(outcome.status(), OutcomeStatus::Refused);
        assert!(events.contains(&ProviderEvent::QuotaLimited {
            message: "provider quota message".into(),
            pool: None,
            resets_in_seconds: None,
        }));
    }
    Ok(())
}

#[test]
fn opencode_free_limit_decodes_retry_after_ms() -> Result<(), Box<dyn std::error::Error>> {
    let message = "Free tier exhausted. See https://opencode.ai/go\\quota";
    for (retry_after_val, expected_resets) in [
        (serde_json::json!(240_000), Some(240)),
        (serde_json::json!(240_000.0), Some(240)),
        (serde_json::json!(1_500), Some(2)),
        (serde_json::json!(999_999_999), Some(86_400)),
        (serde_json::json!(null), None),
        (serde_json::json!(-1000), None),
        (serde_json::json!(0), None),
        (serde_json::json!("soon"), None),
        (serde_json::json!(true), None),
    ] {
        for wrapped in [false, true] {
            let body = serde_json::json!({
                "error": {
                    "type": "FreeUsageLimitError",
                    "message": message,
                }
            });
            let mut event = serde_json::json!({
                "type": "provider.quota",
                "status": 429,
                "response": {"body": body},
                "retryAfterMs": retry_after_val,
            });
            if wrapped {
                event = serde_json::json!({
                    "type": "error",
                    "error": event,
                });
            }
            let (events, outcome) = replay(
                &opencode::OpenCode,
                &(event.to_string() + "\n"),
                ExitObservation {
                    code: Some(1),
                    timed_out: false,
                },
            )?;
            assert_eq!(outcome.status(), OutcomeStatus::Refused);
            assert!(
                events.contains(&ProviderEvent::QuotaLimited {
                    message: message.into(),
                    pool: Some("opencode-free".into()),
                    resets_in_seconds: expected_resets,
                }),
                "failed for retry_after_val: {retry_after_val:?}, wrapped: {wrapped}"
            );
        }
    }

    let body = serde_json::json!({
        "error": {
            "type": "FreeUsageLimitError",
            "message": message,
            "retryAfterMs": 120_000,
        }
    });
    let event = serde_json::json!({
        "type": "provider.quota",
        "status": 429,
        "response": {"body": body},
    });
    let (events, outcome) = replay(
        &opencode::OpenCode,
        &(event.to_string() + "\n"),
        ExitObservation {
            code: Some(1),
            timed_out: false,
        },
    )?;
    assert_eq!(outcome.status(), OutcomeStatus::Refused);
    assert!(events.contains(&ProviderEvent::QuotaLimited {
        message: message.into(),
        pool: Some("opencode-free".into()),
        resets_in_seconds: Some(120),
    }));

    let body = serde_json::json!({
        "error": {
            "type": "FreeUsageLimitError",
            "message": message,
        }
    });
    let event = serde_json::json!({
        "type": "error",
        "retryAfterMs": 180_000,
        "error": {
            "type": "provider.quota",
            "status": 429,
            "response": {"body": body},
        }
    });
    let (events, outcome) = replay(
        &opencode::OpenCode,
        &(event.to_string() + "\n"),
        ExitObservation {
            code: Some(1),
            timed_out: false,
        },
    )?;
    assert_eq!(outcome.status(), OutcomeStatus::Refused);
    assert!(events.contains(&ProviderEvent::QuotaLimited {
        message: message.into(),
        pool: Some("opencode-free".into()),
        resets_in_seconds: Some(180),
    }));

    let (events, outcome) = replay(
        &opencode::OpenCode,
        fixture!("opencode/free-limit.jsonl"),
        ExitObservation {
            code: Some(1),
            timed_out: false,
        },
    )?;
    assert_eq!(outcome.status(), OutcomeStatus::Refused);
    assert!(events.contains(&ProviderEvent::QuotaLimited {
        message: "FIXTURE_USER_OR_PROVIDER_TEXT_021".into(),
        pool: Some("opencode-free".into()),
        resets_in_seconds: Some(240),
    }));

    Ok(())
}

#[test]
fn selected_accounts_control_provider_environment_and_exact_authentication_grants()
-> Result<(), Box<dyn std::error::Error>> {
    use std::path::Path;
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    let temp = tempfile::tempdir().unwrap();
    for adapter in ["claude", "codex"] {
        for key in ["fixture-one", "fixture-two"] {
            let mut route = route(adapter, "fixture-model");
            let email = format!("{key}@example.test");
            let account_key = if adapter == "claude" {
                identity::fingerprint(&email).unwrap()
            } else {
                key.into()
            };
            route.account_key = Some(account_key.clone());
            let dir = temp.path().join(key);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join(".credentials.json"),
                br#"{"claudeAiOauth":{"accessToken":"fixture-token","expiresAt":4000000000000}}"#,
            )?;
            let account = identity::RegisteredAccount::new(adapter, &account_key, dir.clone())?;
            let selected_token;
            let token = if adapter == "claude" {
                let status =
                    serde_json::to_vec(&serde_json::json!({"loggedIn":true,"email":email}))?;
                let script = KeychainScript {
                    items: std::sync::Mutex::new([br#"{"claudeAiOauth":{"accessToken":"fixture-token","expiresAt":4000000000000}}"#.to_vec()].into()),
                    statuses: std::sync::Mutex::new([status.clone(), status].into()),
                    calls: Default::default(),
                };
                selected_token = claude::refresh_for_account(
                    &script,
                    &claude::Login {
                        executable: Path::new("/fixture/bin/claude"),
                        home: Path::new("/fixture/home"),
                        user: "fixture".as_ref(),
                        config_dir: &dir,
                    },
                    &account,
                    &|| 1000,
                    10000,
                )?;
                &selected_token
            } else {
                &token
            };
            let confined = read_only_plan("/fixture", (adapter == "codex").then_some("login"));
            let mut input = fixture_input(
                &route,
                &confined,
                &prompt,
                Path::new("/fixture/repo"),
                Some(token),
            );
            input.account = Some(&account);
            let unplaced = fixture_input(
                &route,
                &confined,
                &prompt,
                Path::new("/fixture/repo"),
                Some(token),
            );
            assert!(
                REGISTRY
                    .iter()
                    .find(|r| r.adapter.id() == adapter)
                    .unwrap()
                    .adapter
                    .prepare(unplaced)
                    .is_err()
            );
            let spec = REGISTRY
                .iter()
                .find(|r| r.adapter.id() == adapter)
                .unwrap()
                .adapter
                .prepare(input)?;

            if adapter == "claude" {
                let mut unattributed_route = route.clone();
                unattributed_route.account_key = None;
                assert!(matches!(
                    claude::Claude.prepare(fixture_input(
                        &unattributed_route,
                        &confined,
                        &prompt,
                        Path::new("/fixture/repo"),
                        Some(token)
                    )),
                    Err(ProviderError::IdentityMismatch)
                ));
                let other_key = identity::fingerprint("other@example.test").unwrap();
                let other = identity::RegisteredAccount::new(
                    "claude",
                    &other_key,
                    Path::new("/fixture/identities/other").into(),
                )?;
                let mut other_route = route.clone();
                other_route.account_key = Some(other_key);
                let mut wrong_token = fixture_input(
                    &other_route,
                    &confined,
                    &prompt,
                    Path::new("/fixture/repo"),
                    Some(token),
                );
                wrong_token.account = Some(&other);
                assert!(matches!(
                    claude::Claude.prepare(wrong_token),
                    Err(ProviderError::IdentityMismatch)
                ));
                assert!(
                    spec.env
                        .iter()
                        .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == dir.as_os_str())
                );
                assert!(
                    spec.profile
                        .spec
                        .state_writes
                        .iter()
                        .filter(|g| !g.path.starts_with("/fixture/state"))
                        .all(|g| g.path.starts_with(&dir))
                );
                let lane = spec
                    .profile
                    .spec
                    .state_writes
                    .iter()
                    .filter(|g| g.path.starts_with("/fixture/state"))
                    .collect::<Vec<_>>();
                let mut expected = vec![
                    crate::launch::subtree("/fixture/state/children/attempt"),
                    crate::launch::subtree("/fixture/state/fabric/locks"),
                    crate::launch::subtree("/fixture/state/workflow"),
                ];
                expected.extend(
                    [
                        "fabric/fabric.sqlite3",
                        "fabric/fabric.sqlite3-wal",
                        "fabric/fabric.sqlite3-shm",
                        "fabric/fabric.sqlite3-journal",
                        "host-binding.lock",
                    ]
                    .map(|name| crate::launch::exact(Path::new("/fixture/state").join(name))),
                );
                assert_eq!(lane.len(), expected.len());
                assert!(expected.iter().all(|grant| lane.contains(&grant)));
            } else {
                // The accepted design keeps runtime state per attempt. Only the
                // selected identity's auth.json is linked into CODEX_HOME.
                let auth = dir.join("auth.json");
                assert_eq!(spec.profile.spec.authentication[0].roots[0].path, auth);
                assert_eq!(
                    spec.profile.spec.authentication[0].in_place_writes[0].path,
                    auth
                );
                assert!(
                    matches!(&spec.setup[0], SetupStep::Symlink { target, .. } if target == &auth)
                );
            }
        }
    }
    Ok(())
}

#[cfg(feature = "recording")]
#[test]
fn claude_writer_shell_has_private_config_scrubbed_children_and_rust_tools()
-> Result<(), ProviderError> {
    let plan = fixture_access_plan(Access::WorktreeWrite, None, false);
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture prompt".into());
    let token = fixture_token();
    let launch = claude::Claude.prepare(fixture_input(
        &route,
        &plan,
        &prompt,
        std::path::Path::new("/fixture/repo"),
        Some(&token),
    ))?;
    assert!(
        launch
            .argv
            .iter()
            .any(|arg| arg == "Read,Grep,Glob,Edit,Write,Bash")
    );
    assert!(launch.argv.windows(2).any(|pair| {
        pair[0] == "--allowedTools" && pair[1] == "Read,Grep,Glob,Edit,Write,Bash,mcp__fabric__*"
    }));
    assert!(
        launch
            .env
            .iter()
            .all(|(key, _)| key != "CLAUDE_CODE_OAUTH_TOKEN")
    );
    assert!(launch.profile.spec.deny_keychain_access);
    assert!(launch.profile.spec.authentication.is_empty());
    for (key, value) in [
        ("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB", "1"),
        (
            "CARGO_HOME",
            "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/cargo-home",
        ),
        (
            "CARGO_TARGET_DIR",
            "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/target",
        ),
        (
            "CARGO_BUILD_BUILD_DIR",
            "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/target",
        ),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
    ] {
        assert!(
            launch.env.iter().any(|(k, v)| k == key && v == value),
            "{key}"
        );
    }
    for path in [
        "/fixture/home/.cargo/bin",
        "/fixture/home/.rustup/toolchains",
        "/fixture/home/Library/Caches/provenant-lane-cargo/fixture",
    ] {
        assert!(
            launch
                .profile
                .spec
                .tool_exec_roots
                .iter()
                .any(|g| g.path == Path::new(path)),
            "{path}"
        );
    }
    // No shared toolchain store is writable; the compiler hard-denies Cargo
    // credentials.
    assert!(launch.profile.spec.tool_writes.is_empty());
    // Claude's env scrub drops GIT_CONFIG_GLOBAL from Bash, so scratch git is a
    // wrapper that sets it, and its directory is executable.
    assert!(launch.setup.iter().any(|step| matches!(
        step,
        crate::launch::SetupStep::WriteExecutable { path, .. } if path == Path::new("/fixture/scratch/bin/git")
    )));
    assert!(
        launch
            .profile
            .spec
            .tool_exec_roots
            .iter()
            .any(|g| g.path == Path::new("/fixture/scratch/bin"))
    );
    // Runtime roots are readable; no grant names a credentials file, and the
    // compiler hard-denies Cargo credentials inside them.
    assert!(
        launch
            .profile
            .spec
            .runtime_reads
            .iter()
            .any(|grant| grant.path == Path::new("/fixture/home/.cargo"))
    );
    assert!(
        launch
            .profile
            .spec
            .runtime_reads
            .iter()
            .all(|grant| !grant.path.to_string_lossy().contains("credentials"))
    );
    assert!(launch.profile.spec.state_writes.iter().all(|grant| {
        !grant.path.starts_with("/fixture/home/.cargo")
            && !grant.path.starts_with("/fixture/home/.rustup")
    }));
    // Only lane grants are writable: never the private config or the user's home.
    assert!(launch.profile.spec.state_writes.iter().all(|grant| {
        !grant.path.starts_with("/fixture/state/claude-config")
            && !grant.path.starts_with("/fixture/home")
    }));
    assert!(
        launch
            .env
            .iter()
            .any(|(key, value)| key == "CLAUDE_CONFIG_DIR"
                && value == "/fixture/state/claude-config")
    );
    let secret = launch.secret_input.as_ref().expect("token pipe");
    assert_eq!(
        secret.descriptor_env,
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR"
    );
    assert_eq!(secret.bytes, FIXTURE_TOKEN.as_bytes());
    Ok(())
}

#[test]
fn claude_account_probe_carries_user_for_keychain_selection() {
    let probe_root = tempfile::tempdir().unwrap();
    let script = KeychainScript {
        items: Default::default(),
        statuses: std::sync::Mutex::new(
            vec![br#"{"loggedIn":true,"email":"fixture@example.test"}"#.to_vec()].into(),
        ),
        calls: std::sync::Mutex::new(vec![]),
    };
    let key = crate::accounts::detect(
        &script,
        "claude",
        Path::new("/opt/homebrew/bin/claude"),
        Path::new("/home/u"),
        OsStr::new("u"),
        Path::new("/home/u/.claude-pro"),
        probe_root.path(),
    );
    assert!(key.is_ok());
    let calls = script.calls.into_inner().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1.iter().any(|entry| entry == "USER=u"));
}

#[test]
fn claude_account_probe_names_the_missing_login() {
    let probe_root = tempfile::tempdir().unwrap();
    let script = KeychainScript {
        items: Default::default(),
        statuses: std::sync::Mutex::new(
            vec![br#"{"loggedIn":false,"authMethod":"none"}"#.to_vec()].into(),
        ),
        calls: std::sync::Mutex::new(vec![]),
    };
    let error = crate::accounts::detect(
        &script,
        "claude",
        Path::new("/opt/homebrew/bin/claude"),
        Path::new("/home/u"),
        OsStr::new("u"),
        Path::new("/home/u/.claude-pro"),
        probe_root.path(),
    )
    .unwrap_err();
    assert!(matches!(error, ProviderError::LoginRequired));
    assert!(error.to_string().contains("unlock the login keychain"));
}

#[test]
fn codex_rollout_skips_long_unparsable_and_unrelated_lines() {
    let home = std::env::temp_dir().join(format!("provenant-rollout-{}", std::process::id()));
    let day = home.join("sessions/2026/10/24");
    std::fs::create_dir_all(&day).unwrap();
    let thread = "01234567-89ab-cdef-0123-456789abcdef";
    let long = format!(
        "{{\"type\":\"response_item\",\"payload\":{{\"output\":\"{}\"}}}}\n",
        "x".repeat(3 * 1024 * 1024)
    );
    let rollout = format!(
        "{}\n{long}not json\n{{\"type\":\"event_msg\",\"payload\":{{\"model\":7}}}}\n{}\n",
        serde_json::json!({"type":"session_meta","payload":{"id":thread}}),
        serde_json::json!({"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"low"}}),
    );
    std::fs::write(
        day.join(format!("rollout-2026-10-24T00-00-00-{thread}.jsonl")),
        rollout,
    )
    .unwrap();
    let observed = crate::codex_rollout::observe(&home, thread);
    std::fs::remove_dir_all(&home).unwrap();
    let turn = observed.unwrap();
    assert_eq!(turn.model, "gpt-6-luna");
    assert_eq!(turn.effort.as_deref(), Some("low"));
}

#[test]
fn rust_tools_exec_repository_scripts_in_the_working_directory() {
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let plan = read_only_plan("/fixture", Some("login"));
    let prompt = Prompt::new("fixture".into());
    let spec = crate::codex::Codex
        .prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            None,
        ))
        .unwrap();
    let exec = &spec.profile.spec.tool_exec_roots;
    assert!(exec.iter().any(|g| g.path == Path::new("/fixture/repo")
        && g.scope == provenant_confinement::permissions::PathScope::Subtree));
    // Exec does not widen reads: the worktree was already readable.
    assert!(!exec.iter().any(|g| g.path == Path::new("/fixture/home")));
}

#[test]
fn writer_git_core_helpers_are_readable_and_executable_only_for_writers()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    // The host resolves Homebrew's bin/git symlink into its versioned keg.
    let keg = temp.path().join("Cellar/git/1.0");
    let git = keg.join("bin/git");
    let core = keg.join("libexec/git-core");
    std::fs::create_dir_all(&core)?;
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("login"), false);
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.provenant.git = &git;
        let launch = crate::codex::Codex.prepare(input)?;
        let profile = &launch.profile.spec;
        let mut helpers = vec![core.clone()];
        if cfg!(target_os = "macos") && Path::new("/Library/Developer/CommandLineTools").is_dir() {
            helpers.push("/Library/Developer/CommandLineTools/usr/libexec/git-core".into());
        }
        for helper in helpers {
            assert_eq!(
                profile
                    .tool_exec_roots
                    .iter()
                    .any(|grant| grant.path == helper
                        && grant.scope == provenant_confinement::permissions::PathScope::Subtree),
                access == Access::WorktreeWrite,
                "{access:?}: {}",
                helper.display()
            );
            assert!(!profile.tool_writes.iter().any(|grant| grant.path == helper));
        }
        assert_eq!(
            profile.runtime_reads.iter().any(|grant| grant.path == core
                && grant.scope == provenant_confinement::permissions::PathScope::Subtree),
            access == Access::WorktreeWrite
        );
        // Only the selected installation's helper directory is added.
        assert!(
            !profile
                .tool_exec_roots
                .iter()
                .any(|grant| grant.path == keg)
        );
    }
    Ok(())
}

#[test]
fn lanes_read_git_global_ignore_and_attributes_only_when_present()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::PathScope;
    let temp = tempfile::tempdir()?;
    let home = temp.path();
    let git = home.join(".config/git");
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let granted = |access: Access| -> Result<Vec<std::path::PathBuf>, ProviderError> {
        let plan = fixture_access_plan(access, Some("login"), false);
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.home = home;
        let launch = crate::codex::Codex.prepare(input)?;
        Ok(launch
            .profile
            .spec
            .runtime_reads
            .iter()
            .filter(|grant| grant.path.starts_with(&git))
            .inspect(|grant| assert_eq!(grant.scope, PathScope::Exact))
            .map(|grant| grant.path.clone())
            .collect())
    };
    // Absent files are not granted, so a missing path cannot break a launch.
    assert!(granted(Access::WorktreeWrite)?.is_empty());
    std::fs::create_dir_all(&git)?;
    std::fs::write(git.join("ignore"), "")?;
    std::fs::write(git.join("attributes"), "")?;
    std::fs::write(git.join("config"), "")?;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let mut paths = granted(access)?;
        paths.sort();
        assert_eq!(paths, [git.join("attributes"), git.join("ignore")]);
    }
    Ok(())
}

#[test]
fn homebrew_node_tools_grant_the_prefix_but_not_var() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let prefix = temp.path();
    for name in ["bin", "Cellar", "etc", "var"] {
        std::fs::create_dir(prefix.join(name))?;
    }
    let (mut reads, mut exec) = (Vec::new(), Vec::new());
    let mut env = vec![(
        std::ffi::OsString::from("PATH"),
        std::ffi::OsString::from("/usr/bin:/bin"),
    )];
    crate::launch::homebrew_tools(prefix, &mut reads, &mut exec, &mut env);
    let paths = |grants: &[provenant_confinement::permissions::PathGrant]| {
        grants
            .iter()
            .map(|grant| grant.path.clone())
            .collect::<Vec<_>>()
    };
    // etc/ holds OpenSSL's configuration; var/ holds service data.
    assert_eq!(
        paths(&reads),
        ["bin", "Cellar", "etc"].map(|name| prefix.join(name))
    );
    assert_eq!(
        paths(&exec),
        ["bin", "Cellar"].map(|name| prefix.join(name))
    );
    assert_eq!(
        env[0].1,
        std::ffi::OsString::from(format!("/usr/bin:/bin:{}", prefix.join("bin").display()))
    );
    Ok(())
}

#[test]
fn resume_argv_carries_the_recorded_session() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let account_dir = std::path::Path::new("/fixture/identities/opaque");
    for (id, session, flag) in [
        ("claude", "5e55-claude-session", "--resume"),
        ("codex", "5e55-codex-thread", "resume"),
    ] {
        let adapter = REGISTRY
            .iter()
            .find(|entry| entry.adapter.id() == id)
            .expect("provider")
            .adapter;
        let route = route(id, "fixture-model");
        let continuation = Continuation::retained(id, session, &route, "fixture")?;
        let account = identity::RegisteredAccount::new(id, "opaque-account", account_dir.into())?;
        let token =
            (id == "claude").then(|| fixture_token().for_account("opaque-account", account_dir));
        let plan = fixture_access_plan(Access::ReadOnly, (id == "codex").then_some("login"), true);
        let mut input = fixture_input(
            &route,
            &plan,
            &prompt,
            std::path::Path::new("/fixture/repo"),
            token.as_ref(),
        );
        input.account = Some(&account);
        input.continuation = Some(&continuation);
        let launch = adapter.prepare(input)?;
        let argv: Vec<_> = launch
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect();
        let at = argv.iter().position(|arg| arg == flag).expect(flag);
        assert_eq!(argv[at + 1], session, "{id}");
    }
    Ok(())
}

#[test]
fn package_root_never_grants_a_hidden_home_tool_tree() {
    use crate::launch::package_root;
    let direct = Path::new("/home/u/.cargo/bin/codex");
    assert_eq!(
        package_root(direct).unwrap(),
        Path::new("/home/u/.cargo/bin")
    );
    let versioned = Path::new("/home/u/.local/share/claude/versions/2.1.310");
    assert_eq!(
        package_root(versioned).unwrap(),
        Path::new("/home/u/.local/share/claude/versions")
    );
    let cask = Path::new("/opt/homebrew/Caskroom/codex/0.159.0/bin/codex");
    assert_eq!(
        package_root(cask).unwrap(),
        Path::new("/opt/homebrew/Caskroom/codex/0.159.0")
    );
}

#[test]
fn registration_org_refuses_a_different_org_and_upgrades_legacy_records() {
    let mut entry = accounts::Registration {
        adapter: "claude".into(),
        label: "pro".into(),
        key: "k".into(),
        org_fingerprint: None,
        directory: "/home/u/.claude-pro".into(),
        plan: None,
        priority: None,
        billing_anchor: None,
        limits: None,
        display_name: None,
        hidden: false,
        order: None,
        headline: None,
    };
    assert_eq!(entry.check_org(Some("a")).unwrap().as_deref(), Some("a"));
    assert_eq!(entry.check_org(None).unwrap(), None);
    entry.org_fingerprint = Some("a".into());
    assert_eq!(entry.check_org(Some("a")).unwrap(), None);
    assert_eq!(entry.check_org(None).unwrap(), None);
    assert!(entry.check_org(Some("b")).is_err());
}
#[test]
fn codex_ignores_web_search_items_with_repeated_members() -> Result<(), ProviderError> {
    let mut stream = codex::Codex.stream(None)?;
    let raw = b"{\"type\":\"item.started\",\"item\":{\"id\":\"item_1\",\"type\":\"web_search\",\"id\":\"exec-1\",\"query\":\"\"}}\n{\"type\":\"item.completed\",\"item\":{\"id\":\"item_1\",\"type\":\"web_search\",\"id\":\"exec-1\",\"query\":\"q\"}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n";
    codex::Codex.observe(&mut stream, raw)?;
    assert_eq!(
        codex::Codex.finish(stream, EXIT)?.status(),
        OutcomeStatus::Success
    );
    Ok(())
}

#[test]
fn account_display_fields_validate() {
    use provenant_config::DomainConfig;
    let base = "[entries.claude-work]\nadapter = \"claude\"\nlabel = \"work\"\nkey = \"k\"\ndirectory = \"/home/u/.claude-work\"\n";
    let parse = |extra: &str| -> accounts::Settings {
        toml_edit::de::from_str(&format!("{base}{extra}")).unwrap()
    };
    assert!(
        parse("display_name = \"Work\"\nhidden = true\norder = 999\nheadline = \"5h\"\n")
            .validate()
            .is_empty()
    );
    for extra in [
        "display_name = \"\"\n",
        "display_name = \" pad\"\n",
        "display_name = \"a\\u0007b\"\n",
        "order = 0\n",
        "order = 1000\n",
    ] {
        let errors = parse(extra).validate();
        assert_eq!(errors.len(), 1, "{extra}");
        assert_eq!(errors[0].code, "display", "{extra}");
    }
    assert!(
        parse(&format!("display_name = \"{}\"\n", "x".repeat(65)))
            .validate()
            .len()
            == 1
    );
    assert!(
        toml_edit::de::from_str::<accounts::Settings>(&format!("{base}headline = \"daily\"\n"))
            .is_err()
    );
}
