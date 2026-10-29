//! Idle-event provider updates. Shared lane leases exclude the native updater.
use provenant_config::{ProviderSettings, ProviderUpdate};
use provenant_os::{
    exec::{ExecOptions, Executor, StreamPolicy, Termination},
    fs::{Directory, LockMode, PublishMode},
};
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, path::Path, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRecord {
    pub checked_unix_seconds: i64,
    pub old_version: Option<String>,
    pub new_version: Option<String>,
    pub outcome: String,
}

fn record_name(provider: &str) -> OsString {
    format!("{provider}.json").into()
}

pub fn latest(state: &Path, provider: &str) -> Option<UpdateRecord> {
    let dir = Directory::open_anchor(&state.join("provider-updates")).ok()?;
    let read = |name: &std::ffi::OsStr| -> Option<UpdateRecord> {
        serde_json::from_slice(&dir.read_stable(name, 4096).ok()?).ok()
    };
    let result = read(&record_name(provider));
    let attempt = read(&attempt_name(provider));
    match (result, attempt) {
        (Some(result), Some(mut attempt))
            if attempt.checked_unix_seconds > result.checked_unix_seconds =>
        {
            attempt.old_version = attempt.old_version.or(result.old_version);
            attempt.new_version = attempt.new_version.or(result.new_version);
            Some(attempt)
        }
        (Some(result), _) => Some(result),
        (None, attempt) => attempt,
    }
}

fn attempt_name(provider: &str) -> OsString {
    format!("{provider}.attempt.json").into()
}

/// Minimum gap between detached worker launches, whatever the outcome. A worker
/// that fails before it reaches a provider writes no per-provider record, so
/// without this every quota event would launch it again.
pub const WORKER_BACKOFF_SECONDS: i64 = 900;
const WORKER_RECORD: &str = "worker.json";

/// The last detached worker launch and, if it failed, its one cause.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRecord {
    pub started_unix_seconds: i64,
    pub failure: Option<String>,
}

fn worker_directory(state: &Path) -> Result<Directory, String> {
    let root = Directory::open_anchor(state).map_err(|e| e.to_string())?;
    match root.mkdir(std::ffi::OsStr::new("provider-updates"), 0o700) {
        Ok(()) => root.sync().map_err(|e| e.to_string())?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.to_string()),
    }
    root.open_verified_directory(std::ffi::OsStr::new("provider-updates"))
        .map_err(|e| e.to_string())
}

pub fn worker(state: &Path) -> Option<WorkerRecord> {
    let dir = Directory::open_anchor(&state.join("provider-updates")).ok()?;
    serde_json::from_slice(
        &dir.read_stable(std::ffi::OsStr::new(WORKER_RECORD), 4096)
            .ok()?,
    )
    .ok()
}

/// Claim the right to launch the detached worker. False inside the backoff, or
/// when the claim cannot be stored, since an unrecorded launch cannot back off.
pub fn reserve_worker(state: &Path, now: i64) -> bool {
    if worker(state).is_some_and(|record| {
        now.saturating_sub(record.started_unix_seconds) < WORKER_BACKOFF_SECONDS
    }) {
        return false;
    }
    worker_directory(state)
        .and_then(|dir| {
            save(
                &dir,
                std::ffi::OsStr::new(WORKER_RECORD),
                &WorkerRecord {
                    started_unix_seconds: now,
                    failure: None,
                },
            )
        })
        .is_ok()
}

/// Keep the worker's cause where a later reader can find it; its own stderr is
/// detached. Best effort: the launch claim already holds the backoff.
pub fn finish_worker(state: &Path, now: i64, failure: Option<&str>) {
    let started = worker(state).map_or(now, |record| record.started_unix_seconds);
    let _ = worker_directory(state).and_then(|dir| {
        save(
            &dir,
            std::ffi::OsStr::new(WORKER_RECORD),
            &WorkerRecord {
                started_unix_seconds: started,
                failure: failure.map(|reason| reason.chars().take(512).collect()),
            },
        )
    });
}

