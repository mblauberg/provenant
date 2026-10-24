use super::*;
use provenant_core::OperationId;
use serde_json::json;

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
            json!({"op":"read","limit":1,"wait_ms":55000}),
        ),
        (
            "fabric_landing",
            json!({"op":"wait","reference":"main","generation":1,"wait_ms":55000}),
        ),
        (
            "fabric_run",
            json!({"op":"wait","targets":[{"attempt":"00000000000000000000000000000001"}],"condition":"any","wait_ms":55000}),
        ),
    ] {
        assert!(ToolCall::parse(tool, args.clone()).is_ok());
        args["wait_ms"] = json!(55001);
        assert!(
            matches!(ToolCall::parse(tool, args), Err(InvalidInput::Arguments { path }) if path == "arguments.wait_ms")
        );
    }
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
