use super::*;
use crate::tools::{FabricService, HostsInput, IdentityInput, OutputInput, RunInput, SubmitInput};
use provenant_core::OperationId;
use serde_json::json;

struct FakeService {
    called: String,
}
impl FakeService {
    fn response(&mut self, name: &str) -> Result<ToolResponse, ServiceError> {
        self.called = name.to_owned();
        Ok(ToolResponse {
            content: vec![TextContent {
                kind: "text",
                text: name.to_owned(),
            }],
            structured_content: json!({"kind":name}),
        })
    }
}
impl FabricService for FakeService {
    fn identity(
        &mut self,
        _: IdentityInput,
        _: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        self.response("identity")
    }
    fn mail(&mut self, _: MailInput, _: &CancelToken) -> Result<ToolResponse, ServiceError> {
        self.response("mail")
    }
    fn task(&mut self, _: TaskInput, _: &CancelToken) -> Result<ToolResponse, ServiceError> {
        self.response("task")
    }
    fn events(&mut self, _: EventsInput, _: &CancelToken) -> Result<ToolResponse, ServiceError> {
        self.response("events")
    }
    fn claim(&mut self, _: ClaimInput, _: &CancelToken) -> Result<ToolResponse, ServiceError> {
        self.response("claim")
    }
    fn landing(&mut self, _: LandingInput, _: &CancelToken) -> Result<ToolResponse, ServiceError> {
        self.response("landing")
    }
    fn submit(&mut self, _: SubmitInput, _: &CancelToken) -> Result<ToolResponse, ServiceError> {
        self.response("submit")
    }
    fn run(&mut self, _: RunInput, _: &CancelToken) -> Result<ToolResponse, ServiceError> {
        self.response("run")
    }
    fn output(&mut self, _: OutputInput, _: &CancelToken) -> Result<ToolResponse, ServiceError> {
        self.response("output")
    }
    fn hosts(&mut self, _: HostsInput, _: &CancelToken) -> Result<ToolResponse, ServiceError> {
        self.response("hosts")
    }
}

#[test]
fn typed_call_dispatches_to_the_matching_service_operation() {
    let call_result = ToolCall::parse("fabric_identity", json!({"op":"inspect"}));
    assert!(call_result.is_ok());
    let Some(call) = call_result.ok() else {
        return;
    };
    let mut service = FakeService {
        called: String::new(),
    };
    let result = call.execute(&mut service, &CancelToken::new());
    assert!(result.is_ok());
    let Some(result) = result.ok() else {
        return;
    };
    assert_eq!(service.called, "identity");
    assert_eq!(result.structured_content, json!({"kind":"identity"}));
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
