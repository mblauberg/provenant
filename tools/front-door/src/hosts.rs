//! Non-billable sign-in diagnostics. Captures remain bounded and in memory.
use crate::{emit, fail, roots};
use provenant_cli::{Globals, Hosts};
use provenant_os::exec::{
    ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor, Termination,
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
struct Report {
    claude: Vec<Identity>,
    /// The macOS login-session peer agent.
    peer_agent: provenant_fabric_runtime::peer_agent::Status,
    /// Every registered account of every adapter.
    #[serde(default)]
    managed: Vec<Managed>,
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
    let config = match provenant_config::read_with_domains(roots, &provenant_cli::domains::SECTIONS)
    {
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
fn local(home: &Path, roots: &provenant_core::Roots) -> Result<Report, Box<dyn std::error::Error>> {
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
    Ok(Report {
        claude,
        peer_agent: provenant_fabric_runtime::peer_agent::status(state, home),
        managed,
        managed_error,
        warnings: crate::pool_effort_warnings(roots),
    })
}
fn inspect(globals: &Globals, host: Option<&str>) -> Result<Report, Box<dyn std::error::Error>> {
    let resolved = roots::resolve(globals, roots::Workspace::Home, roots::Product::Optional)?;
    let home = resolved.home;
    let Some(host) = host else {
        return local(&home, &resolved.roots);
    };
    let ssh = executable("ssh").ok_or("SSH executable is unavailable")?;
    let argv = [
        ssh.into_os_string(),
        "-T".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=3".into(),
        "--".into(),
        host.into(),
        // Fixed remote command; no user content is interpolated into its shell.
        "exec ~/.local/bin/provenant hosts doctor --json".into(),
    ];
    let out = probe(&argv, &home, None, 30)?;
    if !complete(&out) || out.termination != Termination::Exit(0) {
        return Err("remote sign-in probe unavailable or incomplete".into());
    }
    #[derive(Deserialize)]
    struct Remote {
        command: String,
        status: String,
        data: Report,
    }
    let remote: Remote =
        provenant_records::decode(&out.stdout.bytes[..], provenant_records::Limits::CONFIG)?;
    if remote.command != "hosts doctor"
        || remote.status != "ok"
        || remote.data.claude.len() > 128
        || remote.data.claude.iter().any(|row| {
            row.label.is_empty()
                || row.label.len() > 128
                || row.label.chars().any(char::is_control)
                || row.signin.token_validity != "unverified"
        })
        || remote.data.managed.len() > 128
        || remote.data.managed.iter().any(|row| {
            [&row.adapter, &row.label].iter().any(|text| {
                text.is_empty() || text.len() > 128 || text.chars().any(char::is_control)
            })
        })
        || remote
            .data
            .managed_error
            .as_ref()
            .is_some_and(|text| text.len() > 1024 || text.chars().any(char::is_control))
    {
        return Err("invalid remote doctor report".into());
    }
    Ok(remote.data)
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
            emit(
                &CommandResult::new("hosts doctor", Outcome::Ok, report),
                globals,
                &text,
                true,
            )
        }
        Err(error) => fail(
            "hosts doctor",
            Outcome::Unavailable,
            "SIGNIN_PROBE",
            &error.to_string(),
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
    fn keychain_suffix_is_the_hex_digest_prefix() {
        assert_eq!(super::keychain_suffix(b"/home/u/.claude-work"), "9fa37e90");
    }

    #[test]
    fn doctor_report_requires_the_peer_agent() {
        let report: super::Report =
            serde_json::from_str(r#"{"claude":[],"peer_agent":"stopped"}"#).unwrap();
        assert!(report.managed.is_empty() && report.managed_error.is_none());
        assert_eq!(
            report.peer_agent,
            provenant_fabric_runtime::peer_agent::Status::Stopped
        );
        assert!(serde_json::from_str::<super::Report>(r#"{"claude":[]}"#).is_err());
    }
}
