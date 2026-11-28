use super::*;
use provenant_core::{MonoTime, TestClock, WallTime};
use provenant_os::exec::{CapturedStream, ExecError, ExecOutcome};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn project_key(home: &Path) -> CrossHostProjectKey {
    CrossHostProjectKey::from_home(home, &home.join("repo/.git")).unwrap()
}

#[test]
fn host_read_and_gc_frames_preserve_tag_and_command_envelope() {
    for (tag, is_gc) in [("host_read", false), ("gc", true)] {
        let command = provenant_records::CommandResult::new(
            tag,
            provenant_records::Outcome::Ok,
            serde_json::json!({"value": 42}),
        );
        let result = if is_gc {
            PeerResult::Gc {
                data: Box::new(command),
                extra: Default::default(),
            }
        } else {
            PeerResult::HostRead {
                data: Box::new(command),
                extra: Default::default(),
            }
        };
        let bytes = serde_json::to_vec(&result).unwrap();
        let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(wire["result"], tag);
        assert_eq!(wire["data"]["data"]["value"], 42);
        let decoded: PeerResult =
            provenant_records::decode(bytes.as_slice(), Limits::CONFIG).unwrap();
        let data = match decoded {
            PeerResult::HostRead { data, .. } | PeerResult::Gc { data, .. } => data,
            _ => panic!("host command response expected"),
        };
        assert_eq!(data.command, tag);
        assert_eq!(data.status, provenant_records::Outcome::Ok);
        assert_eq!(data.data["value"], 42);
    }
}

#[test]
fn typed_output_frame_round_trips_graph_and_extension_fields() {
    let host = HostId::from_bytes([3; 16]);
    let origin = provenant_records::WorkKey::new(
        provenant_core::ProjectId::from_bytes([4; 16]),
        "graph",
        "review",
        2,
    )
    .unwrap();
    let output = facade::OutputResult::Chunk {
        peer: provenant_records::run::PeerGraphFields {
            peer_graph: Some(provenant_records::run::PeerGraph {
                host,
                graph_id: "graph".into(),
            }),
            peer_graph_collected: Some(true),
            peer_origin_work_key: Some(origin.clone()),
            ..Default::default()
        },
        length: Some(42),
        planner_fallback: Some("bounded planner failure".into()),
        extra: [(
            "future_output_field".into(),
            serde_json::json!({"kept":true}),
        )]
        .into(),
        target: facade::AttemptTarget {
            extra: Default::default(),
            attempt: provenant_core::AttemptId::from_bytes([5; 16]),
            host: Some(host),
        },
        part: facade::OutputPart::Result,
        state: "succeeded".into(),
        reason: None,
        offset: 0,
        next_offset: 2,
        eof: true,
        digest: provenant_core::Digest::from_bytes([6; 32]),
        bytes: facade::OutputBytes::Utf8("ok".into()),
    };
    let response = PeerResponse {
        extra: [("future_response_field".into(), serde_json::json!("kept"))].into(),
        version: 1,
        protocol: PROTOCOL,
        host,
        project: project_key(Path::new("/home/a")),
        result: PeerResult::Output {
            data: Box::new(output),
            extra: [("future_result_field".into(), serde_json::json!(true))].into(),
        },
    };

    // Production writes this typed response directly; the strict frame
    // decoder must see every field once and retain each layer's owner.
    let frame = serde_json::to_vec(&response).unwrap();
    let decoded: PeerResponse =
        provenant_records::decode(frame.as_slice(), Limits::CONFIG).unwrap();
    assert_eq!(decoded.extra["future_response_field"], "kept");
    let PeerResult::Output { data, extra } = decoded.result else {
        panic!("output response expected");
    };
    assert_eq!(extra["future_result_field"], true);
    let facade::OutputResult::Chunk {
        peer,
        length,
        planner_fallback,
        extra,
        ..
    } = *data
    else {
        panic!("output chunk expected");
    };
    assert_eq!(peer.peer_origin_work_key, Some(origin));
    assert_eq!(peer.peer_graph.unwrap().graph_id, "graph");
    assert_eq!(peer.peer_graph_collected, Some(true));
    assert_eq!(length, Some(42));
    assert_eq!(planner_fallback.as_deref(), Some("bounded planner failure"));
    assert_eq!(
        extra["future_output_field"],
        serde_json::json!({"kept":true})
    );
}

#[test]
fn peer_run_subtree_cursor_round_trips_and_rejects_wrong_types() {
    let action = PeerAction::Run {
        extra: [("future_action_fact".into(), serde_json::json!(true))].into(),
        input: facade::RunInput::Status {
            extra: Default::default(),
            target: facade::AttemptTarget {
                extra: Default::default(),
                attempt: provenant_core::AttemptId::from_bytes([9; 16]),
                host: None,
            },
        },
        peer_subtree_after: Some(3),
    };
    let encoded = serde_json::to_value(&action).unwrap();
    assert_eq!(encoded["peer_subtree_after"], 3);
    let decoded: PeerAction = serde_json::from_value(encoded.clone()).unwrap();
    let round_trip = serde_json::to_value(decoded).unwrap();
    assert_eq!(round_trip["peer_subtree_after"], 3);
    assert_eq!(round_trip["future_action_fact"], true);
    let mut malformed = encoded;
    malformed["peer_subtree_after"] = serde_json::json!("three");
    assert!(serde_json::from_value::<PeerAction>(malformed).is_err());
}

// Spec: docs/specs/fabric.md#publication-boundary
#[test]
fn output_audience_requires_graph_evidence_and_new_protocol() {
    let action = declared_feature_actions()
        .into_iter()
        .find(|(name, _, _)| *name == "output")
        .unwrap()
        .2;
    let mut wire = serde_json::to_value(&action).unwrap();
    assert_eq!(action_fence(&action), OUTPUT_AUDIENCE_PROTOCOL);
    assert_eq!(
        feature_protocol_for_wire_name("output"),
        OUTPUT_AUDIENCE_PROTOCOL
    );
    assert!(serde_json::from_value::<PeerAction>(wire.clone()).is_ok());
    wire.as_object_mut().unwrap().remove("audience");
    assert!(serde_json::from_value::<PeerAction>(wire.clone()).is_err());
    wire["audience"] = serde_json::json!({"kind":"graph_execution"});
    assert!(serde_json::from_value::<PeerAction>(wire).is_err());
}

#[test]
fn console_tree_subtree_is_optional_for_older_peer_messages() {
    let action = PeerAction::ConsoleTree {
        extra: Default::default(),
        max_depth: 8,
        max_nodes: 100,
        detail: false,
        selected: false,
        subtree: Some("attempt-root".into()),
    };
    let encoded = serde_json::to_value(&action).unwrap();
    assert_eq!(encoded["subtree"], "attempt-root");
    let decoded: PeerAction = serde_json::from_value(encoded.clone()).unwrap();
    assert!(
        matches!(decoded, PeerAction::ConsoleTree { subtree: Some(root), .. } if root == "attempt-root")
    );

    let mut old_message = encoded;
    old_message.as_object_mut().unwrap().remove("subtree");
    assert!(matches!(
        serde_json::from_value::<PeerAction>(old_message).unwrap(),
        PeerAction::ConsoleTree { subtree: None, .. }
    ));
}

// Spec: docs/specs/fabric.md#publication-boundary
#[test]
fn peer_bound_attempt_withholds_sender_paths_and_keeps_sealed_policy() {
    let root = Path::new("/sender/private");
    let sealed = provenant_records::run::LaunchPolicy {
        confidential: true,
        sources: vec![
            ".provenant/config.toml [launch_policy].protected_paths: inputs/brief".into(),
        ],
        repository_config: Some(".provenant/config.toml".into()),
    };
    let mut extra = provenant_records::ExtraFields::default();
    extra.insert(
        "sender_note".into(),
        serde_json::json!(root.join("note.md").to_string_lossy()),
    );
    let attempt = crate::graph::AttemptSpec {
        directory: Some(root.join("checkout").to_string_lossy().into_owned()),
        host: Some("workshop".into()),
        confidential: true,
        budget_sources: [("token_budget".into(), provenant_records::Layer::Launch)].into(),
        token_budget: Some(500),
        extra,
        write_scope: vec!["src".into()],
        account: Some("builder".into()),
        capabilities: vec!["postgres".into()],
        write_roots: vec![root.join("writes").to_string_lossy().into_owned()],
        new_worktree: Some(crate::coordination::facade::NewWorktree {
            extra: Default::default(),
            default_base: false,
            base: "HEAD".into(),
            branch: "lane".into(),
        }),
        admitted_by: Some("parent".into()),
        parent_sandboxed: true,
        launch_policy: Some(sealed.clone()),
        policy_inputs: vec![root.join("private/brief.md")],
        policy_sealed: true,
        ..Default::default()
    };
    let projected = attempt.peer_bound();
    let encoded = serde_json::to_string(&projected).unwrap();
    for path in [
        root.join("checkout"),
        root.join("private/brief.md"),
        root.join("writes"),
        root.join("note.md"),
    ] {
        let path = path.to_string_lossy().into_owned();
        assert!(
            !encoded.contains(&path),
            "{path} reached the peer: {encoded}"
        );
    }
    assert!(!encoded.contains("\"workshop\""));
    assert!(projected.directory.is_none());
    assert!(projected.policy_inputs.is_empty());
    assert!(projected.write_roots.is_empty());
    assert!(projected.write_scope.is_empty());
    // A branch and a base ref are peer-owned provisioning intent, not a
    // caller path: the receiver creates the worktree in its own clone.
    assert_eq!(
        projected.new_worktree,
        Some(crate::coordination::facade::NewWorktree {
            extra: Default::default(),
            default_base: false,
            base: "HEAD".into(),
            branch: "lane".into(),
        })
    );
    assert!(projected.extra.is_empty());
    assert!(!projected.policy_sealed);
    assert_eq!(projected.launch_policy, Some(sealed.clone()));
    assert!(projected.confidential);
    assert!(projected.parent_sandboxed);
    assert_eq!(projected.admitted_by.as_deref(), Some("parent"));
    assert_eq!(projected.token_budget, Some(500));

    // The wire form keeps that classification and still carries no path.
    let context = GraphSubmitContext {
        launch_actor: ActorId::from_bytes([4; 16]),
        attempt: projected,
        retry_limit: None,
        network: None,
    };
    let wire = serde_json::to_string(&context).unwrap();
    assert!(!wire.contains(root.to_string_lossy().as_ref()));
    let decoded: GraphSubmitContext = serde_json::from_str(&wire).unwrap();
    assert_eq!(decoded.attempt.launch_policy, Some(sealed));
    assert!(decoded.attempt.confidential);
    assert!(decoded.attempt.policy_inputs.is_empty());
}

