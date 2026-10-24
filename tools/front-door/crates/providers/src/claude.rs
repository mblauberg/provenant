use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, observations, package_root,
    subtree, uuid_v4,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{NetworkEndpoint, ProviderProfile, ProviderProfileSpec};
use serde::Deserialize;
use std::ffi::{OsStr, OsString};
use std::path::Path;
pub struct Claude;
crate::adapter!(Claude, "claude", prepare);
const MODEL_ON_WIRE: bool = true;

const OBSERVATIONS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/providers/claude/confinement.json"
));

/// The default config directory's keychain item. Other identities (a
/// `CLAUDE_CONFIG_DIR` adds a suffix) are never read.
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
/// Refresh when the access token expires within this window.
const REFRESH_WINDOW_MS: u64 = 15 * 60 * 1000;

/// A short-lived OAuth access token for the default identity, read by the host
/// outside the sandbox. It is never logged, persisted or printed: `Debug` is
/// redacted and the refresh token is never retained.
pub struct OauthToken {
    access: String,
    expires_at_ms: u64,
}

impl std::fmt::Debug for OauthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OauthToken(redacted)")
    }
}

impl OauthToken {
    /// Parses the keychain item's JSON, keeping only the access token and its
    /// expiry. An empty or expired token is refused.
    pub fn parse(item: &[u8], now_ms: u64) -> Result<Self, ProviderError> {
        #[derive(Deserialize)]
        struct Item {
            #[serde(rename = "claudeAiOauth")]
            oauth: Oauth,
        }
        #[derive(Deserialize)]
        struct Oauth {
            #[serde(rename = "accessToken")]
            access: String,
            #[serde(rename = "expiresAt")]
            expires_at_ms: u64,
        }
        let Item { oauth } = serde_json::from_slice::<Item>(item.trim_ascii())
            .map_err(|_| ProviderError::Malformed)?;
        if oauth.access.is_empty()
            || oauth
                .access
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
            || oauth.expires_at_ms <= now_ms
        {
            return Err(ProviderError::UnsupportedPolicy);
        }
        Ok(Self {
            access: oauth.access,
            expires_at_ms: oauth.expires_at_ms,
        })
    }

    /// True when the host should refresh before launching.
    pub fn needs_refresh(&self, now_ms: u64) -> bool {
        self.expires_at_ms.saturating_sub(now_ms) < REFRESH_WINDOW_MS
    }
}

/// The host's keychain read for the default identity, run outside the sandbox
/// with stdout captured and never logged; parse it with [`OauthToken::parse`].
pub fn token_argv(user: &OsStr) -> Vec<OsString> {
    [
        "/usr/bin/security".as_ref(),
        "find-generic-password".as_ref(),
        "-s".as_ref(),
        KEYCHAIN_SERVICE.as_ref(),
        "-a".as_ref(),
        user,
        "-w".as_ref(),
    ]
    .map(OsString::from)
    .to_vec()
}

/// Zero-cost refresh, run by the host outside the sandbox with the default
/// config directory and stdin null: the CLI refreshes its stored token, then
/// the API rejects the model, so no usage is spent. Re-read the token after.
pub fn refresh_argv(executable: &Path) -> Vec<OsString> {
    [
        executable.as_os_str(),
        "-p".as_ref(),
        "x".as_ref(),
        "--model".as_ref(),
        "provenant-refresh-probe".as_ref(),
        "--max-turns".as_ref(),
        "1".as_ref(),
    ]
    .map(OsString::from)
    .to_vec()
}

/// Claude uses the default config directory. The host reads its login token
/// and passes it in `CLAUDE_CODE_OAUTH_TOKEN`, so the keychain stays
/// unreadable and the CLI execs nothing. The model still gets no shell: file
/// tools only, and edit tools only for writers. The one writable state is this
/// session's transcript.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    // No credential path is granted: the token arrives in the environment.
    if plan.selected_authentication.is_some() {
        return Err(ProviderError::UnsupportedPolicy);
    }
    let token = input.token.ok_or(ProviderError::UnsupportedPolicy)?;
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
    let package = package_root(input.executable)?;
    let scratch = &plan.scratch.path;
    let mut env = base_env(input, scratch, "/usr/bin:/bin");
    env.extend(
        [
            ("CLAUDE_CODE_TMPDIR", scratch.as_os_str()),
            ("DISABLE_AUTOUPDATER", "1".as_ref()),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1".as_ref()),
            ("CLAUDE_CODE_OAUTH_TOKEN", token.access.as_ref()),
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
                process_executables: vec![exact(input.executable)],
                allow_process_fork: true,
                // Not `~/.claude.json`: it can hold MCP server secrets and the
                // launch never needs it.
                runtime_reads: vec![subtree(package)],
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
        #[serde(default)]
        api_error_status: Option<u16>,
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
            api_error_status,
        } => {
            let mut events = vec![E::Session(session_id)];
            if api_error_status == Some(401) {
                events.push(E::AuthRequired);
            } else if is_error || subtype != "success" {
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

/// Metadata only. The caller must never read a token to establish presence.
#[derive(Clone, Copy)]
pub enum CredentialPresence {
    Present,
    Absent,
    Unavailable,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigninState {
    SignedIn,
    SignedOut,
    CredentialStorePresent,
    KeychainUnavailable,
    Unknown,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SigninStatus {
    pub state: SigninState,
    /// A local status probe does not establish network token validity.
    pub token_validity: String,
}
/// Decode only the CLI's login bit. Account identifiers and diagnostics never
/// escape this boundary; a negative probe alone cannot prove sign-out over SSH.
pub fn signin_status(
    stdout: &[u8],
    exit_code: Option<i64>,
    presence: CredentialPresence,
) -> SigninStatus {
    #[derive(Deserialize)]
    struct Status {
        #[serde(rename = "loggedIn")]
        logged_in: bool,
    }
    let state = match wire::<Status>(stdout) {
        Ok(status) if status.logged_in && exit_code == Some(0) => SigninState::SignedIn,
        Ok(status) if !status.logged_in => match presence {
            CredentialPresence::Present => SigninState::CredentialStorePresent,
            CredentialPresence::Absent => SigninState::SignedOut,
            CredentialPresence::Unavailable => SigninState::KeychainUnavailable,
            CredentialPresence::Unknown => SigninState::Unknown,
        },
        _ => SigninState::Unknown,
    };
    SigninStatus {
        state,
        token_validity: "unverified".into(),
    }
}
