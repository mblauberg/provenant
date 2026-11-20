//! Bare object store and its fetch/GC lock.
use super::{BuildError, InfraKind};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn path(state: &Path, project_key: &str) -> PathBuf {
    super::paths::store_path(state, project_key)
}
fn error(detail: impl ToString) -> BuildError {
    BuildError::Infra {
        kind: InfraKind::Git,
        detail: detail.to_string(),
    }
}
fn io_error(error: std::io::Error) -> BuildError {
    let kind = if matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::EIO)) {
        InfraKind::Disk
    } else {
        InfraKind::Git
    };
    BuildError::Infra {
        kind,
        detail: error.to_string(),
    }
}

#[allow(clippy::disallowed_methods, reason = "Git store subprocess boundary")]
fn git() -> Command {
    let mut command =
        Command::new(provenant_os::git::git_executable().unwrap_or_else(|| "/usr/bin/git".into()));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
}

/// Hold the shared fetch or exclusive maintenance lock until dropped.
pub fn lock(path: &Path, exclusive: bool) -> Result<provenant_os::fs::LockGuard, BuildError> {
    use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
    use provenant_os::fs::{Directory, LockMode};
    let parent = path
        .parent()
        .ok_or_else(|| error("store lock has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| error("store lock has no name"))?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let directory = Directory::open_anchor(parent).map_err(error)?;
    let clock = provenant_os::clock::SystemClock::new();
    directory
        .lock(
            name,
            if exclusive {
                LockMode::Exclusive
            } else {
                LockMode::Shared
            },
            WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(60_000_000)),
            &CancelToken::new(),
            &clock,
        )
        .map_err(error)
}

/// Create and configure a bare store, serialising concurrent initialisation.
pub fn ensure(state: &Path, project_key: &str) -> Result<PathBuf, BuildError> {
    let store = path(state, project_key);
    let _lock = lock(&super::paths::store_lock(state, project_key), true)?;
    if !store.join("HEAD").exists() {
        let status = git()
            .args(["init", "--bare"])
            .arg(&store)
            .output()
            .map_err(io_error)?
            .status;
        if !status.success() {
            return Err(error("git init --bare failed"));
        }
    }
    for (key, value) in [
        ("core.hooksPath", "/dev/null"),
        ("gc.auto", "0"),
        ("receive.denyDeletes", "false"),
    ] {
        let status = git()
            .arg("--git-dir")
            .arg(&store)
            .args(["config", key, value])
            .status()
            .map_err(io_error)?;
        if !status.success() {
            return Err(error(format!("git config {key} failed")));
        }
    }
    Ok(store)
}

/// Expire timestamped pins and collect unreachable objects under the exclusive lock.
pub fn gc(
    state: &Path,
    project_key: &str,
    retention: Duration,
    now: SystemTime,
) -> Result<(), BuildError> {
    let store = path(state, project_key);
    let _lock = lock(&super::paths::store_lock(state, project_key), true)?;
    let out = git()
        .arg("--git-dir")
        .arg(&store)
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/snapshots",
            "refs/bases",
        ])
        .output()
        .map_err(io_error)?;
    if !out.status.success() {
        return Err(error("listing pins failed"));
    }
    let cutoff = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .saturating_sub(retention)
        .as_millis();
    for name in String::from_utf8_lossy(&out.stdout).lines() {
        let timestamp = name
            .rsplit('/')
            .next()
            .and_then(|s| s.split('-').next())
            .and_then(|s| s.parse::<u128>().ok());
        if timestamp.is_some_and(|t| t < cutoff) {
            let status = git()
                .arg("--git-dir")
                .arg(&store)
                .args(["update-ref", "-d", name])
                .status()
                .map_err(io_error)?;
            if !status.success() {
                return Err(error("deleting expired pin failed"));
            }
        }
    }
    let status = git()
        .arg("--git-dir")
        .arg(&store)
        .args(["gc", "--prune=1.hour.ago"])
        .status()
        .map_err(io_error)?;
    if !status.success() {
        return Err(error("git gc failed"));
    }
    Ok(())
}
