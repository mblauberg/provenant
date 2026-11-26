use super::*;
use crate::Adapter;

// Spec: docs/specs/routing.md#capacity-is-not-quota
#[test]
fn unknown_error_without_http_status_preserves_unclassified_message() {
    let mut stream = OpenCode
        .stream(None, &provenant_records::policy::Policy::default())
        .unwrap();
    let events = OpenCode.observe(&mut stream, b"{\"type\":\"error\",\"error\":{\"type\":\"unknown\",\"message\":\"fixture failure\"}}\n").unwrap();
    assert!(events.contains(&E::Unclassified {
        message: "fixture failure".into()
    }));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, E::QuotaLimited { .. }))
    );
}

#[test]
fn structured_errors_classify_auth_and_quota() {
    for (error, auth) in [
        (serde_json::json!({"type":"provider.auth"}), true),
        (serde_json::json!({"type":"unknown", "status":401}), true),
        (serde_json::json!({"type":"unknown", "status":403}), true),
        (
            serde_json::json!({"type":"unknown", "response":{"body":"{\"error\":{\"code\":\"api_key_expired\"}}"}}),
            true,
        ),
        (serde_json::json!({"type":"unknown", "status":429}), false),
        (
            serde_json::json!({"type":"unknown", "response":{"body":{"error":{"code":"insufficient_balance"}}}}),
            false,
        ),
    ] {
        let event = serde_json::json!({"type":"error", "error":error}).to_string();
        let events = decode(event.as_bytes()).unwrap();
        assert!(events.iter().any(|event| if auth {
            matches!(event, E::AuthRequired)
        } else {
            matches!(event, E::QuotaLimited { .. })
        }));
    }
}

#[test]
fn live_stderr_quota_requires_error_class_at_line_start() {
    use provenant_os::capture::{FrameDecoder, FrameEvent};
    let decoder = super::StderrQuota;
    assert_eq!(
        decoder
            .decode(b"AI.Error.QuotaExceeded: Go usage limit exceeded")
            .unwrap(),
        FrameEvent::Terminal
    );
    assert_eq!(
        decoder
            .decode(b"\x1b[31mAI.Error.QuotaExceeded: limit")
            .unwrap(),
        FrameEvent::Terminal
    );
    for line in [
        "Rate limit exceeded, retrying in 5 seconds",
        "Rate limit exceeded, retrying in 10 seconds",
    ] {
        assert_eq!(
            decoder.decode(line.as_bytes()).unwrap(),
            FrameEvent::Terminal
        );
        assert!(matches!(
            stderr_quota(line.as_bytes()),
            Some(E::QuotaLimited { .. })
        ));
    }
    for text in [
        "The documentation mentions quota and usage limit exceeded",
        "example AI.Error.QuotaExceeded: Go usage limit exceeded",
        "AI.Error.QuotaExceeded elsewhere in prose",
    ] {
        assert_eq!(
            decoder.decode(text.as_bytes()).unwrap(),
            FrameEvent::Progress
        );
    }
}

#[test]
fn stderr_quota_forms_and_retry() {
    use super::{E, stderr_quota};
    for (line, pool, reset) in [
        (
            "AI.Error.QuotaExceeded: Go usage limit exceeded",
            "opencode-go",
            None,
        ),
        (
            "free tier usage limit exceeded retryAfterMs=1500",
            "opencode-free",
            Some(2),
        ),
        (
            "AI.Error.QuotaExceeded\nretry-after: 60",
            "opencode-go",
            Some(60),
        ),
    ] {
        assert!(
            matches!(stderr_quota(line.as_bytes()), Some(E::QuotaLimited { pool: Some(actual), resets_in_seconds, .. }) if actual == pool && resets_in_seconds == reset)
        );
    }
    assert!(stderr_quota(b"ordinary failure").is_none());
}

#[test]
fn stderr_quota_scopes_pool_and_retry_to_the_matched_line() {
    use super::{E, stderr_quota};
    let quota = |text: &str| match stderr_quota(text.as_bytes()) {
        Some(E::QuotaLimited {
            pool,
            resets_in_seconds,
            ..
        }) => (pool, resets_in_seconds),
        other => panic!("unexpected {other:?}"),
    };
    // Unrelated "free" and retry text elsewhere must not steer the result.
    let (pool, reset) = quota(
        "using free model fallback\nretry after 5 seconds\nAI.Error.QuotaExceeded: Go usage limit exceeded\nstack\nretryAfterMs=9000\nfree tier\n",
    );
    assert_eq!(pool.as_deref(), Some("opencode-go"));
    assert_eq!(reset, None);
    // Units are honoured; a bare number is seconds only in the header form.
    assert_eq!(
        quota("usage limit exceeded, retry after 30 minutes").1,
        Some(1800)
    );
    assert_eq!(
        quota("usage limit exceeded, retry after 30 sec").1,
        Some(30)
    );
    assert_eq!(quota("usage limit exceeded, retry after 30").1, None);
    assert_eq!(
        quota("usage limit exceeded\nretry-after: 30 minutes").1,
        Some(1800)
    );
    assert_eq!(
        quota("usage limit exceeded\nretry-after: 30 fortnights").1,
        None
    );
    assert_eq!(
        quota("Free usage limit exceeded").0.as_deref(),
        Some("opencode-free")
    );
}

#[test]
fn unavailable_model_is_a_typed_refusal() {
    let mut stream = OpenCode
        .stream(None, &provenant_records::policy::Policy::default())
        .unwrap();
    let mut events = Vec::new();
    for chunk in
        include_bytes!("../../../tests/fixtures/providers/opencode/model-unavailable.jsonl")
            .chunks(17)
    {
        events.extend(OpenCode.observe(&mut stream, chunk).unwrap());
    }
    assert!(events.contains(&E::ModelUnavailable {
        message: "Model unavailable: opencode/misspelt-free".into(),
    }));
    assert!(events.contains(&E::Session("ses_x".into())));
    let outcome = OpenCode
        .finish(
            stream,
            crate::ExitObservation {
                code: Some(1),
                timed_out: false,
            },
        )
        .unwrap();
    assert_eq!(outcome.status(), crate::OutcomeStatus::Refused);
    assert_eq!(outcome.session(), Some("ses_x"));
}

// Spec: docs/specs/routing.md#capacity-is-not-quota
#[test]
fn upstream_5xx_is_capacity_not_a_generic_error_or_quota_hold() {
    for (raw, message) in [
            (
                br#"{"type":"error","error":{"type":"provider.internal","status":529,"message":"Overloaded"}}"#.as_slice(),
                "Overloaded",
            ),
            (
                br#"{"type":"error","error":{"type":"provider.internal","status":500,"message":"Overloaded"}}"#.as_slice(),
                "Overloaded",
            ),
            (
                br#"{"type":"error","error":{"type":"provider.internal","status":500,"message":"Upstream request failed: Endpoint is unavailable."}}"#.as_slice(),
                "Upstream request failed: Endpoint is unavailable.",
            ),
        ] {
            assert_eq!(
                decode(raw).unwrap(),
                vec![E::ModelOverloaded {
                    message: message.into()
                }]
            );
        }
    // Only the typed provider.internal kind is capacity; an untyped error
    // with a 5xx status stays generic.
    assert_eq!(
        decode(
            br#"{"type":"error","error":{"type":"unknown","status":529,"message":"Overloaded"}}"#
        )
        .unwrap(),
        vec![E::Error]
    );
    let events = decode(br#"{"type":"error","sessionID":"ses_x","error":{"type":"provider.internal","status":529,"message":"Overloaded"}}"#).unwrap();
    assert_eq!(
        events,
        vec![
            E::Session("ses_x".into()),
            E::ModelOverloaded {
                message: "Overloaded".into()
            }
        ]
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, E::QuotaLimited { .. }))
    );
}

