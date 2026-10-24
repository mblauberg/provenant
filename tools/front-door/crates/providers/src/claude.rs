use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, observations, subtree,
    uuid_v4,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{
    AuthenticationProfile, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use serde::Deserialize;
use std::ffi::OsString;
use std::path::Path;
pub struct Claude;
crate::adapter!(Claude, "claude", prepare);
const MODEL_ON_WIRE: bool = true;

const OBSERVATIONS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/providers/claude/confinement.json"
));

/// Claude uses the default config directory and its keychain login, which it
/// reads through `/usr/bin/security`. Because that keychain path could reach
/// other credentials, the model gets no shell: file tools only, and edit tools
/// only for writers. The one writable state is this session's transcript.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    if plan.selected_authentication.as_deref() != Some("login") {
        return Err(ProviderError::UnsupportedPolicy);
    }
    let session_id = match (session, input.continuation) {
        (Session::Resume, Some(continuation)) => continuation.session().to_owned(),
        (Session::Resume, None) => return Err(ProviderError::UnknownIdentity),
        (Session::Fresh, _) => uuid_v4(input.nonce),
    };
    let mut argv: Vec<OsString> = [
        input.executable.as_os_str(),
        "-p".as_ref(),
        "--safe-mode".as_ref(),
        "--strict-mcp-config".as_ref(),
        "--disable-slash-commands".as_ref(),
        "--permission-prompts".as_ref(),
        "none".as_ref(),
        "--output-format".as_ref(),
        "stream-json".as_ref(),
        "--verbose".as_ref(),
    ]
    .map(OsString::from)
    .to_vec();
    argv.push(match session {
        Session::Fresh => "--session-id".into(),
        Session::Resume => "--resume".into(),
    });
    argv.push(session_id.clone().into());
    let (mode, tools) = match access {
        Access::ReadOnly => ("dontAsk", "Read,Grep,Glob"),
        Access::WorktreeWrite => ("acceptEdits", "Read,Grep,Glob,Edit,Write"),
    };
    argv.extend(
        [
            "--permission-mode",
            mode,
            "--tools",
            tools,
            "--model",
            input.route.model.as_str(),
        ]
        .map(OsString::from),
    );
    if let Some(effort) = &input.route.effort {
        argv.extend(["--effort".into(), effort.into()]);
    }

    let project = input
        .home
        .join(".claude/projects")
        .join(project_key(input.cwd));
    let transcript_dir = project.join(&session_id);
    let package = match input.executable.parent() {
        Some(bin) if bin.file_name() == Some("bin".as_ref()) => bin.parent(),
        parent => parent,
    }
    .ok_or(ProviderError::InvalidPath)?;
    let scratch = &plan.scratch.path;
    let mut env = base_env(input, scratch, "/usr/bin:/bin:/usr/sbin:/sbin");
    env.extend(
        [
            ("CLAUDE_CODE_TMPDIR", scratch.as_os_str()),
            ("DISABLE_AUTOUPDATER", "1".as_ref()),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1".as_ref()),
        ]
        .map(|(key, value)| (OsString::from(key), value.to_owned())),
    );
    Ok(LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(input.prompt.text().as_bytes().to_vec()),
        cwd: input.cwd.to_owned(),
        setup: vec![SetupStep::CreateDir(transcript_dir.clone())],
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![exact(input.executable), exact("/usr/bin/security")],
                allow_process_fork: true,
                runtime_reads: vec![subtree(package)],
                authentication: vec![AuthenticationProfile {
                    mechanism: "login".into(),
                    // Not `~/.claude.json`: it can hold MCP server secrets
                    // and the launch never needs it.
                    roots: vec![exact(
                        input.home.join("Library/Keychains/login.keychain-db"),
                    )],
                    in_place_writes: vec![],
                }],
                state_writes: vec![
                    exact(project.join(format!("{session_id}.jsonl"))),
                    subtree(transcript_dir),
                ],
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

/// Claude's per-project transcript directory name: every byte outside ASCII
/// letters and digits becomes `-`.
fn project_key(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}
crate::quota_source!(Claude, "claude");
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "system")]
    System { subtype: String, session_id: String },
    #[serde(rename = "assistant")]
    Assistant {
        message: Message,
        session_id: String,
        parent_tool_use_id: Option<String>,
        error: Option<String>,
        #[serde(default)]
        is_api_error_message: bool,
    },
    #[serde(rename = "result")]
    Result {
        session_id: String,
        subtype: String,
        is_error: bool,
        result: Option<String>,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Message {
    model: String,
    role: String,
    content: Vec<Content>,
    usage: Option<Usage>,
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
    input_tokens: u64,
    output_tokens: u64,
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
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
            parent_tool_use_id: None,
            error,
            is_api_error_message,
        } if message.role == "assistant" => {
            let mut events = vec![E::Session(session_id)];
            if error.is_none() && !is_api_error_message && message.model != "<synthetic>" {
                events.push(E::Model(message.model));
            }
            if let Some(error) = error {
                events.push(if error == "authentication_failed" {
                    E::AuthRequired
                } else {
                    E::Error
                });
            } else if is_api_error_message {
                events.push(E::Error);
            }
            for content in message.content {
                if let Content::Text { text } = content {
                    events.push(E::Text(text));
                }
            }
            if let Some(usage) = message.usage {
                events.push(E::Usage {
                    input: usage.input_tokens,
                    output: usage.output_tokens,
                });
            }
            events
        }
        Event::Result {
            session_id,
            subtype,
            is_error,
            result,
        } => {
            let mut events = vec![E::Session(session_id)];
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
