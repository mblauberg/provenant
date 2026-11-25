//! Single-flight ownership and restart position for host-wide GC sweeps.
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, LockGuard, LockMode, PublishMode},
};
use provenant_records::warning::{CONFIG_MALFORMED, SETTING_LAYER_IGNORED, Warning, warn};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    time::{Duration, Instant},
};

const PROGRESS_FILE: &str = "gc-progress.json";
const LOCK_FILE: &str = "gc-sweep.lock";
const EPOCH_FILE: &str = "gc-sweep-epoch";
const PROGRESS_LIMIT: u64 = 16 * 1024;
// One-time bound for converting progress files written before cursors moved
// into per-scope sidecars. New lease records remain limited to PROGRESS_LIMIT.
const LEGACY_PROGRESS_LIMIT: u64 = 64 * 1024 * 1024;
// Rewriting an accepted legacy record can add lease metadata. Pending
// migration records have bounded headroom; ordinary legacy input does not.
const MIGRATION_PROGRESS_LIMIT: u64 = LEGACY_PROGRESS_LIMIT + PROGRESS_LIMIT;
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
    fn encode(path: &Path) -> String {
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
    #[serde(default)]
    cursor: Option<Cursor>,
    // Old records have no marker. Once migrated, sidecars are authoritative.
    #[serde(default)]
    scope_cursors_v2: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    migration_pending: bool,
    // Read for migration from the original inline format, then omitted from
    // every new write so the lease file stays bounded regardless of scopes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    cursors: BTreeMap<String, Cursor>,
}

#[derive(Serialize, Deserialize)]
struct ScopeCursor {
    key: String,
    cursor: Cursor,
}

fn scope_key(trigger: &str, scope: &str) -> String {
    format!("{trigger}|{scope}")
}

fn scope_cursor_name(key: &str) -> OsString {
    let digest = Sha256::digest(key.as_bytes());
    format!("gc-cursor-{}", provenant_core::hex(&digest)).into()
}

fn read_scope_cursor(directory: &Directory, key: &str) -> Result<Option<Cursor>, io::Error> {
    let name = scope_cursor_name(key);
    let bytes = match directory.read_stable(&name, PROGRESS_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(io::Error::other(error)),
    };
    let saved: ScopeCursor = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if saved.key != key {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GC scope cursor digest collision",
        ));
    }
    Ok(Some(saved.cursor))
}

fn write_scope_cursor(directory: &Directory, key: &str, cursor: &Cursor) -> Result<(), io::Error> {
    let bytes = serde_json::to_vec(&ScopeCursor {
        key: key.to_owned(),
        cursor: cursor.clone(),
    })
    .map_err(io::Error::other)?;
    if bytes.len() as u64 > PROGRESS_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GC scope cursor exceeds its size limit",
        ));
    }
    directory
        .atomic_write(
            &scope_cursor_name(key),
            ".gc-cursor-",
            &bytes,
            PublishMode::Replace,
        )
        .map_err(io::Error::other)?;
    Ok(())
}

fn clear_scope_cursor(directory: &Directory, key: &str) -> Result<(), io::Error> {
    match directory.unlink(&scope_cursor_name(key)) {
        Ok(()) => Ok(()),
        Err(error) if error.is_not_found() => Ok(()),
        Err(error) => Err(io::Error::other(error)),
    }
}

