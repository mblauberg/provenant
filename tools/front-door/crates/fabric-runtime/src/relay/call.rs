//! Lane relay calls: the typed coordination requests a lane sends its host.
//! The host binds sender identity itself; a call carries only the request.
use crate::config::Limits;
pub use crate::coordination::facade::FacadeError;
use crate::coordination::input::InputError;
use crate::coordination::{
    ClaimInput, CoordinationApi, CoordinationError, CoordinationService, EventsInput, MailInput,
    Mutation,
};
use crate::inbox::InboxError;
use crate::relay::MAX_WAIT_SECONDS;
use provenant_core::{CancelToken, OperationId, WaitBudget, WaitTransport};
use serde_json::Value;
use thiserror::Error;

/// Wire names of the relayed coordination requests.
const MAIL: &str = "fabric_mail";
const EVENTS: &str = "fabric_events";
const CLAIM: &str = "fabric_lease";

/// A parsed relay request. Inputs are the runtime's exact serde contracts.
pub enum ToolCall {
    Mail(MailInput, Option<OperationId>),
    Events(EventsInput, Option<OperationId>),
    Claim(ClaimInput, Option<OperationId>),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvalidInput {
    #[error("unknown relay request")]
    UnknownTool,
    #[error("invalid relay argument at {path}")]
    Arguments { path: String },
    #[error("{0}")]
    Validated(InputError),
}

impl ToolCall {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Mail(..) => MAIL,
            Self::Events(..) => EVENTS,
            Self::Claim(..) => CLAIM,
        }
    }

    fn validate_limits(&self, limits: &Limits) -> Result<(), InputError> {
        let body = |value: &str, path: &str| {
            if u64::try_from(value.len()).unwrap_or(u64::MAX) > limits.body_bytes {
                Err(InputError::new(path))
            } else {
                Ok(())
            }
        };
        let page = |value: u64| {
            if value > limits.page_rows {
                Err(InputError::new("limit"))
            } else {
                Ok(())
            }
        };
        let lease = |value: u64| {
            if value > limits.lease_seconds {
                Err(InputError::new("lease_seconds"))
            } else {
                Ok(())
            }
        };
        match self {
            Self::Mail(MailInput::Send { body: value, .. }, _) => body(value, "body"),
            Self::Mail(MailInput::Read { limit, .. }, _) => page(*limit),
            Self::Events(EventsInput::Read { limit, .. }, _) => page(*limit),
            Self::Events(EventsInput::Note { note, .. }, _) => body(note, "note"),
            Self::Claim(ClaimInput::List { limit, .. }, _) => page(*limit),
            Self::Claim(ClaimInput::Acquire { lease_seconds, .. }, _)
            | Self::Claim(ClaimInput::Renew { lease_seconds, .. }, _) => lease(*lease_seconds),
            _ => Ok(()),
        }
    }

    fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::Mail(MailInput::Send { .. } | MailInput::MarkRead { .. }, _)
                | Self::Events(EventsInput::Note { .. }, _)
                | Self::Claim(
                    ClaimInput::Acquire { .. }
                        | ClaimInput::Renew { .. }
                        | ClaimInput::Release { .. },
                    _
                )
        )
    }

    fn lane_mutation(&self) -> Option<(Mutation, Option<OperationId>)> {
        if !self.is_mutation() {
            return None;
        }
        match self {
            Self::Mail(input, id) => Some((Mutation::Mail(input.clone()), id.clone())),
            Self::Events(input, id) => Some((Mutation::Events(input.clone()), id.clone())),
            Self::Claim(input, id) => Some((Mutation::Claim(input.clone()), id.clone())),
        }
    }

    pub(crate) fn lane_request(&self) -> Result<super::host::Request, ServiceError> {
        let (mut arguments, id) = match self {
            Self::Mail(input, id) => (serde_json::to_value(input)?, id),
            Self::Events(input, id) => (serde_json::to_value(input)?, id),
            Self::Claim(input, id) => (serde_json::to_value(input)?, id),
        };
        if let Some(id) = id {
            arguments["operation_id"] = serde_json::to_value(id)?;
        }
        Ok(super::host::Request {
            tool: self.name().into(),
            arguments,
        })
    }

    /// Relay coordination to the guardian. Identity remains host-bound.
    pub fn relay(
        &self,
        path: &std::path::Path,
        cancel: &CancelToken,
    ) -> Result<Value, ServiceError> {
        let seconds = match self {
            Self::Events(
                EventsInput::Read {
                    wait_seconds: Some(seconds),
                    ..
                },
                _,
            ) => *seconds,
            _ => MAX_WAIT_SECONDS,
        };
        self.relay_within(path, cancel, std::time::Duration::from_secs(seconds))
    }

    /// Preserve a CLI caller's remaining budget through admission and execution.
    pub fn relay_within(
        &self,
        path: &std::path::Path,
        cancel: &CancelToken,
        within: std::time::Duration,
    ) -> Result<Value, ServiceError> {
        super::host::mutate(path, &self.lane_request()?, cancel, within)
    }

    /// Host-only entry. The caller binds the service to its sealed attempt;
    /// parsing remains strict.
    pub fn execute_lane_coordination(
        self,
        coordination: &CoordinationService<'_>,
        limits: &Limits,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Value, ServiceError> {
        self.validate_limits(limits)
            .map_err(ServiceError::Invalid)?;
        if let Some((input, id)) = self.lane_mutation() {
            return Ok(serde_json::to_value(CoordinationApi::mutate(
                coordination,
                input,
                id,
                budget,
                cancel,
            )?)?);
        }
        match self {
            Self::Mail(input, _) => Ok(serde_json::to_value(
                coordination.mail(input, budget, cancel)?,
            )?),
            Self::Events(input, _) => Ok(serde_json::to_value(
                coordination.events(input, budget, cancel)?,
            )?),
            Self::Claim(input, _) => Ok(serde_json::to_value(
                coordination.claim(input, budget, cancel)?,
            )?),
        }
    }

    /// Relay frames stay strict even when shared peer records retain fields
    /// from newer writers: the typed inputs deny unknown fields.
    pub fn parse(name: &str, arguments: Value) -> Result<Self, InvalidInput> {
        let mut arguments = arguments;
        if !matches!(name, MAIL | EVENTS | CLAIM) {
            return Err(InvalidInput::UnknownTool);
        }
        // Sender identity is sealed by the host and is never a lane argument.
        if arguments.get("sender").is_some() {
            return Err(InvalidInput::Arguments {
                path: "arguments.sender".to_owned(),
            });
        }
        // Optional on the wire; the runtime input always carries the list.
        if name == MAIL
            && arguments["op"] == "send"
            && let Some(object) = arguments.as_object_mut()
        {
            object
                .entry("artifacts")
                .or_insert_with(|| Value::Array(Vec::new()));
        }
        let operation_id = match arguments
            .as_object_mut()
            .and_then(|object| object.remove("operation_id"))
        {
            Some(Value::String(value)) => {
                Some(
                    OperationId::parse(&value).map_err(|_| InvalidInput::Arguments {
                        path: "arguments.operation_id".to_owned(),
                    })?,
                )
            }
            Some(_) => {
                return Err(InvalidInput::Arguments {
                    path: "arguments.operation_id".to_owned(),
                });
            }
            None => None,
        };
        macro_rules! parse {
            ($ty:ty, $variant:ident) => {
                <$ty>::decode_for_transport(arguments.clone(), WaitTransport::Relay)
                    .map_err(|_| InvalidInput::Arguments {
                        path: "arguments".to_owned(),
                    })
                    .and_then(|input| {
                        let input = input.map_err(InvalidInput::Validated)?;
                        Ok(Self::$variant(input, operation_id.clone()))
                    })
            };
        }
        let call = match name {
            MAIL => parse!(MailInput, Mail),
            EVENTS => parse!(EventsInput, Events),
            _ => parse!(ClaimInput, Claim),
        }?;
        if operation_id.is_some() && !call.is_mutation() {
            return Err(InvalidInput::Arguments {
                path: "arguments.operation_id".to_owned(),
            });
        }
        Ok(call)
    }
}

