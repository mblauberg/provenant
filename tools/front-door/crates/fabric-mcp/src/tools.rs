//! Typed tool inputs and the single service boundary used by the transport.
use crate::{render, schema};
use provenant_core::{CancelToken, Clock, DurationMicros, OperationId, WaitBudget, WaitTransport};
use provenant_fabric_runtime::config::Limits;
pub use provenant_fabric_runtime::coordination::facade::*;
use provenant_fabric_runtime::coordination::input::{InputError, ValidateInput};
pub use provenant_fabric_runtime::coordination::{
    ClaimInput, CoordinationApi, CoordinationError, CoordinationService, EventsInput, LandingInput,
    MailInput, Mutation, TaskInput,
};
use provenant_fabric_runtime::inbox::{InboxError, InboxInput};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

fn page_default() -> u64 {
    50
}
fn depth_default() -> u64 {
    8
}
fn nodes_default() -> u64 {
    100
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConsoleInput {
    Projects,
    Overview,
    Sessions {
        session: Option<String>,
        #[serde(default = "page_default")]
        limit: u64,
        cursor: Option<String>,
    },
    Board {
        #[serde(default)]
        all: bool,
        #[serde(default = "page_default")]
        limit: u64,
        cursor: Option<String>,
        hosts: Option<String>,
    },
    Tree {
        #[serde(default = "depth_default")]
        max_depth: u64,
        #[serde(default = "nodes_default")]
        max_nodes: u64,
        #[serde(default)]
        detail: bool,
        hosts: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChairInput {
    SummaryShow {
        session: String,
    },
    SummaryDiff {
        session: String,
        from: u64,
        to: Option<u64>,
    },
    SummaryUpdate {
        session: String,
        patch: Value,
        expect_revision: u64,
    },
    LogList {
        session: String,
        after: Option<u64>,
        #[serde(default = "page_default")]
        limit: u64,
    },
    LogAppend {
        session: String,
        entry: Value,
        operation_id: Option<String>,
        issued_at: Option<i64>,
    },
}

impl ConsoleInput {
    fn validate(&self) -> Result<(), InputError> {
        let bad = |path: &str| InputError { path: path.into() };
        match self {
            Self::Sessions { limit, .. } | Self::Board { limit, .. }
                if !(1..=100).contains(limit) =>
            {
                Err(bad("limit"))
            }
            Self::Tree { max_depth, .. } if !(1..=16).contains(max_depth) => Err(bad("max_depth")),
            Self::Tree { max_nodes, .. } if !(1..=100).contains(max_nodes) => Err(bad("max_nodes")),
            _ => Ok(()),
        }
    }
}

impl ChairInput {
    fn validate(&self) -> Result<(), InputError> {
        let bad = |path: &str| InputError { path: path.into() };
        match self {
            Self::SummaryDiff { from, .. } if *from == 0 => Err(bad("from")),
            Self::SummaryDiff { to: Some(0), .. } => Err(bad("to")),
            Self::LogList { limit, .. } if !(1..=100).contains(limit) => Err(bad("limit")),
            Self::LogAppend {
                operation_id: Some(id),
                ..
            } if OperationId::parse(id).is_err() => Err(bad("operation_id")),
            _ => Ok(()),
        }
    }
}

pub trait McpFacade: F3Facade {
    fn inbox_owner(
        &mut self,
        input: InboxInput,
        host: Option<String>,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<provenant_fabric_runtime::inbox::InboxResult, ServiceError>;
    fn console(
        &mut self,
        _input: ConsoleInput,
        _budget: WaitBudget,
        _cancel: &CancelToken,
    ) -> Result<Value, ServiceError> {
        Err(ServiceError::Facade(FacadeError::InvalidInput))
    }
    fn chair(
        &mut self,
        _input: ChairInput,
        _budget: WaitBudget,
        _cancel: &CancelToken,
    ) -> Result<Value, ServiceError> {
        Err(ServiceError::Facade(FacadeError::InvalidInput))
    }
}

/// Parsed tool request. Coordination inputs are the runtime's exact serde
/// contracts; the execution facade contracts also live in runtime for F3.
pub enum ToolCall {
    Discover,
    Inbox(InboxInput, Option<String>),
    Console(ConsoleInput),
    Chair(ChairInput),
    Identity(IdentityInput),
    Mail(MailInput, Option<OperationId>),
    Task(TaskInput, Option<OperationId>),
    Events(EventsInput, Option<OperationId>),
    Claim(ClaimInput, Option<OperationId>),
    Landing(LandingInput, Option<OperationId>),
    Submit(SubmitInput),
    SubmitWait(SubmitInput, u64, bool),
    RunDetailed(RunInput),
    Run(RunInput),
    Output(OutputInput),
    Hosts(HostsInput),
    Graph(GraphInput),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvalidInput {
    #[error("unknown MCP tool")]
    UnknownTool,
    #[error("invalid tool argument at {path}")]
    Arguments { path: String },
}

impl ToolCall {
    pub fn name(&self) -> &'static str {
        schema::tool_name(self.kind())
    }

    fn kind(&self) -> schema::ToolKind {
        match self {
            Self::Discover => schema::ToolKind::Discover,
            Self::Inbox(..) => schema::ToolKind::Inbox,
            Self::Console(_) => schema::ToolKind::Console,
            Self::Chair(_) => schema::ToolKind::Chair,
            Self::Identity(_) => schema::ToolKind::Identity,
            Self::Mail(..) => schema::ToolKind::Mail,
            Self::Task(..) => schema::ToolKind::Task,
            Self::Events(..) => schema::ToolKind::Events,
            Self::Claim(..) => schema::ToolKind::Claim,
            Self::Landing(..) => schema::ToolKind::Claim,
            Self::Submit(_) | Self::SubmitWait(..) | Self::RunDetailed(_) => schema::ToolKind::Run,
            Self::Run(_) => schema::ToolKind::Run,
            Self::Output(_) => schema::ToolKind::Output,
            Self::Hosts(_) => schema::ToolKind::Identity,
            Self::Graph(_) => schema::ToolKind::Graph,
        }
    }
    // Never persist arbitrary caller-supplied tool names.
    pub(crate) fn usage_name(name: &str) -> &'static str {
        schema::tool_kind(name)
            .map(schema::tool_name)
            .unwrap_or("unknown")
    }

    fn validate_limits(&self, limits: &Limits) -> Result<(), InputError> {
        let body = |value: &str, path: &str| {
            if u64::try_from(value.len()).unwrap_or(u64::MAX) > limits.body_bytes {
                Err(InputError {
                    path: path.to_owned(),
                })
            } else {
                Ok(())
            }
        };
        let page = |value: u64| {
            if value > limits.page_rows {
                Err(InputError {
                    path: "limit".to_owned(),
                })
            } else {
                Ok(())
            }
        };
        let lease = |value: u64| {
            if value > limits.lease_seconds {
                Err(InputError {
                    path: "lease_seconds".to_owned(),
                })
            } else {
                Ok(())
            }
        };
        let output = |value: u64| {
            if value > limits.output_bytes {
                Err(InputError {
                    path: "max_bytes".to_owned(),
                })
            } else {
                Ok(())
            }
        };

        match self {
            Self::Inbox(InboxInput::Ask { request, .. }, _) => body(&request.body, "request.body"),
            Self::Inbox(InboxInput::List { limit, .. }, _) => page(*limit),
            Self::Inbox(
                InboxInput::Answer {
                    text: Some(text), ..
                },
                _,
            ) => body(text, "text"),
            Self::Inbox(InboxInput::Close { reason, .. }, _) => body(reason, "reason"),
            Self::Mail(MailInput::Send { body: value, .. }, _) => body(value, "body"),
            Self::Mail(MailInput::Read { limit, .. }, _) => page(*limit),
            Self::Task(
                TaskInput::Create {
                    objective, note, ..
                },
                _,
            )
            | Self::Task(
                TaskInput::Update {
                    objective, note, ..
                },
                _,
            ) => {
                body(objective, "objective")?;
                if let Some(note) = note {
                    body(note, "note")?;
                }
                Ok(())
            }
            Self::Task(TaskInput::List { limit, .. }, _) => page(*limit),
            Self::Task(TaskInput::Claim { lease_seconds, .. }, _) => lease(*lease_seconds),
            Self::Events(EventsInput::Read { limit, .. }, _) => page(*limit),
            Self::Events(EventsInput::Note { note, .. }, _) => body(note, "note"),
            Self::Claim(ClaimInput::List { limit, .. }, _) => page(*limit),
            Self::Claim(ClaimInput::Acquire { lease_seconds, .. }, _)
            | Self::Claim(ClaimInput::Renew { lease_seconds, .. }, _) => lease(*lease_seconds),
            Self::Landing(LandingInput::Acquire { lease_seconds, .. }, _)
            | Self::Landing(LandingInput::Renew { lease_seconds, .. }, _) => lease(*lease_seconds),
            Self::Submit(SubmitInput::Submit { tasks, .. })
            | Self::SubmitWait(SubmitInput::Submit { tasks, .. }, ..) => {
                for (index, task) in tasks.iter().enumerate() {
                    body(&task.prompt, &format!("tasks[{index}].prompt"))?;
                }
                Ok(())
            }
            Self::Run(RunInput::List { limit, .. })
            | Self::RunDetailed(RunInput::List { limit, .. }) => page(u64::from(*limit)),
            Self::Run(RunInput::Resume { prompt, .. })
            | Self::RunDetailed(RunInput::Resume { prompt, .. }) => body(prompt, "prompt"),
            Self::Output(OutputInput::Read { max_bytes, .. })
            | Self::Output(OutputInput::Fetch { max_bytes, .. }) => output(*max_bytes),
            Self::Hosts(HostsInput::Hosts { limit, .. }) => page(u64::from(*limit)),
            _ => Ok(()),
        }
    }

    fn is_mutation(&self) -> bool {
        if let Self::Inbox(input, _) = self {
            return input.is_mutation();
        }
        matches!(
            self,
            Self::Mail(MailInput::Send { .. } | MailInput::MarkRead { .. }, _)
                | Self::Task(
                    TaskInput::Create { .. }
                        | TaskInput::Claim { .. }
                        | TaskInput::Update { .. }
                        | TaskInput::Finish { .. }
                        | TaskInput::Release { .. },
                    _
                )
                | Self::Events(EventsInput::Note { .. }, _)
                | Self::Claim(
                    ClaimInput::Acquire { .. }
                        | ClaimInput::Renew { .. }
                        | ClaimInput::Release { .. },
                    _
                )
                | Self::Landing(
                    LandingInput::Acquire { .. }
                        | LandingInput::Renew { .. }
                        | LandingInput::Release { .. },
                    _
                )
                | Self::Chair(ChairInput::SummaryUpdate { .. } | ChairInput::LogAppend { .. })
                | Self::Graph(
                    GraphInput::Run { .. } | GraphInput::Cancel { .. } | GraphInput::Resume { .. }
                )
                | Self::Submit(_)
                | Self::SubmitWait(..)
                | Self::Run(RunInput::Cancel { .. } | RunInput::Resume { .. })
                | Self::RunDetailed(RunInput::Cancel { .. } | RunInput::Resume { .. })
        )
    }

    /// MCP arguments remain strict even when shared peer records retain fields
    /// from newer writers. The catalogue owns the argument field boundary.
    pub fn parse(name: &str, arguments: Value) -> Result<Self, InvalidInput> {
        // The shared serde record accepts peer-ingress worktree creation.
        // MCP cannot request it; reject the field (including null) at its edge.
        if schema::tool_kind(name) == Some(schema::ToolKind::Run)
            && arguments["op"] == "submit"
            && let Some(index) =
                arguments
                    .get("tasks")
                    .and_then(Value::as_array)
                    .and_then(|tasks| {
                        tasks
                            .iter()
                            .position(|task| task.get("new_worktree").is_some())
                    })
        {
            return Err(InvalidInput::Arguments {
                path: format!("arguments.tasks[{index}].new_worktree"),
            });
        }
        let mut arguments = arguments;
        if let Some(operation) = schema::sole_operation(name)
            && let Some(object) = arguments.as_object_mut()
        {
            object
                .entry("op")
                .or_insert_with(|| Value::String(operation.to_owned()));
        }
        let operation = arguments["op"].as_str().unwrap_or("").to_owned();
        if let Some(path) = schema::invalid_path(name, &arguments) {
            return Err(InvalidInput::Arguments { path });
        }
        let mut detail = false;
        let mut submit_wait = 30;
        if let Some(object) = arguments.as_object_mut() {
            if schema::tool_kind(name) == Some(schema::ToolKind::Run) {
                if let Some(value) = object.remove("detail") {
                    if !matches!(operation.as_str(), "submit" | "status" | "wait" | "cancel") {
                        return Err(InvalidInput::Arguments {
                            path: "arguments.detail".into(),
                        });
                    }
                    detail = value.as_bool().ok_or_else(|| InvalidInput::Arguments {
                        path: "arguments.detail".into(),
                    })?;
                }
                if operation == "submit"
                    && let Some(value) = object.remove("wait_seconds")
                {
                    submit_wait = value
                        .as_u64()
                        .filter(|v| (1..=crate::MAX_WAIT_SECONDS).contains(v))
                        .ok_or_else(|| InvalidInput::Arguments {
                            path: "arguments.wait_seconds".into(),
                        })?;
                }
                if matches!(operation.as_str(), "submit" | "cancel" | "resume")
                    && !object.contains_key("operation_id")
                {
                    object.insert(
                        "operation_id".into(),
                        Value::String(new_operation_id().map_err(|_| InvalidInput::Arguments {
                            path: "arguments.operation_id".into(),
                        })?),
                    );
                }
                if operation == "list" {
                    object.entry("limit").or_insert(serde_json::json!(20));
                }
            }
            if schema::tool_kind(name) == Some(schema::ToolKind::Output) && operation == "read" {
                object.entry("part").or_insert(serde_json::json!("result"));
                object.entry("offset").or_insert(serde_json::json!(0));
                object
                    .entry("max_bytes")
                    .or_insert(serde_json::json!(16_384));
            }
        }
        let host = if schema::tool_kind(name) == Some(schema::ToolKind::Inbox) {
            let host = arguments.as_object_mut().and_then(|o| o.remove("host"));
            if host.is_some() && operation == "ask" {
                return Err(InvalidInput::Arguments {
                    path: "arguments.host".into(),
                });
            }
            match host {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if !s.is_empty() && s.len() <= 128 && !s.contains('\0') => {
                    Some(s)
                }
                _ => {
                    return Err(InvalidInput::Arguments {
                        path: "arguments.host".into(),
                    });
                }
            }
        } else {
            None
        };
        let operation_id =
            match schema::tool_kind(name) {
                Some(
                    schema::ToolKind::Mail
                    | schema::ToolKind::Task
                    | schema::ToolKind::Events
                    | schema::ToolKind::Claim,
                ) => match arguments
                    .as_object_mut()
                    .and_then(|object| object.remove("operation_id"))
                {
                    Some(Value::String(value)) => Some(OperationId::parse(&value).map_err(
                        |_| InvalidInput::Arguments {
                            path: "arguments.operation_id".to_owned(),
                        },
                    )?),
                    Some(_) => {
                        return Err(InvalidInput::Arguments {
                            path: "arguments.operation_id".to_owned(),
                        });
                    }
                    None => None,
                },
                _ => None,
            };
        macro_rules! parse {
            ($ty:ty, $variant:ident $(, $metadata:expr)*) => {
                serde_json::from_value::<$ty>(arguments.clone())
                    .and_then(|input| {
                        input
                            .validate_input(WaitTransport::Mcp)
                            .map_err(serde::de::Error::custom)?;
                        Ok(Self::$variant(input $(, $metadata)*))
                    })
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
        // Merged tools route each op to the input type that owns it.
        let operation = arguments["op"].as_str().unwrap_or("");
        let call = match schema::tool_kind(name) {
            Some(schema::ToolKind::Discover) => {
                if arguments.as_object().is_some_and(|o| {
                    o.is_empty()
                        || (o.len() == 1 && o.get("op") == Some(&serde_json::json!("discover")))
                }) {
                    Ok(Self::Discover)
                } else {
                    Err(InvalidInput::Arguments {
                        path: "arguments".into(),
                    })
                }
            }
            Some(schema::ToolKind::Identity) if operation == "hosts" => parse!(HostsInput, Hosts),
            Some(schema::ToolKind::Inbox) => parse!(InboxInput, Inbox, host),
            Some(schema::ToolKind::Console | schema::ToolKind::Chair) => {
                if let Some(path) = schema::invalid_path(name, &arguments) {
                    return Err(InvalidInput::Arguments { path });
                }
                if schema::tool_kind(name) == Some(schema::ToolKind::Console) {
                    serde_json::from_value(arguments.clone()).map(Self::Console)
                } else {
                    serde_json::from_value(arguments.clone()).map(Self::Chair)
                }
                .map_err(|_| InvalidInput::Arguments {
                    path: "arguments".into(),
                })
            }
            Some(schema::ToolKind::Identity) => parse!(IdentityInput, Identity),
            Some(schema::ToolKind::Mail) => parse!(MailInput, Mail, operation_id.clone()),
            Some(schema::ToolKind::Task) => parse!(TaskInput, Task, operation_id.clone()),
            Some(schema::ToolKind::Events) => parse!(EventsInput, Events, operation_id.clone()),
            Some(schema::ToolKind::Claim) if operation.starts_with("landing_") => {
                parse!(LandingInput, Landing, operation_id.clone())
            }
            Some(schema::ToolKind::Claim) => parse!(ClaimInput, Claim, operation_id.clone()),
            Some(schema::ToolKind::Run) if operation == "submit" => {
                parse!(SubmitInput, SubmitWait, submit_wait, detail)
            }
            Some(schema::ToolKind::Run) if detail => parse!(RunInput, RunDetailed),
            Some(schema::ToolKind::Run) => parse!(RunInput, Run),
            Some(schema::ToolKind::Output) => parse!(OutputInput, Output),
            Some(schema::ToolKind::Graph) => parse!(GraphInput, Graph),
            None => Err(InvalidInput::UnknownTool),
        }?;
        let validation = match &call {
            Self::Console(input) => input.validate(),
            Self::Chair(input) => input.validate(),
            _ => Ok(()),
        };
        validation.map_err(|error| InvalidInput::Arguments {
            path: format!("arguments.{}", error.path),
        })?;
        if operation_id.is_some() && !call.is_mutation() {
            return Err(InvalidInput::Arguments {
                path: "arguments.operation_id".to_owned(),
            });
        }
        Ok(call)
    }

    /// A single match composes the runtime and the F3 seam. Cancellation only
    /// abandons the observation; run cancellation remains an explicit call.
    pub fn execute<F: McpFacade>(
        mut self,
        service: &mut Service<'_, F>,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        if let Self::Chair(ChairInput::LogAppend {
            operation_id,
            issued_at,
            ..
        }) = &mut self
        {
            if operation_id.is_none() {
                let timestamp = match *issued_at {
                    Some(timestamp) => timestamp,
                    None => service.clock.wall_now()?.as_timestamp().as_second(),
                };
                *operation_id = Some(format!("mcp-log-{timestamp}-{}", new_operation_id()?));
                *issued_at = Some(timestamp);
            } else if issued_at.is_none() {
                *issued_at = operation_id
                    .as_deref()
                    .and_then(|id| id.strip_prefix("mcp-log-")?.split_once('-')?.0.parse().ok());
            }
        }
        if let Self::Inbox(
            InboxInput::Ask { operation_id, .. }
            | InboxInput::Answer { operation_id, .. }
            | InboxInput::Close { operation_id, .. }
            | InboxInput::Acknowledge { operation_id, .. },
            _,
        ) = &mut self
            && operation_id.is_none()
        {
            *operation_id = Some(
                OperationId::parse(&new_operation_id()?)
                    .map_err(|_| CoordinationError::EntropyEncoding)?,
            );
        }
        let operation_id = self.operation_id();
        self.execute_inner(service, cancel)
            .map_err(|error| match operation_id {
                Some(operation_id) => ServiceError::Mutation {
                    operation_id,
                    source: Box::new(error),
                },
                None => error,
            })
    }

    pub(crate) fn operation_id(&self) -> Option<OperationId> {
        match self {
            Self::Inbox(
                InboxInput::Ask { operation_id, .. }
                | InboxInput::Answer { operation_id, .. }
                | InboxInput::Close { operation_id, .. }
                | InboxInput::Acknowledge { operation_id, .. },
                _,
            ) => operation_id.clone(),
            Self::Chair(ChairInput::LogAppend {
                operation_id: Some(id),
                ..
            }) => OperationId::parse(id).ok(),
            Self::Submit(SubmitInput::Submit { operation_id, .. })
            | Self::SubmitWait(SubmitInput::Submit { operation_id, .. }, ..)
            | Self::Run(
                RunInput::Cancel { operation_id, .. } | RunInput::Resume { operation_id, .. },
            )
            | Self::RunDetailed(
                RunInput::Cancel { operation_id, .. } | RunInput::Resume { operation_id, .. },
            ) => Some(operation_id.clone()),
            _ => None,
        }
    }

    fn execute_inner<F: McpFacade>(
        self,
        service: &mut Service<'_, F>,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        // ToolCall is public and can be constructed without parse. Apply the
        // same transport cap before any coordination or facade side effects.
        match &self {
            Self::Discover => Ok(()),
            Self::Inbox(input, _) => input.validate_input(WaitTransport::Mcp),
            Self::Console(input) => input.validate(),
            Self::Chair(input) => input.validate(),
            Self::Identity(input) => input.validate_input(WaitTransport::Mcp),
            Self::Mail(input, _) => input.validate_input(WaitTransport::Mcp),
            Self::Task(input, _) => input.validate_input(WaitTransport::Mcp),
            Self::Events(input, _) => input.validate_input(WaitTransport::Mcp),
            Self::Claim(input, _) => input.validate_input(WaitTransport::Mcp),
            Self::Landing(input, _) => input.validate_input(WaitTransport::Mcp),
            Self::Submit(input) | Self::SubmitWait(input, ..) => {
                input.validate_input(WaitTransport::Mcp)
            }
            Self::Run(input) | Self::RunDetailed(input) => input.validate_input(WaitTransport::Mcp),
            Self::Output(input) => input.validate_input(WaitTransport::Mcp),
            Self::Hosts(input) => input.validate_input(WaitTransport::Mcp),
            Self::Graph(input) => input.validate_input(WaitTransport::Mcp),
        }
        .map_err(ServiceError::Invalid)?;
        if let Some(limits) = &service.limits {
            self.validate_limits(limits)
                .map_err(ServiceError::Invalid)?;
        }
        if service.in_lane
            && self.is_mutation()
            && matches!(
                &self,
                Self::Inbox(..)
                    | Self::Chair(..)
                    | Self::Mail(..)
                    | Self::Task(..)
                    | Self::Events(..)
                    | Self::Claim(..)
                    | Self::Landing(..)
                    | Self::Graph(..)
                    | Self::Submit(SubmitInput::Submit { host: Some(_), .. })
                    | Self::SubmitWait(SubmitInput::Submit { host: Some(_), .. }, ..)
            )
        {
            return Err(CoordinationError::LaneMutation.into());
        }
        let mutation = self.is_mutation();
        let output = matches!(&self, Self::Output(_));
        let tool_name = self.name();
        let budget = service.budget(match &self {
            Self::Run(RunInput::Wait { wait_seconds, .. })
            | Self::RunDetailed(RunInput::Wait { wait_seconds, .. }) => *wait_seconds,
            Self::Events(
                EventsInput::Read {
                    wait_seconds: Some(wait_seconds),
                    ..
                },
                _,
            ) => *wait_seconds,
            _ => crate::MAX_WAIT_SECONDS,
        })?;
        macro_rules! call {
            ($owner:ident, $method:ident, $input:ident) => {{ response(tool_name, service.$owner.$method($input, budget, cancel)?) }};
        }
        let result = match self {
            Self::Inbox(input, host) => response(
                tool_name,
                service.facade.inbox_owner(input, host, budget, cancel)?,
            ),
            Self::Console(input) => {
                response(tool_name, service.facade.console(input, budget, cancel)?)
            }
            Self::Chair(input) => response(tool_name, service.facade.chair(input, budget, cancel)?),
            Self::Discover => response(
                tool_name,
                serde_json::json!({"tools": schema::advanced_catalogue()}),
            ),
            Self::Identity(input) => response(
                tool_name,
                IdentityReply {
                    identity: service.facade.identity(input, budget, cancel)?,
                    server_version: env!("CARGO_PKG_VERSION"),
                },
            ),
            Self::Mail(input, operation_id) => {
                if matches!(&input, MailInput::Send { .. } | MailInput::MarkRead { .. }) {
                    response(
                        tool_name,
                        CoordinationApi::mutate(
                            &service.coordination,
                            Mutation::Mail(input),
                            operation_id,
                            budget,
                            cancel,
                        )?,
                    )
                } else {
                    response(tool_name, service.coordination.mail(input, budget, cancel)?)
                }
            }
            Self::Task(input, operation_id) => {
                if !matches!(&input, TaskInput::List { .. }) {
                    response(
                        tool_name,
                        CoordinationApi::mutate(
                            &service.coordination,
                            Mutation::Task(input),
                            operation_id,
                            budget,
                            cancel,
                        )?,
                    )
                } else {
                    response(tool_name, service.coordination.task(input, budget, cancel)?)
                }
            }
            Self::Events(
                EventsInput::Read {
                    after,
                    limit,
                    wait_seconds,
                    ..
                },
                _,
            ) => {
                let mut last_good_coverage = None;
                loop {
                    let coverage = if budget.remaining(service.clock.mono_now()).as_micros() > 0 {
                        match service.facade.refresh_events(budget, cancel) {
                            Ok(value) => {
                                last_good_coverage = Some(value.clone());
                                value
                            }
                            Err(error)
                                if wait_seconds.is_some()
                                    && budget.remaining(service.clock.mono_now()).as_micros()
                                        == 0 =>
                            {
                                coverage_after_timeout(last_good_coverage.as_ref(), error)?
                            }
                            Err(error) => return Err(error.into()),
                        }
                    } else {
                        last_good_coverage.clone().unwrap_or_default()
                    };
                    let result = if budget.remaining(service.clock.mono_now()).as_micros() == 0 {
                        provenant_fabric_runtime::coordination::EventsResult::empty_timeout(after)
                    } else {
                        match service.coordination.events(
                            EventsInput::Read {
                                extra: Default::default(),
                                after,
                                limit,
                                wait_seconds: None,
                            },
                            budget,
                            cancel,
                        ) {
                            Ok(value) => value,
                            Err(_)
                                if wait_seconds.is_some()
                                    && budget.remaining(service.clock.mono_now()).as_micros()
                                        == 0 =>
                            {
                                provenant_fabric_runtime::coordination::EventsResult::empty_timeout(
                                    after,
                                )
                            }
                            Err(error) => return Err(error.into()),
                        }
                    };
                    let provenant_fabric_runtime::coordination::EventsResult::Read {
                        page,
                        cursor,
                        unknown_hosts,
                        run_events_pending,
                        ..
                    } = result
                    else {
                        return Err(ServiceError::Facade(FacadeError::StateCorrupt));
                    };
                    let timed_out = wait_seconds.is_some()
                        && budget.remaining(service.clock.mono_now()).as_micros() == 0;
                    if !page.items.is_empty() || wait_seconds.is_none() || timed_out {
                        let mut unknown_hosts = unknown_hosts;
                        unknown_hosts.extend(coverage.unknown_hosts);
                        unknown_hosts.sort();
                        unknown_hosts.dedup();
                        break response(
                            tool_name,
                            provenant_fabric_runtime::coordination::EventsResult::Read {
                                extra: Default::default(),
                                page,
                                cursor,
                                timed_out,
                                unknown_hosts,
                                run_events_pending: run_events_pending || coverage.more_events,
                            },
                        );
                    }
                    service.coordination.pause_observation(budget, cancel);
                }
            }
            Self::Events(input @ EventsInput::Note { .. }, operation_id) => response(
                tool_name,
                CoordinationApi::mutate(
                    &service.coordination,
                    Mutation::Events(input),
                    operation_id,
                    budget,
                    cancel,
                )?,
            ),
            Self::Claim(input, operation_id) => {
                if matches!(&input, ClaimInput::Verify { .. } | ClaimInput::List { .. }) {
                    response(
                        tool_name,
                        service.coordination.claim(input, budget, cancel)?,
                    )
                } else {
                    response(
                        tool_name,
                        CoordinationApi::mutate(
                            &service.coordination,
                            Mutation::Claim(input),
                            operation_id,
                            budget,
                            cancel,
                        )?,
                    )
                }
            }
            Self::Landing(input, operation_id) => {
                if matches!(
                    &input,
                    LandingInput::Verify { .. } | LandingInput::Wait { .. }
                ) {
                    response(
                        tool_name,
                        service.coordination.landing(input, budget, cancel)?,
                    )
                } else {
                    response(
                        tool_name,
                        CoordinationApi::mutate(
                            &service.coordination,
                            Mutation::Landing(input),
                            operation_id,
                            budget,
                            cancel,
                        )?,
                    )
                }
            }
            Self::Submit(input) => call!(facade, submit, input),
            Self::SubmitWait(input, wait_seconds, detail) => {
                let receipt = serde_json::to_value(service.facade.submit(input, budget, cancel)?)?;
                let targets: Vec<AttemptTarget> = serde_json::from_value(
                    receipt
                        .get("targets")
                        .cloned()
                        .ok_or(FacadeError::StateCorrupt)?,
                )?;
                let wait_budget = service.budget(wait_seconds)?;
                let wait = service.facade.run(
                    RunInput::Wait {
                        extra: Default::default(),
                        targets: targets.clone(),
                        condition: WaitCondition::All,
                        wait_seconds,
                    },
                    wait_budget,
                    cancel,
                );
                let mut value = serde_json::json!({"receipt": receipt});
                match wait {
                    Ok(run) => {
                        let mut run = serde_json::to_value(run)?;
                        if !detail {
                            compact_run(&mut run)?;
                        }
                        let timed_out = run
                            .get("timed_out")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        value["wait"] = run;
                        if timed_out {
                            value["continuation"] = serde_json::json!(
                                "Use fabric_run wait with the receipt targets, then fabric_output to read the result."
                            );
                        } else {
                            let mut outputs = Vec::new();
                            let output_budget = service.budget(crate::MAX_WAIT_SECONDS)?;
                            for target in targets {
                                let remaining = service.limits.as_ref().map_or(16_384, |limits| {
                                    limits
                                        .result_bytes
                                        .saturating_sub(
                                            serde_json::to_vec(&value)
                                                .map_or(u64::MAX, |v| v.len() as u64),
                                        )
                                        .saturating_sub(
                                            serde_json::to_vec(&outputs)
                                                .map_or(u64::MAX, |v| v.len() as u64),
                                        )
                                        .saturating_sub(1024)
                                        .min(16_384)
                                        / 2
                                });
                                if remaining == 0 {
                                    value["continuation"] = serde_json::json!(
                                        "Use fabric_output to read the remaining results using the receipt targets."
                                    );
                                    break;
                                }
                                match service.facade.output(
                                    OutputInput::Read {
                                        extra: Default::default(),
                                        target: target.clone(),
                                        part: OutputPart::Result,
                                        offset: 0,
                                        max_bytes: remaining,
                                    },
                                    output_budget,
                                    cancel,
                                ) {
                                    Ok(output) => outputs.push(serde_json::to_value(output)?),
                                    Err(error) => {
                                        outputs.push(serde_json::json!({"target": target, "error": {"code": error.code(), "message": error.to_string()}}));
                                        value["continuation"] = serde_json::json!(
                                            "Use fabric_output to read each result using the receipt targets."
                                        );
                                    }
                                }
                            }
                            value["outputs"] = Value::Array(outputs);
                        }
                    }
                    Err(error) => {
                        value["continuation"] = serde_json::json!(
                            "Use fabric_run wait with the receipt targets, then fabric_output to read the result."
                        );
                        value["timed_out"] = Value::Bool(matches!(error, FacadeError::Timeout));
                        value["observation_error"] = serde_json::json!({"code": error.code(), "message": error.to_string(), "hint":"Use the receipt targets to continue observing the submitted runs."});
                    }
                }
                if let Some(limits) = &service.limits {
                    let size = serde_json::to_vec(&value)?.len() as u64;
                    if size > limits.result_bytes {
                        value
                            .as_object_mut()
                            .ok_or(FacadeError::StateCorrupt)?
                            .remove("outputs");
                        value
                            .as_object_mut()
                            .ok_or(FacadeError::StateCorrupt)?
                            .remove("wait");
                        value["continuation"] = serde_json::json!(
                            "Use fabric_run wait with the receipt targets, then fabric_output to read the result."
                        );
                    }
                }
                response(tool_name, value)
            }
            Self::Run(input) => {
                let operation_id = match &input {
                    RunInput::Cancel { operation_id, .. }
                    | RunInput::Resume { operation_id, .. } => Some(operation_id.clone()),
                    _ => None,
                };
                let mut value = serde_json::to_value(service.facade.run(input, budget, cancel)?)?;
                compact_run(&mut value)?;
                if let Some(operation_id) = operation_id {
                    value["operation_id"] = serde_json::to_value(operation_id)?;
                }
                response(tool_name, value)
            }
            Self::RunDetailed(input) => {
                let operation_id = match &input {
                    RunInput::Cancel { operation_id, .. }
                    | RunInput::Resume { operation_id, .. } => Some(operation_id.clone()),
                    _ => None,
                };
                let mut value = serde_json::to_value(service.facade.run(input, budget, cancel)?)?;
                if let Some(operation_id) = operation_id {
                    value["operation_id"] = serde_json::to_value(operation_id)?;
                }
                response(tool_name, value)
            }
            Self::Output(input) => call!(facade, output, input),
            Self::Hosts(input) => {
                #[derive(Serialize)]
                struct Hosts<T> {
                    hosts: T,
                }
                response(
                    tool_name,
                    Hosts {
                        hosts: service.facade.hosts(input, budget, cancel)?,
                    },
                )
            }
            Self::Graph(input) => call!(facade, graph, input),
        };
        service.limit_response(result, mutation, output)
    }
}

/// Typed errors crossing the MCP edge. Library details remain available as
/// sources while the protocol exposes a stable domain code.
#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("{message}")]
    Cli {
        code: String,
        message: String,
        data: Value,
    },
    #[error(transparent)]
    Inbox(#[from] InboxError),
    #[error("{message}")]
    InboxOwner { code: String, message: String },
    #[error("{source}")]
    Mutation {
        operation_id: OperationId,
        #[source]
        source: Box<ServiceError>,
    },
    #[error(transparent)]
    Invalid(#[from] InputError),
    #[error(transparent)]
    Coordination(#[from] CoordinationError),
    #[error("execution facade refused the request: {0}")]
    Facade(#[from] FacadeError),
    #[error("tool response could not be encoded")]
    Encode(#[from] serde_json::Error),
    #[error("tool result exceeds the configured output limit")]
    ResultTooLarge,
    #[error("MCP wait budget could not be created")]
    Time(#[from] provenant_core::TimeError),
}

impl ServiceError {
    pub fn code(&self) -> &str {
        match self {
            Self::Mutation { source, .. } => source.code(),
            Self::Inbox(error) => error.code(),
            Self::InboxOwner { code, .. } | Self::Cli { code, .. } => code,
            Self::Invalid(_) => "invalid_input",
            Self::Coordination(error) => error.code(),
            Self::Facade(error) => error.code(),
            Self::Encode(_) => "state_corrupt",
            Self::ResultTooLarge => "result_too_large",
            Self::Time(_) => "invalid_input",
        }
    }

    /// Stable structured error data. Uncertain mutations carry their
    /// idempotency key; prompts, tokens and raw source errors are omitted.
    pub fn structured_error(&self) -> Value {
        if let Self::Coordination(CoordinationError::Store(error)) = self {
            let facade = FacadeError::from(error);
            if matches!(facade, FacadeError::UpgradeRequired { .. }) {
                return Self::Facade(facade).structured_error();
            }
        }
        if let Self::Mutation {
            operation_id,
            source,
        } = self
        {
            let mut detail = source.structured_error();
            detail["operation_id"] = serde_json::json!(operation_id);
            return detail;
        }
        let mut detail = match self {
            Self::Cli { data, .. } => {
                let mut detail = data.as_object().cloned().unwrap_or_default();
                detail.insert("data".into(), data.clone());
                detail.insert("code".into(), Value::String(self.code().into()));
                Value::Object(detail)
            }
            Self::Invalid(error) => serde_json::json!({"code":self.code(),"path":error.path}),
            Self::Coordination(CoordinationError::LaneMutation) => {
                serde_json::json!({"code":self.code(),"message":self.to_string()})
            }
            Self::Facade(FacadeError::DispatchRefused { .. })
            | Self::Facade(FacadeError::UpgradeRequired { .. })
            | Self::Facade(FacadeError::PeerProtocolMismatch { .. })
            | Self::Coordination(CoordinationError::Peer(
                provenant_fabric_runtime::hosts::PeerError::ProtocolMismatch { .. },
            )) => {
                serde_json::json!({"code":self.code(),"message":self.to_string()})
            }
            Self::Facade(
                error @ (FacadeError::Io { .. } | FacadeError::WorktreeUnregistered { .. }),
            ) => serde_json::to_value(error)
                .unwrap_or_else(|_| serde_json::json!({"code":self.code()})),
            Self::Coordination(CoordinationError::Input(error)) => {
                serde_json::json!({"code":self.code(),"path":error.path})
            }
            Self::Facade(FacadeError::LaunchUnknown { operation_id })
            | Self::Coordination(CoordinationError::Peer(
                provenant_fabric_runtime::hosts::PeerError::Unknown { operation_id },
            )) => {
                serde_json::json!({"code":self.code(),"operation_id":operation_id})
            }
            _ => serde_json::json!({"code":self.code()}),
        };
        detail["message"] = Value::String(self.to_string());
        detail["hint"] = Value::String(
            match self {
                Self::Invalid(_) | Self::Time(_) => {
                    "Check the argument path and tool schema, then retry."
                }
                Self::Facade(FacadeError::LaunchUnknown { .. }) => {
                    "Check status before retrying with the same operation ID."
                }
                Self::ResultTooLarge => "Request a smaller page or output chunk.",
                _ => "Inspect the current state and retry when the cause is resolved.",
            }
            .into(),
        );
        detail
    }
}

fn new_operation_id() -> Result<String, provenant_fabric_runtime::coordination::CoordinationError> {
    provenant_fabric_runtime::coordination::generated_operation_id("mcp")
}

fn compact_run(value: &mut Value) -> Result<(), ServiceError> {
    fn compact(value: &mut Value) -> Result<(), ServiceError> {
        let mut record = value.clone();
        let object = record.as_object_mut().ok_or(FacadeError::StateCorrupt)?;
        if let Some(seconds) = object.remove("provider_seconds") {
            object.insert(
                "provider_ms".into(),
                if seconds.is_null() {
                    Value::Null
                } else {
                    serde_json::json!(
                        (seconds.as_f64().ok_or(FacadeError::StateCorrupt)? * 1000.0).round()
                            as u64
                    )
                },
            );
        }
        let record: provenant_records::run::AttemptRecord = serde_json::from_value(record)?;
        *value =
            serde_json::to_value(provenant_fabric_runtime::runs::AttemptSummary::from(record))?;
        Ok(())
    }
    if let Some(attempt) = value.get_mut("attempt") {
        compact(attempt)?;
    }
    if let Some(attempts) = value.get_mut("attempts").and_then(Value::as_array_mut) {
        for attempt in attempts {
            compact(attempt)?;
        }
    }
    Ok(())
}

fn coverage_after_timeout(
    last_good: Option<&EventCoverage>,
    error: FacadeError,
) -> Result<EventCoverage, FacadeError> {
    last_good.cloned().ok_or(error)
}

/// One worker's concrete runtime composition. The clock is sampled for each
/// call so a long-lived worker never reuses an expired deadline.
pub struct Service<'a, F> {
    pub coordination: CoordinationService<'a>,
    pub facade: F,
    clock: &'a dyn Clock,
    limits: Option<Limits>,
    in_lane: bool,
}

impl<'a, F> Service<'a, F> {
    pub fn new(coordination: CoordinationService<'a>, facade: F, clock: &'a dyn Clock) -> Self {
        Self {
            coordination,
            facade,
            clock,
            limits: None,
            in_lane: false,
        }
    }

    /// Apply validated profile limits at the MCP owner boundary.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// The composition edge captures this from the host-set MCP environment.
    /// Tool arguments cannot change it. Lanes still have direct SQLite writes;
    /// this guards the tool surface pending lane-store-isolation (Fabric spec).
    pub fn with_lane_restrictions(mut self, in_lane: bool) -> Self {
        self.in_lane = in_lane;
        self
    }

    fn limit_response(
        &self,
        response: Result<ToolResponse, ServiceError>,
        mutation: bool,
        output: bool,
    ) -> Result<ToolResponse, ServiceError> {
        let response = response?;
        match &self.limits {
            Some(limits) => limit_response(response, limits, mutation, output),
            None => Ok(response),
        }
    }

    fn budget(&self, seconds: u64) -> Result<WaitBudget, ServiceError> {
        let micros = seconds
            .checked_mul(1_000_000)
            .ok_or(provenant_core::TimeError::Overflow)?;
        Ok(WaitBudget::new(
            self.clock.mono_now(),
            DurationMicros::from_micros(micros),
            WaitTransport::Mcp,
        )?)
    }
}

fn limit_response(
    mut response: ToolResponse,
    limits: &Limits,
    mutation: bool,
    output: bool,
) -> Result<ToolResponse, ServiceError> {
    // Never hide a durable mutation receipt because the configured read-result limit is lower.
    if !mutation {
        let output_text = response
            .structured_content
            .get("bytes")
            .and_then(|bytes| bytes.get("data"))
            .and_then(Value::as_str);
        if output
            && output_text.is_some_and(|text| {
                u64::try_from(text.lines().count()).unwrap_or(u64::MAX) > limits.output_lines
            })
        {
            return Err(ServiceError::ResultTooLarge);
        }
        loop {
            let response_bytes = serde_json::to_vec(&response)?.len();
            if u64::try_from(response_bytes).unwrap_or(u64::MAX) <= limits.result_bytes {
                break;
            }
            let Some(page) = response.structured_content.get_mut("page") else {
                return Err(ServiceError::ResultTooLarge);
            };
            let host = page
                .get("next_after")
                .and_then(|cursor| cursor.get("host"))
                .cloned()
                .unwrap_or(Value::Null);
            let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) else {
                return Err(ServiceError::ResultTooLarge);
            };
            if items.len() <= 1 {
                return Err(ServiceError::ResultTooLarge);
            }
            items.pop();
            let last = items.last().ok_or(FacadeError::StateCorrupt)?;
            let attempt = last.get("attempt_id").cloned();
            let sequence = last.get("seq").cloned();
            if attempt.is_none() && sequence.is_none() && last.get("id").is_none() {
                return Err(ServiceError::ResultTooLarge);
            }
            if let Some(attempt) = attempt {
                page["next_after"] = serde_json::json!({"attempt": attempt, "host": host});
            }
            page["truncated"] = Value::Bool(true);
            if let Some(sequence) = sequence {
                response.structured_content["cursor"] = sequence;
            }
            let tool = response.content[0]
                .text
                .split_whitespace()
                .next()
                .unwrap_or("fabric");
            response.content[0].text = render::digest(tool, &response.structured_content);
        }
    }
    Ok(response)
}

fn response<T: Serialize>(tool: &str, typed: T) -> Result<ToolResponse, ServiceError> {
    let structured_content = serde_json::to_value(typed)?;
    let text = render::digest(tool, &structured_content);
    Ok(ToolResponse {
        content: vec![TextContent { kind: "text", text }],
        structured_content,
    })
}

/// Identity plus this server's version. A reinstall does not restart a running
/// server, so agents compare `server_version` with `provenant --version`.
#[derive(Serialize)]
pub(crate) struct IdentityReply<T> {
    #[serde(flatten)]
    pub(crate) identity: T,
    pub(crate) server_version: &'static str,
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

/// Schema-bearing tools report error details in text, because those details
/// cannot satisfy their success schemas. Unknown-tool fallbacks do the same.
pub fn tool_error(tool: Option<&str>, code: &str, detail: Value) -> Value {
    let mut result = serde_json::json!({"content":[{"type":"text","text":format!("{code}: {detail}")}],"isError":true});
    if tool.is_some_and(|name| {
        schema::tool_kind(name)
            .and_then(schema::output_schema)
            .is_none()
    }) {
        result["structuredContent"] = detail;
    }
    result
}

pub fn catalogue() -> &'static [Value] {
    schema::catalogue()
}

pub fn legacy_catalogue() -> &'static [Value] {
    schema::legacy_catalogue()
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
