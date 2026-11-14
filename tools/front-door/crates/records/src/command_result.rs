//! Version-one command transport facts, shared by CLI edges.

use crate::wire::ENVELOPE_VERSION;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Failed,
    Refused,
    Invalid,
    Error,
    Unavailable,
    Timeout,
    Cancelled,
    /// The request was accepted but its effect has not happened yet, such as
    /// a merge queued by the repository.
    Pending,
    /// A wait ended before the work settled. The work continues, so this is
    /// not a failure: the caller polls or waits again.
    Waiting,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandError {
    pub code: String,
    pub message: String,
    pub cause: Option<String>,
    pub repair: Option<String>,
    #[serde(flatten)]
    pub diagnostic: Option<crate::diagnostic::DiagnosticFields>,
}
impl Outcome {
    /// Every status, in declaration order; the published schemas enumerate it.
    pub const ALL: [Outcome; 10] = [
        Self::Ok,
        Self::Failed,
        Self::Refused,
        Self::Invalid,
        Self::Error,
        Self::Unavailable,
        Self::Timeout,
        Self::Cancelled,
        Self::Pending,
        Self::Waiting,
    ];
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResult<T> {
    pub schema_version: u64,
    pub command: String,
    pub status: Outcome,
    pub data: T,
    pub error: Option<CommandError>,
}
impl<T> CommandResult<T> {
    pub fn new(command: impl Into<String>, status: Outcome, data: T) -> Self {
        Self {
            schema_version: ENVELOPE_VERSION,
            command: command.into(),
            status,
            data,
            error: None,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct HelpCatalogue {
    pub commands: Vec<HelpCommand>,
    /// Stable `error.code` values and their meanings. Only the whole
    /// catalogue lists them; a scoped `help COMMAND` leaves this empty.
    pub error_codes: Vec<HelpErrorCode>,
    /// Provider control capabilities and the actions each provider takes.
    /// Like the error codes, only the whole catalogue lists them.
    pub provider_controls: Vec<crate::control::ControlCapability>,
}
#[derive(Debug, Serialize)]
pub struct HelpErrorCode {
    pub code: String,
    pub summary: String,
}
#[derive(Debug, Serialize)]
pub struct HelpCommand {
    pub command: String,
    pub description: Option<String>,
    /// Direct public subcommands, including commands omitted from compact help.
    pub subcommands: Vec<String>,
    /// Configuration domains consumed by this command.
    pub config_domains: Vec<String>,
    pub arguments: Vec<HelpArgument>,
    pub groups: Vec<HelpGroup>,
    pub constraints: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct HelpGroup {
    pub name: String,
    pub arguments: Vec<String>,
    pub required: bool,
    pub multiple: bool,
}
#[derive(Debug, Serialize)]
pub struct HelpArgument {
    pub name: String,
    pub description: Option<String>,
    pub long: Option<String>,
    pub positional: bool,
    pub required: bool,
    pub repeatable: bool,
    pub defaults: Vec<String>,
    pub conditional_defaults: Vec<(String, String)>,
    pub values: Vec<String>,
    pub conflicts: Vec<String>,
    pub requires: Vec<String>,
    /// Arguments whose presence makes this argument optional.
    pub required_unless: Vec<String>,
    pub bounds: Option<(u64, u64)>,
}
