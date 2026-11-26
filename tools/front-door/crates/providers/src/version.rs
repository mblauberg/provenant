//! Harness version probing. One bounded `--version` call per binary, cached by
//! path, modification time and length so a run's many lanes share one read.
use provenant_config::ProviderSettings;
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime},
};

/// The bound for one adapter's probe, or the reason it is off. Missing settings
/// take the compiled defaults, so an unconfigured adapter is probed. A disabled
/// probe names its key and runs nothing.
pub fn configured(provider: &str, settings: Option<&ProviderSettings>) -> Result<Duration, String> {
    let defaults;
    let settings = match settings {
        Some(settings) => settings,
        None => {
            defaults = ProviderSettings::default();
            &defaults
        }
    };
    if settings.version_probe {
        Ok(settings.version_probe_timeout(provider))
    } else {
        Err(format!(
            "version probe disabled by providers.{provider}.version_probe"
        ))
    }
}

/// The process-wide cache every caller shares, so the matrix, `check` and
/// launch preparation read one binary once per process.
pub fn shared() -> &'static VersionCache {
    static CACHE: OnceLock<VersionCache> = OnceLock::new();
    CACHE.get_or_init(VersionCache::new)
}

/// Read one harness's `--version` output. Non-zero exits, timeouts, truncated
/// captures and control characters are not a usable version. The caller bounds
/// the read so a weekly-changing harness CLI cannot stall a command.
pub fn probe(
    executor: &dyn Executor,
    executable: &Path,
    cwd: &Path,
    timeout: Duration,
) -> Option<String> {
    let argv = [executable.into(), "--version".into()];
    let mut options = ExecOptions::new(&argv, cwd, timeout);
    options.stdout = StreamPolicy::Capture { limit: 128 };
    options.stderr = StreamPolicy::Discard;
    let result = executor.execute(options).ok()?;
    if result.termination != Termination::Exit(0) || result.timed_out || result.stdout.truncated {
        return None;
    }
    let text = std::str::from_utf8(&result.stdout.bytes).ok()?.trim();
    (!text.is_empty() && !text.chars().any(char::is_control)).then(|| text.to_owned())
}

/// Process-wide cache of version reads, keyed by the binary's identity. A
/// replaced or rewritten binary changes mtime or length and is probed again.
#[derive(Debug, Default)]
pub struct VersionCache {
    entries: Mutex<HashMap<PathBuf, CachedVersion>>,
}

/// One cached read and the binary identity it belongs to.
#[derive(Debug, Clone)]
struct CachedVersion {
    modified: SystemTime,
    length: u64,
    version: Option<String>,
}

impl VersionCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read(
        &self,
        executor: &dyn Executor,
        executable: &Path,
        cwd: &Path,
        timeout: Duration,
    ) -> Option<String> {
        let metadata = std::fs::metadata(executable).ok()?;
        let modified = metadata.modified().ok()?;
        let length = metadata.len();
        if let Ok(entries) = self.entries.lock()
            && let Some(cached) = entries.get(executable)
            && cached.modified == modified
            && cached.length == length
        {
            return cached.version.clone();
        }
        let version = probe(executor, executable, cwd, timeout);
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(
                executable.to_path_buf(),
                CachedVersion {
                    modified,
                    length,
                    version: version.clone(),
                },
            );
        }
        version
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_os::exec::{CapturedStream, ExecError, ExecOutcome};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting {
        calls: AtomicUsize,
    }
    impl Executor for Counting {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            assert!(options.argv.iter().any(|arg| arg == "--version"));
            Ok(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: CapturedStream {
                    bytes: b"codex 0.61.0\n".to_vec(),
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
    fn cache_probes_once_until_mtime_or_length_changes() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("codex");
        std::fs::write(&executable, b"binary-v1")?;
        let executor = Counting {
            calls: AtomicUsize::new(0),
        };
        let cache = VersionCache::new();

        assert_eq!(
            cache
                .read(&executor, &executable, temp.path(), Duration::from_secs(4))
                .as_deref(),
            Some("codex 0.61.0")
        );
        assert_eq!(
            cache
                .read(&executor, &executable, temp.path(), Duration::from_secs(4))
                .as_deref(),
            Some("codex 0.61.0")
        );
        assert_eq!(executor.calls.load(Ordering::Relaxed), 1);

        // An unchanged length with a later mtime is a new binary.
        let file = std::fs::File::options().write(true).open(&executable)?;
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(10))?;
        drop(file);
        assert!(
            cache
                .read(&executor, &executable, temp.path(), Duration::from_secs(4))
                .is_some()
        );
        assert_eq!(executor.calls.load(Ordering::Relaxed), 2);

        // A changed length is a new binary even without an mtime change.
        std::fs::write(&executable, b"binary-v2-longer")?;
        let file = std::fs::File::options().write(true).open(&executable)?;
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(10))?;
        drop(file);
        assert!(
            cache
                .read(&executor, &executable, temp.path(), Duration::from_secs(4))
                .is_some()
        );
        assert_eq!(executor.calls.load(Ordering::Relaxed), 3);
        Ok(())
    }

    #[test]
    fn probe_uses_the_callers_timeout() -> Result<(), Box<dyn std::error::Error>> {
        struct Recorder(std::sync::Mutex<Option<Duration>>);
        impl Executor for Recorder {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                *self.0.lock().map_err(|_| ExecError::Cancelled)? = Some(options.timeout);
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
                    stdout: CapturedStream {
                        bytes: b"codex 0.61.0\n".to_vec(),
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
        let recorder = Recorder(std::sync::Mutex::new(None));
        let timeout = Duration::from_secs(17);
        assert_eq!(
            probe(&recorder, &temp.path().join("codex"), temp.path(), timeout).as_deref(),
            Some("codex 0.61.0")
        );
        assert_eq!(
            *recorder.0.lock().map_err(|_| "lock poisoned")?,
            Some(timeout)
        );
        Ok(())
    }
}