/// Typed errors crossing the relay edge. Library details remain available as
/// sources while the wire exposes a stable domain code.
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
    #[error("host Fabric endpoint unavailable; check the host guardian and retry")]
    FabricOwnerUnavailable,
    #[error("relay response could not be encoded")]
    Encode(#[from] serde_json::Error),
    #[error("relay result exceeds the configured output limit")]
    ResultTooLarge,
    #[error("relay wait budget could not be created")]
    Time(#[from] provenant_core::TimeError),
}

impl ServiceError {
    pub fn code(&self) -> &str {
        match self {
            Self::Mutation { source, .. } => source.code(),
            Self::Inbox(error) => error.code(),
            Self::InboxOwner { code, .. } | Self::Cli { code, .. }
                if !code.is_empty()
                    && code.len() <= 128
                    && code
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')) =>
            {
                code
            }
            Self::InboxOwner { .. } | Self::Cli { .. } => "CLI_FAILED",
            Self::Invalid(_) => "invalid_input",
            Self::Coordination(error) => error.code(),
            Self::Facade(error) => error.code(),
            Self::FabricOwnerUnavailable => "owner_unavailable",
            Self::Encode(_) => "state_corrupt",
            Self::ResultTooLarge => "result_too_large",
            Self::Time(_) => "invalid_input",
        }
    }

    /// Project relay-owned failures at their typed owner. Delegated failures
    /// retain their owner's projection; raw source messages never become facts.
    pub fn diagnostic(&self) -> provenant_records::diagnostic::Diagnostic {
        use provenant_records::diagnostic::Diagnostic;
        match self {
            Self::Mutation { source, .. } => source.diagnostic(),
            Self::Invalid(error) => error.diagnostic(),
            Self::Coordination(error) => error.diagnostic(),
            Self::Facade(error) => error.diagnostic(),
            Self::Time(_) => {
                Diagnostic::new(self.code()).next("Check the argument path, then retry.")
            }
            Self::ResultTooLarge => {
                Diagnostic::new(self.code()).next("Request a smaller page or output chunk.")
            }
            _ => Diagnostic::new(self.code())
                .next("Inspect the current state and retry when the cause is resolved."),
        }
    }

    /// Stable structured error data. Uncertain mutations carry their
    /// idempotency key; prompts, tokens and raw source errors are omitted.
    pub fn structured_error(&self) -> Value {
        if let Self::Mutation {
            operation_id,
            source,
        } = self
        {
            let mut value = source.structured_error();
            value["operation_id"] = serde_json::json!(operation_id);
            return value;
        }
        let projection = self.diagnostic();
        let mut value = serde_json::to_value(&projection)
            .unwrap_or_else(|_| serde_json::json!({"code": self.code()}));
        value["message"] =
            serde_json::json!(projection.detail.as_deref().unwrap_or(&projection.code));
        value["hint"] = serde_json::json!(projection.next);
        // Preserve transport metadata without deciding causes or remedies here.
        match self {
            Self::Invalid(error) | Self::Coordination(CoordinationError::Input(error)) => {
                value["path"] = serde_json::json!(error.safe_path());
            }
            Self::Facade(FacadeError::LaunchUnknown { operation_id })
            | Self::Coordination(CoordinationError::Peer(crate::hosts::PeerError::Unknown {
                operation_id,
            })) => value["operation_id"] = serde_json::json!(operation_id),
            Self::Facade(FacadeError::GenerationConflict { current_generation }) => {
                value["current_generation"] = serde_json::json!(current_generation);
            }
            Self::Coordination(CoordinationError::Store(crate::StoreError::Invalid(field))) => {
                value["field"] = serde_json::json!(provenant_records::diagnostic::safe_text(field))
            }
            Self::Inbox(InboxError::Invalid(field)) => {
                value["field"] = serde_json::json!(field);
            }
            Self::Cli { data, .. } => {
                for key in ["revision", "current_revision"] {
                    if let Some(revision) = data.get(key).and_then(Value::as_u64) {
                        value[key] = revision.into();
                    }
                }
                if self.code() == provenant_records::control::CONTROL_UNSUPPORTED
                    && let Ok(receipt) = serde_json::from_value::<
                        provenant_records::control::ControlReceipt,
                    >(data.clone())
                    && receipt.state == provenant_records::control::ControlState::Unsupported
                {
                    value["state"] = serde_json::json!(receipt.state);
                    value["operation_id"] = serde_json::json!(receipt.operation_id);
                }
            }
            _ => {}
        }
        let facade = match self {
            Self::Facade(error) => Some(error.clone()),
            Self::Coordination(error) => Some(FacadeError::from(error)),
            _ => None,
        };
        if let Some(FacadeError::Io { operation, .. }) = facade
            && operation == "access Fabric store"
        {
            value["operation"] = Value::String(operation);
        }
        if self.code() == "busy" {
            value["retryable"] = Value::Bool(true);
        }
        value
    }
}
