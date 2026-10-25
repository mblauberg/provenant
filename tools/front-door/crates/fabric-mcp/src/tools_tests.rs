use super::*;
use provenant_core::OperationId;
use serde_json::json;

#[test]
fn legacy_import_refusals_report_upgrade_refused_in_tool_errors() {
    for error in [
        ServiceError::Coordination(CoordinationError::from(
            provenant_fabric_store::Error::LegacyImportData {
                table: "tasks",
                rows: 1,
            },
        )),
        ServiceError::Facade(FacadeError::UpgradeRefused),
    ] {
        assert_eq!(error.code(), "upgrade_refused");
        let result = tool_error(
            Some("fabric_events"),
            error.code(),
            error.structured_error(),
        );
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("upgrade_refused:")
        );
    }
}

#[test]
fn output_bytes_are_utf8_or_an_explicit_lossless_byte_encoding() {
    assert_eq!(
        OutputBytes::from_bytes(b"hello".to_vec()),
        OutputBytes::Utf8("hello".into())
    );
    assert_eq!(
        OutputBytes::from_bytes(vec![0xff, 0x00]),
        OutputBytes::Bytes(vec![0xff, 0x00])
    );
}

#[test]
fn uncertain_mutation_errors_include_only_the_operation_id() {
    let operation_id_result = OperationId::parse("dispatch-17");
    assert!(operation_id_result.is_ok());
    let Some(operation_id) = operation_id_result.ok() else {
        return;
    };
    let error = ServiceError::Facade(FacadeError::LaunchUnknown { operation_id });
    assert_eq!(error.code(), "launch_unknown");
    assert_eq!(
        error.structured_error(),
        json!({"code":"launch_unknown","operation_id":"dispatch-17"})
    );
}

#[test]
fn every_mcp_observation_wait_uses_the_transport_cap() {
    for (tool, mut args) in [
        (
            "fabric_events",
            json!({"op":"read","limit":1,"wait_ms":crate::MAX_WAIT_MS}),
        ),
        (
            "fabric_landing",
            json!({"op":"wait","reference":"main","generation":1,"wait_ms":crate::MAX_WAIT_MS}),
        ),
        (
            "fabric_run",
            json!({"op":"wait","targets":[{"attempt":"00000000000000000000000000000001"}],"condition":"any","wait_ms":crate::MAX_WAIT_MS}),
        ),
    ] {
        assert!(ToolCall::parse(tool, args.clone()).is_ok());
        args["wait_ms"] = json!(crate::MAX_WAIT_MS + 1);
        assert!(
            matches!(ToolCall::parse(tool, args), Err(InvalidInput::Arguments { path }) if path == "arguments.wait_ms")
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
        lease_seconds: 2,
        peer_seconds: 30,
        graph_timeout_ms: 14_400_000,
        bundle_bytes: 67_108_864,
    }
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
            "fabric_claim",
            json!({"op":"acquire","scope":{"kind":"issue","key":"x"},"lease_ms":3000}),
            "lease_ms",
        ),
        (
            "fabric_output",
            json!({"op":"read","target":{"attempt":"00000000000000000000000000000001"},"part":"stdout","offset":0,"max_bytes":9}),
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
    let receipt = response("fabric_submit", json!({"operation_id":"op-1"})).unwrap();
    let kept = limit_response(receipt, &limits, true, false).unwrap();
    assert_eq!(kept.structured_content["operation_id"], "op-1");

    let observation = response("fabric_run", json!({"attempt":"longer"})).unwrap();
    assert!(matches!(
        limit_response(observation, &limits, false, false),
        Err(ServiceError::ResultTooLarge)
    ));
}

#[test]
fn graph_control_operations_are_strict_and_cancel_is_a_mutation() {
    for (op, arguments) in [
        ("run", json!({"op":"run","document":"graph.json"})),
        ("resume", json!({"op":"resume","document":"graph.json"})),
        ("status", json!({"op":"status","id":"g-test"})),
        ("cancel", json!({"op":"cancel","id":"g-test"})),
    ] {
        let call = ToolCall::parse("fabric_graph", arguments.clone()).expect(op);
        assert_eq!(call.is_mutation(), op != "status");
        let mut unknown = arguments;
        unknown["extra"] = json!(true);
        assert!(ToolCall::parse("fabric_graph", unknown).is_err());
    }
    assert!(ToolCall::parse("fabric_graph", json!({"op":"status","id":"../escape"})).is_err());
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
fn submit_accepts_the_single_operation_with_or_without_op() {
    let task = json!({"prompt":"review","route":{"adapter":"codex"},"timeout_ms":1_800_000,"access":"read_only","worktree":"/tmp/work"});
    let bare = json!({"operation_id":"review-pr1486-codex","tasks":[task]});
    let mut tagged = bare.clone();
    tagged["op"] = json!("submission");
    assert!(matches!(
        ToolCall::parse("fabric_submit", bare.clone()),
        Ok(ToolCall::Submit(_))
    ));
    assert!(matches!(
        ToolCall::parse("fabric_submit", tagged),
        Ok(ToolCall::Submit(_))
    ));
    let schema = &crate::schema::catalogue()
        .iter()
        .find(|tool| tool["name"] == "fabric_submit")
        .map(|tool| tool["inputSchema"].clone())
        .unwrap_or_default();
    assert!(schema.get("oneOf").is_none());
    assert!(
        !schema["required"]
            .as_array()
            .is_some_and(|r| r.iter().any(|f| f == "op"))
    );
    let mut bad = bare;
    bad["tasks"][0]["timeout_ms"] = json!("soon");
    assert!(matches!(
        ToolCall::parse("fabric_submit", bad),
        Err(InvalidInput::Arguments { path }) if path == "arguments.tasks[0].timeout_ms"
    ));
}
