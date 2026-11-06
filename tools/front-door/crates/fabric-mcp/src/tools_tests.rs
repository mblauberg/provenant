#[test]
fn graph_admission_error_has_safe_field_and_action() {
    let error = FacadeError::graph_admission(
        provenant_fabric_runtime::graph::AdmissionError::Schema("TOKEN_SENTINEL\u{1b}".into()),
    );
    let value = ServiceError::Facade(error).structured_error();
    assert_eq!(value["code"], "dispatch_refused");
    assert!(value["detail"].as_str().unwrap().contains("schema_version"));
    assert!(value["hint"].is_string());
    assert!(!value.to_string().contains("TOKEN_SENTINEL"));
    let error = FacadeError::graph_admission(
        provenant_fabric_runtime::graph::AdmissionError::OutputKind {
            field: provenant_fabric_runtime::graph::GraphOutputField::Input,
            reason: "TOKEN_SENTINEL".into(),
        },
    );
    let value = ServiceError::Facade(error).structured_error();
    assert!(
        value["detail"]
            .as_str()
            .unwrap()
            .contains("inputs[].outputs")
    );
    assert!(value["next"].is_string());
    assert!(!value.to_string().contains("TOKEN_SENTINEL"));
}

#[test]
fn graph_limit_refusal_keeps_typed_field_and_values() {
    use provenant_fabric_runtime::graph::{AdmissionError, GraphLimitKey, GraphStructuralLimit};
    let error =
        FacadeError::graph_admission(AdmissionError::StructuralLimit(GraphStructuralLimit {
            key: GraphLimitKey::Nodes,
            actual: 4,
            max: 1,
        }));
    let value = ServiceError::Facade(error).structured_error();
    assert_eq!(value["code"], "dispatch_refused");
    let detail = value["detail"].as_str().unwrap();
    assert!(detail.contains("limits.nodes=1"));
    assert!(detail.contains("(4)"));
    assert!(value["next"].as_str().unwrap().contains("limit"));
}

#[test]
fn invalid_input_paths_omit_untrusted_data() {
    use provenant_fabric_runtime::coordination::{CoordinationError, InputError};
    for path in [
        "/private/TOKEN_SENTINEL\u{1b}".to_owned(),
        format!("tasks[{}].title", "1".repeat(200)),
        format!("TOKEN_SENTINEL{}", "é".repeat(2048)),
    ] {
        for error in [
            ServiceError::Invalid(InputError::new(path.clone())),
            ServiceError::Coordination(CoordinationError::Input(InputError::new(path.clone()))),
        ] {
            let detail = error.structured_error();
            let wire = detail.to_string();
            assert_eq!(detail["code"], "invalid_input");
            assert!(detail["path"].is_null());
            assert!(!wire.contains("TOKEN_SENTINEL"));
            assert!(!wire.contains("/private"));
            assert!(wire.len() < 4096);
            assert!(!wire.chars().any(char::is_control));
            assert!(detail["hint"].is_string());
        }
    }
    let detail = ServiceError::Invalid(InputError::new("tasks[1].title")).structured_error();
    assert_eq!(detail["path"], "tasks[1].title");
}

#[test]
fn oversized_input_projects_field_reason_and_action() {
    use provenant_fabric_runtime::coordination::input::ValidateInput;
    let input: SubmitInput = serde_json::from_value(json!({
        "op":"submit", "operation_id":"large-prompt",
        "tasks":[{"prompt":format!("TOKEN_SENTINEL{}", "é".repeat(40000)), "route":{"kind":"named", "name":"utility"}, "timeout_seconds":1}]
    })).unwrap();
    let error = input
        .validate_input(provenant_core::WaitTransport::Mcp)
        .unwrap_err();
    let wire = ServiceError::Invalid(error).structured_error();
    assert_eq!(wire["code"], "invalid_input");
    assert_eq!(wire["path"], "tasks[0].prompt");
    let detail = wire["detail"].as_str().unwrap();
    assert!(detail.contains("tasks[0].prompt"));
    assert!(detail.contains("65536"));
    assert!(wire["hint"].is_string());
    assert!(!wire.to_string().contains("TOKEN_SENTINEL"));
}

