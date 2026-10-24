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

pub(super) fn read_only_plan(
    root: &str,
    authentication: Option<&str>,
) -> provenant_confinement::ConfinedPlan {
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
            read_only: None,
            mode: AccessMode::ReadOnly,
            scratch: grant("scratch"),
            private_state: Some(grant("state")),
            read_roots: vec![grant("repo")],
            protected_roots: vec![],
            secret_roots: vec![],
            selected_authentication: authentication.map(Into::into),
            include_continuation: false,
            network: vec![],
            capabilities: vec![],
            nested_confinement: false,
        },
    })
    .expect("fixture plan")
}

fn fixture_access_plan(
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

fn fixture_token() -> crate::claude::OauthToken {
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
        account: None,
        token,
        cwd,
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
fn prepare_follows_the_activation_matrix_and_pins_launch_controls() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let repo = std::path::Path::new("/fixture/repo");
    let token = fixture_token();
    for entry in REGISTRY {
        let id = entry.adapter.id();
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        // Only Codex reads a credential inside the sandbox.
        let confined = read_only_plan("/fixture", (id == "codex").then_some("login"));
        let token = (id == "claude").then_some(&token);
        let result = entry
            .adapter
            .prepare(fixture_input(&route, &confined, &prompt, repo, token));
        // Recording builds may prepare disabled modes to gather their evidence.
        if !mode_enabled(id, Access::ReadOnly, Session::Fresh) {
            assert!(
                matches!(result, Err(ProviderError::Disabled { .. }))
                    || (cfg!(feature = "recording") && result.is_ok()),
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
            "codex" => &[
                "CARGO_HOME",
                "RUSTUP_HOME",
                "CARGO_TARGET_DIR",
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
                .position(|key| *key == "CARGO_TARGET_DIR")
                .expect("cargo env")
                + 1;
            expected.splice(index..index, ["DEVELOPER_DIR", "SDKROOT"]);
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
        // prefix whose etc/ and var/ would become readable.
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
                ["bin", "Cellar", "opt", "lib"]
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
                        || grant.path == Path::new("/private/etc/ssl/openssl.cnf")))
                || [
                    "/fixture/provenant/bin/provenant",
                    "/fixture/git/bin/git",
                    "/fixture/provenant/release.json",
                    "/fixture/state/config.toml",
                    "/fixture/state/host-id",
                    "/fixture/state/projects",
                    "/fixture/state/routing",
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
    #[serde(default)]
    writer_shell_proof: bool,
    #[serde(default)]
    subprocess_env_scrub: bool,
    #[serde(default)]
    cli_version: String,
    #[serde(default)]
    keychain_ipc_proof: bool,
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
        #[serde(default)]
        shell_enabled: bool,
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
            if mode.adapter == "claude" && mode.shell_enabled {
                assert!(
                    capture.writer_shell_proof,
                    "{reference}: Bash must have run"
                );
                assert!(
                    capture.subprocess_env_scrub,
                    "{reference}: scrub must be enabled"
                );
                assert!(
                    !capture.cli_version.is_empty(),
                    "{reference}: CLI version required"
                );
            }
            if capture.keychain_ipc_proof {
                assert!(capture.writer_shell_proof, "{reference}");
            }
            if capture.writer_shell_proof {
                assert_eq!(mode.access_mode, Access::WorktreeWrite);
                assert!(
                    capture
                        .argv
                        .iter()
                        .any(|arg| arg.split(',').any(|tool| tool == "Bash")),
                    "{reference}"
                );
            }
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

impl provenant_supervisor::exec::Executor for KeychainScript {
    fn execute(
        &self,
        options: provenant_supervisor::exec::ExecOptions<'_>,
    ) -> Result<provenant_supervisor::exec::ExecOutcome, provenant_supervisor::exec::ExecError>
    {
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
        Ok(provenant_supervisor::exec::ExecOutcome {
            termination: provenant_supervisor::exec::Termination::Exit(0),
            stdout: provenant_supervisor::exec::CapturedStream {
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
    let run_by = |config: &str, items: Vec<Vec<u8>>, deadline: u64| {
        let script = KeychainScript {
            items: std::sync::Mutex::new(items.into()),
            statuses: Default::default(),
            calls: std::sync::Mutex::new(vec![]),
        };
        let login = Login {
            executable: Path::new("/opt/homebrew/bin/claude"),
            home: Path::new("/home/u"),
            user: OsStr::new("u"),
            config_dir: Path::new(config),
            scratch: Path::new("/scratch/empty"),
        };
        let result = refresh_if_expiring(&script, &login, &|| now, deadline, None);
        (result, script.calls.into_inner().unwrap())
    };
    let hour = 3_600_000;
    let run = |config: &str, items: Vec<Vec<u8>>| run_by(config, items, now + hour);

    // A spent deadline runs nothing.
    let (token, calls) = run_by("/home/u/.claude", vec![], now);
    assert!(matches!(token, Err(ProviderError::CredentialUnavailable)));
    assert!(calls.is_empty());

    // A live token is used as read: no refresh, and Debug never shows it.
    let (token, calls) = run("/home/u/.claude", vec![item("fixture-access", now + hour)]);
    let token = token.expect("live token");
    let printed = format!("{token:?}");
    assert!(!printed.contains("fixture-access") && !printed.contains("fixture-refresh"));
    assert_eq!(calls.len(), 1);
    let (argv, env, cwd) = &calls[0];
    assert_eq!(
        argv[1..],
        [
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-a",
            "u",
            "-w"
        ]
    );
    assert!(!env.iter().any(|v| v.starts_with("CLAUDE_CONFIG_DIR=")));
    assert_eq!(cwd, Path::new("/scratch/empty"));

    // Inside the window the CLI refreshes once, in the empty scratch directory
    // with hooks, MCP servers, slash commands and session files off.
    let expiring = item("fixture-access", now + 14 * 60 * 1000);
    let (token, calls) = run(
        "/home/u/.claude",
        vec![expiring.clone(), item("fixture-renewed", now + hour)],
    );
    token.expect("refreshed token");
    let (argv, _, cwd) = &calls[1];
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
    assert_eq!(cwd, Path::new("/scratch/empty"));
    assert_eq!(calls.len(), 3);

    // The CLI may keep a token inside the window; one with a minute left is
    // accepted, one about to expire is refused.
    let (token, _) = run("/home/u/.claude", vec![expiring.clone(), expiring.clone()]);
    token.expect("still usable");
    let (token, _) = run(
        "/home/u/.claude",
        vec![expiring, item("fixture-access", now + 30_000)],
    );
    assert!(matches!(token, Err(ProviderError::CredentialUnavailable)));

    // Another identity's directory names its own keychain item and config.
    let (token, calls) = run(
        "/home/u/.claude-work",
        vec![b"not json".to_vec(), b"not json".to_vec()],
    );
    assert!(matches!(token, Err(ProviderError::CredentialUnavailable)));
    let (argv, env, _) = &calls[0];
    let suffix = crate::claude::keychain_suffix(b"/home/u/.claude-work");
    assert_eq!(argv[3], format!("Claude Code-credentials-{suffix}"));
    assert!(env.contains(&"CLAUDE_CONFIG_DIR=/home/u/.claude-work".to_owned()));
    assert!(
        calls[1]
            .1
            .contains(&"CLAUDE_CONFIG_DIR=/home/u/.claude-work".to_owned())
    );
}

#[test]
fn codex_read_only_toolchain_uses_private_build_and_existing_registry() {
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
    assert_eq!(env("CARGO_HOME"), "/fixture/scratch/cargo-home");
    assert_eq!(env("RUSTUP_HOME"), "/fixture/home/.rustup");
    for dir in [
        "/fixture/home/.cargo/bin",
        "/fixture/home/.rustup/toolchains",
    ] {
        assert!(
            spec.profile
                .spec
                .runtime_reads
                .iter()
                .any(|g| g.path == std::path::Path::new(dir))
        );
        assert!(
            spec.profile
                .spec
                .tool_exec_roots
                .iter()
                .any(|g| g.path == std::path::Path::new(dir))
        );
    }
    assert!(spec.setup.contains(&crate::launch::SetupStep::Symlink {
        link: "/fixture/scratch/cargo-home/registry".into(),
        target: "/fixture/home/.cargo/registry".into(),
    }));
    assert!(!spec.profile.spec.runtime_reads.iter().any(|g| g.path.ends_with(".cargo") || g.path.to_string_lossy().contains("credentials")));
}

#[test]
fn provider_recipes_cannot_write_user_configuration() -> Result<(), ProviderError> {
    use provenant_confinement::permissions::{PathGrant, PathScope};
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
    let covers = |grant: &PathGrant, path: &std::path::Path| match grant.scope {
        PathScope::Exact => grant.path == path,
        PathScope::Subtree => path.starts_with(&grant.path),
    };
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
        // Synthetic continuation for path-policy coverage, including disabled
        // adapters without answering-model evidence. This enables no mode.
        let continuation = Continuation::from_outcome(id, &outcome, &route, "fixture", false)?;
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            for session in [Session::Fresh, Session::Resume] {
                let plan = fixture_access_plan(
                    access,
                    (id == "codex").then_some("login"),
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
                let launch = match adapter.prepare(input) {
                    Ok(launch) => launch,
                    Err(ProviderError::Disabled { .. }) => {
                        assert!(!mode_enabled(id, access, session));
                        continue;
                    }
                    Err(error) => return Err(error),
                };
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
                pool: Some("opencode-free".into())
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
            pool: None
        }));
    }
    Ok(())
}

#[test]
fn selected_accounts_control_provider_environment_and_exact_authentication_grants()
-> Result<(), Box<dyn std::error::Error>> {
    use std::path::Path;
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
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
            let dir = Path::new("/fixture/identities").join(key);
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
                        scratch: Path::new("/fixture/scratch"),
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
        ("CARGO_HOME", "/fixture/scratch/cargo-home"),
        (
            "CARGO_TARGET_DIR",
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
    // The existing registry is linked read-only into scratch Cargo home, and the
    // user's Cargo configuration and credentials are not granted.
    assert!(launch.setup.contains(&crate::launch::SetupStep::Symlink {
        link: "/fixture/scratch/cargo-home/registry".into(),
        target: "/fixture/home/.cargo/registry".into(),
    }));
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
    assert!(launch.profile.spec.runtime_reads.iter().all(|grant| {
        !grant.path.ends_with(".cargo") && !grant.path.to_string_lossy().contains("credentials")
    }));
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
        Path::new("/scratch/empty"),
    );
    assert!(key.is_ok());
    let calls = script.calls.into_inner().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1.iter().any(|entry| entry == "USER=u"));
}

#[test]
fn claude_account_probe_names_the_missing_login() {
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
        Path::new("/scratch/empty"),
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
fn homebrew_node_tools_grant_toolchain_directories_but_not_etc_or_var() {
    let prefix = std::path::Path::new("/opt/homebrew");
    let (mut reads, mut exec) = (Vec::new(), Vec::new());
    let mut env = vec![(
        std::ffi::OsString::from("PATH"),
        std::ffi::OsString::from("/usr/bin:/bin"),
    )];
    crate::launch::homebrew_tools(prefix, &mut reads, &mut exec, &mut env);
    let paths: Vec<_> = reads.iter().map(|grant| grant.path.clone()).collect();
    assert_eq!(
        paths,
        ["bin", "Cellar", "opt", "lib"].map(|name| prefix.join(name))
    );
    assert_eq!(reads, exec);
    assert!(
        !paths
            .iter()
            .any(|path| path.ends_with("etc") || path.ends_with("var"))
    );
    assert_eq!(env[0].1, "/usr/bin:/bin:/opt/homebrew/bin");
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
    };
    assert_eq!(entry.check_org(Some("a")).unwrap().as_deref(), Some("a"));
    assert_eq!(entry.check_org(None).unwrap(), None);
    entry.org_fingerprint = Some("a".into());
    assert_eq!(entry.check_org(Some("a")).unwrap(), None);
    assert_eq!(entry.check_org(None).unwrap(), None);
    assert!(entry.check_org(Some("b")).is_err());
}
