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
        ("Individual quota reached. Resets in 1w", Some(604_800)),
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
fn agy_overload_is_typed_capacity_not_a_generic_failure() -> Result<(), ProviderError> {
    let exit = ExitObservation {
        code: Some(1),
        ..EXIT
    };
    let result = |error: &str| {
        serde_json::json!({"event":"result","result":{
            "conversation_id":"c","status":"ERROR","response":"","error":error,
            "usage":{"input_tokens":0,"output_tokens":0}}})
        .to_string()
            + "\n"
    };
    for message in [
        "The model is overloaded. Please try again later.",
        "503: The model is overloaded. Please try again later.",
        "503 Service Unavailable: The model is overloaded.",
        "UNAVAILABLE: model is overloaded",
    ] {
        let (events, outcome) = replay(&agy::Agy, &result(message), exit)?;
        assert_eq!(outcome.status(), OutcomeStatus::Refused, "{message}");
        assert!(
            events.contains(&ProviderEvent::ModelOverloaded {
                message: message.into()
            }),
            "{message}"
        );
    }
    for message in [
        "503",
        "upstream error",
        r#"Invalid prompt: "The model is overloaded""#,
    ] {
        let (events, outcome) = replay(&agy::Agy, &result(message), exit)?;
        assert_eq!(outcome.status(), OutcomeStatus::Failed, "{message}");
        assert!(events.contains(&ProviderEvent::Error), "{message}");
    }
    Ok(())
}
#[test]
fn codex_usage_limit_is_quota_limited() -> Result<(), ProviderError> {
    let raw = r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit.","codex_error_info":"usage_limit_exceeded"}}"#;
    let (events, outcome) = replay(&codex::Codex, &format!("{raw}\n"), EXIT)?;
    assert_eq!(outcome.status(), OutcomeStatus::Refused);
    assert!(events.contains(&ProviderEvent::QuotaLimited {
        message: "You've hit your usage limit.".into(),
        pool: None,
        resets_in_seconds: None,
    }));
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
        &[
            ProviderEvent::MessageStart,
            ProviderEvent::Text("kept".into())
        ]
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
        extra: Default::default(),
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
            extra: Default::default(),
            reason: provenant_records::RouteExclusionReason::Disabled,
        },
        confidentiality: provenant_records::RouteConfidentiality {
            extra: Default::default(),
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
fn claude_usage_comes_from_the_result_event() -> Result<(), ProviderError> {
    let stream = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
        "{\"type\":\"assistant\",\"session_id\":\"s\",\"parent_tool_use_id\":null,\"message\":{\"model\":\"claude-sonnet-5\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\",\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":40,\"cache_creation\":{\"ephemeral_5m_input_tokens\":15,\"ephemeral_1h_input_tokens\":25}}}\n",
    );
    let (events, _) = replay(&claude::Claude, stream, EXIT)?;
    let usage: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, ProviderEvent::Usage { .. }))
        .collect();
    assert_eq!(
        usage,
        [&ProviderEvent::Usage {
            input: 10,
            output: 20,
            cache_read: Some(30),
            cache_write: Some(40),
            cache_write_1h: Some(25),
        }]
    );
    Ok(())
}

