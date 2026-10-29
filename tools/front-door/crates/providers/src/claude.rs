use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, package_root, subtree,
    uuid_v4,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{
    NetworkAccess, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
use provenant_os::clock::SystemClock;
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
use provenant_os::fs::{Directory, LockMode};
use provenant_records::CanonicalV1;
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;
pub struct Claude;

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &Claude,
    quota_source: Some(&Claude),
    display_name: "Claude",
    quota: provenant_quota::QuotaPolicy {
        adapter: "claude",
        timer_seconds: 300,
        panel_seconds: 120,
        probe_seconds: 14,
        low_power: true,
        solo: false,
    },
};

crate::adapter!(Claude, "claude", prepare);
const MODEL_ON_WIRE: bool = true;

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
    #[cfg(test)]
    pub(crate) fn for_account(mut self, key: &str, dir: &Path) -> Self {
        self.account = Some((key.into(), dir.to_owned()));
        self
    }

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
        {
            return Err(ProviderError::Malformed);
        }
        if oauth.expires_at_ms <= now_ms {
            return Err(ProviderError::CredentialExpired);
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
}

/// Placeholder working directory for host probes, never a caller's choice, so
/// a lane-writable directory cannot be named. Production executors are
/// `HostProbeExecutor`, which replaces it with a fresh private directory so no
/// project settings, hooks or instructions apply. Any other executor runs the
/// probe in `/`, a fallback that is root-owned and never lane-written.
pub(crate) const PROBE_CWD: &str = "/";

/// Keychain service suffix for a non-default `CLAUDE_CONFIG_DIR`: the first
/// eight hex digits of the directory path's digest.
pub fn keychain_suffix(config_dir: &[u8]) -> String {
    let digest = CanonicalV1::file_digest(config_dir).to_string();
    let hex = digest.strip_prefix("sha256:").unwrap_or(&digest);
    hex.get(..8).unwrap_or(hex).to_owned()
}

/// Task-scoped non-secret authority, derived from a validated registration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CredentialSource {
    File { path: std::path::PathBuf },
    Keychain { service: String, account: String },
}
#[derive(Clone, Debug, Serialize)]
pub struct CredentialAuthority {
    pub actor: String,
    pub source: CredentialSource,
    pub purpose: String,
    pub expires_unix_ms: u64,
}
impl CredentialAuthority {
    pub fn for_dispatch(
        actor: String,
        login: &Login<'_>,
        expires_unix_ms: u64,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            actor,
            source: credential_source(login)?,
            purpose: "lane dispatch".into(),
            expires_unix_ms,
        })
    }
    fn check(&self, source: &CredentialSource, now: u64) -> Result<(), ProviderError> {
        if self.actor.is_empty()
            || self.purpose != "lane dispatch"
            || self.expires_unix_ms <= now
            || &self.source != source
        {
            return Err(ProviderError::CredentialUnavailable);
        }
        Ok(())
    }
}
fn credential_source(login: &Login<'_>) -> Result<CredentialSource, ProviderError> {
    let path = login.config_dir.join(".credentials.json");
    let directory =
        provenant_os::fs::Directory::open_anchor(login.config_dir).map_err(|error| {
            if error.is_not_found() {
                ProviderError::CredentialNotFound
            } else {
                ProviderError::CredentialReadFailed("cannot open credential directory".into())
            }
        })?;
    match directory.metadata_at(OsStr::new(".credentials.json"), false) {
        Ok(meta) if meta.kind == provenant_os::fs::FileKind::Regular && meta.links == 1 => {
            return Ok(CredentialSource::File { path });
        }
        Ok(_) => {
            return Err(ProviderError::CredentialReadFailed(
                "credential file is not a single regular file".into(),
            ));
        }
        Err(error) if error.is_not_found() => {}
        Err(_) => {
            return Err(ProviderError::CredentialReadFailed(
                "cannot inspect credential file".into(),
            ));
        }
    }
    if !cfg!(target_os = "macos") {
        return Ok(CredentialSource::File { path });
    }
    Ok(keychain_source(login))
}
fn keychain_source(login: &Login<'_>) -> CredentialSource {
    let service = if login.config_dir == login.home.join(".claude") {
        KEYCHAIN_SERVICE.into()
    } else {
        format!(
            "{KEYCHAIN_SERVICE}-{}",
            keychain_suffix(login.config_dir.as_os_str().as_bytes())
        )
    };
    CredentialSource::Keychain {
        service,
        account: login.user.to_string_lossy().into_owned(),
    }
}

