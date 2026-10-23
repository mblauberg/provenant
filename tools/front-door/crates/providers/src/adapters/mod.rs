//! Provider CLI profiles. Builders are pure: callers supply the route, plan,
//! environment facts, and clock-derived values they have already selected.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    ReadOnly,
    WorktreeWrite,
}

impl Mode {
    pub(super) fn is_writer(self) -> bool {
        self == Self::WorktreeWrite
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Plan {
    pub mode: Mode,
    pub prompt: String,
    pub boundary_prompt: String,
    pub timeout_seconds: u64,
    pub agy_sandbox: bool,
    pub resume_session: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub session_id: String,
    pub cwd: String,
    pub add_dirs: Vec<String>,
    pub claude_api_key: bool,
    pub context_ceiling: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTransport {
    Argv,
    Stdin,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StdinMode {
    Null,
    Prompt,
    Pipe,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdapterProfile {
    pub cli: String,
    pub argv: Vec<String>,
    pub prompt_transport: PromptTransport,
    pub stdin: StdinMode,
    pub output_format: String,
    pub effort_flag: Option<String>,
    pub session_keys: Vec<String>,
    pub model_source: Option<String>,
}

pub(super) struct Metadata<'a> {
    pub transport: PromptTransport,
    pub stdin: StdinMode,
    pub output: &'a str,
    pub effort_flag: Option<&'a str>,
    pub session_keys: &'a [&'a str],
    pub model_source: Option<&'a str>,
}

pub(super) fn profile(cli: &str, argv: Vec<String>, metadata: Metadata<'_>) -> AdapterProfile {
    AdapterProfile {
        cli: cli.into(),
        argv,
        prompt_transport: metadata.transport,
        stdin: metadata.stdin,
        output_format: metadata.output.into(),
        effort_flag: metadata.effort_flag.map(str::to_owned),
        session_keys: metadata
            .session_keys
            .iter()
            .map(|key| (*key).into())
            .collect(),
        model_source: metadata.model_source.map(str::to_owned),
    }
}

pub mod agy;
pub mod claude;
pub mod copilot;
