use super::*;

#[test]
fn admitted_invalid_launch_preserves_schema_cause_and_safe_projection() {
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
    assert_eq!(
        progress.advance_error_code.as_deref(),
        Some("launch_refused")
    );
    assert_eq!(
        progress.advance_owner_code.as_deref(),
        Some("invalid_input")
    );
    let reason = progress.advance_error.as_deref().unwrap();
    // Schema is the typed owner detail; the projection retains its safe field.
    assert!(reason.contains("schema_version"));
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
    let progress = &run.checkpoint.nodes[0];
    assert_eq!(
        progress.advance_error_code.as_deref(),
        Some("launch_refused")
    );
    assert_eq!(
        progress.advance_owner_code.as_deref(),
        Some("invalid_input")
    );
    assert!(
        !progress
            .advance_error
            .as_deref()
            .unwrap()
            .contains("TOKEN_SENTINEL")
    );
}

#[test]
fn admitted_invalid_launch_records_typed_input_failure_category() {
    use crate::coordination::facade::{FacadeError, InvalidInputDetail};
    for detail in [
        InvalidInputDetail::InputTooLarge,
        InvalidInputDetail::InputUnreadable,
    ] {
        let document = graph(vec![node("one", route("first"))]);
        let mut owner = ErrorLauncher {
            now: Some(at(0)),
            errors: [(1, FacadeError::invalid(detail))].into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let progress = &run.checkpoint.nodes[0];
        assert_eq!(progress.state, NodeState::Blocked);
        assert_eq!(
            progress.advance_error_code.as_deref(),
            Some("launch_refused")
        );
        assert_eq!(
            progress.advance_owner_code.as_deref(),
            Some("invalid_input")
        );
        let reason = progress.advance_error.as_deref().unwrap();
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
    }))
    .unwrap();
    let SubmitInput::Submit { task, .. } = &mut input;
    let prompt_limit = crate::coordination::input::MAX_CLI_PROMPT_BYTES;
    task.prompt = format!("TOKEN_SENTINEL{}", "é".repeat(prompt_limit / "é".len() + 1));
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
    let progress = &run.checkpoint.nodes[0];
    assert_eq!(
        progress.advance_error_code.as_deref(),
        Some("launch_refused")
    );
    assert_eq!(
        progress.advance_owner_code.as_deref(),
        Some("invalid_input")
    );
    let reason = progress.advance_error.as_deref().unwrap();
    assert!(reason.contains("task.prompt"));
    assert!(reason.contains(&prompt_limit.to_string()));
    assert!(!reason.contains("TOKEN_SENTINEL"));
    assert!(reason.len() <= MAX_ADVANCE_ERROR_BYTES);
}

#[test]
fn memory_pressure_launch_refusal_retains_typed_cause() {
    use crate::coordination::facade::FacadeError;
    use provenant_records::diagnostic::{
        PressureLevel, PressureThreshold, ResourceFact, SettingSource,
    };
    let resource = ResourceFact::MemoryPressure {
        level: PressureLevel::Critical,
        threshold: PressureThreshold::Critical,
        origin: SettingSource::ShippedDefault,
    };
    let error = FacadeError::ConfinementRefused {
        resource: Some(resource),
    };
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.code, "confinement_refused");
    let detail = diagnostic.detail.as_deref().unwrap();
    assert!(detail.contains(PressureLevel::Critical.name()));
    assert!(detail.contains(PressureThreshold::Critical.name()));
    assert!(detail.contains(SettingSource::ShippedDefault.description()));
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
    let progress = &run.checkpoint.nodes[0];
    assert_eq!(
        progress.advance_owner_code.as_deref(),
        Some("confinement_refused")
    );
    assert!(progress.advance_error.is_some());
}

