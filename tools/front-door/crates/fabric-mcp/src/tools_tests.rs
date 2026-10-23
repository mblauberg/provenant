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
