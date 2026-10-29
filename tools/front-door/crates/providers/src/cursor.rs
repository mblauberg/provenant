use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, package_root, subtree,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{
    AuthenticationProfile, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use serde::Deserialize;
pub struct Cursor;

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &Cursor,
    quota_source: Some(&Cursor),
    display_name: "Cursor",
    quota: provenant_quota::QuotaPolicy {
        adapter: "cursor",
        timer_seconds: 1800,
        panel_seconds: 600,
        probe_seconds: 14,
        low_power: true,
        solo: false,
    },
};

crate::adapter!(Cursor, "cursor", prepare);
// The stream reports a display name (including Auto), not an exact model id.
const MODEL_ON_WIRE: bool = false;

fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    input.prompt.check_argv()?;
    let plan = input.confined.permissions();
    if plan.selected_authentication.as_deref() != Some("login") {
        return Err(ProviderError::UnsupportedPolicy);
    }
    let mut argv = vec![
        input.executable.into(),
        "--print".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--workspace".into(),
        input.cwd.into(),
        "--trust".into(),
        // Cursor's own sandbox does not nest inside the confinement profile.
        "--sandbox".into(),
        "disabled".into(),
    ];
    match access {
        Access::ReadOnly => argv.extend(["--mode".into(), "ask".into()]),
        Access::WorktreeWrite => argv.push("--force".into()),
    }
    if session == Session::Resume {
        let continuation = input.continuation.ok_or(ProviderError::UnknownIdentity)?;
        argv.extend(["--resume".into(), continuation.session().into()]);
    }
    if !input.route.model.is_empty() {
        argv.extend(["--model".into(), input.route.model.as_str().into()]);
    }
    argv.extend(["--".into(), input.prompt.text().into()]);
    let package = package_root(input.executable)?;
    let mut authentication = vec![exact(input.home.join(".cursor/cli-config.json"))];
    let config = input.home.join(".config/cursor");
    if config.is_dir() {
        authentication.push(subtree(config));
    }
    let chats = input.home.join(".cursor/chats");
    let mut env = base_env(input, &plan.scratch.path, "/usr/bin:/bin");
    env.extend([
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]);
    Ok(LaunchSpec {
        argv,
        env,
        stdin: Stdin::Null,
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup: vec![SetupStep::CreateDir(chats.clone())],
        git_wrapper: Default::default(),
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![exact(input.executable)],
                allow_process_fork: true,
                runtime_reads: [subtree(package)]
                    .into_iter()
                    .chain(if access == Access::WorktreeWrite {
                        crate::launch::git_global_files(&input.home.join(".config"))
                    } else {
                        Vec::new()
                    })
                    .collect(),
                authentication: vec![AuthenticationProfile {
                    mechanism: "login".into(),
                    roots: authentication,
                    in_place_writes: Vec::new(),
                }],
                state_writes: vec![subtree(chats)],
                // The resolved launcher is a shell script in the self-updated
                // `versions/<version>/` payload that execs its sibling `node`,
                // so every lane needs a shell and that one payload directory.
                tool_exec_roots: vec![subtree("/bin"), subtree("/usr/bin"), subtree(package)],
                provider_api: vec![NetworkEndpoint {
                    address: "*".into(),
                    port: 443,
                }],
                ..ProviderProfileSpec::default()
            },
            observations: Default::default(),
        },
        stream,
    })
}
crate::quota_source!(Cursor, "cursor");
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "system")]
    System { subtype: String, session_id: String },
    #[serde(rename = "assistant")]
    Assistant {
        message: Message,
        session_id: String,
    },
    #[serde(rename = "result")]
    Result {
        session_id: String,
        subtype: String,
        is_error: bool,
        result: Option<String>,
        usage: Option<Usage>,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Message {
    role: String,
    content: Vec<Content>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Content {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Usage {
    #[serde(rename = "inputTokens")]
    input: u64,
    #[serde(rename = "outputTokens")]
    output: u64,
    // Cursor reports cache tokens apart from `inputTokens`, as its own
    // dashboard rows do.
    #[serde(default, rename = "cacheReadTokens")]
    cache_read: Option<u64>,
    #[serde(default, rename = "cacheWriteTokens")]
    cache_write: Option<u64>,
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        // Auto is a selector, not answering-model evidence.
        Event::System {
            subtype,
            session_id,
        } => {
            if subtype == "init" {
                vec![E::Session(session_id)]
            } else {
                vec![]
            }
        }
        Event::Assistant {
            message,
            session_id,
        } if message.role == "assistant" => {
            let mut events = vec![E::Session(session_id)];
            for content in message.content {
                if let Content::Text { text } = content {
                    events.push(E::Text(text));
                }
            }
            events
        }
        Event::Result {
            session_id,
            subtype,
            is_error,
            result,
            usage,
        } => {
            let mut events = vec![E::Session(session_id)];
            if let Some(usage) = usage {
                events.push(E::Usage {
                    input: usage.input,
                    output: usage.output,
                    cache_read: usage.cache_read,
                    cache_write: usage.cache_write,
                    cache_write_1h: None,
                });
            }
            if is_error || subtype != "success" {
                events.push(E::Error);
            } else {
                if let Some(result) = result {
                    events.push(E::Text(result));
                }
                events.push(E::Terminal);
            }
            events
        }
        _ => vec![],
    })
}
