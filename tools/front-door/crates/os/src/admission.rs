//! Host-wide named admission classes: cooperative fair sharing of CPU and memory among
//! the commands and lanes that share one state directory. This is an advisory occupancy signal,
//! not a security boundary and not a hard cap.
//!
//! Each slot is a file the host pre-creates with `provision`. A lane is granted read access to the
//! directory and its files only, so it can neither create, unlink nor replace a slot file. The
//! command or shim opens a slot file read-only and takes `flock` on it; flock works on a
//! read-only descriptor. The descriptor is close-on-exec, so only the shim/holder
//! holds the slot and a daemon or orphaned grandchild never keeps it. The
//! kernel drops the lock when the process exits, even after SIGKILL.
use crate::fs::{Directory, FsError, LockGuard, LockMode, Result};
use crate::sys::fs as os;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdmissionError {
    #[error(
        "admission slot directory {path} is owned by uid {actual}, expected uid {expected}",
        path = .path.display()
    )]
    DirectoryOwner {
        path: PathBuf,
        actual: u32,
        expected: u32,
    },
    #[error("admission class name is invalid: {0}")]
    InvalidClassName(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Fs(#[from] FsError),
}

/// Slot directory below the host state root.
pub const DIRECTORY: [&str; 1] = ["admission"];
pub const SHIM_DIRECTORY: &str = "cargo-bin";

pub fn validate_class_name(class: &str) -> std::result::Result<(), AdmissionError> {
    if class.is_empty()
        || class.len() > 64
        || !class
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AdmissionError::InvalidClassName(class.to_string()));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WaitInfo {
    pub class: String,
    pub pid: i64,
    pub start_unix_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// The masked, bounded argv of the command waiting for the slot. The
    /// admission shim records it before publication, so raw arguments never
    /// reach a reader or the state directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeldInfo {
    pub class: String,
    pub slot: u64,
    pub pid: i64,
    pub start_unix_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// The masked, bounded argv of the command holding the slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

fn now_unix_seconds() -> u64 {
    use provenant_core::Clock;
    crate::clock::SystemClock::new()
        .wall_now()
        .map_or(0, |w| w.unix_seconds().max(0) as u64)
}

pub struct WaitGuard {
    _guard: HostWaitGuard,
}
pub struct HeldGuard {
    _guard: HostHeldGuard,
}

pub fn mark_waiting(
    scratch: &Path,
    class: &str,
    pid: i64,
    attempt_id: Option<&str>,
) -> Result<WaitGuard> {
    mark_waiting_with_command(scratch, class, pid, attempt_id, None)
}

/// Record a waiter together with the masked command it is waiting to run.
pub fn mark_waiting_with_command(
    scratch: &Path,
    class: &str,
    pid: i64,
    attempt_id: Option<&str>,
    command: Option<String>,
) -> Result<WaitGuard> {
    let info = WaitInfo {
        class: class.to_string(),
        pid,
        start_unix_seconds: now_unix_seconds(),
        attempt_id: attempt_id.map(str::to_string),
        command,
    };
    let (lock, json_path, lock_path) =
        mark_record(&scratch.join(SHIM_DIRECTORY).join("waiters"), pid, &info)?;
    Ok(WaitGuard {
        _guard: HostWaitGuard {
            _lock: lock,
            json_path,
            lock_path,
        },
    })
}

pub fn mark_held(
    scratch: &Path,
    class: &str,
    slot: u64,
    pid: i64,
    attempt_id: Option<&str>,
) -> Result<HeldGuard> {
    mark_held_with_command(scratch, class, slot, pid, attempt_id, None)
}

/// Record a holder together with the masked command it is running.
pub fn mark_held_with_command(
    scratch: &Path,
    class: &str,
    slot: u64,
    pid: i64,
    attempt_id: Option<&str>,
    command: Option<String>,
) -> Result<HeldGuard> {
    let info = HeldInfo {
        class: class.to_string(),
        slot,
        pid,
        start_unix_seconds: now_unix_seconds(),
        attempt_id: attempt_id.map(str::to_string),
        command,
    };
    let (lock, json_path, lock_path) =
        mark_record(&scratch.join(SHIM_DIRECTORY).join("holders"), pid, &info)?;
    Ok(HeldGuard {
        _guard: HostHeldGuard {
            _lock: lock,
            json_path,
            lock_path,
        },
    })
}

pub fn lane_waiters(scratch: &Path) -> Vec<WaitInfo> {
    live_records(&scratch.join(SHIM_DIRECTORY).join("waiters"))
}

pub fn lane_holders(scratch: &Path) -> Vec<HeldInfo> {
    live_records(&scratch.join(SHIM_DIRECTORY).join("holders"))
}

fn mark_record<T: Serialize>(
    dir_path: &Path,
    pid: i64,
    info: &T,
) -> Result<(LockGuard, PathBuf, PathBuf)> {
    use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
    let clock = crate::clock::SystemClock::new();
    let _ = std::fs::create_dir_all(dir_path);
    let json_path = dir_path.join(format!("{pid}.json"));
    let lock_file = format!("{pid}.lock");
    let lock_path = dir_path.join(&lock_file);
    if let Ok(bytes) = serde_json::to_vec(info) {
        let _ = std::fs::write(&json_path, bytes);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&lock_path);
    let anchor = Directory::open_anchor(dir_path)?;
    let lock = anchor.lock_existing(
        std::ffi::OsStr::new(&lock_file),
        LockMode::Shared,
        WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(1_000_000)),
        &CancelToken::new(),
        &clock,
    )?;
    Ok((lock, json_path, lock_path))
}

pub struct HostWaitGuard {
    _lock: LockGuard,
    json_path: PathBuf,
    lock_path: PathBuf,
}

impl Drop for HostWaitGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.json_path);
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

