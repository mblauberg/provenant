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
    pub endpoint_base_url: Option<String>,
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
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    #[test]
    fn argv_profiles_match_python_on_shared_fixtures() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../");
        let probe = Command::new("python3").arg("--version").output();
        if matches!(probe, Err(ref error) if error.kind() == std::io::ErrorKind::NotFound) {
            eprintln!("skipping Python differential: python3 is unavailable");
            return;
        }
        assert!(probe.unwrap().status.success(), "python3 --version failed");

        for (mode, empty_options) in [
            (Mode::ReadOnly, false),
            (Mode::WorktreeWrite, false),
            (Mode::ReadOnly, true),
            (Mode::WorktreeWrite, true),
        ] {
            let plan = Plan {
                mode,
                prompt: "Compare the fixture exactly.".into(),
                boundary_prompt: "Use file-reading tools only.".into(),
                timeout_seconds: 30,
                agy_sandbox: true,
                resume_session: Some(if empty_options { "" } else { "session-fixture" }.into()),
                model: Some(if empty_options { "" } else { "model-fixture" }.into()),
                effort: Some(if empty_options { "" } else { "high" }.into()),
                session_id: "session-id-fixture".into(),
                cwd: "/workspace/fixture".into(),
                add_dirs: vec!["/shared/fixture".into()],
                claude_api_key: false,
                endpoint_base_url: Some("https://example.invalid".into()),
                context_ceiling: Some(if empty_options { 0 } else { 12_000 }),
            };
            let rust_profiles = [
                ("agy", agy::agy(&plan)),
                ("claude", claude::claude(&plan)),
                ("copilot", copilot::copilot(&plan)),
                ("cursor", cursor::cursor(&plan)),
                ("kiro", kiro::kiro(&plan)),
            ];
            let mut input = serde_json::to_value(&plan).unwrap();
            input["applied"] = serde_json::json!({"add_dirs": plan.add_dirs});
            input["route"] = serde_json::json!({"endpoint_base_url": plan.endpoint_base_url});
            let encoded = serde_json::to_vec(&input).unwrap();
            for (name, profile) in rust_profiles {
                let mut child = match Command::new("python3")
                    .args(["-c", "import importlib,json,sys; p=json.load(sys.stdin); print(json.dumps(importlib.import_module(sys.argv[1]).argv(p), ensure_ascii=False))", &format!("skills.orchestrate.scripts.adapters.{name}")])
                    .current_dir(&root)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                {
                    Ok(child) => child,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        eprintln!("skipping Python differential: python3 disappeared");
                        return;
                    }
                    Err(error) => panic!("could not start Python adapter oracle: {error}"),
                };
                child.stdin.take().unwrap().write_all(&encoded).unwrap();
                let output = child.wait_with_output().unwrap();
                assert!(
                    output.status.success(),
                    "{name} Python oracle failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let expected: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(profile.argv, expected, "{name} argv differs for {mode:?}");
            }
        }
    }
}
