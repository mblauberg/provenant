//! Non-billable sign-in diagnostics. Captures remain bounded and in memory.
use crate::{emit, fail, roots};
use provenant_cli::{Globals, Hosts};
use provenant_providers::claude::{CredentialPresence, SigninStatus, signin_status};
use provenant_records::{CommandResult, Outcome};
use provenant_supervisor::exec::{
    ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor, Termination,
};
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
#[derive(Serialize, Deserialize)]
struct Report {
    claude: Vec<Identity>,
}
fn executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|p| p.is_absolute())
        .map(|p| p.join(name))
        .find(|p| provenant_supervisor::sys::is_executable(p))
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
    let mut options = ExecOptions::new(argv, home, Duration::from_secs(seconds));
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
    let mut options = ExecOptions::new(&argv, home, Duration::from_secs(2));
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
fn local(home: &Path) -> Result<Report, Box<dyn std::error::Error>> {
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
    Ok(Report { claude })
}
fn inspect(host: Option<&str>) -> Result<Report, Box<dyn std::error::Error>> {
    let home = roots::home()?;
    let Some(host) = host else {
        return local(&home);
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
    {
        return Err("invalid remote doctor report".into());
    }
    Ok(remote.data)
}
pub fn run(command: Hosts, globals: &Globals) -> u8 {
    let Hosts::Doctor { host } = command;
    match inspect(host.as_deref()) {
        Ok(report) => {
            let mut text = String::new();
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
}
