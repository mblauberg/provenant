//! Live quota effects. Credentials and unredacted captures never leave this edge.
mod claude;
mod opencode;

use provenant_quota::{
    AccountRecord, AccountScope, ProbeCapture, ProbeFailure, QuotaDetails, QuotaEffects,
    QuotaError, QuotaProbePlan,
};
use provenant_records::CanonicalV1;
use provenant_supervisor::exec::{
    ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor, Termination,
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
fn fingerprint(identity: &str, length: usize) -> String {
    let digest = CanonicalV1::file_digest(identity.trim().to_lowercase().as_bytes()).to_string();
    digest.strip_prefix("sha256:").unwrap_or(&digest)[..length].to_owned()
}

fn expand(home: &Path, path: &str) -> PathBuf {
    if path == "~" {
        home.to_path_buf()
    } else if let Some(path) = path.strip_prefix("~/") {
        home.join(path)
    } else {
        PathBuf::from(path)
    }
}
fn default_dir(home: &Path, adapter: &str) -> PathBuf {
    match adapter {
        "claude" => home.join(".claude"),
        "codex" => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex")),
        "cursor" if cfg!(target_os = "macos") => {
            home.join("Library/Application Support/Cursor/User/globalStorage")
        }
        "cursor" => home.join(".config/Cursor/User/globalStorage"),
        "agy" => home.join(".gemini/antigravity-cli"),
        _ => home.join(".local/share/opencode"),
    }
}
impl LiveQuota {
    pub(crate) fn new(state: &Path) -> Result<Self, QuotaError> {
        let home = PathBuf::from(std::env::var_os("HOME").ok_or(QuotaError::Invalid("home"))?);
        let config_root = std::env::var_os("AGENT_FABRIC_INSTANCE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".agents"));
        let config_file = config_root.join("config/telemetry.json");
        let config = match value(&config_file) {
            Ok(v) if v.is_object() => v,
            Err(_) if !config_file.exists() => json!({}),
            _ => return Err(QuotaError::Invalid("accounts configuration")),
        };
        let mut accounts = Vec::new();
        if config.pointer("/quota/enabled").and_then(Value::as_bool) != Some(false) {
            let configured = config.get("accounts").and_then(Value::as_object);
            if config.get("accounts").is_some() && configured.is_none() {
                return Err(QuotaError::Invalid("accounts configuration"));
            }
            for adapter in ["claude", "codex", "agy", "cursor", "opencode"] {
                if let Some(entries) = configured.and_then(|a| a.get(adapter)) {
                    let entries = entries
                        .as_object()
                        .ok_or(QuotaError::Invalid("account entries"))?;
                    for (label, options) in entries {
                        if !options.is_object() {
                            return Err(QuotaError::Invalid("account options"));
                        }
                        if options.get("enabled").and_then(Value::as_bool) == Some(false) {
                            continue;
                        }
                        let dir = options
                            .get("identity_dir")
                            .and_then(Value::as_str)
                            .map(|s| expand(&home, s))
                            .unwrap_or_else(|| default_dir(&home, adapter));
                        if !dir.is_absolute()
                            || dir
                                .components()
                                .any(|c| matches!(c, std::path::Component::ParentDir))
                        {
                            return Err(QuotaError::Invalid("absolute identity directory"));
                        }
                        let scope = AccountScope {
                            adapter: adapter.into(),
                            account_key: label.clone(),
                            label: label.clone(),
                        };
                        scope.validate()?;
                        accounts.push(Account {
                            scope,
                            identity_dir: dir,
                            options: options.clone(),
                        });
                    }
                } else {
                    let dir = default_dir(&home, adapter);
                    if dir.exists() {
                        let label = if adapter == "agy" {
                            agy_identity(&dir).unwrap_or_else(|| "default".into())
                        } else {
                            "default".into()
                        };
                        accounts.push(Account {
                            scope: AccountScope {
                                adapter: adapter.into(),
                                account_key: label.clone(),
                                label,
                            },
                            identity_dir: dir,
                            options: json!({}),
                        });
                    }
                }
            }
        }
        if accounts.len() > provenant_quota::MAX_ACCOUNTS {
            return Err(QuotaError::Invalid("account bound"));
        }
        Ok(Self {
            home,
            state: state.into(),
            accounts,
            refresh_expired: config
                .pointer("/quota/claude_refresh_expired")
                .and_then(Value::as_bool)
                .unwrap_or(true),
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
        let scratch = tempfile::Builder::new()
            .prefix("provenant-quota-")
            .tempdir()
            .map_err(|_| ProbeFailure::Network)?;
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
        let mut options = ExecOptions::new(
            &argv,
            scratch.path(),
            Duration::from_secs((remaining as u64).min(max_seconds)),
        );
        options.env = Some(&env);
        options.stdin = stdin;
        options.stdout = StreamPolicy::Capture { limit: LIMIT };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        SystemExecutor::new(1)
            .and_then(|e| e.execute(options))
            .map_err(|_| ProbeFailure::MissingCli)
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
            .find(|a| a.scope == *plan.account())
            .ok_or(ProbeFailure::Auth)?;
        let deadline = plan.input().deadline_unix_seconds;
        let mut details = QuotaDetails {
            plan: account
                .options
                .get("plan")
                .and_then(Value::as_str)
                .map(str::to_owned),
            priority: account.options.get("priority").and_then(Value::as_u64),
            ..QuotaDetails::default()
        };
        let payload = match account.scope.adapter.as_str() {
            "claude" => self.claude(account, previous, deadline, &mut details)?,
            "codex" => self.codex(account, deadline)?,
            "cursor" => self.cursor(account, deadline)?,
            "agy" => self.agy(account, previous, deadline, &mut details)?,
            "opencode" => {
                details.note = Some("Local estimate, this machine only; calibrated Go caps, other hosts not counted".into());
                opencode::capture(&account.identity_dir, &account.options, now(), deadline)?
            }
            _ => return Err(ProbeFailure::Disabled),
        };
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
            10,
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
        use rusqlite::{Connection, OpenFlags};
        let db = Connection::open_with_flags(
            account.identity_dir.join("state.vscdb"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| ProbeFailure::Auth)?;
        db.busy_timeout(Duration::from_millis(250))
            .map_err(|_| ProbeFailure::Auth)?;
        let token: String = db.query_row("SELECT substr(value,1,1048577) FROM ItemTable WHERE key='cursorAuth/accessToken' LIMIT 1", [], |r| r.get(0)).map_err(|_| ProbeFailure::Auth)?;
        if token.len() > LIMIT {
            return Err(ProbeFailure::Auth);
        }
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
        previous: Option<&AccountRecord>,
        deadline: i64,
        details: &mut QuotaDetails,
    ) -> Result<Value, ProbeFailure> {
        let scratch = tempfile::Builder::new()
            .prefix("provenant-agy-quota-")
            .tempdir()
            .map_err(|_| ProbeFailure::Network)?;
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
            30,
        )?;
        if payload.get("status").and_then(Value::as_str) != Some("SUCCESS") {
            return Err(ProbeFailure::Auth);
        }
        let actual = read(&log).ok().and_then(|b| agy_fingerprint(&b));
        let expected = account
            .options
            .get("fingerprint")
            .and_then(Value::as_str)
            .or_else(|| {
                previous
                    .and_then(|p| p.snapshot.as_ref())
                    .and_then(|s| s.details.as_ref())
                    .and_then(|d| d.fingerprint.as_deref())
            })
            .or_else(|| {
                account
                    .scope
                    .label
                    .starts_with("google-")
                    .then_some(account.scope.label.as_str())
            });
        if expected.is_some_and(|e| Some(e) != actual.as_deref()) {
            return Err(ProbeFailure::Auth);
        }
        let actual = actual.ok_or(ProbeFailure::Auth)?;
        details.fingerprint = Some(actual);
        Ok(payload)
    }
}
fn base64url(s: &str) -> Option<Vec<u8>> {
    let (mut bits, mut count) = (0u32, 0u32);
    let mut bytes = Vec::new();
    for c in s.bytes().take_while(|b| *b != b'=') {
        let n = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        bits = (bits << 6) | n as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(bytes)
}
fn agy_fingerprint(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (_, tail) = text.rsplit_once("OAuth: authenticated successfully as ")?;
    let email = tail.split_whitespace().next().filter(|s| s.contains('@'))?;
    Some(format!("google-{}", fingerprint(email, 12)))
}
fn agy_identity(dir: &Path) -> Option<String> {
    let mut logs: Vec<_> = fs::read_dir(dir.join("log"))
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("cli-") && n.ends_with(".log"))
        })
        .collect();
    logs.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
    logs.iter().take(20).find_map(|e| {
        use std::io::{Seek, SeekFrom};
        let mut f = fs::File::open(e.path()).ok()?;
        let len = f.metadata().ok()?.len();
        f.seek(SeekFrom::Start(len.saturating_sub(LIMIT as u64)))
            .ok()?;
        let mut bytes = Vec::new();
        f.take(LIMIT as u64).read_to_end(&mut bytes).ok()?;
        agy_fingerprint(&bytes)
    })
}