#[test]
fn graph_peer_contexts_round_trip_and_reject_malformed_values() {
    let submit = GraphSubmitContext {
        launch_actor: ActorId::from_bytes([1; 16]),
        attempt: crate::graph::AttemptSpec::default(),
        retry_limit: Some(2),
        network: Some(provenant_records::run::NetworkAccess::Local),
    };
    let encoded = serde_json::to_value(&submit).unwrap();
    assert_eq!(
        serde_json::from_value::<GraphSubmitContext>(encoded.clone()).unwrap(),
        submit
    );
    for (field, bad) in [
        ("launch_actor", serde_json::json!(false)),
        ("attempt", serde_json::json!(false)),
        ("retry_limit", serde_json::json!("many")),
        ("network", serde_json::json!("unlisted")),
    ] {
        let mut malformed = encoded.clone();
        malformed[field] = bad;
        assert!(serde_json::from_value::<GraphSubmitContext>(malformed).is_err());
    }

    let receipt = GraphReceiptContext {
        launch_actor: ActorId::from_bytes([2; 16]),
        origin_work_key: provenant_records::WorkKey::new(
            provenant_core::ProjectId::from_bytes([5; 16]),
            "G",
            "N",
            1,
        )
        .unwrap(),
    };
    let encoded = serde_json::to_value(&receipt).unwrap();
    assert_eq!(
        serde_json::from_value::<GraphReceiptContext>(encoded.clone()).unwrap(),
        receipt
    );
    let mut malformed = encoded;
    malformed["origin_work_key"] = serde_json::json!(false);
    assert!(serde_json::from_value::<GraphReceiptContext>(malformed).is_err());

    let cancel = GraphCancelContext {
        launch_actor: ActorId::from_bytes([3; 16]),
    };
    let encoded = serde_json::to_value(&cancel).unwrap();
    assert_eq!(
        serde_json::from_value::<GraphCancelContext>(encoded.clone()).unwrap(),
        cancel
    );
    let mut malformed = encoded;
    malformed["launch_actor"] = serde_json::json!(false);
    assert!(serde_json::from_value::<GraphCancelContext>(malformed).is_err());
}

#[test]
fn peer_refusals_preserve_typed_errors_and_reject_unknown_codes() {
    let error = facade::FacadeError::LaunchUnknown {
        detail: None,
        operation_id: OperationId::parse("typed-error-operation").unwrap(),
    };
    let value = serde_json::to_value(PeerResult::Error {
        data: crate::hosts::PeerResultError {
            extra: Default::default(),
            error: error.clone(),
        },
        extra: Default::default(),
    })
    .unwrap();
    // The peer wire keeps its own tag; only the code a caller sees changed spelling.
    assert_eq!(value["data"]["error"]["code"], "launch_unknown");
    assert!(matches!(
        serde_json::from_value::<PeerResult>(value).unwrap(),
        PeerResult::Error { data: crate::hosts::PeerResultError { error: actual, .. }, .. } if actual == error
    ));
    for error in [
        serde_json::json!({"code": "unknown_error"}),
        serde_json::json!({"code": "launch_unknown"}),
    ] {
        assert!(serde_json::from_value::<facade::FacadeError>(error).is_err());
    }
}

#[test]
fn recorded_peer_responses_refuse_retired_protocols() {
    let response = PeerResponse {
        extra: Default::default(),
        version: 1,
        protocol: PROTOCOL - 1,
        host: HostId::from_bytes([3; 16]),
        project: project_key(Path::new("/home/a")),
        result: serde_json::json!({"result": "error", "data": {"error": {"code": "busy"}}}),
    };
    assert!(matches!(
        decode_recorded_response(&serde_json::to_vec(&response).unwrap()),
        Err(PeerError::Version)
    ));
}

#[test]
fn protocol_fence_precedes_action_decoding() {
    let mut request = PeerRequest {
        extra: Default::default(),
        release: None,
        release_probe: false,
        version: 1,
        protocol: PROTOCOL - 1,
        target_host: HostId::from_bytes([3; 16]),
        source_host: HostId::from_bytes([4; 16]),
        agent: "fixture".into(),
        project: ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/home/a")),
            common_dir: "repo/.git".into(),
        },
        action: serde_json::json!({"action": "future_action", "build_heavy": true}),
        trace: Trace::default(),
    };
    for protocol in [6, PROTOCOL - 1] {
        for action in [
            serde_json::json!({"action":"future_action"}),
            serde_json::json!({"action":"submit", "input":{"op":"submit", "task":"invalid shape", "origin_work_key":"new payload"}}),
        ] {
            request.protocol = protocol;
            request.action = action;
            let bytes = serde_json::to_vec(&request).unwrap();
            assert!(decode_request(&bytes).is_err());
            let response = protocol_refusal(&bytes).unwrap().unwrap();
            assert_eq!(response.protocol, PROTOCOL);
            assert!(matches!(
                response.result,
                PeerResult::Error {
                    data: crate::hosts::PeerResultError {
                        error: facade::FacadeError::PeerVersion,
                        ..
                    },
                    ..
                }
            ));
        }
    }
}

#[test]
fn validate_refuses_the_target_as_its_own_source() {
    let temp = crate::test_support::tempdir().unwrap();
    let home = temp.path();
    let local = HostId::from_bytes([1; 16]);
    let remote = HostId::from_bytes([2; 16]);
    let mut config = FabricConfig {
        agent_label: "fixture".into(),
        client_label: "fixture".into(),
        commands: Default::default(),
        host_native: Default::default(),
        ssh_executable: None,
        home_hosts: Default::default(),
        peers: Default::default(),
        graph: Default::default(),
        lanes: Default::default(),
        plan: Default::default(),
        implement: Default::default(),
        batch: Default::default(),
        queue: Default::default(),
        steer: Default::default(),
        limits: crate::config::Limits {
            body_bytes: 65536,
            input_frame_bytes: 1048576,
            json_depth: 64,
            page_rows: 100,
            output_lines: 500,
            output_bytes: 65536,
            result_bytes: 1048576,
            peer_seconds: 30,
            graph_timeout_seconds: 14400,
            store_busy_timeout_ms: 30_000,
        },
    };
    for (name, host) in [("self", local), ("fixture", remote)] {
        config.peers.insert(
            name.into(),
            Peer {
                host,
                destination: name.into(),
                remote_executable: None,
                connect_timeout_seconds: 10,
            },
        );
    }
    let mut request = PeerRequest {
        extra: Default::default(),
        release: None,
        release_probe: false,
        version: 1,
        protocol: PROTOCOL,
        target_host: local,
        source_host: local,
        trace: Default::default(),
        agent: "fixture".into(),
        project: ProjectLocator {
            extra: Default::default(),
            key: project_key(home),
            common_dir: "repo/.git".into(),
        },
        action: PeerAction::Health {
            extra: Default::default(),
        },
    };
    assert!(matches!(
        request.validate(local, &config, home),
        Err(PeerError::SourceHost)
    ));
    assert_eq!(
        request.validate(local, &config, home).unwrap_err().code(),
        "invalid_input"
    );
    assert_eq!(
        request
            .validate(local, &config, home)
            .unwrap_err()
            .to_string(),
        "peer source host must be a configured peer other than this host"
    );
    request.source_host = remote;
    assert!(request.validate(local, &config, home).is_ok());
    // A trace that names another host than the sender is refused.
    request.trace.host = Some(local);
    assert!(matches!(
        request.validate(local, &config, home),
        Err(PeerError::SourceHost)
    ));
    request.trace.host = Some(remote);
    assert!(request.validate(local, &config, home).is_ok());
    request.source_host = HostId::from_bytes([3; 16]);
    assert!(matches!(
        request.validate(local, &config, home),
        Err(PeerError::SourceHost)
    ));
}

