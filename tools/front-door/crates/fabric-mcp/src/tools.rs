//! Typed tool inputs and the single service boundary used by the transport.
use crate::{render, schema};
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget, WaitTransport};
pub use provenant_fabric_runtime::coordination::facade::*;
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
#[error("invalid input for MCP tool `{tool}`")]
pub struct InvalidInput {
    pub tool: String,
}

impl ToolCall {
    /// Deserialize one named tool's arguments with the tool enum's strict
    /// tagged serde model. Unknown tools and unknown fields fail closed.
    pub fn parse(name: &str, arguments: Value) -> Result<Self, InvalidInput> {
        macro_rules! parse {
            ($ty:ty, $variant:ident) => {
                serde_json::from_value::<$ty>(arguments)
                    .map(Self::$variant)
                    .map_err(|_| InvalidInput {
                        tool: name.to_owned(),
                    })
            };
        }
        let call = match name {
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
            _ => Err(InvalidInput {
                tool: name.to_owned(),
            }),
        }?;
        validate(&call).map_err(|()| InvalidInput {
            tool: name.to_owned(),
        })?;
        Ok(call)
    }

    /// Execute a parsed call through the service seam. Cancellation suppresses
    /// this response; it never means cancel the submitted run.
    pub fn execute<S: FabricService>(
        self,
        service: &mut S,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        match self {
            Self::Identity(input) => service.identity(input, cancel),
            Self::Mail(input) => service.mail(input, cancel),
            Self::Task(input) => service.task(input, cancel),
            Self::Events(input) => service.events(input, cancel),
            Self::Claim(input) => service.claim(input, cancel),
            Self::Landing(input) => service.landing(input, cancel),
            Self::Submit(input) => service.submit(input, cancel),
            Self::Run(input) => service.run(input, cancel),
            Self::Output(input) => service.output(input, cancel),
            Self::Hosts(input) => service.hosts(input, cancel),
        }
    }
}

/// Typed errors crossing the MCP edge. Library details remain available as
/// sources while the protocol exposes a stable domain code.
#[derive(Debug, Error)]
pub enum ServiceError {
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
            Self::Facade(FacadeError::LaunchUnknown { operation_id }) => {
                serde_json::json!({"code":self.code(),"operation_id":operation_id})
            }
            _ => serde_json::json!({"code":self.code()}),
        }
    }
}