pub struct HostHeldGuard {
    _lock: LockGuard,
    json_path: PathBuf,
    lock_path: PathBuf,
}

impl Drop for HostHeldGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.json_path);
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

pub fn mark_host_waiting(state: &Path, class: &str, pid: i64) -> Result<HostWaitGuard> {
    mark_host_waiting_with_command(state, class, pid, None)
}

/// Record a host-wide waiter together with its masked command.
pub fn mark_host_waiting_with_command(
    state: &Path,
    class: &str,
    pid: i64,
    command: Option<String>,
) -> Result<HostWaitGuard> {
    use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
    let clock = crate::clock::SystemClock::new();
    let dir_path = class_directory(state, class).join("waiters");
    let _ = std::fs::create_dir_all(&dir_path);
    let info = WaitInfo {
        class: class.to_string(),
        pid,
        start_unix_seconds: now_unix_seconds(),
        attempt_id: None,
        command,
    };
    let json_file = format!("{pid}.json");
    let lock_file = format!("{pid}.lock");
    let json_path = dir_path.join(&json_file);
    let lock_path = dir_path.join(&lock_file);
    if let Ok(bytes) = serde_json::to_vec(&info) {
        let _ = std::fs::write(&json_path, bytes);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&lock_path);
    let anchor = Directory::open_anchor(&dir_path)?;
    let lock = anchor.lock_existing(
        std::ffi::OsStr::new(&lock_file),
        LockMode::Shared,
        WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(1_000_000)),
        &CancelToken::new(),
        &clock,
    )?;
    Ok(HostWaitGuard {
        _lock: lock,
        json_path,
        lock_path,
    })
}

pub fn mark_host_held(state: &Path, class: &str, slot: u64, pid: i64) -> Result<HostHeldGuard> {
    mark_host_held_with_command(state, class, slot, pid, None)
}