#[test]
fn typed_validation_selects_safe_field_without_parsing_error_text() {
    let arguments = json!({"op":"submit","tasks":[{
        "title":"TOKEN_SENTINEL\u{1b}", "prompt":"work",
        "route":{"kind":"named","name":"utility"}, "timeout_seconds":1
    }]});
    let Err(error) = ToolCall::parse("fabric_run", arguments) else {
        panic!("control characters in title must be refused");
    };
    assert!(
        matches!(error, InvalidInput::Validated(error) if error.safe_path() == Some("tasks[0].title"))
    );
}

#[test]
fn memory_pressure_projects_configured_source_and_action() {
    use provenant_records::diagnostic::{
        PressureLevel, PressureThreshold, ResourceFact, SettingSource,
    };
    let error = FacadeError::ConfinementRefused {
        resource: Some(ResourceFact::MemoryPressure {
            level: PressureLevel::Critical,
            threshold: PressureThreshold::Warning,
            origin: SettingSource::Configuration,
        }),
    };
    let value = ServiceError::Facade(error).structured_error();
    assert_eq!(value["code"], "confinement_refused");
    assert!(
        value["detail"]
            .as_str()
            .unwrap()
            .contains("memory_pressure_threshold")
    );
    assert!(
        value["detail"]
            .as_str()
            .unwrap()
            .contains("host configuration")
    );
    assert!(value["next"].as_str().unwrap().contains("memory"));

    let error = FacadeError::DispatchRefused {
        reason: "TOKEN_SENTINEL /private/host".into(),
        disk: None,
        fact: Some(
            provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::Resource(
                ResourceFact::MemoryPressure {
                    level: PressureLevel::Critical,
                    threshold: PressureThreshold::Warning,
                    origin: SettingSource::Configuration,
                },
            ),
        ),
    };
    let value = ServiceError::Facade(error).structured_error();
    assert_eq!(value["code"], "dispatch_refused");
    assert!(
        value["detail"]
            .as_str()
            .unwrap()
            .contains("host configuration")
    );
    assert!(value["next"].as_str().unwrap().contains("memory"));
    assert!(!value.to_string().contains("TOKEN_SENTINEL"));
    assert!(!value.to_string().contains("/private/host"));
}

#[test]
fn lane_cap_projects_its_actual_limit_and_source() {
    use provenant_records::diagnostic::{LaneLimit, ResourceFact, SettingSource};
    for (kind, key, origin) in [
        (
            LaneLimit::HostWriters,
            "max_writer_lanes",
            SettingSource::Configuration,
        ),
        (
            LaneLimit::ProjectWriters,
            "max_project_writer_lanes",
            SettingSource::ShippedDefault,
        ),
        (
            LaneLimit::ProjectTotal,
            "max_project_lanes",
            SettingSource::ShippedDefault,
        ),
    ] {
        let error = FacadeError::ConfinementRefused {
            resource: Some(ResourceFact::LaneCap {
                active: 3,
                limit: 3,
                lane_limit: kind,
                origin,
            }),
        };
        let wire = serde_json::to_vec(&error).unwrap();
        let decoded: FacadeError = serde_json::from_slice(&wire).unwrap();
        let value = ServiceError::Facade(decoded).structured_error();
        assert_eq!(value["code"], "confinement_refused");
        assert!(value["detail"].as_str().unwrap().contains(key));
        assert!(value["next"].as_str().unwrap().contains("lane"));
        assert!(
            value["detail"]
                .as_str()
                .unwrap()
                .contains(origin.description())
        );
    }
}

#[test]
fn process_limit_keeps_io_code_and_omits_source_text() {
    let error = FacadeError::Io {
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
    };
    let decoded: FacadeError =
        serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
    let value = ServiceError::Facade(decoded).structured_error();
    assert_eq!(value["code"], "io");
    assert!(value["detail"].as_str().unwrap().contains("RLIMIT_NPROC"));
    assert!(value["next"].as_str().unwrap().contains("process"));
    assert!(!value.to_string().contains("TOKEN_SENTINEL"));
    assert!(!value.to_string().contains("/private/host"));
}

