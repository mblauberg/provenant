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
    let mut state = adapter.stream();
    let mut events = Vec::new();
    // Split inside UTF-8 and JSON tokens, not just at event boundaries.
    for chunk in bytes.as_bytes().chunks(17) {
        events.extend(adapter.observe(&mut state, chunk)?);
    }
    Ok((events, adapter.finish(state, exit)?))
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
    let codex = replay(&codex::Codex, fixture!("codex/success.jsonl"), EXIT)?.1;
    assert!(matches!(
        codex::Codex.continuation(&codex, &self::route("codex", "requested"), "grants"),
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
        let mut state = codex::Codex.stream();
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
    let mut state = codex::Codex.stream();
    assert!(matches!(
        codex::Codex.observe(&mut state, &vec![b'x'; 1_048_577]),
        Err(ProviderError::Overflow)
    ));
    assert_eq!(
        codex::Codex.finish(state, EXIT)?.status(),
        OutcomeStatus::Malformed
    );
    let mut state = codex::Codex.stream();
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
    let mut state = claude::Claude.stream();
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
    let mut state = cursor::Cursor.stream();
    cursor::Cursor.observe(&mut state, fixture!("cursor/fresh.jsonl").as_bytes())?;
    // apiKeySource=login identifies a mechanism, not a non-secret account.
    assert_eq!(cursor::Cursor.account(&state)?, AccountDetection::Unknown);
    Ok(())
}

#[test]
fn registry_refuses_every_disabled_mode_and_quota_source() -> Result<(), Box<dyn std::error::Error>>
{
    #[derive(Deserialize)]
    struct Matrix {
        execution_modes: Vec<Mode>,
        quota_sources: Vec<Source>,
    }
    #[derive(Deserialize)]
    struct Mode {
        adapter: String,
        enabled: bool,
    }
    #[derive(Deserialize)]
    struct Source {
        adapter: String,
        enabled: bool,
    }
    let matrix: Matrix = serde_json::from_str(fixture!("activation.json"))?;
    assert_eq!(
        REGISTRY
            .iter()
            .map(|r| r.adapter.id())
            .collect::<std::collections::BTreeSet<_>>(),
        ["codex", "claude", "cursor", "agy", "opencode"]
            .into_iter()
            .collect()
    );
    for mode in matrix.execution_modes {
        let entry = REGISTRY
            .iter()
            .find(|r| r.adapter.id() == mode.adapter)
            .ok_or("missing registration")?;
        assert!(!mode.enabled);
        assert!(matches!(
            entry.adapter.preflight(&PreflightInput {
                route: route(&mode.adapter, "fixture"),
                observed_unix_seconds: 0
            }),
            Err(ProviderError::Disabled { .. })
        ));
    }
    for source in matrix.quota_sources {
        assert!(!source.enabled);
        let entry = REGISTRY
            .iter()
            .find(|r| r.adapter.id() == source.adapter)
            .ok_or("missing registration")?;
        if let Some(source) = entry.quota_source {
            assert_eq!(entry.adapter.id(), source.adapter_id());
            let account = provenant_quota::AccountScope {
                adapter: source.adapter_id().into(),
                account_key: "opaque".into(),
                label: "fixture".into(),
            };
            assert!(matches!(
                source.probe(
                    &account,
                    &provenant_quota::QuotaProbeInput {
                        now_unix_seconds: 0,
                        deadline_unix_seconds: 1,
                        authority_ref: None
                    }
                ),
                Err(provenant_quota::QuotaError::Disabled)
            ));
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

#[test]
fn prepare_refuses_unrecorded_launches_and_checks_argv_bound() -> Result<(), ProviderError> {
    use provenant_confinement::permissions::{ConfinedLaunch, Enforcement};
    let confined = ConfinedLaunch {
        enforcement: Enforcement::MacOsSandboxExec,
        profile: Vec::new(),
        profile_digest: [0; 32],
        native_controls: Vec::new(),
    };
    let prompt = Prompt::new("fixture prompt".into());
    for entry in REGISTRY {
        let route = route(entry.adapter.id(), "fixture");
        assert!(matches!(
            entry.adapter.prepare(PrepareInput {
                route: &route,
                confined: &confined,
                prompt: &prompt,
                continuation: None,
                grant_digest: "fixture"
            }),
            Err(ProviderError::Disabled { .. })
        ));
        let oversized = Prompt::new("a".repeat(65_537));
        let result = entry.adapter.prepare(PrepareInput {
            route: &route,
            confined: &confined,
            prompt: &oversized,
            continuation: None,
            grant_digest: "fixture",
        });
        if matches!(entry.adapter.id(), "cursor" | "agy" | "opencode") {
            assert!(matches!(result, Err(ProviderError::PromptTooLarge)));
        } else {
            assert!(matches!(result, Err(ProviderError::Disabled { .. })));
        }
    }
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
