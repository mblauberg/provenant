use crate::launch::{Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{
    AuthenticationProfile, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use serde::Deserialize;
use std::ffi::OsString;
pub struct Agy;
crate::adapter!(Agy, "agy", prepare);
const MODEL_ON_WIRE: bool = true;

/// The CLI's own turn limit; the supervisor's deadline is the real bound.
const PRINT_TIMEOUT: &str = "900s";
const SECURITY: &str = "/usr/bin/security";
const KEYCHAIN: &str = "Library/Keychains/login.keychain-db";

/// Agy runs with the attempt's private state as `HOME`, so it sees none of the
/// user's Gemini or Antigravity configuration, MCP servers, plugins, hooks,
/// sidecars or knowledge: its state lives and dies in the private home. Its
/// sign-in is in the login keychain, so the private home links only that one
/// file, read-only. Headless mode auto-denies every tool permission it cannot
/// prompt for, so writers auto-approve tools: our confinement profile, not
/// agy's prompts, bounds what a writer can do. Its own sandbox cannot nest
/// inside ours.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    // Agy has no signed-in identity source: a registered account is unsupported.
    if plan.selected_authentication.as_deref() != Some("keychain") || input.account.is_some() {
        return Err(ProviderError::UnsupportedPolicy);
    }
    input.prompt.check_argv()?;
    let home = &plan
        .private_state
        .as_ref()
        .ok_or(ProviderError::UnsupportedPolicy)?
        .path;
    let keychain = input.home.join(KEYCHAIN);
    let mut argv: Vec<OsString> = [
        input.executable.as_os_str(),
        "--output-format".as_ref(),
        "stream-json".as_ref(),
        "--disable-slash-commands".as_ref(),
        "--print-timeout".as_ref(),
        PRINT_TIMEOUT.as_ref(),
    ]
    .map(OsString::from)
    .to_vec();
    if access == Access::WorktreeWrite {
        argv.push("--dangerously-skip-permissions".into());
    }
    if session == Session::Resume {
        let continuation = input.continuation.ok_or(ProviderError::UnknownIdentity)?;
        argv.extend(["--conversation".into(), continuation.session().into()]);
    }
    argv.extend(["--model".into(), input.route.model.as_str().into()]);
    if let Some(effort) = &input.route.effort {
        argv.extend(["--effort".into(), effort.into()]);
    }
    argv.extend(["--print".into(), input.prompt.text().into()]);

    let mut env = base_env(input, &plan.scratch.path, "/usr/bin:/bin");
    for (key, value) in &mut env {
        if key == "HOME" {
            *value = home.into();
        }
    }
    let keychains = home.join("Library/Keychains");
    Ok(LaunchSpec {
        argv,
        env,
        stdin: Stdin::Null,
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup: vec![
            SetupStep::CreateDir(keychains.clone()),
            SetupStep::Symlink {
                link: keychains.join("login.keychain-db"),
                target: keychain.clone(),
            },
        ],
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                // The keyring library reads its token through Apple's `security`.
                process_executables: vec![exact(input.executable), exact(SECURITY)],
                allow_process_fork: true,
                authentication: vec![AuthenticationProfile {
                    mechanism: "keychain".into(),
                    roots: vec![exact(&keychain)],
                    in_place_writes: Vec::new(),
                }],
                provider_api: vec![NetworkEndpoint {
                    address: "*".into(),
                    port: 443,
                }],
                // Its agent runs behind a local server on a loopback port.
                loopback_server: true,
                ..ProviderProfileSpec::default()
            },
            observations: Default::default(),
        },
        stream,
    })
}
crate::quota_source!(Agy, "agy");
#[derive(Deserialize)]
#[serde(tag = "event")]
enum Event {
    #[serde(rename = "init")]
    Init {
        conversation_id: String,
        init: InitWire,
    },
    #[serde(rename = "step_update")]
    Step { step_update: Step },
    #[serde(rename = "result")]
    Result { result: ResultWire },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct InitWire {
    model: String,
}
#[derive(Deserialize)]
struct Step {
    conversation_id: String,
    step_type: String,
    text_delta: Option<String>,
}
#[derive(Deserialize)]
struct ResultWire {
    conversation_id: String,
    status: String,
    response: String,
    error: Option<String>,
    usage: Usage,
}
#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        Event::Init {
            conversation_id,
            init,
        } => vec![E::Session(conversation_id), E::Model(init.model)],
        Event::Step { step_update } => {
            let mut events = vec![E::Session(step_update.conversation_id)];
            if step_update.step_type == "agent_response"
                && let Some(text) = step_update.text_delta
            {
                events.push(E::Text(text));
            }
            events
        }
        Event::Result { result } => {
            let mut events = vec![E::Usage {
                input: result.usage.input_tokens,
                output: result.usage.output_tokens,
            }];
            if !result.conversation_id.is_empty() {
                events.push(E::Session(result.conversation_id));
            }
            if result.error.is_some() || result.status != "SUCCESS" {
                events.push(E::Error);
            } else {
                events.push(E::Text(result.response));
                events.push(E::Terminal);
            }
            events
        }
        _ => vec![],
    })
}