// Spec: docs/specs/fabric.md#publication-boundary
#[test]
fn cross_host_submit_projects_credentials_before_encoding() {
    let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
    let local = HostId::from_bytes([1; 16]);
    let remote = HostId::from_bytes([2; 16]);
    let actor = crate::identity::Address::new(local, "origin")
        .unwrap()
        .actor;
    let project = ProjectLocator {
        extra: Default::default(),
        key: project_key(Path::new("/h")),
        common_dir: "repo/.git".into(),
    };
    let secret = "submit-private-value";
    let input = serde_json::from_value(serde_json::json!({
            "op":"submit", "operation_id":"submit-test",
            "task":{"prompt":format!("token={secret}"), "access":"read_only",
                "route":{"kind":"model","provider":"fixture","model":"fixture/model"}, "timeout_seconds":30}
        })).unwrap();
    let request = PeerRequest::new(
        remote,
        local,
        "worker".into(),
        project,
        PeerAction::Submit {
            extra: Default::default(),
            graph_context: Box::new(GraphSubmitContext {
                launch_actor: actor,
                attempt: Default::default(),
                retry_limit: None,
                network: None,
            }),
            input: Box::new(input),
        },
    );
    let encoded = encode_peer_request(&request).unwrap();
    let wire = std::str::from_utf8(&encoded).unwrap();
    assert!(!wire.contains(secret));
    assert!(wire.contains("[REDACTED]"));
    assert!(serde_json::to_string(&request).unwrap().contains(secret));

    let mut prose = request.clone();
    if let PeerAction::Submit { input, .. } = &mut prose.action {
        let facade::SubmitInput::Submit { task, .. } = input.as_mut();
        task.prompt = "token: it; it stays outside; token: ordinary; ordinary stays".into();
        task.title = Some("it ordinary submit".into());
    }
    let wire: serde_json::Value =
        serde_json::from_slice(&encode_peer_request(&prose).unwrap()).unwrap();
    let mut expected = serde_json::to_value(&prose).unwrap();
    expected["action"]["input"]["task"]["prompt"] =
        serde_json::json!("token: [REDACTED]; it stays outside; token: [REDACTED]; ordinary stays");
    assert_eq!(wire, expected);

    // A typed value shared with an operation ID is masked only in text.
    if let PeerAction::Submit { input, .. } = &mut prose.action {
        let facade::SubmitInput::Submit { task, .. } = input.as_mut();
        task.prompt = r#"{"token":"submit-test"} copy submit-test"#.into();
        task.title = Some("submit-test".into());
    }
    let wire = encode_peer_request(&prose).unwrap();
    let decoded = decode_request(&wire).unwrap();
    let actual: serde_json::Value = serde_json::to_value(decoded).unwrap();
    let mut expected = serde_json::to_value(prose).unwrap();
    expected["action"]["input"]["task"]["prompt"] =
        serde_json::json!(r#"{"token":"[REDACTED]"} copy [REDACTED]"#);
    expected["action"]["input"]["task"]["title"] = serde_json::json!("[REDACTED]");
    assert_eq!(actual, expected);
}

// Spec: docs/specs/fabric.md#publication-boundary
#[test]
fn cross_host_errors_mask_credentials_in_prose() {
    let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
    use crate::graph::template::{ParameterFault, ParameterRefusal};

    let remote = HostId::from_bytes([2; 16]);
    let exact = "declared-credential-value";
    let shaped_identifier = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let projector =
        provenant_confinement::publication::Projector::new([exact.as_bytes().to_vec()]).unwrap();
    let response = |error| PeerResponse {
        extra: Default::default(),
        version: 1,
        protocol: PROTOCOL,
        host: remote,
        project: project_key(Path::new("/h")),
        result: PeerResult::Error {
            data: PeerResultError {
                error,
                extra: Default::default(),
            },
            extra: Default::default(),
        },
    };

    let refusal = facade::FacadeError::DispatchRefused {
        reason: format!("node {shaped_identifier}: validator saw {exact}"),
        disk: None,
        fact: Some(facade::DispatchRefusalFact::GraphPlan),
    };
    let bytes = encode_peer_response(response(refusal), &projector).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let error = &value["result"]["data"]["error"];
    let reason = error["reason"].as_str().unwrap();
    assert_eq!(error["fact"], "graph_plan");
    assert!(!reason.contains(shaped_identifier));
    assert!(!reason.contains(exact));

    let parameter = facade::FacadeError::invalid(facade::InvalidInputDetail::Parameter(Box::new(
        ParameterRefusal {
            node: Some("implement".into()),
            template: "task".into(),
            fault: ParameterFault::Shape {
                key: "mode".into(),
                expected: exact.into(),
            },
        },
    )));
    let bytes = encode_peer_response(response(parameter), &projector).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let error = &value["result"]["data"]["error"];
    assert_eq!(error["code"], "invalid_input");
    assert!(!value.to_string().contains(exact));
    assert_eq!(
        error["detail_fact"]["value"]["fault"]["expected"],
        "[REDACTED]"
    );
}

#[test]
fn graph_refusal_identifier_remains_visible_to_peer() {
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    for mode in [provenant_redact::Mode::Off, provenant_redact::Mode::Secrets] {
        let _mode = provenant_redact::scoped(mode);
        let node = crate::graph::refusal::GraphIdentifier::try_from(token.to_owned()).unwrap();
        assert_eq!(node.to_string(), token);
        let response = PeerResponse {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL,
            host: HostId::from_bytes([2; 16]),
            project: project_key(Path::new("/h")),
            result: PeerResult::Error {
                extra: Default::default(),
                data: PeerResultError {
                    extra: Default::default(),
                    error: facade::FacadeError::DispatchRefused {
                        reason: format!("node {token}"),
                        disk: None,
                        fact: Some(facade::DispatchRefusalFact::GraphCycle { node }),
                    },
                },
            },
        };
        let projector = provenant_confinement::publication::Projector::new([]).unwrap();
        let wire = String::from_utf8(encode_peer_response(response, &projector).unwrap()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert!(value.to_string().contains(token));
        assert!(
            value["result"]["data"]["error"]["fact"]
                .to_string()
                .contains(token)
        );
    }
}

#[test]
fn cross_host_response_projects_only_declared_text_fields() {
    let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
    let remote = HostId::from_bytes([2; 16]);
    let projector = provenant_confinement::publication::Projector::new([]).unwrap();
    for (message, projected) in [
        ("token: it; it remains", "token: [REDACTED]; it remains"),
        (
            r#"{"token":"submit-test"} copy submit-test"#,
            r#"{"token":"[REDACTED]"} copy [REDACTED]"#,
        ),
    ] {
        let response = PeerResponse {
            extra: [("submit-test".into(), serde_json::json!("submit-test"))].into(),
            version: 1,
            protocol: PROTOCOL,
            host: remote,
            project: project_key(Path::new("/h")),
            result: PeerResult::Inbox {
                extra: Default::default(),
                data: crate::inbox::InboxReply::Error {
                    extra: Default::default(),
                    code: "submit-test".into(),
                    message: message.into(),
                },
            },
        };
        let mut expected = serde_json::to_value(&response).unwrap();
        expected["result"]["data"]["message"] = serde_json::json!(projected);
        let wire = encode_peer_response(response, &projector).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&wire).unwrap(),
            expected
        );
        serde_json::from_slice::<PeerResponse>(&wire).unwrap();
    }
    let data = serde_json::json!({
        "warnings": [r#"{"token":"submit-test"}"#],
        "managed_error":"submit-test token: it; it stays",
        "extension":{"message":"submit-test", "id":"it"},
        "message":"submit-test"
    });
    let response = PeerResponse {
        extra: Default::default(),
        version: 1,
        protocol: PROTOCOL,
        host: remote,
        project: project_key(Path::new("/h")),
        result: PeerResult::Doctor {
            data,
            extra: Default::default(),
        },
    };
    let mut expected = serde_json::to_value(&response).unwrap();
    expected["result"]["data"]["warnings"][0] = serde_json::json!(r#"{"token":"[REDACTED]"}"#);
    expected["result"]["data"]["managed_error"] =
        serde_json::json!("[REDACTED] token: [REDACTED]; it stays");
    let wire = encode_peer_response(response, &projector).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&wire).unwrap(),
        expected
    );
    let data = serde_json::json!({
        "lanes":[{"metadata":{"attempt":{
            "title":r#"{"token":"submit-test"}"#,
            "state":"failed", "reason":"submit-test token: it; it remains",
            "applied_route":{"account_key":"submit-test", "model":"submit-test", "effort":"submit-test", "adapter":"submit-test", "name":"submit-test", "alias":"submit-test", "launch_warnings":["submit-test"]},
            "extension":{"title":"submit-test"}
        }}}],
        "nodes":[{"title":"submit-test", "status":"failed", "lane_id":{"attempt_id":"submit-test"}}],
        "unfetched_children":[{"reason":"unavailable", "detail":"submit-test"}],
        "unfetched":[{"detail":"submit-test"}],
        "delivery":{"state":{"state":"failed", "reason":"submit-test"}},
        "extension":{"reason":"submit-test"}
    });
    let response = PeerResponse {
        extra: Default::default(),
        version: 1,
        protocol: PROTOCOL,
        host: remote,
        project: project_key(Path::new("/h")),
        result: PeerResult::Console {
            data,
            extra: Default::default(),
        },
    };
    let mut expected = serde_json::to_value(&response).unwrap();
    let attempt = &mut expected["result"]["data"]["lanes"][0]["metadata"]["attempt"];
    attempt["title"] = serde_json::json!(r#"{"token":"[REDACTED]"}"#);
    attempt["reason"] = serde_json::json!("[REDACTED] token: [REDACTED]; it remains");
    attempt["applied_route"]["launch_warnings"][0] = serde_json::json!("[REDACTED]");
    expected["result"]["data"]["delivery"]["state"]["reason"] = serde_json::json!("[REDACTED]");
    expected["result"]["data"]["unfetched_children"][0]["detail"] = serde_json::json!("[REDACTED]");
    let wire = encode_peer_response(response, &projector).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&wire).unwrap(),
        expected
    );
    // Sealed stop metadata survives while its handoff prose shares the inventory.
    let handoff = serde_json::from_value(serde_json::json!({
        "schema_version":2, "status":"stopped_early", "note":r#"{"token":"stopped_early"}"#,
        "remaining":["stopped_early"], "decisions":[], "verify":["stopped_early"]
    }))
    .unwrap();
    let stop =
        provenant_records::Stop::new(provenant_records::StopCause::StoppedEarly, Some(handoff))
            .unwrap();
    let mut raw_stop = serde_json::to_value(stop).unwrap();
    raw_stop["stopped_early"] = serde_json::json!("stopped_early");
    let stop: provenant_records::Stop = serde_json::from_value(raw_stop.clone()).unwrap();
    let event = provenant_records::run::RunEvent {
        extra: Default::default(),
        schema_version: 1,
        sequence: 1,
        project_id: provenant_core::ProjectId::from_bytes([3; 16]),
        attempt_id: provenant_core::AttemptId::from_bytes([4; 16]),
        revision: 1,
        trace: Default::default(),
        change: provenant_records::run::AttemptState::Terminal {
            extra: Default::default(),
            cause: None,
            terminal_note: None,
            stop: Some(stop.clone()),
            outcome: provenant_records::run::Outcome::Failed,
            exit_code: Some(1),
            finished_at: provenant_core::WallTime::from_micros(1).unwrap(),
            reason: Some("stopped_early".into()),
        },
    };
    let response = PeerResponse {
        extra: Default::default(),
        version: 1,
        protocol: PROTOCOL,
        host: remote,
        project: project_key(Path::new("/h")),
        result: PeerResult::RunEvents {
            extra: Default::default(),
            data: crate::runs::RunEventsPage {
                extra: Default::default(),
                items: vec![event],
                cursor: 1,
                truncated: false,
            },
        },
    };
    let mut expected = serde_json::to_value(&response).unwrap();
    let state = &mut expected["result"]["data"]["items"][0]["change"];
    state["reason"] = serde_json::json!("[REDACTED]");
    let handoff = &mut state["stop"]["handoff"]["Ok"];
    handoff["note"] = serde_json::json!(r#"{"token":"[REDACTED]"}"#);
    handoff["remaining"][0] = serde_json::json!("[REDACTED]");
    handoff["verify"][0] = serde_json::json!("[REDACTED]");
    let wire = encode_peer_response(response, &projector).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&wire).unwrap(),
        expected
    );
    serde_json::from_slice::<PeerResponse>(&wire).unwrap();
    assert_eq!(serde_json::to_value(&stop).unwrap(), raw_stop);
    let mut incompatible = stop.handoff().unwrap().clone();
    incompatible.status = provenant_records::handoff::HandoffStatus::Done;
    assert!(stop.with_handoff(Ok(Some(incompatible))).is_err());
}