fn validate(call: &ToolCall) -> Result<(), ()> {
    fn page(limit: u64) -> Result<(), ()> {
        (1..=100).contains(&limit).then_some(()).ok_or(())
    }
    fn lease(milliseconds: u64) -> Result<(), ()> {
        (1_000..=3_600_000)
            .contains(&milliseconds)
            .then_some(())
            .ok_or(())
    }
    fn wait(milliseconds: u64) -> Result<(), ()> {
        (1_000..=55_000)
            .contains(&milliseconds)
            .then_some(())
            .ok_or(())
    }
    match call {
        ToolCall::Identity(_) => {}
        ToolCall::Hosts(HostsInput::List { limit, .. }) => page(u64::from(*limit))?,
        ToolCall::Mail(MailInput::Send {
            recipients,
            body,
            artifacts,
            ..
        }) => {
            if recipients.is_empty()
                || recipients.len() > 100
                || recipients.iter().enumerate().any(|(index, recipient)| {
                    recipients
                        .iter()
                        .take(index)
                        .any(|previous| previous == recipient)
                })
                || body.is_empty()
                || body.len() > 65_536
                || body.contains('\0')
                || artifacts.len() > 100
            {
                return Err(());
            }
        }
        ToolCall::Mail(MailInput::Read { after, limit }) => {
            if after.is_some_and(|seq| seq < 0) {
                return Err(());
            }
            page(*limit)?;
        }
        ToolCall::Mail(MailInput::MarkRead { through }) if *through < 0 => return Err(()),
        ToolCall::Mail(MailInput::MarkRead { .. }) => {}
        ToolCall::Task(TaskInput::Create { objective, note }) => {
            if objective.is_empty()
                || objective.len() > 65_536
                || objective.contains('\0')
                || note.as_ref().is_some_and(|value| {
                    value.is_empty() || value.len() > 65_536 || value.contains('\0')
                })
            {
                return Err(());
            }
        }
        ToolCall::Task(TaskInput::List {
            after: _, limit, ..
        }) => page(*limit)?,
        ToolCall::Task(TaskInput::Claim { lease_ms, .. }) => lease(*lease_ms)?,
        ToolCall::Task(TaskInput::Update {
            objective, note, ..
        }) => {
            if objective.is_empty()
                || objective.len() > 65_536
                || objective.contains('\0')
                || note.as_ref().is_some_and(|value| {
                    value.is_empty() || value.len() > 65_536 || value.contains('\0')
                })
            {
                return Err(());
            }
        }
        ToolCall::Task(TaskInput::Finish { state, .. })
            if !matches!(
                state,
                provenant_fabric_runtime::coordination::TaskState::Done
                    | provenant_fabric_runtime::coordination::TaskState::Cancelled
            ) =>
        {
            return Err(());
        }
        ToolCall::Task(TaskInput::Finish { .. } | TaskInput::Release { .. }) => {}
        ToolCall::Events(EventsInput::Read {
            after,
            limit,
            wait_ms,
        }) => {
            if after.is_some_and(|seq| seq < 0) {
                return Err(());
            }
            page(*limit)?;
            if let Some(milliseconds) = wait_ms {
                wait(*milliseconds)?;
            }
        }
        ToolCall::Events(EventsInput::Note { note, dedup_key }) => {
            if note.is_empty()
                || note.len() > 65_536
                || note.contains('\0')
                || dedup_key
                    .as_ref()
                    .is_some_and(|key| key.is_empty() || key.len() > 1024 || key.contains('\0'))
            {
                return Err(());
            }
        }
        ToolCall::Claim(ClaimInput::Acquire { lease_ms, .. })
        | ToolCall::Claim(ClaimInput::Renew { lease_ms, .. }) => lease(*lease_ms)?,
        ToolCall::Claim(ClaimInput::List { after: _, limit }) => page(*limit)?,
        ToolCall::Claim(ClaimInput::Verify { .. } | ClaimInput::Release { .. }) => {}
        ToolCall::Landing(LandingInput::Acquire { lease_ms, .. })
        | ToolCall::Landing(LandingInput::Renew { lease_ms, .. }) => lease(*lease_ms)?,
        ToolCall::Landing(LandingInput::Wait { wait_ms, .. }) => wait(*wait_ms)?,
        ToolCall::Landing(LandingInput::Verify { .. } | LandingInput::Release { .. }) => {}
        ToolCall::Submit(SubmitInput::Submission {
            tasks, concurrency, ..
        }) => {
            if tasks.is_empty()
                || tasks.len() > 100
                || *concurrency == 0
                || usize::from(*concurrency) > tasks.len()
                || tasks.iter().any(|task| {
                    task.prompt.is_empty()
                        || task.prompt.len() > 65_536
                        || task.prompt.contains('\0')
                        || task.inputs.len() > 100
                        || !(1_000..=14_400_000).contains(&task.timeout_ms)
                })
            {
                return Err(());
            }
        }
        ToolCall::Run(RunInput::List { limit, .. }) => page(u64::from(*limit))?,
        ToolCall::Run(RunInput::Wait {
            targets, wait_ms, ..
        }) => {
            if targets.is_empty() || targets.len() > 100 {
                return Err(());
            }
            wait(*wait_ms)?;
        }
        ToolCall::Run(RunInput::Resume { prompt, .. }) => {
            if prompt.is_empty() || prompt.len() > 65_536 || prompt.contains('\0') {
                return Err(());
            }
        }
        ToolCall::Run(RunInput::Status { .. } | RunInput::Cancel { .. }) => {}
        ToolCall::Output(OutputInput::Read { max_bytes, .. }) => {
            if !(1..=65_536).contains(max_bytes) {
                return Err(());
            }
        }
        ToolCall::Output(OutputInput::Fetch { max_bytes, .. }) => {
            if !(1..=1_048_576).contains(max_bytes) {
                return Err(());
            }
        }
    }
    Ok(())
}

