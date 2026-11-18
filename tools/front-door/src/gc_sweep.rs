//! Single-flight ownership and restart position for host-wide GC sweeps.
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, LockGuard, LockMode, PublishMode},
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    io::{self, Write},
    time::{Duration, Instant},
};

const PROGRESS_FILE: &str = "gc-progress.json";
const LOCK_FILE: &str = "gc-sweep.lock";
const EPOCH_FILE: &str = "gc-sweep-epoch";
const PROGRESS_LIMIT: u64 = 16 * 1024;
const EPOCH_LIMIT: u64 = 32;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Cursor {
    pub class: String,
    #[serde(with = "path_option")]
    pub project: Option<std::path::PathBuf>,
    #[serde(with = "path_option")]
    pub candidate: Option<std::path::PathBuf>,
    /// Opaque owner-specific continuation state, such as a cache survey token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

mod path_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::path::{Path, PathBuf};

    pub fn serialize<S>(path: &Option<PathBuf>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        path.as_ref()
            .map(|path| encode(path.as_path()))
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)?
            .map(|value| {
                decode(&value).ok_or_else(|| serde::de::Error::custom("invalid GC cursor path"))
            })
            .transpose()
    }

    #[cfg(unix)]
    fn encode(path: &Path) -> String {
        use std::os::unix::ffi::OsStrExt;
        format!("unix:{}", provenant_core::hex(path.as_os_str().as_bytes()))
    }

    #[cfg(not(unix))]
    fn encode(path: &PathBuf) -> String {
        format!("utf8:{}", path.to_string_lossy())
    }

    #[cfg(unix)]
    fn decode(value: &str) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        let bytes = provenant_core::unhex(value.strip_prefix("unix:")?)?;
        Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }

    #[cfg(not(unix))]
    fn decode(value: &str) -> Option<PathBuf> {
        Some(PathBuf::from(value.strip_prefix("utf8:")?))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Progress {
    pid: i64,
    epoch: u64,
    started_unix_seconds: i64,
    started_host_monotonic_micros: Option<u64>,
    boot_identity: Option<String>,
    budget_seconds: u64,
    released: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    cursor: Option<Cursor>,
}

pub(crate) struct Lease {
    directory: Directory,
    progress: Progress,
    pub warning: Option<String>,
    watcher: Option<FenceWatcher>,
    fenced_cancel: Option<CancelToken>,
}

struct FenceWatcher {
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[allow(clippy::large_enum_variant)] // A held lease owns its fencing watcher and durable cursor.
pub(crate) enum Acquire {
    Held(Lease),
    Busy(Option<String>),
}

/// Cooperative wall-clock deadline shared with the collectors and their
/// bounded subprocesses. Dropping it wakes and joins its timer thread.
pub(crate) struct Deadline {
    cancel: CancelToken,
    stop: std::sync::mpsc::Sender<()>,
    timer: Option<std::thread::JoinHandle<()>>,
    started: Instant,
    budget: Duration,
}

impl Deadline {
    #[allow(
        clippy::disallowed_methods,
        reason = "Start the explicit real-time sweep deadline."
    )]
    pub fn start(budget: Duration) -> Self {
        let cancel = CancelToken::new();
        let timer_cancel = cancel.clone();
        let (stop, receiver) = std::sync::mpsc::channel();
        let timer = std::thread::spawn(move || {
            if receiver.recv_timeout(budget).is_err() {
                timer_cancel.cancel();
            }
        });
        Self {
            cancel,
            stop,
            timer: Some(timer),
            started: Instant::now(),
            budget,
        }
    }

    pub fn cancel(&self) -> &CancelToken {
        &self.cancel
    }

    pub fn expired(&self) -> bool {
        elapsed_over_budget(self.started.elapsed(), self.budget)
    }

    pub fn remaining(&self) -> Duration {
        self.budget.saturating_sub(self.started.elapsed())
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(timer) = self.timer.take() {
            let _ = timer.join();
        }
    }
}

impl Lease {
    pub fn cursor(&self) -> Option<&Cursor> {
        self.progress.cursor.as_ref()
    }

    pub fn epoch(&self) -> u64 {
        self.progress.epoch
    }