#[test]
fn new_requests_trace_the_sender_and_the_operation() {
    let (local, remote) = (HostId::from_bytes([1; 16]), HostId::from_bytes([2; 16]));
    let project = ProjectLocator {
        extra: Default::default(),
        key: project_key(Path::new("/h")),
        common_dir: "repo/.git".into(),
    };
    let operation = OperationId::parse("note-once").unwrap();
    let note: PeerAction = serde_json::from_value(serde_json::json!({
        "action": "coordination",
        "input": {"domain": "events", "input": {"op": "note", "note": "n", "dedup_key": null}},
        "operation_id": "note-once",
    }))
    .unwrap();
    let request = PeerRequest::new(remote, local, "worker".into(), project.clone(), note);
    assert_eq!(request.trace.host, Some(local));
    assert_eq!(request.trace.actor, Some(request.actor().unwrap()));
    assert_eq!(request.trace.operation, Some(operation));
    let health = PeerRequest::new(
        remote,
        local,
        "worker".into(),
        project,
        PeerAction::Health {
            extra: Default::default(),
        },
    );
    assert_eq!(health.trace.operation, None);
    assert_eq!((health.version, health.protocol), (1, PROTOCOL));
    assert_eq!(health.target_host, remote);
}

struct Ssh {
    request: Mutex<Option<PeerRequest>>,
    lose_reply: bool,
    calls: AtomicUsize,
    expected_executable: OsString,
    expected_connect_timeout: OsString,
    expected_remote_command: OsString,
    expected_timeout: Option<Duration>,
}
impl Executor for Ssh {
    fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(options.argv.first(), Some(&self.expected_executable));
        assert!(options.argv.iter().any(|a| a == "-oBatchMode=yes"));
        assert!(
            options
                .argv
                .iter()
                .any(|a| a == &self.expected_connect_timeout)
        );
        assert_eq!(options.argv.last(), Some(&self.expected_remote_command));
        if let Some(expected_timeout) = self.expected_timeout {
            assert_eq!(options.timeout, expected_timeout);
        }
        let request = decode_request(options.stdin.unwrap()).unwrap();
        let response = PeerResponse {
            extra: Default::default(),
            version: 1,
            protocol: PROTOCOL,
            host: request.target_host,
            project: request.project.key.clone(),
            result: PeerResult::Health {
                data: PeerHealth {
                    extra: Default::default(),
                    host: request.target_host,
                    available: true,
                    execution: false,
                    release: env!("CARGO_PKG_VERSION").into(),
                    run_capabilities: crate::runs::RunCapabilities {
                        submit: false,
                        list: true,
                        status: true,
                        wait: true,
                        cancel: true,
                        resume: true,
                        output: true,
                        events: true,
                    },
                    readiness: PeerReadiness::reporting(),
                },
                extra: Default::default(),
            },
        };
        let lose_reply = self.lose_reply && !matches!(request.action, PeerAction::Health { .. });
        *self.request.lock().unwrap() = Some(request);
        Ok(ExecOutcome {
            termination: Termination::Exit(if lose_reply { 255 } else { 0 }),
            stdout: CapturedStream {
                bytes: serde_json::to_vec(&response).unwrap(),
                ..Default::default()
            },
            stderr: CapturedStream::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        })
    }
}
/// The `PeerTransport` contract every implementation honours: a health
/// exchange answers as the addressed host for the requested project, a
/// cancelled token is refused before any peer contact, and a request
/// addressed to another host than the peer is refused as a project error.
fn assert_peer_transport_conforms(
    transport: &dyn PeerTransport,
    peer: &Peer,
    health: &PeerRequest,
    budget: WaitBudget,
) {
    let response = transport
        .exchange(peer, health, budget, &CancelToken::new())
        .unwrap();
    assert_eq!(response.host, peer.host);
    assert_eq!(response.project, health.project.key);
    assert!(matches!(response.result, PeerResult::Health { .. }));
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(matches!(
        transport.exchange(peer, health, budget, &cancelled),
        Err(PeerError::Cancelled)
    ));
    let mut misaddressed = health.clone();
    misaddressed.target_host = HostId::from_bytes([9; 16]);
    assert!(matches!(
        transport.exchange(peer, &misaddressed, budget, &CancelToken::new()),
        Err(PeerError::Project)
    ));
}