#[test]
fn claude_success_that_stopped_for_another_reason_is_an_early_stop() -> Result<(), ProviderError> {
    let result = |extra: &str| {
        format!(
            "{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\"{extra}}}\n"
        )
    };
    let early = |extra: &str| -> Result<Vec<ProviderEvent>, ProviderError> {
        Ok(replay(&claude::Claude, &result(extra), EXIT)?
            .0
            .into_iter()
            .filter(|event| matches!(event, ProviderEvent::EarlyStop { .. }))
            .collect())
    };
    assert!(early(r#","stop_reason":"end_turn","terminal_reason":"completed""#)?.is_empty());
    assert!(early("")?.is_empty());
    assert!(early(r#","stop_reason":null"#)?.is_empty());
    assert_eq!(
        early(r#","stop_reason":"max_tokens","terminal_reason":"completed""#)?,
        [ProviderEvent::EarlyStop {
            reason: "stop_reason max_tokens".into()
        }]
    );
    assert_eq!(
        early(r#","stop_reason":"end_turn","terminal_reason":"max_turns""#)?,
        [ProviderEvent::EarlyStop {
            reason: "terminal_reason max_turns".into()
        }]
    );
    Ok(())
}

fn usage_events(events: &[ProviderEvent]) -> Vec<&ProviderEvent> {
    events
        .iter()
        .filter(|event| matches!(event, ProviderEvent::Usage { .. }))
        .collect()
}

#[test]
fn codex_cached_input_is_cache_read_and_leaves_uncached_input() -> Result<(), ProviderError> {
    // Shape of the recorded exec stream: `input_tokens` contains the cached part.
    let stream = concat!(
        "{\"type\":\"thread.started\",\"thread_id\":\"t1\"}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":13501,\"cached_input_tokens\":11520,\"cache_write_input_tokens\":1000,\"output_tokens\":5,\"reasoning_output_tokens\":0}}\n",
    );
    let (events, _) = replay(&codex::Codex, stream, EXIT)?;
    assert_eq!(
        usage_events(&events),
        [&ProviderEvent::Usage {
            input: 981,
            output: 5,
            cache_read: Some(11520),
            cache_write: Some(1000),
            cache_write_1h: None,
        }]
    );
    Ok(())
}

#[test]
fn codex_usage_without_cache_fields_stays_unknown_and_never_underflows() -> Result<(), ProviderError>
{
    let stream = concat!(
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":7,\"output_tokens\":2}}\n",
        // A cached count above the input cannot make the remainder negative.
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":5,\"cached_input_tokens\":9,\"output_tokens\":2}}\n",
    );
    let (events, _) = replay(&codex::Codex, stream, EXIT)?;
    assert_eq!(
        usage_events(&events),
        [
            &ProviderEvent::Usage {
                input: 7,
                output: 2,
                cache_read: None,
                cache_write: None,
                cache_write_1h: None,
            },
            &ProviderEvent::Usage {
                input: 0,
                output: 2,
                cache_read: Some(9),
                cache_write: None,
                cache_write_1h: None,
            },
        ]
    );
    Ok(())
}

#[test]
fn cursor_cache_tokens_are_recorded_beside_input() -> Result<(), ProviderError> {
    let stream = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\",\"usage\":{\"inputTokens\":10,\"outputTokens\":20,\"cacheReadTokens\":30,\"cacheWriteTokens\":40}}\n",
    );
    let (events, _) = replay(&cursor::Cursor, stream, EXIT)?;
    assert_eq!(
        usage_events(&events),
        [&ProviderEvent::Usage {
            input: 10,
            output: 20,
            cache_read: Some(30),
            cache_write: Some(40),
            cache_write_1h: None,
        }]
    );
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
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderEvent::Text(_)))
    );
    assert_eq!(outcome.status(), OutcomeStatus::Partial);
    let mut state = cursor::Cursor.stream(None)?;
    cursor::Cursor.observe(&mut state, fixture!("cursor/fresh.jsonl").as_bytes())?;
    // apiKeySource=login identifies a mechanism, not a non-secret account.
    assert_eq!(cursor::Cursor.account(&state)?, AccountDetection::Unknown);
    Ok(())
}

#[test]
fn registry_has_unique_ids_and_matching_quota_sources() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        REGISTRY.iter().map(|r| r.adapter.id()).collect::<Vec<_>>(),
        ["codex", "claude", "cursor", "agy", "opencode", "command"]
    );
    let ids: std::collections::BTreeSet<_> = REGISTRY.iter().map(|r| r.adapter.id()).collect();
    assert_eq!(ids.len(), REGISTRY.len(), "duplicate adapter id");
    assert_eq!(
        ids,
        provenant_records::ADAPTER_SPECS
            .iter()
            // Host-native delegates; the feature-gated fixture is injected by its test.
            .filter(|spec| spec.id != provenant_records::Harness::HostNative)
            .filter(|spec| spec.name != "fixture-provider")
            .map(|spec| spec.name)
            .collect()
    );
    for entry in REGISTRY {
        let opencode = entry.adapter.id() == "opencode";
        assert_eq!(
            entry.stdout_decoder.is_some(),
            matches!(entry.adapter.id(), "codex" | "claude" | "opencode")
        );
        assert_eq!(entry.stderr_decoder.is_some(), opencode);
        if let Some(source) = entry.quota_source {
            assert_eq!(source.adapter_id(), entry.adapter.id());
        } else {
            assert!(
                !provenant_records::AdapterSpec::get(entry.adapter.id())
                    .unwrap()
                    .launch
                    .model
            );
        }
    }
    Ok(())
}

