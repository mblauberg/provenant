use super::*;

#[test]
fn projected_cli_error_omits_unsafe_source_and_keeps_bounded_fields() {
    for source in [
        "TOKEN_SENTINEL\u{1b}".to_owned(),
        format!("TOKEN_SENTINEL{}", "é".repeat(2048)),
    ] {
        let failure = super::Fail::from(provenant_run::RunError::Io(std::io::Error::other(source)));
        let reply = super::reply("run", Err(failure));
        let wire = serde_json::to_value(&reply.result).unwrap();
        assert_eq!(wire["error"]["code"], "IO");
        assert!(wire["error"].get("detail").is_none());
        assert!(!wire.to_string().contains("TOKEN_SENTINEL"));
        assert!(wire.to_string().len() < 4096);
        let mut human = Vec::new();
        crate::render::render(
            &reply.result,
            crate::render::RenderMode::Human {
                text: "",
                summary: None,
                read_only: false,
            },
            &mut human,
        )
        .unwrap();
        let human = String::from_utf8(human).unwrap();
        assert!(!human.contains("TOKEN_SENTINEL"));
        assert!(!human.chars().any(|c| c == '\u{1b}'));
        assert!(human.len() < 4096);
    }
    let failure = super::Fail::from(provenant_run::RunError::Io(std::io::Error::other(
        "TOKEN_SENTINEL\u{1b}",
    )));
    let reply = super::reply("run", Err(failure));
    let wire = serde_json::to_value(&reply.result).unwrap();
    assert_eq!(wire["error"]["code"], "IO");
    assert_eq!(wire["error"]["category"], "error");
    assert!(wire["error"].get("detail").is_none());
    assert!(!wire.to_string().contains("TOKEN_SENTINEL"));
    let mut rendered = Vec::new();
    crate::render::render(
        &reply.result,
        crate::render::RenderMode::Human {
            text: "",
            summary: None,
            read_only: false,
        },
        &mut rendered,
    )
    .unwrap();
    assert!(
        !String::from_utf8(rendered)
            .unwrap()
            .contains("TOKEN_SENTINEL")
    );
}
#[test]
fn non_refused_run_error_names_its_safe_cause_only() {
    let failure = super::Fail::from(provenant_run::RunError::OperationIo {
        operation: "read attempt",
        path: Some("/TOKEN_SENTINEL".into()),
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "TOKEN_SENTINEL"),
    });
    let reply = super::reply("telemetry stats", Err(failure));
    let wire = serde_json::to_value(&reply.result).unwrap();
    assert_eq!(wire["error"]["category"], "error");
    let cause = wire["error"]["cause"].as_str().unwrap();
    assert!(cause.contains("read attempt"), "{cause}");
    assert!(cause.contains("PermissionDenied"), "{cause}");
    assert!(!wire.to_string().contains("TOKEN_SENTINEL"));
    let failure = super::Fail::from(provenant_run::RunError::Contention);
    let reply = super::reply("telemetry stats", Err(failure));
    let wire = serde_json::to_value(&reply.result).unwrap();
    assert!(
        wire["error"]["cause"]
            .as_str()
            .unwrap_or_else(|| panic!("{wire}"))
            .contains("busy timeout")
    );
}
#[test]
fn cli_safe_local_facts_are_bounded_and_escape_controls() {
    use provenant_confinement::worktrees::WorktreeError;
    use provenant_records::diagnostic::MAX_DIAGNOSTIC_BYTES;
    for evidence in [
        "é\u{1b}\nend".to_owned(),
        format!("é\u{1b}{}", "界".repeat(1024)),
    ] {
        let failure = super::Fail::from(provenant_run::RunError::Writer(
            WorktreeError::CensusUnknown {
                path: "/fixture/tree".into(),
                detail: evidence,
            },
        ));
        let reply = super::reply("run", Err(failure));
        let wire = serde_json::to_value(&reply.result).unwrap();
        let detail = wire["error"]["detail"].as_str().unwrap();
        assert!(
            detail.contains("é\\u{1b}\\n") || detail.contains("é\\u{1b}"),
            "{detail:?}"
        );
        assert!(detail.len() <= MAX_DIAGNOSTIC_BYTES);
        assert!(!detail.chars().any(char::is_control));
        assert!(wire["error"]["next"].is_string());
        let mut human = Vec::new();
        crate::render::render(
            &reply.result,
            crate::render::RenderMode::Human {
                text: "",
                summary: None,
                read_only: false,
            },
            &mut human,
        )
        .unwrap();
        let human = String::from_utf8(human).unwrap();
        assert!(!human.contains('\u{1b}'));
        assert!(human.contains("é\\u{1b}"));
    }
}
#[test]
fn cli_recorded_remedies_preserve_typed_causes() {
    use provenant_fabric_runtime::coordination::facade::{DispatchRefusalFact, FacadeError};
    use provenant_records::diagnostic::{ResourceFact, SettingSource};
    let cases = [
        (
            FacadeError::DispatchRefused {
                fact: Some(DispatchRefusalFact::DirtyWriter),
                disk: None,
                reason: "TOKEN_SENTINEL".into(),
            },
            "commit",
        ),
        (
            FacadeError::DispatchRefused {
                fact: Some(DispatchRefusalFact::Resource(ResourceFact::Disk {
                    free: 1,
                    floor: 2,
                    origin: SettingSource::Configuration,
                })),
                disk: None,
                reason: "TOKEN_SENTINEL".into(),
            },
            "free disk space",
        ),
        (
            FacadeError::LaunchUnknown {
                operation_id: OperationId::parse("dispatch-17").unwrap(),
            },
            "dispatch-17",
        ),
    ];
    for (error, condition) in cases {
        let projection = error.diagnostic();
        let failure = super::Fail::from(provenant_run::RunError::RecordedRefusal(Box::new(
            projection.clone(),
        )));
        let reply = super::reply("run", Err(failure));
        let wire = serde_json::to_value(&reply.result).unwrap();
        assert_eq!(wire["error"]["next"], projection.next.unwrap());
        let action = wire["error"]["next"].as_str().unwrap();
        assert!(action.contains(condition), "{wire}");
        if condition == "dispatch-17" {
            assert!(action.contains("inspect") && action.contains("same operation ID"));
        }
        assert!(!wire.to_string().contains("TOKEN_SENTINEL"));
    }
}
#[test]
fn malformed_claude_result_preserves_provider_error_in_settlement() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/fixture".into()),
        ..Default::default()
    };
    let message = "Permission mode forced to default — CLAUDE_CODE_SUBPROCESS_ENV_SCRUB is set";
    let result =
        serde_json::json!({"type":"result", "is_error":true, "errors":[message]}).to_string();
    let earlier = r#"{"type":"result","session_id":"s","subtype":"success","is_error":false,"result":"earlier answer"}"#;
    for (result, reason) in [
        (
            result.as_str(),
            format!("provider protocol is malformed: {message}"),
        ),
        ("{", "provider protocol is malformed".into()),
    ] {
        for suffix in ["", "\n"] {
            let decoded = Registry::default()
                .decode(
                    &route,
                    format!("{earlier}\n{result}{suffix}").as_bytes(),
                    &clean_exit(),
                    Path::new("/unused"),
                )
                .unwrap();
            assert_eq!(decoded.outcome, run::Outcome::Failed);
            assert_eq!(decoded.reason.as_deref(), Some(reason.as_str()));
        }
    }
}
#[test]
fn output_retains_safe_refusal_remedies() -> Result<(), Box<dyn std::error::Error>> {
    struct Refusing {
        note: String,
    }
    impl run::Providers for Refusing {
        fn prepare(
            &self,
            _: &run::TaskRequest,
            _: &str,
            _: &run::LaunchPaths,
            _: Option<&run::AttemptRecord>,
        ) -> Result<run::PreparedLaunch, run::PrepareRefusal> {
            Err(run::PrepareRefusal {
                boundary_class: provenant_records::diagnostic::BoundaryClass::None,
                policy_code: None,
                resource: None,
                terminal_cause: provenant_records::run::TerminalCause::AccessPolicyRefused,
                terminal_note: Some(self.note.clone()),
                message: "PROVIDER_SENTINEL must not become a public diagnostic".into(),
                skipped_routes: Vec::new(),
                fallback_outcome: None,
            })
        }
        fn decode(
            &self,
            _: &run::RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &Path,
        ) -> Result<run::Decoded, String> {
            unreachable!("a refused launch never reaches a provider")
        }
    }
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
            let session = open(edge, &call).map_err(|fail| fail.message)?;
            let prepared = trace_run(10, AttemptId::from_bytes([99; 16]), clock.wall_now()?)?;
            session.store.create(&prepared, &call)?;
            let note = format!(
                "configured access boundary: {} END_SAFE_NOTE",
                "fixture setting ".repeat(24)
            );
            assert!(note.len() > 160);
            session.store.start(
                prepared.attempt_id,
                &Refusing { note: note.clone() },
                &|_, _| unreachable!(),
                &call,
            )?;
            let expected = format!(
                "{}; {note}",
                provenant_records::run::TerminalCause::AccessPolicyRefused.detail()
            );
            let attempt = prepared.attempt_id;
            let output_command = || Output {
                target: crate::Target::parse("g/n").expect("fixture target"),
                part: Part::Result,
                to: None,
                path: false,
                offset: 0,
                tail: None,
                max_bytes: 65_536,
            };
            let reply = output_from_session(output_command(), edge, &session, attempt, &call);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            assert_eq!(reply.result.data["reason"], expected);
            assert!(reply.human.contains(&expected));
            assert!(!serde_json::to_string(&reply.result.data)?.contains("PROVIDER_SENTINEL"));
            let mut bytes = Vec::new();
            crate::render(
                &reply.result,
                crate::RenderMode::Compact {
                    text: &reply.human,
                    read_only: true,
                    summary: reply.summary.as_ref(),
                },
                &mut bytes,
            )?;
            let default = String::from_utf8(bytes)?;
            assert!(default.contains(&expected), "{default}");
            assert_eq!(default.matches("END_SAFE_NOTE").count(), 1, "{default}");
            assert!(!default.contains("PROVIDER_SENTINEL"));
            Ok(())
        },
    )?
}
#[test]
fn explicit_output_keeps_the_disk_sample() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt;
    let reason = "host capacity: disk space: 23622320128 bytes available; gc.free_floor is 32212254720 bytes (source: host configuration)";
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
            let session = open(edge, &call).map_err(|fail| fail.message)?;
            std::fs::write(
                edge.roots.state.as_path().join("config.toml"),
                "schema_version=1\n[gc]\nfree_floor='32212254720B'\n",
            )?;
            let mut prepared = trace_run(10, AttemptId::from_bytes([99; 16]), clock.wall_now()?)?;
            prepared.request.task.writer = Some(run::WriterRequest {
                extra: Default::default(),
                project: fixture.project.clone(),
                worktree: fixture.tree.clone(),
            });
            session.store.create(&prepared, &call)?;
            let mut lane = session.store.record(prepared.attempt_id, &call)?;
            lane.state = run::AttemptState::Starting {
                extra: Default::default(),
            };
            let database = rusqlite::Connection::open(
                session.context.state_root.as_path().join("run.sqlite3"),
            )?;
            assert_eq!(
                database.execute(
                    "UPDATE attempts SET record=?1 WHERE id=?2",
                    rusqlite::params![serde_json::to_vec(&lane)?, lane.attempt_id.to_string()]
                )?,
                1
            );
            // Admission reads private guardian checkpoints. Match their mode
            // rather than inheriting the test runner's umask.
            let mut sample = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(artifact(
                    session.context.state_root.as_path(),
                    &lane,
                    "admission-disk.json",
                ))?;
            std::io::Write::write_all(
                &mut sample,
                &serde_json::to_vec(&json!({"generation":lane.generation,"free":23622320128_u64}))?,
            )?;
            drop(sample);
            let command = || Output {
                target: crate::Target::parse("g/n").expect("fixture target"),
                part: Part::Result,
                to: None,
                path: false,
                offset: 0,
                tail: None,
                max_bytes: 65_536,
            };
            let reply = output_from_session(command(), edge, &session, lane.attempt_id, &call);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            let (data, text) = (reply.result.data, reply.human);
            let expected = format!("{reason}; free disk space or run `provenant gc --apply`");
            assert_eq!(data["reason"], expected);
            assert!(text.contains(&expected), "{text}");
            for field in ["length", "digest", "complete", "clipped"] {
                assert!(data[field].is_null(), "{field}: {data}");
            }
            assert_eq!(data["bytes"]["data"], "");
            let destination = fixture.root.join("preserved.txt");
            std::fs::write(&destination, "existing content")?;
            let mut save = command();
            save.to = Some(destination.clone());
            let reply = output_from_session(save, edge, &session, lane.attempt_id, &call);
            assert!(reply.result.error.is_some(), "{:?}", reply.result);
            assert_eq!(std::fs::read_to_string(destination)?, "existing content");
            Ok(())
        },
    )?
}
