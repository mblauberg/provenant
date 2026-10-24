use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, observations, package_root,
    subtree, uuid_v4,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{NetworkEndpoint, ProviderProfile, ProviderProfileSpec};
use provenant_records::CanonicalV1;
use provenant_supervisor::exec::{ExecOptions, Executor, StreamPolicy, Termination};
use serde::Deserialize;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;
pub struct Claude;
crate::adapter!(Claude, "claude", prepare);
const MODEL_ON_WIRE: bool = true;

const OBSERVATIONS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/providers/claude/confinement.json"
));

/// The default config directory's keychain service. A non-default
/// `CLAUDE_CONFIG_DIR` appends [`keychain_suffix`].
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
/// Refresh when the access token expires within this window.
const REFRESH_WINDOW_MS: u64 = 15 * 60 * 1000;
/// After a refresh, the CLI may keep a token that is inside the window; accept
/// it while this much life remains.
const MIN_LIFETIME_MS: u64 = 60 * 1000;

/// A short-lived OAuth access token for the default identity, read by the host
/// outside the sandbox. It is never logged, persisted or printed: `Debug` is
/// redacted and the refresh token is never retained.
pub struct OauthToken {
    access: String,
    expires_at_ms: u64,
    account: Option<(String, std::path::PathBuf)>,
}

impl std::fmt::Debug for OauthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OauthToken(redacted)")
    }
}

impl OauthToken {
    /// Parses the keychain item's JSON, keeping only the access token and its
    /// expiry. An empty or expired token is refused.
    pub(crate) fn parse(item: &[u8], now_ms: u64) -> Result<Self, ProviderError> {
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
            account: None,
        })
    }

    /// True when the host should refresh before launching.
    fn needs_refresh(&self, now_ms: u64) -> bool {
        self.expires_at_ms.saturating_sub(now_ms) < REFRESH_WINDOW_MS
    }
}

/// Where the host finds one Claude identity's login. The account is never
/// inferred: the caller names the config directory it is entitled to use.
pub struct Login<'a> {
    pub executable: &'a Path,
    pub home: &'a Path,
    pub user: &'a OsStr,
    pub config_dir: &'a Path,
    /// An empty private directory: the refresh's working directory, so no
    /// project settings, hooks or instructions apply.
    pub scratch: &'a Path,
}

/// Keychain service suffix for a non-default `CLAUDE_CONFIG_DIR`: the first
/// eight hex digits of the directory path's digest.
pub fn keychain_suffix(config_dir: &[u8]) -> String {
    let digest = CanonicalV1::file_digest(config_dir).to_string();
    let hex = digest.strip_prefix("sha256:").unwrap_or(&digest);
    hex.get(..8).unwrap_or(hex).to_owned()
}

/// Reads one identity's login token outside the sandbox. When it expires
/// within the refresh window, runs the CLI once at zero cost to refresh it,
/// then reads again and accepts a token with at least a minute left. The
/// refresh runs with every customisation off (safe mode, no hooks, no MCP
/// servers, no slash commands, no session file) in the empty scratch
/// directory. Every subprocess is capped by the caller's `deadline_ms` on the
/// `now_ms` clock. The token is never logged, printed or persisted.
pub fn refresh_if_expiring(
    executor: &dyn Executor,
    login: &Login<'_>,
    now_ms: &dyn Fn() -> u64,
    deadline_ms: u64,
) -> Result<OauthToken, ProviderError> {
    let budget = |cap: Duration| -> Result<Duration, ProviderError> {
        match deadline_ms.saturating_sub(now_ms()) {
            0 => Err(ProviderError::CredentialUnavailable),
            left => Ok(cap.min(Duration::from_millis(left))),
        }
    };
    let default = login.config_dir == login.home.join(".claude");
    let mut service = KEYCHAIN_SERVICE.to_owned();
    let mut env: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), login.home.into()),
        ("USER".into(), login.user.into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    if !default {
        service.push('-');
        service.push_str(&keychain_suffix(login.config_dir.as_os_str().as_bytes()));
        env.push(("CLAUDE_CONFIG_DIR".into(), login.config_dir.into()));
    }
    let read = || -> Result<OauthToken, ProviderError> {
        let argv: Vec<OsString> = [
            "/usr/bin/security".as_ref(),
            "find-generic-password".as_ref(),
            "-s".as_ref(),
            OsStr::new(&service),
            "-a".as_ref(),
            login.user,
            "-w".as_ref(),
        ]
        .map(OsString::from)
        .to_vec();
        let mut options = ExecOptions::new(&argv, login.scratch, budget(Duration::from_secs(10))?);
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
        options.stderr = StreamPolicy::Discard;
        let out = executor
            .execute(options)
            .map_err(|_| ProviderError::CredentialUnavailable)?;
        if out.termination != Termination::Exit(0) || out.stdout.truncated {
            return Err(ProviderError::CredentialUnavailable);
        }
        OauthToken::parse(&out.stdout.bytes, now_ms())
            .map_err(|_| ProviderError::CredentialUnavailable)
    };
    match read() {
        Ok(token) if !token.needs_refresh(now_ms()) => return Ok(token),
        Ok(_) | Err(_) => {}
    }
    let argv: Vec<OsString> = [
        login.executable.as_os_str(),
        "-p".as_ref(),
        "x".as_ref(),
        // The API rejects this model after the CLI refreshes its stored
        // token, so no usage is spent.
        "--model".as_ref(),
        "provenant-refresh-probe".as_ref(),
        "--max-turns".as_ref(),
        "1".as_ref(),
        "--safe-mode".as_ref(),
        "--strict-mcp-config".as_ref(),
        "--settings".as_ref(),
        r#"{"disableAllHooks":true}"#.as_ref(),
        "--disable-slash-commands".as_ref(),
        "--no-session-persistence".as_ref(),
    ]
    .map(OsString::from)
    .to_vec();
    let mut options = ExecOptions::new(&argv, login.scratch, budget(Duration::from_secs(60))?);
    options.env = Some(&env);
    options.stdout = StreamPolicy::Discard;
    options.stderr = StreamPolicy::Discard;
    executor
        .execute(options)
        .map_err(|_| ProviderError::CredentialUnavailable)?;
    let token = read()?;
    if token.expires_at_ms.saturating_sub(now_ms()) < MIN_LIFETIME_MS {
        return Err(ProviderError::CredentialUnavailable);
    }
    Ok(token)
}