#[test]
fn argv_prompt_limit_counts_utf8_bytes() {
    assert!(Prompt::new("a".repeat(65_536)).check_argv().is_ok());
    assert!(
        Prompt::new("a".repeat(65_537))
            .with_argv_limit(
                131_072,
                provenant_records::diagnostic::SettingSource::Configuration
            )
            .check_argv()
            .is_ok()
    );
    assert!(matches!(
        Prompt::new("é".repeat(32_769)).check_argv(),
        Err(ProviderError::PromptTooLarge { .. })
    ));
    let error = Prompt::new("abcd".into())
        .with_argv_limit(
            3,
            provenant_records::diagnostic::SettingSource::Configuration,
        )
        .check_argv()
        .expect_err("configured limit must be enforced");
    assert!(
        error
            .to_string()
            .contains("domains.run.argv_prompt_limit_bytes")
    );
    assert!(error.to_string().contains("host configuration"));
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
            "publishing": true,
            "common_git_dir": "/fixture/repo/.git",
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

pub(super) fn fixture_network_plan(
    access: Access,
    authentication: Option<&str>,
    network: provenant_confinement::permissions::NetworkAccess,
) -> provenant_confinement::ConfinedPlan {
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let plan = fixture_access_plan(access, authentication, false);
    let mut permissions = plan.permissions().clone();
    permissions.network = network;
    ConfinedPlan::new(PlanRequest {
        scope: plan.scope(),
        permissions,
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
    .expect("fixture network plan")
}

#[test]
fn claude_network_on_enables_web_tools_and_codex_toolchain_without_writer_grants()
-> Result<(), ProviderError> {
    use provenant_confinement::permissions::NetworkAccess;
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    for path in [
        ".cargo/bin",
        ".cargo/registry",
        ".cargo/git",
        ".rustup/toolchains",
    ] {
        std::fs::create_dir_all(home.join(path)).unwrap();
    }
    let repo = Path::new("/fixture/repo");
    let mut claude_route = route("claude", "fixture-model");
    claude_route.account_key = None;
    let mut codex_route = route("codex", "fixture-model");
    codex_route.account_key = None;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for network in [
            NetworkAccess::Provider,
            NetworkAccess::Local,
            NetworkAccess::Open,
        ] {
            let plan = fixture_network_plan(access, None, network);
            let launch = claude::Claude.prepare(PrepareInput {
                home: &home,
                ..fixture_input(&claude_route, &plan, &prompt, repo, Some(&token))
            })?;
            if access == Access::ReadOnly {
                for flag in ["--tools", "--allowedTools"] {
                    let list = launch.argv.windows(2).find(|pair| pair[0] == flag).unwrap();
                    let tools: Vec<_> = list[1].to_str().unwrap().split(',').collect();
                    for tool in ["WebFetch", "WebSearch"] {
                        assert_eq!(
                            tools.contains(&tool),
                            network == NetworkAccess::Open,
                            "{access:?} {network:?} {flag} {tool}"
                        );
                    }
                    if access == Access::ReadOnly {
                        assert!(!tools.contains(&"Edit") && !tools.contains(&"Write"));
                    }
                }
            } else {
                assert!(
                    launch
                        .argv
                        .windows(2)
                        .any(|pair| pair[0] == "--tools" && pair[1] == "default")
                );
                assert!(!launch.argv.iter().any(|arg| arg == "--allowedTools"));
            }
            assert!(launch.profile.spec.authentication.is_empty());
            assert!(launch.profile.spec.deny_keychain_access);
            assert!(launch.profile.spec.tool_writes.is_empty());
            let rendered = provenant_confinement::permissions::snapshot::render(
                plan.permissions(),
                &launch.profile,
                &home,
            )
            .unwrap();
            assert!(rendered.contains(&format!(
                "(deny file-read* (literal \"{}/.cargo/credentials.toml\"))",
                home.display()
            )));
            assert_eq!(
                rendered.contains(&format!(
                    "(allow file-read* (subpath \"{}/.config/gh\"))",
                    home.display()
                )),
                network == NetworkAccess::Open,
                "the existing gh policy still controls credential reads"
            );
            if access == Access::ReadOnly {
                for path in [
                    repo.to_owned(),
                    home.join(".cargo"),
                    home.join(".rustup"),
                    Path::new("/private/etc/ssl").to_owned(),
                ] {
                    assert!(!rendered.contains(&format!(
                        "(allow file-write* (subpath \"{}\"))",
                        path.display()
                    )));
                }
            }
            if access == Access::ReadOnly && network != NetworkAccess::Open {
                assert!(launch.env.iter().all(|(key, _)| key != "CARGO_HOME"));
                assert_eq!(
                    launch.env.iter().find(|(key, _)| key == "PATH").unwrap().1,
                    "/fixture/scratch/read-only-git:/fixture/scratch/bin:/usr/bin:/bin"
                );
                continue;
            }
            let codex_plan = fixture_network_plan(access, Some("login"), network);
            let codex = codex::Codex.prepare(PrepareInput {
                home: &home,
                ..fixture_input(&codex_route, &codex_plan, &prompt, repo, None)
            })?;
            for key in [
                "PATH",
                "CARGO_HOME",
                "CARGO_TARGET_DIR",
                "CARGO_BUILD_BUILD_DIR",
                "CARGO_NET_OFFLINE",
                "RUSTUP_HOME",
                "DEVELOPER_DIR",
                "SDKROOT",
                "npm_config_cache",
                "COREPACK_HOME",
            ] {
                let env = |spec: &LaunchSpec| {
                    spec.env
                        .iter()
                        .find(|(k, _)| k == key)
                        .map(|(_, value)| value.clone())
                };
                assert_eq!(env(&launch), env(&codex), "{access:?} {network:?} {key}");
            }
            let package = Path::new("/opt/homebrew/Cellar/provider/1.0");
            for grant in &codex.profile.spec.runtime_reads {
                assert!(
                    launch.profile.spec.runtime_reads.contains(grant),
                    "missing read: {grant:?}"
                );
            }
            for grant in &codex.profile.spec.tool_exec_roots {
                if grant.path != package {
                    assert!(
                        launch.profile.spec.tool_exec_roots.contains(grant),
                        "missing exec: {grant:?}"
                    );
                }
            }
            for secret in [
                ".config/gh",
                "Library/Keychains",
                ".codex/auth.json",
                ".claude/.credentials.json",
            ] {
                assert!(
                    !launch
                        .profile
                        .spec
                        .runtime_reads
                        .iter()
                        .any(|grant| covers(grant, &home.join(secret))),
                    "new credential read: {secret}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn claude_network_on_without_complete_rust_keeps_public_system_tools()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::NetworkAccess;
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    for installed in [None, Some(".cargo/bin"), Some(".rustup/toolchains")] {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        if let Some(path) = installed {
            std::fs::create_dir_all(home.join(path))?;
        }
        let plan = fixture_network_plan(Access::ReadOnly, None, NetworkAccess::Open);
        let launch = claude::Claude.prepare(PrepareInput {
            home: &home,
            ..fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                Some(&token),
            )
        })?;
        assert!(launch.env.iter().all(|(key, _)| key != "CARGO_HOME"));
        assert!(
            launch
                .profile
                .spec
                .tool_exec_roots
                .contains(&crate::launch::subtree("/fixture/repo"))
        );
        assert!(launch.profile.spec.tool_writes.is_empty());
        for grant in launch
            .profile
            .spec
            .runtime_reads
            .iter()
            .chain(&launch.profile.spec.tool_exec_roots)
        {
            assert!(
                !grant.path.starts_with(home.join(".cargo"))
                    && !grant.path.starts_with(home.join(".rustup"))
            );
        }
        #[cfg(target_os = "macos")]
        assert!(
            launch
                .profile
                .spec
                .runtime_reads
                .contains(&crate::launch::subtree("/private/etc/ssl"))
        );
    }
    Ok(())
}

/// A host without Rust still lets every shell lane run checked-in scripts,
/// read Git's global ignore and attributes, and gives writers Git's helpers.
#[test]
fn rust_less_hosts_keep_repository_execution_in_the_rendered_profile()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::{NetworkAccess, snapshot::render};
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let git = home.join(".config/git");
    std::fs::create_dir_all(&git)?;
    std::fs::write(git.join("ignore"), "")?;
    let core = home.join("Cellar/git/1.0/libexec/git-core");
    std::fs::create_dir_all(&core)?;
    let git_executable = home.join("Cellar/git/1.0/bin/git");
    for (adapter, access) in [
        ("claude", Access::ReadOnly),
        ("codex", Access::ReadOnly),
        ("codex", Access::WorktreeWrite),
    ] {
        let mut route = route(adapter, "fixture-model");
        route.account_key = None;
        let authentication = (adapter == "codex").then_some("login");
        let plan = fixture_network_plan(access, authentication, NetworkAccess::Open);
        let mut input = fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            (adapter == "claude").then_some(&token),
        );
        input.home = &home;
        input.provenant.git = &git_executable;
        let launch = match adapter {
            "claude" => claude::Claude.prepare(input)?,
            _ => crate::codex::Codex.prepare(input)?,
        };
        assert!(launch.env.iter().all(|(key, _)| key != "CARGO_HOME"));
        let spec = &launch.profile.spec;
        assert!(
            spec.runtime_reads
                .contains(&crate::launch::exact(git.join("ignore"))),
            "{adapter} {access:?}"
        );
        assert_eq!(
            spec.tool_exec_roots
                .contains(&crate::launch::subtree(&core)),
            access == Access::WorktreeWrite,
            "{adapter} {access:?}"
        );
        let profile = render(plan.permissions(), &launch.profile, &home)?;
        assert!(
            profile.contains(if access == Access::WorktreeWrite {
                "(allow process-exec)"
            } else {
                "(allow process-exec (subpath \"/fixture/repo\"))"
            }),
            "{adapter} {access:?}: repository execution missing from\n{profile}"
        );
    }
    Ok(())
}
