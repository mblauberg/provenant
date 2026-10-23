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

pub(super) struct Metadata<'a> {
    pub transport: PromptTransport,
    pub stdin: StdinMode,
    pub output: &'a str,
    pub effort_flag: Option<&'a str>,
    pub session_keys: &'a [&'a str],
    pub model_source: Option<&'a str>,
}

pub(super) fn profile(cli: &str, argv: Vec<String>, metadata: Metadata<'_>) -> AdapterProfile {
    let mut command = vec![cli.into()];
    command.extend(argv);
    AdapterProfile {
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
    }
}

pub mod agy;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod kiro;

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
