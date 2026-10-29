//! Host housekeeping on existing quota events. No background timer or service.
use provenant_core::{CancelToken, Clock, OperationId, Roots, WaitBudget};
use provenant_fabric_runtime::coordination::{
    EventsInput, LocalCoordination, Mutation, ThreadWait,
};
use provenant_os::{clock::SystemClock, exec::SystemExecutor};
use std::path::Path;

/// Quota events only schedule work; the detached command owns its lifetime.
pub fn schedule(roots: &Roots, cwd: &Path) -> Result<(), String> {
    if crate::fabric::in_lane() {
        return Ok(());
    }
    let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|e| e.to_string())?;
    let now = SystemClock::new()
        .wall_now()
        .map_err(|e| e.to_string())?
        .as_timestamp()
        .as_second();
    if !config.providers.iter().any(|(provider, settings)| {
        schedulable(roots.state.as_path(), provider, settings, now, || {
            crate::run::provider_executable(provider).is_ok()
        })
    }) {
        return Ok(());
    }
    if !provenant_providers::update::reserve_worker(roots.state.as_path(), now) {
        return Ok(());
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let argv = worker_argv(&executable, roots.state.as_path());
    provenant_os::exec::spawn_detached(&argv, cwd).map_err(|e| e.to_string())
}

/// The worker starts in the event's working directory, which is its project.
/// Selecting that directory with `--project` would also demand a Git checkout,
/// and quota events can run anywhere.
fn worker_argv(executable: &Path, state: &Path) -> [std::ffi::OsString; 5] {
    [
        executable.as_os_str().to_owned(),
        "--state-dir".into(),
        state.as_os_str().to_owned(),
        "internal".into(),
        "provider-updates".into(),
    ]
}

fn schedulable(
    state: &Path,
    provider: &str,
    settings: &provenant_config::ProviderSettings,
    now: i64,
    installed: impl FnOnce() -> bool,
) -> bool {
    if !provenant_providers::update::due(state, provider, settings, now) || !installed() {
        return false;
    }
    // Other fence errors still reach the worker's normal failure reporting.
    !matches!(
        provenant_os::provider_fence::acquire(
            state,
            provider,
            provenant_os::fs::LockMode::Exclusive
        ),
        Err(provenant_os::fs::FsError::LockTimeout { .. })
    )
}

/// The detached worker. It records its one failure cause beside the update
/// records, because its own output goes nowhere.
pub fn housekeep(roots: &Roots, home: &Path, cwd: &Path) -> Result<(), String> {
    if crate::fabric::in_lane() {
        return Ok(());
    }
    let result = update_providers(roots, home, cwd);
    let now = SystemClock::new()
        .wall_now()
        .map(|time| time.as_timestamp().as_second());
    if let Ok(now) = now {
        provenant_providers::update::finish_worker(
            roots.state.as_path(),
            now,
            result.as_ref().err().map(String::as_str),
        );
    }
    result
}

fn update_providers(roots: &Roots, home: &Path, cwd: &Path) -> Result<(), String> {
    let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|e| e.to_string())?;
    let clock = SystemClock::new();
    let now = clock
        .wall_now()
        .map_err(|e| e.to_string())?
        .as_timestamp()
        .as_second();
    let executor = SystemExecutor::new(1).map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    // Stop after a completed attempt; provider errors do not suppress later providers.
    for (provider, settings) in &config.providers {
        let Ok(executable) = crate::run::provider_executable(provider) else {
            continue;
        };
        let result = provenant_providers::update::check(
            roots.state.as_path(),
            provider,
            &executable,
            settings,
            &executor,
            now,
        );
        let (record, failed) = match result {
            Ok(Some(record)) => (record, false),
            Ok(None) => continue,
            Err(error) => {
                errors.push(format!("{provider}: {error}"));
                (
                    provenant_providers::update::UpdateRecord {
                        checked_unix_seconds: now,
                        old_version: None,
                        new_version: None,
                        outcome: format!("failed: {error}"),
                    },
                    true,
                )
            }
        };
        if let Err(error) = note_update(roots, home, cwd, &clock, provider, &record) {
            errors.push(format!("{provider} activity: {error}"));
        }
        if !failed && record.outcome != "unknown install method" {
            break;
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn note_update(
    roots: &Roots,
    home: &Path,
    cwd: &Path,
    clock: &SystemClock,
    provider: &str,
    record: &provenant_providers::update::UpdateRecord,
) -> Result<(), String> {
    let ctx = crate::fabric::Context::open(roots.clone(), home, cwd).map_err(|e| e.to_string())?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 2).map_err(|e| e.to_string())?;
    let service = LocalCoordination::open(
        &ctx.roots.state,
        ctx.identity.project,
        ctx.identity.inbox.actor,
        clock,
        &ThreadWait,
        budget,
        &cancel,
    )
    .map_err(|e| e.to_string())?;
    let operation = OperationId::parse(&format!(
        "provider-update-{provider}-{}",
        record.checked_unix_seconds
    ))
    .map_err(|e| e.to_string())?;
    let note = format!(
        "Provider {provider} update: {}; old version: {}; new version: {}",
        record.outcome,
        record.old_version.as_deref().unwrap_or("unknown"),
        record.new_version.as_deref().unwrap_or("unknown"),
    );
    service
        .mutate(
            Mutation::Events(EventsInput::Note {
                note,
                dedup_key: None,
            }),
            &operation,
            budget,
            &cancel,
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_does_not_require_a_git_checkout() {
        let argv = worker_argv(Path::new("/bin/provenant"), Path::new("/state"));
        assert!(!argv.iter().any(|arg| arg == "--project"));
        assert_eq!(argv[3], "internal");
    }

    #[test]
    fn scheduling_skips_missing_and_busy_providers() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let settings = provenant_config::ProviderSettings::default();
        assert!(!schedulable(temp.path(), "claude", &settings, 100, || {
            false
        }));
        let lane = provenant_os::provider_fence::acquire(
            temp.path(),
            "claude",
            provenant_os::fs::LockMode::Shared,
        )?;
        assert!(!schedulable(temp.path(), "claude", &settings, 100, || true));
        drop(lane);
        assert!(schedulable(temp.path(), "claude", &settings, 100, || true));
        Ok(())
    }
}
