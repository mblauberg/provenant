use crate::launch::{Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{
    AuthenticationProfile, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use serde::Deserialize;
use std::ffi::OsString;
pub struct Agy;

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &Agy,
    quota_source: Some(&Agy),
    display_name: "Agy",
    quota: provenant_quota::QuotaPolicy {
        adapter: "agy",
        timer_seconds: 1200,
        panel_seconds: 900,
        probe_seconds: 30,
        low_power: false,
        solo: true,
    },
};

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
    // Agy's keychain sign-in cannot be selected through a registered directory.
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
            if let Some(message) = result
                .error
                .as_deref()
                .filter(|message| quota_reached(message))
            {
                events.push(E::QuotaLimited {
                    message: message.to_owned(),
                    pool: None,
                    resets_in_seconds: resets_in(message),
                });
            } else if result.error.is_some() || result.status != "SUCCESS" {
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
/// The CLI's own exhausted-quota refusal, `Individual quota reached ... Resets
/// in 7h23m`, possibly behind its 429 status. The error must open with it, so
/// an echoed prompt or another error quoting it does not count. A bare 429 or
/// `RESOURCE_EXHAUSTED` may be a per-minute rate limit, so neither counts.
fn quota_reached(message: &str) -> bool {
    const REFUSAL: &str = "individual quota reached";
    let message = message.trim_start();
    let message = message
        .strip_prefix("429")
        .filter(|rest| rest.starts_with([':', ' ']))
        .map_or(message, |rest| rest.trim_start_matches([':', ' ']));
    message
        .get(..REFUSAL.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(REFUSAL))
}
/// The refusal's stated wait, only from its closing `Resets in 7h23m`, within
/// one weekly window.
fn resets_in(message: &str) -> Option<u64> {
    const MARKER: &str = "resets in ";
    let lower = message
        .trim_end()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let mut rest = lower[lower.rfind(MARKER)? + MARKER.len()..].trim_start();
    let mut total = 0_u64;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let scale = match rest.as_bytes().get(digits).filter(|_| digits > 0)? {
            b'd' => 86_400,
            b'h' => 3_600,
            b'm' => 60,
            b's' => 1,
            _ => return None,
        };
        total = total.checked_add(rest[..digits].parse::<u64>().ok()?.checked_mul(scale)?)?;
        rest = rest[digits + 1..].trim_start();
    }
    (1..=7 * 86_400).contains(&total).then_some(total)
}