/// Record a host-wide holder together with its masked command.
pub fn mark_host_held_with_command(
    state: &Path,
    class: &str,
    slot: u64,
    pid: i64,
    command: Option<String>,
) -> Result<HostHeldGuard> {
    use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
    let clock = crate::clock::SystemClock::new();
    let dir_path = class_directory(state, class).join("holders");
    let _ = std::fs::create_dir_all(&dir_path);
    let info = HeldInfo {
        class: class.to_string(),
        slot,
        pid,
        start_unix_seconds: now_unix_seconds(),
        attempt_id: None,
        command,
    };
    let json_file = format!("{pid}.json");
    let lock_file = format!("{pid}.lock");
    let json_path = dir_path.join(&json_file);
    let lock_path = dir_path.join(&lock_file);
    if let Ok(bytes) = serde_json::to_vec(&info) {
        let _ = std::fs::write(&json_path, bytes);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&lock_path);
    let anchor = Directory::open_anchor(&dir_path)?;
    let lock = anchor.lock_existing(
        std::ffi::OsStr::new(&lock_file),
        LockMode::Shared,
        WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(1_000_000)),
        &CancelToken::new(),
        &clock,
    )?;
    Ok(HostHeldGuard {
        _lock: lock,
        json_path,
        lock_path,
    })
}

pub fn host_holders(state: &Path, class: &str) -> Vec<HeldInfo> {
    live_records(&class_directory(state, class).join("holders"))
}

pub fn host_waiters(state: &Path, class: &str) -> Vec<WaitInfo> {
    live_records(&class_directory(state, class).join("waiters"))
}

fn live_records<T: serde::de::DeserializeOwned>(dir_path: &Path) -> Vec<T> {
    let Ok(dir) = Directory::open_anchor(dir_path) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("lock") {
                continue;
            }
            let Some(file_name) = path.file_name() else {
                continue;
            };
            let is_locked = dir
                .try_lock_existing(file_name, LockMode::Exclusive)
                .is_err_and(|error| matches!(error, FsError::LockTimeout { .. }));
            let json_path = path.with_extension("json");
            if is_locked
                && let Some(info) = std::fs::read(&json_path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            {
                records.push(info);
            }
        }
    }
    records
}

pub fn active_classes(state: &Path) -> Vec<String> {
    let dir = class_directory(state, "");
    let mut classes = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|ft| ft.is_dir())
                && let Ok(name) = entry.file_name().into_string()
                && !name.starts_with('.')
                && validate_class_name(&name).is_ok()
            {
                classes.push(name);
            }
        }
    }
    classes.sort();
    classes
}

pub fn standalone_holders(state: &Path) -> Vec<HeldInfo> {
    let mut all = Vec::new();
    for class in active_classes(state) {
        for holder in host_holders(state, &class) {
            if holder.attempt_id.is_none() {
                all.push(holder);
            }
        }
    }
    all
}

pub fn standalone_waiters(state: &Path) -> Vec<WaitInfo> {
    let mut all = Vec::new();
    for class in active_classes(state) {
        for waiter in host_waiters(state, &class) {
            if waiter.attempt_id.is_none() {
                all.push(waiter);
            }
        }
    }
    all
}

pub fn class_directory(state: &Path, class: &str) -> PathBuf {
    DIRECTORY
        .iter()
        .fold(state.to_owned(), |p, n| p.join(n))
        .join(class)
}

fn slot_name(index: u64) -> OsString {
    format!("slot-{index}.lock").into()
}

/// An open admission slot directory, read-only for the lane.
pub struct AdmissionSlots {
    dir: Directory,
    class: String,
}

impl AdmissionSlots {
    /// Host side: create the slot directory (mode 0700) and `slot-0..limit`
    /// lock files (mode 0600), repairing what is there.
    pub fn provision(
        state: &Path,
        class: &str,
        limit: u64,
    ) -> std::result::Result<(), AdmissionError> {
        validate_class_name(class)?;
        Self::provision_owned(state, class, limit, crate::fs::effective_uid())
    }

