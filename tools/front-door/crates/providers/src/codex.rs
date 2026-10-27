use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, package_root, subtree,
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
    input.provenant.codex(&mut argv)?;
    argv.push("-".into());

    // Codex runs helpers from its own package (code-mode host, bundled `rg`
    // and `zsh`), and its shell tool runs system programs. System directories
    // are readable through the runtime baseline; exec roots add exec, not
    // reads. No shared install prefix: its var/ stays unreadable.
    let package = package_root(input.executable)?;
    let mut tool_exec_roots = vec![
        subtree("/bin"),
        subtree("/usr/bin"),
        subtree("/usr/libexec"),
        subtree(package),
    ];
    let mut runtime_reads = vec![subtree(package)];
    let tool_writes = Vec::new();
    let mut setup = vec![SetupStep::Symlink {
        link: codex_home.join("auth.json"),
        target: credential.clone(),
    }];
    let path = format!("{}:/usr/bin:/bin", input.home.join(".cargo/bin").display());
    let mut env = base_env(input, &plan.scratch.path, &path);
    input
        .provenant
        .environment(&plan.scratch.path, &mut env, &mut setup, false)?;
    crate::launch::rust_tools(
        input,
        access,
        &mut runtime_reads,
        &mut tool_exec_roots,
        &mut env,
        &mut setup,
    );
    crate::launch::node_tools(
        input.home,
        access,
        crate::launch::lane_cache(input),
        &mut runtime_reads,
        &mut tool_exec_roots,
        &mut env,
    );
    // Git may inspect global config even for status. Keep host hooks,
    // includes and credential helpers outside the lane's environment.
    env.extend([
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]);
    env.push(("CODEX_HOME".into(), codex_home.into()));
    env.push(("SHELL".into(), "/bin/zsh".into()));
    let mut spec = LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(input.prompt.text().as_bytes().to_vec()),
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup,
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
                cache_read: None,
                cache_write: None,
            },
            E::Terminal,
        ],
        Event::Error { message } => {
            let lower = message.to_ascii_lowercase();
            vec![if lower.contains("reconnecting") {
                E::TransientError
            } else if lower.contains("too many requests") || lower.contains("status: 429") {
                E::RateLimited { message }
            } else {
                E::Error
            }]
        }
        Event::Failed => vec![E::Error],
        _ => vec![],
    })
}