#[test]
fn ssh_transport_conforms_to_the_peer_transport_port() {
    let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let (local, host) = (HostId::from_bytes([1; 16]), HostId::from_bytes([2; 16]));
    let peer = Peer {
        host,
        destination: "worker".into(),
        remote_executable: None,
        connect_timeout_seconds: 10,
    };
    let project = ProjectLocator {
        extra: Default::default(),
        key: project_key(Path::new("/home/a")),
        common_dir: "repo/.git".into(),
    };
    let health = PeerRequest::new(
        host,
        local,
        "chair".into(),
        project,
        PeerAction::Health {
            extra: Default::default(),
        },
    );
    let ssh = Ssh {
        request: Mutex::new(None),
        lose_reply: false,
        calls: AtomicUsize::new(0),
        expected_executable: "/usr/bin/ssh".into(),
        expected_connect_timeout: "-oConnectTimeout=10".into(),
        expected_remote_command: "provenant fabric peer".into(),
        expected_timeout: None,
    };
    let transport = SshTransport {
        executor: &ssh,
        clock: &clock,
        home: Path::new("/home/a"),
        cwd: Path::new("/home/a/repo"),
        executable: "/usr/bin/ssh".into(),
        seconds: 30,
    };
    assert_peer_transport_conforms(&transport, &peer, &health, budget);
    // Only the first exchange reached the executor.
    assert_eq!(ssh.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn fixed_peer_transport_preserves_mutation_uncertainty_without_retry() {
    let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let local = HostId::from_bytes([1; 16]);
    let host = HostId::from_bytes([2; 16]);
    let operation = OperationId::parse("mail-once").unwrap();
    let project = ProjectLocator {
        extra: Default::default(),
        key: project_key(Path::new("/home/a")),
        common_dir: "repo/.git".into(),
    };
    let request = PeerRequest {
        extra: Default::default(),
        release: None,
        release_probe: false,
        version: 1,
        protocol: PROTOCOL,
        target_host: host,
        source_host: local,
        trace: Default::default(),
        agent: "chair".into(),
        project,
        action: PeerAction::Coordination {
            extra: Default::default(),
            input: CoordinationInput::Events {
                input: EventsInput::Note {
                    extra: Default::default(),
                    note: "hello".into(),
                    dedup_key: None,
                },
                extra: Default::default(),
            },
            operation_id: Some(operation.clone()),
        },
    };
    let peer = Peer {
        host,
        destination: "worker".into(),
        remote_executable: None,
        connect_timeout_seconds: 10,
    };
    let ssh = Ssh {
        request: Mutex::new(None),
        lose_reply: true,
        calls: AtomicUsize::new(0),
        expected_executable: "/usr/bin/ssh".into(),
        expected_connect_timeout: "-oConnectTimeout=10".into(),
        expected_remote_command: "provenant fabric peer".into(),
        expected_timeout: None,
    };
    let transport = SshTransport {
        executor: &ssh,
        clock: &clock,
        home: Path::new("/home/a"),
        cwd: Path::new("/home/a/repo"),
        executable: "/usr/bin/ssh".into(),
        seconds: 30,
    };
    assert!(
        matches!(transport.exchange(&peer,&request,budget,&CancelToken::new()),Err(PeerError::Unknown{operation_id}) if operation_id==operation)
    );
    assert_eq!(ssh.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        ssh.request.lock().unwrap().as_ref().unwrap().operation(),
        Some(operation.clone())
    );
    assert!(request.project.resolve(Path::new("/home/b")).is_ok());

    // Submit has its own operation-id path. An expired deadline must not
    // contact SSH; once contacted, a lost reply is an unknown launch.
    let mut submit = request.clone();
    let task = serde_json::from_value(serde_json::json!({
        "prompt": "inspect the project",
        "route": {"kind": "model", "provider": "fixture", "model": "fixture/model"},
        "timeout_seconds": 30
    }))
    .unwrap();
    submit.action = PeerAction::Submit {
        extra: Default::default(),
        graph_context: Box::new(GraphSubmitContext {
            launch_actor: ActorId::from_bytes([9; 16]),
            attempt: crate::graph::AttemptSpec::default(),
            retry_limit: None,
            network: None,
        }),
        input: Box::new(facade::SubmitInput::Submit {
            extra: Default::default(),
            origin_work_key: None,
            operation_id: operation.clone(),
            task,
            knobs: Vec::new(),
            host: None,
        }),
    };
    assert!(matches!(
        transport.exchange_within(&peer, &submit, budget, &CancelToken::new(), Duration::ZERO),
        Err(PeerError::Deadline)
    ));
    assert_eq!(ssh.calls.load(Ordering::SeqCst), 2);
    assert!(matches!(
        transport.exchange(&peer, &submit, budget, &CancelToken::new()),
        Err(PeerError::Unknown { operation_id }) if operation_id == operation
    ));
    assert_eq!(ssh.calls.load(Ordering::SeqCst), 4);
}

struct Canned(Mutex<Option<Result<ExecOutcome, ExecError>>>);
impl Executor for Canned {
    fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
        let request = decode_request(options.stdin.unwrap()).unwrap();
        let expected = action_fence(&request.action);
        assert_eq!(request.protocol, expected);
        if matches!(request.action, PeerAction::Health { .. }) {
            return Ok(healthy_outcome(&request, env!("CARGO_PKG_VERSION")));
        }
        self.0.lock().unwrap().take().unwrap()
    }
}
fn healthy_outcome(request: &PeerRequest, release: &str) -> ExecOutcome {
    let response = PeerResponse {
        extra: Default::default(),
        version: 1,
        protocol: request.protocol,
        host: request.target_host,
        project: request.project.key.clone(),
        result: PeerResult::Health {
            data: PeerHealth {
                extra: Default::default(),
                host: request.target_host,
                available: true,
                execution: true,
                release: release.into(),
                run_capabilities: crate::runs::RunCapabilities {
                    submit: true,
                    list: true,
                    status: true,
                    wait: true,
                    cancel: true,
                    resume: true,
                    output: true,
                    events: true,
                },
                readiness: PeerReadiness::reporting(),
            },
            extra: Default::default(),
        },
    };
    let mut result = outcome(0, "", false);
    result.stdout.bytes = serde_json::to_vec(&response).unwrap();
    result
}

#[test]
fn mixed_releases_refuse_before_dispatch_and_project_transient() {
    struct MixedPeer(&'static str, AtomicUsize);
    impl Executor for MixedPeer {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            self.1.fetch_add(1, Ordering::SeqCst);
            let request = decode_request(options.stdin.unwrap()).unwrap();
            assert!(matches!(request.action, PeerAction::Health { .. }));
            Ok(healthy_outcome(&request, self.0))
        }
    }
    for release in ["0.0.1", "9999.0.0"] {
        let peer = MixedPeer(release, AtomicUsize::new(0));
        let error = exchange_error(&peer, true);
        assert_eq!(error.code(), PEER_UPGRADE_PENDING);
        assert_eq!(peer.1.load(Ordering::SeqCst), 1);
        let projected = facade::FacadeError::from(&error);
        let command_error = provenant_records::CommandError::new(
            projected.code(),
            projected.to_string(),
            None,
            projected.diagnostic().next.unwrap(),
        );
        assert!(matches!(
            command_error.class,
            provenant_records::command_result::ErrorClass::Transient { .. }
        ));
    }
}

#[test]
fn fixture_creation_probes_release_without_requiring_the_remote_project() {
    struct FixtureProbe;
    impl Executor for FixtureProbe {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let request = decode_request(options.stdin.unwrap()).unwrap();
            assert!(matches!(request.action, PeerAction::Health { .. }));
            assert!(request.release_probe);
            Ok(healthy_outcome(&request, "0.0.1"))
        }
    }
    let error = exchange_action_error(
        &FixtureProbe,
        PeerAction::SmokeFixture {
            extra: Default::default(),
            op: SmokeFixtureOp::Create {
                nonce: "0".repeat(12),
                recipe: 1,
                root: ".local/state/provenant-smoke".into(),
            },
        },
    );
    assert_eq!(error.code(), PEER_UPGRADE_PENDING);
}