/// A read-only hint for quota events. The worker rechecks under the lease.
pub fn due(state: &Path, provider: &str, settings: &ProviderSettings, now: i64) -> bool {
    !matches!(settings.update, ProviderUpdate::Enabled(false))
        && latest(state, provider).is_none_or(|record| {
            now.saturating_sub(record.checked_unix_seconds)
                >= i64::try_from(settings.update_interval_seconds).unwrap_or(i64::MAX)
        })
}

fn save(dir: &Directory, name: &std::ffi::OsStr, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    dir.atomic_write(name, "provider-update", &bytes, PublishMode::Replace)
        .and_then(|p| p.require_durable())
        .map_err(|e| e.to_string())
}

/// Resolve the installation owner from the real executable, rather than PATH aliases.
fn command(provider: &str, executable: &Path, setting: &ProviderUpdate) -> Option<Vec<OsString>> {
    match setting {
        ProviderUpdate::Enabled(false) => return None,
        ProviderUpdate::Command { command } => {
            return Some(command.iter().map(OsString::from).collect());
        }
        ProviderUpdate::Enabled(true) => {}
    }
    match provider {
        "claude" => Some(vec![executable.into(), "update".into()]),
        "opencode" => Some(vec![executable.into(), "upgrade".into()]),
        "codex" => {
            let resolved = provenant_os::fs::canonicalize(executable).ok()?;
            let text = resolved.to_str()?;
            if text.contains("/Cellar/") || text.contains("/Caskroom/") {
                let root = text
                    .split_once(if text.contains("/Cellar/") {
                        "/Cellar/"
                    } else {
                        "/Caskroom/"
                    })?
                    .0;
                Some(vec![
                    Path::new(root).join("bin/brew").into(),
                    "upgrade".into(),
                    "codex".into(),
                ])
            } else if text.contains("/node_modules/@openai/codex/") {
                let prefix = text.split_once("/lib/node_modules/@openai/codex/")?.0;
                Some(vec![
                    Path::new(prefix).join("bin/npm").into(),
                    "install".into(),
                    "-g".into(),
                    "@openai/codex@latest".into(),
                ])
            } else {
                None
            }
        }
        _ => None,
    }
}

fn version(executor: &dyn Executor, executable: &Path, cwd: &Path) -> Option<String> {
    let argv = [executable.into(), "--version".into()];
    let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(4));
    options.stdout = StreamPolicy::Capture { limit: 128 };
    options.stderr = StreamPolicy::Discard;
    let result = executor.execute(options).ok()?;
    if result.termination != Termination::Exit(0) || result.timed_out || result.stdout.truncated {
        return None;
    }
    let text = std::str::from_utf8(&result.stdout.bytes).ok()?.trim();
    (!text.is_empty() && !text.chars().any(char::is_control)).then(|| text.to_owned())
}

