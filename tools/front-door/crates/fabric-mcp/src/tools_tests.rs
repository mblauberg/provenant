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