fn migrate_legacy_cursors(directory: &Directory, progress: &mut Progress) -> Result<(), io::Error> {
    for (key, cursor) in &progress.cursors {
        if read_scope_cursor(directory, key)?.is_none() {
            write_scope_cursor(directory, key, cursor)?;
        }
    }
    // An empty map with an inline cursor is ambiguous: a sidecar-era
    // completion may have removed the cursor just before a crash. Restarting
    // that scope from the beginning is safe; resurrecting it could skip work.
    progress.cursors.clear();
    Ok(())
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
            return Ok(());
        }
        let key = scope_key(trigger, scope);
        let cursor = read_scope_cursor(&self.directory, &key)?;
        self.progress.trigger = Some(trigger.into());
        self.progress.scope = Some(scope.into());
        self.progress.cursor = cursor;
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
            return Ok(());
        }
        if let (Some(trigger), Some(scope)) = (self.trigger(), self.scope()) {
            let key = scope_key(trigger, scope);
            write_scope_cursor(&self.directory, &key, &cursor)?;
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
        if let (Some(trigger), Some(scope)) = (self.trigger(), self.scope()) {
            let key = scope_key(trigger, scope);
            clear_scope_cursor(&self.directory, &key)?;
        }
        self.progress.cursor = None;
        // Stop the watcher first: it reads without the mutex, so it would
        // otherwise read the cleared record as a takeover.
        if let Some(watcher) = self.watcher.take() {
            drop(watcher);
        }
        self.directory
            .unlink(OsStr::new(PROGRESS_FILE))
            .map_err(io::Error::other)?;
        Ok(())
    }

    fn write_progress_locked(&self) -> Result<(), io::Error> {
        let bytes = serde_json::to_vec(&self.progress).map_err(io::Error::other)?;
        if bytes.len() as u64 > PROGRESS_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "GC progress record exceeds its size limit",
            ));
        }
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
        // The unlocked watcher would read the released record as a takeover.
        if let Some(watcher) = self.watcher.take() {
            drop(watcher);
        }
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
            warn(
                Warning::new(
                    provenant_records::warning::GC_SWEEP_LEASE_UNRELEASED,
                    "could not lock GC sweep metadata to release its lease",
                )
                .once_per_change(),
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
            warn(
                Warning::new(
                    provenant_records::warning::GC_SWEEP_CURSOR_UNWRITTEN,
                    format!("could not persist the released GC sweep cursor: {error}"),
                )
                .once_per_change(),
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
    let mut old = read_progress(&directory)?;
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
    if let Some(progress) = &mut old
        && !progress.scope_cursors_v2
    {
        // Fence the previous holder before the potentially long migration.
        // Keep the legacy payload intact until every sidecar has been written,
        // so a crash or write failure can retry it under the new epoch.
        progress.pid = std::process::id() as i64;
        progress.epoch = epoch;
        progress.started_unix_seconds = clock.unix_seconds();
        progress.started_host_monotonic_micros = clock.host_monotonic_micros().ok();
        progress.boot_identity = clock.boot_identity().ok();
        progress.budget_seconds = budget.as_secs().max(1);
        progress.released = false;
        progress.migration_pending = true;
        write_legacy_progress(&directory, progress)?;
        if let Err(error) = migrate_legacy_cursors(&directory, progress) {
            progress.released = true;
            let _ = write_legacy_progress(&directory, progress);
            return Err(error.into());
        }
        progress.scope_cursors_v2 = true;
    }
    if budget.is_zero() {
        warn(
            Warning::new(
                SETTING_LAYER_IGNORED,
                "gc.sweep_budget=0 checkpoints immediately without collecting",
            )
            .once_per_change(),
        );
    }
    let cursor = if let Some(progress) = &old {
        if let (Some(trigger), Some(scope)) = (&progress.trigger, &progress.scope) {
            read_scope_cursor(&directory, &scope_key(trigger, scope))?
        } else {
            progress.cursor.clone()
        }
    } else {
        None
    };
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
        cursor,
        scope_cursors_v2: true,
        migration_pending: false,
        cursors: BTreeMap::new(),
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

/// Acquire the sweep lease, waiting up to `wait` for a running sweep to
/// release it. A second explicit collection joins the in-flight pass in this
/// way instead of returning an empty skip while another process holds the
/// lease. The wait is bounded and never holds the metadata mutex while it
/// waits; `Acquire::Busy` means the bound elapsed, not that work was done.
#[allow(
    clippy::disallowed_methods,
    reason = "A bounded join of an in-flight sweep is an explicit real-time boundary; the lease fence observes cancellation."
)]
pub(crate) fn acquire_waiting(
    state_root: &std::path::Path,
    budget: Duration,
    wait: Duration,
    clock: &dyn Clock,
) -> Result<Acquire, Box<dyn std::error::Error>> {
    let started = Instant::now();
    loop {
        match acquire(state_root, budget, clock)? {
            Acquire::Held(lease) => return Ok(Acquire::Held(lease)),
            Acquire::Busy(warning) => {
                if started.elapsed() >= wait {
                    return Ok(Acquire::Busy(warning));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

fn metadata_lock(directory: &Directory) -> Result<LockGuard, provenant_os::fs::FsError> {
    directory.try_lock(OsStr::new(LOCK_FILE), LockMode::Exclusive)
}

// The fence watcher never takes the metadata mutex. Every writer replaces the
// progress file atomically, so a reader sees a whole record, and a watcher that
// held the mutex could stall the sweep's own checkpoint, which waits for it
// only briefly. A read that races a replacement is retried, not a fence.
fn fence_observation(
    directory: &Directory,
    epoch: u64,
) -> Result<Option<bool>, Box<dyn std::error::Error>> {
    match read_progress(directory) {
        Ok(progress) => {
            Ok(Some(progress.is_some_and(|progress| {
                progress.epoch == epoch && !progress.released
            })))
        }
        Err(error)
            if matches!(
                error.downcast_ref::<provenant_os::fs::FsError>(),
                Some(provenant_os::fs::FsError::RaceRefused { .. })
            ) =>
        {
            Ok(None)
        }
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

fn write_legacy_progress(directory: &Directory, progress: &Progress) -> Result<(), io::Error> {
    let bytes = serde_json::to_vec(progress).map_err(io::Error::other)?;
    if bytes.len() as u64 > MIGRATION_PROGRESS_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "legacy GC progress exceeds its size limit",
        ));
    }
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

fn read_progress(directory: &Directory) -> Result<Option<Progress>, Box<dyn std::error::Error>> {
    let bytes = match directory.read_stable(OsStr::new(PROGRESS_FILE), MIGRATION_PROGRESS_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(Box::new(error)),
    };
    match serde_json::from_slice::<Progress>(&bytes) {
        Ok(progress) => {
            let progress: Progress = progress;
            if bytes.len() as u64 > LEGACY_PROGRESS_LIMIT && !progress.migration_pending {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "legacy GC progress exceeds its size limit",
                )
                .into());
            }
            Ok(Some(progress))
        }
        Err(error) => {
            warn(
                Warning::new(
                    CONFIG_MALFORMED,
                    format!("corrupt {PROGRESS_FILE}: {error}; resetting cursor"),
                )
                .subject(PROGRESS_FILE)
                .once_per_change(),
            );
            Ok(None)
        }
    }
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
        Ok(bytes) => match std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
        {
            Some(epoch) => epoch,
            None => {
                warn(
                    Warning::new(
                        CONFIG_MALFORMED,
                        format!("corrupt {EPOCH_FILE}; resetting epoch counter"),
                    )
                    .subject(EPOCH_FILE)
                    .once_per_change(),
                );
                0
            }
        },
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
    #[allow(
        clippy::disallowed_methods,
        reason = "The test releases a held lease on a short real-time boundary."
    )]
    fn explicit_gc_waits_for_a_held_sweep_instead_of_skipping() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let Acquire::Held(lease) = acquire(&root, Duration::from_secs(30), &clock).unwrap() else {
            panic!("first sweep should acquire its lease");
        };
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(75));
            drop(lease);
        });
        let acquired = acquire_waiting(
            &root,
            Duration::from_secs(30),
            Duration::from_secs(2),
            &clock,
        )
        .unwrap();
        release.join().unwrap();
        assert!(
            matches!(acquired, Acquire::Held(_)),
            "a held sweep must be waited for and joined, not skipped"
        );
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn waiting_for_a_held_sweep_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let Acquire::Held(_lease) = acquire(&root, Duration::from_secs(30), &clock).unwrap() else {
            panic!("first sweep should acquire its lease");
        };
        let outcome = acquire_waiting(
            &root,
            Duration::from_secs(30),
            Duration::from_millis(50),
            &clock,
        )
        .unwrap();
        assert!(
            matches!(outcome, Acquire::Busy(_)),
            "an in-flight sweep must report a bounded wait, not an empty skip"
        );
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
        // The watcher reads without the mutex, so a held mutex neither fences
        // the sweep nor is held by the watcher against its own checkpoint.
        let guard = metadata_lock(&lease.directory).unwrap();
        assert_eq!(
            fence_observation(&lease.directory, lease.epoch()).unwrap(),
            Some(true)
        );
        assert_eq!(
            fence_observation(&lease.directory, lease.epoch() + 1).unwrap(),
            Some(false)
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
            scope_cursors_v2: false,
            migration_pending: false,
            cursors: BTreeMap::new(),
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
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let mut clock = test_clock();
        let budget = Duration::from_secs(30);
        let mut old = match acquire(&root, budget, &clock).unwrap() {
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
            acquire(&root, budget, &clock).unwrap(),
            Acquire::Busy(_)
        ));

        clock
            .advance(DurationMicros::from_micros(31_000_000))
            .unwrap();
        let current = match acquire(&root, budget, &clock).unwrap() {
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
        // Fenced owners stop cooperatively without overwriting the new epoch.
        old.checkpoint(Cursor::default()).unwrap();
        assert!(cancel.is_cancelled());
        old.complete().unwrap();
        assert_eq!(current.cursor(), Some(&cursor));
        let saved = read_progress(&current.directory).unwrap().unwrap();
        assert_eq!(saved.epoch, current.epoch());
        assert_eq!(saved.cursor.as_ref(), Some(&cursor));

        drop(current);
        // An early exit releases its lease but keeps the cursor for resumption.
        assert!(matches!(
            acquire(&root, budget, &clock).unwrap(),
            Acquire::Held(_)
        ));
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn dead_owner_is_taken_over_with_a_warning() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let directory = provenant_config::private_directory(&root.join("tick"), true).unwrap();
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
            scope_cursors_v2: false,
            migration_pending: false,
            cursors: BTreeMap::new(),
        };
        next_epoch(&directory, Some(progress.epoch)).unwrap();
        write_progress(&directory, &progress).unwrap();
        let lease = match acquire(&root, Duration::from_secs(30), &clock).unwrap() {
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
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let budget = Duration::from_secs(30);
        let mut lease = match acquire(&root, budget, &clock).unwrap() {
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

        let resumed = match acquire(&root, budget, &clock).unwrap() {
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
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let budget = Duration::from_secs(30);
        let mut lease = match acquire(&root, budget, &clock).unwrap() {
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

        let mut resumed = match acquire(&root, budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("released sweep should resume"),
        };
        assert_eq!(resumed.cursor(), Some(&cursor));
        assert!(resumed.warning.is_none());
        assert!(resumed.matches_identity("full_gc", "scheduled"));
        assert!(!resumed.matches_identity("pressure", "scheduled"));
        assert!(!resumed.matches_identity("full_gc", "manual:repo-a"));

        // A mismatched request starts fresh without destroying the saved cursor.
        resumed.set_identity("pressure", "scheduled").unwrap();
        assert_eq!(resumed.cursor(), None);

        // Switching back or running as full_gc resumes from the saved cursor.
        resumed.set_identity("full_gc", "scheduled").unwrap();
        assert_eq!(resumed.cursor(), Some(&cursor));
    }

    #[test]
    fn legacy_migration_does_not_overwrite_an_existing_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let budget = Duration::from_secs(30);
        let directory = provenant_config::private_directory(&root.join("tick"), true).unwrap();
        let old = Cursor {
            class: "old".into(),
            ..Cursor::default()
        };
        let newer = Cursor {
            class: "newer".into(),
            ..Cursor::default()
        };
        let key = scope_key("full_gc", "first");
        let legacy = Progress {
            pid: 2_000_000_000,
            epoch: 3,
            started_unix_seconds: clock.unix_seconds(),
            started_host_monotonic_micros: clock.host_monotonic_micros().ok(),
            boot_identity: clock.boot_identity().ok(),
            budget_seconds: 30,
            released: false,
            trigger: Some("full_gc".into()),
            scope: Some("first".into()),
            cursor: Some(old),
            scope_cursors_v2: false,
            migration_pending: false,
            cursors: BTreeMap::new(),
        };
        next_epoch(&directory, Some(legacy.epoch)).unwrap();
        write_progress(&directory, &legacy).unwrap();
        write_scope_cursor(&directory, &key, &newer).unwrap();
        let Acquire::Held(mut resumed) = acquire(&root, budget, &clock).unwrap() else {
            panic!("dead sweep is busy")
        };
        resumed.set_identity("full_gc", "first").unwrap();
        assert_eq!(resumed.cursor(), Some(&newer));
    }

    #[test]
    fn markerless_completed_scope_does_not_resurrect_inline_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let budget = Duration::from_secs(30);
        let directory = provenant_config::private_directory(&root.join("tick"), true).unwrap();
        let legacy = Progress {
            pid: 2_000_000_000,
            epoch: 3,
            started_unix_seconds: clock.unix_seconds(),
            started_host_monotonic_micros: clock.host_monotonic_micros().ok(),
            boot_identity: clock.boot_identity().ok(),
            budget_seconds: 30,
            released: false,
            trigger: Some("full_gc".into()),
            scope: Some("first".into()),
            cursor: Some(Cursor {
                class: "old".into(),
                ..Cursor::default()
            }),
            scope_cursors_v2: false,
            migration_pending: false,
            cursors: BTreeMap::new(),
        };
        next_epoch(&directory, Some(legacy.epoch)).unwrap();
        write_progress(&directory, &legacy).unwrap();
        let Acquire::Held(mut resumed) = acquire(&root, budget, &clock).unwrap() else {
            panic!("dead sweep is busy")
        };
        resumed.set_identity("full_gc", "first").unwrap();
        assert_eq!(resumed.cursor(), None);
    }

    #[test]
    fn failed_legacy_migration_preserves_cursor_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let budget = Duration::from_secs(30);
        let directory = provenant_config::private_directory(&root.join("tick"), true).unwrap();
        let cursor = Cursor {
            class: "kept".into(),
            ..Cursor::default()
        };
        let key = scope_key("full_gc", "first");
        let legacy = Progress {
            pid: 2_000_000_000,
            epoch: 3,
            started_unix_seconds: clock.unix_seconds(),
            started_host_monotonic_micros: clock.host_monotonic_micros().ok(),
            boot_identity: clock.boot_identity().ok(),
            budget_seconds: 30,
            released: false,
            trigger: Some("full_gc".into()),
            scope: Some("first".into()),
            cursor: Some(cursor.clone()),
            scope_cursors_v2: false,
            migration_pending: false,
            cursors: [(key.clone(), cursor.clone())].into(),
        };
        next_epoch(&directory, Some(legacy.epoch)).unwrap();
        write_progress(&directory, &legacy).unwrap();
        let blocked = root.join("tick").join(scope_cursor_name(&key));
        std::fs::create_dir(&blocked).unwrap();
        assert!(acquire(&root, budget, &clock).is_err());
        let preserved = read_progress(&directory).unwrap().unwrap();
        assert!(preserved.released);
        assert!(preserved.epoch > legacy.epoch);
        assert_eq!(preserved.cursors.get(&key), Some(&cursor));
        std::fs::remove_dir(blocked).unwrap();
        let Acquire::Held(mut resumed) = acquire(&root, budget, &clock).unwrap() else {
            panic!("released sweep is busy")
        };
        resumed.set_identity("full_gc", "first").unwrap();
        assert_eq!(resumed.cursor(), Some(&cursor));
    }

    #[test]
    fn sidecar_checkpoint_survives_stale_lease_and_failed_scope_switch() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let budget = Duration::from_secs(30);
        let Acquire::Held(mut lease) = acquire(&root, budget, &clock).unwrap() else {
            panic!("new sweep is busy")
        };
        lease.set_identity("full_gc", "first").unwrap();
        let old = Cursor {
            class: "old".into(),
            ..Cursor::default()
        };
        let newer = Cursor {
            class: "newer".into(),
            ..Cursor::default()
        };
        lease.checkpoint(old.clone()).unwrap();
        // Simulate a crash between publishing the sidecar and the lease record.
        write_scope_cursor(&lease.directory, &scope_key("full_gc", "first"), &newer).unwrap();
        lease
            .directory
            .atomic_write(
                &scope_cursor_name(&scope_key("full_gc", "second")),
                ".test-",
                b"invalid",
                PublishMode::Replace,
            )
            .unwrap();
        assert!(lease.set_identity("full_gc", "second").is_err());
        assert_eq!(lease.scope(), Some("first"));
        drop(lease);
        let Acquire::Held(mut resumed) = acquire(&root, budget, &clock).unwrap() else {
            panic!("released sweep is busy")
        };
        resumed.set_identity("full_gc", "first").unwrap();
        assert_eq!(resumed.cursor(), Some(&newer));
    }

    #[test]
    fn many_scope_cursors_migrate_out_of_the_bounded_lease_record() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let budget = Duration::from_secs(30);
        let scope_names = (0..160)
            .map(|index| format!("manual:scope-{index:04}-{}", "s".repeat(64)))
            .collect::<Vec<_>>();
        let cursor_for = |index: usize| Cursor {
            class: format!("CacheSurvey-{index:04}"),
            project: Some(format!("/repo/project-{index:04}-{}", "p".repeat(48)).into()),
            candidate: Some(format!("/repo/target-{index:04}-{}", "c".repeat(48)).into()),
            detail: Some(format!("continuation-{index:04}-{}", "d".repeat(96))),
        };
        let cursors = scope_names
            .iter()
            .enumerate()
            .map(|(index, scope)| (scope_key("full_gc", scope), cursor_for(index)))
            .collect::<BTreeMap<_, _>>();
        let legacy = Progress {
            pid: 2_000_000_000,
            epoch: 3,
            started_unix_seconds: clock.unix_seconds(),
            started_host_monotonic_micros: clock.host_monotonic_micros().ok(),
            boot_identity: clock.boot_identity().ok(),
            budget_seconds: 30,
            released: false,
            trigger: Some("full_gc".into()),
            scope: Some(scope_names[0].clone()),
            cursor: Some(cursor_for(0)),
            scope_cursors_v2: false,
            migration_pending: false,
            cursors,
        };
        let directory = provenant_config::private_directory(&root.join("tick"), true).unwrap();
        next_epoch(&directory, Some(legacy.epoch)).unwrap();
        write_progress(&directory, &legacy).unwrap();
        assert!(
            std::fs::metadata(root.join("tick").join(PROGRESS_FILE))
                .unwrap()
                .len()
                > PROGRESS_LIMIT
        );

        let mut lease = match acquire(&root, budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("dead legacy sweep should be taken over"),
        };
        assert_eq!(lease.cursor(), Some(&cursor_for(0)));
        assert!(
            std::fs::metadata(root.join("tick").join(PROGRESS_FILE))
                .unwrap()
                .len()
                <= PROGRESS_LIMIT
        );
        assert!(
            read_progress(&lease.directory)
                .unwrap()
                .unwrap()
                .cursors
                .is_empty()
        );

        for (index, scope) in scope_names.iter().enumerate() {
            lease.set_identity("full_gc", scope).unwrap();
            assert_eq!(lease.cursor(), Some(&cursor_for(index)), "scope {index}");
        }

        lease.set_identity("full_gc", &scope_names[0]).unwrap();
        lease.complete().unwrap();

        let mut resumed = match acquire(&root, budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("completed scope should release its lease"),
        };
        resumed.set_identity("full_gc", &scope_names[0]).unwrap();
        assert_eq!(resumed.cursor(), None);
        resumed.set_identity("full_gc", &scope_names[1]).unwrap();
        assert_eq!(resumed.cursor(), Some(&cursor_for(1)));
    }

    #[test]
    fn corrupt_progress_or_epoch_file_resets_gracefully() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let clock = test_clock();
        let budget = Duration::from_secs(30);

        // Write corrupt progress and corrupt epoch
        let tick = root.join("tick");
        std::fs::create_dir_all(&tick).unwrap();
        std::fs::write(tick.join(PROGRESS_FILE), b"not valid json").unwrap();
        std::fs::write(tick.join(EPOCH_FILE), b"not valid epoch number").unwrap();

        // acquire should succeed, warn, and reset
        let lease = match acquire(&root, budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("should acquire despite corrupt files"),
        };
        assert_eq!(lease.cursor(), None);
        assert!(lease.epoch() > 0);
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn bounded_fixture_sweep_releases_and_resumes_at_its_last_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let fixture = root.join("candidates");
        std::fs::create_dir(&fixture).unwrap();
        for index in 0..256 {
            std::fs::create_dir(fixture.join(format!("candidate-{index:03}"))).unwrap();
        }

        let mut clock = test_clock();
        let budget = Duration::from_secs(1);
        let mut lease = match acquire(&root, budget, &clock).unwrap() {
            Acquire::Held(lease) => lease,
            Acquire::Busy(_) => panic!("new fixture sweep should acquire its lease"),
        };
        let mut candidates = std::fs::read_dir(&fixture)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        candidates.sort();
        let mut visited = 0;
        for (index, candidate) in candidates.into_iter().enumerate() {
            if holder_over_budget(&lease.progress, &clock) {
                break;
            }
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

        let resumed = match acquire(&root, budget, &clock).unwrap() {
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

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn repeated_bounded_passes_collect_monotonically_and_finish() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let fixture = root.join("attempts");
        std::fs::create_dir(&fixture).unwrap();
        for index in 0..64 {
            std::fs::create_dir(fixture.join(format!("candidate-{index:03}"))).unwrap();
        }
        let mut clock = test_clock();
        let budget = Duration::from_secs(1);
        let mut cursor: Option<Cursor> = None;
        let mut collected = std::collections::BTreeSet::new();
        let mut passes = 0;
        loop {
            passes += 1;
            let Acquire::Held(mut lease) = acquire(&root, budget, &clock).unwrap() else {
                panic!("pass {passes} should acquire the released lease")
            };
            lease.set_identity("full_gc", "monotonic").unwrap();
            let start = cursor.as_ref().and_then(|saved| saved.candidate.clone());
            let mut candidates = std::fs::read_dir(&fixture)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            candidates.sort();
            let mut visited = 0;
            for candidate in candidates
                .into_iter()
                .filter(|candidate| start.as_ref().is_none_or(|start| candidate >= start))
            {
                if holder_over_budget(&lease.progress, &clock) {
                    break;
                }
                lease
                    .checkpoint(Cursor {
                        class: "AttemptScratch".into(),
                        project: Some(fixture.clone()),
                        candidate: Some(candidate.clone()),
                        detail: None,
                    })
                    .unwrap();
                collected.insert(candidate);
                visited += 1;
                clock.advance(DurationMicros::from_micros(20_000)).unwrap();
            }
            assert!(visited > 0, "pass {passes} must decide a candidate");
            cursor = lease.cursor().cloned();
            let finished = cursor.as_ref().and_then(|saved| saved.candidate.as_ref())
                == Some(&fixture.join("candidate-063"));
            drop(lease);
            if finished {
                break;
            }
            assert!(passes < 64, "the bounded sweep never finished");
        }
        assert!(passes > 1, "a tight budget must require more than one pass");
        assert_eq!(
            collected.len(),
            64,
            "every candidate was eventually collected"
        );
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
