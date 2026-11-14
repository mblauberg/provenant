//! Live quota effects: the one provider module that runs probes and reads
//! credentials. Credentials and unredacted captures never leave it.
pub(crate) mod agy;
pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod cursor;
pub(crate) mod opencode;

use crate::accounts::Settings;
use crate::host::PROBE_DIR_OPERATION;

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
/// schedule; unavailable probe infrastructure does not imply an account failure.
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
#[derive(Clone)]
pub(crate) struct Account {
    scope: AccountScope,
    identity_dir: PathBuf,
    options: Value,
}
pub struct LiveQuota {
    home: PathBuf,
    state: PathBuf,
    roots: Option<provenant_core::Roots>,
    accounts: RefCell<Vec<Account>>,
    refresh_expired: bool,
    refresh_pending: RefCell<bool>,
    refresh_min_budget_seconds: u64,
    refresh_budget_seconds: u64,
    access_expiry: RefCell<Option<i64>>,
    signed_in: RefCell<Option<bool>>,
    login_expiry: RefCell<Option<i64>>,
    readiness_failure: RefCell<Option<provenant_quota::ReadinessReason>>,
    last_reconcile_error: RefCell<Option<String>>,
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

impl LiveQuota {
    /// Probes for the registered accounts, with private probe directories
    /// under the host state root and `home` as each probe's `HOME`.
    pub fn new(
        roots: &provenant_core::Roots,
        home: &Path,
        settings: &Settings,
    ) -> Result<Self, QuotaError> {
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
            state: roots.state.as_path().to_owned(),
            roots: Some(roots.clone()),
            accounts: RefCell::new(accounts),
            refresh_expired: true,
            refresh_pending: RefCell::new(false),
            refresh_min_budget_seconds: 60,
            refresh_budget_seconds: 120,
            access_expiry: RefCell::new(None),
            signed_in: RefCell::new(None),
            login_expiry: RefCell::new(None),
            readiness_failure: RefCell::new(None),
            last_reconcile_error: RefCell::new(None),
            clock: SystemClock::new(),
            #[cfg(test)]
            executor: None,
        })
    }
    pub fn with_refresh_budget(mut self, seconds: u64) -> Self {
        self.refresh_budget_seconds = seconds;
        self
    }
    pub fn with_refresh_minimum(mut self, seconds: u64) -> Self {
        self.refresh_min_budget_seconds = seconds;
        self
    }
    pub fn last_reconcile_error(&self) -> Option<String> {
        self.last_reconcile_error.borrow().clone()
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
                *self.readiness_failure.borrow_mut() = Some(failure.readiness_reason());
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
impl LiveQuota {
    pub(crate) fn capture_source(
        &self,
        plan: &QuotaProbePlan,
        _previous: Option<&AccountRecord>,
        identity: Option<fn(&Path) -> Option<String>>,
        operation: impl FnOnce(
            &LiveQuota,
            &Account,
            i64,
            &mut QuotaDetails,
        ) -> Result<Value, ProbeFailure>,
    ) -> Result<ProbeCapture, ProbeFailure> {
        *self.readiness_failure.borrow_mut() = None;
        *self.login_expiry.borrow_mut() = None;
        *self.access_expiry.borrow_mut() = None;
        *self.signed_in.borrow_mut() = None;
        *self.refresh_pending.borrow_mut() = false;
        let account = self
            .accounts
            .borrow()
            .iter()
            .find(|account| account.scope == plan.account)
            .cloned()
            .ok_or(ProbeFailure::Auth)?;
        let check_identity = || {
            if provenant_records::AdapterSpec::get(&account.scope.adapter)
                .is_some_and(|spec| spec.quota_identity_check)
                && identity
                    .and_then(|read_identity| read_identity(&account.identity_dir))
                    .as_deref()
                    != Some(account.scope.account_key.as_str())
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
        let payload = operation(
            self,
            &account,
            plan.input.deadline_unix_seconds,
            &mut details,
        )?;
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

impl QuotaEffects for LiveQuota {
    fn accounts(&self) -> Result<Vec<AccountScope>, QuotaError> {
        Ok(self
            .accounts
            .borrow()
            .iter()
            .map(|a| a.scope.clone())
            .collect())
    }
    fn capture(
        &self,
        plan: &QuotaProbePlan,
        previous: Option<&AccountRecord>,
    ) -> Result<ProbeCapture, ProbeFailure> {
        let source = crate::REGISTRY
            .iter()
            .filter_map(|registration| registration.quota_source)
            .find(|source| source.adapter_id() == plan.account.adapter)
            .ok_or(ProbeFailure::Disabled)?;
        source.capture(self, plan, previous)
    }

    fn readiness_reason(&self, failure: ProbeFailure) -> provenant_quota::ReadinessReason {
        let reason = failure.readiness_reason();
        if reason == provenant_quota::ReadinessReason::CommandFailure {
            self.readiness_failure.borrow().unwrap_or(reason)
        } else {
            reason
        }
    }

    fn access_token_expiry_unix_seconds(&self) -> Option<i64> {
        *self.access_expiry.borrow()
    }
    fn refresh_pending(&self) -> bool {
        *self.refresh_pending.borrow()
    }
    fn signed_in(&self) -> Option<bool> {
        *self.signed_in.borrow()
    }
    fn login_expiry_unix_seconds(&self) -> Option<i64> {
        *self.login_expiry.borrow()
    }
}

/// The signed-in agy fingerprint, or why the probe cannot trust it. Sign-out
/// and unreadable identity need a login. The caller reconciles a changed key;
/// the identity itself is never part of the failure.
/// Explicit Claude registrations only. No auth stores, identity probes or
/// implicit default directory discovery are involved in chair switching.
pub fn registered_claude(
    settings: &Settings,
) -> Result<Vec<crate::identity::RegisteredAccount>, Box<dyn std::error::Error>> {
    use crate::identity::{Accounts, RegisteredAccount};
    let mut accounts = Vec::new();
    for registration in settings.entries.values() {
        if !provenant_records::AdapterSpec::get(&registration.adapter)
            .is_some_and(|spec| spec.chair_switch)
        {
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
        accounts.push(RegisteredAccount::new(
            &registration.adapter,
            &registration.key,
            path,
        )?);
    }
    Accounts::new(accounts.clone())?;
    Ok(accounts)
}

#[cfg(test)]
mod tests {
    // The worktree driver includes this module without libtest.
    #[allow(unused_imports)]
    use super::agy::agy_signed_in_account;
    #[allow(unused_imports)]
    use super::*;
    #[allow(unused_imports)]
    use crate::identity::agy_fingerprint;
    #[allow(unused_imports)]
    use provenant_os::exec::ExecError;

    #[allow(dead_code)]
    fn quota(home: &Path, state: PathBuf) -> LiveQuota {
        LiveQuota {
            home: home.to_owned(),
            state,
            roots: None,
            accounts: RefCell::new(Vec::new()),
            refresh_expired: false,
            refresh_pending: RefCell::new(false),
            refresh_min_budget_seconds: 60,
            refresh_budget_seconds: 120,
            access_expiry: RefCell::new(None),
            signed_in: RefCell::new(None),
            login_expiry: RefCell::new(None),
            readiness_failure: RefCell::new(None),
            last_reconcile_error: RefCell::new(None),
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
        let old = agy_signed_in_account(true, Some(log("old@example.com"))).unwrap();
        let new = agy_signed_in_account(true, Some(log("new@example.com"))).unwrap();
        assert_ne!(old, new);
        // Signed out, or no identity in the log: still a login problem.
        assert_eq!(
            agy_signed_in_account(false, Some(log("old@example.com"))),
            Err(ProbeFailure::Auth)
        );
        assert_eq!(
            agy_signed_in_account(true, Some(b"no identity".to_vec())),
            Err(ProbeFailure::Auth)
        );
        assert_eq!(agy_signed_in_account(true, None), Err(ProbeFailure::Auth));
    }

    #[test]
    fn agy_identity_refuses_to_cache_when_keychain_changes_during_probe() {
        struct ChangingKeychain {
            inner: AgyUsage,
            keychain: PathBuf,
            atomic: bool,
        }
        impl Executor for ChangingKeychain {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                let outcome = self.inner.execute(options)?;
                if self.atomic {
                    let replacement = self.keychain.with_extension("replacement");
                    fs::write(&replacement, "atomic save").unwrap();
                    fs::rename(replacement, &self.keychain).unwrap();
                } else {
                    fs::write(&self.keychain, "signed in as a different account").unwrap();
                }
                Ok(outcome)
            }
        }
        for (quota_probe, atomic) in [(false, false), (true, false), (true, true)] {
            let root = tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            let state = root.path().join("state");
            let keychain = home.join("Library/Keychains/login.keychain-db");
            fs::create_dir_all(&state).unwrap();
            fs::create_dir_all(keychain.parent().unwrap()).unwrap();
            fs::write(&keychain, "initial").unwrap();
            let stamp = crate::agy::keychain_stamp(&home);
            let homes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let executor = ChangingKeychain {
                inner: AgyUsage {
                    sign_ins: vec![(home.clone(), "fixture@example.test")],
                    homes: homes.clone(),
                },
                keychain,
                atomic,
            };
            let key = format!(
                "agy-{}",
                agy_fingerprint(b"OAuth: authenticated successfully as fixture@example.test\n")
                    .unwrap()
            );
            let mut live = quota(&home, state.clone());
            if quota_probe {
                live.executor = Some(Box::new(executor));
                let account = Account {
                    scope: AccountScope {
                        adapter: "agy".into(),
                        account_key: key,
                        label: "test".into(),
                    },
                    identity_dir: home.clone(),
                    options: Value::Null,
                };
                assert_eq!(
                    live.agy(&account, live.now() + 60, &mut QuotaDetails::default()),
                    Err(ProbeFailure::Network)
                );
            } else {
                assert_eq!(
                    crate::accounts::detect(
                        &executor,
                        "agy",
                        Path::new("agy"),
                        &home,
                        std::ffi::OsStr::new("test"),
                        &home,
                        &state,
                    )
                    .unwrap(),
                    key
                );
            }
            assert_eq!(homes.lock().unwrap().len(), 1);
            assert_ne!(crate::agy::keychain_stamp(&home), stamp);
            assert!(
                crate::agy::cached_identity(&state, &home, live.now()).is_none(),
                "probe cached a pre-switch identity (quota_probe={quota_probe})"
            );
        }
    }

    #[test]
    fn agy_switch_rebinds_registration_and_excludes_the_stale_key() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        let mut inputs = provenant_core::RootInputs::new(home.clone(), root.path().join("repo"));
        inputs.state_dir = Some(state.clone());
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        let key = |email: &str| {
            format!(
                "agy-{}",
                agy_fingerprint(
                    format!("OAuth: authenticated successfully as {email}\n").as_bytes()
                )
                .unwrap()
            )
        };
        let old = key("old@example.test");
        let new = key("new@example.test");
        let config = format!(
            "schema_version=1\n[domains.other]\nuntouched=true\n[domains.accounts.entries.agy-work]\nadapter='agy'\nlabel='work'\nkey='{old}'\ndirectory='{}'\npriority=7\n",
            home.display()
        );
        fs::write(state.join("config.toml"), &config).unwrap();
        let settings: Settings = provenant_config::read_domain(&state, "accounts").unwrap();
        let mut live = LiveQuota::new(&roots, &home, &settings).unwrap();
        let homes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let fake = |email| {
            Box::new(AgyUsage {
                sign_ins: vec![(home.clone(), email)],
                homes: homes.clone(),
            })
        };
        live.executor = Some(fake("old@example.test"));
        let account = live.accounts.borrow()[0].clone();
        live.agy(&account, live.now() + 60, &mut QuotaDetails::default())
            .unwrap();
        live.executor = Some(fake("new@example.test"));
        let outcome = live
            .agy(&account, live.now() + 60, &mut QuotaDetails::default())
            .map(|_| unreachable!());
        assert_eq!(outcome, Err(ProbeFailure::AccountMismatch));
        let now = live.now();
        let mut stale = AccountRecord::empty(account.scope.clone());
        stale.last_attempt_unix_seconds = Some(now);
        stale.claim = Some(provenant_quota::RefreshClaim {
            token: "switch".into(),
            deadline_unix_seconds: now + provenant_quota::schedule::TICK_BUDGET_SECONDS,
        });
        let stale = provenant_quota::schedule::complete(&stale, "switch", now, outcome, None)
            .unwrap()
            .replacement
            .unwrap();
        let admission = provenant_quota::Admission {
            policies: &[],
            records: std::slice::from_ref(&stale),
            pools: &[],
            now_unix_seconds: now,
        };
        assert_eq!(
            admission.account_block("agy", &old, "model").unwrap(),
            Some(provenant_quota::AccountBlock::Mismatch)
        );
        assert!(
            admission
                .account_block("agy", &new, "model")
                .unwrap()
                .is_none()
        );
        assert_eq!(live.accounts().unwrap()[0].account_key, new);
        let saved: Settings = provenant_config::read_domain(&state, "accounts").unwrap();
        assert_eq!(saved.entries["agy-work"].key, new);
        assert_eq!(saved.entries["agy-work"].priority, Some(7));
        // A delayed probe cannot overwrite a newer manual registration.
        assert!(
            !crate::accounts::reconcile_agy(&roots, &home, &old, &key("third@example.test"))
                .unwrap()
        );
        let unchanged: Settings = provenant_config::read_domain(&state, "accounts").unwrap();
        assert_eq!(unchanged.entries["agy-work"].key, new);
        assert!(
            fs::read_to_string(state.join("config.toml"))
                .unwrap()
                .contains("untouched=true")
        );
        let next = LiveQuota::new(&roots, &home, &saved).unwrap();
        assert_eq!(next.accounts().unwrap()[0].account_key, new);
        assert_eq!(
            crate::agy::cached_identity(&state, &home, live.now()),
            Some(new)
        );
        assert_eq!(homes.lock().unwrap().len(), 2);
    }

    #[test]
    fn agy_switch_surfaces_rebind_error_when_new_key_already_registered() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let other = root.path().join("other");
        let state = root.path().join("state");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::create_dir_all(&state).unwrap();
        let mut inputs = provenant_core::RootInputs::new(home.clone(), root.path().join("repo"));
        inputs.state_dir = Some(state.clone());
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        let key = |email: &str| {
            format!(
                "agy-{}",
                agy_fingerprint(
                    format!("OAuth: authenticated successfully as {email}\n").as_bytes()
                )
                .unwrap()
            )
        };
        let old = key("old@example.test");
        let new = key("new@example.test");
        let config = format!(
            "schema_version=1\n[domains.accounts.entries.agy-work]\nadapter='agy'\nlabel='work'\nkey='{old}'\ndirectory='{}'\n[domains.accounts.entries.agy-other]\nadapter='agy'\nlabel='other'\nkey='{new}'\ndirectory='{}'\n",
            home.display(),
            other.display()
        );
        fs::write(state.join("config.toml"), &config).unwrap();
        let settings: Settings = provenant_config::read_domain(&state, "accounts").unwrap();
        let mut live = LiveQuota::new(&roots, &home, &settings).unwrap();
        let homes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let fake = |email| {
            Box::new(AgyUsage {
                sign_ins: vec![(home.clone(), email)],
                homes: homes.clone(),
            })
        };
        live.executor = Some(fake("new@example.test"));
        let account = live
            .accounts
            .borrow()
            .iter()
            .find(|a| a.scope.account_key == old)
            .unwrap()
            .clone();
        let outcome = live
            .agy(&account, live.now() + 60, &mut QuotaDetails::default())
            .map(|_| unreachable!());
        assert_eq!(outcome, Err(ProbeFailure::AccountMismatch));
        let error = live
            .last_reconcile_error()
            .expect("rebind error must be recorded");
        assert!(
            error.contains("agy identity is already registered; run `provenant accounts list`"),
            "{error}"
        );
        // Stale key remains excluded from admission
        let now = live.now();
        let mut stale = AccountRecord::empty(account.scope.clone());
        stale.last_attempt_unix_seconds = Some(now);
        stale.claim = Some(provenant_quota::RefreshClaim {
            token: "refused-rebind".into(),
            deadline_unix_seconds: now + provenant_quota::schedule::TICK_BUDGET_SECONDS,
        });
        let stale =
            provenant_quota::schedule::complete(&stale, "refused-rebind", now, outcome, None)
                .unwrap()
                .replacement
                .unwrap();
        let admission = provenant_quota::Admission {
            policies: &[],
            records: std::slice::from_ref(&stale),
            pools: &[],
            now_unix_seconds: now,
        };
        assert_eq!(
            admission.account_block("agy", &old, "model").unwrap(),
            Some(provenant_quota::AccountBlock::Mismatch)
        );
        // Config was not rewritten with duplicate key
        let saved: Settings = provenant_config::read_domain(&state, "accounts").unwrap();
        assert_eq!(saved.entries["agy-work"].key, old);
        assert_eq!(saved.entries["agy-other"].key, new);
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

        struct ProbeExecutor(bool);
        impl Executor for ProbeExecutor {
            fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                Ok(ExecOutcome {
                    termination: Termination::Exit(1),
                    stdout: Default::default(),
                    stderr: Default::default(),
                    timed_out: self.0,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 0,
                })
            }
        }
        let temp = tempfile::tempdir().unwrap();
        for (timed_out, expected) in [
            (true, provenant_quota::ReadinessReason::ProbeTimeout),
            (false, provenant_quota::ReadinessReason::CommandFailure),
        ] {
            let mut quota = quota(temp.path(), temp.path().to_owned());
            quota.executor = Some(Box::new(ProbeExecutor(timed_out)));
            let failure = quota
                .http("https://fixture.invalid", &[], quota.now() + 60)
                .unwrap_err();
            assert_eq!(failure, ProbeFailure::Network);
            assert_eq!(quota.readiness_reason(failure), expected);
            for (failure, expected) in [
                (
                    ProbeFailure::MissingCli,
                    provenant_quota::ReadinessReason::MissingExecutable,
                ),
                (
                    ProbeFailure::Auth,
                    provenant_quota::ReadinessReason::SignedOut,
                ),
                (
                    ProbeFailure::AccountMismatch,
                    provenant_quota::ReadinessReason::AccountMismatch,
                ),
            ] {
                assert_eq!(quota.readiness_reason(failure), expected);
            }
        }
    }

    fn claude_selected_identity(scenario: u8) {
        use std::ffi::OsStr;
        struct Selected {
            dir: PathBuf,
            expires_ms: i64,
            refreshes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
            refusal: bool,
            credential_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        impl Executor for Selected {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                let program = options.argv[0].to_string_lossy();
                let bytes = if program == "/usr/bin/security" {
                    self.credential_reads
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let suffix =
                        crate::claude::keychain_suffix(self.dir.as_os_str().as_encoded_bytes());
                    assert_eq!(
                        options.argv[3],
                        OsString::from(format!("Claude Code-credentials-{suffix}"))
                    );
                    fs::read(self.dir.join(".credentials.json")).unwrap()
                } else if program == "claude" {
                    assert_eq!(options.argv[1], OsStr::new("auth"));
                    assert_eq!(options.argv[2], OsStr::new("status"));
                    let env = options.env.unwrap();
                    let (_, value) = env
                        .iter()
                        .find(|(key, _)| key == "CLAUDE_CONFIG_DIR")
                        .expect("probe only the selected login, never the default");
                    assert_eq!(value, self.dir.as_os_str());
                    br#"{"loggedIn":true,"email":"selected@example.test","loginExpiresAt":"2100-01-01T00:00:00Z"}"#.to_vec()
                } else if program == "/fixture/claude" {
                    assert!(options.wait_for_completion);
                    assert!(options.inherited_lease.is_some());
                    self.refreshes
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    fs::write(self.dir.join(".credentials.json"), format!(r#"{{"claudeAiOauth":{{"accessToken":"fake-renewed","expiresAt":{}}}}}"#, self.expires_ms + 7_200_000)).unwrap();
                    Vec::new()
                } else {
                    assert_eq!(program, "/usr/bin/curl");
                    let request = self
                        .requests
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if self.refusal && request == 0 {
                        b"{}\n401".to_vec()
                    } else {
                        b"{}\n200".to_vec()
                    }
                };
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
                    stdout: provenant_os::exec::CapturedStream {
                        bytes,
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
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let selected = root.path().join("selected");
        let state = root.path().join("state");
        fs::create_dir_all(&selected).unwrap();
        fs::create_dir_all(&state).unwrap();
        let mut live = quota(&home, state);
        let credential_reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let refreshes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let expires_ms = (live.now() + 3600) * 1000;
        fs::write(
            selected.join(".credentials.json"),
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"fake-selected","expiresAt":{expires_ms}}}}}"#
            ),
        )
        .unwrap();
        live.executor = Some(Box::new(Selected {
            dir: selected.clone(),
            expires_ms,
            refreshes: refreshes.clone(),
            requests: requests.clone(),
            refusal: scenario == 3,
            credential_reads: credential_reads.clone(),
        }));
        let account = Account {
            scope: AccountScope {
                adapter: "claude".into(),
                account_key: crate::identity::fingerprint("selected@example.test").unwrap(),
                label: "selected".into(),
            },
            identity_dir: selected,
            options: Value::Null,
        };
        if scenario == 0 || scenario == 3 {
            live.refresh_expired = scenario == 3;
            assert!(
                live.claude(&account, live.now() + 120, &mut QuotaDetails::default())
                    .is_ok()
            );
            assert_eq!(live.login_expiry_unix_seconds(), Some(4102444800));
            assert_eq!(live.signed_in(), Some(true));
            assert_eq!(
                refreshes.load(std::sync::atomic::Ordering::Relaxed),
                usize::from(scenario == 3)
            );
            assert_eq!(
                requests.load(std::sync::atomic::Ordering::Relaxed),
                if scenario == 3 { 2 } else { 1 }
            );
            return;
        }
        if scenario == 1 {
            let mut wrong_account = account.clone();
            wrong_account.scope.account_key =
                crate::identity::fingerprint("other@example.test").unwrap();
            let before = credential_reads.load(std::sync::atomic::Ordering::Relaxed);
            // If the mismatched account opens this fixture before checking its
            // identity, parsing fails instead of producing AccountMismatch.
            fs::write(
                account.identity_dir.join(".credentials.json"),
                b"fake-unrelated-secret",
            )
            .unwrap();
            *live.login_expiry.borrow_mut() = None;
            assert_eq!(
                live.claude(
                    &wrong_account,
                    live.now() + 120,
                    &mut QuotaDetails::default()
                ),
                Err(ProbeFailure::AccountMismatch)
            );
            assert_eq!(
                credential_reads.load(std::sync::atomic::Ordering::Relaxed),
                before
            );
            assert_eq!(live.login_expiry_unix_seconds(), None);
            return;
        }
        live.refresh_expired = true;
        let expires_ms = (live.now() - 60) * 1000;
        fs::write(
            account.identity_dir.join(".credentials.json"),
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"fake-selected","refreshToken":"fake-shared","expiresAt":{expires_ms}}}}}"#
            ),
        )
        .unwrap();
        live.executor = Some(Box::new(Selected {
            dir: account.identity_dir.clone(),
            expires_ms,
            refreshes: refreshes.clone(),
            requests,
            refusal: false,
            credential_reads,
        }));
        if scenario == 4 {
            live.refresh_budget_seconds = 30;
            assert_eq!(
                live.claude(&account, live.now() + 120, &mut QuotaDetails::default()),
                Err(ProbeFailure::RefreshDeferred)
            );
            assert_eq!(live.signed_in(), Some(true));
            assert!(live.refresh_pending());
            assert_eq!(
                live.access_token_expiry_unix_seconds(),
                Some(expires_ms / 1000)
            );
            assert_eq!(refreshes.load(std::sync::atomic::Ordering::Relaxed), 0);
            return;
        }
        assert!(
            live.claude(&account, live.now() + 120, &mut QuotaDetails::default())
                .is_ok()
        );
        assert_eq!(refreshes.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(live.login_expiry_unix_seconds(), Some(4102444800));
        assert_eq!(
            live.access_token_expiry_unix_seconds(),
            Some(expires_ms / 1000 + 7200)
        );
        assert!(!live.state.join("quota-claude-refresh.json").exists());
    }

    #[test]
    fn claude_quota_reads_selected_identity() {
        claude_selected_identity(0);
    }
    #[test]
    fn claude_quota_rejects_mismatch_before_credential_read() {
        claude_selected_identity(1);
    }
    #[test]
    fn claude_quota_refreshes_selected_login_for_same_account() {
        claude_selected_identity(2);
    }

    #[test]
    fn claude_usage_refusal_refreshes_once_then_retries() {
        claude_selected_identity(3);
    }

    #[test]
    fn claude_expired_access_token_with_signed_in_status_defers_without_refresh() {
        claude_selected_identity(4);
    }

    #[test]
    fn claude_distinct_account_retries_after_transient_refresh_failure() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Fake {
            dir: PathBuf,
            count: Arc<AtomicUsize>,
            renewed: i64,
        }
        impl Executor for Fake {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                let program = options.argv[0].to_string_lossy();
                let bytes = if program == "claude" {
                    let selected = options
                        .env
                        .unwrap()
                        .iter()
                        .any(|(key, _)| key == "CLAUDE_CONFIG_DIR");
                    if selected {
                        br#"{"loggedIn":true,"email":"selected@example.test"}"#.to_vec()
                    } else {
                        br#"{"loggedIn":true,"email":"default@example.test"}"#.to_vec()
                    }
                } else if program == "/usr/bin/security" {
                    fs::read(self.dir.join(".credentials.json")).unwrap()
                } else if program == "/fixture/claude" {
                    if self.count.fetch_add(1, Ordering::Relaxed) == 0 {
                        return Err(ExecError::Busy);
                    }
                    fs::write(
                        self.dir.join(".credentials.json"),
                        format!(
                            r#"{{"claudeAiOauth":{{"accessToken":"fake-new","expiresAt":{}}}}}"#,
                            self.renewed
                        ),
                    )
                    .unwrap();
                    Vec::new()
                } else {
                    assert_eq!(program, "/usr/bin/curl");
                    b"{}\n200".to_vec()
                };
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
                    stdout: provenant_os::exec::CapturedStream {
                        bytes,
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
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let selected = home.join(".claude-work");
        let state = temp.path().join("state");
        fs::create_dir_all(&selected).unwrap();
        fs::create_dir_all(&state).unwrap();
        let mut live = quota(&home, state);
        live.refresh_expired = true;
        let expiry = (live.now() - 60) * 1000;
        fs::write(selected.join(".credentials.json"), format!(r#"{{"claudeAiOauth":{{"accessToken":"fake-old","refreshToken":"fake-refresh","expiresAt":{expiry}}}}}"#)).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        live.executor = Some(Box::new(Fake {
            dir: selected.clone(),
            count: count.clone(),
            renewed: (live.now() + 3600) * 1000,
        }));
        let account = Account {
            scope: AccountScope {
                adapter: "claude".into(),
                account_key: crate::identity::fingerprint("selected@example.test").unwrap(),
                label: "work".into(),
            },
            identity_dir: selected,
            options: Value::Null,
        };
        assert_eq!(
            live.claude(&account, live.now() + 120, &mut QuotaDetails::default()),
            Err(ProbeFailure::Network)
        );
        assert_eq!(live.signed_in(), Some(true));
        assert!(live.refresh_pending());
        assert!(
            live.claude(&account, live.now() + 120, &mut QuotaDetails::default())
                .is_ok()
        );
        assert_eq!(count.load(Ordering::Relaxed), 2);
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
            .claude(&account, quota.now() + 60, &mut QuotaDetails::default())
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
        let mut inputs =
            provenant_core::RootInputs::new("/fixture/home".into(), "/fixture/repo".into());
        inputs.state_dir = Some("/fixture/state".into());
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        let live = LiveQuota::new(&roots, Path::new("/fixture/home"), &settings).unwrap();
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
            let snapshot = crate::REGISTRY
                .iter()
                .filter_map(|registration| registration.quota_source)
                .find(|source| source.adapter_id() == plan.account.adapter)
                .unwrap()
                .decode(&plan, &capture)
                .unwrap();
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
