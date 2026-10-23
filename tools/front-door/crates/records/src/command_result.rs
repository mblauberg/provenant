//! Version-one command transport facts, shared by CLI and MCP edges.
use serde::Serialize;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
}
#[derive(Debug, Clone, Serialize)]
pub struct CommandError {
    pub code: String,
    pub message: String,
    pub cause: Option<String>,
    pub repair: Option<String>,
}
#[derive(Debug, Serialize)]
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
            schema_version: 1,
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
}
#[derive(Debug, Serialize)]
pub struct HelpCommand {
    pub command: String,
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
    pub long: Option<String>,
    pub positional: bool,
    pub required: bool,
    pub repeatable: bool,
    pub defaults: Vec<String>,
    pub conditional_defaults: Vec<(String, String)>,
    pub values: Vec<String>,
    pub conflicts: Vec<String>,
    pub requires: Vec<String>,
    pub bounds: Option<(u64, u64)>,
}