// Spec: docs/specs/telemetry.md#usage-merges-by-declared-semantics
#[test]
fn step_finish_reports_keyed_usage_and_provider_cost() {
    let frames = [
        r#"{"type":"step_finish","sessionID":"ses_x","part":{"id":"step-1","reason":"tool-calls","tokens":{"input":9276,"output":3,"reasoning":0,"cache":{"read":1792,"write":0}},"cost":0.5}}"#,
        r#"{"type":"step_finish","sessionID":"ses_x","part":{"id":"step-2","reason":"stop","tokens":{"input":10,"output":4},"cost":0.25}}"#,
    ];
    // The adapter decodes one line at a time; StreamState splits the lines.
    let events: Vec<_> = frames
        .iter()
        .flat_map(|frame| decode(frame.as_bytes()).unwrap())
        .collect();
    let reports: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            E::Usage(report) => Some(report.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].scope, provenant_records::UsageScope::Step);
    assert_eq!(reports[0].key.as_deref(), Some("step-1"));
    assert_eq!(reports[0].tokens.input, Some(9276));
    assert_eq!(reports[0].tokens.cache_read, Some(1792));
    assert_eq!(reports[0].tokens.reasoning, Some(0));
    let cost = reports[0].cost.as_ref().expect("stated cost");
    assert_eq!(cost.micros, 500_000);
    assert_eq!(cost.currency, "USD");
    assert_eq!(cost.stated_by, CostOrigin::Provider);
    assert_eq!(reports[1].key.as_deref(), Some("step-2"));

    let semantics = provenant_records::AdapterSpec::get("opencode")
        .expect("declared adapter")
        .launch
        .usage;
    let mut accumulator = crate::UsageAccumulator::new(semantics);
    for report in &reports {
        accumulator.push(report.clone());
    }
    let (tokens, cost) = accumulator.total();
    assert_eq!(tokens.input, Some(9286));
    assert_eq!(tokens.output, Some(7));
    assert_eq!(tokens.cache_read, Some(1792));
    assert_eq!(tokens.cache_write, Some(0));
    assert_eq!(cost.expect("cost").micros, 250_000);
}

// Spec: docs/specs/telemetry.md#usage-merges-by-declared-semantics
#[test]
fn step_finish_without_tokens_leaves_usage_unknown() {
    for raw in [
            br#"{"type":"step_finish","sessionID":"ses_x","part":{"reason":"stop"}}"#.as_slice(),
            br#"{"type":"step_finish","sessionID":"ses_x","part":{"id":"step-1","reason":"tool-calls","cost":0}}"#.as_slice(),
        ] {
            let events = decode(raw).unwrap();
            assert!(events.iter().all(|event| !matches!(event, E::Usage(_))));
        }
}

// Spec: docs/specs/routing.md#capacity-is-not-quota
#[test]
fn rate_limit_retry_ends_the_attempt_with_a_reset() {
    use crate::ExitObservation;
    use provenant_os::capture::{FrameDecoder, FrameEvent};
    let frame = concat!(
        r#"{"type":"retry","attempt":1,"error":{"type":"provider.rate-limit","#,
        r#""status":429,"message":"rate limited","retryAfterMs":30000}}"#,
        "\n",
    );
    let events = decode(frame.as_bytes()).unwrap();
    assert_eq!(
        events,
        vec![E::QuotaLimited {
            message: "rate limited".into(),
            pool: None,
            resets_in_seconds: Some(30),
        }]
    );
    assert_eq!(
        OpenCode.decode(frame.as_bytes()).unwrap(),
        FrameEvent::Terminal
    );

    let mut stream = OpenCode
        .stream(None, &provenant_records::policy::Policy::default())
        .unwrap();
    OpenCode.observe(&mut stream, frame.as_bytes()).unwrap();
    let outcome = OpenCode
        .finish(
            stream,
            ExitObservation {
                code: None,
                timed_out: true,
            },
        )
        .unwrap();
    assert_eq!(outcome.status(), crate::OutcomeStatus::Refused);
}

// Spec: docs/specs/routing.md#capacity-is-not-quota
#[test]
fn capacity_retry_loop_ends_at_the_bound() {
    use provenant_os::capture::{FrameDecoder, FrameEvent};
    let retry = |attempt| {
        format!(
            "{{\"type\":\"retry\",\"attempt\":{attempt},\"error\":{{\"type\":\"provider.internal\",\"status\":500,\"message\":\"Upstream request failed\"}}}}\n"
        )
    };
    for attempt in 1..CAPACITY_RETRY_BOUND {
        let raw = retry(attempt);
        assert_eq!(decode(raw.as_bytes()).unwrap(), vec![E::TransientError]);
        assert_eq!(
            OpenCode.decode(raw.as_bytes()).unwrap(),
            FrameEvent::Progress
        );
    }
    // An untyped provider.internal retry (no 5xx status, no attempt) is
    // transient too: one internal retry does not end the attempt.
    let plain = br#"{"type":"retry","error":{"type":"provider.internal","message":"internal"}}"#;
    assert_eq!(decode(plain).unwrap(), vec![E::TransientError]);
    assert_eq!(OpenCode.decode(plain).unwrap(), FrameEvent::Progress);

    let raw = retry(CAPACITY_RETRY_BOUND);
    let events = decode(raw.as_bytes()).unwrap();
    assert_eq!(
        events,
        vec![E::ModelOverloaded {
            message: "Upstream request failed".into()
        }]
    );
    assert_eq!(
        OpenCode.decode(raw.as_bytes()).unwrap(),
        FrameEvent::Terminal
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, E::QuotaLimited { .. }))
    );
}

