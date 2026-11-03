//! Live quota effects: the one provider module that runs probes and reads
//! credentials. Credentials and unredacted captures never leave it.
mod claude;
mod opencode;

use crate::accounts::Settings;
use crate::host::PROBE_DIR_OPERATION;
use crate::identity::{agy_fingerprint, base64url, codex_account, cursor_account};
use provenant_core::Clock;
use provenant_os::clock::SystemClock;
use provenant_os::exec::{
    ExecError, ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor, Termination,
};
use provenant_quota::{
    AccountRecord, AccountScope, ProbeCapture, ProbeFailure, QuotaDetails, QuotaEffects,
    QuotaError, QuotaProbePlan,
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const LIMIT: usize = 1_048_576;
/// A day's backoff is for a probe binary that is absent or cannot execute.
/// Any other failure is the host (busy executor, process budget, a probe
/// directory that vanished before spawn) and retries on the short `Network`
/// schedule. No new persisted variant: older readers reject unknown values.
fn command_error(program: &str, error: ExecError) -> ProbeFailure {
    match error {
        ExecError::Io {
            operation: PROBE_DIR_OPERATION,
            ..
        } => ProbeFailure::ProbeDirectory,
        ExecError::Io {
            operation: crate::agy::KEYCHAIN_OPERATION,
            ..
        } => ProbeFailure::IsolatedKeychainUnavailable,
        ExecError::Io {
            operation: "spawn",
            source,
        } if source.raw_os_error() == Some(ENOEXEC)
            || (matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) && !runnable(program)) =>
        {
            ProbeFailure::MissingCli
        }
        _ => ProbeFailure::Network,
    }
}
/// `ENOEXEC`: the same value on Linux and macOS.
const ENOEXEC: i32 = 8;
/// Whether `program` resolves to a file this user can execute, directly or
/// through `PATH`. The probe runs in its own private directory, so a relative
/// program path or `PATH` entry cannot resolve there and counts as absent.
fn runnable(program: &str) -> bool {
    let executable = |path: &Path| {
        path.is_absolute() && path.is_file() && provenant_os::sys::is_executable(path)
    };
    if program.contains('/') {
        return executable(Path::new(program));
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| executable(&dir.join(program))))
}
struct Account {
    scope: AccountScope,
    identity_dir: PathBuf,
    options: Value,
}
pub struct LiveQuota {
    home: PathBuf,
    state: PathBuf,
    accounts: Vec<Account>,
    refresh_expired: bool,
    claude_default: RefCell<Option<claude::Identity>>,
    readiness_failure: RefCell<Option<provenant_quota::ReadinessReason>>,
    clock: SystemClock,
    /// Stands in for the system executor so tests can see each probe's command.
    #[cfg(test)]
    executor: Option<Box<dyn Executor>>,
}
fn read(path: &Path) -> Result<Vec<u8>, ProbeFailure> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| ProbeFailure::Auth)?
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ProbeFailure::Malformed)?;
    if bytes.len() > LIMIT {
        return Err(ProbeFailure::Malformed);
    }
    Ok(bytes)
}
fn value(path: &Path) -> Result<Value, ProbeFailure> {
    serde_json::from_slice(&read(path)?).map_err(|_| ProbeFailure::Malformed)
}
// Keep Lane Q's Claude identity scheme in the provider owner.
fn fingerprint(identity: &str, length: usize) -> String {
    crate::identity::fingerprint(identity)
        .and_then(|s| s.get(..length).map(str::to_owned))
        .unwrap_or_default()
}
fn cursor_token(dir: &Path) -> Option<String> {
    use rusqlite::{Connection, OpenFlags};
    let db = Connection::open_with_flags(dir.join("state.vscdb"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .ok()?;
    db.busy_timeout(Duration::from_millis(250)).ok()?;
    let token: String = db.query_row("SELECT substr(value,1,1048577) FROM ItemTable WHERE key='cursorAuth/accessToken' LIMIT 1", [], |r| r.get(0)).ok()?;
    (token.len() <= LIMIT).then_some(token)
}
fn signin_key(adapter: &str, dir: &Path) -> Option<String> {
    match adapter {
        "codex" => read(&dir.join("auth.json"))
            .ok()
            .and_then(|b| codex_account(&b)),
        "cursor" => cursor_token(dir).and_then(|s| cursor_account(&s)),
        _ => None,
    }
}

impl LiveQuota {
    /// Probes for the registered accounts, with private probe directories
    /// under the `state` root and `home` as each probe's `HOME`.
    pub fn new(state: &Path, home: &Path, settings: &Settings) -> Result<Self, QuotaError> {
        let mut accounts = Vec::new();
        for registration in settings.with_derived_accounts().entries.values() {
            let dir = &registration.directory;
            if !dir.is_absolute()
                || dir
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(QuotaError::Invalid("absolute identity directory"));
            }
            let scope = AccountScope {
                adapter: registration.adapter.clone(),
                account_key: registration.key.clone(),
                label: registration.label.clone(),
            };
            scope.validate()?;
            accounts.push(Account {
                scope,
                identity_dir: dir.clone(),
                options: json!({
                    "plan": &registration.plan,
                    "priority": registration.priority,
                    "billing_anchor": &registration.billing_anchor,
                    "limits": &registration.limits,
                }),
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        accounts.retain(|a| seen.insert((a.scope.adapter.clone(), a.scope.account_key.clone())));
        if accounts.len() > provenant_quota::MAX_ACCOUNTS {
            return Err(QuotaError::Invalid("account bound"));
        }
        Ok(Self {
            home: home.to_owned(),
            state: state.to_owned(),
            accounts,
            refresh_expired: true,
            claude_default: RefCell::new(None),
            readiness_failure: RefCell::new(None),
            clock: SystemClock::new(),
            #[cfg(test)]
            executor: None,
        })
    }
    fn now(&self) -> i64 {
        self.clock.unix_seconds()
    }
    fn command(
        &self,
        program: &str,
        args: &[&str],
        selector: Option<(&str, &Path)>,
        stdin: Option<&[u8]>,
        deadline: i64,
        max_seconds: u64,
    ) -> Result<ExecOutcome, ProbeFailure> {
        self.command_as(
            &self.home,
            None,
            program,
            args,
            selector,
            stdin,
            deadline,
            max_seconds,
        )
    }
    /// Runs a probe with `home` as its `HOME`.
    #[allow(clippy::too_many_arguments)]
    fn command_as(
        &self,
        home: &Path,
        keychain: Option<&crate::agy::ProbeHome>,
        program: &str,
        args: &[&str],
        selector: Option<(&str, &Path)>,
        stdin: Option<&[u8]>,
        deadline: i64,
        max_seconds: u64,
    ) -> Result<ExecOutcome, ProbeFailure> {
        let remaining = deadline.saturating_sub(self.now());
        if remaining <= 0 {
            return Err(ProbeFailure::Network);
        }
        let system = SystemExecutor::new(1).map_err(|_| ProbeFailure::Network)?;
        #[cfg(test)]
        let base: &dyn Executor = self.executor.as_deref().unwrap_or(&system);
        #[cfg(not(test))]
        let base: &dyn Executor = &system;
        let host = crate::host::HostProbeExecutor::new(base, &self.state)
            .map_err(|_| ProbeFailure::ProbeDirectory)?;
        // An isolated agy keychain is re-checked around the spawn.
        let guarded = keychain.map(|keychain| keychain.guard(&host));
        let executor: &dyn Executor = match &guarded {
            Some(guarded) => guarded,
            None => &host,
        };
        let mut argv = vec![OsString::from(program)];
        argv.extend(args.iter().map(OsString::from));
        let mut env = vec![
            (OsString::from("HOME"), home.as_os_str().to_owned()),
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
        ];
        // Claude Code uses the OS login name when selecting its Keychain item.
        for key in ["USER", "LOGNAME", "SHELL", "TMPDIR", "LANG"] {
            if let Some(value) = std::env::var_os(key) {
                env.push((key.into(), value));
            }
        }
        if let Some((key, path)) = selector {
            env.push((key.into(), path.as_os_str().to_owned()));
        }
        // HostProbeExecutor replaces this `/` cwd with a fresh private directory.
        let mut options = ExecOptions::new(
            &argv,
            Path::new("/"),
            Duration::from_secs((remaining as u64).min(max_seconds)),
        );
        options.env = Some(&env);
        options.stdin = stdin;
        options.stdout = StreamPolicy::Capture { limit: LIMIT };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        match executor.execute(options) {
            Ok(outcome) => {
                if outcome.timed_out {
                    *self.readiness_failure.borrow_mut() =
                        Some(provenant_quota::ReadinessReason::ProbeTimeout);
                }
                Ok(outcome)
            }
            Err(error) => {
                let failure = command_error(program, error);
                *self.readiness_failure.borrow_mut() = Some(match failure {
                    ProbeFailure::MissingCli => provenant_quota::ReadinessReason::MissingExecutable,
                    _ => provenant_quota::ReadinessReason::CommandFailure,
                });
                Err(failure)
            }
        }
    }
    fn json_command(
        &self,
        home: &crate::agy::ProbeHome,
        program: &str,
        args: &[&str],
        selector: Option<(&str, &Path)>,
        deadline: i64,
        seconds: u64,
    ) -> Result<Value, ProbeFailure> {
        let out = self.command_as(
            home.path(),
            Some(home),
            program,
            args,
            selector,
            None,
            deadline,
            seconds,
        )?;
        if !clean(&out) || out.termination != Termination::Exit(0) {
            return Err(ProbeFailure::Network);
        }
        serde_json::from_slice(&out.stdout.bytes).map_err(|_| ProbeFailure::Malformed)
    }
    fn http(
        &self,
        url: &str,
        headers: &[(&str, String)],
        deadline: i64,
    ) -> Result<Value, ProbeFailure> {
        // Curl reads sensitive headers from an anonymous pipe, never argv or disk.
        let mut input = String::new();
        for (name, value) in headers {
            if value.chars().any(char::is_control) {
                return Err(ProbeFailure::Auth);
            }
            let header = serde_json::to_string(&format!("{name}: {value}"))
                .map_err(|_| ProbeFailure::Malformed)?;
            input.push_str(&format!("header = {header}\n"));
        }
        let out = self.command(
            "/usr/bin/curl",
            &[
                "--disable",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--connect-timeout",
                "5",
                "--max-time",
                "12",
                "--max-filesize",
                "1048576",
                "--config",
                "-",
                "--write-out",
                "\n%{http_code}",
                url,
            ],
            None,
            Some(input.as_bytes()),
            deadline,
            14,
        )?;
        if !clean(&out) || out.termination != Termination::Exit(0) {
            return Err(ProbeFailure::Network);
        }
        let split = out
            .stdout
            .bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .ok_or(ProbeFailure::Malformed)?;
        match &out.stdout.bytes[split + 1..] {
            b"200" => serde_json::from_slice(&out.stdout.bytes[..split])
                .map_err(|_| ProbeFailure::Malformed),
            b"401" | b"403" => Err(ProbeFailure::Auth),
            b"429" => Err(ProbeFailure::RateLimited),
            _ => Err(ProbeFailure::Network),
        }
    }
}
fn clean(out: &ExecOutcome) -> bool {
    !out.timed_out
        && !out.cancelled
        && !out.drain_truncated
        && !out.stdout.truncated
        && !out.stdout.read_failed
        && !out.overflow_stopped
}
impl QuotaEffects for LiveQuota {
    fn accounts(&self) -> Result<Vec<AccountScope>, QuotaError> {
        Ok(self.accounts.iter().map(|a| a.scope.clone()).collect())
    }
    fn capture(
        &self,
        plan: &QuotaProbePlan,
        previous: Option<&AccountRecord>,
    ) -> Result<ProbeCapture, ProbeFailure> {
        *self.readiness_failure.borrow_mut() = None;
        let account = self
            .accounts
            .iter()
            .find(|a| a.scope == plan.account)
            .ok_or(ProbeFailure::Auth)?;
        let deadline = plan.input.deadline_unix_seconds;
        let check_identity = || {
            if provenant_records::AdapterSpec::get(&account.scope.adapter)
                .is_some_and(|spec| spec.quota_identity_check)
                && signin_key(&account.scope.adapter, &account.identity_dir)
                    .as_deref()
                    .unwrap_or("unknown")
                    != account.scope.account_key
            {
                return Err(ProbeFailure::Auth);
            }
            Ok(())
        };
        check_identity()?;
        let mut details = QuotaDetails {
            plan: account
                .options
                .get("plan")
                .and_then(Value::as_str)
                .map(str::to_owned),
            priority: account.options.get("priority").and_then(Value::as_u64),
            billing_anchor: account
                .options
                .get("billing_anchor")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..QuotaDetails::default()
        };
        let payload = match account.scope.adapter.as_str() {
            "claude" => self.claude(account, previous, deadline, &mut details)?,
            "codex" => self.codex(account, deadline)?,
            "cursor" => self.cursor(account, deadline)?,
            "agy" => self.agy(account, deadline, &mut details)?,
            "opencode" if account.scope.account_key == "zen" => {
                details.note = Some(
                    "Observed free-limit refusals only; allowance and daily reset unknown".into(),
                );
                json!({})
            }
            "opencode" => {
                details.note = Some("Local estimate, this machine only; calibrated Go caps, other hosts not counted".into());
                opencode::capture(
                    &account.identity_dir,
                    &account.options,
                    self.now(),
                    deadline,
                )?
            }
            "command" => return Err(ProbeFailure::Disabled),
            _ => return Err(ProbeFailure::Disabled),
        };
        check_identity()?;
        Ok(ProbeCapture {
            stdout: serde_json::to_vec(&json!({"payload":payload,"details":details}))
                .map_err(|_| ProbeFailure::Malformed)?,
            stderr: Vec::new(),
            exit_code: Some(0),
            overflowed: false,
        })
    }

    fn readiness_reason(&self, failure: ProbeFailure) -> provenant_quota::ReadinessReason {
        match failure {
            ProbeFailure::MissingCli | ProbeFailure::Disabled => {
                provenant_quota::ReadinessReason::MissingExecutable
            }
            ProbeFailure::Auth => provenant_quota::ReadinessReason::SignedOut,
            ProbeFailure::AccountMismatch => provenant_quota::ReadinessReason::AccountMismatch,
            _ => self
                .readiness_failure
                .borrow()
                .unwrap_or(provenant_quota::ReadinessReason::CommandFailure),
        }
    }
}
impl LiveQuota {
    fn codex(&self, account: &Account, deadline: i64) -> Result<Value, ProbeFailure> {
        // The service handles its own credentials. EOF closes this non-inference exchange.
        let input = concat!(
            "{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"provenant\",\"version\":\"1\"}}}\n",
            "{\"method\":\"initialized\"}\n",
            "{\"id\":2,\"method\":\"account/rateLimits/read\"}\n"
        );
        let out = self.command(
            "codex",
            &["-s", "read-only", "-a", "never", "app-server"],
            Some(("CODEX_HOME", &account.identity_dir)),
            Some(input.as_bytes()),
            deadline,
            crate::codex::REGISTRATION.quota.probe_seconds,
        )?;
        if out.stdout.truncated || out.stdout.read_failed {
            return Err(ProbeFailure::Malformed);
        }
        for line in out.stdout.bytes.split(|b| *b == b'\n') {
            if let Ok(v) = serde_json::from_slice::<Value>(line)
                && v.get("id").and_then(Value::as_u64) == Some(2)
                && let Some(result) = v.get("result").filter(|r| r.is_object())
            {
                return Ok(result.clone());
            }
        }
        // App-server can be unavailable when its runtime database is read-only.
        // The usage endpoint accepts the same account-scoped OAuth token.
        let auth = value(&account.identity_dir.join("auth.json"))?;
        let token = auth
            .pointer("/tokens/access_token")
            .and_then(Value::as_str)
            .ok_or(ProbeFailure::Auth)?;
        let mut headers = vec![("Authorization", format!("Bearer {token}"))];
        if let Some(id) = auth.pointer("/tokens/account_id").and_then(Value::as_str) {
            headers.push(("ChatGPT-Account-Id", id.to_owned()));
        }
        self.http(
            "https://chatgpt.com/backend-api/wham/usage",
            &headers,
            deadline,
        )
    }
    fn cursor(&self, account: &Account, deadline: i64) -> Result<Value, ProbeFailure> {
        let token = cursor_token(&account.identity_dir).ok_or(ProbeFailure::Auth)?;
        let claims = token
            .split('.')
            .nth(1)
            .and_then(base64url)
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .ok_or(ProbeFailure::Auth)?;
        let user = claims
            .get("sub")
            .and_then(Value::as_str)
            .and_then(|s| s.rsplit('|').next())
            .filter(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
            .ok_or(ProbeFailure::Auth)?;
        if claims
            .get("exp")
            .and_then(Value::as_i64)
            .is_none_or(|e| e <= self.now() + 60)
        {
            return Err(ProbeFailure::Auth);
        }
        self.http(
            "https://cursor.com/api/usage-summary",
            &[(
                "Cookie",
                format!("WorkosCursorSessionToken={user}%3A%3A{token}"),
            )],
            deadline,
        )
    }
    fn agy(
        &self,
        account: &Account,
        deadline: i64,
        details: &mut QuotaDetails,
    ) -> Result<Value, ProbeFailure> {
        // An isolated agy account is signed in under its own directory as
        // `HOME`. A missing keychain has its own failure, never auth, and the
        // probe re-checks it around the spawn.
        let probe_home =
            crate::agy::ProbeHome::select(&account.identity_dir, &self.home).map_err(|error| {
                match error {
                    crate::ProviderError::IsolatedKeychainUnavailable(_) => {
                        ProbeFailure::IsolatedKeychainUnavailable
                    }
                    _ => ProbeFailure::Auth,
                }
            })?;
        let scratch = crate::host::HostProbeDir::create(&self.state)
            .map_err(|_| ProbeFailure::ProbeDirectory)?;
        let log = scratch.path().join("cli.log");
        let log_arg = log.to_str().ok_or(ProbeFailure::Malformed)?;
        let payload = self.json_command(
            &probe_home,
            "agy",
            &[
                "-p",
                "/usage",
                "--output-format",
                "json",
                "--log-file",
                log_arg,
            ],
            None,
            deadline,
            crate::agy::REGISTRATION.quota.probe_seconds,
        )?;
        let signed_in = payload.get("status").and_then(Value::as_str) == Some("SUCCESS");
        let actual = agy_signed_in_account(&account.scope.account_key, signed_in, read(&log).ok())?;
        details.fingerprint = Some(actual);
        Ok(payload)
    }
}
/// The signed-in agy fingerprint, or why the probe cannot trust it. A sign-out
/// or unreadable identity needs a login; a readable identity that is not the
/// registered key is a different account, repaired by registering it. The
/// identity itself is never part of the failure.
fn agy_signed_in_account(
    registered_key: &str,
    signed_in: bool,
    log: Option<Vec<u8>>,
) -> Result<String, ProbeFailure> {
    if !signed_in {
        return Err(ProbeFailure::Auth);
    }
    let actual = log
        .and_then(|bytes| agy_fingerprint(&bytes))
        .ok_or(ProbeFailure::Auth)?;
    if registered_key != format!("agy-{actual}") {
        return Err(ProbeFailure::AccountMismatch);
    }
    Ok(actual)
}
/// Explicit Claude registrations only. No auth stores, identity probes or
/// implicit default directory discovery are involved in chair switching.
pub fn registered_claude(
    settings: &Settings,
) -> Result<Vec<crate::identity::RegisteredAccount>, Box<dyn std::error::Error>> {
    use crate::identity::{Accounts, RegisteredAccount};
    let mut accounts = Vec::new();
    for registration in settings.entries.values() {
        if registration.adapter != "claude" {
            continue;
        }
        let path = &registration.directory;
        if !path.is_absolute() {
            return Err("registered Claude directory must be absolute".into());
        }
        let path = fs::canonicalize(path)?;
        if !path.is_dir() {
            return Err("registered Claude directory is unavailable".into());
        }
        accounts.push(RegisteredAccount::new("claude", &registration.key, path)?);
    }
    Accounts::new(accounts.clone())?;
    Ok(accounts)
}

#[cfg(test)]
mod tests {
    // The worktree driver includes this module without libtest.
    #[allow(unused_imports)]
    use super::*;
    #[allow(unused_imports)]
    use provenant_os::exec::ExecError;

    #[allow(dead_code)]
    fn quota(home: &Path, state: PathBuf) -> LiveQuota {
        LiveQuota {
            home: home.to_owned(),
            state,
            accounts: Vec::new(),
            refresh_expired: false,
            claude_default: RefCell::new(None),
            readiness_failure: RefCell::new(None),
            clock: SystemClock::new(),
            executor: None,
        }
    }

    #[test]
    fn command_names_probe_directory_setup_failures() {
        let home = tempfile::tempdir().unwrap();
        let quota = quota(home.path(), home.path().join("missing"));
        let failure = quota
            .command("unused-provider", &[], None, None, quota.now() + 60, 1)
            .unwrap_err();
        assert_eq!(serde_json::to_value(failure).unwrap(), "probe_directory");
    }

    #[test]
    fn agy_account_switch_is_distinct_from_sign_out() {
        let log =
            |email: &str| format!("OAuth: authenticated successfully as {email}\n").into_bytes();
        let registered = format!("agy-{}", agy_fingerprint(&log("old@example.com")).unwrap());
        assert!(agy_signed_in_account(&registered, true, Some(log("old@example.com"))).is_ok());
        assert_eq!(
            agy_signed_in_account(&registered, true, Some(log("new@example.com"))),
            Err(ProbeFailure::AccountMismatch)
        );
        // Signed out, or no identity in the log: still a login problem.
        assert_eq!(
            agy_signed_in_account(&registered, false, Some(log("old@example.com"))),
            Err(ProbeFailure::Auth)
        );
        assert_eq!(
            agy_signed_in_account(&registered, true, Some(b"no identity".to_vec())),
            Err(ProbeFailure::Auth)
        );
        assert_eq!(
            agy_signed_in_account(&registered, true, None),
            Err(ProbeFailure::Auth)
        );
    }

    /// A fake `agy` that signs in as one Google account per `HOME`, recording
    /// the `HOME` of each `-p /usage` probe the quota path runs.
    struct AgyUsage {
        sign_ins: Vec<(PathBuf, &'static str)>,
        homes: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    }
    impl Executor for AgyUsage {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            assert_eq!(options.argv[0], "agy");
            assert_eq!(options.argv[1], "-p");
            assert_eq!(options.argv[2], "/usage");
            let home: PathBuf = options
                .env
                .unwrap_or_default()
                .iter()
                .find(|(key, _)| key == "HOME")
                .map(|(_, value)| value.into())
                .expect("probe sets HOME");
            let log = options
                .argv
                .iter()
                .position(|arg| arg == "--log-file")
                .map(|index| PathBuf::from(&options.argv[index + 1]))
                .expect("probe names a log file");
            let email = self
                .sign_ins
                .iter()
                .find(|(directory, _)| *directory == home)
                .expect("probe HOME is a known sign-in")
                .1;
            fs::write(
                log,
                format!("OAuth: authenticated successfully as {email}\n"),
            )
            .unwrap();
            self.homes.lock().unwrap().push(home);
            Ok(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: provenant_os::exec::CapturedStream {
                    bytes: br#"{"status":"SUCCESS"}"#.to_vec(),
                    ..Default::default()
                },
                stderr: Default::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }

    #[test]
    fn agy_quota_probe_runs_under_the_accounts_own_home_and_never_falls_back_to_the_login() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        let isolated = root.path().join("agy-second");
        let legacy = root.path().join("gemini");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(isolated.join("Library/Keychains")).unwrap();
        fs::write(isolated.join("Library/Keychains/login.keychain-db"), "").unwrap();
        fs::create_dir_all(&legacy).unwrap();
        // A keychain that is only a symlink is not the account's own.
        let linked = root.path().join("agy-linked");
        fs::create_dir_all(linked.join("Library/Keychains")).unwrap();
        std::os::unix::fs::symlink(
            isolated.join("Library/Keychains/login.keychain-db"),
            linked.join("Library/Keychains/login.keychain-db"),
        )
        .unwrap();
        let homes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut quota = quota(&home, state);
        quota.executor = Some(Box::new(AgyUsage {
            sign_ins: vec![
                (home.clone(), "first@example.test"),
                (isolated.clone(), "second@example.test"),
            ],
            homes: homes.clone(),
        }));
        let account = |dir: &Path, email: &str| Account {
            scope: AccountScope {
                adapter: "agy".into(),
                account_key: format!(
                    "agy-{}",
                    agy_fingerprint(
                        format!("OAuth: authenticated successfully as {email}\n").as_bytes()
                    )
                    .unwrap()
                ),
                label: "test".into(),
            },
            identity_dir: dir.to_owned(),
            options: Value::Null,
        };
        let probe = |dir: &Path, email: &str| {
            quota.agy(
                &account(dir, email),
                quota.now() + 60,
                &mut QuotaDetails::default(),
            )
        };
        // The isolated account is probed under its own directory, and its
        // identity check matches the second sign-in, not the login keychain's.
        probe(&isolated, "second@example.test").unwrap();
        // The user's own login is registered at the home directory itself.
        probe(&home, "first@example.test").unwrap();
        assert_eq!(*homes.lock().unwrap(), [isolated.clone(), home.clone()]);
        // Any other directory without a regular keychain file (absent, no file,
        // or a symlink) is refused without a probe, never run under the login.
        for dir in [&legacy, &root.path().join("absent"), &linked] {
            assert_eq!(
                probe(dir, "first@example.test").unwrap_err(),
                ProbeFailure::IsolatedKeychainUnavailable
            );
        }
        // The keychain vanishing after a successful probe fails closed too.
        fs::remove_file(isolated.join("Library/Keychains/login.keychain-db")).unwrap();
        assert_eq!(
            probe(&isolated, "second@example.test").unwrap_err(),
            ProbeFailure::IsolatedKeychainUnavailable
        );
        assert_eq!(*homes.lock().unwrap(), [isolated, home]);
    }

    /// Runs the wrapped fake, then removes `keychain` as a competing process
    /// would while the probe is running.
    struct VanishingKeychain {
        inner: AgyUsage,
        keychain: PathBuf,
    }
    impl Executor for VanishingKeychain {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let outcome = self.inner.execute(options);
            fs::remove_file(&self.keychain).unwrap();
            outcome
        }
    }

    #[test]
    fn agy_quota_probe_refuses_a_keychain_that_vanishes_while_it_runs() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        let isolated = root.path().join("agy-second");
        let keychain = isolated.join("Library/Keychains/login.keychain-db");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(keychain.parent().unwrap()).unwrap();
        fs::write(&keychain, "").unwrap();
        let homes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut quota = quota(&home, state);
        quota.executor = Some(Box::new(VanishingKeychain {
            inner: AgyUsage {
                sign_ins: vec![(isolated.clone(), "second@example.test")],
                homes: homes.clone(),
            },
            keychain,
        }));
        let email = "OAuth: authenticated successfully as second@example.test\n";
        let account = Account {
            scope: AccountScope {
                adapter: "agy".into(),
                account_key: format!("agy-{}", agy_fingerprint(email.as_bytes()).unwrap()),
                label: "test".into(),
            },
            identity_dir: isolated,
            options: Value::Null,
        };
        // The probe ran, but its result is not trusted once the keychain is gone.
        assert_eq!(
            quota
                .agy(&account, quota.now() + 60, &mut QuotaDetails::default())
                .unwrap_err(),
            ProbeFailure::IsolatedKeychainUnavailable
        );
        assert_eq!(homes.lock().unwrap().len(), 1);
    }

    #[test]
    fn agy_names_probe_log_directory_failures() {
        let home = tempfile::tempdir().unwrap();
        let quota = quota(home.path(), home.path().join("missing"));
        let account = Account {
            scope: AccountScope {
                adapter: "agy".into(),
                account_key: "agy-test".into(),
                label: "test".into(),
            },
            identity_dir: home.path().to_owned(),
            options: Value::Null,
        };
        let failure = quota
            .agy(&account, quota.now() + 60, &mut QuotaDetails::default())
            .unwrap_err();
        assert_eq!(serde_json::to_value(failure).unwrap(), "probe_directory");
    }

    #[test]
    fn command_names_per_command_probe_directory_failures() {
        let failure = command_error(
            "/bin/sh",
            ExecError::Io {
                operation: PROBE_DIR_OPERATION,
                source: std::io::Error::other("test directory failure"),
            },
        );
        assert_eq!(serde_json::to_value(failure).unwrap(), "probe_directory");
        let spawn = |kind| ExecError::Io {
            operation: "spawn",
            source: std::io::Error::from(kind),
        };
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied,
        ] {
            // An absent binary backs off for a day; a present one that fails
            // to spawn (its probe directory vanished) retries soon.
            assert_eq!(
                command_error("/nonexistent/provenant-probe", spawn(kind)),
                ProbeFailure::MissingCli
            );
            assert_eq!(
                command_error("provenant-probe-not-on-path", spawn(kind)),
                ProbeFailure::MissingCli
            );
            assert_eq!(command_error("/bin/sh", spawn(kind)), ProbeFailure::Network);
            assert_eq!(command_error("sh", spawn(kind)), ProbeFailure::Network);
        }
        // Present but not executable by this user, or reachable only through a
        // relative path that cannot resolve in the private probe directory.
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("provenant-probe");
        fs::write(&plain, "").unwrap();
        let plain = plain.to_str().unwrap();
        let relative = "bin/provenant-probe";
        for program in [plain, relative] {
            assert_eq!(
                command_error(program, spawn(std::io::ErrorKind::PermissionDenied)),
                ProbeFailure::MissingCli,
                "{program}"
            );
        }
        let not_executable = ExecError::Io {
            operation: "spawn",
            source: std::io::Error::from_raw_os_error(ENOEXEC),
        };
        assert_eq!(
            command_error("/bin/sh", not_executable),
            ProbeFailure::MissingCli
        );
        for failure in [
            ExecError::Busy,
            ExecError::ProcessBudget { used: 9, limit: 8 },
            ExecError::Io {
                operation: "spawn",
                source: std::io::Error::other("test spawn failure"),
            },
            ExecError::Io {
                operation: "poll",
                source: std::io::Error::other("test poll failure"),
            },
        ] {
            assert_eq!(command_error("/bin/sh", failure), ProbeFailure::Network);
        }

        let quota = LiveQuota {
            home: std::env::temp_dir(),
            state: std::env::temp_dir(),
            accounts: Vec::new(),
            refresh_expired: false,
            claude_default: RefCell::new(None),
            readiness_failure: RefCell::new(None),
            clock: SystemClock::new(),
            #[cfg(test)]
            executor: None,
        };
        let reason = |failure| quota.readiness_reason(failure);
        assert_eq!(
            reason(ProbeFailure::MissingCli),
            provenant_quota::ReadinessReason::MissingExecutable
        );
        assert_eq!(
            provenant_quota::schedule::backoff(ProbeFailure::MissingCli, 1, None),
            86_400
        );
        *quota.readiness_failure.borrow_mut() =
            Some(provenant_quota::ReadinessReason::ProbeTimeout);
        assert_eq!(
            reason(ProbeFailure::Network),
            provenant_quota::ReadinessReason::ProbeTimeout
        );
        assert_eq!(
            provenant_quota::schedule::backoff(ProbeFailure::Network, 1, None),
            15
        );
        *quota.readiness_failure.borrow_mut() =
            Some(provenant_quota::ReadinessReason::CommandFailure);
        assert_eq!(
            reason(ProbeFailure::Network),
            provenant_quota::ReadinessReason::CommandFailure
        );
        assert_eq!(
            provenant_quota::schedule::backoff(ProbeFailure::Network, 1, None),
            15
        );
        *quota.readiness_failure.borrow_mut() = None;
        assert_eq!(
            reason(ProbeFailure::Auth),
            provenant_quota::ReadinessReason::SignedOut
        );
        assert_eq!(
            provenant_quota::schedule::backoff(ProbeFailure::Auth, 1, None),
            900
        );
    }

    #[test]
    fn claude_does_not_hide_probe_directory_failures_as_auth() {
        let home = tempfile::tempdir().unwrap();
        let quota = quota(home.path(), home.path().join("missing"));
        let account = Account {
            scope: AccountScope {
                adapter: "claude".into(),
                account_key: "test".into(),
                label: "test".into(),
            },
            identity_dir: home.path().join(".claude-work"),
            options: Value::Null,
        };
        let failure = quota
            .claude(
                &account,
                None,
                quota.now() + 60,
                &mut QuotaDetails::default(),
            )
            .unwrap_err();
        assert_eq!(serde_json::to_value(failure).unwrap(), "probe_directory");
    }
}

#[cfg(test)]
mod zen_tests {
    use super::*;
    use provenant_quota::{QuotaProbeInput, QuotaStatus, Refusal};

    #[test]
    fn zen_is_derived_and_uses_refusals_without_reading_go_ledger() {
        let settings: Settings = serde_json::from_value(json!({
            "entries": {"paid": {
                "adapter": "opencode", "label": "go", "key": "go",
                "directory": "/fixture/opencode", "plan": "go"
            }}
        }))
        .unwrap();
        let live = LiveQuota::new(
            Path::new("/fixture/state"),
            Path::new("/fixture/home"),
            &settings,
        )
        .unwrap();
        let accounts = live.accounts().unwrap();
        assert_eq!(accounts.len(), 2);
        assert!(accounts.iter().any(|scope| scope.account_key == "go"));
        let zen = accounts
            .iter()
            .find(|scope| scope.account_key == "zen")
            .unwrap();
        assert_eq!(zen.label, "zen");
        let derived = settings.with_derived_accounts();
        assert_eq!(derived.with_derived_accounts().entries.len(), 2);
        let mut previous = AccountRecord::empty(zen.clone());
        previous.refusals.push(Refusal {
            extra: Default::default(),
            account: zen.clone(),
            model: "opencode/beam-free".into(),
            pool: Some("opencode-free".into()),
            observed_unix_seconds: 100,
            resets_unix_seconds: None,
        });
        for at in [101, 3700] {
            let plan = crate::quota::plan(
                "opencode",
                zen,
                &QuotaProbeInput {
                    now_unix_seconds: at,
                    deadline_unix_seconds: at + 30,
                    authority_ref: None,
                },
            )
            .unwrap();
            let capture = live.capture(&plan, Some(&previous)).unwrap();
            let snapshot = crate::quota::decode(&plan, &capture).unwrap();
            assert_eq!(snapshot.status, QuotaStatus::Unknown);
            assert!(
                snapshot
                    .windows
                    .iter()
                    .all(|window| window.resets_unix_seconds.is_none())
            );
        }
    }
}
