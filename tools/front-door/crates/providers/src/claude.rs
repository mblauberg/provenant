use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, package_grant, subtree,
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

/// Live token usage for a token budget. Each assistant message carries the
/// usage of its API call, repeated on every content block of that message, so
/// a call is keyed by its message id and counted once. Output counts are
/// streaming placeholders until the result, so a live total may undercount
/// output; it counts fresh input and cache writes in full. Each call also
/// reports the context it sent (input, cached reads and cache writes) for the
/// context handoff threshold.
pub struct LiveUsage;
impl provenant_os::capture::FrameDecoder for LiveUsage {
    fn decode(
        &self,
        frame: &[u8],
    ) -> Result<provenant_os::capture::FrameEvent, provenant_os::capture::DecodeFailure> {
        use provenant_os::capture::FrameEvent;
        Ok(
            live_usage(frame).map_or(FrameEvent::Progress, |(key, tokens, context)| {
                FrameEvent::Usage {
                    key,
                    tokens,
                    context: Some(context),
                }
            }),
        )
    }
    fn reports_usage(&self) -> bool {
        true
    }
}
/// The key, fresh token total and context size of an assistant frame, or
/// `None` for any other frame or one that does not parse. A bad frame never
/// ends an attempt.
fn live_usage(frame: &[u8]) -> Option<(u64, u64, u64)> {
    use std::hash::{Hash, Hasher};
    if !frame.windows(7).any(|window| window == b"\"usage\"") {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(frame).ok()?;
    if value.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let message = value.get("message")?;
    let usage = message.get("usage")?;
    let count = |name: &str| usage.get(name).and_then(serde_json::Value::as_u64);
    let tokens = [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
    ]
    .into_iter()
    .filter_map(count)
    .fold(0_u64, u64::saturating_add);
    let context = [
        "input_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ]
    .into_iter()
    .filter_map(count)
    .fold(0_u64, u64::saturating_add);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    message.get("id")?.as_str()?.hash(&mut hasher);
    Some((hasher.finish(), tokens, context))
}

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &Claude,
    quota_source: Some(&Claude),
    stdout_decoder: Some(&LiveUsage),
    stderr_decoder: None,
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

/// The owner's long-lived lane token (`claude setup-token`) for one account,
/// kept in that account's own config directory. It belongs to its own grant,
/// so the host renewing the account login never revokes it; lanes prefer it.
pub const LANE_TOKEN_FILE: &str = "provenant-lane-token";

/// A short-lived OAuth access token for the default identity, read by the host
/// outside the sandbox. It is never logged, persisted or printed: `Debug` is
/// redacted and the refresh token is never retained.
pub struct OauthToken {
    pub(crate) access: String,
    expires_at_ms: u64,
    account: Option<(String, std::path::PathBuf)>,
    pub(crate) lane_token: bool,
}

impl std::fmt::Debug for OauthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OauthToken(redacted)")
    }
}

impl OauthToken {
    /// True when this credential is a registered long-lived lane token,
    /// rather than a rotating login access token.
    pub fn is_lane_token(&self) -> bool {
        self.lane_token
    }
    /// Construct an in-memory token for cross-crate fixtures.
    #[cfg(feature = "fixture")]
    pub fn fixture() -> Result<Self, ProviderError> {
        Self::parse(br#"{"claudeAiOauth":{"accessToken":"fixture","refreshToken":"fixture","expiresAt":4000000000000}}"#, 1)
    }

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
            lane_token: false,
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

/// Reads the account's long-lived lane token when the owner registered one.
/// It must be a single regular file that only its owner can read.
pub(crate) fn lane_token(login: &Login<'_>) -> Result<Option<OauthToken>, ProviderError> {
    let directory = Directory::open_anchor(login.config_dir)
        .map_err(|_| ProviderError::CredentialUnavailable)?;
    let name = OsStr::new(LANE_TOKEN_FILE);
    let mut file = match directory.open_read(name) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error)
            if error.is_symlink()
                || matches!(error, provenant_os::fs::FsError::NotRegular { .. }) =>
        {
            return Err(ProviderError::CredentialReadFailed(format!(
                "{LANE_TOKEN_FILE} must be a single regular file with mode 0600"
            )));
        }
        Err(_) => {
            return Err(ProviderError::CredentialReadFailed(format!(
                "cannot inspect {LANE_TOKEN_FILE}"
            )));
        }
    };
    let meta = file.metadata().map_err(|_| {
        ProviderError::CredentialReadFailed(format!("cannot inspect {LANE_TOKEN_FILE}"))
    })?;
    if meta.kind != provenant_os::fs::FileKind::Regular
        || meta.uid != provenant_os::fs::effective_uid()
        || meta.links != 1
        || meta.mode & 0o077 != 0
    {
        return Err(ProviderError::CredentialReadFailed(format!(
            "{LANE_TOKEN_FILE} must be a single regular file with mode 0600"
        )));
    }
    let bytes = file.read_bounded(4096).map_err(|_| {
        ProviderError::CredentialReadFailed(format!("cannot read {LANE_TOKEN_FILE}"))
    })?;
    let access = std::str::from_utf8(bytes.trim_ascii()).map_err(|_| ProviderError::Malformed)?;
    if access.is_empty() || access.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ProviderError::Malformed);
    }
    Ok(Some(OauthToken {
        access: access.into(),
        // The owner renews it yearly; the host never refreshes it.
        expires_at_ms: u64::MAX,
        account: None,
        lane_token: true,
    }))
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
    let mut token = match lane_token(login)? {
        Some(token) => token,
        None => refresh_if_expiring(executor, login, now_ms, deadline_ms, authority)?,
    };
    check()?;
    token.account = Some((account.key().into(), account.directory().to_owned()));
    Ok(token)
}