    /// `provision`, refusing a directory owned by anyone but `owner`.
    fn provision_owned(
        state: &Path,
        class: &str,
        limit: u64,
        owner: u32,
    ) -> std::result::Result<(), AdmissionError> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
        let path = class_directory(state, class);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_dir() => {
                return Err(AdmissionError::Io(std::io::Error::other(format!(
                    "{} is not a directory",
                    path.display()
                ))));
            }
            Ok(meta) if meta.uid() != owner => {
                return Err(AdmissionError::DirectoryOwner {
                    path: path.clone(),
                    actual: meta.uid(),
                    expected: owner,
                });
            }
            Ok(meta) => {
                if meta.permissions().mode() & 0o7777 != 0o700 {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
                }
            }
            Err(_) => std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&path)?,
        }
        for index in 0..limit {
            let file = path.join(slot_name(index));
            match std::fs::symlink_metadata(&file) {
                Ok(meta) if meta.is_file() && meta.nlink() == 1 && meta.uid() == owner => {
                    if meta.permissions().mode() & 0o7777 != 0o600 {
                        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
                    }
                    continue;
                }
                Ok(meta) if meta.is_dir() => std::fs::remove_dir(&file)?,
                Ok(_) => std::fs::remove_file(&file)?,
                Err(_) => {}
            }
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&file)?;
        }
        Ok(())
    }

    /// Open the slot directory without creating anything.
    pub fn open(state: &Path, class: &str) -> std::result::Result<Self, AdmissionError> {
        validate_class_name(class)?;
        Ok(Self {
            dir: Directory::open_anchor(&class_directory(state, class))?,
            class: class.to_string(),
        })
    }

    pub fn class(&self) -> &str {
        &self.class
    }

    /// Take the first free slot below `limit`, or none when all are held.
    pub fn try_acquire(&self, limit: u64) -> Result<Option<SlotGuard>> {
        let mut missing = None;
        let mut absent = 0;
        for index in 0..limit {
            match self
                .dir
                .try_lock_existing(&slot_name(index), LockMode::Exclusive)
            {
                Ok(guard) => {
                    return Ok(Some(SlotGuard {
                        guard,
                        index,
                        class: self.class.clone(),
                    }));
                }
                Err(FsError::LockTimeout { .. }) => {}
                Err(error) if error.is_not_found() => {
                    absent += 1;
                    missing = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        match missing {
            Some(error) if absent == limit => Err(error),
            _ => Ok(None),
        }
    }

    /// Poll until a slot is free or `timeout` elapses. `Ok(None)` means every
    /// slot stayed busy for the whole wait.
    #[allow(clippy::disallowed_methods)]
    pub fn acquire_within(
        &self,
        limit: u64,
        timeout: std::time::Duration,
    ) -> Result<Option<SlotGuard>> {
        let start = std::time::Instant::now();
        loop {
            if let Some(guard) = self.try_acquire(limit)? {
                return Ok(Some(guard));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            let slice =
                std::time::Duration::from_millis(25).min(timeout.saturating_sub(start.elapsed()));
            std::thread::sleep(slice);
        }
    }

    /// How many of the first `limit` slots are held now.
    pub fn held(&self, limit: u64) -> u64 {
        (0..limit)
            .filter(|index| {
                matches!(
                    self.dir
                        .try_lock_existing(&slot_name(*index), LockMode::Shared),
                    Err(FsError::LockTimeout { .. })
                )
            })
            .count() as u64
    }
}

/// One held advisory slot. The kernel releases it when the process exits.
#[derive(Debug)]
pub struct SlotGuard {
    #[allow(dead_code, reason = "the open descriptor is the held lock")]
    guard: LockGuard,
    index: u64,
    class: String,
}

impl SlotGuard {
    pub fn index(&self) -> u64 {
        self.index
    }

    pub fn class(&self) -> &str {
        &self.class
    }

    pub fn close_on_exec(&self) -> bool {
        os::is_cloexec(self.guard.handoff_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn state() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn provisioned(class: &str, limit: u64) -> (tempfile::TempDir, AdmissionSlots) {
        let state = state();
        AdmissionSlots::provision(state.path(), class, limit).unwrap();
        let slots = AdmissionSlots::open(state.path(), class).unwrap();
        (state, slots)
    }

    #[test]
    fn slots_are_bounded_and_reusable_per_class() {
        let _serial = serial();
        let (_state, slots) = provisioned("build", 2);
        let first = slots.try_acquire(2).unwrap().unwrap();
        let second = slots.try_acquire(2).unwrap().unwrap();
        assert_ne!(first.index(), second.index());
        assert_eq!(first.class(), "build");
        assert!(slots.try_acquire(2).unwrap().is_none());
        assert_eq!(slots.held(2), 2);
        drop(first);
        assert_eq!(slots.held(2), 1);
        assert!(slots.try_acquire(2).unwrap().is_some());
    }

    #[test]
    fn custom_named_classes_are_independent() {
        let _serial = serial();
        let state = state();
        AdmissionSlots::provision(state.path(), "build", 1).unwrap();
        AdmissionSlots::provision(state.path(), "heavy", 1).unwrap();
        let build = AdmissionSlots::open(state.path(), "build").unwrap();
        let heavy = AdmissionSlots::open(state.path(), "heavy").unwrap();
        let b1 = build.try_acquire(1).unwrap().unwrap();
        assert!(build.try_acquire(1).unwrap().is_none());
        // Heavy slot is still free!
        let h1 = heavy.try_acquire(1).unwrap().unwrap();
        assert_eq!(h1.class(), "heavy");
        drop(b1);
        drop(h1);
    }

    #[test]
    fn wait_and_hold_metadata_is_tracked() {
        let _serial = serial();
        let temp = tempfile::tempdir().unwrap();
        let scratch = temp.path().join("scratch");
        std::fs::create_dir_all(scratch.join(SHIM_DIRECTORY)).unwrap();

        assert!(lane_waiters(&scratch).is_empty());
        assert!(lane_holders(&scratch).is_empty());

        let wait_guard = mark_waiting(&scratch, "build", 42, Some("attempt-1")).unwrap();
        let second_wait = mark_waiting(&scratch, "build", 43, Some("attempt-1")).unwrap();
        assert_eq!(lane_waiters(&scratch).len(), 2);
        drop(second_wait);
        let proceeding = mark_held(&scratch, "build", 0, 43, Some("attempt-1")).unwrap();
        assert_eq!(lane_waiters(&scratch).len(), 1);
        assert_eq!(lane_holders(&scratch)[0].pid, 43);
        drop(proceeding);
        let waiting = lane_waiters(&scratch).remove(0);
        assert_eq!(waiting.class, "build");
        assert_eq!(waiting.pid, 42);
        assert_eq!(waiting.attempt_id.as_deref(), Some("attempt-1"));
        drop(wait_guard);
        assert!(lane_waiters(&scratch).is_empty());

        let held_guard = mark_held(&scratch, "build", 0, 42, Some("attempt-1")).unwrap();
        let holding = lane_holders(&scratch).remove(0);
        assert_eq!(holding.class, "build");
        assert_eq!(holding.slot, 0);
        assert_eq!(holding.pid, 42);
        drop(held_guard);
        assert!(lane_holders(&scratch).is_empty());
    }

    #[test]
    fn wait_and_hold_records_keep_the_child_command() {
        let _serial = serial();
        let temp = tempfile::tempdir().unwrap();
        let scratch = temp.path().join("scratch");
        std::fs::create_dir_all(scratch.join(SHIM_DIRECTORY)).unwrap();

        let waiting = mark_waiting_with_command(
            &scratch,
            "heavy",
            42,
            Some("attempt-1"),
            Some("cargo test --locked".into()),
        )
        .unwrap();
        assert_eq!(
            lane_waiters(&scratch)[0].command.as_deref(),
            Some("cargo test --locked")
        );
        drop(waiting);

        let holding = mark_held_with_command(
            &scratch,
            "heavy",
            0,
            42,
            Some("attempt-1"),
            Some("cargo build --release".into()),
        )
        .unwrap();
        assert_eq!(
            lane_holders(&scratch)[0].command.as_deref(),
            Some("cargo build --release")
        );
        drop(holding);
    }

    #[test]
    fn invalid_class_names_are_refused() {
        let _serial = serial();
        let state = state();
        assert!(AdmissionSlots::provision(state.path(), "", 1).is_err());
        assert!(AdmissionSlots::provision(state.path(), "bad/name", 1).is_err());
        assert!(AdmissionSlots::provision(state.path(), "bad name", 1).is_err());
        assert!(AdmissionSlots::provision(state.path(), "valid_class-1", 1).is_ok());
    }
}
