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
    let bytes = dir.read_stable(&record_name(provider), 4096).ok()?;
    serde_json::from_slice(&bytes).ok()
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
    if matches!(settings.update, ProviderUpdate::Enabled(false)) {
        return Ok(None);
    }
    let _lease = match provenant_os::provider_fence::acquire(state, provider, LockMode::Exclusive) {
        Ok(lease) => lease,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let dir = Directory::open_anchor(&state.join("provider-updates")).map_err(|e| e.to_string())?;
    let previous = latest(state, provider);
    let argv = command(provider, executable, &settings.update);
    if previous.as_ref().is_some_and(|record| {
        (record.outcome == "unknown install method" && argv.is_none())
            || now.saturating_sub(record.checked_unix_seconds)
                < i64::try_from(settings.update_interval_seconds).unwrap_or(i64::MAX)
    }) {
        return Ok(None);
    }
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
    let old_version = version(executor, executable, state);
    // Save the check before launching so interruption also observes the backoff.
    let mut record = UpdateRecord {
        checked_unix_seconds: now,
        old_version: old_version.clone(),
        new_version: old_version,
        outcome: "failed (interrupted)".into(),
    };
    save(&dir, &record_name(provider), &record)?;
    let mut options = ExecOptions::new(
        &argv,
        state,
        Duration::from_secs(settings.update_timeout_seconds),
    );
    options.stdout = StreamPolicy::Discard;
    options.stderr = StreamPolicy::Discard;
    // Only housekeeping owns the exclusive lease. Updater descendants must
    // never retain it after timeout; the executor kills their private group.
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
                assert!(options.inherited_lease.is_none());
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
                    assert!(options.inherited_lease.is_none());
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