    pub fn trigger(&self) -> Option<&str> {
        self.progress.trigger.as_deref()
    }

    pub fn scope(&self) -> Option<&str> {
        self.progress.scope.as_deref()
    }

    pub fn matches_identity(&self, trigger: &str, scope: &str) -> bool {
        self.cursor().is_none()
            || (self.trigger().is_none_or(|saved| saved == trigger)
                && self.scope().is_none_or(|saved| saved == scope))
    }

    pub fn set_identity(&mut self, trigger: &str, scope: &str) -> Result<(), io::Error> {
        let _lock = metadata_lock_for_release(&self.directory).map_err(io::Error::other)?;
        if !progress_has_epoch(&self.directory, self.progress.epoch) {
            if let Some(cancel) = &self.fenced_cancel {
                cancel.cancel();
            }
            return Err(io::Error::other(
                "GC sweep lease was fenced by a newer owner",
            ));
        }
        self.progress.trigger = Some(trigger.into());
        self.progress.scope = Some(scope.into());
        self.progress.released = false;
        self.write_progress_locked()
    }

    /// Validate the fencing epoch at a collector's safe boundary.
    pub fn check_fence(&self, cancel: &CancelToken) -> bool {
        if cancel.is_cancelled() || !progress_has_epoch(&self.directory, self.progress.epoch) {
            cancel.cancel();
            false
        } else {
            true
        }
    }

    /// Fence collector work promptly if another sweep takes over this lease.
    pub fn watch_fence(&mut self, cancel: CancelToken) -> Result<(), io::Error> {
        let directory = self.directory.try_clone().map_err(io::Error::other)?;
        let epoch = self.progress.epoch;
        self.fenced_cancel = Some(cancel.clone());
        let (stop, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            loop {
                match receiver.recv_timeout(Duration::from_millis(25)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if matches!(
                            fence_observation(&directory, epoch),
                            Ok(Some(false)) | Err(_)
                        ) {
                            cancel.cancel();
                            break;
                        }
                    }
                }
            }
        });
        self.watcher = Some(FenceWatcher {
            stop,
            thread: Some(thread),
        });
        Ok(())
    }

    /// Record the next safe restart point under the short metadata mutex.
    pub fn checkpoint(&mut self, cursor: Cursor) -> Result<(), io::Error> {
        let _lock = metadata_lock_for_release(&self.directory).map_err(io::Error::other)?;
        if !progress_has_epoch(&self.directory, self.progress.epoch) {
            if let Some(cancel) = &self.fenced_cancel {
                cancel.cancel();
            }
            return Err(io::Error::other(
                "GC sweep lease was fenced by a newer owner",
            ));
        }
        self.progress.cursor = Some(cursor);
        self.progress.released = false;
        self.write_progress_locked()
    }

    /// Clear progress only after every scheduled class has completed.
    pub fn complete(mut self) -> Result<(), io::Error> {
        let _lock = metadata_lock_for_release(&self.directory).map_err(io::Error::other)?;
        if !progress_has_epoch(&self.directory, self.progress.epoch) {
            if let Some(cancel) = &self.fenced_cancel {
                cancel.cancel();
            }
            return Ok(());
        }
        self.directory
            .unlink(OsStr::new(PROGRESS_FILE))
            .map_err(io::Error::other)?;
        if let Some(watcher) = self.watcher.take() {
            drop(watcher);
        }
        Ok(())
    }

    fn write_progress_locked(&self) -> Result<(), io::Error> {
        let bytes = serde_json::to_vec(&self.progress).map_err(io::Error::other)?;
        self.directory
            .atomic_write(
                OsStr::new(PROGRESS_FILE),
                ".gc-progress-",
                &bytes,
                PublishMode::Replace,
            )
            .map_err(io::Error::other)?;
        Ok(())
    }
}

