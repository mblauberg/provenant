//! Live quota effects. Credentials and unredacted captures never leave this edge.
mod claude;
mod opencode;

use provenant_core::Roots;
use provenant_providers::host::PROBE_DIR_OPERATION;
use provenant_providers::identity::{agy_fingerprint, base64url, codex_account, cursor_account};
use provenant_quota::{
    AccountRecord, AccountScope, ProbeCapture, ProbeFailure, QuotaDetails, QuotaEffects,
    QuotaError, QuotaProbePlan,
};
use provenant_supervisor::exec::{
    ExecError, ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor, Termination,
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const LIMIT: usize = 1_048_576;
fn command_error(error: ExecError) -> ProbeFailure {
    match error {
        ExecError::Io {
            operation: PROBE_DIR_OPERATION,
            ..
        } => ProbeFailure::ProbeDirectory,
        _ => ProbeFailure::MissingCli,
    }
}
struct Account {
    scope: AccountScope,
    identity_dir: PathBuf,
    options: Value,
}
pub(crate) struct LiveQuota {
    home: PathBuf,
    state: PathBuf,
    accounts: Vec<Account>,
    refresh_expired: bool,
    claude_default: RefCell<Option<claude::Identity>>,
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
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |n| n.as_secs() as i64)
}
// Keep Lane Q's Claude identity scheme in the provider owner.
fn fingerprint(identity: &str, length: usize) -> String {
    provenant_providers::identity::fingerprint(identity)
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
    pub(crate) fn new(roots: &Roots) -> Result<Self, QuotaError> {
        let home = PathBuf::from(std::env::var_os("HOME").ok_or(QuotaError::Invalid("home"))?);
        let config = provenant_config::read_with_domains(
            roots,
            &provenant_supervisor::fs::LocalFs,
            &provenant_cli::domains::SECTIONS,
        )
        .map_err(|_| QuotaError::Invalid("accounts configuration"))?;
        let mut accounts = Vec::new();
        for registration in config.domains.accounts.entries.values() {
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
            home,
            state: roots.state.as_path().into(),
            accounts,
            refresh_expired: true,
            claude_default: RefCell::new(None),
        })
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
        let remaining = deadline.saturating_sub(now());
        if remaining <= 0 {
            return Err(ProbeFailure::Network);
        }
        let system = SystemExecutor::new(1).map_err(|_| ProbeFailure::MissingCli)?;
        let executor = provenant_providers::host::HostProbeExecutor::new(&system, &self.state)
            .map_err(|_| ProbeFailure::ProbeDirectory)?;
        let mut argv = vec![OsString::from(program)];
        argv.extend(args.iter().map(OsString::from));
        let mut env = vec![
            (OsString::from("HOME"), self.home.as_os_str().to_owned()),
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
        executor.execute(options).map_err(command_error)
    }
    fn json_command(
        &self,
        program: &str,
        args: &[&str],
        selector: Option<(&str, &Path)>,
        deadline: i64,
        seconds: u64,
    ) -> Result<Value, ProbeFailure> {
        let out = self.command(program, args, selector, None, deadline, seconds)?;
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
            "opencode" => {
                details.note = Some("Local estimate, this machine only; calibrated Go caps, other hosts not counted".into());
                opencode::capture(&account.identity_dir, &account.options, now(), deadline)?
            }
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
            provenant_providers::codex::REGISTRATION.quota.probe_seconds,
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
            .is_none_or(|e| e <= now() + 60)
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
        let scratch = provenant_providers::host::HostProbeDir::create(&self.state)
            .map_err(|_| ProbeFailure::ProbeDirectory)?;
        let log = scratch.path().join("cli.log");
        let log_arg = log.to_str().ok_or(ProbeFailure::Malformed)?;
        let payload = self.json_command(
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
            provenant_providers::agy::REGISTRATION.quota.probe_seconds,
        )?;
        if payload.get("status").and_then(Value::as_str) != Some("SUCCESS") {
            return Err(ProbeFailure::Auth);
        }
        let actual = read(&log)
            .ok()
            .and_then(|b| agy_fingerprint(&b))
            .ok_or(ProbeFailure::Auth)?;
        if account.scope.account_key != format!("agy-{actual}") {
            return Err(ProbeFailure::Auth);
        }
        details.fingerprint = Some(actual);
        Ok(payload)
    }
}
/// Explicit Claude registrations only. No auth stores, identity probes or
/// implicit default directory discovery are involved in chair switching.
pub(crate) fn registered_claude(
    roots: &Roots,
) -> Result<Vec<provenant_providers::identity::RegisteredAccount>, Box<dyn std::error::Error>> {
    use provenant_providers::identity::{Accounts, RegisteredAccount};
    let config = provenant_config::read_with_domains(
        roots,
        &provenant_supervisor::fs::LocalFs,
        &provenant_cli::domains::SECTIONS,
    )?;
    let mut accounts = Vec::new();
    for registration in config.domains.accounts.entries.values() {
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
    use provenant_supervisor::exec::ExecError;

    #[allow(dead_code)]
    fn quota(home: &Path, state: PathBuf) -> LiveQuota {
        LiveQuota {
            home: home.to_owned(),
            state,
            accounts: Vec::new(),
            refresh_expired: false,
            claude_default: RefCell::new(None),
        }
    }

    #[test]
    fn command_names_probe_directory_setup_failures() {
        let home = tempfile::tempdir().unwrap();
        let quota = quota(home.path(), home.path().join("missing"));
        let failure = quota
            .command("unused-provider", &[], None, None, now() + 60, 1)
            .unwrap_err();
        assert_eq!(serde_json::to_value(failure).unwrap(), "probe_directory");
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
            .agy(&account, now() + 60, &mut QuotaDetails::default())
            .unwrap_err();
        assert_eq!(serde_json::to_value(failure).unwrap(), "probe_directory");
    }

    #[test]
    fn command_names_per_command_probe_directory_failures() {
        let failure = command_error(ExecError::Io {
            operation: PROBE_DIR_OPERATION,
            source: std::io::Error::other("test directory failure"),
        });
        assert_eq!(serde_json::to_value(failure).unwrap(), "probe_directory");
        for failure in [
            ExecError::Busy,
            ExecError::Io {
                operation: "spawn",
                source: std::io::Error::other("test spawn failure"),
            },
        ] {
            assert_eq!(command_error(failure), ProbeFailure::MissingCli);
        }
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
            .claude(&account, None, now() + 60, &mut QuotaDetails::default())
            .unwrap_err();
        assert_eq!(serde_json::to_value(failure).unwrap(), "probe_directory");
    }
}