/// A busy provider is left for the next event, without spending its interval.
/// Output from updaters is discarded; only bounded version strings are retained.
pub fn check(
    state: &Path,
    provider: &str,
    executable: &Path,
    settings: &ProviderSettings,
    executor: &dyn Executor,
    now: i64,
) -> Result<Option<UpdateRecord>, String> {
    // Also honour error backoff when the fence itself cannot be opened.
    if !due(state, provider, settings, now) {
        return Ok(None);
    }
    let lease = provenant_os::provider_fence::acquire(state, provider, LockMode::Exclusive);
    let result = match &lease {
        Ok(lease) => check_inner(state, provider, executable, settings, executor, now, lease),
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(None),
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = &result {
        // Complement the attempt record: early failures (including a broken
        // attempt destination or lock file) can still publish to the result.
        // If both destinations are unwritable, durable backoff is impossible.
        if let Ok(dir) = Directory::open_anchor(&state.join("provider-updates")) {
            let latest = latest(state, provider);
            if latest
                .as_ref()
                .is_some_and(|record| record.checked_unix_seconds > now)
            {
                return result;
            }
            let mut record = latest.unwrap_or(UpdateRecord {
                checked_unix_seconds: now,
                old_version: None,
                new_version: None,
                outcome: String::new(),
            });
            record.checked_unix_seconds = now;
            record.outcome = format!("failed: {error}");
            let _ = save(&dir, &record_name(provider), &record);
        }
    }
    result
}

fn check_inner(
    state: &Path,
    provider: &str,
    executable: &Path,
    settings: &ProviderSettings,
    executor: &dyn Executor,
    now: i64,
    lease: &provenant_os::fs::LockGuard,
) -> Result<Option<UpdateRecord>, String> {
    let dir = Directory::open_anchor(&state.join("provider-updates")).map_err(|e| e.to_string())?;
    let previous = latest(state, provider);
    let argv = command(provider, executable, &settings.update);
    if previous.as_ref().is_some_and(|record| {
        now.saturating_sub(record.checked_unix_seconds)
            < i64::try_from(settings.update_interval_seconds).unwrap_or(i64::MAX)
    }) {
        return Ok(None);
    }
    if let Some(mut record) = previous
        .clone()
        .filter(|record| record.outcome == "unknown install method" && argv.is_none())
    {
        // Restamp so `due` stays false for another interval; no new note.
        record.checked_unix_seconds = now;
        save(&dir, &record_name(provider), &record)?;
        return Ok(None);
    }
    // Reserve the interval independently of result publication. If that
    // destination is broken, subsequent events still back off on the error.
    let mut record = UpdateRecord {
        checked_unix_seconds: now,
        old_version: previous
            .as_ref()
            .and_then(|record| record.old_version.clone()),
        new_version: previous
            .as_ref()
            .and_then(|record| record.new_version.clone()),
        outcome: "failed (interrupted)".into(),
    };
    save(&dir, &attempt_name(provider), &record)?;
    let Some(argv) = argv else {
        let record = UpdateRecord {
            checked_unix_seconds: now,
            old_version: None,
            new_version: None,
            outcome: "unknown install method".into(),
        };
        save(&dir, &record_name(provider), &record)?;
        return Ok(Some(record));
    };
    if let Some(old_version) = version(executor, executable, state) {
        record.old_version = Some(old_version.clone());
        record.new_version = Some(old_version);
    }
    save(&dir, &attempt_name(provider), &record)?;
    let mut options = ExecOptions::new(
        &argv,
        state,
        Duration::from_secs(settings.update_timeout_seconds),
    );
    options.stdout = StreamPolicy::Discard;
    options.stderr = StreamPolicy::Discard;
    // Inheritance keeps the fence intact if housekeeping is killed. A child
    // that daemonises out of this group also keeps the lease until it exits.
    options.inherited_lease = Some(lease);
    options.process_group = true;
    let result = executor.execute(options);
    let succeeded =
        result.is_ok_and(|out| out.termination == Termination::Exit(0) && !out.timed_out);
    record.new_version =
        version(executor, executable, state).or_else(|| record.old_version.clone());
    record.outcome = if succeeded { "updated" } else { "failed" }.into();
    save(&dir, &record_name(provider), &record)?;
    Ok(Some(record))
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_os::exec::{CapturedStream, ExecError, ExecOutcome};
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct Fake<'a> {
        state: &'a Path,
        updated: AtomicBool,
        calls: AtomicUsize,
        fail: bool,
    }
    impl Executor for Fake<'_> {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let version = options.argv[1] == "--version";
            if !version {
                assert_eq!(options.argv[1], "update");
                assert_eq!(options.timeout, Duration::from_secs(300));
                assert!(options.inherited_lease.is_some());
                assert!(options.process_group);
                assert!(matches!(
                    provenant_os::provider_fence::acquire(self.state, "claude", LockMode::Shared),
                    Err(provenant_os::fs::FsError::LockTimeout { .. })
                ));
                assert!(matches!(options.stdout, StreamPolicy::Discard));
                assert!(matches!(options.stderr, StreamPolicy::Discard));
                self.updated.store(!self.fail, Ordering::Relaxed);
            }
            Ok(ExecOutcome {
                termination: Termination::Exit(if self.fail && !version { 1 } else { 0 }),
                stdout: CapturedStream {
                    bytes: if version {
                        if self.updated.load(Ordering::Relaxed) {
                            b"2.0".to_vec()
                        } else {
                            b"1.0".to_vec()
                        }
                    } else {
                        Vec::new()
                    },
                    ..CapturedStream::default()
                },
                stderr: CapturedStream::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 1,
            })
        }
    }

    #[test]
    fn worker_launches_back_off_after_any_outcome() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        assert!(reserve_worker(temp.path(), 1000));
        assert!(!reserve_worker(
            temp.path(),
            1000 + WORKER_BACKOFF_SECONDS - 1
        ));
        finish_worker(temp.path(), 1005, Some("project must be a Git checkout"));
        let record = worker(temp.path()).ok_or("missing worker record")?;
        assert_eq!(record.started_unix_seconds, 1000);
        assert_eq!(
            record.failure.as_deref(),
            Some("project must be a Git checkout")
        );
        assert!(!reserve_worker(
            temp.path(),
            1000 + WORKER_BACKOFF_SECONDS - 1
        ));
        assert!(reserve_worker(temp.path(), 1000 + WORKER_BACKOFF_SECONDS));
        assert_eq!(worker(temp.path()).ok_or("missing")?.failure, None);
        Ok(())
    }

    #[test]
    fn early_failure_preserves_last_known_versions() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let settings = ProviderSettings::default();
        let fake = Fake {
            state: temp.path(),
            updated: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let previous = check(
            temp.path(),
            "claude",
            Path::new("/fake/claude"),
            &settings,
            &fake,
            100,
        )?
        .ok_or("missing update")?;
        std::fs::remove_file(temp.path().join("provider-updates/claude.attempt.json"))?;
        std::fs::create_dir(temp.path().join("provider-updates/claude.attempt.json"))?;
        let now = 100 + settings.update_interval_seconds as i64;
        assert!(
            check(
                temp.path(),
                "claude",
                Path::new("/fake/claude"),
                &settings,
                &fake,
                now
            )
            .is_err()
        );
        let failure = latest(temp.path(), "claude").ok_or("missing failure")?;
        assert_eq!(failure.checked_unix_seconds, now);
        assert!(failure.outcome.starts_with("failed:"));
        assert_eq!(failure.old_version, previous.old_version);
        assert_eq!(failure.new_version, previous.new_version);
        Ok(())
    }

    #[test]
    fn result_write_error_backs_off() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let fake = Fake {
            state: temp.path(),
            updated: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let settings = ProviderSettings::default();
        // A broken result destination must not cause an attempt on every tick.
        std::fs::create_dir_all(temp.path().join("provider-updates/claude.json"))?;
        assert!(
            check(
                temp.path(),
                "claude",
                Path::new("/fake/claude"),
                &settings,
                &fake,
                100
            )
            .is_err()
        );
        let calls = fake.calls.load(Ordering::Relaxed);
        assert!(
            check(
                temp.path(),
                "claude",
                Path::new("/fake/claude"),
                &settings,
                &fake,
                101
            )?
            .is_none()
        );
        assert_eq!(fake.calls.load(Ordering::Relaxed), calls);
        assert!(
            check(
                temp.path(),
                "claude",
                Path::new("/fake/claude"),
                &settings,
                &fake,
                100 + settings.update_interval_seconds as i64
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn early_check_errors_back_off() -> Result<(), Box<dyn std::error::Error>> {
        for blocked in ["claude.attempt.json", "claude.lock"] {
            let temp = tempfile::tempdir()?;
            std::fs::create_dir_all(temp.path().join("provider-updates").join(blocked))?;
            let fake = Fake {
                state: temp.path(),
                updated: AtomicBool::new(false),
                calls: AtomicUsize::new(0),
                fail: false,
            };
            let settings = ProviderSettings::default();
            assert!(
                check(
                    temp.path(),
                    "claude",
                    Path::new("/fake/claude"),
                    &settings,
                    &fake,
                    100
                )
                .is_err()
            );
            assert!(
                latest(temp.path(), "claude")
                    .ok_or("missing failure backoff")?
                    .outcome
                    .starts_with("failed:")
            );
            assert!(
                check(
                    temp.path(),
                    "claude",
                    Path::new("/fake/claude"),
                    &settings,
                    &fake,
                    101
                )?
                .is_none()
            );
            assert_eq!(fake.calls.load(Ordering::Relaxed), 0);
        }
        Ok(())
    }

    #[test]
    fn updater_runner_fixture() -> Result<(), Box<dyn std::error::Error>> {
        let Some(state) = std::env::var_os("PROVENANT_TEST_UPDATER_STATE") else {
            return Ok(());
        };
        let state = Path::new(&state);
        let settings = ProviderSettings {
            update: ProviderUpdate::Command {
                command: vec![state.join("provider").to_string_lossy().into_owned()],
            },
            ..Default::default()
        };
        check(
            state,
            "claude",
            &state.join("provider"),
            &settings,
            &provenant_os::exec::SystemExecutor::new(1)?,
            100,
        )?;
        Ok(())
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // A separate runner must be killed without unwinding.
    fn killed_runner_keeps_exclusive_lease_until_updater_group_exits()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("provider");
        std::fs::write(&executable, b"#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.0; exit; fi\n(while [ ! -e release ]; do /bin/sleep 0.05; done) &\necho ready > ready\nwait\n")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        let mut runner = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "update::tests::updater_runner_fixture",
                "--nocapture",
            ])
            .env("PROVENANT_TEST_UPDATER_STATE", temp.path())
            .spawn()?;
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5)?;
        while !temp.path().join("ready").exists()
            && budget.remaining(clock.mono_now()).as_micros() != 0
        {
            clock.wait_until(
                clock
                    .mono_now()
                    .checked_add(DurationMicros::from_micros(10_000))?,
            )?;
        }
        runner.kill()?;
        runner.wait()?;
        let busy = provenant_os::provider_fence::acquire(temp.path(), "claude", LockMode::Shared);
        // Always release the fixture even if the regression assertion fails.
        std::fs::write(temp.path().join("release"), b"exit")?;
        assert!(temp.path().join("ready").exists(), "updater did not start");
        assert!(matches!(
            busy,
            Err(provenant_os::fs::FsError::LockTimeout { .. })
        ));
        let _lane = provenant_os::provider_fence::wait(
            temp.path(),
            "claude",
            LockMode::Shared,
            WaitBudget::cli(clock.mono_now(), 5)?,
            &CancelToken::new(),
            &clock,
        )?;
        Ok(())
    }

    #[test]
    fn version_change_is_recorded() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let fake = Fake {
            state: root,
            updated: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let record = check(
            root,
            "claude",
            Path::new("/fake/claude"),
            &ProviderSettings::default(),
            &fake,
            100,
        )?
        .ok_or("no update")?;
        assert_eq!(record.old_version.as_deref(), Some("1.0"));
        assert_eq!(record.new_version.as_deref(), Some("2.0"));
        assert_eq!(record.outcome, "updated");
        assert_eq!(latest(root, "claude"), Some(record));
        assert!(
            check(
                root,
                "claude",
                Path::new("/fake/claude"),
                &ProviderSettings::default(),
                &fake,
                101
            )?
            .is_none()
        );
        assert_eq!(fake.calls.load(Ordering::Relaxed), 3);
        Ok(())
    }

    #[test]
    fn opt_out_and_live_lane_run_no_updater() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let fake = Fake {
            state: temp.path(),
            updated: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let settings = ProviderSettings {
            update: ProviderUpdate::Enabled(false),
            ..ProviderSettings::default()
        };
        assert!(
            check(
                temp.path(),
                "claude",
                Path::new("/fake/claude"),
                &settings,
                &fake,
                100
            )?
            .is_none()
        );
        assert!(!temp.path().join("provider-updates").exists());
        let _lane = provenant_os::provider_fence::acquire(temp.path(), "claude", LockMode::Shared)?;
        assert!(
            check(
                temp.path(),
                "claude",
                Path::new("/fake/claude"),
                &ProviderSettings::default(),
                &fake,
                100
            )?
            .is_none()
        );
        assert_eq!(fake.calls.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[test]
    fn failed_update_retains_version_and_backs_off() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let fake = Fake {
            state: temp.path(),
            updated: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            fail: true,
        };
        let record = check(
            temp.path(),
            "claude",
            Path::new("/fake/claude"),
            &ProviderSettings::default(),
            &fake,
            100,
        )?
        .ok_or("no update")?;
        assert_eq!(record.outcome, "failed");
        assert_eq!(record.old_version, record.new_version);
        assert!(
            check(
                temp.path(),
                "claude",
                Path::new("/fake/claude"),
                &ProviderSettings::default(),
                &fake,
                999
            )?
            .is_none()
        );
        assert_eq!(fake.calls.load(Ordering::Relaxed), 3);
        Ok(())
    }

    #[test]
    fn post_update_version_follows_the_replaced_install_link()
    -> Result<(), Box<dyn std::error::Error>> {
        struct Replace<'a> {
            alias: &'a Path,
            old: &'a Path,
            new: &'a Path,
        }
        impl Executor for Replace<'_> {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                let bytes = if options.argv.get(1).is_some_and(|arg| arg == "--version") {
                    std::fs::read(&options.argv[0]).unwrap()
                } else {
                    assert!(options.inherited_lease.is_some());
                    assert!(options.process_group);
                    std::fs::remove_file(self.alias).unwrap();
                    std::os::unix::fs::symlink(self.new, self.alias).unwrap();
                    std::fs::remove_file(self.old).unwrap();
                    Vec::new()
                };
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
                    stdout: CapturedStream {
                        bytes,
                        ..CapturedStream::default()
                    },
                    stderr: CapturedStream::default(),
                    timed_out: false,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 1,
                })
            }
        }
        let temp = tempfile::tempdir()?;
        let old = temp.path().join("v1");
        let new = temp.path().join("v2");
        let alias = temp.path().join("codex");
        std::fs::write(&old, b"1.0")?;
        std::fs::write(&new, b"2.0")?;
        std::os::unix::fs::symlink(&old, &alias)?;
        let fake = Replace {
            alias: &alias,
            old: &old,
            new: &new,
        };
        let settings = ProviderSettings {
            update: ProviderUpdate::Command {
                command: vec!["fake".into()],
            },
            ..ProviderSettings::default()
        };
        let record =
            check(temp.path(), "codex", &alias, &settings, &fake, 100)?.ok_or("no update")?;
        assert_eq!(record.old_version.as_deref(), Some("1.0"));
        assert_eq!(record.new_version.as_deref(), Some("2.0"));
        Ok(())
    }

    #[test]
    fn argv_override_is_literal_and_failed_update_does_not_block_lanes()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::exec::SystemExecutor;
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("provider");
        std::fs::write(&executable, b"#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.0; exit; fi\nprintf '%s' \"$1\" > argument\nexit 1\n")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        let literal = "$(touch injected); * quoted argument";
        let settings = ProviderSettings {
            update: ProviderUpdate::Command {
                command: vec![executable.to_string_lossy().into_owned(), literal.into()],
            },
            ..ProviderSettings::default()
        };
        let executor = SystemExecutor::new(1)?;
        let record = check(
            temp.path(),
            "claude",
            &executable,
            &settings,
            &executor,
            100,
        )?
        .ok_or("no update")?;
        assert_eq!(record.outcome, "failed");
        assert_eq!(record.old_version.as_deref(), Some("1.0"));
        assert_eq!(record.old_version, record.new_version);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("argument"))?,
            literal
        );
        assert!(!temp.path().join("injected").exists());
        let _lane = provenant_os::provider_fence::acquire(temp.path(), "claude", LockMode::Shared)?;
        let argv = [executable.into_os_string(), "--version".into()];
        let result =
            executor.execute(ExecOptions::new(&argv, temp.path(), Duration::from_secs(2)))?;
        assert_eq!(result.termination, Termination::Exit(0));
        assert!(
            check(
                temp.path(),
                "claude",
                Path::new("/unused"),
                &settings,
                &executor,
                101
            )?
            .is_none()
        );
        Ok(())
    }

    #[test]
    fn hung_updater_kills_background_descendant_and_releases_lease()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::exec::SystemExecutor;
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("provider");
        std::fs::write(&executable, b"#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.0; exit; fi\n(trap '' TERM; echo ready > descendant.ready; /bin/sleep 2; echo survived > descendant.survived) &\nwait\n")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        let settings = ProviderSettings {
            update: ProviderUpdate::Command {
                command: vec![executable.to_string_lossy().into_owned()],
            },
            update_timeout_seconds: 1,
            ..ProviderSettings::default()
        };
        struct Observe(SystemExecutor);
        impl Executor for Observe {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                let update = options.process_group;
                let result = self.0.execute(options);
                if update {
                    assert!(
                        matches!(&result, Ok(outcome) if outcome.timed_out),
                        "updater timeout result: {result:?}"
                    );
                }
                result
            }
        }
        let executor = Observe(SystemExecutor::new(1)?);
        let record = check(
            temp.path(),
            "claude",
            &executable,
            &settings,
            &executor,
            100,
        )?
        .ok_or("no update")?;
        assert_eq!(record.outcome, "failed");
        let _lane = provenant_os::provider_fence::acquire(temp.path(), "claude", LockMode::Shared)?;
        assert!(
            temp.path().join("descendant.ready").exists(),
            "descendant never started"
        );
        // The descendant would write after its sleep if only the leader died.
        // Wait past that point without ps, which some contributor sandboxes deny.
        let argv = ["/bin/sleep".into(), "2".into()];
        let outcome =
            executor.execute(ExecOptions::new(&argv, temp.path(), Duration::from_secs(3)))?;
        assert_eq!(outcome.termination, Termination::Exit(0));
        assert!(
            !temp.path().join("descendant.survived").exists(),
            "background updater survived"
        );
        assert!(
            check(
                temp.path(),
                "claude",
                &executable,
                &settings,
                &executor,
                101
            )?
            .is_none()
        );
        Ok(())
    }

    #[test]
    fn install_detection_and_unknown_note_once() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let fake = Fake {
            state: temp.path(),
            updated: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let settings = ProviderSettings::default();
        let binary = temp.path().join("bin/codex");
        std::fs::create_dir_all(binary.parent().ok_or("parent")?)?;
        std::fs::write(&binary, b"fake")?;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
        assert_eq!(
            check(temp.path(), "codex", &binary, &settings, &fake, 100)?
                .ok_or("no note")?
                .outcome,
            "unknown install method"
        );
        assert!(check(temp.path(), "codex", &binary, &settings, &fake, 100_000)?.is_none());
        assert_eq!(
            latest(temp.path(), "codex").map(|r| r.checked_unix_seconds),
            Some(100_000)
        );
        assert!(!due(temp.path(), "codex", &settings, 100_001));
        assert_eq!(fake.calls.load(Ordering::Relaxed), 0);
        let brew = temp.path().join("Cellar/codex/1/bin/codex");
        let npm = temp
            .path()
            .join("lib/node_modules/@openai/codex/bin/codex.js");
        for path in [&brew, &npm] {
            std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
            std::fs::write(path, b"fake")?;
        }
        assert_eq!(
            command("codex", &brew, &settings.update).ok_or("brew")?[1],
            "upgrade"
        );
        assert_eq!(
            command("codex", &npm, &settings.update).ok_or("npm")?[1],
            "install"
        );
        assert_eq!(
            command(
                "claude",
                &binary,
                &ProviderUpdate::Command {
                    command: vec!["custom".into(), "update".into()]
                }
            )
            .ok_or("override")?,
            vec![OsString::from("custom"), "update".into()]
        );
        Ok(())
    }
}