#[test]
fn process_limit_launch_projection_keeps_io_code_and_redacts_sources() {
    let resource = provenant_records::diagnostic::ResourceFact::Processes {
        used: 950,
        limit: 1000,
        reserved: 100,
    };
    let error = crate::coordination::facade::FacadeError::Io {
        operation: "TOKEN_SENTINEL".into(),
        path: Some("/private/host".into()),
        delivery_limit: None,
        delivery_stage: None,
        resource: Some(Box::new(resource)),
        input_failure: None,
        cause: None,
    };
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.code, "io");
    let detail = diagnostic.detail.as_deref().unwrap();
    let (used, limit, reserved) = match resource {
        provenant_records::diagnostic::ResourceFact::Processes {
            used,
            limit,
            reserved,
        } => (used, limit, reserved),
        _ => unreachable!("the fixture carries a process resource fact"),
    };
    for fact in [used, limit, reserved] {
        assert!(detail.contains(&fact.to_string()));
    }
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
    let checkpoint = journal.0.as_ref().unwrap();
    let progress = &checkpoint.nodes[0];
    assert_eq!(progress.advance_owner_code.as_deref(), Some("io"));
    let reason = retry_of(checkpoint).reason.as_deref().unwrap();
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
        ("task.title".to_owned(), Some("task.title")),
        ("task.TOKEN_SENTINEL\u{1b}".to_owned(), None),
        (format!("task.TOKEN_SENTINEL{}", "é".repeat(2048)), None),
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
            "dispatch_refused",
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
            "dispatch_refused",
            false,
        ),
        (
            FacadeError::LaunchUnknown {
                operation_id: provenant_core::OperationId::parse("dispatch-17").unwrap(),
            },
            "LAUNCH_UNKNOWN",
            true,
        ),
    ];
    for (error, expected_code, uncertain) in cases {
        assert_eq!(error.diagnostic().code, expected_code);
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
        assert_eq!(progress.advance_owner_code.as_deref(), Some(expected_code));
        let reason = if uncertain {
            retry_of(journal.0.as_ref().unwrap())
                .reason
                .as_deref()
                .unwrap()
        } else {
            progress.advance_error.as_deref().unwrap()
        };
        assert!(!reason.contains("TOKEN_SENTINEL"));
        if uncertain {
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
fn graph_plan_refusal_preserves_typed_fact_and_safe_detail() {
    use crate::coordination::facade::{FacadeError, InvalidInputDetail};
    let error = FacadeError::graph_admission(crate::graph::AdmissionError::Plan(
        "node implement: writer worktree".into(),
    ));
    let diagnostic = error.diagnostic();
    assert!(matches!(
        &error,
        FacadeError::DispatchRefused {
            fact: Some(crate::coordination::facade::DispatchRefusalFact::GraphPlan),
            ..
        }
    ));
    assert_eq!(diagnostic.code, "dispatch_refused");
    assert!(
        diagnostic
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("implement"))
    );
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
    let document = document_fixture(serde_json::json!({
        "graph_id":"long-identifiers","nodes":[{
            "id":"lanes","kind":"composite","access":"read_only",
            "composite":{"nodes":[{"id":"lane-cuts-quarantine","kind":"composite","access":"read_only",
              "composite":{"nodes":[{"id":"lens--review--lens-conventions-readability","access":"read_only","prompt":"review","route":{"alias":"junior"}}],"exports":[]}}],"exports":[]}
        }]
    }));
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
fn launched_reader_stop_restart_replays_file_checkpoint_without_new_work() {
    use provenant_core::{Clock, ProjectId};
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let inputs = provenant_test_support::root_inputs(&base).unwrap();
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
            peer_origin_context: None,
            peer_submission_digest: None,

            extra: Default::default(),
            bases: None,
            directory: inputs.checkout.clone(),
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
    assert_eq!(run.checkpoint.nodes[0].recorded_attempts().count(), 2);
    assert_eq!(owner.calls.len(), 2);
    assert_ne!(owner.attempt_numbers[0], owner.attempt_numbers[1]);
    drop(journal);
    let mut journal =
        FileJournal::open(&root, project, &document.graph_id, budget, &cancel, &clock).unwrap();
    let mut replay_owner = FakeOwner::default();
    let replay = advance(&document, &mut replay_owner, &mut journal).unwrap();
    assert!(replay_owner.calls.is_empty());
    assert_eq!(replay.checkpoint, run.checkpoint);
    assert_eq!(replay.checkpoint.nodes[0].state, NodeState::Succeeded);
}

#[test]
fn untyped_launch_refusal_persists_reason_without_an_attempt() {
    use crate::coordination::facade::FacadeError;
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let document = graph(vec![node("one", route("first"))]);
    let mut owner = ErrorLauncher {
        now: Some(at(0)),
        errors: [(
            1,
            FacadeError::DispatchRefused {
                reason: format!("worktree setup failed: missing dependency {secret}"),
                disk: None,
                fact: None,
            },
        )]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let node = &run.checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Blocked);
    assert!(
        node.attempts
            .iter()
            .all(|attempt| attempt.attempt_ref.is_none())
    );
    assert!(
        node.attempts
            .iter()
            .all(|attempt| attempt.host_refused_before_acceptance)
    );
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