fn parse_stored_token(bytes: &[u8], now_ms: u64) -> Result<OauthToken, ProviderError> {
    OauthToken::parse(bytes, now_ms).map_err(|error| match error {
        ProviderError::CredentialExpired => error,
        _ => ProviderError::CredentialReadFailed("stored credential JSON is invalid".into()),
    })
}

/// Reads one identity's login token outside the sandbox. When it expires
/// within the refresh window, runs the CLI once at zero cost to refresh it,
/// then reads again and accepts a token with at least a minute left. The
/// refresh runs with every customisation off (safe mode, no hooks, no MCP
/// servers, no slash commands, no session file) in the executor's private
/// working directory. Host callers share an exclusive config-directory lock
/// across a second read, refresh and reread, so only one CLI can rotate its token.
/// Every subprocess is capped by the caller's `deadline_ms` on the
/// `now_ms` clock. The token is never logged, printed or persisted.
pub fn refresh_if_expiring(
    executor: &dyn Executor,
    login: &Login<'_>,
    now_ms: &dyn Fn() -> u64,
    deadline_ms: u64,
    authority: Option<&CredentialAuthority>,
) -> Result<OauthToken, ProviderError> {
    let source = credential_source(login)?;
    if let Some(authority) = authority {
        authority.check(&source, now_ms())?;
    }
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
        let timeout = budget(Duration::from_secs(10))?;
        if authority.is_some() && credential_source(login)? != source {
            return Err(ProviderError::CredentialUnavailable);
        }
        if let Some(authority) = authority {
            authority.check(&source, now_ms())?;
        }
        if let CredentialSource::File { path } = &source {
            let directory = provenant_os::fs::Directory::open_anchor(
                path.parent().ok_or(ProviderError::CredentialUnavailable)?,
            )
            .map_err(|_| {
                ProviderError::CredentialReadFailed("cannot open credential directory".into())
            })?;
            let bytes = directory
                .read_bounded(OsStr::new(".credentials.json"), 65536)
                .map_err(|error| {
                    if error.is_not_found() {
                        ProviderError::CredentialNotFound
                    } else {
                        ProviderError::CredentialReadFailed("cannot read credential file".into())
                    }
                })?;
            return parse_stored_token(&bytes, now_ms());
        }
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
        let mut options = ExecOptions::new(&argv, Path::new(PROBE_CWD), timeout);
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
        options.stderr = StreamPolicy::Discard;
        let out = executor.execute(options).map_err(|_| {
            ProviderError::CredentialReadFailed("cannot execute keychain read".into())
        })?;
        if out.timed_out {
            return Err(ProviderError::CredentialReadFailed(
                "keychain read timed out".into(),
            ));
        }
        if out.stdout.truncated
            || out.stdout.read_failed
            || out.drain_truncated
            || out.cancelled
            || out.overflow_stopped
        {
            return Err(ProviderError::CredentialReadFailed(
                "keychain read was incomplete".into(),
            ));
        }
        match out.termination {
            Termination::Exit(0) => parse_stored_token(&out.stdout.bytes, now_ms()),
            // security's errSecItemNotFound exit status.
            Termination::Exit(44) => Err(ProviderError::CredentialNotFound),
            Termination::Exit(code) => Err(ProviderError::CredentialReadFailed(format!(
                "security exited with code {code}"
            ))),
            Termination::Signal(signal) => Err(ProviderError::CredentialReadFailed(format!(
                "security terminated by signal {signal}"
            ))),
        }
    };
    match read() {
        Ok(token) if !token.needs_refresh(now_ms()) => return Ok(token),
        // An expired token was read successfully, so refreshing is safe: no
        // running lane can still be using it. Read failures never refresh.
        Ok(_) | Err(ProviderError::CredentialExpired) => {}
        Err(error) => return Err(error),
    }
    // Stable inode, never removed: reread storage under this cross-process
    // lock in case another caller already refreshed the token.
    let clock = SystemClock::new();
    // A future deadline preserves sub-second waits despite WaitBudget's
    // one-second minimum for its duration constructor.
    let wait_micros = budget(Duration::from_secs(60))?.as_micros() as u64;
    let deadline = clock
        .mono_now()
        .checked_add(DurationMicros::from_micros(wait_micros))
        .map_err(|_| ProviderError::CredentialUnavailable)?;
    let wait = WaitBudget::nonblocking(deadline);
    let directory = Directory::open_anchor(login.config_dir)
        .map_err(|_| ProviderError::CredentialUnavailable)?;
    let _refresh_lock = directory
        .lock(
            OsStr::new("provenant-oauth-refresh.lock"),
            LockMode::Exclusive,
            wait,
            &CancelToken::new(),
            &clock,
        )
        .map_err(|_| ProviderError::CredentialUnavailable)?;
    match read() {
        Ok(token) if !token.needs_refresh(now_ms()) => return Ok(token),
        Ok(_) | Err(ProviderError::CredentialExpired) => {}
        Err(error) => return Err(error),
    }
    if authority.is_some() && credential_source(login)? != source {
        return Err(ProviderError::CredentialUnavailable);
    }
    if let Some(authority) = authority {
        authority.check(&source, now_ms())?;
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
    let mut options = ExecOptions::new(
        &argv,
        Path::new(PROBE_CWD),
        budget(Duration::from_secs(60))?,
    );
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
    refresh_account(executor, login, account, now_ms, deadline_ms, None)
}
pub fn refresh_for_account_authorised(
    executor: &dyn Executor,
    login: &Login<'_>,
    account: &crate::identity::RegisteredAccount,
    now_ms: &dyn Fn() -> u64,
    deadline_ms: u64,
    authority: &CredentialAuthority,
) -> Result<OauthToken, ProviderError> {
    authority.check(&credential_source(login)?, now_ms())?;
    refresh_account(
        executor,
        login,
        account,
        now_ms,
        deadline_ms,
        Some(authority),
    )
}
fn refresh_account(
    executor: &dyn Executor,
    login: &Login<'_>,
    account: &crate::identity::RegisteredAccount,
    now_ms: &dyn Fn() -> u64,
    deadline_ms: u64,
    authority: Option<&CredentialAuthority>,
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
            Path::new(PROBE_CWD),
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
    let mut token = refresh_if_expiring(executor, login, now_ms, deadline_ms, authority)?;
    check()?;
    token.account = Some((account.key().into(), account.directory().to_owned()));
    Ok(token)
}

