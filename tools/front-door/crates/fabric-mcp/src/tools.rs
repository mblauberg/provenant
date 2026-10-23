//! Typed tool inputs and the single service boundary used by the transport.
use crate::{render, schema};
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget, WaitTransport};
pub use provenant_fabric_runtime::coordination::facade::*;
use provenant_fabric_runtime::coordination::input::{InputError, ValidateInput};
pub use provenant_fabric_runtime::coordination::{
    ClaimInput, CoordinationApi, CoordinationError, CoordinationService, EventsInput, LandingInput,
    MailInput, TaskInput,
};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// Parsed tool request. Coordination inputs are the runtime's exact serde
/// contracts; the execution facade contracts also live in runtime for F3.
pub enum ToolCall {
    Identity(IdentityInput),
    Mail(MailInput),
    Task(TaskInput),
    Events(EventsInput),
    Claim(ClaimInput),
    Landing(LandingInput),
    Submit(SubmitInput),
    Run(RunInput),
    Output(OutputInput),
    Hosts(HostsInput),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvalidInput {
    #[error("unknown MCP tool")]
    UnknownTool,
    #[error("invalid tool argument at {path}")]
    Arguments { path: String },
}

impl ToolCall {
    /// Strict runtime serde contracts own argument validation. The catalogue
    /// supplies a field path for serde's structural errors at the MCP edge.
    pub fn parse(name: &str, arguments: Value) -> Result<Self, InvalidInput> {
        macro_rules! parse {
            ($ty:ty, $variant:ident) => {
                serde_json::from_value::<$ty>(arguments.clone())
                    .map(Self::$variant)
                    .map_err(|error| InvalidInput::Arguments {
                        path: schema::invalid_path(name, &arguments).unwrap_or_else(|| {
                            let message = error.to_string();
                            message
                                .strip_prefix("invalid input field `")
                                .and_then(|path| path.strip_suffix('`'))
                                .map(|path| format!("arguments.{path}"))
                                .unwrap_or_else(|| "arguments".to_owned())
                        }),
                    })
            };
        }
        match name {
            "fabric_identity" => parse!(IdentityInput, Identity),
            "fabric_mail" => parse!(MailInput, Mail),
            "fabric_task" => parse!(TaskInput, Task),
            "fabric_events" => parse!(EventsInput, Events),
            "fabric_claim" => parse!(ClaimInput, Claim),
            "fabric_landing" => parse!(LandingInput, Landing),
            "fabric_submit" => parse!(SubmitInput, Submit),
            "fabric_run" => parse!(RunInput, Run),
            "fabric_output" => parse!(OutputInput, Output),
            "fabric_hosts" => parse!(HostsInput, Hosts),
            _ => Err(InvalidInput::UnknownTool),
        }
    }

    /// A single match composes the runtime and the F3 seam. Cancellation only
    /// abandons the observation; run cancellation remains an explicit call.
    pub fn execute<F: F3Facade>(
        self,
        service: &mut Service<'_, F>,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        let budget = service.budget(match &self {
            Self::Run(RunInput::Wait { wait_ms, .. }) => *wait_ms,
            _ => 55_000,
        })?;
        macro_rules! call {
            ($owner:ident, $method:ident, $input:ident, $tool:literal) => {{
                $input.validate_input().map_err(ServiceError::Invalid)?;
                response($tool, service.$owner.$method($input, budget, cancel)?)
            }};
        }
        match self {
            Self::Identity(input) => call!(facade, identity, input, "fabric_identity"),
            Self::Mail(input) => response(
                "fabric_mail",
                service.coordination.mail(input, budget, cancel)?,
            ),
            Self::Task(input) => response(
                "fabric_task",
                service.coordination.task(input, budget, cancel)?,
            ),
            Self::Events(input) => response(
                "fabric_events",
                service.coordination.events(input, budget, cancel)?,
            ),
            Self::Claim(input) => response(
                "fabric_claim",
                service.coordination.claim(input, budget, cancel)?,
            ),
            Self::Landing(input) => response(
                "fabric_landing",
                service.coordination.landing(input, budget, cancel)?,
            ),
            Self::Submit(input) => call!(facade, submit, input, "fabric_submit"),
            Self::Run(input) => call!(facade, run, input, "fabric_run"),
            Self::Output(input) => call!(facade, output, input, "fabric_output"),
            Self::Hosts(input) => call!(facade, hosts, input, "fabric_hosts"),
        }
    }
}

/// Typed errors crossing the MCP edge. Library details remain available as
/// sources while the protocol exposes a stable domain code.
#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    Invalid(#[from] InputError),
    #[error(transparent)]
    Coordination(#[from] CoordinationError),
    #[error("execution facade refused the request: {0}")]
    Facade(#[from] FacadeError),
    #[error("tool response could not be encoded")]
    Encode(#[from] serde_json::Error),
    #[error("MCP wait budget could not be created")]
    Time(#[from] provenant_core::TimeError),
}

impl ServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_input",
            Self::Coordination(error) => error.code(),
            Self::Facade(error) => error.code(),
            Self::Encode(_) => "state_corrupt",
            Self::Time(_) => "invalid_input",
        }
    }

    /// Stable structured error data. Uncertain mutations carry their
    /// idempotency key; prompts, tokens and raw source errors are omitted.
    pub fn structured_error(&self) -> Value {
        match self {
            Self::Invalid(error) => serde_json::json!({"code":self.code(),"path":error.path}),
            Self::Coordination(CoordinationError::Input(error)) => {
                serde_json::json!({"code":self.code(),"path":error.path})
            }
            Self::Facade(FacadeError::LaunchUnknown { operation_id }) => {
                serde_json::json!({"code":self.code(),"operation_id":operation_id})
            }
            _ => serde_json::json!({"code":self.code()}),
        }
    }
}

/// One worker's concrete runtime composition. The clock is sampled for each
/// call so a long-lived worker never reuses an expired deadline.
pub struct Service<'a, F> {
    pub coordination: CoordinationService<'a>,
    pub facade: F,
    clock: &'a dyn Clock,
}

impl<'a, F> Service<'a, F> {
    pub fn new(coordination: CoordinationService<'a>, facade: F, clock: &'a dyn Clock) -> Self {
        Self {
            coordination,
            facade,
            clock,
        }
    }

    fn budget(&self, milliseconds: u64) -> Result<WaitBudget, ServiceError> {
        let micros = milliseconds
            .checked_mul(1_000)
            .ok_or(provenant_core::TimeError::Overflow)?;
        Ok(WaitBudget::new(
            self.clock.mono_now(),
            DurationMicros::from_micros(micros),
            WaitTransport::Mcp,
        )?)
    }
}

fn response<T: Serialize>(tool: &str, typed: T) -> Result<ToolResponse, ServiceError> {
    let structured_content = serde_json::to_value(typed)?;
    let text = render::digest(tool, &structured_content);
    Ok(ToolResponse {
        content: vec![TextContent { kind: "text", text }],
        structured_content,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextContent {
    #[serde(rename = "type")]
    kind: &'static str,
    pub text: String,
}

/// MCP tool result. `structured_content` is produced by serialising a typed
/// coordination or facade record; text is a bounded digest for quick display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolResponse {
    pub content: Vec<TextContent>,
    #[serde(rename = "structuredContent")]
    pub structured_content: Value,
}

/// One shared construction for all MCP tool errors.
pub fn tool_error(code: &str, detail: Value) -> Value {
    serde_json::json!({"content":[{"type":"text","text":format!("{code}: {detail}")}],"structuredContent":detail,"isError":true})
}

pub fn catalogue() -> &'static [Value] {
    schema::catalogue()
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