#[test]
fn receiving_peer_rejects_mixed_or_missing_release_before_action_decode() {
    let request = PeerRequest::new(
        HostId::from_bytes([2; 16]),
        HostId::from_bytes([1; 16]),
        "chair".into(),
        ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/home/a")),
            common_dir: "repo/.git".into(),
        },
        PeerAction::Health {
            extra: Default::default(),
        },
    );
    let mut value = serde_json::to_value(request).unwrap();
    assert!(
        protocol_refusal(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .is_none()
    );
    for release in [Some("0.0.1"), Some("9999.0.0"), None] {
        if let Some(release) = release {
            value["release"] = release.into();
        } else {
            value.as_object_mut().unwrap().remove("release");
        }
        let response = protocol_refusal(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .unwrap();
        assert!(
            matches!(response.result, PeerResult::Error { data, .. } if data.error.code() == PEER_UPGRADE_PENDING)
        );
    }
}
fn outcome(exit: i64, stderr: &str, timed_out: bool) -> ExecOutcome {
    ExecOutcome {
        termination: Termination::Exit(exit),
        stdout: CapturedStream::default(),
        stderr: CapturedStream {
            bytes: stderr.as_bytes().to_vec(),
            ..Default::default()
        },
        timed_out,
        drain_truncated: false,
        overflow_stopped: false,
        cancelled: false,
        elapsed_micros: 0,
    }
}
fn read_only_failure(result: Result<ExecOutcome, ExecError>) -> PeerError {
    exchange_failure(result, false)
}
fn exchange_failure(result: Result<ExecOutcome, ExecError>, mutation: bool) -> PeerError {
    let executor = Canned(Mutex::new(Some(result)));
    exchange_error(&executor, mutation)
}
fn exchange_error(executor: &dyn Executor, mutation: bool) -> PeerError {
    let action = if mutation {
        PeerAction::Coordination {
            extra: Default::default(),
            input: CoordinationInput::Events {
                input: EventsInput::Note {
                    extra: Default::default(),
                    note: "must not run".into(),
                    dedup_key: None,
                },
                extra: Default::default(),
            },
            operation_id: Some(OperationId::parse("wrong-version").unwrap()),
        }
    } else {
        PeerAction::RunStatuses {
            extra: Default::default(),
            targets: vec![],
        }
    };
    exchange_action_error(executor, action)
}
fn exchange_action_error(executor: &dyn Executor, action: PeerAction) -> PeerError {
    let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let host = HostId::from_bytes([2; 16]);
    let request = PeerRequest {
        extra: Default::default(),
        release: None,
        release_probe: false,
        version: 1,
        protocol: action_fence(&action),
        target_host: host,
        source_host: HostId::from_bytes([1; 16]),
        trace: Default::default(),
        agent: "chair".into(),
        project: ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/home/a")),
            common_dir: "repo/.git".into(),
        },
        action,
    };
    let peer = Peer {
        host,
        destination: "worker".into(),
        remote_executable: None,
        connect_timeout_seconds: 10,
    };
    let transport = SshTransport {
        executor,
        clock: &clock,
        home: Path::new("/home/a"),
        cwd: Path::new("/home/a/repo"),
        executable: "/usr/bin/ssh".into(),
        seconds: 30,
    };
    transport
        .exchange(&peer, &request, budget, &CancelToken::new())
        .unwrap_err()
}
#[test]
fn console_peer_invalid_frames_fail_and_unreachable() {
    for action in [
        PeerAction::ConsoleBoard {
            extra: Default::default(),
            all: false,
            limit: 50,
            selected: false,
        },
        PeerAction::ConsoleTree {
            extra: Default::default(),
            max_depth: 8,
            max_nodes: 100,
            detail: false,
            selected: false,
            subtree: Some("attempt".into()),
        },
    ] {
        let old = Canned(Mutex::new(Some(Ok(outcome(
            3,
            "provenant: fabric peer: invalid peer frame\n",
            false,
        )))));
        assert!(matches!(
            exchange_action_error(&old, action.clone()),
            PeerError::Failed(_)
        ));
        let offline = Canned(Mutex::new(Some(Ok(outcome(
            255,
            "connection refused",
            false,
        )))));
        assert!(matches!(
            exchange_action_error(&offline, action),
            PeerError::Failed(_)
        ));
    }
}
#[test]
fn console_inbox_invalid_frame_fails_closed() {
    let action = PeerAction::ConsoleInbox {
        extra: Default::default(),
        state: crate::inbox::InboxFilter::Open,
        limit: 50,
    };
    let old = Canned(Mutex::new(Some(Ok(outcome(
        3,
        "provenant: fabric peer: invalid peer frame\n",
        false,
    )))));
    assert!(matches!(
        exchange_action_error(&old, action.clone()),
        PeerError::Failed(_)
    ));
    let offline = Canned(Mutex::new(Some(Ok(outcome(
        255,
        "connection refused",
        false,
    )))));
    assert!(matches!(
        exchange_action_error(&offline, action),
        PeerError::Failed(_)
    ));
}
#[test]
fn peer_feature_declaration_drives_every_protocol_fence() {
    let features = declared_feature_actions();
    assert!(
        features
            .iter()
            .any(|(name, protocol, _)| *name == "smoke_fixture"
                && *protocol == SMOKE_FIXTURE_PROTOCOL)
    );
    let newest = features
        .iter()
        .map(|(_, protocol, _)| *protocol)
        .max()
        .unwrap();
    assert_eq!(newest_feature_protocol(), newest);

    for (wire_name, protocol, action) in features {
        assert_eq!(action_fence(&action), protocol, "{wire_name}");
        let request = PeerRequest::new(
            HostId::from_bytes([2; 16]),
            HostId::from_bytes([1; 16]),
            "chair".into(),
            ProjectLocator {
                extra: Default::default(),
                key: project_key(Path::new("/home/a")),
                common_dir: "repo/.git".into(),
            },
            action,
        );
        assert_eq!(request.protocol, protocol, "{wire_name}");
        let mut value = serde_json::to_value(request).unwrap();
        assert_eq!(value["action"]["action"], wire_name);
        assert!(
            protocol_refusal(&serde_json::to_vec(&value).unwrap())
                .unwrap()
                .is_none(),
            "{wire_name} at its declared fence"
        );

        value["protocol"] = (protocol + 1).into();
        let refusal = protocol_refusal(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(refusal.protocol, protocol, "{wire_name}");
    }
    assert_eq!(feature_protocol_for_wire_name("health"), PROTOCOL);
}
#[test]
fn read_only_failures_name_their_cause_on_the_same_wire_code() {
    let error = read_only_failure(Ok(outcome(
        255,
        "\nssh: connect to host w port 22: Connection refused\nmore",
        false,
    )));
    assert_eq!(error.code(), "home_unavailable");
    let text = error.to_string();
    assert!(text.contains("status 255"), "{text}");
    assert!(text.contains("Connection refused"), "{text}");
    assert!(!text.contains("more"), "{text}");
    assert!(
        read_only_failure(Ok(outcome(0, "", true)))
            .to_string()
            .contains("timed out")
    );
    assert!(
        read_only_failure(Err(ExecError::Busy))
            .to_string()
            .contains("could not be run")
    );
    let bad = read_only_failure(Ok(outcome(0, "", false)));
    assert!(bad.to_string().contains("valid frame"), "{bad}");
}

#[test]
fn missing_peer_protocol_fields_are_rejected() {
    let host = HostId::from_bytes([2; 16]);
    let mut request = serde_json::to_value(PeerRequest::new(
        host,
        HostId::from_bytes([1; 16]),
        "chair".into(),
        ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/home/a")),
            common_dir: "repo/.git".into(),
        },
        PeerAction::Health {
            extra: Default::default(),
        },
    ))
    .unwrap();
    request.as_object_mut().unwrap().remove("protocol");
    assert!(matches!(
        decode_request(&serde_json::to_vec(&request).unwrap()),
        Err(PeerError::Frame)
    ));

    let mut response = serde_json::json!({
        "version": 1,
        "protocol": PROTOCOL,
        "host": host,
        "project": project_key(Path::new("/home/a")),
        "result": {"result": "health", "data": {"host": host}},
    });
    response.as_object_mut().unwrap().remove("protocol");
    assert!(
        provenant_records::decode::<PeerResponse<serde_json::Value>, _>(
            serde_json::to_vec(&response).unwrap().as_slice(),
            Limits::CONFIG,
        )
        .is_err()
    );
}