/// Claude uses the selected config directory. The host reads its login token
/// and passes it through an inherited pipe, so the keychain stays
/// unreadable. Both lane types deny keychain services and the security
/// executable and ask Claude to scrub subprocess credentials. Writers use
/// private configuration; read-only lanes retain their transcript location.
/// Provenant and its hook helpers are added explicitly for lane MCP use.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    // No credential path is granted: the token arrives through a pipe.
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
    let shell = access == Access::WorktreeWrite;
    let web = plan.network == NetworkAccess::Open;
    let (mode, tools) = match access {
        Access::ReadOnly => ("dontAsk", "Read,Grep,Glob,Bash"),
        Access::WorktreeWrite => ("default", "Read,Grep,Glob,Edit,Write,Bash"),
    };
    let tools = if web {
        format!("{tools},WebFetch,WebSearch")
    } else {
        tools.to_owned()
    };
    argv.extend(
        [
            "--permission-mode",
            mode,
            "--tools",
            &tools,
            "--model",
            input.route.model.as_str(),
        ]
        .map(OsString::from),
    );
    if let Some(effort) = &input.route.effort {
        argv.extend(["--effort".into(), effort.into()]);
    }
    if !shell && let Some(read_only) = &plan.read_only {
        // The following MCP flags terminate --add-dir's variadic paths.
        add_git_dirs(&mut argv, read_only.git_read_paths(), input.cwd);
    }
    if access == Access::ReadOnly {
        // dontAsk mode refuses reads outside the working directory and its
        // added directories, even where the seatbelt profile would allow them.
        // A root that holds the working directory still reaches beyond it.
        for root in &plan.read_roots {
            if !root.path.starts_with(input.cwd) {
                argv.extend(["--add-dir".into(), root.path.as_os_str().to_owned()]);
            }
        }
    }

    let config_dir = input.account.map_or_else(
        || input.home.join(".claude"),
        |account| account.directory().to_owned(),
    );
    let project = config_dir.join("projects").join(project_key(input.cwd));
    let transcript_dir = project.join(&session_id);
    let package = package_root(input.executable)?;
    let scratch = &plan.scratch.path;
    // Scrub mode forces default permissions, regardless of --permission-mode.
    let allowed_tools = if shell {
        "Read,Grep,Glob,Edit,Write,Bash,mcp__fabric__*"
    } else {
        "Bash,mcp__fabric__*"
    };
    let allowed_tools = if web {
        format!("{allowed_tools},WebFetch,WebSearch")
    } else {
        allowed_tools.to_owned()
    };
    let mut setup = input.provenant.claude(scratch, &mut argv, &allowed_tools)?;

    let toolchain = shell || web;
    let path = if toolchain {
        format!("{}:/usr/bin:/bin", input.home.join(".cargo/bin").display())
    } else {
        "/usr/bin:/bin".into()
    };
    let mut env = base_env(input, scratch, &path);
    input
        .provenant
        .environment(scratch, &mut env, &mut setup, true)?;
    env.extend(
        [
            ("CLAUDE_CODE_TMPDIR", scratch.as_os_str()),
            ("DISABLE_AUTOUPDATER", "1".as_ref()),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1".as_ref()),
        ]
        .map(|(key, value)| (OsString::from(key), value.to_owned())),
    );
    if !shell {
        setup.push(SetupStep::CreateDir(transcript_dir.clone()));
    }
    if shell {
        let config = plan
            .private_state
            .as_ref()
            .ok_or(ProviderError::UnsupportedPolicy)?
            .path
            .join("claude-config");
        env.push(("CLAUDE_CONFIG_DIR".into(), config.clone().into_os_string()));
        setup.push(SetupStep::CreateDir(config));
    } else if input.account.is_some() {
        env.push(("CLAUDE_CONFIG_DIR".into(), config_dir.into_os_string()));
    }
    let mut runtime_reads = vec![subtree(package)];
    let mut tool_exec_roots = ["/bin", "/usr/bin", "/usr/libexec"].map(subtree).to_vec();
    let tool_writes = Vec::new();
    env.extend([
        ("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]);
    // The git wrapper script; the scrub drops GIT_CONFIG_GLOBAL from Bash.
    tool_exec_roots.push(subtree(scratch.join("bin")));
    // Use the same read-only toolchain, public TLS configuration and CLT
    // selection as Codex. Access still controls private caches and writes.
    if toolchain {
        // Network access must not make Rust an installation prerequisite.
        // Every subtree grant needs an existing directory at anchoring time.
        let rust_installed = [".cargo/bin", ".rustup/toolchains"]
            .iter()
            .all(|path| input.home.join(path).is_dir());
        if shell || rust_installed {
            crate::launch::rust_tools(
                input,
                access,
                &mut runtime_reads,
                &mut tool_exec_roots,
                &mut env,
                &mut setup,
            );
        } else {
            crate::launch::system_tools(access, &mut runtime_reads, &mut tool_exec_roots, &mut env);
            tool_exec_roots.push(subtree(input.cwd));
        }
        crate::launch::node_tools(
            input.home,
            access,
            crate::launch::lane_cache(input),
            &mut runtime_reads,
            &mut tool_exec_roots,
            &mut env,
        );
    }
    let mut spec = LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(input.prompt.text().as_bytes().to_vec()),
        secret_input: Some(provenant_os::exec::SecretInput {
            descriptor_env: "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR".into(),
            bytes: token.access.as_bytes().to_vec(),
        }),
        cwd: input.cwd.to_owned(),
        setup,
        git_wrapper: Default::default(),
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![exact(input.executable)],
                allow_process_fork: true,
                deny_keychain_access: true,
                tool_exec_roots,
                tool_writes,
                // Never the user's .claude.json, which can hold MCP secrets.
                runtime_reads,
                state_writes: if shell {
                    Vec::new()
                } else {
                    vec![
                        exact(project.join(format!("{session_id}.jsonl"))),
                        subtree(transcript_dir),
                    ]
                },
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
        .grant(&mut spec.profile.spec, &mut spec.setup, true);
    Ok(spec)
}

pub(super) fn add_git_dirs<'a>(
    argv: &mut Vec<OsString>,
    paths: impl IntoIterator<Item = &'a Path>,
    cwd: &Path,
) {
    for path in paths {
        if !path.starts_with(cwd) {
            argv.extend(["--add-dir".into(), path.as_os_str().to_owned()]);
        }
    }
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
        #[serde(default)]
        api_error_status: Option<u16>,
    },
    #[serde(rename = "result")]
    Result {
        session_id: String,
        subtype: String,
        is_error: bool,
        result: Option<String>,
        #[serde(default)]
        api_error_status: Option<u16>,
        // The result carries the run total; assistant messages only carry
        // streaming placeholders.
        #[serde(default)]
        usage: Option<Usage>,
        /// Why the final turn ended; `end_turn` is a finished one.
        #[serde(default)]
        stop_reason: Option<String>,
        /// Why the session ended; `completed` is a finished one.
        #[serde(default)]
        terminal_reason: Option<String>,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Message {
    model: String,
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
    // Cursor shares this result shape and spells the counts in camelCase.
    #[serde(alias = "inputTokens")]
    input_tokens: u64,
    #[serde(alias = "outputTokens")]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    /// Cache writes split by time-to-live bucket.
    #[serde(default)]
    cache_creation: Option<CacheCreation>,
}
#[derive(Deserialize)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_1h_input_tokens: Option<u64>,
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
            api_error_status,
        } if message.role == "assistant" => {
            let mut events = vec![E::Session(session_id)];
            if error.is_none() && !is_api_error_message && message.model != "<synthetic>" {
                events.push(E::Model(message.model));
            }
            let failed = error.is_some() || is_api_error_message;
            if failed {
                let overload = api_error_status == Some(529)
                    || matches!(error.as_deref(), Some("overloaded" | "overloaded_error"));
                events.push(if overload {
                    let message = message
                        .content
                        .iter()
                        .find_map(|content| match content {
                            Content::Text { text } if !text.is_empty() => Some(text.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| "provider overloaded".into());
                    E::ModelOverloaded { message }
                } else if error.as_deref() == Some("authentication_failed")
                    || api_error_status == Some(401)
                {
                    E::AuthRequired
                } else {
                    E::Error
                });
            }
            // The result event repeats the final answer, so assistant text is
            // kept only as error evidence; otherwise every answer lands twice.
            if failed {
                for content in message.content {
                    if let Content::Text { text } = content {
                        events.push(E::Text(text));
                    }
                }
            }
            events
        }
        Event::Result {
            session_id,
            subtype,
            is_error,
            result,
            api_error_status,
            usage,
            stop_reason,
            terminal_reason,
        } => {
            let mut events = vec![E::Session(session_id)];
            if let Some(usage) = usage {
                events.push(E::Usage {
                    input: usage.input_tokens,
                    output: usage.output_tokens,
                    cache_read: usage.cache_read_input_tokens,
                    cache_write: usage.cache_creation_input_tokens,
                    cache_write_1h: usage
                        .cache_creation
                        .and_then(|split| split.ephemeral_1h_input_tokens),
                });
            }
            if is_error || subtype != "success" {
                if api_error_status == Some(401) {
                    events.push(E::AuthRequired);
                } else if api_error_status == Some(529) {
                    events.push(E::ModelOverloaded {
                        message: result
                            .filter(|message| !message.is_empty())
                            .unwrap_or_else(|| "provider overloaded (HTTP 529)".into()),
                    });
                } else {
                    events.push(E::Error);
                }
            } else {
                if let Some(result) = result {
                    events.push(E::Text(result));
                }
                // A success that stopped for another reason than finishing its
                // turn is still a partial piece of work.
                let early = stop_reason
                    .filter(|reason| !matches!(reason.as_str(), "end_turn" | "stop_sequence"))
                    .map(|reason| format!("stop_reason {reason}"))
                    .or_else(|| {
                        terminal_reason
                            .filter(|reason| reason != "completed")
                            .map(|reason| format!("terminal_reason {reason}"))
                    });
                if let Some(reason) = early {
                    events.push(E::EarlyStop { reason });
                }
                events.push(E::Terminal);
            }
            events
        }
        _ => vec![],
    })
}

