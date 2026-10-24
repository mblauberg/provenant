use crate::launch::{Access, PrepareInput, Session, Stdin, base_env, exact, observations, subtree};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{NetworkEndpoint, ProviderProfile, ProviderProfileSpec};
use serde::Deserialize;
use std::ffi::OsString;
pub struct OpenCode;
crate::adapter!(OpenCode, "opencode", prepare);
/// The JSON run stream carries no answering-model position.
const MODEL_ON_WIRE: bool = false;

const OBSERVATIONS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/providers/opencode/confinement.json"
));

/// OpenCode v2 runs a private standalone server (a child over loopback) with
/// every XDG directory in the attempt's private state, so it sees none of the
/// user's OpenCode configuration, sessions or stored provider keys: only
/// keyless providers (such as OpenCode's free models) can answer. Repository
/// configuration is ignored; inline configuration denies the shell and web
/// fetch, and file edits for read-only runs.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    if plan.selected_authentication.is_some() {
        return Err(ProviderError::UnsupportedPolicy);
    }
    let private = &plan
        .private_state
        .as_ref()
        .ok_or(ProviderError::UnsupportedPolicy)?
        .path;
    let mut argv: Vec<OsString> = [
        input.executable.as_os_str(),
        "run".as_ref(),
        "--standalone".as_ref(),
        "--format".as_ref(),
        "json".as_ref(),
        "--log-level".as_ref(),
        "error".as_ref(),
    ]
    .map(OsString::from)
    .to_vec();
    if session == Session::Resume {
        let continuation = input.continuation.ok_or(ProviderError::UnknownIdentity)?;
        argv.extend(["-s".into(), continuation.session().into()]);
    }
    let model = match &input.route.effort {
        Some(effort) => format!("{}#{effort}", input.route.model),
        None => input.route.model.clone(),
    };
    argv.extend(["-m".into(), model.into()]);
    let edit = match access {
        Access::ReadOnly => "deny",
        Access::WorktreeWrite => "allow",
    };
    let config = format!(
        r#"{{"autoupdate":false,"share":"disabled","mcp":{{}},"plugin":[],"permission":{{"edit":"{edit}","bash":"deny","webfetch":"deny"}}}}"#
    );
    let package = match input.executable.parent() {
        Some(bin) if bin.file_name() == Some("bin".as_ref()) => bin.parent(),
        parent => parent,
    }
    .ok_or(ProviderError::InvalidPath)?;
    // No Git on the search path: project discovery finds none and OpenCode
    // treats the working directory as a plain folder, so no Git is executed.
    let mut env = base_env(input, &plan.scratch.path, "/bin");
    // The private state is OpenCode's home: its data, configuration, caches
    // and every home-relative lookup (other agents' files included) stay
    // inside it. The free models need no credentials from the user's home.
    for (key, value) in &mut env {
        if key == "HOME" {
            *value = private.into();
        }
    }
    env.push(("OPENCODE_CONFIG_CONTENT".into(), config.into()));
    // No repository configuration or plugins, and no Claude Code files.
    for key in [
        "OPENCODE_DISABLE_AUTOUPDATE",
        "OPENCODE_DISABLE_PROJECT_CONFIG",
        "OPENCODE_DISABLE_CLAUDE_CODE",
        "OPENCODE_DISABLE_FILEWATCHER",
    ] {
        env.push((key.into(), "1".into()));
    }
    Ok(LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(input.prompt.text().as_bytes().to_vec()),
        cwd: input.cwd.to_owned(),
        setup: Vec::new(),
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![exact(input.executable)],
                // The standalone server is a child copy of the same program.
                allow_process_fork: true,
                runtime_reads: vec![subtree(package)],
                provider_api: vec![NetworkEndpoint {
                    address: "*".into(),
                    port: 443,
                }],
                loopback_server: true,
                ..ProviderProfileSpec::default()
            },
            observations: observations(OBSERVATIONS, access)?,
        },
        stream,
    })
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    Text {
        #[serde(rename = "sessionID")]
        session: String,
        part: Text,
    },
    StepFinish {
        #[serde(rename = "sessionID")]
        session: String,
        part: Finish,
    },
    Error,
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Text {
    text: String,
}
#[derive(Deserialize)]
struct Finish {
    reason: String,
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        Event::Text { session, part } => vec![E::Session(session), E::Text(part.text)],
        Event::StepFinish { session, part } => {
            let mut events = vec![E::Session(session)];
            if part.reason == "stop" {
                events.push(E::Terminal);
            }
            events
        }
        Event::Error => vec![E::Error],
        _ => vec![],
    })
}