impl Drop for FenceWatcher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if !progress_has_epoch(&self.directory, self.progress.epoch) {
            if let Some(cancel) = &self.fenced_cancel {
                cancel.cancel();
            }
            return;
        }
        let Ok(_lock) = metadata_lock_for_release(&self.directory) else {
            if let Some(cancel) = &self.fenced_cancel {
                cancel.cancel();
            }
            let _ = writeln!(
                io::stderr().lock(),
                "warning: could not lock GC sweep metadata to release its lease"
            );
            return;
        };
        if !progress_has_epoch(&self.directory, self.progress.epoch) {
            if let Some(cancel) = &self.fenced_cancel {
                cancel.cancel();
            }
            return;
        }
        self.progress.released = true;
        if let Err(error) = self.write_progress_locked() {
            let _ = writeln!(
                io::stderr().lock(),
                "warning: could not persist the released GC sweep cursor: {error}"
            );
        }
    }
}

/// Acquire the host-wide fenced sweep lease. The stable flock inode is only a
/// short metadata mutex; sweep work never holds it. A stale lease is replaced
/// atomically and its old owner cannot checkpoint or complete the new epoch.
pub(crate) fn acquire(
    state_root: &std::path::Path,
    budget: Duration,
    clock: &dyn Clock,
) -> Result<Acquire, Box<dyn std::error::Error>> {
    let directory = provenant_config::private_directory(&state_root.join("tick"), true)?;
    let metadata = match metadata_lock(&directory) {
        Ok(metadata) => metadata,
        Err(error) if is_lock_timeout(&error) => return Ok(Acquire::Busy(None)),
        Err(error) => return Err(Box::new(error)),
    };
    let old = read_progress(&directory)?;
    let old_over_budget = old
        .as_ref()
        .is_some_and(|progress| holder_over_budget(progress, clock));
    let old_dead = old.as_ref().is_some_and(|progress| {
        matches!(
            provenant_os::pid::process_state(progress.pid),
            Ok(provenant_os::pid::ProcessState::Gone)
        )
    });
    if old
        .as_ref()
        .is_some_and(|progress| !progress.released && !old_over_budget && !old_dead)
    {
        return Ok(Acquire::Busy(None));
    }
    let warning = old.as_ref().and_then(|progress| {
        if progress.released {
            return None;
        }
        if old_over_budget {
            Some(format!(
                "taking over GC sweep owned by pid {} after gc.sweep_budget expired",
                progress.pid
            ))
        } else if old_dead {
            Some(format!(
                "taking over GC sweep from dead pid {} at its saved cursor",
                progress.pid
            ))
        } else {
            None
        }
    });
    let epoch = next_epoch(&directory, old.as_ref().map(|progress| progress.epoch))?;
    let progress = Progress {
        pid: std::process::id() as i64,
        epoch,
        started_unix_seconds: clock.unix_seconds(),
        started_host_monotonic_micros: clock.host_monotonic_micros().ok(),
        boot_identity: clock.boot_identity().ok(),
        budget_seconds: budget.as_secs().max(1),
        released: false,
        trigger: old.as_ref().and_then(|progress| progress.trigger.clone()),
        scope: old.as_ref().and_then(|progress| progress.scope.clone()),
        cursor: old.and_then(|progress| progress.cursor),
    };
    let lease = Lease {
        directory,
        progress,
        warning,
        watcher: None,
        fenced_cancel: None,
    };
    lease.write_progress_locked()?;
    drop(metadata);
    Ok(Acquire::Held(lease))
}

fn metadata_lock(directory: &Directory) -> Result<LockGuard, provenant_os::fs::FsError> {
    directory.try_lock(OsStr::new(LOCK_FILE), LockMode::Exclusive)
}