/// The host reads the selected Claude login token
/// and passes it through an inherited pipe, so the keychain stays
/// unreadable. Both lane types deny keychain services and the security
/// executable. Both lane types use attempt-private configuration and transcripts.
/// Provenant and its hook helpers are added explicitly for lane MCP use.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    let mut tool_requirements = crate::launch::ToolRequirements::default();
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
        Access::ReadOnly => ("dontAsk", "Read,Grep,Glob,Bash,Skill"),
        Access::WorktreeWrite => ("bypassPermissions", "default"),
    };
    let tools = if web && !shell {
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
        // The handoff is beside the pinned source view, outside the cwd.
        // Bash is pre-approved below; native file tools also need this root.
        if let Some(private) = &plan.private_state {
            argv.extend(["--add-dir".into(), private.path.as_os_str().to_owned()]);
        }
        // dontAsk mode refuses reads outside the working directory and its
        // added directories, even where the seatbelt profile would allow them.
        // A root that holds the working directory still reaches beyond it.
        for root in &plan.read_roots {
            if !root.path.starts_with(input.cwd) && !input.images.contains(&root.path) {
                argv.extend(["--add-dir".into(), root.path.as_os_str().to_owned()]);
            }
        }
    }

    let mut package_omissions = Vec::new();
    let package = package_grant(input.executable, &mut package_omissions);
    let scratch = &plan.scratch.path;
    // Filesystem confinement owns writer path access; native permission prompts
    // would refuse tools that cannot ask for approval in a headless lane.
    let allowed_tools = if web {
        "Bash,Skill,mcp__fabric__*,WebFetch,WebSearch"
    } else {
        "Bash,Skill,mcp__fabric__*"
    };
    let image_rules = if shell {
        Vec::new()
    } else {
        input
            .images
            .iter()
            .map(|image| {
                let path = image.to_str().ok_or(ProviderError::InvalidPath)?;
                if !image.is_absolute() {
                    return Err(ProviderError::InvalidPath);
                }
                // Read rules use gitignore patterns; escape metacharacters in filenames.
                let escaped: String = path
                    .chars()
                    .flat_map(|c| {
                        if matches!(c, '*' | '?' | '[' | ']' | '\\' | ' ') {
                            vec!['\\', c]
                        } else {
                            vec![c]
                        }
                    })
                    .collect();
                Ok(format!("Read(/{escaped})"))
            })
            .collect::<Result<Vec<_>, ProviderError>>()?
    };
    let mut setup = input.provenant.claude(
        scratch,
        &mut argv,
        (!shell).then_some(allowed_tools),
        &image_rules,
    )?;

    let toolchain = shell || web;
    let path = crate::launch::tool_search_path(input.home, toolchain);
    let mut env = base_env(input, scratch, path);
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
    let staged_skills = input.stage_user_skills();
    let user_skills = staged_skills.root.clone();
    let skill_reads = user_skills.iter().map(subtree).collect::<Vec<_>>();
    let omitted_discovery = package_omissions;
    let discover_skills = user_skills.is_some();
    let private = &plan
        .private_state
        .as_ref()
        .ok_or(ProviderError::UnsupportedPolicy)?
        .path;
    let config = private.join("claude-config");
    env.push(("CLAUDE_CONFIG_DIR".into(), config.clone().into_os_string()));
    setup.push(SetupStep::CreateDir(config.clone()));
    if let Some(user_skills) = &user_skills {
        setup.push(SetupStep::Symlink {
            link: config.join("skills"),
            target: user_skills.clone(),
        });
    }
    let mut runtime_reads = vec![package.clone()];
    if discover_skills {
        for grant in &skill_reads {
            // Staging produces a directory subtree for Claude's workspace.
            argv.extend(["--add-dir".into(), grant.path.as_os_str().to_owned()]);
        }
        runtime_reads.extend(skill_reads);
    }
    let mut tool_exec_roots = ["/bin", "/usr/bin", "/usr/libexec"].map(subtree).to_vec();
    let tool_writes = Vec::new();
    env.extend([
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]);
    // The git wrapper script remains available to Bash.
    tool_exec_roots.push(subtree(scratch.join("bin")));
    // Use the same read-only toolchain, public TLS configuration and CLT
    // selection as Codex. Access still controls private caches and writes.
    if toolchain {
        crate::launch::rust_tools(input, access, &mut tool_requirements, &mut env, &mut setup);
        crate::launch::node_tools(
            input.home,
            access,
            crate::launch::lane_cache(input),
            &mut tool_requirements,
            &mut env,
        );
    }
    let mut prompt = input.prompt.text().to_owned();
    if !input.images.is_empty() {
        prompt.push_str("\n\nAttached images:\n");
        for image in input.images {
            prompt.push_str(&format!("- {}\n", image.display()));
        }
    }
    let mut spec = LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(prompt.into_bytes()),
        secret_input: Some(provenant_os::exec::SecretInput {
            descriptor_env: "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR".into(),
            bytes: token.access.as_bytes().to_vec(),
        }),
        cwd: input.cwd.to_owned(),
        setup,
        omitted_discovery,
        user_skills: staged_skills,
        tool_requirements,
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
    crate::launch::prepare_tool_requirements(input, &mut spec);
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
    /// Tool results come back as user events.
    #[serde(rename = "user")]
    User {
        #[serde(default)]
        parent_tool_use_id: Option<String>,
        #[serde(default)]
        message: Option<UserMessage>,
    },
    #[serde(rename = "result")]
    Result {
        session_id: String,
        subtype: String,
        is_error: bool,
        result: Option<String>,
        #[serde(default)]
        permission_denials: Vec<serde_json::Value>,
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
/// Any other shape is kept as an unknown message, never a malformed stream.
#[derive(Deserialize)]
#[serde(untagged)]
enum UserMessage {
    Message { content: UserContent },
    Other(serde::de::IgnoredAny),
}
/// Only the block types are read; tool output is never kept.
#[derive(Deserialize)]
#[serde(untagged)]
enum UserContent {
    // Only the shape matters: a string here is a plain text message.
    Text(#[allow(dead_code)] String),
    Blocks(Vec<UserBlock>),
    Other(serde::de::IgnoredAny),
}
#[derive(Deserialize)]
struct UserBlock {
    #[serde(rename = "type")]
    kind: String,
}
/// A user message of text alone: a string, or only `text` blocks.
fn text_only(message: &UserMessage) -> bool {
    match message {
        UserMessage::Message {
            content: UserContent::Text(_),
        } => true,
        UserMessage::Message {
            content: UserContent::Blocks(blocks),
        } => !blocks.is_empty() && blocks.iter().all(|block| block.kind == "text"),
        _ => false,
    }
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
    ToolUse,
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
/// A rate limit holds the account only when its text names a plan allowance:
/// a usage, session, five-hour, daily, weekly or monthly limit. A reset alone
/// is not enough, since a per-minute limit also names one. Any other 429 is
/// transient, retried like capacity rather than recorded as a refusal that
/// would hold the account for an hour.
fn rate_limited(text: Option<String>, fallback: &str) -> E {
    let usage = text.as_deref().is_some_and(|text| {
        let text = text.to_ascii_lowercase();
        [
            "usage limit",
            "session limit",
            "weekly limit",
            "hour limit",
            "daily limit",
            "monthly limit",
        ]
        .iter()
        .any(|marker| text.contains(marker))
    });
    let message = text.unwrap_or_else(|| fallback.into());
    if usage {
        E::QuotaLimited {
            message,
            pool: None,
            resets_in_seconds: None,
        }
    } else {
        E::ModelOverloaded { message }
    }
}

fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    let event = wire::<Event>(bytes).map_err(|error| {
        // Startup failures can use a result shape without session metadata.
        // Preserve the provider's diagnostic while retaining malformed status.
        let Ok(value) = wire::<serde_json::Value>(bytes) else {
            return error;
        };
        if value.get("type").and_then(|v| v.as_str()) != Some("result") {
            return error;
        }
        let message = value
            .get("errors")
            .and_then(|v| v.as_array())
            .map(|errors| {
                errors
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|message| !message.is_empty())
            .or_else(|| {
                value
                    .get("result")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            });
        match message.filter(|message| !message.is_empty()) {
            Some(message) => ProviderError::MalformedResult(
                provenant_confinement::redact::bounded_line(&message, 2048),
            ),
            None => error,
        }
    })?;
    Ok(match event {
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
            // Ahead of this message's own text, so that text still counts as the
            // last thing said; only later work invalidates the fallback.
            if message
                .content
                .iter()
                .any(|content| matches!(content, Content::ToolUse))
            {
                events.push(E::ToolActivity);
            }
            if error.is_none() && !is_api_error_message && message.model != "<synthetic>" {
                let text: String = message
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        Content::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if !text.trim().is_empty() {
                    events.push(E::AssistantText(text.clone()));
                }
                if !text.is_empty() {
                    events.push(E::AnswerModel {
                        model: message.model.clone(),
                        digest: *provenant_records::CanonicalV1::file_digest(text.as_bytes())
                            .bytes(),
                    });
                }
            }
            let failed = error.is_some() || is_api_error_message;
            if failed {
                let overload = api_error_status == Some(529)
                    || matches!(error.as_deref(), Some("overloaded" | "overloaded_error"));
                let text = message.content.iter().find_map(|content| match content {
                    Content::Text { text } if !text.is_empty() => Some(text.clone()),
                    _ => None,
                });
                let limited =
                    error.as_deref() == Some("rate_limit") || api_error_status == Some(429);
                events.push(if overload {
                    E::ModelOverloaded {
                        message: text.unwrap_or_else(|| "provider overloaded".into()),
                    }
                } else if limited {
                    rate_limited(text, "provider rate limited")
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
        // A plain text user message, such as a background task notification,
        // starts a turn but is not tool work.
        Event::User {
            parent_tool_use_id: None,
            message,
        } if message.as_ref().is_some_and(text_only) => {
            vec![]
        }
        Event::User {
            parent_tool_use_id: None,
            ..
        } => vec![E::ToolActivity],
        Event::Result {
            session_id,
            subtype,
            is_error,
            result,
            permission_denials,
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
            // Claude reports denials accumulated during the session, but does not
            // say whether any denial ended the work. Record them without turning
            // a potentially recovered denial into a failed lane.
            if let Some(denied) = permission_denials.first() {
                let name = denied
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let action = if (1..=64).contains(&name.len())
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    name
                } else {
                    "unknown action"
                };
                events.push(E::DeniedNote {
                    action: action.into(),
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
                } else if api_error_status == Some(429) {
                    events.push(rate_limited(
                        result.filter(|message| !message.is_empty()),
                        "provider rate limited (HTTP 429)",
                    ));
                } else {
                    if let Some(result) = result.filter(|message| !message.is_empty()) {
                        events.push(E::Text(result));
                    }
                    events.push(E::Error);
                }
            } else {
                // A later turn, such as a reply to a late notification, starts
                // on its own line and keeps its own boundary.
                if let Some(result) = result {
                    events.push(E::MessageStart);
                    events.push(E::Text(result));
                }
                events.push(E::TurnEnd {
                    tool_activity: false,
                });
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

    fn finish_text(input: &str) -> Vec<E> {
        let mut stream = StreamState::new("claude", decode);
        let mut events = stream.observe(input.as_bytes()).unwrap();
        let outcome = stream
            .finish(crate::ExitObservation {
                code: Some(0),
                timed_out: false,
            })
            .unwrap();
        events.extend(outcome.final_events().iter().cloned());
        events
    }

    #[test]
    fn permission_denial_with_answer_is_a_note() {
        let events = decode(br#"{"type":"result","session_id":"s","subtype":"success","is_error":false,"result":"done","permission_denials":[{"tool_name":"Read"}]}"#).unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, E::DeniedNote { action } if action == "Read"))
        );
        assert!(events.iter().any(|event| matches!(event, E::Terminal)));
        assert!(!events.iter().any(|event| matches!(event, E::Denied { .. })));
    }

    #[test]
    fn permission_denial_without_answer_is_a_note() {
        let events = decode(br#"{"type":"result","session_id":"s","subtype":"success","is_error":false,"result":"","permission_denials":[{"tool_name":"Read"}]}"#).unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, E::DeniedNote { action } if action == "Read"))
        );
        assert!(events.iter().any(|event| matches!(event, E::Terminal)));
        assert!(!events.iter().any(|event| matches!(event, E::Denied { .. })));
    }

    #[test]
    fn empty_result_falls_back_to_the_last_assistant_text_with_a_marker() {
        let input = concat!(
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{\"type\":\"text\",\"text\":\"first\"}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{\"type\":\"text\",\"text\":\"committed the work\"},{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"Bash\",\"input\":{}}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n",
            "{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\"}\n"
        );
        let events = finish_text(input);
        assert!(events.contains(&E::Text("committed the work".into())));
        assert!(events.contains(&E::ResultFallback));
        assert!(!events.iter().any(|e| matches!(e, E::AssistantText(_))));
    }

    #[test]
    fn each_result_ends_a_turn_and_a_notification_is_not_tool_work() {
        let tool_use = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"Bash\",\"input\":{}}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n";
        let tool_result = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t\",\"content\":\"ok\"}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n";
        let notification = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"<task-notification>done</task-notification>\"}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n";
        let prompt = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"continue\"},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n";
        let odd = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":7},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n";
        let result = |text: &str| {
            format!(
                "{{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"{text}\"}}\n"
            )
        };
        let events = finish_text(&format!(
            "{tool_use}{tool_result}{}{notification}{prompt}{}{odd}{}",
            result("first"),
            result("second"),
            result("third")
        ));
        let turns: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                E::TurnEnd { tool_activity } => Some(*tool_activity),
                _ => None,
            })
            .collect();
        // An unknown user message shape still counts as work, never as malformed.
        assert_eq!(turns, [true, false, true]);
        let order: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, E::MessageStart | E::Text(_) | E::TurnEnd { .. }))
            .cloned()
            .collect();
        assert_eq!(
            order[..3],
            [
                E::MessageStart,
                E::Text("first".into()),
                E::TurnEnd {
                    tool_activity: true
                }
            ]
        );
    }

    #[test]
    fn tool_work_in_a_turn_that_never_finishes_ends_a_turn_and_voids_the_handoff() {
        let tool_use = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"Bash\",\"input\":{}}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n";
        let result = |text: &str| {
            format!(
                "{{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"{text}\"}}\n"
            )
        };
        let fence = "```handoff\\n{\\\"schema_version\\\":2,\\\"status\\\":\\\"done\\\",\\\"note\\\":\\\"n\\\",\\\"remaining\\\":[],\\\"decisions\\\":[],\\\"verify\\\":[]}\\n```";
        let error = "{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"error_max_turns\",\"is_error\":true,\"result\":\"\"}\n";
        let finished = format!("{tool_use}{}{}", result(fence), result("noted"));
        let turns = |events: &[E]| -> Vec<bool> {
            events
                .iter()
                .filter_map(|event| match event {
                    E::TurnEnd { tool_activity } => Some(*tool_activity),
                    _ => None,
                })
                .collect()
        };
        // No tool work after the last result: no extra turn.
        assert_eq!(turns(&finish_text(&finished)), [true, false]);
        // Unfinished tool work, by error result or by a stream that just stops.
        for tail in [format!("{tool_use}{error}"), tool_use.to_owned()] {
            let events = finish_text(&format!("{finished}{tail}"));
            assert_eq!(turns(&events), [true, false, true]);
            let mut text = String::new();
            let mut ends = Vec::new();
            for event in &events {
                match event {
                    E::Text(piece) => text.push_str(piece),
                    E::TurnEnd { tool_activity } => {
                        ends.push(provenant_records::handoff::TurnEnd {
                            at: text.len(),
                            tool_activity: *tool_activity,
                        })
                    }
                    _ => {}
                }
            }
            assert_eq!(
                provenant_records::handoff::Handoff::from_turns(&text, &ends),
                Ok(None)
            );
        }
    }

    #[test]
    fn text_followed_by_later_tool_work_is_not_the_fallback() {
        let text = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{\"type\":\"text\",\"text\":\"I will stop early\"}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n";
        let tool_use = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"Bash\",\"input\":{}}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n";
        let tool_result = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t\",\"content\":\"ok\"}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n";
        let result = "{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\"}\n";
        // A later tool call, and a later tool result alone, both invalidate it.
        for between in [tool_use, tool_result] {
            let events = finish_text(&format!("{text}{between}{result}"));
            assert!(!events.contains(&E::Text("I will stop early".into())));
            assert!(!events.contains(&E::ResultFallback));
            assert!(!events.iter().any(|e| matches!(e, E::ToolActivity)));
        }
        // Text as the last event before the terminal one still qualifies.
        let events = finish_text(&format!("{tool_use}{tool_result}{text}{result}"));
        assert!(events.contains(&E::Text("I will stop early".into())));
        assert!(events.contains(&E::ResultFallback));
    }

    #[test]
    fn fallback_observes_and_checks_answering_model() {
        let input = concat!(
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n",
            "{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\"}\n"
        );
        let mut stream = StreamState::new("claude", decode);
        stream.bind_identity("s", "m");
        stream.observe(input.as_bytes()).unwrap();
        let outcome = stream
            .finish(crate::ExitObservation {
                code: Some(0),
                timed_out: false,
            })
            .unwrap();
        assert_eq!(outcome.model(), Some("m"));
        assert!(outcome.final_events().contains(&E::Model("m".into())));
        let mut stream = StreamState::new("claude", decode);
        stream.bind_identity("s", "other");
        stream.observe(input.as_bytes()).unwrap();
        assert!(matches!(
            stream.finish(crate::ExitObservation {
                code: Some(0),
                timed_out: false
            }),
            Err(ProviderError::IdentityMismatch)
        ));
    }

    #[test]
    fn unsuccessful_streams_never_emit_fallback() {
        let assistant = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n";
        let success = "{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\"}\n";
        let refusal = "{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"api_error_status\":429,\"result\":\"limited\"}\n";
        let incomplete = "{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"success\",\"is_error\":false,\"stop_reason\":\"max_tokens\",\"result\":\"\"}\n";
        for (tail, code, timed_out) in [
            (success, Some(1), false),
            (success, Some(0), true),
            ("{", Some(0), false),
            (refusal, Some(0), false),
            (incomplete, Some(0), false),
            ("", Some(0), false),
        ] {
            let mut stream = StreamState::new("claude", decode);
            let mut events = stream
                .observe(format!("{assistant}{tail}").as_bytes())
                .unwrap();
            let outcome = stream
                .finish(crate::ExitObservation { code, timed_out })
                .unwrap();
            events.extend_from_slice(outcome.final_events());
            assert!(!events.contains(&E::ResultFallback), "tail: {tail}");
            assert!(!events.contains(&E::Text("answer".into())), "tail: {tail}");
        }
    }

    #[test]
    fn non_empty_result_is_not_replaced() {
        let input = concat!(
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n",
            "{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"answer\"}\n"
        );
        let events = finish_text(input);
        assert!(!events.contains(&E::ResultFallback));
        assert_eq!(events.iter().filter(|e| matches!(e, E::Text(_))).count(), 1);
    }

    #[test]
    fn final_answer_identifies_its_model_despite_helper_messages() {
        let run = |answering_model: &str| {
            let mut stream = StreamState::new("claude", decode);
            let input = format!(
                "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"model\":\"helper\",\"content\":[{{\"type\":\"text\",\"text\":\"helper output\"}}]}},\"session_id\":\"s\",\"parent_tool_use_id\":null}}\n{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"model\":\"{answering_model}\",\"content\":[{{\"type\":\"text\",\"text\":\"final answer\"}}]}},\"session_id\":\"s\",\"parent_tool_use_id\":null}}\n{{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"final answer\"}}\n"
            );
            stream.observe(input.as_bytes()).unwrap();
            stream
                .finish(crate::ExitObservation {
                    code: Some(0),
                    timed_out: false,
                })
                .unwrap()
                .model()
                .unwrap()
                .to_owned()
        };
        assert_eq!(run("requested"), "requested");
        assert_eq!(run("substitute"), "substitute");
    }

    #[test]
    fn resumed_answer_rejects_substitution_after_a_helper() {
        let mut stream = StreamState::new("claude", decode);
        stream.bind_identity("s", "requested");
        let input = concat!(
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"helper\",\"content\":[{\"type\":\"text\",\"text\":\"helper\"}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"substitute\",\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]},\"session_id\":\"s\",\"parent_tool_use_id\":null}\n",
            "{\"type\":\"result\",\"session_id\":\"s\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"answer\"}\n"
        );
        assert!(matches!(
            stream.observe(input.as_bytes()),
            Err(ProviderError::IdentityMismatch)
        ));
    }

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
    fn live_usage_counts_each_call_once_with_fresh_input() {
        use provenant_os::capture::{FrameDecoder, FrameEvent};
        let frame = |id: &str| {
            format!(
                r#"{{"type":"assistant","session_id":"s","message":{{"id":"{id}","model":"m","role":"assistant","content":[],"usage":{{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":1000,"cache_creation_input_tokens":100}}}}}}"#
            )
        };
        let one = LiveUsage.decode(frame("a").as_bytes()).unwrap();
        // Every content block of a message repeats its usage under one key.
        assert_eq!(one, LiveUsage.decode(frame("a").as_bytes()).unwrap());
        let FrameEvent::Usage {
            tokens, context, ..
        } = one
        else {
            panic!("an assistant frame with usage reports it");
        };
        // 10 (input) + 5 (output) + 100 (cache creation) = 115. Cache reads (1000) do not count.
        assert_eq!(tokens, 115);
        // The context sent is 10 + 1000 + 100: cached reads count here.
        assert_eq!(context, Some(1110));
        assert_ne!(one, LiveUsage.decode(frame("b").as_bytes()).unwrap());
        for other in [
            &br#"{"type":"system","subtype":"init","session_id":"s"}"#[..],
            br#"{"type":"assistant","message":{"usage":{"input_tokens":1}}}"#,
            b"not json with \"usage\"",
        ] {
            assert_eq!(LiveUsage.decode(other).unwrap(), FrameEvent::Progress);
        }
        assert!(LiveUsage.reports_usage());
    }

    #[test]
    fn usage_limit_is_a_typed_quota_refusal_not_a_generic_failure() {
        let raw = br#"{"type":"assistant","session_id":"s","error":"rate_limit","is_api_error_message":true,"message":{"role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"You've hit your weekly limit \u00b7 resets Nov 1 at 11am"}]}}"#;
        let events = decode(raw).unwrap();
        assert!(events.contains(&E::QuotaLimited {
            message: "You've hit your weekly limit \u{b7} resets Nov 1 at 11am".into(),
            pool: None,
            resets_in_seconds: None,
        }));
        assert!(!events.contains(&E::Error));
        let events = decode(br#"{"type":"result","session_id":"s","subtype":"success","is_error":true,"api_error_status":429,"result":"You've hit your weekly limit"}"#).unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, E::QuotaLimited { .. }))
        );
        // A per-minute 429 that names no usage limit is transient capacity,
        // never a refusal that holds the account.
        for raw in [
            &br#"{"type":"assistant","session_id":"s","error":"rate_limit","is_api_error_message":true,"message":{"role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"Rate limited: too many requests, please slow down"}]}}"#[..],
            br#"{"type":"result","session_id":"s","subtype":"success","is_error":true,"api_error_status":429,"result":""}"#,
            // A short window names a reset too; that alone is not an allowance.
            br#"{"type":"assistant","session_id":"s","error":"rate_limit","is_api_error_message":true,"message":{"role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"Rate limit exceeded; resets in 60 seconds"}]}}"#,
            br#"{"type":"result","session_id":"s","subtype":"success","is_error":true,"api_error_status":429,"result":"Rate limit exceeded; resets in 60 seconds"}"#,
        ] {
            let events = decode(raw).unwrap();
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, E::ModelOverloaded { .. })),
                "{events:?}"
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, E::QuotaLimited { .. }))
            );
        }
        // Other API errors stay generic failures.
        let raw = br#"{"type":"assistant","session_id":"s","error":"invalid_request","is_api_error_message":true,"message":{"role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"bad"}]}}"#;
        let events = decode(raw).unwrap();
        assert!(events.contains(&E::Error));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, E::QuotaLimited { .. }))
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
