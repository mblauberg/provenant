use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, package_grant, subtree,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::CodexPolicy;
use provenant_confinement::permissions::{
    AuthenticationProfile, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use serde::Deserialize;
use std::ffi::OsString;

pub struct Codex;

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &Codex,
    quota_source: Some(&Codex),
    stdout_decoder: None,
    stderr_decoder: None,
    display_name: "Codex",
    quota: provenant_quota::QuotaPolicy {
        adapter: "codex",
        timer_seconds: 900,
        panel_seconds: 300,
        probe_seconds: 10,
        low_power: true,
        solo: false,
    },
};

crate::adapter!(Codex, "codex", prepare);
/// The exec JSON stream carries no answering-model position.
const MODEL_ON_WIRE: bool = false;

/// Codex runs with a private `CODEX_HOME` (the attempt's private state) that
/// links only `auth.json` from the selected registered identity (the default
/// login source when no account was selected). Its own seatbelt cannot nest
/// inside ours, so it runs unsandboxed inside the outer profile, which is the
/// real boundary. Its shell tool may run system and install-prefix programs.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    let mut tool_requirements = crate::launch::ToolRequirements::default();
    if input.confined.codex_policy() != CodexPolicy::Clean
        || plan.selected_authentication.as_deref() != Some("login")
    {
        return Err(ProviderError::UnsupportedPolicy);
    }
    let codex_home = &plan
        .private_state
        .as_ref()
        .ok_or(ProviderError::UnsupportedPolicy)?
        .path;
    let credential = input.account.map_or_else(
        || input.home.join(".codex/auth.json"),
        |account| account.directory().join("auth.json"),
    );
    let mut argv: Vec<OsString> = vec![input.executable.into(), "exec".into()];
    if session == Session::Resume {
        let continuation = input.continuation.ok_or(ProviderError::UnknownIdentity)?;
        argv.extend(["resume".into(), continuation.session().into()]);
    }
    argv.extend(
        [
            "--json",
            "--skip-git-repo-check",
            "--ignore-user-config",
            "--ignore-rules",
            "--dangerously-bypass-approvals-and-sandbox",
            "-c",
            "service_tier=\"default\"",
            "-m",
            input.route.model.as_str(),
        ]
        .map(OsString::from),
    );
    if let Some(effort) = &input.route.effort {
        argv.extend([
            "-c".into(),
            format!("model_reasoning_effort=\"{effort}\"").into(),
        ]);
    }
    for image in input.images {
        argv.extend(["--image".into(), image.as_os_str().to_owned()]);
    }
    input.provenant.codex(&mut argv)?;
    argv.push("-".into());

    // Codex runs helpers from its own package (code-mode host, bundled `rg`
    // and `zsh`), and its shell tool runs system programs. System directories
    // are readable through the runtime baseline; exec roots add exec, not
    // reads. No shared install prefix: its var/ stays unreadable.
    let mut package_omissions = Vec::new();
    let package = package_grant(input.executable, &mut package_omissions);
    let tool_exec_roots = vec![
        subtree("/bin"),
        subtree("/usr/bin"),
        subtree("/usr/libexec"),
        package.clone(),
    ];
    let mut runtime_reads = vec![package.clone()];
    let tool_writes = Vec::new();
    let staged_skills = input.stage_user_skills();
    let omitted_discovery = package_omissions;
    let mut setup = vec![SetupStep::Symlink {
        link: codex_home.join("auth.json"),
        target: credential.clone(),
    }];
    if let Some(skills) = &staged_skills.root {
        runtime_reads.push(subtree(skills));
        setup.push(SetupStep::CreateDir(codex_home.join(".agents")));
        setup.push(SetupStep::Symlink {
            link: codex_home.join(".agents/skills"),
            target: skills.clone(),
        });
    }
    let path = crate::launch::tool_search_path(input.home, true);
    let mut env = base_env(input, &plan.scratch.path, path);
    input
        .provenant
        .environment(&plan.scratch.path, &mut env, &mut setup, false)?;
    crate::launch::rust_tools(input, access, &mut tool_requirements, &mut env, &mut setup);
    crate::launch::node_tools(
        input.home,
        access,
        crate::launch::lane_cache(input),
        &mut tool_requirements,
        &mut env,
    );
    // Git may inspect global config even for status. Keep host hooks,
    // includes and credential helpers outside the lane's environment.
    env.extend([
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]);
    if staged_skills.root.is_some() || !input.user_skills() {
        for (key, value) in &mut env {
            if key == "HOME" {
                *value = codex_home.into();
            }
        }
    }
    env.push(("CODEX_HOME".into(), codex_home.into()));
    env.push(("SHELL".into(), "/bin/zsh".into()));
    let mut spec = LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(input.prompt.text().as_bytes().to_vec()),
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup,
        omitted_discovery,
        user_skills: staged_skills,
        tool_requirements,
        git_wrapper: Default::default(),
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![exact(input.executable)],
                allow_process_fork: true,
                runtime_reads,
                authentication: vec![AuthenticationProfile {
                    mechanism: "login".into(),
                    roots: vec![exact(&credential)],
                    // Token refresh rewrites auth.json through the link.
                    in_place_writes: vec![exact(&credential)],
                }],
                tool_exec_roots,
                tool_writes,
                provider_api: vec![NetworkEndpoint {
                    address: "*".into(),
                    port: 443,
                }],
                ..ProviderProfileSpec::default()
            },
            observations: Default::default(),
        },
        stream,
    };
    crate::launch::prepare_tool_requirements(input, &mut spec);
    input
        .provenant
        .grant(&mut spec.profile.spec, &mut spec.setup, false);
    Ok(spec)
}

