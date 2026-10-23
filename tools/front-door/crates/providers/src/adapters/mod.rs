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

/// Builder input, flattened from Python's dispatch plan.
///
/// All fields retain their Python top-level names except `add_dirs`, which
/// maps from `p["applied"]["add_dirs"]`, and `endpoint_base_url`, which maps
/// from `p["route"].get("endpoint_base_url")`. Missing and null endpoints are
/// both false, while present JSON values use Python truthiness. The API-key
/// environment value is injected into `claude` separately and is never stored
/// or serialised in this record. Other provider configuration/environment facts
/// (such as `agy_sandbox`) are supplied by the caller.
///
/// This is an inert builder API, not the persisted dispatch-plan schema.
/// Numeric coercions occur at the adapter seam, retaining Python JSON types.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Plan {
    pub mode: Mode,
    pub prompt: String,
    pub boundary_prompt: String,
    pub timeout_seconds: serde_json::Value,
    pub agy_sandbox: bool,
    pub resume_session: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub session_id: String,
    pub cwd: String,
    pub add_dirs: Vec<String>,
    pub endpoint_base_url: Option<serde_json::Value>,
    pub context_ceiling: Option<crate::ContextCeiling>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<codex::BuildInputs>,
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
    pub exit_after_output: bool,
}

/// Data that describes the stable transport and identity capabilities of an adapter.
/// Provider-specific behavior stays behind [`Adapter`]; adding a provider adds one
/// row and a small implementation instead of another dispatcher branch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capabilities {
    pub name: &'static str,
    pub cli: &'static str,
    pub prompt_transport: PromptTransport,
    pub stdin: StdinMode,
    pub output_format: &'static str,
    pub effort_flag: Option<&'static str>,
    pub session_keys: &'static [&'static str],
    pub model_source: Option<&'static str>,
    pub exit_after_output: bool,
    pub empty_failure: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("unknown adapter: {0}")]
    UnknownAdapter(String),
    #[error(transparent)]
    Integer(#[from] crate::IntegerError),
    #[error("Codex requires a configuration decision, permissions and host facts")]
    CodexContextRequired,
    #[error(transparent)]
    Codex(#[from] codex::AdapterError),
}

type Constructor = fn(&Plan, Option<&str>) -> Result<AdapterProfile, BuildError>;

/// One row owns provider identity, all capabilities and its constructor.
pub struct Registration {
    pub capabilities: Capabilities,
    constructor: Constructor,
    continues_turn: fn(&serde_json::Value) -> Result<bool, opencode::OutputError>,
    diagnostics: fn(&str) -> (String, String),
}

pub static REGISTRY: &[Registration] = &[
    Registration {
        capabilities: Capabilities {
            name: "claude",
            cli: "claude",
            prompt_transport: PromptTransport::Stdin,
            stdin: StdinMode::Prompt,
            output_format: "stream-json",
            effort_flag: Some("--effort"),
            session_keys: &["session_id"],
            model_source: Some("claude:init.model"),
            exit_after_output: false,
            empty_failure: "",
        },
        constructor: claude::claude,
        continues_turn: default_continues,
        diagnostics: default_diagnostics,
    },
    Registration {
        capabilities: Capabilities {
            name: "codex",
            cli: "codex",
            prompt_transport: PromptTransport::Stdin,
            stdin: StdinMode::Prompt,
            output_format: "jsonl",
            effort_flag: Some("model_reasoning_effort"),
            session_keys: &["thread_id"],
            model_source: Some("codex:rollout.turn_context.model"),
            exit_after_output: false,
            empty_failure: "",
        },
        constructor: |plan, _| {
            codex::codex(
                plan,
                plan.codex
                    .as_ref()
                    .ok_or(BuildError::CodexContextRequired)?,
            )
        },
        continues_turn: default_continues,
        diagnostics: default_diagnostics,
    },
    Registration {
        capabilities: Capabilities {
            name: "opencode",
            cli: "opencode",
            prompt_transport: PromptTransport::Argv,
            stdin: StdinMode::Null,
            output_format: "jsonl",
            effort_flag: Some("#variant"),
            session_keys: &["sessionID", "session_id"],
            model_source: Some("opencode:session-export.info.model"),
            exit_after_output: opencode::EXIT_AFTER_OUTPUT,
            empty_failure: opencode::EMPTY_FAILURE,
        },
        constructor: |plan, _| opencode::opencode(plan),
        continues_turn: opencode::continues_turn,
        diagnostics: opencode::diagnostics,
    },
    Registration {
        capabilities: Capabilities {
            name: "cursor",
            cli: "cursor-agent",
            prompt_transport: PromptTransport::Argv,
            stdin: StdinMode::Pipe,
            output_format: "stream-json",
            effort_flag: None,
            session_keys: &["session_id", "sessionId"],
            model_source: Some("cursor:init.model"),
            exit_after_output: false,
            empty_failure: "",
        },
        constructor: |plan, _| cursor::cursor(plan),
        continues_turn: default_continues,
        diagnostics: default_diagnostics,
    },
    Registration {
        capabilities: Capabilities {
            name: "agy",
            cli: "agy",
            prompt_transport: PromptTransport::Argv,
            stdin: StdinMode::Null,
            output_format: "stream-json",
            effort_flag: Some("--effort"),
            session_keys: &["session_id", "conversation_id", "conversationId"],
            model_source: Some("agy:init.model|log"),
            exit_after_output: false,
            empty_failure: "",
        },
        constructor: |plan, _| agy::agy(plan),
        continues_turn: default_continues,
        diagnostics: default_diagnostics,
    },
    Registration {
        capabilities: Capabilities {
            name: "kiro",
            cli: "kiro-cli",
            prompt_transport: PromptTransport::Argv,
            stdin: StdinMode::Null,
            output_format: "stream-json",
            effort_flag: Some("--effort"),
            session_keys: &[
                "sessionId",
                "session_id",
                "conversation_id",
                "conversationId",
            ],
            model_source: Some("kiro:stream"),
            exit_after_output: false,
            empty_failure: "",
        },
        constructor: |plan, _| kiro::kiro(plan),
        continues_turn: default_continues,
        diagnostics: default_diagnostics,
    },
    Registration {
        capabilities: Capabilities {
            name: "copilot",
            cli: "copilot",
            prompt_transport: PromptTransport::Argv,
            stdin: StdinMode::Null,
            output_format: "json",
            effort_flag: Some("--effort"),
            session_keys: &["session_id"],
            model_source: None,
            exit_after_output: false,
            empty_failure: "",
        },
        constructor: |plan, _| copilot::copilot(plan),
        continues_turn: default_continues,
        diagnostics: default_diagnostics,
    },
];

pub fn capabilities(name: &str) -> Option<&'static Capabilities> {
    REGISTRY
        .iter()
        .find(|r| r.capabilities.name == name)
        .map(|r| &r.capabilities)
}

