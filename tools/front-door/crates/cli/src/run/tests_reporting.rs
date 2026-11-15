use super::*;

#[test]
fn non_refused_run_errors_keep_typed_causes_without_source_text() {
    use provenant_records::diagnostic::DiagnosticFact;
    let failure = super::Fail::from(provenant_run::RunError::OperationIo {
        operation: "read attempt",
        path: Some("/TOKEN_SENTINEL".into()),
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "TOKEN_SENTINEL"),
    });
    assert_eq!(failure.kind, ErrorKind::Io);
    let fact = failure
        .projection
        .as_ref()
        .and_then(|projection| projection.fact.as_ref());
    assert!(matches!(
        fact,
        Some(DiagnosticFact::Filesystem {
            operation: "read attempt",
            cause: Some(_),
        })
    ));
    assert!(
        !serde_json::to_string(&super::reply("telemetry stats", Err(failure)).result)
            .unwrap()
            .contains("TOKEN_SENTINEL")
    );

    let failure = super::Fail::from(provenant_run::RunError::Contention);
    assert_eq!(failure.kind, ErrorKind::Contention);
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
        assert!(detail.starts_with("é\\u{1b}"), "{detail:?}");
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
fn every_dispatch_refusal_fact_projects_a_safe_recovery_action() {
    use provenant_fabric_runtime::{
        coordination::facade::{DispatchRefusalFact, FacadeError},
        graph::{GraphOutputField, GraphStructuralLimit},
    };
    use provenant_records::diagnostic::{ResourceFact, SettingSource};
    let operation_id = OperationId::parse("dispatch-17").unwrap();
    let facts = [
        DispatchRefusalFact::DetachedWriter,
        DispatchRefusalFact::DirtyWriter,
        DispatchRefusalFact::CheckedOutWriter,
        DispatchRefusalFact::GraphSchema,
        DispatchRefusalFact::GraphComposite,
        DispatchRefusalFact::GraphPlan,
        DispatchRefusalFact::GraphStructuralLimit(GraphStructuralLimit {
            key: provenant_fabric_runtime::graph::GraphLimitKey::GraphPromptBytes,
            actual: 9,
            max: 8,
        }),
        DispatchRefusalFact::GraphOutputKind(GraphOutputField::Input),
        DispatchRefusalFact::GraphOutputKind(GraphOutputField::CompositeExport),
        DispatchRefusalFact::GraphEnvelope,
        DispatchRefusalFact::Resource(ResourceFact::Disk {
            free: 1,
            floor: 2,
            origin: SettingSource::Configuration,
        }),
    ];
    for fact in facts {
        let projection = FacadeError::DispatchRefused {
            fact: Some(fact),
            disk: None,
            reason: "TOKEN_SENTINEL".into(),
        }
        .diagnostic();
        let expected_next = projection.next.clone();
        let action = projection.next.as_deref().expect("recovery action");
        match fact {
            DispatchRefusalFact::DetachedWriter => assert!(!action.is_empty()),
            DispatchRefusalFact::DirtyWriter => assert!(!action.is_empty()),
            DispatchRefusalFact::CheckedOutWriter => assert!(!action.is_empty()),
            DispatchRefusalFact::GraphSchema => assert!(!action.is_empty()),
            DispatchRefusalFact::GraphComposite => assert!(!action.is_empty()),
            DispatchRefusalFact::GraphPlan => assert!(!action.is_empty()),
            DispatchRefusalFact::GraphStructuralLimit(_) => assert!(!action.is_empty()),
            DispatchRefusalFact::GraphOutputKind(_) => assert!(!action.is_empty()),
            DispatchRefusalFact::GraphEnvelope => assert!(!action.is_empty()),
            DispatchRefusalFact::Resource(_) => assert!(!action.is_empty()),
        }
        assert!(!action.contains("TOKEN_SENTINEL"));
        let failure = super::Fail::from(provenant_run::RunError::RecordedRefusal(Box::new(
            projection,
        )));
        let wire = serde_json::to_value(&super::reply("run", Err(failure)).result).unwrap();
        assert_eq!(wire["error"]["next"].as_str(), expected_next.as_deref());
        assert!(!wire.to_string().contains("TOKEN_SENTINEL"));
    }

    let projection = FacadeError::LaunchUnknown {
        operation_id: operation_id.clone(),
    }
    .diagnostic();
    assert!(
        projection
            .next
            .as_deref()
            .unwrap()
            .contains(&operation_id.to_string())
    );
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
    for result in [result.as_str(), "{"] {
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
            assert!(!decoded.complete);
            assert!(decoded.result.is_empty());
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
        ) -> Result<run::PreparedLaunch, Box<run::PrepareRefusal>> {
            Err(Box::new(run::PrepareRefusal {
                boundary_class: provenant_records::diagnostic::BoundaryClass::None,
                policy_code: None,
                resource: None,
                terminal_cause: provenant_records::run::TerminalCause::AccessPolicyRefused,
                terminal_note: Some(self.note.clone()),
                message: "PROVIDER_SENTINEL must not become a public diagnostic".into(),
                skipped_routes: Vec::new(),
                fallback_outcome: None,
            }))
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