#[cfg(test)]
mod overload_tests {
    use super::*;

    #[test]
    fn result_529_is_typed_overload_with_provider_reason() {
        let events = decode(br#"{"type":"result","session_id":"s","subtype":"error_during_execution","is_error":true,"api_error_status":529,"result":"Overloaded"}"#).unwrap();
        assert!(events.contains(&E::ModelOverloaded {
            message: "Overloaded".into()
        }));
        let events = decode(br#"{"type":"result","session_id":"s","subtype":"error_during_execution","is_error":true,"api_error_status":500,"result":"Overloaded"}"#).unwrap();
        assert!(events.contains(&E::Error));
        let events = decode(br#"{"type":"result","session_id":"s","subtype":"success","is_error":false,"api_error_status":529,"result":"ok"}"#).unwrap();
        assert!(events.contains(&E::Terminal));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, E::ModelOverloaded { .. }))
        );
    }

    #[test]
    fn assistant_typed_overload_keeps_reason_and_session() {
        for error in ["overloaded", "overloaded_error"] {
            let raw = format!(
                r#"{{"type":"assistant","session_id":"s","error":"{error}","is_api_error_message":true,"message":{{"role":"assistant","model":"<synthetic>","content":[{{"type":"text","text":"service unavailable"}}]}}}}"#
            );
            let events = decode(raw.as_bytes()).unwrap();
            assert!(events.contains(&E::Session("s".into())));
            assert!(events.contains(&E::ModelOverloaded {
                message: "service unavailable".into()
            }));
        }
        let raw = br#"{"type":"assistant","session_id":"s","is_api_error_message":true,"api_error_status":529,"message":{"role":"assistant","model":"<synthetic>","content":[]}}"#;
        assert!(
            decode(raw)
                .unwrap()
                .iter()
                .any(|event| matches!(event, E::ModelOverloaded { .. }))
        );
    }
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
