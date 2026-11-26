//! Shared execution and digest rules for declared build preparation commands.

use super::process::Command;
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_os::fs::{Directory, LockMode, PublishMode};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

const DIGEST_FILE: &str = "provenant-build-prepare-digest";
const LOCK_FILE: &str = "provenant-build-prepare.lock";

/// Hash the command declaration using the same representation for slots and
/// lane worktrees.
pub fn digest(commands: &[Vec<String>]) -> String {
    let bytes = serde_json::to_vec(commands).unwrap_or_default();
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Run declared commands with a clean, explicit environment and repository
/// working directory. Output is bounded by the OS executor.
pub fn execute(
    commands: &[Vec<String>],
    cwd: &Path,
    env: &[(OsString, OsString)],
) -> Result<(), String> {
    for argv in commands {
        let Some(program) = argv.first() else {
            continue;
        };
        let output = Command::new(program)
            .args(&argv[1..])
            .current_dir(cwd)
            .env_clear()
            .envs(env.iter().cloned())
            .output()
            .map_err(|error| format!("{}: {error}", display_argv(argv)))?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr);
            let detail = detail.trim();
            return Err(if detail.is_empty() {
                format!("{} exited with {}", display_argv(argv), output.status)
            } else {
                format!(
                    "{} exited with {}: {detail}",
                    display_argv(argv),
                    output.status
                )
            });
        }
    }
    Ok(())
}

/// Prepare a lane worktree only when its declared command digest changes.
/// The private Git directory keeps the receipt out of the checked-out branch.
pub fn run_if_changed(
    private_git: &Directory,
    commands: &[Vec<String>],
    cwd: &Path,
    env: &[(OsString, OsString)],
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<bool, String> {
    if commands.is_empty() {
        return Ok(false);
    }
    let digest = digest(commands);
    let _lock = private_git
        .lock(
            OsStr::new(LOCK_FILE),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )
        .map_err(|error| format!("cannot acquire preparation lock: {error}"))?;
    match private_git.read_stable(OsStr::new(DIGEST_FILE), 128) {
        Ok(current) if current == digest.as_bytes() => return Ok(false),
        Ok(_) => {}
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(format!("cannot read preparation receipt: {error}")),
    }

    execute(commands, cwd, env)?;
    private_git
        .atomic_write(
            OsStr::new(DIGEST_FILE),
            ".build-prepare-",
            digest.as_bytes(),
            PublishMode::Replace,
        )
        .and_then(|publication| publication.require_durable())
        .map_err(|error| format!("cannot record preparation digest: {error}"))?;
    Ok(true)
}

/// The host-side environment needed by project preparation commands. Secrets
/// and lane-specific Git variables are excluded by constructing it explicitly.
pub fn lane_environment(home: &Path) -> Vec<(OsString, OsString)> {
    let mut env = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        env.push((OsString::from("PATH"), path));
    }
    env.push((OsString::from("HOME"), home.as_os_str().to_owned()));
    env.push((
        OsString::from("CARGO_HOME"),
        std::env::var_os("CARGO_HOME").unwrap_or_else(|| home.join(".cargo").into_os_string()),
    ));
    for name in ["RUSTUP_HOME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            env.push((OsString::from(name), value));
        }
    }
    env
}

fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|part| format!("{:?}", part))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::DurationMicros;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn lane_worktree_preparation_runs_once_per_declaration_digest() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("git")).unwrap();
        let private_git = Directory::open_anchor(&root.path().join("git")).unwrap();
        let counter = root.path().join("runs");
        let command = |value: &str| {
            vec![vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("printf '%s\\n' '{value}' >> '{}'", counter.display()),
            ]]
        };
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(2_000_000));

        assert!(
            run_if_changed(
                &private_git,
                &command("first"),
                root.path(),
                &[],
                budget,
                &cancel,
                &clock,
            )
            .unwrap()
        );
        assert!(
            !run_if_changed(
                &private_git,
                &command("first"),
                root.path(),
                &[],
                budget,
                &cancel,
                &clock,
            )
            .unwrap()
        );
        assert!(
            run_if_changed(
                &private_git,
                &command("second"),
                root.path(),
                &[],
                budget,
                &cancel,
                &clock,
            )
            .unwrap()
        );
        assert_eq!(fs::read_to_string(counter).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn failed_prepare_does_not_record_the_digest() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("git")).unwrap();
        let private_git = Directory::open_anchor(&root.path().join("git")).unwrap();
        let commands = vec![vec!["/bin/false".into()]];
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(2_000_000));
        assert!(
            run_if_changed(
                &private_git,
                &commands,
                root.path(),
                &[],
                budget,
                &cancel,
                &clock,
            )
            .is_err()
        );
        assert!(
            private_git
                .read_stable(OsStr::new(DIGEST_FILE), 128)
                .unwrap_err()
                .is_not_found()
        );
    }

    #[test]
    fn undeclared_prepare_has_no_receipt_or_execution() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("git")).unwrap();
        let private_git = Directory::open_anchor(&root.path().join("git")).unwrap();
        let cancel = CancelToken::new();
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(2_000_000));
        assert!(matches!(
            run_if_changed(&private_git, &[], root.path(), &[], budget, &cancel, &clock,),
            Ok(false)
        ));
        assert!(
            private_git
                .read_stable(OsStr::new(DIGEST_FILE), 128)
                .unwrap_err()
                .is_not_found()
        );
    }
}