/// Load a token for a registered identity. The status probes bracket the secret
/// read/refresh so a rotating default directory cannot be attributed by path.
/// This uses Lane Q's fingerprint, never token material, for identity.
pub fn refresh_for_account(
    executor: &dyn Executor,
    login: &Login<'_>,
    account: &crate::identity::RegisteredAccount,
    now_ms: &dyn Fn() -> u64,
    deadline_ms: u64,
) -> Result<OauthToken, ProviderError> {
    if account.adapter() != "claude" || login.config_dir != account.directory() {
        return Err(ProviderError::IdentityMismatch);
    }
    let check = || -> Result<(), ProviderError> {
        let remaining = deadline_ms.saturating_sub(now_ms());
        if remaining == 0 {
            return Err(ProviderError::CredentialUnavailable);
        }
        let argv = [
            login.executable.into(),
            "auth".into(),
            "status".into(),
            "--json".into(),
        ];
        let env = [
            ("HOME".into(), login.home.into()),
            ("USER".into(), login.user.into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("CLAUDE_CONFIG_DIR".into(), login.config_dir.into()),
        ];
        let mut options = ExecOptions::new(
            &argv,
            login.scratch,
            Duration::from_millis(remaining.min(4000)),
        );
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        let out = executor
            .execute(options)
            .map_err(|_| ProviderError::CredentialUnavailable)?;
        if out.termination != Termination::Exit(0)
            || out.timed_out
            || out.stdout.truncated
            || out.stdout.read_failed
            || out.drain_truncated
            || out.cancelled
        {
            return Err(ProviderError::CredentialUnavailable);
        }
        #[derive(Deserialize)]
        struct Status {
            #[serde(rename = "loggedIn")]
            logged_in: bool,
            email: String,
        }
        let status: Status = serde_json::from_slice(&out.stdout.bytes)
            .map_err(|_| ProviderError::UnknownIdentity)?;
        if !status.logged_in
            || crate::identity::fingerprint(&status.email).as_deref() != Some(account.key())
        {
            return Err(ProviderError::IdentityMismatch);
        }
        Ok(())
    };
    check()?;
    let mut token = refresh_if_expiring(executor, login, now_ms, deadline_ms)?;
    check()?;
    token.account = Some((account.key().into(), account.directory().to_owned()));
    Ok(token)
}

/// Claude uses the selected config directory. The host reads its login token
/// and passes it in `CLAUDE_CODE_OAUTH_TOKEN`, so the keychain stays
/// unreadable. Provenant and its hook helpers are the only added executables.
/// The model still gets no shell: file tools only, and edit tools only for
/// writers. Writable state covers this session's transcript and Provenant's
/// bounded coordination state.
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
    if token.account.is_some() && input.account.is_none() {
        return Err(ProviderError::IdentityMismatch);
    }
    if let Some(account) = input.account
        && token
            .account
            .as_ref()
            .is_none_or(|(key, dir)| key != account.key() || dir != account.directory())
    {
        return Err(ProviderError::IdentityMismatch);
    }
    let session_id = match (session, input.continuation) {
        (Session::Resume, Some(continuation)) => continuation.session().to_owned(),
        (Session::Resume, None) => return Err(ProviderError::UnknownIdentity),
        (Session::Fresh, _) => uuid_v4(input.nonce),
    };
    let mut argv: Vec<OsString> = [
        input.executable.as_os_str(),
        "-p".as_ref(),
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

    let config_dir = input.account.map_or_else(
        || input.home.join(".claude"),
        |account| account.directory().to_owned(),
    );
    let project = config_dir.join("projects").join(project_key(input.cwd));
    let transcript_dir = project.join(&session_id);
    let package = package_root(input.executable)?;
    let scratch = &plan.scratch.path;
    let mut setup = input.provenant.claude(scratch, &mut argv)?;
    setup.push(SetupStep::CreateDir(transcript_dir.clone()));
    let mut env = base_env(input, scratch, "/usr/bin:/bin");
    input.provenant.environment(&mut env);
    env.extend(
        [
            ("CLAUDE_CODE_TMPDIR", scratch.as_os_str()),
            ("DISABLE_AUTOUPDATER", "1".as_ref()),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1".as_ref()),
            ("CLAUDE_CODE_OAUTH_TOKEN", token.access.as_ref()),
        ]
        .map(|(key, value)| (OsString::from(key), value.to_owned())),
    );
    if input.account.is_some() {
        env.push(("CLAUDE_CONFIG_DIR".into(), config_dir.into_os_string()));
    }
    let mut spec = LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(input.prompt.text().as_bytes().to_vec()),
        cwd: input.cwd.to_owned(),
        setup,
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
    };
    input
        .provenant
        .grant(&mut spec.profile.spec, &mut spec.setup, true);
    Ok(spec)
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
