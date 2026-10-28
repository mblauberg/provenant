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
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// Parsed tool request. Coordination inputs are the runtime's exact serde
/// contracts; the execution facade contracts also live in runtime for F3.
pub enum ToolCall {
    Discover,
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
            Self::Mail(MailInput::Send { body: value, .. }, _) => body(value, "body"),
            Self::Mail(MailInput::Read { limit, .. }, _) => page(*limit),
            Self::Task(TaskInput::Create { objective, note }, _)
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
                | Self::Graph(
                    GraphInput::Run { .. } | GraphInput::Cancel { .. } | GraphInput::Resume { .. }
                )
                | Self::Submit(_)
                | Self::SubmitWait(..)
                | Self::Run(RunInput::Cancel { .. } | RunInput::Resume { .. })
                | Self::RunDetailed(RunInput::Cancel { .. } | RunInput::Resume { .. })
        )
    }

    /// Strict runtime serde contracts own argument validation. The catalogue
    /// supplies a field path for serde's structural errors at the MCP edge.
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
        if operation_id.is_some() && !call.is_mutation() {
            return Err(InvalidInput::Arguments {
                path: "arguments.operation_id".to_owned(),
            });
        }
        Ok(call)
    }

    /// A single match composes the runtime and the F3 seam. Cancellation only
    /// abandons the observation; run cancellation remains an explicit call.
    pub fn execute<F: F3Facade>(
        self,
        service: &mut Service<'_, F>,
        cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        // ToolCall is public and can be constructed without parse. Apply the
        // same transport cap before any coordination or facade side effects.
        match &self {
            Self::Discover => Ok(()),
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
                Self::Mail(..)
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
            Self::SubmitWait(_, wait_seconds, _) => *wait_seconds,
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
                let wait = service.facade.run(
                    RunInput::Wait {
                        targets: targets.clone(),
                        condition: WaitCondition::All,
                        wait_seconds,
                    },
                    budget,
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
                                        target,
                                        part: OutputPart::Result,
                                        offset: 0,
                                        max_bytes: remaining,
                                    },
                                    budget,
                                    cancel,
                                ) {
                                    Ok(output) => outputs.push(serde_json::to_value(output)?),
                                    Err(_) => {
                                        value["continuation"] = serde_json::json!(
                                            "Use fabric_output to read each result using the receipt targets."
                                        );
                                        break;
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
                let mut value = serde_json::to_value(service.facade.run(input, budget, cancel)?)?;
                compact_run(&mut value)?;
                response(tool_name, value)
            }
            Self::RunDetailed(input) => call!(facade, run, input),
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
    pub fn code(&self) -> &'static str {
        match self {
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
        let mut detail = match self {
            Self::Invalid(error) => serde_json::json!({"code":self.code(),"path":error.path}),
            Self::Coordination(CoordinationError::LaneMutation) => {
                serde_json::json!({"code":self.code(),"message":self.to_string()})
            }
            Self::Facade(FacadeError::DispatchRefused { .. })
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

fn new_operation_id() -> Result<String, provenant_os::fs::FsError> {
    Ok(format!(
        "mcp-{}",
        provenant_core::hex(&provenant_os::fs::entropy_128()?)
    ))
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
            let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) else {
                return Err(ServiceError::ResultTooLarge);
            };
            if items.len() <= 1
                || items
                    .last()
                    .and_then(|item| item.get("attempt_id"))
                    .is_none()
            {
                return Err(ServiceError::ResultTooLarge);
            }
            items.pop();
            let attempt = items
                .last()
                .and_then(|item| item.get("attempt_id"))
                .cloned()
                .ok_or(FacadeError::StateCorrupt)?;
            page["next_after"] = serde_json::json!({"attempt": attempt, "host": null});
            page["truncated"] = Value::Bool(true);
            response.content[0].text = render::digest("fabric_run", &response.structured_content);
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