#[test]
fn closed_peer_reply_is_uncertain_and_never_retried() {
    struct ClosedPeer(AtomicUsize);
    impl Executor for ClosedPeer {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let request = decode_request(options.stdin.unwrap()).unwrap();
            assert_eq!(request.protocol, PROTOCOL);
            if matches!(request.action, PeerAction::Health { .. }) {
                return Ok(healthy_outcome(&request, env!("CARGO_PKG_VERSION")));
            }
            Ok(outcome(
                3,
                "provenant: fabric peer: peer agent closed without a reply; see peer.log\n",
                false,
            ))
        }
    }

    for mutation in [false, true] {
        let peer = ClosedPeer(AtomicUsize::new(0));
        let error = exchange_error(&peer, mutation);
        if mutation {
            assert!(matches!(error, PeerError::Unknown { .. }), "{error}");
        } else {
            assert!(matches!(error, PeerError::Failed(_)), "{error}");
        }
        assert_eq!(peer.0.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn frame_errors_have_a_distinct_diagnostic() {
    let frame = PeerError::Frame.to_string();
    assert!(!frame.is_empty());
    assert_ne!(frame, PeerError::Version.to_string());
}

#[test]
fn peer_health_requires_every_format_two_field() {
    let host = HostId::from_bytes([2; 16]);
    let complete = serde_json::json!({
        "host": host,
        "available": true,
        "execution": true,
        "release": "2.0.0",
        "run_capabilities": {
            "submit": true, "list": true, "status": true, "wait": true,
            "cancel": true, "resume": true, "output": true, "events": true
        },
        "readiness": {"state": "unknown", "reason": "not_probed"}
    });
    let decoded: PeerHealth = serde_json::from_value(complete.clone()).unwrap();
    assert_eq!(decoded.release, "2.0.0");
    assert!(decoded.run_capabilities.submit);
    assert_eq!(decoded.readiness.state, PeerReadinessState::Unknown);
    for field in [
        "host",
        "available",
        "execution",
        "release",
        "run_capabilities",
        "readiness",
    ] {
        let mut omitted = complete.clone();
        omitted.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<PeerHealth>(omitted).is_err(),
            "format-2 health accepted a missing {field}"
        );
    }
    let encoded = serde_json::to_value(decoded).unwrap();
    assert!(encoded.get("protocol").is_none());
    assert!(encoded.get("reason").is_none());
}

#[test]
fn protocol_mismatch_names_both_versions_without_decoding_payload() {
    let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
    let host = HostId::from_bytes([2; 16]);
    let project = ProjectLocator {
        extra: Default::default(),
        key: project_key(Path::new("/home/a")),
        common_dir: "repo/.git".into(),
    };
    for (requested, action) in [
        (
            PROTOCOL,
            PeerAction::RunStatuses {
                extra: Default::default(),
                targets: vec![],
            },
        ),
        (
            PROTOCOL,
            PeerAction::Coordination {
                extra: Default::default(),
                input: CoordinationInput::Events {
                    input: EventsInput::Note {
                        extra: Default::default(),
                        note: "hello".into(),
                        dedup_key: None,
                    },
                    extra: Default::default(),
                },
                operation_id: Some(OperationId::parse("note").unwrap()),
            },
        ),
        // A feature action raises both the requested fence and the prose
        // beyond the base level.
        (
            DOCTOR_PROTOCOL,
            PeerAction::Doctor {
                extra: Default::default(),
            },
        ),
    ] {
        for remote in [1, PROTOCOL + 1] {
            let response = PeerResponse {
                extra: Default::default(),
                version: 1,
                protocol: remote,
                host,
                project: project.key.clone(),
                result: serde_json::json!({"result": "future_protocol_payload"}),
            };
            let mut reply = outcome(0, "", false);
            reply.stdout.bytes = serde_json::to_vec(&response).unwrap();
            let executor = Canned(Mutex::new(Some(Ok(reply))));
            let transport = SshTransport {
                executor: &executor,
                clock: &clock,
                home: Path::new("/home/a"),
                cwd: Path::new("/home/a/repo"),
                executable: "/usr/bin/ssh".into(),
                seconds: 30,
            };
            let request = PeerRequest {
                extra: Default::default(),
                release: None,
                release_probe: false,
                version: 1,
                protocol: requested,
                target_host: host,
                source_host: HostId::from_bytes([1; 16]),
                trace: Default::default(),
                agent: "chair".into(),
                project: project.clone(),
                action: action.clone(),
            };
            let peer = Peer {
                host,
                destination: "workshop".into(),
                remote_executable: None,
                connect_timeout_seconds: 10,
            };
            let error = transport
                .exchange(
                    &peer,
                    &request,
                    WaitBudget::cli(clock.mono_now(), 5).unwrap(),
                    &CancelToken::new(),
                )
                .unwrap_err();
            assert_eq!(error.code(), "peer_version");
            assert_eq!(
                error.to_string(),
                format!(
                    "peer workshop speaks protocol {remote}, this host expects protocol {requested}; upgrade both"
                )
            );
            assert!(executor.0.lock().unwrap().is_none());
        }
    }
}

#[test]
fn health_reports_only_observed_protocols() {
    let mut health = serde_json::json!({
        "host": HostId::from_bytes([2; 16]),
        "available": true, "execution": true,
    });
    let unprobed = serde_json::from_value::<HostHealth>(health.clone()).unwrap();
    assert_eq!(unprobed.protocol, None);
    let unprobed = serde_json::to_value(unprobed).unwrap();
    assert!(unprobed.get("protocol").is_none());
    assert!(unprobed.get("features").is_none());
    health["protocol"] = serde_json::json!("2");
    assert!(serde_json::from_value::<HostHealth>(health.clone()).is_err());
    health["protocol"] = serde_json::json!(PROTOCOL);
    assert_eq!(
        serde_json::from_value::<HostHealth>(health)
            .unwrap()
            .protocol,
        Some(PROTOCOL)
    );
}

#[test]
fn peer_readiness_reads_protocol_mismatch_and_unreachable_peers_as_unknown() {
    let host = HostId::from_bytes([2; 16]);
    let reason = |probe: &Result<PeerResponse, PeerError>| {
        let read = PeerReadiness::from_probe(probe);
        (read.state, read.reason)
    };
    let unknown = |reason: &str| (PeerReadinessState::Unknown, Some(reason.to_owned()));
    // A protocol mismatch remains distinct from a transport failure.
    let mismatch = Err(PeerError::ProtocolMismatch {
        peer: "workshop".into(),
        remote: PROTOCOL - 1,
        expected: PROTOCOL,
    });
    assert_eq!(reason(&mismatch), unknown("unsupported_peer_version"));
    // An unreachable peer, or any failed exchange, reads unknown, never ready.
    for failed in [
        PeerError::Unavailable,
        PeerError::Failed("ssh".into()),
        PeerError::Frame,
    ] {
        assert_eq!(reason(&Err(failed)), unknown("unreachable_peer"));
    }
    // A current peer's report is kept, with its reason bounded and cleaned.
    let response = serde_json::json!({
        "version": 1, "protocol": PROTOCOL, "host": host,
        "project": project_key(Path::new("/home/a")),
        "result": {"result": "health", "data": {
            "host": host, "available": true, "execution": true,
            "release": "2.0.0",
            "run_capabilities": {
                "submit": true, "list": true, "status": true, "wait": true,
                "cancel": true, "resume": true, "output": true, "events": true
            },
            "readiness": {"state": "unavailable", "reason": "exhausted\u{202e}", "future": 1}
        }}
    });
    let current: PeerResponse = serde_json::from_value(response).unwrap();
    assert_eq!(
        reason(&Ok(current)),
        (
            PeerReadinessState::Unavailable,
            Some("exhausted".to_owned())
        )
    );
    // The current peer reports its readiness record even before probing routes.
    let sent = PeerHealth {
        extra: Default::default(),
        host,
        available: true,
        execution: true,
        release: "2.0.0".into(),
        run_capabilities: crate::runs::RunCapabilities {
            submit: true,
            list: true,
            status: true,
            wait: true,
            cancel: true,
            resume: true,
            output: true,
            events: true,
        },
        readiness: PeerReadiness::reporting(),
    };
    let value = serde_json::to_value(sent).unwrap();
    assert_eq!(value["readiness"]["state"], "unknown");
    assert_eq!(value["readiness"]["reason"], "not_probed");
    assert!(value.get("extra").is_none());
}

#[test]
fn plain_text_drops_format_and_separator_characters() {
    let text = "a\u{202e}b\u{2066}c\u{200b}d\u{2028}e\u{feff}f\u{1b}g";
    assert_eq!(plain_text(text, 100), "abcdefg");
    assert_eq!(plain_text("abcdef", 3), "abc");
}

// Spec: docs/specs/fabric.md#publication-boundary
#[test]
fn session_handoff_fits_one_frame_and_keeps_its_operation_on_a_lost_reply() {
    let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
    use crate::handoff::{
        ATTACHMENT_LIMIT, Attachment, PROMPT_HEADER, PROMPT_LIMIT, SessionHandoff, encode_base64,
    };
    let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let local = HostId::from_bytes([1; 16]);
    let host = HostId::from_bytes([2; 16]);
    let operation = OperationId::parse("handoff-1").unwrap();
    // The largest hand-off: a full prompt of characters JSON escapes and
    // the whole attachment allowance.
    let input = SessionHandoff {
        extra: Default::default(),
        operation_id: operation.clone(),
        account: "work".into(),
        model: None,
        prompt: format!(
            "{PROMPT_HEADER}{}",
            "\"".repeat(PROMPT_LIMIT - PROMPT_HEADER.len())
        ),
        branches: (0..64).map(|i| format!("feat/branch-{i}")).collect(),
        attachments: vec![Attachment {
            extra: Default::default(),
            name: "bundle.bin".into(),
            base64: encode_base64(&vec![b'x'; ATTACHMENT_LIMIT]),
        }],
        launch: false,
    };
    input.validate().unwrap();
    let request = PeerRequest {
        extra: Default::default(),
        release: None,
        release_probe: false,
        version: 1,
        protocol: PROTOCOL,
        target_host: host,
        source_host: local,
        trace: Default::default(),
        agent: "chair".into(),
        project: ProjectLocator {
            extra: Default::default(),
            key: project_key(Path::new("/home/a")),
            common_dir: "repo/.git".into(),
        },
        action: PeerAction::SessionHandoff {
            extra: Default::default(),
            input: Box::new(input.clone()),
            budget_seconds: 30,
        },
    };
    let mut secret_request = request.clone();
    let PeerAction::SessionHandoff {
        input: secret_input,
        ..
    } = &mut secret_request.action
    else {
        unreachable!()
    };
    secret_input.attachments[0].base64 = encode_base64(br#"{"token":"attachment-private-value"}"#);
    secret_input.prompt = format!("{PROMPT_HEADER} attachment-private-value");
    let wire = encode_peer_request(&secret_request).unwrap();
    let decoded = decode_request(&wire).unwrap();
    let PeerAction::SessionHandoff {
        input: projected, ..
    } = decoded.action
    else {
        unreachable!()
    };
    assert!(!projected.prompt.contains("attachment-private-value"));
    let content = crate::handoff::decode_base64(&projected.attachments[0].base64).unwrap();
    assert!(
        !String::from_utf8(content.clone())
            .unwrap()
            .contains("attachment-private-value")
    );
    assert!(String::from_utf8(content).unwrap().contains("[REDACTED]"));
    let PeerAction::SessionHandoff {
        input: binary_input,
        ..
    } = &mut secret_request.action
    else {
        unreachable!()
    };
    binary_input.attachments[0].base64 = encode_base64(&[0xff]);
    assert!(encode_peer_request(&secret_request).is_err());
    let frame = serde_json::to_vec(&request).unwrap();
    assert!(frame.len() < MAX_FRAME, "{} bytes", frame.len());
    let mut without_budget = serde_json::to_value(&request).unwrap();
    without_budget["action"]
        .as_object_mut()
        .unwrap()
        .remove("budget_seconds");
    assert!(decode_request(&serde_json::to_vec(&without_budget).unwrap()).is_err());
    let decoded = decode_request(&frame).unwrap();
    assert!(
        matches!(&decoded.action, PeerAction::SessionHandoff { input: got, .. } if **got == input)
    );
    let peer = Peer {
        host,
        destination: "worker".into(),
        remote_executable: None,
        connect_timeout_seconds: 10,
    };
    let ssh = Ssh {
        request: Mutex::new(None),
        lose_reply: true,
        calls: AtomicUsize::new(0),
        expected_executable: "/usr/bin/ssh".into(),
        expected_connect_timeout: "-oConnectTimeout=10".into(),
        expected_remote_command: "provenant fabric peer".into(),
        expected_timeout: None,
    };
    let transport = SshTransport {
        executor: &ssh,
        clock: &clock,
        home: Path::new("/home/a"),
        cwd: Path::new("/home/a/repo"),
        executable: "/usr/bin/ssh".into(),
        seconds: 30,
    };
    assert!(
        matches!(transport.exchange(&peer, &request, budget, &CancelToken::new()), Err(PeerError::Unknown { operation_id }) if operation_id == operation)
    );
    assert_eq!(ssh.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn peer_policy_overrides_remote_executable_and_connect_timeout() {
    let ssh_path = Path::new("/bin/sh");
    assert_eq!(
        discover_ssh_executable(Some(ssh_path)).unwrap(),
        ssh_path.to_path_buf()
    );

    let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let local = HostId::from_bytes([1; 16]);
    let host = HostId::from_bytes([2; 16]);
    let project = ProjectLocator {
        extra: Default::default(),
        key: project_key(Path::new("/home/a")),
        common_dir: "repo/.git".into(),
    };
    let request = PeerRequest {
        extra: Default::default(),
        release: None,
        release_probe: false,
        version: 1,
        protocol: PROTOCOL,
        target_host: host,
        source_host: local,
        trace: Default::default(),
        agent: "chair".into(),
        project,
        action: PeerAction::Health {
            extra: Default::default(),
        },
    };
    let peer = Peer {
        host,
        destination: "worker".into(),
        remote_executable: Some("/opt/provenant/bin/provenant".into()),
        connect_timeout_seconds: 7,
    };
    let ssh = Ssh {
        request: Mutex::new(None),
        lose_reply: false,
        calls: AtomicUsize::new(0),
        expected_executable: ssh_path.into(),
        expected_connect_timeout: "-oConnectTimeout=7".into(),
        expected_remote_command: "/opt/provenant/bin/provenant fabric peer".into(),
        expected_timeout: None,
    };
    let transport = SshTransport {
        executor: &ssh,
        clock: &clock,
        home: Path::new("/home/a"),
        cwd: Path::new("/home/a/repo"),
        executable: ssh_path.to_path_buf(),
        seconds: 30,
    };

    transport
        .exchange(&peer, &request, budget, &CancelToken::new())
        .unwrap();
    assert_eq!(ssh.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn peer_exchange_respects_a_fair_probe_cap() {
    let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let local = HostId::from_bytes([1; 16]);
    let host = HostId::from_bytes([2; 16]);
    let project = ProjectLocator {
        extra: Default::default(),
        key: project_key(Path::new("/home/a")),
        common_dir: "repo/.git".into(),
    };
    let request = PeerRequest {
        extra: Default::default(),
        release: None,
        release_probe: false,
        version: 1,
        protocol: PROTOCOL,
        target_host: host,
        source_host: local,
        trace: Default::default(),
        agent: "chair".into(),
        project,
        action: PeerAction::Health {
            extra: Default::default(),
        },
    };
    let peer = Peer {
        host,
        destination: "worker".into(),
        remote_executable: None,
        connect_timeout_seconds: 10,
    };
    let ssh = Ssh {
        request: Mutex::new(None),
        lose_reply: false,
        calls: AtomicUsize::new(0),
        expected_executable: "/usr/bin/ssh".into(),
        expected_connect_timeout: "-oConnectTimeout=10".into(),
        expected_remote_command: "provenant fabric peer".into(),
        expected_timeout: Some(Duration::from_millis(250)),
    };
    let transport = SshTransport {
        executor: &ssh,
        clock: &clock,
        home: Path::new("/home/a"),
        cwd: Path::new("/home/a/repo"),
        executable: "/usr/bin/ssh".into(),
        seconds: 30,
    };

    transport
        .exchange_with_cap(
            &peer,
            &request,
            budget,
            &CancelToken::new(),
            Duration::from_millis(250),
        )
        .unwrap();
    assert_eq!(ssh.calls.load(Ordering::SeqCst), 1);
}

// The deadline is shared out between the SSH timeout and the executor's
// termination and reap allowances, so their sum never exceeds it.
#[test]
fn peer_exchange_within_a_deadline_includes_termination_and_reap() {
    struct Budgets(Mutex<Vec<(Duration, Duration, Duration)>>);
    impl Executor for Budgets {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            self.0.lock().unwrap().push((
                options.timeout,
                options.terminate_grace,
                options.reap_timeout,
            ));
            Ok(ExecOutcome {
                termination: Termination::Signal(9),
                stdout: CapturedStream::default(),
                stderr: CapturedStream::default(),
                timed_out: true,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }
    let clock = TestClock::new(WallTime::from_micros(1).unwrap(), MonoTime::from_micros(1));
    // The whole-second budget has far longer left than the deadline.
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let local = HostId::from_bytes([1; 16]);
    let host = HostId::from_bytes([2; 16]);
    let project = ProjectLocator {
        extra: Default::default(),
        key: project_key(Path::new("/home/a")),
        common_dir: "repo/.git".into(),
    };
    let request = PeerRequest {
        extra: Default::default(),
        release: None,
        release_probe: false,
        version: 1,
        protocol: PROTOCOL,
        target_host: host,
        source_host: local,
        trace: Default::default(),
        agent: "chair".into(),
        project,
        action: PeerAction::Health {
            extra: Default::default(),
        },
    };
    let peer = Peer {
        host,
        destination: "worker".into(),
        remote_executable: None,
        connect_timeout_seconds: 10,
    };
    let executor = Budgets(Mutex::new(Vec::new()));
    let transport = SshTransport {
        executor: &executor,
        clock: &clock,
        home: Path::new("/home/a"),
        cwd: Path::new("/home/a/repo"),
        executable: "/usr/bin/ssh".into(),
        seconds: 30,
    };
    let ms = Duration::from_millis;
    let us = Duration::from_micros;
    for within in [ms(10), ms(2000)] {
        let result =
            transport.exchange_within(&peer, &request, budget, &CancelToken::new(), within);
        assert!(matches!(result, Err(PeerError::Failed(_))), "{result:?}");
    }
    // A short deadline gives each allowance a quarter; a long one keeps the
    // usual grace. Either way the child's whole lifetime fits the deadline.
    // A cap alone keeps the executor's own allowances on top of the cap.
    transport
        .exchange_with_cap(&peer, &request, budget, &CancelToken::new(), ms(250))
        .unwrap_err();
    let recorded = executor.0.lock().unwrap().clone();
    assert_eq!(
        recorded,
        [
            (ms(5), us(2500), us(2500)),
            (ms(1500), TERMINATE_GRACE, REAP_TIMEOUT),
            (ms(250), ms(250), ms(250)),
        ]
    );
    for ((timeout, grace, reap), within) in recorded.iter().zip([ms(10), ms(2000)]) {
        assert!(*timeout + *grace + *reap <= within);
    }
    // A deadline too short to leave any timeout fails before SSH starts.
    assert!(matches!(
        transport.exchange_within(&peer, &request, budget, &CancelToken::new(), Duration::ZERO),
        Err(PeerError::Deadline)
    ));
    assert_eq!(executor.0.lock().unwrap().len(), 3);
}