#[test]
fn unreadable_input_io_projects_safe_reason() {
    let error = FacadeError::Io {
        operation: "TOKEN_SENTINEL".into(),
        path: Some("/private/host".into()),
        delivery_limit: None,
        delivery_stage: None,
        resource: None,
        input_failure: Some(provenant_records::diagnostic::IoFact::InputUnreadable),
    };
    let decoded: FacadeError =
        serde_json::from_slice(&serde_json::to_vec(&error).unwrap()).unwrap();
    let value = ServiceError::Facade(decoded).structured_error();
    assert_eq!(value["code"], "io");
    assert!(
        value["detail"]
            .as_str()
            .unwrap()
            .contains("input could not be read")
    );
    assert!(value["next"].is_string());
    assert!(!value.to_string().contains("TOKEN_SENTINEL"));
    assert!(!value.to_string().contains("/private/host"));
}

use super::*;
use provenant_core::OperationId;
use serde_json::json;

#[test]
fn shared_peer_records_remain_strict_at_the_mcp_argument_boundary() {
    let submit = json!({"op":"submit","operation_id":"submit","tasks":[{
        "prompt":"work","route":{"kind":"named","name":"senior"},"timeout_seconds":60,
        "deliver":{"remote":"origin","branch":"work","open_pr":true}
    }]});
    assert!(ToolCall::parse("fabric_run", submit.clone()).is_ok());
    for pointer in ["", "/tasks/0", "/tasks/0/route", "/tasks/0/deliver"] {
        let mut arguments = submit.clone();
        arguments.pointer_mut(pointer).unwrap()["future"] = json!({"nested":true});
        assert!(
            matches!(
                ToolCall::parse("fabric_run", arguments),
                Err(InvalidInput::Arguments { .. })
            ),
            "{pointer}"
        );
    }
    let mail = json!({"op":"send","recipients":["11111111111111111111111111111111"],"kind":"note","body":"work",
        "artifacts":[{"path":{"encoding":"utf8","path":"out"},"digest":null}]});
    assert!(ToolCall::parse("fabric_mail", mail.clone()).is_ok());
    for pointer in ["/artifacts/0", "/artifacts/0/path"] {
        let mut arguments = mail.clone();
        arguments.pointer_mut(pointer).unwrap()["future"] = json!(true);
        assert!(
            ToolCall::parse("fabric_mail", arguments).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn io_error_payload_omits_unanchored_path_and_source_text() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
    let error = FacadeError::from(&provenant_fabric_store::Error::Io {
        path: PathBuf::from(OsString::from_vec(b"/state-\xff/worktrees".to_vec())),
        source: std::io::Error::other("private source text"),
    });
    let detail = ServiceError::Facade(error).structured_error();
    assert_eq!(detail["code"], "io");
    assert_eq!(detail["operation"], "access Fabric store");
    assert!(detail.get("path").is_none());
    assert!(!detail.to_string().contains("private source text"));
    assert!(detail["message"].is_string() && detail["hint"].is_string());
}

#[test]
fn host_delivery_io_keeps_its_reason_under_the_io_code() {
    let fact = provenant_records::diagnostic::DeliveryLimit::new(
        provenant_records::run::WriterDeliveryStage::Prepare,
        "git",
        "worktree list",
        8388608,
    )
    .unwrap();
    let reason = fact.detail();
    let detail = ServiceError::Facade(FacadeError::HostDeliveryLimit { fact }).structured_error();
    assert_eq!(detail["code"], "io");
    let message = detail["message"].as_str().unwrap_or_default();
    assert!(message.contains(&reason), "{message}");
}

#[test]
fn output_bytes_are_utf8_or_an_explicit_lossless_byte_encoding() {
    assert_eq!(
        OutputBytes::from_bytes(b"hello".to_vec()),
        OutputBytes::Utf8("hello".into())
    );
    assert_eq!(
        OutputBytes::from_bytes(vec![0xff, 0x00]),
        OutputBytes::Base64("/wA=".to_owned())
    );
}

#[test]
fn uncertain_mutation_errors_include_operation_id_and_recovery() {
    let operation_id_result = OperationId::parse("dispatch-17");
    assert!(operation_id_result.is_ok());
    let Some(operation_id) = operation_id_result.ok() else {
        return;
    };
    let error = ServiceError::Facade(FacadeError::LaunchUnknown { operation_id });
    assert_eq!(error.code(), "LAUNCH_UNKNOWN");
    let detail = error.structured_error();
    assert_eq!(detail["code"], "LAUNCH_UNKNOWN");
    assert_eq!(detail["operation_id"], "dispatch-17");
    assert!(detail["message"].is_string() && detail["hint"].is_string());
}

#[test]
fn every_mcp_observation_wait_uses_the_transport_cap() {
    for (tool, mut args) in [
        (
            "fabric_events",
            json!({"op":"read","limit":1,"wait_seconds":crate::MAX_WAIT_SECONDS}),
        ),
        (
            "fabric_lease",
            json!({"op":"landing_wait","reference":"main","generation":1,"wait_seconds":crate::MAX_WAIT_SECONDS}),
        ),
        (
            "fabric_run",
            json!({"op":"wait","targets":[{"attempt":"00000000000000000000000000000001"}],"condition":"any","wait_seconds":crate::MAX_WAIT_SECONDS}),
        ),
    ] {
        assert!(ToolCall::parse(tool, args.clone()).is_ok());
        args["wait_seconds"] = json!(crate::MAX_WAIT_SECONDS + 1);
        assert!(
            matches!(ToolCall::parse(tool, args), Err(InvalidInput::Arguments { path }) if path == "arguments.wait_seconds")
        );
    }
}

#[test]
fn spent_wait_budget_preserves_last_good_event_coverage() {
    let host = provenant_core::HostId::from_bytes([7; 16]);
    let last_good = EventCoverage {
        unknown_hosts: vec![host],
        more_events: true,
    };

    let recovered =
        coverage_after_timeout(Some(&last_good), FacadeError::OwnerUnavailable).unwrap();

    assert_eq!(recovered.unknown_hosts, vec![host]);
    assert!(recovered.more_events);
}

fn limits() -> Limits {
    Limits {
        body_bytes: 4,
        input_frame_bytes: 1_048_576,
        json_depth: 64,
        page_rows: 2,
        output_lines: 1,
        output_bytes: 8,
        result_bytes: 16,
        lease_seconds: 0,
        peer_seconds: 30,
        graph_timeout_seconds: 14_400,
        store_busy_timeout_ms: 30_000,
    }
}

#[test]
fn inbox_disclosure_strictness_and_mutation_limits() {
    assert!(
        !schema::catalogue()
            .iter()
            .any(|tool| tool["name"] == "fabric_inbox")
    );
    assert!(
        schema::advanced_catalogue()
            .iter()
            .any(|tool| tool["name"] == "fabric_inbox")
    );
    let answer = json!({"op":"answer","id":"00000000000000000000000000000001",
        "action":"reply","text":" yes ","expect_revision":1,"channel":"cli",
        "operation_id":"answer-retry-1"});
    let call = ToolCall::parse("fabric_inbox", answer.clone()).unwrap();
    assert!(call.is_mutation());
    assert_eq!(call.operation_id().unwrap().as_str(), "answer-retry-1");
    let receipt = response("fabric_inbox", json!({"operation_id":"answer-retry-1"})).unwrap();
    assert!(limit_response(receipt, &limits(), call.is_mutation(), false).is_ok());
    let mut forged = answer;
    forged["actor"] = json!("00000000000000000000000000000002");
    assert!(ToolCall::parse("fabric_inbox", forged).is_err());
    let list = ToolCall::parse("fabric_inbox", json!({"op":"list"})).unwrap();
    assert!(!list.is_mutation());
    assert_eq!(list.validate_limits(&limits()).unwrap_err().path, "limit");
    assert!(ToolCall::parse("fabric_inbox", json!({"op":"list","operation_id":"bad"})).is_err());
}

#[test]
fn configured_limits_reject_typed_body_page_lease_and_output_values() {
    let limits = limits();
    for (tool, args, path) in [
        (
            "fabric_task",
            json!({"op":"create","objective":"12345","note":null}),
            "objective",
        ),
        (
            "fabric_mail",
            json!({"op":"read","after":null,"limit":3}),
            "limit",
        ),
        (
            "fabric_lease",
            json!({"op":"claim_acquire","scope":{"kind":"issue","key":"x"},"lease_seconds":3}),
            "lease_seconds",
        ),
        (
            "fabric_output",
            json!({"op":"read","target":{"attempt":"00000000000000000000000000000001"},"part":"result","offset":0,"max_bytes":9}),
            "max_bytes",
        ),
    ] {
        let call = ToolCall::parse(tool, args).unwrap();
        assert_eq!(call.validate_limits(&limits).unwrap_err().path, path);
    }
}

#[test]
fn result_limit_preserves_mutation_receipts_and_rejects_oversized_observations() {
    let limits = limits();
    let receipt = response("fabric_run", json!({"operation_id":"op-1"})).unwrap();
    let kept = limit_response(receipt, &limits, true, false).unwrap();
    assert_eq!(kept.structured_content["operation_id"], "op-1");

    let observation = response("fabric_run", json!({"attempt":"longer"})).unwrap();
    assert!(matches!(
        limit_response(observation, &limits, false, false),
        Err(ServiceError::ResultTooLarge)
    ));
}

#[test]
fn graph_documents_use_the_run_interface_and_paths_are_rejected() {
    for arguments in [
        json!({"op":"run","document":{"id":"test","nodes":[]}}),
        json!({"op":"run","brief":"Inspect the parser","use":"explore"}),
        json!({"op":"edit","id":"test","patch":{"nodes":[]}}),
        json!({"op":"status","id":"test","stats":true,"by":"tier","wait_seconds":1}),
        json!({"op":"status","stats":true,"by":"template"}),
        json!({"op":"stop","id":"test","reason":"maintenance","wait_seconds":1}),
        json!({"op":"retry","id":"test","node":"task","wait_seconds":1}),
        json!({"op":"templates"}),
    ] {
        let call = ToolCall::parse("fabric_run", arguments.clone()).unwrap();
        assert!(matches!(call, ToolCall::GraphDocument(_)));
        let mut unknown = arguments;
        unknown["extra"] = json!(true);
        assert!(ToolCall::parse("fabric_run", unknown).is_err());
    }
    assert!(ToolCall::parse("fabric_graph", json!({"op":"status","id":"test"})).is_err());
    assert!(ToolCall::parse("fabric_run", json!({"op":"run","document":"graph.json"})).is_err());
}

#[test]
fn coordination_mutations_accept_only_valid_top_level_operation_ids() {
    let call = ToolCall::parse(
        "fabric_mail",
        json!({"op":"mark_read","through":0,"operation_id":"retry-safe-1"}),
    )
    .unwrap();
    assert!(call.is_mutation());
    assert!(
        ToolCall::parse(
            "fabric_mail",
            json!({"op":"mark_read","through":0,"operation_id":"bad id"}),
        )
        .is_err()
    );
    assert!(
        ToolCall::parse(
            "fabric_mail",
            json!({"op":"read","limit":1,"operation_id":"retry-safe-1"}),
        )
        .is_err()
    );
    assert!(
        ToolCall::parse(
            "fabric_graph",
            json!({"op":"cancel","id":"g-test","operation_id":"retry-safe-1"}),
        )
        .is_err()
    );
}

#[test]
fn run_submission_requires_its_explicit_operation() {
    let task = json!({"prompt":"review","route":{"kind":"model","provider":"codex","model":"model"},"timeout_seconds":1800,"access":"read_only","worktree":"/tmp/work"});
    let untagged = json!({"operation_id":"submit-1","tasks":[task]});
    assert!(ToolCall::parse("fabric_run", untagged.clone()).is_err());
    let mut tagged = untagged;
    tagged["op"] = json!("submit");
    assert!(matches!(
        ToolCall::parse("fabric_run", tagged.clone()),
        Ok(ToolCall::SubmitWait(_, 30, false))
    ));
    tagged["tasks"][0]["capabilities"] = json!(["postgres"]);
    assert!(
        matches!(ToolCall::parse("fabric_run", tagged.clone()), Ok(ToolCall::SubmitWait(SubmitInput::Submit { tasks, .. }, _, _)) if tasks[0].capabilities.as_deref() == Some(["postgres".to_owned()].as_slice()))
    );
    tagged["tasks"][0]["timeout_seconds"] = json!("soon");
    assert!(
        matches!(ToolCall::parse("fabric_run", tagged.clone()), Err(InvalidInput::Arguments { path }) if path == "arguments.tasks[0].timeout_seconds")
    );
    tagged["op"] = json!("submission");
    assert!(ToolCall::parse("fabric_run", tagged).is_err());
}

#[test]
fn run_defaults_preserve_explicit_ids_and_generate_distinct_ids() {
    let task = json!({"prompt":"review","route":{"kind":"model","provider":"codex","model":"model"},"timeout_seconds":1800,"access":"read_only","worktree":"/tmp/work"});
    let args = json!({"op":"submit","tasks":[task]});
    let id = |args| match ToolCall::parse("fabric_run", args).unwrap() {
        ToolCall::SubmitWait(SubmitInput::Submit { operation_id, .. }, _, _) => operation_id,
        _ => panic!("expected submit"),
    };
    assert_ne!(id(args.clone()), id(args.clone()));
    let mut explicit = args;
    explicit["operation_id"] = json!("retry-1");
    assert_eq!(id(explicit).to_string(), "retry-1");
    assert!(matches!(
        ToolCall::parse(
            "fabric_run",
            json!({"op":"status","target":{"attempt":"00000000000000000000000000000001"},"detail":true})
        ),
        Ok(ToolCall::RunDetailed(_))
    ));
    assert!(matches!(
        ToolCall::parse(
            "fabric_output",
            json!({"op":"read","target":{"attempt":"00000000000000000000000000000001"}})
        ),
        Ok(ToolCall::Output(OutputInput::Read {
            part: OutputPart::Result,
            offset: 0,
            max_bytes: 16_384,
            ..
        }))
    ));
}

#[test]
fn run_page_shrinks_before_cursor_is_issued() {
    let mut limits = limits();
    limits.result_bytes = 400;
    let rows: Vec<Value> = (1..=10)
        .map(|i| json!({"attempt_id": format!("{i:032x}"), "state":"running"}))
        .collect();
    let response = response(
        "fabric_run",
        json!({"result":"list","page":{"items":rows,"next_after":{"attempt":"0000000000000000000000000000000a","host":"00000000000000000000000000000002"},"truncated":true}}),
    )
    .unwrap();
    let result = limit_response(response, &limits, false, false).unwrap();
    let page = &result.structured_content["page"];
    let items = page["items"].as_array().unwrap();
    assert!(items.len() < 10 && !items.is_empty());
    assert_eq!(
        page["next_after"]["attempt"],
        items.last().unwrap()["attempt_id"]
    );
    assert_eq!(
        page["next_after"]["host"],
        "00000000000000000000000000000002"
    );
    assert_eq!(page["truncated"], true);
    assert!(serde_json::to_vec(&result).unwrap().len() <= 400);
}

#[test]
fn id_based_pages_preserve_last_id_for_continuation() {
    let mut limits = limits();
    limits.result_bytes = 300;
    for tool in ["fabric_task", "fabric_lease"] {
        let items: Vec<Value> = (1..=10)
            .map(|id| json!({"id":format!("{id:032x}"),"description":"a long record"}))
            .collect();
        let reply = response(
            tool,
            json!({"result":"listed","page":{"items":items,"truncated":false}}),
        )
        .unwrap();
        let result = limit_response(reply, &limits, false, false).unwrap();
        let page = &result.structured_content["page"];
        let items = page["items"].as_array().unwrap();
        assert!(!items.is_empty() && items.len() < 10);
        assert_eq!(items.last().unwrap()["id"], format!("{:032x}", items.len()));
        assert_eq!(page["truncated"], true);
        assert!(result.content[0].text.starts_with(tool));
    }
}

#[test]
fn event_page_shrinks_and_moves_cursor_to_last_retained_sequence() {
    let mut limits = limits();
    limits.result_bytes = 300;
    let items: Vec<Value> = (1..=10)
        .map(|seq| json!({"seq":seq,"detail":"a long event"}))
        .collect();
    let reply = response(
        "fabric_events",
        json!({"result":"read","page":{"items":items,"truncated":false},"cursor":10}),
    )
    .unwrap();
    let result = limit_response(reply, &limits, false, false).unwrap();
    let page = &result.structured_content["page"];
    let items = page["items"].as_array().unwrap();
    assert!(items.len() < 10 && !items.is_empty());
    assert_eq!(
        result.structured_content["cursor"],
        items.last().unwrap()["seq"]
    );
    assert_eq!(page["truncated"], true);
}

#[test]
fn removed_tools_are_unknown_and_merged_operations_are_typed() {
    for name in [
        "fabric_identity",
        "fabric_hosts",
        "fabric_claim",
        "fabric_landing",
        "fabric_submit",
    ] {
        assert!(matches!(
            ToolCall::parse(name, json!({})),
            Err(InvalidInput::UnknownTool)
        ));
    }
    assert!(matches!(
        ToolCall::parse("fabric_context", json!({"op":"inspect"})),
        Ok(ToolCall::Identity(_))
    ));
    assert!(matches!(
        ToolCall::parse("fabric_context", json!({"op":"hosts","limit":1})),
        Ok(ToolCall::Hosts(_))
    ));
    for operation in ["claim_acquire", "landing_acquire"] {
        let input = if operation == "claim_acquire" {
            json!({"op":operation,"scope":{"kind":"issue","key":"1571"},"lease_seconds":1})
        } else {
            json!({"op":operation,"reference":"refs/heads/main","expected":"a".repeat(40),"candidate":"b".repeat(40),"lease_seconds":1})
        };
        let call = ToolCall::parse("fabric_lease", input).unwrap();
        assert_eq!(call.name(), "fabric_lease");
    }
}

#[test]
fn request_correlation_mcp_accepts_links_but_rejects_caller_coverage() {
    let mut input = json!({"op":"ask","session":"chair-a","request":{
        "kind":"question","title":"Repair","body":"Which repair?",
        "work":{"run_id":"00000000000000000000000000000001",
            "attempt_id":"00000000000000000000000000000002",
            "repository":{"relative_to_root":{"encoding":"utf8","path":"repo/.git"}},
            "head":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}});
    assert!(ToolCall::parse("fabric_inbox", input.clone()).is_ok());
    input["request"]["work_coverage"] = json!("verified");
    assert!(ToolCall::parse("fabric_inbox", input).is_err());
}

#[test]
fn request_correlation_mcp_replacement_and_deadline_are_explicit_and_typed() {
    let mut input = json!({"op":"ask","session":"chair-a","request":{
        "kind":"question","title":"Next","body":"What next?",
        "supersedes":{"id":"00000000000000000000000000000001","expect_revision":2},
        "deadline":"2033-05-18T03:33:20.000000Z"}});
    assert!(ToolCall::parse("fabric_inbox", input.clone()).is_ok());
    input["request"]["supersedes"]["expect_revision"] = json!(0);
    assert!(ToolCall::parse("fabric_inbox", input.clone()).is_err());
    input["request"]["supersedes"]["expect_revision"] = json!(2);
    input["request"]["deadline"] = json!("tomorrow");
    assert!(ToolCall::parse("fabric_inbox", input).is_err());
}

#[test]
fn summary_patch_accepts_empty_text_as_the_cli_does() {
    let value = json!({"op":"summary_update","session":"s","expect_revision":0,
        "patch":{"now":"","next":"","risk":"","needs_you":[""]}});
    assert!(ToolCall::parse("fabric_chair", value).is_ok());
    let value = json!({"op":"log_append","session":"s",
        "entry":{"kind":"update","markdown":"","channel":"chat"}});
    assert!(matches!(
        ToolCall::parse("fabric_chair", value),
        Err(InvalidInput::Arguments { path }) if path == "arguments.entry.markdown"
    ));
}

#[test]
fn cli_error_detail_keeps_safe_revision_without_raw_data() {
    let error = ServiceError::Cli {
        code: "conflict".into(),
        message: "stale".into(),
        data: json!({"revision": 4, "data": "kept"}),
    };
    let detail = error.structured_error();
    assert_eq!(detail["code"], "conflict");
    assert_eq!(detail["revision"], 4);
    assert!(detail.get("data").is_none());
    let error = ServiceError::Cli {
        code: "CLI_FAILED".into(),
        message: "failed".into(),
        data: json!("text"),
    };
    let detail = error.structured_error();
    assert_eq!(detail["code"], "CLI_FAILED");
    assert!(detail.get("data").is_none());
}

#[test]
fn summary_update_carries_a_validated_operation_id() {
    let value = json!({"op":"summary_update","session":"s","expect_revision":0,
        "patch":{"now":"x"},"operation_id":"retry-1"});
    let call = ToolCall::parse("fabric_chair", value).unwrap();
    assert_eq!(call.operation_id().unwrap().as_str(), "retry-1");
    let value = json!({"op":"summary_update","session":"s","expect_revision":0,
        "patch":{"now":"x"},"operation_id":"bad id!"});
    let call = ToolCall::parse("fabric_chair", value);
    assert!(call.is_err() || call.unwrap().operation_id().is_none());
}

#[test]
fn control_ops_are_strict_local_mutations_with_bounded_text() {
    let steer = json!({"op":"steer","attempt":"00000000000000000000000000000001",
        "text":"slow down","operation_id":"steer-retry-1"});
    let call = ToolCall::parse("fabric_inbox", steer.clone()).unwrap();
    assert!(call.is_mutation());
    assert_eq!(call.operation_id().unwrap().as_str(), "steer-retry-1");
    let message = json!({"op":"chair_message","session":"chair-a","text":"look at CI",
        "channel":"console"});
    assert!(
        ToolCall::parse("fabric_inbox", message.clone())
            .unwrap()
            .is_mutation()
    );
    for base in [steer, message] {
        let mut host = base.clone();
        host["host"] = json!("peer");
        assert!(ToolCall::parse("fabric_inbox", host).is_err());
        let mut forged = base.clone();
        forged["actor"] = json!("00000000000000000000000000000002");
        assert!(ToolCall::parse("fabric_inbox", forged).is_err());
        let mut long = base.clone();
        long["text"] = json!("x".repeat(provenant_records::control::CONTROL_TEXT_BYTES + 1));
        assert!(ToolCall::parse("fabric_inbox", long).is_err());
        let mut blank = base;
        blank["text"] = json!("");
        assert!(ToolCall::parse("fabric_inbox", blank).is_err());
    }
}

#[test]
fn coordination_store_errors_include_safe_context() {
    use provenant_fabric_store::Error;
    let invalid = ServiceError::Coordination(CoordinationError::Store(Error::Invalid("through")));
    let invalid = invalid.structured_error();
    assert_eq!(invalid["field"], "through");
    assert!(invalid["message"].as_str().unwrap().contains("through"));
    assert!(invalid["hint"].is_string());
    let io = ServiceError::Coordination(CoordinationError::Store(Error::Io {
        path: "/state/inbox.db".into(),
        source: std::io::Error::other("private"),
    }));
    let detail = io.structured_error();
    assert_eq!(detail["code"], "io");
    assert!(detail.get("path").is_none_or(serde_json::Value::is_null));
    assert!(!detail.to_string().contains("/state/inbox.db"));
    assert!(!detail.to_string().contains("private"));
}

#[test]
fn continuation_conflict_includes_current_generation() {
    let error = ServiceError::Facade(FacadeError::GenerationConflict {
        current_generation: 1,
    });
    let detail = error.structured_error();
    assert_eq!(detail["code"], "conflict");
    assert_eq!(detail["current_generation"], 1);
}

#[test]
fn projected_mcp_failure_omits_untrusted_sources_and_bounds_safe_detail() {
    let unsafe_error = ServiceError::Facade(FacadeError::Io {
        delivery_limit: None,
        delivery_stage: None,
        resource: None,
        input_failure: None,
        operation: "TOKEN_SENTINEL\u{1b}".into(),
        path: Some("/private/host".into()),
    })
    .structured_error();
    let encoded = unsafe_error.to_string();
    assert!(!encoded.contains("TOKEN_SENTINEL"));
    assert!(!encoded.contains("/private/host"));
    for raw_detail in [
        format!("schema version TOKEN_SENTINEL\u{1b}{}", "é".repeat(2000)),
        "invalid input field `tasks[0].title` TOKEN_SENTINEL".to_owned(),
        "graph document schema_version is invalid".to_owned(),
    ] {
        let received: FacadeError = serde_json::from_value(json!({
            "code":"invalid_input", "detail":raw_detail
        }))
        .unwrap();
        let error = ServiceError::Facade(received).structured_error();
        let detail = error["detail"].as_str().unwrap();
        assert!(detail.len() <= provenant_records::diagnostic::MAX_DIAGNOSTIC_BYTES);
        assert!(!detail.chars().any(char::is_control));
        assert!(!error.to_string().contains("TOKEN_SENTINEL"));
        assert!(error["next"].is_string());
    }
}