pub trait Adapter: Sync {
    fn capabilities(&self) -> &Capabilities;
    fn build(&self, plan: &Plan, api_key: Option<&str>) -> Result<AdapterProfile, BuildError>;
    fn continues_turn(&self, event: &serde_json::Value) -> Result<bool, opencode::OutputError> {
        let _ = event;
        Ok(false)
    }
    fn diagnostics(&self, stderr: &str) -> (String, String) {
        (stderr.to_owned(), String::new())
    }
}
impl Adapter for Registration {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    fn build(&self, plan: &Plan, key: Option<&str>) -> Result<AdapterProfile, BuildError> {
        (self.constructor)(plan, key)
    }
    fn continues_turn(&self, event: &serde_json::Value) -> Result<bool, opencode::OutputError> {
        (self.continues_turn)(event)
    }
    fn diagnostics(&self, stderr: &str) -> (String, String) {
        (self.diagnostics)(stderr)
    }
}

pub(super) fn profile(name: &str, argv: Vec<String>) -> Result<AdapterProfile, BuildError> {
    let c = capabilities(name).ok_or_else(|| BuildError::UnknownAdapter(name.into()))?;
    let mut command = vec![c.cli.into()];
    command.extend(argv);
    Ok(AdapterProfile {
        cli: c.cli.into(),
        argv: command,
        prompt_transport: c.prompt_transport,
        stdin: c.stdin,
        output_format: c.output_format.into(),
        effort_flag: c.effort_flag.map(str::to_owned),
        session_keys: c.session_keys.iter().map(|s| (*s).into()).collect(),
        model_source: c.model_source.map(str::to_owned),
        exit_after_output: c.exit_after_output,
    })
}

fn default_continues(_: &serde_json::Value) -> Result<bool, opencode::OutputError> {
    Ok(false)
}
fn default_diagnostics(s: &str) -> (String, String) {
    (s.into(), String::new())
}

