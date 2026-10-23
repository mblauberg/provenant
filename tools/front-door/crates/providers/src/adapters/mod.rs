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
/// Unsigned timeout/context inputs intentionally remain a partial port; see
/// RP-1303 in the divergence log before wiring a dynamic plan decoder.
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
    pub endpoint_base_url: Option<serde_json::Value>,
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
}

pub const CAPABILITIES: &[Capabilities] = &[
    Capabilities {
        name: "agy",
        cli: "agy",
        prompt_transport: PromptTransport::Argv,
        stdin: StdinMode::Null,
        output_format: "stream-json",
        effort_flag: Some("--effort"),
        session_keys: &["session_id", "conversation_id", "conversationId"],
        model_source: Some("agy:init.model|log"),
    },
    Capabilities {
        name: "claude",
        cli: "claude",
        prompt_transport: PromptTransport::Stdin,
        stdin: StdinMode::Prompt,
        output_format: "stream-json",
        effort_flag: Some("--effort"),
        session_keys: &["session_id"],
        model_source: Some("claude:init.model"),
    },
    Capabilities {
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
    },
    Capabilities {
        name: "opencode",
        cli: "opencode",
        prompt_transport: PromptTransport::Argv,
        stdin: StdinMode::Null,
        output_format: "jsonl",
        effort_flag: Some("#variant"),
        session_keys: &["sessionID", "session_id"],
        model_source: Some("opencode:session-export.info.model"),
    },
    Capabilities {
        name: "copilot",
        cli: "copilot",
        prompt_transport: PromptTransport::Argv,
        stdin: StdinMode::Null,
        output_format: "json",
        effort_flag: Some("--effort"),
        session_keys: &["session_id"],
        model_source: None,
    },
    Capabilities {
        name: "cursor",
        cli: "cursor-agent",
        prompt_transport: PromptTransport::Argv,
        stdin: StdinMode::Pipe,
        output_format: "stream-json",
        effort_flag: None,
        session_keys: &["session_id", "sessionId"],
        model_source: Some("cursor:init.model"),
    },
];

pub fn capabilities(name: &str) -> Option<&'static Capabilities> {
    CAPABILITIES.iter().find(|entry| entry.name == name)
}

/// Common adapter seam consumed by planners that select a provider by name.
pub trait Adapter: Sync {
    fn name(&self) -> &'static str;
    fn build(&self, plan: &Plan, api_key: Option<&str>) -> AdapterProfile;
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
    let capabilities = capabilities_by_cli(cli);
    let mut command = vec![cli.into()];
    command.extend(argv);
    let fallback = AdapterProfile {
        cli: cli.into(),
        argv: command,
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
    };
    let Some(capabilities) = capabilities else {
        return fallback;
    };
    AdapterProfile {
        cli: capabilities.cli.to_owned(),
        argv: fallback.argv,
        prompt_transport: capabilities.prompt_transport,
        stdin: capabilities.stdin,
        output_format: capabilities.output_format.to_owned(),
        effort_flag: capabilities.effort_flag.map(str::to_owned),
        session_keys: capabilities
            .session_keys
            .iter()
            .map(|key| (*key).to_owned())
            .collect(),
        model_source: capabilities.model_source.map(str::to_owned),
    }
}

fn capabilities_by_cli(cli: &str) -> Option<&'static Capabilities> {
    CAPABILITIES.iter().find(|entry| entry.cli == cli)
}

pub mod agy;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod kiro;
pub mod opencode;

struct Agy;
struct Claude;
struct Copilot;
struct Cursor;
struct Kiro;
struct OpenCode;

impl Adapter for Agy {
    fn name(&self) -> &'static str {
        "agy"
    }
    fn build(&self, plan: &Plan, _: Option<&str>) -> AdapterProfile {
        agy::agy(plan)
    }
}
impl Adapter for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }
    fn build(&self, plan: &Plan, api_key: Option<&str>) -> AdapterProfile {
        claude::claude(plan, api_key)
    }
}
impl Adapter for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }
    fn build(&self, plan: &Plan, _: Option<&str>) -> AdapterProfile {
        copilot::copilot(plan)
    }
}
impl Adapter for Cursor {
    fn name(&self) -> &'static str {
        "cursor"
    }
    fn build(&self, plan: &Plan, _: Option<&str>) -> AdapterProfile {
        cursor::cursor(plan)
    }
}
impl Adapter for Kiro {
    fn name(&self) -> &'static str {
        "kiro"
    }
    fn build(&self, plan: &Plan, _: Option<&str>) -> AdapterProfile {
        kiro::kiro(plan)
    }
}
impl Adapter for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }
    fn build(&self, plan: &Plan, _: Option<&str>) -> AdapterProfile {
        opencode::opencode(plan)
    }
}

static AGY_ADAPTER: Agy = Agy;
static CLAUDE_ADAPTER: Claude = Claude;
static COPILOT_ADAPTER: Copilot = Copilot;
static CURSOR_ADAPTER: Cursor = Cursor;
static KIRO_ADAPTER: Kiro = Kiro;
static OPENCODE_ADAPTER: OpenCode = OpenCode;

pub static ADAPTERS: &[&dyn Adapter] = &[
    &AGY_ADAPTER,
    &CLAUDE_ADAPTER,
    &COPILOT_ADAPTER,
    &CURSOR_ADAPTER,
    &KIRO_ADAPTER,
    &OPENCODE_ADAPTER,
];

pub fn adapter(name: &str) -> Option<&'static dyn Adapter> {
    ADAPTERS.iter().copied().find(|entry| entry.name() == name)
}

pub fn build(name: &str, plan: &Plan, api_key: Option<&str>) -> Option<AdapterProfile> {
    adapter(name).map(|provider| provider.build(plan, api_key))
}

#[cfg(test)]
mod registry_tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn registry_and_capabilities_are_data_driven_and_consistent() {
        let plan = plan(Mode::ReadOnly);
        assert_eq!(ADAPTERS.len(), CAPABILITIES.len());
        for capabilities in CAPABILITIES {
            let provider = adapter(capabilities.name).expect("capability has adapter");
            let profile = provider.build(&plan, None);
            assert_eq!(profile.cli, capabilities.cli);
            assert_eq!(profile.prompt_transport, capabilities.prompt_transport);
            assert_eq!(profile.stdin, capabilities.stdin);
            assert_eq!(profile.output_format, capabilities.output_format);
            assert_eq!(profile.effort_flag.as_deref(), capabilities.effort_flag);
            assert_eq!(profile.session_keys, capabilities.session_keys);
            assert_eq!(profile.model_source.as_deref(), capabilities.model_source);
        }
        assert!(build("unknown", &plan, None).is_none());
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
                            timeout_seconds: 30,
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
                            context_ceiling: Some(if empty_options { 0 } else { 12_000 }),
                        };
                        let mut input = serde_json::to_value(&plan).unwrap();
                        input["applied"] = serde_json::json!({"add_dirs": plan.add_dirs});
                        input["route"] = match &plan.endpoint_base_url {
                            Some(endpoint) => serde_json::json!({"endpoint_base_url": endpoint}),
                            None => serde_json::json!({}),
                        };
                        fixtures.push(input);
                        actual.push([
                            agy::agy(&plan).argv,
                            claude::claude(&plan, api_key).argv,
                            copilot::copilot(&plan).argv,
                            cursor::cursor(&plan).argv,
                            kiro::kiro(&plan).argv,
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