// A checkpoint replaces the progress file under this mutex. A busy mutex is
// not evidence of takeover; retry rather than cancelling our own sweep.
fn fence_observation(
    directory: &Directory,
    epoch: u64,
) -> Result<Option<bool>, provenant_os::fs::FsError> {
    match metadata_lock(directory) {
        Ok(_guard) => Ok(Some(progress_has_epoch(directory, epoch))),
        Err(error) if is_lock_timeout(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn metadata_lock_for_release(
    directory: &Directory,
) -> Result<LockGuard, provenant_os::fs::FsError> {
    let clock = SystemClock::new();
    let budget = WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(100_000));
    directory.lock(
        OsStr::new(LOCK_FILE),
        LockMode::Exclusive,
        budget,
        &CancelToken::new(),
        &clock,
    )
}

fn read_progress(directory: &Directory) -> Result<Option<Progress>, Box<dyn std::error::Error>> {
    let bytes = match directory.read_stable(OsStr::new(PROGRESS_FILE), PROGRESS_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(Box::new(error)),
    };
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn progress_has_epoch(directory: &Directory, epoch: u64) -> bool {
    read_progress(directory)
        .ok()
        .flatten()
        .is_some_and(|progress| progress.epoch == epoch && !progress.released)
}

fn next_epoch(
    directory: &Directory,
    prior_epoch: Option<u64>,
) -> Result<u64, Box<dyn std::error::Error>> {
    let stored = match directory.read_stable(OsStr::new(EPOCH_FILE), EPOCH_LIMIT) {
        Ok(bytes) => std::str::from_utf8(&bytes)?.parse::<u64>()?,
        Err(error) if error.is_not_found() => 0,
        Err(error) => return Err(Box::new(error)),
    };
    let epoch = stored
        .max(prior_epoch.unwrap_or(0))
        .checked_add(1)
        .ok_or_else(|| io::Error::other("GC sweep epoch exhausted"))?;
    directory
        .atomic_write(
            OsStr::new(EPOCH_FILE),
            ".gc-epoch-",
            epoch.to_string().as_bytes(),
            PublishMode::Replace,
        )
        .map_err(io::Error::other)?;
    Ok(epoch)
}

#[cfg(test)]
#[allow(dead_code)] // The worktree driver includes the library without a test harness.
fn write_progress(directory: &Directory, progress: &Progress) -> Result<(), io::Error> {
    let bytes = serde_json::to_vec(progress).map_err(io::Error::other)?;
    directory
        .atomic_write(
            OsStr::new(PROGRESS_FILE),
            ".gc-progress-",
            &bytes,
            PublishMode::Replace,
        )
        .map_err(io::Error::other)?;
    Ok(())
}

fn is_lock_timeout(error: &provenant_os::fs::FsError) -> bool {
    matches!(error, provenant_os::fs::FsError::LockTimeout { .. })
}

fn elapsed_since(started: i64, now: i64) -> u64 {
    now.saturating_sub(started).try_into().unwrap_or(0)
}

fn holder_over_budget(progress: &Progress, clock: &dyn Clock) -> bool {
    let now_boot = clock.boot_identity().ok();
    if progress.boot_identity.is_some()
        && progress.boot_identity == now_boot
        && let (Some(started), Ok(now)) = (
            progress.started_host_monotonic_micros,
            clock.host_monotonic_micros(),
        )
    {
        let budget_micros = progress.budget_seconds.saturating_mul(1_000_000);
        return now.saturating_sub(started) >= budget_micros;
    }
    elapsed_since(progress.started_unix_seconds, clock.unix_seconds()) >= progress.budget_seconds
}

fn elapsed_over_budget(elapsed: Duration, budget: Duration) -> bool {
    elapsed >= budget
}

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The worktree driver includes the library without a test harness.
mod tests {
    use super::*;
    use provenant_core::{DurationMicros, MonoTime, TestClock, WallTime};

    fn test_clock() -> TestClock {
        TestClock::new(
            WallTime::from_micros(100_000_000).unwrap(),
            MonoTime::from_micros(10),
        )
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn failed_setup_without_a_cursor_does_not_reserve_a_scope() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let budget = Duration::from_secs(30);
        let Acquire::Held(mut first) = acquire(&root, budget, &clock).unwrap() else {
            panic!("new sweep is busy");
        };
        first.set_identity("full_gc", "manual:first").unwrap();
        drop(first);
        let Acquire::Held(next) = acquire(&root, budget, &clock).unwrap() else {
            panic!("released sweep is busy");
        };
        assert!(next.matches_identity("full_gc", "manual:second"));
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn busy_checkpoint_mutex_is_not_a_fence() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let Acquire::Held(lease) = acquire(&root, Duration::from_secs(30), &clock).unwrap() else {
            panic!("new sweep is busy");
        };
        let guard = metadata_lock(&lease.directory).unwrap();
        assert_eq!(
            fence_observation(&lease.directory, lease.epoch()).unwrap(),
            None
        );
        drop(guard);
        assert_eq!(
            fence_observation(&lease.directory, lease.epoch()).unwrap(),
            Some(true)
        );
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn deadline_is_injected_and_stops_at_the_budget() {
        assert!(!elapsed_over_budget(
            Duration::from_millis(999),
            Duration::from_secs(1)
        ));
        assert!(elapsed_over_budget(
            Duration::from_secs(1),
            Duration::from_secs(1)
        ));
        let mut clock = test_clock();
        let progress = Progress {
            pid: 41,
            epoch: 1,
            started_unix_seconds: clock.unix_seconds(),
            started_host_monotonic_micros: clock.host_monotonic_micros().ok(),
            boot_identity: clock.boot_identity().ok(),
            budget_seconds: 5,
            released: false,
            trigger: None,
            scope: None,
            cursor: None,
        };
        assert!(!holder_over_budget(&progress, &clock));
        clock
            .advance(DurationMicros::from_micros(4_999_999))
            .unwrap();
        assert!(!holder_over_budget(&progress, &clock));
        clock.advance(DurationMicros::from_micros(1)).unwrap();
        assert!(holder_over_budget(&progress, &clock));
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn live_over_budget_lease_is_fenced_and_resumes_its_saved_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let mut clock = test_clock();
        let budget = Duration::from_secs(30);
        let mut old = match acquire(temp.path(), budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("new lock should be available"),
        };
        let cursor = Cursor {
            class: "GitWorktrees".into(),
            project: Some("/repo/project".into()),
            candidate: Some("/repo/project/.worktrees/lane".into()),
            detail: None,
        };
        let old_epoch = old.epoch();
        old.checkpoint(cursor.clone()).unwrap();
        let cancel = CancelToken::new();
        old.watch_fence(cancel.clone()).unwrap();
        assert!(matches!(
            acquire(temp.path(), budget, &clock).unwrap(),
            Acquire::Busy(_)
        ));

        clock
            .advance(DurationMicros::from_micros(31_000_000))
            .unwrap();
        let current = match acquire(temp.path(), budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("expired lease should be fenced and taken over"),
        };
        assert!(current.epoch() > old_epoch);
        assert_eq!(current.cursor(), Some(&cursor));
        assert!(
            current
                .warning
                .as_deref()
                .is_some_and(|warning| warning.contains("gc.sweep_budget"))
        );
        assert!(old.checkpoint(Cursor::default()).is_err());
        assert!(cancel.is_cancelled());
        old.complete().unwrap();
        assert_eq!(current.cursor(), Some(&cursor));

        drop(current);
        // An early exit releases its lease but keeps the cursor for resumption.
        assert!(matches!(
            acquire(temp.path(), budget, &clock).unwrap(),
            Acquire::Held(_)
        ));
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn dead_owner_is_taken_over_with_a_warning() {
        let temp = tempfile::tempdir().unwrap();
        let clock = test_clock();
        let directory =
            provenant_config::private_directory(&temp.path().join("tick"), true).unwrap();
        let progress = Progress {
            pid: 2_000_000_000,
            epoch: 3,
            started_unix_seconds: clock.unix_seconds(),
            started_host_monotonic_micros: clock.host_monotonic_micros().ok(),
            boot_identity: clock.boot_identity().ok(),
            budget_seconds: 30,
            released: false,
            trigger: None,
            scope: None,
            cursor: Some(Cursor {
                class: "GitWorktrees".into(),
                project: None,
                candidate: None,
                detail: None,
            }),
        };
        next_epoch(&directory, Some(progress.epoch)).unwrap();
        write_progress(&directory, &progress).unwrap();
        let lease = match acquire(temp.path(), Duration::from_secs(30), &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("dead owner should be taken over"),
        };
        assert!(lease.epoch() > progress.epoch);
        assert!(
            lease
                .warning
                .as_deref()
                .is_some_and(|warning| warning.contains("dead pid"))
        );
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn early_exit_releases_lease_and_keeps_the_resume_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let clock = test_clock();
        let budget = Duration::from_secs(30);
        let mut lease = match acquire(temp.path(), budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("new sweep should acquire its lease"),
        };
        let cursor = Cursor {
            class: "FixtureCandidates".into(),
            project: Some("/repo".into()),
            candidate: Some("/repo/next".into()),
            detail: None,
        };
        lease.checkpoint(cursor.clone()).unwrap();
        drop(lease);

        let resumed = match acquire(temp.path(), budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("early exit must release its lease immediately"),
        };
        assert_eq!(resumed.cursor(), Some(&cursor));
        assert!(resumed.warning.is_none());
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn saved_cursor_is_bound_to_its_trigger_and_selector_scope() {
        let temp = tempfile::tempdir().unwrap();
        let clock = test_clock();
        let budget = Duration::from_secs(30);
        let mut lease = match acquire(temp.path(), budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("new sweep should acquire its lease"),
        };
        lease.set_identity("full_gc", "scheduled").unwrap();
        let cursor = Cursor {
            class: "GitWorktrees".into(),
            project: Some("/repo/project".into()),
            candidate: Some("/repo/project/.worktrees/lane".into()),
            detail: None,
        };
        lease.checkpoint(cursor.clone()).unwrap();
        drop(lease);

        let resumed = match acquire(temp.path(), budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("released sweep should resume"),
        };
        assert_eq!(resumed.cursor(), Some(&cursor));
        assert!(resumed.warning.is_none());
        assert!(resumed.matches_identity("full_gc", "scheduled"));
        assert!(!resumed.matches_identity("pressure", "scheduled"));
        assert!(!resumed.matches_identity("full_gc", "manual:repo-a"));
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn bounded_fixture_sweep_releases_and_resumes_at_its_last_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = temp.path().join("candidates");
        std::fs::create_dir(&fixture).unwrap();
        for index in 0..256 {
            std::fs::create_dir(fixture.join(format!("candidate-{index:03}"))).unwrap();
        }

        let mut clock = test_clock();
        let budget = Duration::from_secs(1);
        let mut lease = match acquire(temp.path(), budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("new fixture sweep should acquire its lease"),
        };
        let mut visited = 0;
        for (index, entry) in std::fs::read_dir(&fixture).unwrap().enumerate() {
            if holder_over_budget(&lease.progress, &clock) {
                break;
            }
            let candidate = entry.unwrap().path();
            lease
                .checkpoint(Cursor {
                    class: "FixtureCandidates".into(),
                    project: Some(fixture.clone()),
                    candidate: Some(candidate),
                    detail: None,
                })
                .unwrap();
            visited += 1;
            if index == 7 {
                clock
                    .advance(DurationMicros::from_micros(1_100_000))
                    .unwrap();
            }
        }
        let stopped_at = lease.cursor().cloned().unwrap();
        assert_eq!(visited, 8);
        assert!(
            stopped_at
                .candidate
                .as_ref()
                .is_some_and(|path| path.starts_with(&fixture))
        );
        drop(lease);

        let resumed = match acquire(temp.path(), budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("released fixture sweep should resume"),
        };
        assert_eq!(resumed.cursor(), Some(&stopped_at));
        let start = stopped_at.candidate.as_ref().unwrap();
        let mut candidates = std::fs::read_dir(&fixture)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        candidates.sort();
        let resumed_candidates = candidates
            .into_iter()
            .filter(|candidate| candidate >= start)
            .collect::<Vec<_>>();
        assert_eq!(resumed_candidates.first(), Some(start));
        assert_eq!(resumed_candidates.len(), 256 - visited + 1);
    }

    #[cfg(unix)]
    #[test]
    fn saved_cursor_round_trips_native_path_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let cursor = Cursor {
            class: "GitWorktrees".into(),
            project: Some(std::path::PathBuf::from(std::ffi::OsString::from_vec(
                b"/repo/\xff".to_vec(),
            ))),
            candidate: None,
            detail: None,
        };
        let bytes = serde_json::to_vec(&cursor).unwrap();
        let resumed: Cursor = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resumed, cursor);
    }
}
