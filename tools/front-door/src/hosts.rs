//! Non-billable sign-in diagnostics. Captures remain bounded and in memory.
use crate::{emit, fail, roots};
use provenant_cli::{
    Globals, Hosts,
    fabric::{Context, PeerFailure, facade_outcome},
};
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_fabric_runtime::hosts::{PeerAction, PeerResult};
use provenant_os::{
    clock::SystemClock,
    exec::{ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor, Termination},
};
use provenant_providers::claude::{CredentialPresence, SigninStatus, signin_status};
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Serialize, Deserialize)]
struct Identity {
    label: String,
    signin: SigninStatus,
}
/// Whether one registered account holds the managed settings, hooks and skills
/// of the installed release. `accounts sync` repairs any state but `synced`.
#[derive(Serialize, Deserialize)]
struct Managed {
    adapter: String,
    label: String,
    state: provenant_config::AccountState,
    /// Findings inside the account's directory; `check install` names each.
    findings: u64,
}
#[derive(Serialize, Deserialize)]
struct AgyAccount {
    label: String,
    directory: String,
    keychain: String,
    keychain_available: bool,
}
#[derive(Serialize, Deserialize)]
struct LoginAccount {
    adapter: String,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_key: Option<String>,
    reason: Option<provenant_quota::ProbeFailure>,
    last_success_unix_seconds: Option<i64>,
    login_expiry: Option<provenant_quota::LoginExpiryView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    readiness: Option<provenant_quota::ReadinessRead>,
    repair: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct InstalledSignin {
    adapter: String,
    state: provenant_providers::host::LocalSignin,
}
fn login_account(row: provenant_quota::AccountRow) -> LoginAccount {
    LoginAccount {
        adapter: row.account.adapter,
        label: row.account.label,
        account_key: Some(row.account.account_key),
        reason: row.error,
        last_success_unix_seconds: row.snapshot.map(|snapshot| snapshot.observed_unix_seconds),
        login_expiry: row.login_expiry,
        readiness: row.readiness,
        repair: row.repair,
    }
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Report {
    claude: Vec<Identity>,
    login_accounts: Vec<LoginAccount>,
    /// Installed adapters with no account observation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unobserved_adapters: Vec<String>,
    /// Fresh local sign-in checks, separate from recorded account readiness.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    installed_sign_ins: Vec<InstalledSignin>,
    /// The executable lanes can resolve through their read-only Rust toolchain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nextest: Option<String>,
    /// The macOS login-session peer agent.
    peer_agent: provenant_fabric_runtime::peer_agent::Status,
    #[serde(default)]
    tick_agent: crate::tick_agent::Diagnosis,
    /// Every registered account of every adapter.
    #[serde(default)]
    managed: Vec<Managed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    agy_accounts: Vec<AgyAccount>,
    /// Why `managed` could not be read, when it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    managed_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}
fn executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|p| p.is_absolute())
        .map(|p| p.join(name))
        .find(|p| provenant_os::sys::is_executable(p))
}
fn unobserved_adapters(accounts: &[LoginAccount], installed: impl Fn(&str) -> bool) -> Vec<String> {
    provenant_records::ADAPTER_SPECS
        .iter()
        .filter(|spec| {
            !spec.binary.is_empty()
                && installed(spec.binary)
                && !accounts.iter().any(|row| row.adapter == spec.name)
        })
        .map(|spec| spec.name.to_owned())
        .collect()
}
fn installed_sign_ins(
    adapters: &[String],
    home: &Path,
    executable: impl Fn(&str) -> Option<PathBuf>,
    host_var: impl Fn(&str) -> Option<OsString>,
    executor: &dyn Executor,
) -> Vec<InstalledSignin> {
    adapters
        .iter()
        .filter_map(|name| {
            let spec = provenant_records::AdapterSpec::get(name)?;
            let executable = executable(spec.binary)?;
            let account_dir = spec
                .default_account_directory
                .map(|(env, fallback)| {
                    host_var(env)
                        .map(PathBuf::from)
                        .unwrap_or_else(|| home.join(fallback))
                })
                .unwrap_or_else(|| home.to_owned());
            let state = provenant_providers::host::local_signin(
                spec.login_reprobe,
                executor,
                &executable,
                home,
                &account_dir,
                Path::new("/"),
            )?;
            Some(InstalledSignin {
                adapter: name.clone(),
                state,
            })
        })
        .collect()
}
fn lane_nextest(home: &Path) -> Option<String> {
    let path = home.join(".cargo/bin/cargo-nextest");
    provenant_os::sys::is_executable(&path).then(|| path.display().to_string())
}
fn probe(
    argv: &[OsString],
    home: &Path,
    config: Option<&Path>,
    seconds: u64,
) -> Result<ExecOutcome, Box<dyn std::error::Error>> {
    // No inherited model credentials, hooks or provider configuration selectors.
    let mut env = vec![
        ("HOME".into(), home.as_os_str().to_owned()),
        ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
    ];
    if config.is_none()
        && let Some(socket) = std::env::var_os("SSH_AUTH_SOCK")
    {
        env.push(("SSH_AUTH_SOCK".into(), socket));
    }
    if let Some(config) = config {
        env.push(("CLAUDE_CONFIG_DIR".into(), config.as_os_str().to_owned()));
    }
    let mut options = ExecOptions::new(argv, Path::new("/"), Duration::from_secs(seconds));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stderr = StreamPolicy::Discard;
    options.stop_on_output_limit = true;
    Ok(SystemExecutor::new(1)?.execute(options)?)
}
fn complete(out: &ExecOutcome) -> bool {
    !out.timed_out && !out.drain_truncated && !out.stdout.truncated && !out.stdout.read_failed
}
fn presence(home: &Path, config: &Path) -> CredentialPresence {
    // Existence is enough; never open the credential file or ask security for
    // its password (-w/-g). Locked/inaccessible stores remain indeterminate.
    match fs::symlink_metadata(config.join(".credentials.json")) {
        Ok(meta) if meta.file_type().is_file() && meta.len() > 0 => {
            return CredentialPresence::Present;
        }
        Ok(meta) if !meta.file_type().is_file() => return CredentialPresence::Unknown,
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return CredentialPresence::Unknown,
    }
    if !cfg!(target_os = "macos") {
        return CredentialPresence::Absent;
    }
    let mut service = "Claude Code-credentials".to_owned();
    if config != home.join(".claude") {
        use std::os::unix::ffi::OsStrExt;
        service.push('-');
        service.push_str(&keychain_suffix(config.as_os_str().as_bytes()));
    }
    let argv = [
        "/usr/bin/security".into(),
        "find-generic-password".into(),
        "-s".into(),
        service.into(),
    ];
    // The metadata is not needed, so discard both streams rather than retain it.
    let env = [(OsString::from("HOME"), home.as_os_str().to_owned())];
    let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(2));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Discard;
    options.stderr = StreamPolicy::Discard;
    match SystemExecutor::new(1).and_then(|executor| executor.execute(options)) {
        Ok(out) if !out.timed_out => match out.termination {
            Termination::Exit(0) => CredentialPresence::Present,
            // errSecItemNotFound, distinct from locked keychain/access errors.
            Termination::Exit(44) => CredentialPresence::Absent,
            _ => CredentialPresence::Unavailable,
        },
        _ => CredentialPresence::Unavailable,
    }
}
/// Reads the receipt and the files it owns, never a credential. A configuration
/// or receipt defect is reported in place of rows rather than failing doctor.
fn managed(roots: &provenant_core::Roots, home: &Path) -> (Vec<Managed>, Option<String>) {
    let config = match provenant_cli::config_reads::read_for(roots, "accounts") {
        Ok(config) => config,
        Err(error) => return (Vec::new(), Some(error.to_string())),
    };
    let mut targets = provenant_config::Targets::new(roots.clone(), home, &[]);
    targets.accounts = provenant_cli::accounts::homes(&config.domains.accounts);
    targets.shared_settings = crate::install::shared_settings(&config.domains.accounts, home)
        .ok()
        .flatten();
    match provenant_config::inspect_accounts(&targets) {
        Ok(statuses) => (
            statuses
                .into_iter()
                .map(|status| Managed {
                    adapter: status.harness.spec().name.to_owned(),
                    label: status.label,
                    state: status.state,
                    findings: status.findings.len() as u64,
                })
                .collect(),
            None,
        ),
        Err(error) => (Vec::new(), Some(error.to_string())),
    }
}
fn agy_accounts(
    accounts: &provenant_providers::accounts::Settings,
    home: &Path,
) -> Vec<AgyAccount> {
    accounts
        .entries
        .values()
        .filter(|entry| {
            provenant_records::AdapterSpec::get(&entry.adapter)
                .is_some_and(|spec| spec.isolated_login_keychain)
        })
        .map(|entry| {
            let keychain = entry.directory.join("Library/Keychains/login.keychain-db");
            let keychain_available =
                provenant_providers::agy::account_keychain(&entry.directory, home).is_ok();
            AgyAccount {
                label: entry.label.clone(),
                directory: entry.directory.display().to_string(),
                keychain: keychain.display().to_string(),
                keychain_available,
            }
        })
        .collect()
}
pub(crate) fn local(
    home: &Path,
    roots: &provenant_core::Roots,
) -> Result<Report, Box<dyn std::error::Error>> {
    let state = roots.state.as_path();
    let mut identities = vec![("default (~/.claude)".to_owned(), home.join(".claude"))];
    for entry in fs::read_dir(home)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(label) = name.to_str().and_then(|n| n.strip_prefix(".claude-")) else {
            continue;
        };
        if label.is_empty()
            || label.len() > 128
            || label.chars().any(char::is_control)
            || !entry.path().is_dir()
        {
            continue;
        }
        if identities.len() >= 128 {
            return Err("too many Claude identities".into());
        }
        identities.push((label.into(), entry.path()));
    }
    identities.sort_by(|a, b| a.0.cmp(&b.0));
    let cli = executable("claude");
    let mut claude = Vec::new();
    for (label, config) in identities {
        let status = cli.as_ref().and_then(|cli| {
            probe(
                &[
                    cli.as_os_str().to_owned(),
                    "auth".into(),
                    "status".into(),
                    "--json".into(),
                ],
                home,
                Some(&config),
                2,
            )
            .ok()
        });
        let signin = match status {
            Some(out) if complete(&out) => {
                let exit = match out.termination {
                    Termination::Exit(code) => Some(code),
                    _ => None,
                };
                let signed_in = signin_status(&out.stdout.bytes, exit, CredentialPresence::Unknown);
                if signed_in.state == provenant_providers::claude::SigninState::SignedIn {
                    signed_in
                } else {
                    signin_status(&out.stdout.bytes, exit, presence(home, &config))
                }
            }
            _ => signin_status(&[], None, CredentialPresence::Unknown),
        };
        claude.push(Identity { label, signin });
    }
    let (managed, managed_error) = managed(roots, home);
    let agy_accounts = provenant_cli::config_reads::read_for(roots, "accounts")
        .ok()
        .map(|config| agy_accounts(&config.domains.accounts, home))
        .unwrap_or_default();
    let mut warnings = crate::pool_effort_warnings(roots);
    let login_accounts = match provenant_cli::quota::recorded_login_rows(roots) {
        Ok(rows) => rows.into_iter().map(login_account).collect(),
        Err(_) => {
            warnings.push("login observations unavailable; run `provenant quota`".into());
            Vec::new()
        }
    };
    let unobserved_adapters =
        unobserved_adapters(&login_accounts, |binary| executable(binary).is_some());
    let installed_sign_ins = SystemExecutor::new(1)
        .ok()
        .map_or_else(Vec::new, |executor| {
            installed_sign_ins(
                &unobserved_adapters,
                home,
                executable,
                |name| std::env::var_os(name),
                &executor,
            )
        });
    Ok(Report {
        claude,
        login_accounts,
        unobserved_adapters,
        installed_sign_ins,
        nextest: lane_nextest(home),
        peer_agent: provenant_fabric_runtime::peer_agent::status(state, home),
        tick_agent: crate::tick_agent::diagnosis(state, home),
        managed,
        agy_accounts,
        managed_error,
        warnings,
    })
}
struct DoctorError {
    outcome: Outcome,
    code: &'static str,
    message: String,
}
impl DoctorError {
    fn local(error: impl std::fmt::Display) -> Self {
        Self {
            outcome: Outcome::Unavailable,
            code: "SIGNIN_PROBE",
            message: error.to_string(),
        }
    }
    fn unknown(name: &str) -> Self {
        Self {
            outcome: Outcome::Invalid,
            code: "UNKNOWN_HOST",
            message: format!(
                "peer {name} is not configured; run `provenant hosts` to list configured peers"
            ),
        }
    }
    fn peer(failure: PeerFailure) -> Self {
        let PeerFailure { error, detail } = failure;
        let message = match detail {
            Some(detail) => format!("{error} ({detail})"),
            None => error.to_string(),
        };
        let (outcome, kind) = facade_outcome(&error);
        Self {
            outcome,
            code: kind.code(),
            message: format!("peer: {message}"),
        }
    }
    fn malformed() -> Self {
        Self {
            outcome: Outcome::Error,
            code: "PEER",
            message: "peer doctor report is not a valid report".into(),
        }
    }
}
/// Peer text is untrusted: bounded labels, no controls, and an unverified
/// token validity exactly as the local report produces it.
fn valid_remote(report: &Report) -> bool {
    [&report.tick_agent.state]
        .into_iter()
        .chain(report.tick_agent.last_exit_reason.iter())
        .chain(report.tick_agent.warning.iter())
        .chain(report.tick_agent.repair.iter())
        .all(|text| text.len() <= 1024 && !text.chars().any(char::is_control))
        && report.claude.len() <= 128
        && report.claude.iter().all(|row| {
            !row.label.is_empty()
                && row.label.len() <= 128
                && !row.label.chars().any(char::is_control)
                && row.signin.token_validity == "unverified"
        })
        && report.unobserved_adapters.len() <= 128
        && report.unobserved_adapters.iter().all(|name| {
            !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
        })
        && report.installed_sign_ins.len() <= 128
        && report.installed_sign_ins.iter().all(|row| {
            !row.adapter.is_empty()
                && row.adapter.len() <= 128
                && !row.adapter.chars().any(char::is_control)
        })
        && report.login_accounts.len() <= 128
        && report.login_accounts.iter().all(|row| {
            [&row.adapter, &row.label].iter().all(|text| {
                !text.is_empty() && text.len() <= 128 && !text.chars().any(char::is_control)
            }) && row.account_key.as_ref().is_none_or(|key| {
                !key.is_empty() && key.len() <= 128 && !key.chars().any(char::is_control)
            }) && row.readiness.as_ref().is_none_or(|readiness| {
                readiness.observed_scope.as_ref().is_none_or(|scope| {
                    scope.adapter == row.adapter
                        && row
                            .account_key
                            .as_ref()
                            .is_some_and(|key| key == &scope.account_key)
                        && [
                            &scope.host,
                            &scope.adapter,
                            &scope.account_key,
                            &scope.route,
                        ]
                        .iter()
                        .all(|value| {
                            !value.is_empty()
                                && value.len() <= 512
                                && !value.chars().any(char::is_control)
                        })
                }) && readiness
                    .evidence_ref
                    .as_ref()
                    .is_none_or(|value| value.len() <= 256 && !value.chars().any(char::is_control))
            }) && row
                .repair
                .as_ref()
                .is_none_or(|text| text.len() <= 4096 && !text.chars().any(char::is_control))
        })
        && report.managed.len() <= 128
        && report.managed.iter().all(|row| {
            [&row.adapter, &row.label].iter().all(|text| {
                !text.is_empty() && text.len() <= 128 && !text.chars().any(char::is_control)
            })
        })
        && report.agy_accounts.len() <= 128
        && report.agy_accounts.iter().all(|row| {
            [&row.label, &row.directory, &row.keychain]
                .iter()
                .all(|text| {
                    !text.is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control)
                })
        })
        && report
            .managed_error
            .as_ref()
            .is_none_or(|text| text.len() <= 1024 && !text.chars().any(char::is_control))
        && report
            .nextest
            .as_ref()
            .is_none_or(|path| path.len() <= 4096 && !path.chars().any(char::is_control))
        && report.warnings.len() <= 128
        && report
            .warnings
            .iter()
            .all(|text| text.len() <= 1024 && !text.chars().any(char::is_control))
}
fn discard_remote_repairs(report: &mut Report) {
    report.tick_agent.repair = None;
    for row in &mut report.login_accounts {
        row.repair = None;
    }
}
fn inspect(globals: &Globals, host: Option<&str>) -> Result<Report, DoctorError> {
    let Some(name) = host else {
        let resolved = roots::resolve(globals, roots::Workspace::Home, roots::Product::Optional)
            .map_err(DoctorError::local)?;
        return local(&resolved.home, &resolved.roots).map_err(DoctorError::local);
    };
    let resolved = roots::resolve(globals, roots::Workspace::Project, roots::Product::Required)
        .map_err(DoctorError::local)?;
    let context = Context::open_read_only(resolved.roots, &resolved.home, &resolved.project)
        .map_err(DoctorError::local)?;
    let host = context
        .peer_host(name)
        .ok_or_else(|| DoctorError::unknown(name))?;
    let clock = SystemClock::new();
    let budget = WaitBudget::cli(clock.mono_now(), context.config.limits.peer_seconds)
        .map_err(DoctorError::local)?;
    let result = context
        .peer(
            &clock,
            host,
            PeerAction::Doctor {
                extra: Default::default(),
            },
            budget,
            &CancelToken::new(),
        )
        .map_err(DoctorError::peer)?;
    let PeerResult::Doctor { data, .. } = result else {
        return Err(DoctorError::malformed());
    };
    let mut report: Report = serde_json::from_value(data).map_err(|_| DoctorError::malformed())?;
    if !valid_remote(&report) {
        return Err(DoctorError::malformed());
    }
    // Commands supplied by a peer are not trusted as local remedies.
    discard_remote_repairs(&mut report);
    Ok(report)
}
pub fn run(command: Hosts, globals: &Globals) -> u8 {
    let Hosts::Doctor { host } = command;
    match inspect(globals, host.as_deref()) {
        Ok(report) => {
            let mut text = String::new();
            for warning in &report.warnings {
                text.push_str(warning);
                text.push('\n');
            }
            match &report.nextest {
                Some(path) => {
                    text.push_str(&format!("cargo nextest: available for lanes ({path})\n"))
                }
                None => text.push_str(
                    "cargo nextest: unavailable for lanes; install cargo-nextest in ~/.cargo/bin\n",
                ),
            }
            for row in &report.claude {
                let state = serde_json::to_value(row.signin.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".into());
                text.push_str(&format!("claude [{}]: {state}", row.label));
                if row.signin.state
                    == provenant_providers::claude::SigninState::CredentialStorePresent
                {
                    text.push_str(" (status probe could not confirm sign-in)");
                }
                text.push_str("; token validity unverified\n");
            }
            for adapter in &report.unobserved_adapters {
                text.push_str(&format!("recorded account readiness {}: unknown (no account observation; configure an account if needed, then run `provenant quota tick` to probe)\n", adapter.escape_debug()));
            }
            for row in &report.installed_sign_ins {
                let state = match row.state {
                    provenant_providers::host::LocalSignin::SignedIn => "signed in",
                    provenant_providers::host::LocalSignin::SignedOut => "signed out",
                    provenant_providers::host::LocalSignin::Unknown => "unknown",
                };
                text.push_str(&format!(
                    "installed {} sign-in: {state}\n",
                    row.adapter.escape_debug()
                ));
            }
            for row in &report.login_accounts {
                let reason = row
                    .reason
                    .and_then(|value| serde_json::to_value(value).ok())
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "no recorded login failure".into());
                let observed = row
                    .last_success_unix_seconds
                    .and_then(|at| jiff::Timestamp::from_second(at).ok())
                    .map_or_else(|| "unknown".into(), |at| at.to_string());
                let expiry = row
                    .login_expiry
                    .as_ref()
                    .and_then(|view| serde_json::to_value(view.state).ok())
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".into());
                text.push_str(&format!("login {} [{}]: {reason}; expiry {expiry}; last successful observation {observed}", row.label.escape_debug(), row.adapter.escape_debug()));
                if let Some(readiness) = &row.readiness {
                    text.push_str(&format!(
                        "; account readiness {}",
                        provenant_cli::quota::readiness_words(readiness)
                    ));
                }
                if let Some(repair) = &row.repair {
                    text.push_str(&format!("; {repair}"));
                }
                text.push('\n');
            }
            for row in &report.agy_accounts {
                text.push_str(&format!(
                    "agy [{}]: {}",
                    row.label,
                    if row.keychain_available {
                        "keychain available"
                    } else {
                        "isolated keychain unavailable"
                    }
                ));
                if !row.keychain_available {
                    text.push_str(&format!("; expected {}; re-register with `provenant accounts register --adapter agy --directory ~ {}` if this is the login-keychain account, or create the isolated keychain per docs/runbooks/providers.md", row.keychain, row.label));
                }
                text.push('\n');
            }
            // Without a receipt there is nothing to be out of step with.
            for row in report
                .managed
                .iter()
                .filter(|row| row.state != provenant_config::AccountState::NotInstalled)
            {
                let state = serde_json::to_value(row.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".into());
                text.push_str(&format!("managed [{} {}]: {state}", row.adapter, row.label));
                if !matches!(
                    row.state,
                    provenant_config::AccountState::Synced
                        | provenant_config::AccountState::NotInstalled
                ) {
                    text.push_str(&format!(
                        "; {} findings; run `provenant accounts sync`",
                        row.findings
                    ));
                }
                text.push('\n');
            }
            if let Some(error) = &report.managed_error {
                text.push_str(&format!("managed: unavailable: {error}\n"));
            }
            let state = serde_json::to_value(report.peer_agent)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".into());
            text.push_str(&format!(
                "peer agent [{}]: {state}\n",
                provenant_fabric_runtime::peer_agent::LABEL
            ));
            text.push_str(&format!(
                "tick agent [{}]: {}\n",
                crate::tick_agent::LABEL,
                report.tick_agent.state
            ));
            if let Some(reason) = &report.tick_agent.last_exit_reason {
                text.push_str(&format!("  last exit: {reason}\n"));
            }
            if let Some(warning) = &report.tick_agent.warning {
                text.push_str(&format!("  {warning}\n"));
            }
            if let Some(repair) = &report.tick_agent.repair {
                text.push_str(&format!("  repair: {repair}\n"));
            }
            emit(
                &CommandResult::new("hosts doctor", Outcome::Ok, report),
                globals,
                &text,
                true,
            )
        }
        Err(error) => fail(
            "hosts doctor",
            error.outcome,
            error.code,
            &error.message,
            globals,
        ),
    }
}

/// Claude Code names a non-default identity's keychain item after the first
/// eight hex digits of the SHA-256 of its config directory path.
pub(crate) fn keychain_suffix(config_dir: &[u8]) -> String {
    provenant_providers::claude::keychain_suffix(config_dir)
}

#[cfg(test)]
mod tests {
    #[test]
    fn doctor_flags_an_agy_registration_without_its_isolated_keychain() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".gemini");
        std::fs::create_dir_all(&directory).unwrap();
        let accounts: provenant_providers::accounts::Settings = serde_json::from_value(
            serde_json::json!({"entries":{"legacy":{"adapter":"agy","label":"legacy","key":"agy-google-abcdef123456","directory":directory}}}),
        )
        .unwrap();
        let rows = super::agy_accounts(&accounts, home.path());
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].keychain_available);
        assert!(
            rows[0]
                .keychain
                .ends_with(".gemini/Library/Keychains/login.keychain-db")
        );
    }

    #[test]
    fn keychain_suffix_is_the_hex_digest_prefix() {
        assert_eq!(super::keychain_suffix(b"/home/u/.claude-work"), "9fa37e90");
    }

    #[test]
    fn doctor_identifies_installed_adapters_without_account_observations() {
        let row: super::LoginAccount = serde_json::from_value(serde_json::json!({
            "adapter": "agy", "label": "work", "account_key": "key",
            "reason": null, "last_success_unix_seconds": null,
            "login_expiry": null, "repair": null
        }))
        .unwrap();
        let names =
            super::unobserved_adapters(&[row], |binary| binary == "agy" || binary == "opencode");
        assert_eq!(names, ["opencode"]);
        assert!(super::unobserved_adapters(&[], |_| false).is_empty());
    }

    #[test]
    fn doctor_probes_installed_login_status_without_exposing_output() {
        use provenant_os::exec::{
            CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, Termination,
        };
        struct Fake;
        impl Executor for Fake {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                let codex = options.argv[0] == "/fixture/codex";
                if codex {
                    assert_eq!(options.argv[1..], ["login", "status"]);
                    assert!(options.env.unwrap().iter().any(|(key, value)| key == "CODEX_HOME" && value == "/fixture/selected"));
                } else {
                    assert_eq!(options.argv[1..], ["status"]);
                }
                Ok(ExecOutcome {
                    termination: Termination::Exit(if codex { 0 } else { 1 }),
                    stdout: CapturedStream::default(),
                    stderr: CapturedStream {
                        bytes: if codex {
                            b"fixture account".to_vec()
                        } else {
                            b"Not logged in".to_vec()
                        },
                        ..Default::default()
                    },
                    timed_out: false,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 0,
                })
            }
        }
        let adapters = [
            "codex".to_owned(),
            "cursor".to_owned(),
            "opencode".to_owned(),
        ];
        let rows = super::installed_sign_ins(
            &adapters,
            std::path::Path::new("/fixture/home"),
            |name| Some(std::path::PathBuf::from(format!("/fixture/{name}"))),
            |name| (name == "CODEX_HOME").then(|| "/fixture/selected".into()),
            &Fake,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].adapter, "codex");
        assert_eq!(
            rows[0].state,
            provenant_providers::host::LocalSignin::SignedIn
        );
        assert_eq!(rows[1].adapter, "cursor");
        assert_eq!(
            rows[1].state,
            provenant_providers::host::LocalSignin::SignedOut
        );
        let json = serde_json::to_string(&rows).unwrap();
        assert!(!json.contains("fixture account"));
    }

    #[test]
    fn doctor_login_projection_keeps_account_reason_and_recovery() {
        let row = provenant_quota::AccountRow {
            account: provenant_quota::AccountScope {
                adapter: "claude".into(),
                account_key: "key".into(),
                label: "work".into(),
            },
            readiness: Some(provenant_quota::ReadinessRead {
                observed_scope: None,
                state: provenant_quota::ReadinessState::Unavailable,
                reason: Some(provenant_quota::ReadinessReason::LoginExpired),
                observed_unix_seconds: Some(100),
                expires_unix_seconds: Some(200),
                reset_unix_seconds: None,
                evidence_ref: Some("fixture".into()),
            }),
            auth_state: None,
            login_expiry: Some(provenant_quota::LoginExpiryView {
                state: provenant_quota::LoginExpiryState::Expired,
                expires_unix_seconds: Some(90),
                observed_unix_seconds: Some(100),
            }),
            snapshot: Some(provenant_quota::QuotaSnapshot {
                schema_version: 1,
                account: provenant_quota::AccountScope {
                    adapter: "claude".into(),
                    account_key: "key".into(),
                    label: "work".into(),
                },
                revision: 0,
                status: provenant_quota::QuotaStatus::Available,
                observed_unix_seconds: 80,
                source_ref: "fixture".into(),
                authority_ref: None,
                windows: vec![],
                details: None,
            }),
            status: provenant_quota::QuotaStatus::AuthRequired,
            resets_at: None,
            refusals: vec![],
            free_pool_refusal: false,
            age_seconds: Some(20),
            next_due_unix_seconds: None,
            refreshing: false,
            error: Some(provenant_quota::ProbeFailure::LoginExpired),
            repair: Some("run selected login".into()),
            capacity_cooldowns: vec![],
            display: None,
        };
        let value = serde_json::to_value(super::login_account(row)).unwrap();
        assert_eq!(value["label"], "work");
        assert_eq!(value["account_key"], "key");
        assert_eq!(value["reason"], "login_expired");
        assert_eq!(value["last_success_unix_seconds"], 80);
        assert_eq!(value["login_expiry"]["state"], "expired");
        assert_eq!(value["readiness"]["state"], "unavailable");
        assert_eq!(value["readiness"]["reason"], "login_expired");
        assert_eq!(value["readiness"]["observed_unix_seconds"], 100);
        assert_eq!(value["repair"], "run selected login");
    }

    #[test]
    fn doctor_report_requires_the_peer_agent() {
        let report: super::Report =
            serde_json::from_str(r#"{"claude":[],"peer_agent":"stopped","login_accounts":[]}"#)
                .unwrap();
        assert!(report.nextest.is_none());
        assert!(report.managed.is_empty() && report.managed_error.is_none());
        assert_eq!(
            report.peer_agent,
            provenant_fabric_runtime::peer_agent::Status::Stopped
        );
        assert!(serde_json::from_str::<super::Report>(r#"{"claude":[]}"#).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn doctor_reports_nextest_only_when_lane_reachable() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".cargo/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let nextest = bin.join("cargo-nextest");
        std::fs::write(&nextest, b"#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&nextest, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            super::lane_nextest(home.path()),
            Some(nextest.display().to_string())
        );
        std::fs::remove_file(&nextest).unwrap();
        assert_eq!(super::lane_nextest(home.path()), None);
    }

    #[test]
    fn remote_doctor_discards_peer_repair_commands() {
        let mut report: super::Report = serde_json::from_str(
            r#"{"claude":[],"peer_agent":"stopped","login_accounts":[{"adapter":"claude","label":"work","reason":"login_expired","last_success_unix_seconds":100,"login_expiry":null,"repair":"run peer command"}]}"#,
        ).unwrap();
        report.tick_agent.repair = Some("run another peer command".into());
        super::discard_remote_repairs(&mut report);
        assert!(report.login_accounts[0].repair.is_none());
        assert!(report.tick_agent.repair.is_none());
    }

    #[test]
    fn remote_doctor_report_requires_bounded_safe_fields() {
        let report: super::Report = serde_json::from_str(
            r#"{"claude":[{"label":"default","signin":{"state":"signed_out","token_validity":"unverified"}}],"peer_agent":"stopped","login_accounts":[],"warnings":["check config"]}"#,
        )
        .unwrap();
        assert!(super::valid_remote(&report));
        let mut installed: super::Report = serde_json::from_str(
            r#"{"claude":[],"peer_agent":"stopped","login_accounts":[],"unobserved_adapters":["fixture"]}"#,
        ).unwrap();
        assert!(super::valid_remote(&installed));
        installed.unobserved_adapters[0] = "bad\nname".into();
        assert!(!super::valid_remote(&installed));

        let mut scoped: super::Report = serde_json::from_str(
            r#"{"claude":[],"peer_agent":"stopped","login_accounts":[{"adapter":"fixture","label":"work","account_key":"key","reason":null,"last_success_unix_seconds":null,"login_expiry":null,"repair":null,"readiness":{"state":"unavailable","reason":"signed_out","observed_unix_seconds":1,"expires_unix_seconds":2,"reset_unix_seconds":null,"evidence_ref":"fixture","observed_scope":{"host":"local","adapter":"fixture","account_key":"key","route":"*"}}}]}"#,
        ).unwrap();
        assert!(super::valid_remote(&scoped));
        scoped.login_accounts[0]
            .readiness
            .as_mut()
            .unwrap()
            .observed_scope
            .as_mut()
            .unwrap()
            .account_key = "other".into();
        assert!(!super::valid_remote(&scoped));
        scoped.login_accounts[0].account_key = None;
        assert!(!super::valid_remote(&scoped));

        let mut report: super::Report = serde_json::from_str(
            r#"{"claude":[{"label":"default","signin":{"state":"signed_out","token_validity":"unverified"}}],"peer_agent":"stopped","login_accounts":[],"warnings":[]}"#,
        )
        .unwrap();
        report.warnings.push("x".repeat(1025));
        assert!(!super::valid_remote(&report));

        let mut report: super::Report = serde_json::from_str(
            r#"{"claude":[],"peer_agent":"stopped","login_accounts":[],"warnings":[]}"#,
        )
        .unwrap();
        report.warnings = vec!["ok".into(); 129];
        assert!(!super::valid_remote(&report));

        let mut report: super::Report = serde_json::from_str(
            r#"{"claude":[],"peer_agent":"stopped","login_accounts":[],"warnings":["safe"]}"#,
        )
        .unwrap();
        report.warnings[0] = "bad\nwarning".into();
        assert!(!super::valid_remote(&report));

        let mut report: super::Report =
            serde_json::from_str(r#"{"claude":[],"peer_agent":"stopped","login_accounts":[]}"#)
                .unwrap();
        assert_eq!(report.tick_agent.state, "unknown");
        report.tick_agent.last_exit_reason = Some("x".repeat(1025));
        assert!(!super::valid_remote(&report));
        report.tick_agent.last_exit_reason = Some("bad\nreason".into());
        assert!(!super::valid_remote(&report));

        let mut report: super::Report = serde_json::from_str(
            r#"{"claude":[],"peer_agent":"stopped","login_accounts":[{"adapter":"claude","label":"work","reason":"login_expired","last_success_unix_seconds":100,"login_expiry":null,"repair":"safe"}]}"#,
        ).unwrap();
        assert!(super::valid_remote(&report));
        report.login_accounts[0].repair = Some("bad\nrepair".into());
        assert!(!super::valid_remote(&report));
    }
}
