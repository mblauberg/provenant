//! Host-only command preparation, kept separate from attempt-owned storage.
use provenant_supervisor::{
    exec::{CaptureFilter, ExecError, ExecOptions, ExecOutcome, Executor},
    fs::{self, Directory, LockGuard, LockMode, temporary_name},
};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

const PREFIX: &str = "probe-";
const LOCK_SUFFIX: &str = ".lock";
/// The random part of `temporary_name`, after its fixed staging prefix.
const STAGING_PREFIX: &str = ".provenant-staging.";
const ID_LEN: usize = 24;

/// A fresh empty 0700 directory under a host-trusted root, removed on drop.
///
/// Liveness is an exclusive flock on a sibling `probe-<id>.lock`, taken before
/// the directory exists and held until the directory is gone. The kernel drops
/// it when the owner dies, even by SIGKILL, so a free lock beside a directory
/// proves its owner is gone. `sweep_stale` relies on that.
pub struct HostProbeDir {
    lock_name: OsString,
    root_path: PathBuf,
    path: PathBuf,
    // Dropped after `drop` has removed the directory and lock file.
    _lock: LockGuard,
}

impl HostProbeDir {
    /// The root must be host-trusted configuration state, never a child store
    /// or any part of an attempt's LaunchPaths.
    pub fn create(host_root: &Path) -> fs::Result<Self> {
        Self::create_in(&Directory::open_anchor(host_root)?, host_root)
    }

    fn create_in(root: &Directory, host_root: &Path) -> fs::Result<Self> {
        let staging = temporary_name()?;
        let id = staging
            .to_str()
            .and_then(|name| name.strip_prefix(STAGING_PREFIX))
            .ok_or_else(|| fs::FsError::RaceRefused {
                name: staging.clone(),
            })?
            .to_owned();
        let name = OsString::from(format!("{PREFIX}{id}"));
        let lock_name = OsString::from(format!("{PREFIX}{id}{LOCK_SUFFIX}"));
        // Lock first: any directory a sweeper can see is already leased.
        let lock = root.try_lock(&lock_name, LockMode::Exclusive)?;
        let created = root.mkdir(&name, 0o700);
        if let Err(error) = created {
            let _ = std::fs::remove_file(host_root.join(&lock_name));
            return Err(error);
        }
        Ok(Self {
            path: host_root.join(&name),
            lock_name,
            root_path: host_root.to_owned(),
            _lock: lock,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for HostProbeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        // The directory API reserves `.lock` names, so unlink by path. The
        // root is host-trusted and this probe still holds the lock.
        let _ = std::fs::remove_file(self.root_path.join(&self.lock_name));
    }
}

/// The `<id>` of a `probe-<id>` or `probe-<id>.lock` entry name.
fn probe_id(name: &OsStr) -> Option<&str> {
    let name = name.to_str()?.strip_prefix(PREFIX)?;
    let id = name.strip_suffix(LOCK_SUFFIX).unwrap_or(name);
    (id.len() == ID_LEN && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))).then_some(id)
}

/// Remove probe directories and lock files left by owners that died. A probe
/// is stale only when its lock can be taken exclusively here: a live owner
/// holds it, so its directory is never touched. Best effort; an entry that
/// cannot be inspected is left for the next sweep.
fn sweep_stale(root: &Directory, host_root: &Path) {
    let Ok(entries) = root.entries() else {
        return;
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|name| probe_id(&name).map(str::to_owned))
        .collect();
    ids.sort();
    ids.dedup();
    for id in ids {
        let name = OsString::from(format!("{PREFIX}{id}"));
        let lock_name = OsString::from(format!("{PREFIX}{id}{LOCK_SUFFIX}"));
        let Ok(_lease) = root.try_lock(&lock_name, LockMode::Exclusive) else {
            continue;
        };
        match root.metadata_at(&name, false) {
            Ok(metadata) if metadata.kind == fs::FileKind::Directory => {
                if std::fs::remove_dir_all(host_root.join(&name)).is_err() {
                    continue;
                }
            }
            Ok(_) => continue,
            Err(error) if error.is_not_found() => {}
            Err(_) => continue,
        }
        let _ = std::fs::remove_file(host_root.join(&lock_name));
    }
}

/// Every command gets a new empty private cwd, regardless of the cwd supplied
/// by its caller. The root must be host-trusted configuration state, never a
/// child store or any part of an attempt's LaunchPaths. Creating the executor
/// first sweeps probe directories that crashed owners left behind.
pub struct HostProbeExecutor<'a> {
    executor: &'a dyn Executor,
    root: Directory,
    path: PathBuf,
}

