use super::*;

#[test]
fn admitted_invalid_launch_projects_safe_schema_reason_and_action() {
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(
            1,
            crate::coordination::facade::FacadeError::InvalidInput {
                detail: Some(crate::coordination::facade::InvalidInputDetail::Schema),
            },
        )]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let result = advance(&document, &mut owner, &mut journal).unwrap();
    let progress = &result.checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Blocked);
    let reason = progress.advance_error.as_deref().unwrap();
    assert!(reason.contains("invalid_input"));
    assert!(reason.contains("schema_version"));
    assert!(reason.contains("check the named input field"));
    assert!(!reason.contains("TOKEN_SENTINEL"));
    assert!(!reason.chars().any(char::is_control));

    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(
            1,
            crate::coordination::facade::FacadeError::InvalidInput { detail: None },
        )]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let reason = run.checkpoint.nodes[0].advance_error.as_deref().unwrap();
    assert!(reason.contains("invalid_input"));
    assert!(reason.contains("check the input"));
}

#[test]
fn admitted_invalid_launch_reports_input_failure_reason() {
    use crate::coordination::facade::{FacadeError, InvalidInputDetail};
    for (detail, expected) in [
        (InvalidInputDetail::InputTooLarge, "1 MiB"),
        (InvalidInputDetail::InputUnreadable, "unreadable"),
    ] {
        let document = graph(vec![node("one", route("first"))]);
        let mut owner = ErrorLauncher {
            now: Some(at(0)),
            errors: [(1, FacadeError::invalid(detail))].into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let node = &run.checkpoint.nodes[0];
        assert_eq!(node.state, NodeState::Blocked);
        let reason = node.advance_error.as_deref().unwrap();
        assert!(reason.contains("invalid_input"));
        assert!(reason.contains("input"));
        assert!(reason.contains(expected));
        assert!(reason.contains("retry"));
        assert!(!reason.contains("TOKEN_SENTINEL"));
    }
}

#[test]
fn admitted_launch_projects_typed_prompt_field_and_limit() {
    use crate::coordination::input::ValidateInput;
    use crate::coordination::{
        CoordinationError,
        facade::{FacadeError, SubmitInput},
    };
    let mut input: SubmitInput = serde_json::from_value(serde_json::json!({
        "op":"submit", "operation_id":"field-limit",
        "task":{"prompt":"valid", "route":{"kind":"named", "name":"utility"}, "timeout_seconds":1}
    })).unwrap();
    let SubmitInput::Submit { task, .. } = &mut input;
    task.prompt = format!("TOKEN_SENTINEL{}", "é".repeat(524_289));
    let cause = input
        .validate_input(provenant_core::WaitTransport::Cli)
        .unwrap_err();
    let error = FacadeError::from(&CoordinationError::Input(cause));
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(1, error)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(
        &graph(vec![node("one", route("first"))]),
        &mut owner,
        &mut journal,
    )
    .unwrap();
    let reason = run.checkpoint.nodes[0].advance_error.as_deref().unwrap();
    assert!(reason.contains("tasks[0].prompt"));
    assert!(reason.contains("1048576"));
    assert!(reason.contains("retry"));
    assert!(!reason.contains("TOKEN_SENTINEL"));
    assert!(reason.len() <= MAX_ADVANCE_ERROR_BYTES);
}

#[test]
fn memory_pressure_launch_refusal_names_source_and_action() {
    use crate::coordination::facade::FacadeError;
    use provenant_records::diagnostic::{
        PressureLevel, PressureThreshold, ResourceFact, SettingSource,
    };
    let error = FacadeError::ConfinementRefused {
        resource: Some(ResourceFact::MemoryPressure {
            level: PressureLevel::Critical,
            threshold: PressureThreshold::Critical,
            origin: SettingSource::ShippedDefault,
        }),
    };
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(1, error)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(
        &graph(vec![node("one", route("first"))]),
        &mut owner,
        &mut journal,
    )
    .unwrap();
    let reason = run.checkpoint.nodes[0].advance_error.as_deref().unwrap();
    assert!(reason.contains("memory_pressure_threshold"));
    assert!(reason.contains("shipped default"));
    assert!(reason.contains("reduce host memory pressure"));
}

#[test]
fn process_limit_launch_projection_keeps_io_and_resource_reason() {
    let error = crate::coordination::facade::FacadeError::Io {
        operation: "TOKEN_SENTINEL".into(),
        path: Some("/private/host".into()),
        delivery_limit: None,
        delivery_stage: None,
        resource: Some(Box::new(
            provenant_records::diagnostic::ResourceFact::Processes {
                used: 950,
                limit: 1000,
                reserved: 100,
            },
        )),
        input_failure: None,
        cause: None,
    };
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(1, error)].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(
        &graph(vec![node("one", route("first"))]),
        &mut owner,
        &mut journal,
    )
    .unwrap();
    let reason = retry_of(journal.0.as_ref().unwrap())
        .reason
        .as_deref()
        .unwrap();
    assert!(reason.contains("io"));
    assert!(reason.contains("RLIMIT_NPROC"));
    assert!(reason.contains("process"));
    assert!(!reason.contains("TOKEN_SENTINEL"));
    assert!(!reason.contains("/private/host"));
}