crate::quota_source!(Codex, "codex");
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "thread.started")]
    Thread { thread_id: String },
    #[serde(rename = "turn.started")]
    Started,
    #[serde(rename = "item.completed")]
    Item { item: Item },
    #[serde(rename = "turn.completed")]
    Completed { usage: Usage },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "turn.failed")]
    Failed { error: Option<serde_json::Value> },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Item {
    #[serde(rename = "agent_message")]
    Answer { text: String },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Usage {
    /// Everything the model read, including cached and newly cached tokens.
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: Option<u64>,
    #[serde(default)]
    cache_write_input_tokens: Option<u64>,
    output_tokens: u64,
}
/// The provider's own refusal of the requested model, in its words.
fn model_refusal(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("model")
        && [
            "not supported",
            "unsupported model",
            "does not exist",
            "not found",
            "unknown model",
            "no access",
        ]
        .iter()
        .any(|phrase| lower.contains(phrase))
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        Event::Thread { thread_id } => vec![E::Session(thread_id)],
        Event::Item {
            item: Item::Answer { text },
        } => vec![E::MessageStart, E::Text(text)],
        Event::Completed { usage } => vec![
            E::Usage {
                // Codex counts cache reads and writes inside `input_tokens`;
                // the recorded categories are disjoint.
                input: usage.input_tokens.saturating_sub(
                    usage
                        .cached_input_tokens
                        .unwrap_or(0)
                        .saturating_add(usage.cache_write_input_tokens.unwrap_or(0)),
                ),
                output: usage.output_tokens,
                cache_read: usage.cached_input_tokens,
                cache_write: usage.cache_write_input_tokens,
                cache_write_1h: None,
            },
            E::Terminal,
        ],
        Event::Error { message } => {
            let lower = message.to_ascii_lowercase();
            vec![if lower.contains("reconnecting") {
                E::TransientError
            } else if model_refusal(&message) {
                E::ModelUnavailable { message }
            } else if lower.contains("too many requests") || lower.contains("status: 429") {
                E::RateLimited { message }
            } else {
                E::Error
            }]
        }
        Event::Failed { error } => {
            let typed = error.as_ref().and_then(|error| {
                let refused = error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .filter(|message| model_refusal(message))
                    .map(|message| E::ModelUnavailable {
                        message: message.to_owned(),
                    });
                let message = || {
                    error
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                };
                if refused.is_some() {
                    return refused;
                }
                match error.get("codex_error_info")?.as_str()? {
                    // Without the capacity message the failure stays generic.
                    "server_overloaded" => message().map(|message| E::ModelOverloaded { message }),
                    // The account's plan allowance is spent; another account
                    // may still serve the lane.
                    "usage_limit_exceeded" => Some(E::QuotaLimited {
                        message: message().unwrap_or_else(|| "provider usage limit reached".into()),
                        pool: None,
                        resets_in_seconds: None,
                    }),
                    _ => None,
                }
            });
            vec![typed.unwrap_or(E::Error)]
        }
        _ => vec![],
    })
}

#[cfg(test)]
mod model_refusal_tests {
    use super::*;

    #[test]
    fn a_provider_model_refusal_is_a_typed_cause() {
        let message = "The 'gpt-9' model is not supported when using Codex with a ChatGPT account.";
        for line in [
            serde_json::json!({"type": "error", "message": message}),
            serde_json::json!({"type": "turn.failed", "error": {"message": message}}),
        ] {
            let events = decode(line.to_string().as_bytes()).expect("decoded");
            assert!(
                matches!(events.as_slice(), [E::ModelUnavailable { message: m }] if m == message),
                "{events:?}"
            );
        }
        let other = serde_json::json!({"type": "error", "message": "stream disconnected"});
        let events = decode(other.to_string().as_bytes()).expect("decoded");
        assert!(matches!(events.as_slice(), [E::Error]));
    }
}