#[derive(Clone, Copy)]
enum ModelSource {
    Stream(&'static [u8]),
    SessionV2,
}

struct ModelCase {
    session: &'static str,
    model: &'static str,
    expected: &'static str,
    source: ModelSource,
    live_wal: bool,
}

/// Seed only the current session and assistant-turn tables.
fn seed_v2_only(dir: &std::path::Path, session: &str, provider: &str, model: &str) {
    let db = rusqlite::Connection::open(dir.join("opencode.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_v2 (id TEXT PRIMARY KEY); \
             CREATE TABLE IF NOT EXISTS session_message (session_id TEXT, type TEXT, data TEXT)",
    )
    .unwrap();
    db.execute("INSERT INTO session_v2 VALUES (?)", [session])
        .unwrap();
    db.execute(
        "INSERT INTO session_message VALUES (?, 'assistant', ?)",
        (
            session,
            format!(
                r#"{{"model":{{"id":"{model}","providerID":"{provider}","variant":"default"}}}}"#
            ),
        ),
    )
    .unwrap();
}

#[test]
fn model_observation_follows_each_session_source() {
    let cases = [
        ModelCase {
            session: "ses_x",
            model: r#"{"model":{"id":"beam-free","providerID":"opencode-go"},"finish":"stop"}"#,
            expected: "opencode/opencode-go/beam-free",
            source: ModelSource::Stream(include_bytes!(
                "../../../tests/fixtures/providers/opencode/v2.2.0-success.jsonl"
            )),
            live_wal: true,
        },
        ModelCase {
            session: "FIXTURE_SESSION_IDENTIFIER_007",
            model: r#"{"model":{"id":"glm-5.3-flash-free","providerID":"opencode"}}"#,
            expected: "opencode/opencode/glm-5.3-flash-free",
            source: ModelSource::Stream(include_bytes!(
                "../../../tests/fixtures/providers/opencode/unversioned-success.jsonl"
            )),
            live_wal: false,
        },
        ModelCase {
            session: "ses_zen",
            model: r#"{"model":{"id":"deepseek-v4.1-flash-free","providerID":"opencode","variant":"default"}}"#,
            expected: "opencode/opencode/deepseek-v4.1-flash-free",
            source: ModelSource::SessionV2,
            live_wal: false,
        },
    ];

    for case in cases {
        let dir = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(dir.path().join("opencode.db")).unwrap();
        if case.live_wal {
            db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
                .unwrap();
        }
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_message (session_id TEXT, type TEXT, data TEXT)",
        )
        .unwrap();
        if matches!(case.source, ModelSource::SessionV2) {
            db.execute_batch("CREATE TABLE session_v2 (id TEXT PRIMARY KEY)")
                .unwrap();
            db.execute("INSERT INTO session_v2 VALUES (?)", [case.session])
                .unwrap();
        }
        db.execute(
            "INSERT INTO session_message VALUES (?, 'assistant', ?)",
            (case.session, case.model),
        )
        .unwrap();
        if case.live_wal {
            assert!(dir.path().join("opencode.db-wal").is_file());
        }

        let observed_session = match case.source {
            ModelSource::Stream(bytes) => {
                let mut stream = OpenCode
                    .stream(None, &provenant_records::policy::Policy::default())
                    .unwrap();
                for chunk in bytes.chunks(17) {
                    OpenCode.observe(&mut stream, chunk).unwrap();
                }
                let outcome = OpenCode
                    .finish(
                        stream,
                        crate::ExitObservation {
                            code: Some(0),
                            timed_out: false,
                        },
                    )
                    .unwrap();
                assert_eq!(outcome.status(), crate::OutcomeStatus::Success);
                outcome.session().unwrap().to_owned()
            }
            ModelSource::SessionV2 => {
                session_exists(dir.path(), case.session).unwrap();
                case.session.to_owned()
            }
        };
        assert_eq!(observed_session, case.session);
        assert_eq!(
            observe_model(dir.path(), &observed_session).unwrap(),
            case.expected
        );
    }
}

#[test]
fn model_observation_rejects_unknown_sessions_and_conflicting_models() {
    let dir = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(dir.path().join("opencode.db")).unwrap();
    db.execute_batch("CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT)")
        .unwrap();
    db.execute(
        "INSERT INTO session_message VALUES (?, 'assistant', ?)",
        (
            "fixture-session",
            r#"{"model":{"id":"glm-5.3-flash-free","providerID":"opencode"}}"#,
        ),
    )
    .unwrap();
    assert!(matches!(
        observe_model(dir.path(), "unknown-session"),
        Err(ProviderError::UnknownIdentity)
    ));
    db.execute(
        "INSERT INTO session_message VALUES (?, 'assistant', ?)",
        (
            "fixture-session",
            r#"{"model":{"id":"other","providerID":"opencode"}}"#,
        ),
    )
    .unwrap();
    assert!(matches!(
        observe_model(dir.path(), "fixture-session"),
        Err(ProviderError::IdentityMismatch)
    ));
}

#[test]
fn go_session_recorded_only_in_session_v2_names_the_go_provider() {
    let dir = tempfile::tempdir().unwrap();
    seed_v2_only(dir.path(), "ses_go", "opencode-go", "mimo-v2.6-flash");
    session_exists(dir.path(), "ses_go").unwrap();
    // The route requested a Go model: the session must name the Go provider,
    // so a session that ran on the free provider cannot stand in for it.
    assert_eq!(
        observe_model(dir.path(), "ses_go").unwrap(),
        "opencode/opencode-go/mimo-v2.6-flash"
    );
    seed_v2_only(dir.path(), "ses_free", "opencode", "mimo-v2.6-flash");
    assert_ne!(
        observe_model(dir.path(), "ses_free").unwrap(),
        observe_model(dir.path(), "ses_go").unwrap()
    );
}

// Reduced from #1891: three tool steps, then final text without step_finish.
const FINAL_TEXT_WITHOUT_FINISH: &str = concat!(
    "{\"type\":\"step_start\",\"sessionID\":\"fixture\"}\n",
    "{\"type\":\"tool_use\",\"sessionID\":\"fixture\"}\n",
    "{\"type\":\"step_finish\",\"sessionID\":\"fixture\",\"part\":{\"reason\":\"tool-calls\"}}\n",
    "{\"type\":\"step_start\",\"sessionID\":\"fixture\"}\n",
    "{\"type\":\"tool_use\",\"sessionID\":\"fixture\"}\n",
    "{\"type\":\"step_finish\",\"sessionID\":\"fixture\",\"part\":{\"reason\":\"tool-calls\"}}\n",
    "{\"type\":\"step_start\",\"sessionID\":\"fixture\"}\n",
    "{\"type\":\"tool_use\",\"sessionID\":\"fixture\"}\n",
    "{\"type\":\"step_finish\",\"sessionID\":\"fixture\",\"part\":{\"reason\":\"tool-calls\"}}\n",
    "{\"type\":\"step_start\",\"sessionID\":\"fixture\"}\n",
    "{\"type\":\"text\",\"sessionID\":\"fixture\",\"part\":{\"text\":\"Provenant\"}}\n",
);

#[test]
fn completion_status_follows_answer_stop_and_exit_state() {
    use crate::{ExitObservation, OutcomeStatus};

    let stop =
        "{\"type\":\"step_finish\",\"sessionID\":\"fixture\",\"part\":{\"reason\":\"stop\"}}\n";
    let answerless = FINAL_TEXT_WITHOUT_FINISH
        .rsplit_once("{\"type\":\"text\"")
        .unwrap()
        .0;
    for (answer, stop_present, code, timed_out, expected) in [
        (true, false, Some(0), false, OutcomeStatus::Success),
        (true, true, Some(0), false, OutcomeStatus::Success),
        (false, false, Some(0), false, OutcomeStatus::Partial),
        (false, true, Some(0), false, OutcomeStatus::Success),
        (true, false, Some(1), false, OutcomeStatus::Failed),
        (true, false, None, false, OutcomeStatus::Failed),
        (true, false, Some(0), true, OutcomeStatus::Partial),
    ] {
        let mut stream = OpenCode
            .stream(None, &provenant_records::policy::Policy::default())
            .unwrap();
        let mut events = Vec::new();
        let raw = if answer {
            FINAL_TEXT_WITHOUT_FINISH
        } else {
            answerless
        };
        let mut input = raw.to_owned();
        if stop_present {
            input.push_str(stop);
        }
        for chunk in input.as_bytes().chunks(17) {
            events.extend(OpenCode.observe(&mut stream, chunk).unwrap());
        }
        assert_eq!(events.contains(&E::Text("Provenant".into())), answer);
        let outcome = OpenCode
            .finish(stream, ExitObservation { code, timed_out })
            .unwrap();
        assert_eq!(outcome.status(), expected, "{answer}, {stop_present}");
    }
}

#[test]
fn final_text_does_not_hide_error_events() {
    let error = b"{\"type\":\"error\",\"error\":{\"type\":\"APIError\"}}\n";
    for error_first in [true, false] {
        let mut stream = OpenCode
            .stream(None, &provenant_records::policy::Policy::default())
            .unwrap();
        let text = FINAL_TEXT_WITHOUT_FINISH.as_bytes();
        let parts = if error_first {
            [error.as_slice(), text]
        } else {
            [text, error.as_slice()]
        };
        for part in parts {
            OpenCode.observe(&mut stream, part).unwrap();
        }
        let outcome = OpenCode
            .finish(
                stream,
                crate::ExitObservation {
                    code: Some(0),
                    timed_out: false,
                },
            )
            .unwrap();
        assert_eq!(outcome.status(), crate::OutcomeStatus::Failed);
    }
}

#[test]
fn server_shell_state_is_created_and_writable() {
    let plan = crate::tests::read_only_plan("/fixture", Some("login"));
    let route = crate::tests::route("opencode", "fixture-model");
    let prompt = crate::Prompt::new("fixture".into());
    let input = crate::tests::fixture_input(
        &route,
        &plan,
        &prompt,
        std::path::Path::new("/fixture/repo"),
        None,
    );
    let spec = OpenCode.prepare(input).unwrap();
    let shell = plan
        .permissions()
        .private_state
        .as_ref()
        .unwrap()
        .path
        .join("data/opencode/shell");
    assert!(spec.setup.iter().any(|step| matches!(
            step, SetupStep::CreateDir(path) if *path == shell)));
    assert!(shell.starts_with(&plan.permissions().private_state.as_ref().unwrap().path));
    let argv: Vec<_> = spec.argv.iter().map(|a| a.to_string_lossy()).collect();
    assert!(argv.iter().any(|a| a == "--print-logs"));
}

// Spec: docs/specs/run-launch.md#lane-access
#[test]
fn every_lane_launch_archives_its_session() -> Result<(), ProviderError> {
    let route = crate::tests::route("opencode", "fixture-model");
    let prompt = crate::Prompt::new("fixture".into());
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = crate::tests::fixture_access_plan(access, Some("login"), true);
        for session in [Session::Fresh, Session::Resume] {
            let continuation = (session == Session::Resume)
                .then(|| {
                    crate::Continuation::retained("opencode", "fixture-session", &route, "fixture")
                })
                .transpose()?;
            let mut input = crate::tests::fixture_input(
                &route,
                &plan,
                &prompt,
                std::path::Path::new("/fixture/repo"),
                None,
            );
            input.continuation = continuation.as_ref();
            let spec = prepare(
                &input,
                access,
                session,
                OpenCode.stream(None, &provenant_records::policy::Policy::default())?,
            )?;
            assert!(
                spec.argv.iter().any(|arg| arg == "--archived"),
                "{access:?} {session:?}"
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn cached_search_library_grants_follow_content_and_exclude_lane_paths()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    fn image(path: &Path, file_type: u32, dependencies: &[&Path]) -> std::io::Result<()> {
        let mut bytes = vec![0; 32];
        bytes[..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&file_type.to_le_bytes());
        bytes[16..20].copy_from_slice(&(dependencies.len() as u32).to_le_bytes());
        for dependency in dependencies {
            let name = dependency.to_str().unwrap().as_bytes();
            let start = bytes.len();
            let size = (24 + name.len() + 1).next_multiple_of(8);
            bytes.extend(0xc_u32.to_le_bytes());
            bytes.extend((size as u32).to_le_bytes());
            bytes.extend(24_u32.to_le_bytes());
            bytes.resize(start + 24, 0);
            bytes.extend(name);
            bytes.resize(start + size, 0);
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, bytes)
    }

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let lane = root.join("lane");
    let cwd = lane.join("repo");
    let home = lane.join("home");
    let first = root.join("host/arbitrary/first.dylib");
    let second = root.join("host/other/second.dylib");
    image(&first, 6, &[&second])?;
    image(&second, 6, &[])?;
    let alias = root.join("host/alias.dylib");
    symlink(&first, &alias)?;
    #[derive(Clone, Copy)]
    enum Coverage {
        Library,
        AlreadyCovered,
        CredentialOmitted,
        Ineligible,
    }
    struct DependencyFixture {
        path: PathBuf,
        coverage: Coverage,
    }

    let home_text = home.join("Documents/plain.txt");
    let host_text = root.join("host/Cellar/plain.txt");
    let home_library = home.join("fixture.dylib");
    let state_library = lane.join("state/fixture.dylib");
    let scratch_library = lane.join("scratch/fixture.dylib");
    let worktree_library = cwd.join("fixture.dylib");
    let credential_library = root.join("host/.ssh/fixture.dylib");
    let executable = root.join("host/executable");
    let directory = root.join("host/directory");
    // Already-covered images still contribute their own dependencies.
    let hidden = root.join("host/hidden.dylib");
    image(&hidden, 6, &[])?;
    for path in [
        &home_text,
        &host_text,
        &home_library,
        &state_library,
        &scratch_library,
        &worktree_library,
        &credential_library,
        &executable,
        &directory,
    ] {
        image(path, 6, &[&hidden])?;
    }
    std::fs::write(&home_text, "plain text")?;
    std::fs::write(&host_text, "plain text")?;
    image(&executable, 2, &[&hidden])?;
    std::fs::remove_file(&directory)?;
    std::fs::create_dir(&directory)?;
    let cached = lane.join("state/cache/opencode/bin/rg");
    let mut dependencies = vec![alias.as_path()];
    dependencies.extend(
        [
            &home_text,
            &host_text,
            &home_library,
            &state_library,
            &scratch_library,
            &worktree_library,
            &credential_library,
            &executable,
            &directory,
        ]
        .iter()
        .map(|path| path.as_path()),
    );
    image(&cached, 2, &dependencies)?;
    // A non-executable, unrelated rg earlier on PATH cannot suppress grants.
    let host_bin = root.join("host/bin");
    std::fs::create_dir_all(&host_bin)?;
    std::fs::write(host_bin.join("rg"), "unrelated")?;
    let plan = crate::tests::read_only_plan(lane.to_str().unwrap(), Some("login"));
    let route = crate::tests::route("opencode", "fixture-model");
    let prompt = crate::Prompt::new("fixture".into());
    let mut input = crate::tests::fixture_input(&route, &plan, &prompt, &cwd, None);
    input.home = &home;
    // Credential stores are declared paths; the host's store is declared
    // here as host configuration would declare it.
    let mut credential_roots = provenant_core::CredentialRoots::for_home(&home);
    credential_roots.add(
        root.join("host/.ssh"),
        provenant_core::CredentialScope::Subtree,
        "fixture host declaration",
        provenant_core::CredentialKind::Store,
    );
    input.credential_roots = credential_roots;
    let host_var = |name: &str| (name == "PATH").then(|| host_bin.clone().into_os_string());
    input.host_var = &host_var;
    let spec = prepare(
        &input,
        Access::ReadOnly,
        Session::Fresh,
        OpenCode.stream(None, &provenant_records::policy::Policy::default())?,
    )?;
    let tool = spec
        .prepared_tools()
        .iter()
        .find(|tool| tool.executable == cached)
        .expect("cached rg has prepared closure facts");
    assert_eq!(
        tool.dependency_closure.outcome,
        crate::ClosureOutcome::Incomplete
    );
    assert!(
        tool.dependency_closure
            .reasons
            .iter()
            .any(|reason| matches!(reason, crate::IncompleteReason::Malformed(_)))
    );
    for path in &tool.dependency_closure.libraries {
        assert_eq!(std::fs::canonicalize(path)?, *path);
        assert!(spec.profile.spec.runtime_reads.iter().any(|grant| {
            grant.path == *path
                && grant.scope == provenant_confinement::permissions::PathScope::Exact
        }));
        assert!(spec.profile.spec.runtime_reads.iter().all(|grant| {
            grant.scope != provenant_confinement::permissions::PathScope::Subtree
                || !path.starts_with(&grant.path)
        }));
        assert!(
            provenant_confinement::write_grants(plan.permissions(), &spec.profile)
                .iter()
                .all(|grant| !path.starts_with(&grant.path))
        );
    }
    let denied = [
        home_text.clone(),
        host_text.clone(),
        home_library.clone(),
        state_library.clone(),
        scratch_library.clone(),
        worktree_library.clone(),
        credential_library.clone(),
        executable.clone(),
        directory.clone(),
    ];
    let dependency_fixtures = [
        DependencyFixture {
            path: first.clone(),
            coverage: Coverage::Library,
        },
        DependencyFixture {
            path: second.clone(),
            coverage: Coverage::Library,
        },
        DependencyFixture {
            path: home_library,
            coverage: Coverage::Library,
        },
        DependencyFixture {
            path: hidden.clone(),
            coverage: Coverage::Library,
        },
        DependencyFixture {
            path: state_library,
            coverage: Coverage::AlreadyCovered,
        },
        DependencyFixture {
            path: scratch_library,
            coverage: Coverage::AlreadyCovered,
        },
        DependencyFixture {
            path: worktree_library,
            coverage: Coverage::AlreadyCovered,
        },
        DependencyFixture {
            path: credential_library,
            coverage: Coverage::CredentialOmitted,
        },
        DependencyFixture {
            path: home_text,
            coverage: Coverage::Ineligible,
        },
        DependencyFixture {
            path: host_text,
            coverage: Coverage::Ineligible,
        },
        DependencyFixture {
            path: executable,
            coverage: Coverage::Ineligible,
        },
        DependencyFixture {
            path: directory,
            coverage: Coverage::Ineligible,
        },
    ];
    for fixture in dependency_fixtures {
        let libraries = tool.dependency_closure.libraries.contains(&fixture.path);
        let covered = tool
            .dependency_closure
            .already_covered
            .contains(&fixture.path);
        let omitted = tool.dependency_closure.omitted.contains(&fixture.path);
        match fixture.coverage {
            Coverage::Library => {
                assert!(libraries, "{}", fixture.path.display());
                assert_eq!(std::fs::canonicalize(&fixture.path)?, fixture.path);
                assert!(spec.profile.spec.runtime_reads.iter().all(|grant| {
                    grant.scope != provenant_confinement::permissions::PathScope::Subtree
                        || !fixture.path.starts_with(&grant.path)
                }));
                assert!(
                    provenant_confinement::write_grants(plan.permissions(), &spec.profile)
                        .iter()
                        .all(|grant| !fixture.path.starts_with(&grant.path)
                            && !grant.path.starts_with(&fixture.path))
                );
            }
            Coverage::AlreadyCovered => assert!(covered, "{}", fixture.path.display()),
            Coverage::CredentialOmitted => {
                assert!(omitted, "{}", fixture.path.display());
                assert!(tool.dependency_closure.reasons.iter().any(|reason| matches!(
                        reason,
                        crate::IncompleteReason::OmittedCredential(path) if path == &fixture.path
                    )));
            }
            Coverage::Ineligible => assert!(!libraries && !covered && !omitted),
        }
    }
    assert!(
        !tool
            .receipt()
            .to_string()
            .contains(&root.display().to_string())
    );
    let profile = provenant_confinement::permissions::snapshot::render(
        plan.permissions(),
        &spec.profile,
        &home,
    )?;
    for path in [&first, &second, &denied[2], &hidden] {
        assert!(
            profile.contains(&format!(
                "(allow file-read* (literal \"{}\"))",
                path.display()
            )),
            "{}",
            path.display()
        );
    }
    for path in denied
        .iter()
        .enumerate()
        .filter_map(|(index, path)| (index != 2).then_some(path))
        .chain([&alias])
    {
        assert!(
            !profile.contains(&format!(
                "(allow file-read* (literal \"{}\"))",
                path.display()
            )),
            "{}",
            path.display()
        );
    }
    for exec in profile
        .lines()
        .filter_map(|line| line.strip_prefix("(allow process-exec (subpath \""))
        .filter_map(|line| line.strip_suffix("\"))"))
    {
        for lane_path in [
            &cwd,
            &home,
            &plan.permissions().scratch.path,
            &plan
                .permissions()
                .private_state
                .as_ref()
                .ok_or("private state")?
                .path,
        ] {
            assert!(
                !Path::new(exec).starts_with(lane_path),
                "lane exec subtree: {exec}"
            );
        }
    }
    Ok(())
}

fn search_tool(path: &std::ffi::OsStr, required: bool) -> Result<Option<PathBuf>, String> {
    let rg = std::env::split_paths(path)
        .map(|dir| dir.join("rg"))
        .find(|path| path.is_file());
    if rg.is_none() {
        provenant_test_support::missing_prerequisite(required, "rg for native search")?;
    }
    Ok(rg)
}

#[test]
fn missing_native_search_tool_obeys_required_control() {
    let path = tempfile::tempdir().unwrap();
    let error = search_tool(path.path().as_os_str(), true).unwrap_err();
    assert!(error.contains("rg"));
    assert_eq!(search_tool(path.path().as_os_str(), false).unwrap(), None);
}

#[cfg(target_os = "macos")]
#[test]
#[allow(clippy::disallowed_methods)]
fn read_only_search_runs_in_native_profile() -> Result<(), Box<dyn std::error::Error>> {
    let Some(rg) = search_tool(
        &std::env::var_os("PATH").unwrap_or_default(),
        std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_some(),
    )?
    else {
        return Ok(());
    };
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let cached = root.join("state/cache/opencode/bin/rg");
    std::fs::create_dir_all(cached.parent().unwrap())?;
    std::fs::copy(rg, &cached)?;
    // `repo` is the read root the renderer takes as the lane's source and
    // `other-member` a protected checkout, the two roots a read-only
    // shell must not write.
    let plan = crate::tests::read_only_plan_protecting(
        root.to_str().unwrap(),
        Some("login"),
        &["other-member"],
    );
    let route = crate::tests::route("opencode", "fixture-model");
    let prompt = crate::Prompt::new("fixture".into());
    let mut input = crate::tests::fixture_input(&route, &plan, &prompt, &root, None);
    input.host_var = &|name| std::env::var_os(name);
    let spec = prepare(
        &input,
        Access::ReadOnly,
        Session::Fresh,
        OpenCode.stream(None, &provenant_records::policy::Policy::default())?,
    )?;
    let requirement = spec
        .prepared_tools()
        .iter()
        .find(|tool| tool.executable == cached)
        .expect("cached rg has prepared closure facts");
    for library in &requirement.dependency_closure.libraries {
        assert!(spec.profile.spec.runtime_reads.contains(&exact(library)));
    }
    let profile = provenant_confinement::permissions::snapshot::render(
        plan.permissions(),
        &spec.profile,
        input.home,
    )?;
    for program in [cached.as_path(), std::path::Path::new("/usr/bin/grep")] {
        let output = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile])
            .arg(program)
            .arg("--version")
            .output()?;
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        if diagnostic.contains("sandbox_apply: Operation not permitted") {
            provenant_test_support::skip_sandbox_dependent_test(diagnostic.to_string())?;
            return Ok(());
        }
        assert!(
            output.status.success(),
            "{}: {diagnostic}",
            program.display()
        );
    }
    // The same launch must support the brief's temporary-file + rename
    // protocol while the shell it grants cannot write the source or a
    // protected checkout. The profile is `allow default`; these refusals
    // come from the read-only source and protected-root denials in
    // `provenant_confinement::permissions::render::push_boundaries`, not
    // from OpenCode's permission config.
    let private = &plan.permissions().private_state.as_ref().unwrap().path;
    std::fs::create_dir_all(private)?;
    let source = root.join("repo");
    let protected = root.join("other-member");
    std::fs::create_dir_all(&source)?;
    std::fs::create_dir_all(&protected)?;
    let output = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile, "/bin/sh", "-c"])
            .arg(r#"set -eu
printf '%s' '{"schema_version":2,"status":"done","note":"progress","remaining":[],"decisions":[],"verify":[]}' > "$1/handoff.tmp"
/bin/mv "$1/handoff.tmp" "$1/handoff.json"
if (printf forbidden > "$2/forbidden") 2>/dev/null; then exit 91; fi
if (printf forbidden > "$3/forbidden") 2>/dev/null; then exit 92; fi
if /bin/mv "$1/handoff.json" "$2/handoff.json" 2>/dev/null; then exit 93; fi
"#)
            .arg("handoff-probe")
            .arg(private)
            .arg(&source)
            .arg(&protected)
            .output()?;
    assert!(
        output.status.success(),
        "probe exit {:?} (91: source written, 92: protected checkout written, \
             93: handoff moved into source): {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let progress: serde_json::Value =
        serde_json::from_slice(&std::fs::read(private.join("handoff.json"))?)?;
    assert_eq!(progress["status"], "done");
    assert!(!private.join("handoff.tmp").exists());
    assert!(!source.join("forbidden").exists());
    assert!(!source.join("handoff.json").exists());
    assert!(!protected.join("forbidden").exists());
    Ok(())
}

#[test]
fn server_http_failure_diagnostic_carries_its_typed_cause_and_status() {
    let log = concat!(
        "timestamp=t level=INFO run=a message=\"watcher stopped\" path=/x http.span=1\n",
        "timestamp=t level=INFO run=a cause=\"Error: EPERM: operation not permitted, ",
        "mkdir '/h/.local/share/opencode/shell/abc'\\n    at ServerProcess.start\" ",
        "http.span=68 role=server http.method=GET http.url=/api/location http.status=500\n",
    );
    let failure = parse_failure_diagnostic(log.as_bytes()).unwrap();
    assert_eq!(failure.status, 500);
    assert!(failure.cause.contains("EPERM"));
    assert!(failure.cause.contains("mkdir"));
    let rendered = failure_diagnostic(log.as_bytes()).unwrap();
    assert!(rendered.contains(&failure.status.to_string()));
    assert!(rendered.contains(&failure.cause));
    let long = format!(
        "level=INFO cause=\"{}\" http.status=503\n",
        "x".repeat(1000)
    );
    let diagnostic = parse_failure_diagnostic(long.as_bytes()).unwrap();
    assert_eq!(diagnostic.status, 503);
    assert!(diagnostic.cause.chars().count() <= SERVER_FAILURE_CAUSE_LIMIT);
    let secret =
        "level=INFO cause=\"fetch https://user:secret@example.invalid/x\" http.status=500\n";
    let diagnostic = parse_failure_diagnostic(secret.as_bytes()).unwrap();
    assert!(diagnostic.cause.contains("user:secret"));
    for unrelated in [
        "level=INFO cause=\"boom\" http.status=404\n",
        "level=INFO message=\"ok\" http.status=500\n",
        "level=INFO cause=\"\" http.status=500\n",
        "garbage",
    ] {
        assert_eq!(
            failure_diagnostic(unrelated.as_bytes()),
            None,
            "{unrelated}"
        );
    }
}

#[test]
fn discovery_reads_are_exact_below_the_home_for_each_cwd_shape() {
    let cases = [
        ("/fixture", "/fixture/repo", &["/fixture/repo"][..]),
        (
            "/fixture/home",
            "/fixture/home/repo/crate",
            &["/fixture/home/repo/crate", "/fixture/home/repo"][..],
        ),
    ];
    for (root, cwd, expected_parents) in cases {
        let plan = crate::tests::read_only_plan(root, Some("login"));
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        let cwd = std::path::Path::new(cwd);
        let input = crate::tests::fixture_input(&route, &plan, &prompt, cwd, None);
        let spec = prepare(
            &input,
            Access::ReadOnly,
            Session::Fresh,
            OpenCode
                .stream(None, &provenant_records::policy::Policy::default())
                .unwrap(),
        )
        .unwrap();
        let expected = expected_parents
            .iter()
            .flat_map(|parent| {
                [".agents", ".claude", ".codex"]
                    .map(|name| exact(PathBuf::from(*parent).join(name)))
            })
            .collect::<Vec<_>>();
        let mut actual = spec.profile.spec.discovery_reads;
        actual.sort_by(|left, right| left.path.cmp(&right.path));
        let mut expected = expected;
        expected.sort_by(|left, right| left.path.cmp(&right.path));
        assert_eq!(actual, expected);
        assert!(
            actual
                .iter()
                .all(|grant| grant.scope == provenant_confinement::permissions::PathScope::Exact)
        );
    }
}

#[cfg(unix)]
#[test]
fn search_path_is_independent_of_unencodable_git_shim_directory() {
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let base = crate::tests::read_only_plan("/fixture", Some("login"));
    let scratch = tempfile::tempdir().unwrap();
    let scratch_path = scratch.path().canonicalize().unwrap().join("scratch:other");
    std::fs::create_dir_all(scratch_path.join("bin")).unwrap();
    let mut permissions = base.permissions().clone();
    permissions.scratch.path = scratch_path;
    let plan = ConfinedPlan::new(PlanRequest {
        scope: base.scope(),
        permissions,
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
    .unwrap();
    let route = crate::tests::route("opencode", "fixture-model");
    let prompt = crate::Prompt::new("fixture".into());
    let input = crate::tests::fixture_input(
        &route,
        &plan,
        &prompt,
        std::path::Path::new("/fixture/repo"),
        None,
    );
    let spec = prepare(
        &input,
        Access::ReadOnly,
        Session::Fresh,
        OpenCode
            .stream(None, &provenant_records::policy::Policy::default())
            .unwrap(),
    )
    .unwrap();
    let path = &spec.env.iter().find(|(key, _)| key == "PATH").unwrap().1;
    let defaults = crate::launch::tool_search_path(input.home, true, input.policy).unwrap();
    assert_eq!(path, &defaults);
    let writer = prepare(
        &input,
        Access::WorktreeWrite,
        Session::Fresh,
        OpenCode
            .stream(None, &provenant_records::policy::Policy::default())
            .unwrap(),
    )
    .expect("OpenCode's provider builder leaves Git wrapper setup to shared finalisation");
    assert_eq!(
        writer.env.iter().find(|(key, _)| key == "PATH").unwrap().1,
        defaults,
        "provider preparation must not add a private Git prefix"
    );
}

#[test]
fn writer_search_path_preserves_non_utf8_home() {
    use std::os::unix::ffi::OsStringExt;

    let plan = crate::tests::read_only_plan("/fixture", Some("login"));
    let route = crate::tests::route("opencode", "fixture-model");
    let prompt = crate::Prompt::new("fixture".into());
    let home = PathBuf::from(OsString::from_vec(b"/fixture/home-\xff".to_vec()));
    let mut input = crate::tests::fixture_input(
        &route,
        &plan,
        &prompt,
        std::path::Path::new("/fixture/repo"),
        None,
    );
    input.home = &home;
    input.credential_roots = provenant_core::CredentialRoots::for_home(&home);
    let spec = prepare(
        &input,
        Access::WorktreeWrite,
        Session::Fresh,
        OpenCode
            .stream(None, &provenant_records::policy::Policy::default())
            .unwrap(),
    )
    .unwrap();
    let path = spec.env.iter().find(|(key, _)| key == "PATH").unwrap();
    assert!(std::env::split_paths(&path.1).any(|entry| entry == home.join(".cargo/bin")));
}

#[test]
fn writer_search_path_skips_home_with_path_separator() {
    let plan = crate::tests::read_only_plan("/fixture", Some("login"));
    let route = crate::tests::route("opencode", "fixture-model");
    let prompt = crate::Prompt::new("fixture".into());
    let home = std::path::Path::new("/fixture/home:with-separator");
    let mut input = crate::tests::fixture_input(
        &route,
        &plan,
        &prompt,
        std::path::Path::new("/fixture/repo"),
        None,
    );
    input.home = home;
    input.credential_roots = provenant_core::CredentialRoots::for_home(home);
    let spec = prepare(
        &input,
        Access::WorktreeWrite,
        Session::Fresh,
        OpenCode
            .stream(None, &provenant_records::policy::Policy::default())
            .unwrap(),
    )
    .unwrap();
    let path = spec.env.iter().find(|(key, _)| key == "PATH").unwrap();
    assert!(!std::env::split_paths(&path.1).any(|entry| entry == home.join(".cargo/bin")));
}

#[test]
fn git_discovery_preserves_writer_config_and_isolates_reader() {
    let route = crate::tests::route("opencode", "fixture-model");
    let prompt = crate::Prompt::new("fixture".into());
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = crate::tests::fixture_access_plan(access, Some("login"), false);
        let spec = OpenCode
            .prepare(crate::tests::fixture_input(
                &route,
                &plan,
                &prompt,
                std::path::Path::new("/fixture/repo"),
                None,
            ))
            .unwrap();
        let global = spec
            .env
            .iter()
            .find(|(name, _)| name == "GIT_CONFIG_GLOBAL");
        let expected = if access == Access::WorktreeWrite {
            "/fixture/scratch/gitconfig"
        } else {
            "/dev/null"
        };
        assert_eq!(
            global.map(|(_, value)| value.as_os_str()),
            Some(expected.as_ref())
        );
        // The lane does not force system Git configuration off, whatever
        // the access.
        assert!(
            spec.env
                .iter()
                .all(|(name, _)| name != "GIT_CONFIG_NOSYSTEM"),
            "{access:?}"
        );
    }
}

// Anchors are real host paths; on Linux `/bin` links to `/usr/bin`, and
// only the macOS backend confines OpenCode.
#[cfg(target_os = "macos")]
#[test]
fn homebrew_launch_plan_anchors_with_discovery_directories_for_each_access_mode() {
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        homebrew_discovery_fixture(access);
    }
}

#[cfg(target_os = "macos")]
fn homebrew_discovery_fixture(access: Access) {
    use provenant_confinement::permissions::snapshot::compile;
    use provenant_confinement::permissions::{AnchoredDirectory, Anchors, required_anchors};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let prefix = root.join("opt/homebrew");
    let package = prefix.join("Cellar/opencode-v2/2.0.32");
    let bin = prefix.join("bin");
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::write(package.join("bin/opencode"), "fixture").unwrap();
    let link = bin.join("opencode");
    std::os::unix::fs::symlink("../Cellar/opencode-v2/2.0.32/bin/opencode", &link).unwrap();
    // Dispatch resolves the CLI before passing it to the provider.
    let executable = link.canonicalize().unwrap();
    // The selected Git can be outside both system and provider roots,
    // and its executable need not be named `git`.
    let git = prefix.join("Cellar/git/1.0/bin/selected-git");
    fs::create_dir_all(git.parent().unwrap()).unwrap();
    fs::write(&git, "fixture git").unwrap();
    // The project sits under the home, as lane worktrees do.
    let home = root.clone();
    fs::create_dir_all(home.join(".config/opencode")).unwrap();
    let project_parent = root.join("projects");
    fs::create_dir_all(project_parent.join("repo")).unwrap();
    // Both the read root and an ancestor contain discovery directories.
    for parent in [&project_parent, &project_parent.join("repo")] {
        for name in [".agents", ".claude", ".codex"] {
            fs::create_dir(parent.join(name)).unwrap();
            fs::write(parent.join(name).join("config.json"), "private config").unwrap();
        }
    }
    // Compilation refuses private directories other users can reach.
    for name in ["scratch", "state"] {
        fs::create_dir(project_parent.join(name)).unwrap();
        fs::set_permissions(project_parent.join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let cached = project_parent.join("state/cache/opencode/bin/rg");
    if access == Access::ReadOnly && std::path::Path::new("/opt/homebrew/bin/rg").is_file() {
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::copy("/opt/homebrew/bin/rg", &cached).unwrap();
    }
    let plan = crate::tests::read_only_plan(project_parent.to_str().unwrap(), Some("login"));
    let route = crate::tests::route("opencode", "fixture-model");
    let prompt = crate::Prompt::new("fixture".into());
    let cwd = project_parent.join("repo");
    let mut input = PrepareInput {
        executable: &executable,
        home: &home,
        credential_roots: provenant_core::CredentialRoots::for_home(&home),
        ..crate::tests::fixture_input(&route, &plan, &prompt, &cwd, None)
    };
    input.provenant.git = &git;
    {
        let spec = prepare(
            &input,
            access,
            Session::Fresh,
            OpenCode
                .stream(None, &provenant_records::policy::Policy::default())
                .unwrap(),
        )
        .unwrap();
        for step in &spec.setup {
            match step {
                SetupStep::CreateDir(path) => fs::create_dir_all(path).unwrap(),
                SetupStep::Symlink { link, target } => {
                    std::os::unix::fs::symlink(target, link).unwrap();
                }
                _ => panic!("unexpected setup step: {step:?}"),
            }
        }
        // OpenCode's provider builder leaves Git wrapper setup to the
        // shared finaliser; it does not create the removed private prefix.
        assert!(!project_parent.join("scratch/bin/git").exists());
        assert_eq!(
            spec.env.iter().find(|(key, _)| key == "PATH").unwrap().1,
            crate::launch::tool_search_path(&home, true, input.policy).unwrap()
        );
        assert!(spec.profile.spec.runtime_reads.contains(&subtree(&package)));
        let directories = required_anchors(plan.permissions(), &spec.profile, None)
            .into_iter()
            .map(|path| {
                AnchoredDirectory::open_canonical(&path).unwrap_or_else(|error| {
                    panic!("{access:?} anchor {} refused: {error}", path.display())
                })
            })
            .collect::<Vec<_>>();
        let anchors = Anchors {
            home: &home,
            directories: &directories,
            worktree: None,
        };
        let launch = compile(plan.permissions(), &spec.profile, &anchors)
            .unwrap_or_else(|error| panic!("{access:?} profile refused: {error}"));
        let profile = String::from_utf8(launch.profile).unwrap();
        assert!(profile.contains(&format!(
            "(allow process-exec (literal \"{}\"))",
            git.display()
        )));
        assert!(!profile.contains(&format!(
            "(allow process-exec (subpath \"{}\"))",
            git.display()
        )));
        assert!(profile.contains(&format!("(literal \"{}\")", cwd.join(".claude").display())));
        let grants_subpath = |path: &std::path::Path| {
            let needle = format!("(subpath \"{}\")", path.display());
            profile
                .lines()
                .any(|line| line.trim_start().starts_with("(allow") && line.contains(&needle))
        };
        for name in [".agents", ".claude", ".codex"] {
            for parent in [&project_parent, &cwd] {
                let directory = parent.join(name);
                assert!(profile.contains(&format!("(literal \"{}\")", directory.display())));
                // Credential-root denials may name the subtree; no grant may.
                assert!(!grants_subpath(&directory));
                assert!(!profile.contains(directory.join("config.json").to_str().unwrap()));
            }
            // Ancestor configuration has no enclosing data-read grant.
            assert!(!grants_subpath(&project_parent));
        }
    }
}

#[test]
fn host_account_uses_xdg_fallback_while_session_data_is_private() {
    let plan = crate::tests::read_only_plan("/fixture", Some("login"));
    let route = crate::tests::route("opencode", "fixture-model");
    let prompt = crate::Prompt::new("fixture".into());
    let lookup = |key: &str| match key {
        "XDG_CONFIG_HOME" => Some("/xdg/config".into()),
        "XDG_DATA_HOME" => Some("relative/data".into()),
        _ => None,
    };
    let input = PrepareInput {
        host_var: &lookup,
        credential_roots: {
            let mut roots =
                provenant_core::CredentialRoots::for_home(std::path::Path::new("/fixture/home"));
            roots.add(
                PathBuf::from("/xdg/config/opencode"),
                provenant_core::CredentialScope::Subtree,
                "provider declaration: opencode XDG config",
                provenant_core::CredentialKind::Store,
            );
            roots.add(
                PathBuf::from("/xdg/data/opencode"),
                provenant_core::CredentialScope::Subtree,
                "provider declaration: opencode XDG data",
                provenant_core::CredentialKind::Store,
            );
            roots
        },
        ..crate::tests::fixture_input(
            &route,
            &plan,
            &prompt,
            std::path::Path::new("/fixture/repo"),
            None,
        )
    };
    let spec = prepare(
        &input,
        Access::ReadOnly,
        Session::Fresh,
        OpenCode
            .stream(None, &provenant_records::policy::Policy::default())
            .unwrap(),
    )
    .unwrap();
    let env = |key: &str| {
        spec.env
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(env("XDG_CONFIG_HOME"), Some("/fixture/state/config".into()));
    assert_eq!(env("XDG_DATA_HOME"), Some("/fixture/state/data".into()));
    let auth = exact("/fixture/home/.local/share/opencode/auth.json");
    assert_eq!(spec.profile.spec.authentication[0].mechanism, "login");
    assert!(spec.profile.spec.authentication[0].roots.contains(&auth));
    assert!(spec.setup.contains(&SetupStep::Symlink {
        link: "/fixture/state/data/opencode/auth.json".into(),
        target: auth.path
    }));
    let lookup = |key: &str| match key {
        "XDG_DATA_HOME" => Some("/xdg/data".into()),
        _ => None,
    };
    let custom = PrepareInput {
        host_var: &lookup,
        ..input
    };
    let custom = prepare(
        &custom,
        Access::ReadOnly,
        Session::Fresh,
        OpenCode
            .stream(None, &provenant_records::policy::Policy::default())
            .unwrap(),
    )
    .unwrap();
    assert!(
        custom.profile.spec.authentication[0]
            .roots
            .contains(&exact("/xdg/data/opencode/auth.json"))
    );
    assert!(custom.setup.contains(&SetupStep::Symlink {
        link: "/fixture/state/data/opencode/auth.json".into(),
        target: "/xdg/data/opencode/auth.json".into()
    }));
    assert!(
        spec.profile
            .spec
            .state_writes
            .iter()
            .all(|grant| grant.path.starts_with("/fixture/state/data/opencode"))
    );
}

// Spec: docs/specs/routing.md#exact-selection-no-inference
#[test]
fn variant_unavailable_is_a_typed_route_refusal() {
    let mut stream = OpenCode
        .stream(None, &provenant_records::policy::Policy::default())
        .unwrap();
    // observe() decodes complete lines; an unterminated frame stays pending.
    let raw = concat!(
        r#"{"type":"error","sessionID":"ses_variant","error":{"type":"provider.no-route","message":"Variant unavailable for opencode/hy4-flash-free: bogus"}}"#,
        "\n",
    );
    let events = OpenCode.observe(&mut stream, raw.as_bytes()).unwrap();
    assert!(events.contains(&E::Refusal), "{events:?}");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, E::ModelUnavailable { .. })),
        "{events:?}"
    );
    let outcome = OpenCode
        .finish(
            stream,
            crate::ExitObservation {
                code: Some(1),
                timed_out: false,
            },
        )
        .unwrap();
    assert_eq!(outcome.status(), crate::OutcomeStatus::Refused);
}

// Spec: docs/specs/routing.md#exact-selection-no-inference
#[test]
fn variant_unavailable_gets_route_guidance_and_invalid_reference_still_does() {
    let raw = br#"{"type":"error","error":{"type":"provider.no-route","message":"Variant unavailable for opencode/hy4-flash-free: bogus"}}"#;
    let note = stdout_error_diagnostic(raw, Some("opencode/hy4-flash-free"), Some("bogus"))
        .expect("variant refusal has a diagnostic");
    assert!(
        note.contains("choose a supported model and variant"),
        "{note}"
    );
    let raw =
        br#"{"type":"error","error":{"type":"unknown","message":"Invalid model reference: foo"}}"#;
    let note =
        stdout_error_diagnostic(raw, None, None).expect("invalid reference has a diagnostic");
    assert!(
        note.contains("choose a supported model and variant"),
        "{note}"
    );
}