#[test]
fn unreadable_launch_input_keeps_io_code_and_safe_reason() {
    let error = || crate::coordination::facade::FacadeError::Io {
        operation: "TOKEN_SENTINEL".into(),
        path: Some("/private/host".into()),
        delivery_limit: None,
        delivery_stage: None,
        resource: None,
        input_failure: Some(provenant_records::diagnostic::IoFact::InputUnreadable),
        cause: None,
    };
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: (1..=4).map(|call| (call, error())).collect(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(
        &graph(vec![node("one", route("first"))]),
        &mut owner,
        &mut journal,
    )
    .unwrap();
    let reason = retry_of(journal.0.as_ref().unwrap())
        .reason
        .as_deref()
        .unwrap();
    assert!(reason.contains("io"));
    assert!(reason.contains("input could not be read"));
    assert!(reason.contains("retry"));
    assert!(!reason.contains("TOKEN_SENTINEL"));
    assert!(!reason.contains("/private/host"));
    for now in [6, 17, 38, 3600] {
        owner.now = Some(at(now));
        advance(
            &graph(vec![node("one", route("first"))]),
            &mut owner,
            &mut journal,
        )
        .unwrap();
    }
    let node = &journal.0.as_ref().unwrap().nodes[0];
    assert_eq!(node.state, NodeState::Blocked);
    let reason = node.advance_error.as_deref().unwrap();
    assert!(reason.contains("input could not be read"));
    assert!(reason.contains("io"));
    assert!(!reason.contains("TOKEN_SENTINEL"));
    assert!(!reason.contains("/private/host"));
}

#[test]
fn launch_projection_omits_untrusted_error_sources() {
    let document = graph(vec![node("one", route("first"))]);
    for operation in [
        "TOKEN_SENTINEL\u{1b}".to_owned(),
        format!("TOKEN_SENTINEL{}", "é".repeat(2048)),
    ] {
        let mut owner = ErrorLauncher {
            now: Some(at(0)),
            errors: [(
                1,
                crate::coordination::facade::FacadeError::Io {
                    delivery_limit: None,
                    delivery_stage: None,
                    resource: None,
                    input_failure: None,
                    cause: None,
                    operation,
                    path: Some("/private/host".into()),
                },
            )]
            .into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        advance(&document, &mut owner, &mut journal).unwrap();
        let reason = retry_of(journal.0.as_ref().unwrap())
            .reason
            .as_deref()
            .unwrap();
        assert!(!reason.contains("TOKEN_SENTINEL"));
        assert!(!reason.contains("/private/host"));
        assert!(!reason.chars().any(char::is_control));
        assert!(reason.len() <= MAX_ADVANCE_ERROR_BYTES);
    }
}

#[test]
fn launch_invalid_field_matrix_persists_only_approved_facts() {
    use crate::coordination::facade::{FacadeError, InvalidInputDetail};
    for (field, expected) in [
        ("tasks[0].title".to_owned(), Some("tasks[0].title")),
        ("tasks[0].TOKEN_SENTINEL\u{1b}".to_owned(), None),
        (format!("tasks[0].TOKEN_SENTINEL{}", "é".repeat(2048)), None),
    ] {
        let mut owner = ErrorLauncher {
            now: Some(at(0)),
            errors: [(
                1,
                FacadeError::invalid(InvalidInputDetail::FieldTooLarge { field, limit: 17 }),
            )]
            .into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        advance(
            &graph(vec![node("one", route("first"))]),
            &mut owner,
            &mut journal,
        )
        .unwrap();
        let reason = journal.0.as_ref().unwrap().nodes[0]
            .advance_error
            .as_deref()
            .unwrap();
        assert!(!reason.contains("TOKEN_SENTINEL"));
        assert!(!reason.chars().any(char::is_control));
        assert!(reason.len() <= MAX_ADVANCE_ERROR_BYTES);
        if let Some(field) = expected {
            assert!(reason.contains(field) && reason.contains("17"), "{reason}");
        }
        assert!(reason.contains("retry"), "{reason}");
    }
}

#[test]
fn graph_remedies_follow_writer_resource_and_unknown_handoff_causes() {
    use crate::coordination::facade::{DispatchRefusalFact, FacadeError};
    use provenant_records::diagnostic::{ResourceFact, SettingSource};
    let cases = [
        (
            FacadeError::DispatchRefused {
                fact: Some(DispatchRefusalFact::DirtyWriter),
                disk: None,
                reason: "TOKEN_SENTINEL".into(),
            },
            "commit",
            false,
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
            false,
        ),
        (
            FacadeError::LaunchUnknown {
                operation_id: provenant_core::OperationId::parse("dispatch-17").unwrap(),
            },
            "dispatch-17",
            true,
        ),
    ];
    for (error, condition, uncertain) in cases {
        let mut owner = ErrorLauncher {
            now: Some(at(0)),
            errors: [(1, error)].into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        advance(
            &graph(vec![node("one", route("first"))]),
            &mut owner,
            &mut journal,
        )
        .unwrap();
        let progress = &journal.0.as_ref().unwrap().nodes[0];
        let reason = if uncertain {
            retry_of(journal.0.as_ref().unwrap())
                .reason
                .as_deref()
                .unwrap()
        } else {
            progress.advance_error.as_deref().unwrap()
        };
        assert!(reason.contains(condition), "{reason}");
        assert!(!reason.contains("TOKEN_SENTINEL"));
        if uncertain {
            assert!(
                reason.contains("inspect") && reason.contains("same operation ID"),
                "{reason}"
            );
            assert!(!reason.contains("provenant retry"));
            owner.now = Some(at(6));
            advance(
                &graph(vec![node("one", route("first"))]),
                &mut owner,
                &mut journal,
            )
            .unwrap();
            assert_eq!(owner.calls.len(), 2);
            assert_eq!(
                owner.calls[0], owner.calls[1],
                "unknown handoff replays the same attempt key"
            );
        }
    }
}

#[test]
fn graph_plan_refusal_preserves_node_reason_and_writer_remedy() {
    use crate::coordination::facade::{FacadeError, InvalidInputDetail};
    let error = FacadeError::graph_admission(crate::graph::AdmissionError::Plan(
        "node implement: writer worktree".into(),
    ));
    let diagnostic = error.diagnostic();
    assert!(error.to_string().contains("node implement: writer worktree"));
    assert_eq!(
        diagnostic.detail.as_deref(),
        Some("node implement: writer worktree")
    );
    assert!(diagnostic
        .next
        .as_deref()
        .is_some_and(|next| next.contains("--worktree PATH") && next.contains("--branch B --base REF")));
    assert!(!matches!(
        error,
        FacadeError::InvalidInput {
            detail: Some(InvalidInputDetail::GraphReason(_))
        }
    ));
}

#[test]
fn writer_binding_failure_persists_only_the_safe_projection() {
    use crate::coordination::facade::FacadeError;
    let mut writer = node("writer", route("first"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some("/fixture/worktree".into());
    let mut owner = ErrorLauncher {
        binding_error: Some(FacadeError::Io {
            operation: format!("TOKEN_SENTINEL\u{1b}{}", "é".repeat(2048)),
            path: Some("/private/host".into()),
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
            cause: None,
        }),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![writer]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let reason = run.checkpoint.nodes[0].advance_error.as_deref().unwrap();
    assert!(reason.contains("io"));
    assert!(!reason.contains("TOKEN_SENTINEL"));
    assert!(!reason.contains("/private/host"));
    assert!(!reason.chars().any(char::is_control));
    assert!(reason.len() <= MAX_ADVANCE_ERROR_BYTES);

    let mut writer = node("writer", route("first"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some("/fixture/worktree".into());
    let mut owner = ErrorLauncher {
        binding_error: Some(FacadeError::DispatchRefused {
            fact: Some(crate::coordination::facade::DispatchRefusalFact::DirtyWriter),
            reason: "TOKEN_SENTINEL".into(),
            disk: None,
        }),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![writer]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let reason = run.checkpoint.nodes[0].advance_error.as_deref().unwrap();
    assert!(reason.contains("uncommitted"));
    assert!(reason.contains("commit"));
    assert!(!reason.contains("TOKEN_SENTINEL"));
}

#[test]
fn graph_advance_preserves_typed_admission_refusal() {
    let mut document = graph(vec![node("one", route("first"))]);
    document.schema_version = 99;
    let mut journal = MemoryJournal::default();
    let error = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap_err();
    assert_eq!(error.code(), "invalid_graph");
    assert!(matches!(
        error,
        GraphError::Admission(AdmissionError::Schema(_))
    ));
    assert_eq!(error.to_string(), "invalid graph: schema version");
    assert!(journal.0.is_none());
}

#[test]
fn failed_parent_waits_for_sibling_before_blocking_join() {
    let first = node("first", route("fixture"));
    let second = node("second", route("fixture"));
    let mut join = node("join", route("fixture"));
    join.depends_on = vec!["first".into(), "second".into()];
    let document = graph(vec![first, second, join]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal(AttemptOutcome::TaskFailed),
            AttemptObservation::Pending { attempt_ref: None },
        ]),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "join")
            .unwrap()
            .state,
        NodeState::Pending
    );
    owner
        .observations
        .push_back(terminal(AttemptOutcome::Succeeded));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "second")
            .unwrap()
            .state,
        NodeState::Succeeded
    );
    assert_eq!(
        run.checkpoint
            .nodes
            .iter()
            .find(|n| n.id == "join")
            .unwrap()
            .state,
        NodeState::Blocked
    );
}

#[test]
fn expanded_composite_identifiers_support_long_lane_names() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"long-identifiers","nodes":[{
            "id":"lanes","kind":"composite","access":"read_only",
            "composite":{"nodes":[{"id":"lane-cuts-quarantine","kind":"composite","access":"read_only",
              "composite":{"nodes":[{"id":"lens--review--lens-conventions-readability","access":"read_only","prompt":"review","route":{"alias":"junior"}}],"exports":[]}}],"exports":[]}
        }]
    })).unwrap();
    let expanded = composites::expand(&document, &crate::config::GraphLimits::default()).unwrap();
    assert!(
        expanded.nodes.iter().any(|node| node.id
            == "lanes--lane-cuts-quarantine--lens--review--lens-conventions-readability")
    );
    assert!(
        document
            .admit(&crate::config::GraphLimits::default())
            .is_ok()
    );
}

#[test]
fn launched_reader_fallback_reopens_file_checkpoint() {
    use provenant_core::{Clock, ProjectId};
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let mut task = node("reader", route("first"));
    task.route = None;
    task.routes = Some(vec![route("first"), route("second")]);
    let document = graph(vec![task]);
    let mut journal =
        FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
    journal
        .bind(GraphExecution {
            extra: Default::default(),
            review_base: None,
            integration_base: None,
            directory: base,
            document: document.clone(),
        })
        .unwrap();
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            terminal(AttemptOutcome::UsageLimited),
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.checkpoint.nodes[0].attempts.len(), 2);
    drop(journal);
    let mut journal =
        FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
    let replay = advance(&document, &mut FakeOwner::default(), &mut journal).unwrap();
    assert_eq!(replay.checkpoint.nodes[0].state, NodeState::Succeeded);
}

#[test]
fn untyped_launch_refusal_persists_reason_without_an_attempt() {
    use crate::coordination::facade::FacadeError;
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(1, FacadeError::DispatchRefused {
            reason: format!("worktree setup failed: missing dependency {secret}"),
            disk: None,
            fact: None,
        })].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let node = &run.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Blocked);
    assert!(node.attempts.iter().all(|attempt| attempt.attempt_ref.is_none()));
    assert!(node.attempts.iter().all(|attempt|
        attempt.extra.get(PRELAUNCH_REFUSAL_PROOF).and_then(serde_json::Value::as_bool) == Some(true)));
    let reason = node.advance_error.as_deref().unwrap();
    assert!(reason.contains("worktree setup failed: missing dependency"));
    assert!(!reason.contains(secret));
    assert!(!reason.contains("receipt"));
    // Re-read the persisted checkpoint as a later status request does.
    let encoded = serde_json::to_vec(journal.0.as_ref().unwrap()).unwrap();
    let retained: GraphCheckpoint = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(retained.nodes[0].advance_error, node.advance_error);
    let later = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(later.checkpoint.nodes[0].advance_error, node.advance_error);
}