/// Common typed seam consumed by the MCP transport and implemented by the
/// concrete composition below or by focused test fakes.
pub trait FabricService {
    fn identity(
        &mut self,
        input: IdentityInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError>;
    fn mail(
        &mut self,
        input: MailInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError>;
    fn task(
        &mut self,
        input: TaskInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError>;
    fn events(
        &mut self,
        input: EventsInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError>;
    fn claim(
        &mut self,
        input: ClaimInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError>;
    fn landing(
        &mut self,
        input: LandingInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError>;
    fn submit(
        &mut self,
        input: SubmitInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError>;
    fn run(&mut self, input: RunInput, cancel: &CancelToken) -> Result<ToolResponse, ServiceError>;
    fn output(
        &mut self,
        input: OutputInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError>;
    fn hosts(
        &mut self,
        input: HostsInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError>;
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

    fn budget(&self, wait_ms: Option<u64>) -> Result<WaitBudget, ServiceError> {
        let milliseconds = wait_ms.unwrap_or(55_000);
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

impl<F: F3Facade> FabricService for Service<'_, F> {
    fn identity(
        &mut self,
        input: IdentityInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        let budget = self.budget(None)?;
        response(
            "fabric_identity",
            self.facade.identity(input, budget, cancel)?,
        )
    }
    fn mail(
        &mut self,
        input: MailInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        let budget = self.budget(None)?;
        response(
            "fabric_mail",
            self.coordination.mail(input, budget, cancel)?,
        )
    }
    fn task(
        &mut self,
        input: TaskInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        let budget = self.budget(None)?;
        response(
            "fabric_task",
            self.coordination.task(input, budget, cancel)?,
        )
    }
    fn events(
        &mut self,
        input: EventsInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        let budget = self.budget(match &input {
            EventsInput::Read { wait_ms, .. } => *wait_ms,
            _ => None,
        })?;
        response(
            "fabric_events",
            self.coordination.events(input, budget, cancel)?,
        )
    }
    fn claim(
        &mut self,
        input: ClaimInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        let budget = self.budget(None)?;
        response(
            "fabric_claim",
            self.coordination.claim(input, budget, cancel)?,
        )
    }
    fn landing(
        &mut self,
        input: LandingInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        let budget = self.budget(match &input {
            LandingInput::Wait { wait_ms, .. } => Some(*wait_ms),
            _ => None,
        })?;
        response(
            "fabric_landing",
            self.coordination.landing(input, budget, cancel)?,
        )
    }
    fn submit(
        &mut self,
        input: SubmitInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        let budget = self.budget(None)?;
        response("fabric_submit", self.facade.submit(input, budget, cancel)?)
    }
    fn run(&mut self, input: RunInput, cancel: &CancelToken) -> Result<ToolResponse, ServiceError> {
        let budget = self.budget(match &input {
            RunInput::Wait { wait_ms, .. } => Some(*wait_ms),
            _ => None,
        })?;
        response("fabric_run", self.facade.run(input, budget, cancel)?)
    }
    fn output(
        &mut self,
        input: OutputInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        let budget = self.budget(None)?;
        response("fabric_output", self.facade.output(input, budget, cancel)?)
    }
    fn hosts(
        &mut self,
        input: HostsInput,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        let budget = self.budget(None)?;
        response("fabric_hosts", self.facade.hosts(input, budget, cancel)?)
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

/// Fixed catalogue order is part of both MCP profile contracts.
pub fn catalogue() -> Vec<Value> {
    schema::catalogue()
}

/// Test-only helpers are kept with the tests; this constant ensures the
/// ten-name public surface stays visible to callers.
pub const TOOL_NAMES: [&str; 10] = [
    "fabric_identity",
    "fabric_mail",
    "fabric_task",
    "fabric_events",
    "fabric_claim",
    "fabric_landing",
    "fabric_submit",
    "fabric_run",
    "fabric_output",
    "fabric_hosts",
];

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
