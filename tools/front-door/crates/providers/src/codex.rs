use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, observations, subtree,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::CodexPolicy;
use provenant_confinement::permissions::{
    AuthenticationProfile, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use serde::Deserialize;
use std::ffi::OsString;
use std::path::Path;
pub struct Codex;
crate::adapter!(Codex, "codex", prepare);
/// The exec JSON stream carries no answering-model position.
const MODEL_ON_WIRE: bool = false;

const OBSERVATIONS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/providers/codex/confinement.json"
));

/// Codex runs with a private `CODEX_HOME` (the attempt's private state) that
/// links only `auth.json` from the user's home. Its own seatbelt cannot nest
/// inside ours, so it runs unsandboxed inside the outer profile, which is the
/// real boundary. Its shell tool may run system and install-prefix programs.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
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
    let credential = input.home.join(".codex/auth.json");
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
    argv.push("-".into());

    let prefix = install_prefix(input.executable);
    let mut tool_exec_roots: Vec<_> = ["/bin", "/sbin", "/usr/bin", "/usr/sbin", "/usr/libexec"]
        .into_iter()
        .map(subtree)
        .collect();
    let mut search_path = String::from("/usr/bin:/bin:/usr/sbin:/sbin");
    if let Some(prefix) = prefix {
        tool_exec_roots.push(subtree(prefix));
        search_path = format!("{prefix}/bin:{search_path}");
    }
    let mut env = base_env(input, &plan.scratch.path, &search_path);
    env.push(("CODEX_HOME".into(), codex_home.into()));
    env.push(("SHELL".into(), "/bin/zsh".into()));
    Ok(LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(input.prompt.text().as_bytes().to_vec()),
        cwd: input.cwd.to_owned(),
        setup: vec![SetupStep::Symlink {
            link: codex_home.join("auth.json"),
            target: credential.clone(),
        }],
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![exact(input.executable)],
                allow_process_fork: true,
                authentication: vec![AuthenticationProfile {
                    mechanism: "login".into(),
                    roots: vec![exact(&credential)],
                    // Token refresh rewrites auth.json through the link.
                    in_place_writes: vec![exact(&credential)],
                }],
                tool_exec_roots,
                provider_api: vec![NetworkEndpoint {
                    address: "*".into(),
                    port: 443,
                }],
                ..ProviderProfileSpec::default()
            },
            observations: observations(OBSERVATIONS, access)?,
        },
        stream,
    })
}

/// Homebrew-style install prefixes whose programs the shell tool may run.
fn install_prefix(executable: &Path) -> Option<&'static str> {
    ["/opt/homebrew", "/usr/local"]
        .into_iter()
        .find(|prefix| executable.starts_with(prefix))
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
    Failed,
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
    input_tokens: u64,
    output_tokens: u64,
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        Event::Thread { thread_id } => vec![E::Session(thread_id)],
        Event::Item {
            item: Item::Answer { text },
        } => vec![E::Text(text)],
        Event::Completed { usage } => vec![
            E::Usage {
                input: usage.input_tokens,
                output: usage.output_tokens,
            },
            E::Terminal,
        ],
        Event::Error { message } => {
            vec![if message.to_ascii_lowercase().contains("reconnecting") {
                E::TransientError
            } else {
                E::Error
            }]
        }
        Event::Failed => vec![E::Error],
        _ => vec![],
    })
}