impl<'a> HostProbeExecutor<'a> {
    pub fn new(executor: &'a dyn Executor, host_root: &Path) -> fs::Result<Self> {
        let root = Directory::open_anchor(host_root)?;
        sweep_stale(&root, host_root);
        Ok(Self {
            executor,
            root,
            path: host_root.to_owned(),
        })
    }
}

impl Executor for HostProbeExecutor<'_> {
    fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
        // mkdir is exclusive: an existing entry is an error, never a cwd.
        let scratch = HostProbeDir::create_in(&self.root, &self.path);
        let scratch = scratch.map_err(|error| ExecError::Io {
            operation: "create host probe directory",
            source: std::io::Error::other(error),
        })?;
        // Reborrow filters with the shorter cwd lifetime. Struct update syntax
        // would retain ExecOptions' original, invariant filter lifetime.
        self.executor.execute(ExecOptions {
            argv: options.argv,
            env: options.env,
            cwd: scratch.path(),
            stdin: options.stdin,
            secret_input: options.secret_input,
            stdout: options.stdout,
            stderr: options.stderr,
            stdout_decoder: options.stdout_decoder,
            stderr_decoder: options.stderr_decoder,
            stdout_filter: options
                .stdout_filter
                .map(|filter| filter as &mut dyn CaptureFilter),
            stderr_filter: options
                .stderr_filter
                .map(|filter| filter as &mut dyn CaptureFilter),
            cancel: options.cancel,
            timeout: options.timeout,
            terminate_grace: options.terminate_grace,
            reap_timeout: options.reap_timeout,
            stop_on_output_limit: options.stop_on_output_limit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_host_commands_remove_their_private_cwd() {
        struct Fail;
        impl Executor for Fail {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                assert!(options.cwd.is_dir());
                std::fs::write(options.cwd.join("partial"), b"partial").unwrap();
                Err(ExecError::Busy)
            }
        }
        let host_root = tempfile::tempdir().unwrap();
        let executor = HostProbeExecutor::new(&Fail, host_root.path()).unwrap();
        let argv = ["provider".into(), "--version".into()];
        assert!(matches!(
            executor.execute(ExecOptions::new(
                &argv,
                Path::new("/child/scratch"),
                std::time::Duration::from_secs(1),
            )),
            Err(ExecError::Busy)
        ));
        assert_eq!(std::fs::read_dir(host_root.path()).unwrap().count(), 0);
    }

    const STALE: &str = "probe-0123456789abcdef01234567";

    #[test]
    fn start_up_sweeps_crashed_probes_but_never_a_live_one() {
        struct Never;
        impl Executor for Never {
            fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                Err(ExecError::Busy)
            }
        }
        let host_root = tempfile::tempdir().unwrap();
        let root = host_root.path();
        // A SIGKILLed owner leaves its directory and an unlocked lock file.
        std::fs::create_dir(root.join(STALE)).unwrap();
        std::fs::write(root.join(STALE).join("partial"), b"x").unwrap();
        std::fs::write(root.join(format!("{STALE}.lock")), b"").unwrap();
        // A lock file whose directory is already gone is also stale.
        std::fs::write(root.join("probe-fedcba9876543210fedcba98.lock"), b"").unwrap();
        // Names outside the exact pattern are never touched.
        for name in ["probe-short", "probe-0123456789abcdef0123456g", "other"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        let live = HostProbeDir::create(root).unwrap();
        assert!(live.path().is_dir());

        let _executor = HostProbeExecutor::new(&Never, root).unwrap();

        assert!(!root.join(STALE).exists(), "stale directory is collected");
        assert!(!root.join(format!("{STALE}.lock")).exists());
        assert!(!root.join("probe-fedcba9876543210fedcba98.lock").exists());
        assert!(live.path().is_dir(), "a live probe is never removed");
        for name in ["probe-short", "probe-0123456789abcdef0123456g", "other"] {
            assert!(root.join(name).is_dir(), "{name}");
        }
        // Once its owner is done the next sweep may take it, and Drop removes it.
        let kept = live.path().to_owned();
        let live_lock = format!("{}.lock", kept.file_name().unwrap().to_string_lossy());
        drop(live);
        assert!(!kept.exists());
        assert!(!root.join(live_lock).exists());
    }
}