pub mod agy;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod kiro;
pub mod opencode;

pub fn adapter(name: &str) -> Option<&'static dyn Adapter> {
    REGISTRY
        .iter()
        .find(|r| r.capabilities.name == name)
        .map(|r| r as &dyn Adapter)
}
pub fn build(name: &str, plan: &Plan, key: Option<&str>) -> Result<AdapterProfile, BuildError> {
    adapter(name)
        .ok_or_else(|| BuildError::UnknownAdapter(name.into()))?
        .build(plan, key)
}

#[cfg(test)]
mod registry_tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn registry_and_capabilities_are_data_driven_and_consistent() {
        assert_eq!(
            REGISTRY
                .iter()
                .map(|r| r.capabilities.name)
                .collect::<Vec<_>>(),
            ["claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot"]
        );
        let plan = plan(Mode::ReadOnly);
        for row in REGISTRY {
            let capabilities = &row.capabilities;
            if capabilities.name == "codex" {
                continue;
            }
            let provider = adapter(capabilities.name).expect("capability has adapter");
            let profile = provider.build(&plan, None).unwrap();
            assert_eq!(profile.cli, capabilities.cli);
            assert_eq!(profile.prompt_transport, capabilities.prompt_transport);
            assert_eq!(profile.stdin, capabilities.stdin);
            assert_eq!(profile.output_format, capabilities.output_format);
            assert_eq!(profile.effort_flag.as_deref(), capabilities.effort_flag);
            assert_eq!(profile.session_keys, capabilities.session_keys);
            assert_eq!(profile.model_source.as_deref(), capabilities.model_source);
        }
        assert!(matches!(
            build("unknown", &plan, None),
            Err(BuildError::UnknownAdapter(_))
        ));
    }
}

#[cfg(test)]
mod differential_tests {
    use super::*;

    #[test]
    fn argv_profiles_match_python_on_shared_fixtures() {
        for api_key in [None, Some(""), Some("fixture-key")] {
            let mut fixtures = Vec::new();
            let mut actual = Vec::new();
            for mode in [Mode::ReadOnly, Mode::WorktreeWrite] {
                for empty_options in [false, true] {
                    for endpoint in [
                        None,
                        Some(serde_json::json!("")),
                        Some(serde_json::json!("https://example.invalid")),
                    ] {
                        let plan = Plan {
                            mode,
                            prompt: "Compare the fixture exactly.".into(),
                            boundary_prompt: "Use file-reading tools only.".into(),
                            codex: None,
                            timeout_seconds: serde_json::json!(30),
                            agy_sandbox: true,
                            resume_session: Some(
                                if empty_options { "" } else { "session-fixture" }.into(),
                            ),
                            model: Some(if empty_options { "" } else { "model-fixture" }.into()),
                            effort: Some(if empty_options { "" } else { "high" }.into()),
                            session_id: "session-id-fixture".into(),
                            cwd: "/workspace/fixture".into(),
                            add_dirs: vec!["/shared/fixture".into()],
                            endpoint_base_url: endpoint,
                            context_ceiling: Some(
                                serde_json::json!(if empty_options { 0 } else { 12_000 }).into(),
                            ),
                        };
                        let mut input = serde_json::to_value(&plan).unwrap();
                        input["applied"] = serde_json::json!({"add_dirs": plan.add_dirs});
                        input["route"] = match &plan.endpoint_base_url {
                            Some(endpoint) => serde_json::json!({"endpoint_base_url": endpoint}),
                            None => serde_json::json!({}),
                        };
                        fixtures.push(input);
                        actual.push([
                            agy::agy(&plan).unwrap().argv,
                            claude::claude(&plan, api_key).unwrap().argv,
                            copilot::copilot(&plan).unwrap().argv,
                            cursor::cursor(&plan).unwrap().argv,
                            kiro::kiro(&plan).unwrap().argv,
                        ]);
                    }
                }
            }
            let Some(expected): Option<Vec<[Vec<String>; 5]>> = crate::oracle::python(
                "import importlib,json,sys; modules=[importlib.import_module('skills.orchestrate.scripts.adapters.'+name) for name in ['agy','claude','copilot','cursor','kiro']]; print(json.dumps([[m.argv(p) for m in modules] for p in json.load(sys.stdin)]))",
                &serde_json::json!(fixtures), api_key,
            ) else { return; };
            assert_eq!(actual, expected);
        }
    }
}
