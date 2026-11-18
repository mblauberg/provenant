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
/// Per-lane advisory signal held shared while an operation waits for admission.
pub const WAIT_FILE: &str = "admission-wait.lock";
pub const WAIT_INFO_FILE: &str = "admission-wait.json";
pub const HELD_FILE: &str = "admission-held.lock";
pub const HELD_INFO_FILE: &str = "admission-held.json";
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
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeldInfo {
    pub class: String,
    pub slot: u64,
    pub pid: i64,
    pub start_unix_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
}

fn now_unix_seconds() -> u64 {
    use provenant_core::Clock;
    crate::clock::SystemClock::new()
        .wall_now()
        .map_or(0, |w| w.unix_seconds().max(0) as u64)
}

pub struct WaitGuard {
    _lock: LockGuard,
    path: PathBuf,
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub struct HeldGuard {
    _lock: LockGuard,
    path: PathBuf,
}

impl Drop for HeldGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn mark_waiting(
    scratch: &Path,
    class: &str,
    pid: i64,
    attempt_id: Option<&str>,
) -> Result<WaitGuard> {
    use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
    let clock = crate::clock::SystemClock::new();
    let dir = scratch.join(SHIM_DIRECTORY);
    let _ = std::fs::create_dir_all(&dir);
    let info = WaitInfo {
        class: class.to_string(),
        pid,
        start_unix_seconds: now_unix_seconds(),
        attempt_id: attempt_id.map(str::to_string),
    };
    let info_path = dir.join(WAIT_INFO_FILE);
    if let Ok(bytes) = serde_json::to_vec(&info) {
        let _ = std::fs::write(&info_path, bytes);
    }
    let wait_file_path = dir.join(WAIT_FILE);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&wait_file_path);
    let anchor = Directory::open_anchor(&dir)?;
    let lock = anchor.lock_existing(
        std::ffi::OsStr::new(WAIT_FILE),
        LockMode::Shared,
        WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(1_000_000)),
        &CancelToken::new(),
        &clock,
    )?;
    Ok(WaitGuard {
        _lock: lock,
        path: info_path,
    })
}

pub fn is_waiting(scratch: &Path) -> Option<WaitInfo> {
    let dir_path = scratch.join(SHIM_DIRECTORY);
    let dir = Directory::open_anchor(&dir_path).ok()?;
    let is_locked = dir
        .try_lock_existing(std::ffi::OsStr::new(WAIT_FILE), LockMode::Exclusive)
        .is_err_and(|error| matches!(error, FsError::LockTimeout { .. }));
    if !is_locked {
        return None;
    }
    let bytes = std::fs::read(dir_path.join(WAIT_INFO_FILE)).ok();
    let info = bytes
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| WaitInfo {
            class: "build".to_string(),
            pid: 0,
            start_unix_seconds: 0,
            attempt_id: None,
        });
    Some(info)
}

pub fn mark_held(
    scratch: &Path,
    class: &str,
    slot: u64,
    pid: i64,
    attempt_id: Option<&str>,
) -> Result<HeldGuard> {
    use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
    let clock = crate::clock::SystemClock::new();
    let dir = scratch.join(SHIM_DIRECTORY);
    let _ = std::fs::create_dir_all(&dir);
    let info = HeldInfo {
        class: class.to_string(),
        slot,
        pid,
        start_unix_seconds: now_unix_seconds(),
        attempt_id: attempt_id.map(str::to_string),
    };
    let info_path = dir.join(HELD_INFO_FILE);
    if let Ok(bytes) = serde_json::to_vec(&info) {
        let _ = std::fs::write(&info_path, bytes);
    }
    let anchor = Directory::open_anchor(&dir)?;
    let lock = anchor.lock_existing(
        std::ffi::OsStr::new(HELD_FILE),
        LockMode::Shared,
        WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(1_000_000)),
        &CancelToken::new(),
        &clock,
    )?;
    Ok(HeldGuard {
        _lock: lock,
        path: info_path,
    })
}

pub fn is_holding(scratch: &Path) -> Option<HeldInfo> {
    let dir_path = scratch.join(SHIM_DIRECTORY);
    let dir = Directory::open_anchor(&dir_path).ok()?;
    let is_locked = dir
        .try_lock_existing(std::ffi::OsStr::new(HELD_FILE), LockMode::Exclusive)
        .is_err_and(|error| matches!(error, FsError::LockTimeout { .. }));
    if !is_locked {
        return None;
    }
    let bytes = std::fs::read(dir_path.join(HELD_INFO_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
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
    use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
    let clock = crate::clock::SystemClock::new();
    let dir_path = class_directory(state, class).join("waiters");
    let _ = std::fs::create_dir_all(&dir_path);
    let info = WaitInfo {
        class: class.to_string(),
        pid,
        start_unix_seconds: now_unix_seconds(),
        attempt_id: None,
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
    let dir_path = class_directory(state, class).join("holders");
    let Ok(dir) = Directory::open_anchor(&dir_path) else {
        return Vec::new();
    };
    let mut holders = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("lock") {
                let Some(file_name) = path.file_name() else {
                    continue;
                };
                let is_locked = dir
                    .try_lock_existing(file_name, LockMode::Exclusive)
                    .is_err_and(|error| matches!(error, FsError::LockTimeout { .. }));
                let json_path = path.with_extension("json");
                if is_locked {
                    if let Some(info) = std::fs::read(&json_path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<HeldInfo>(&bytes).ok())
                    {
                        holders.push(info);
                    }
                } else {
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(&json_path);
                }
            }
        }
    }
    holders
}

pub fn host_waiters(state: &Path, class: &str) -> Vec<WaitInfo> {
    let dir_path = class_directory(state, class).join("waiters");
    let Ok(dir) = Directory::open_anchor(&dir_path) else {
        return Vec::new();
    };
    let mut waiters = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("lock") {
                let Some(file_name) = path.file_name() else {
                    continue;
                };
                let is_locked = dir
                    .try_lock_existing(file_name, LockMode::Exclusive)
                    .is_err_and(|error| matches!(error, FsError::LockTimeout { .. }));
                let json_path = path.with_extension("json");
                if is_locked {
                    if let Some(info) = std::fs::read(&json_path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<WaitInfo>(&bytes).ok())
                    {
                        waiters.push(info);
                    }
                } else {
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(&json_path);
                }
            }
        }
    }
    waiters
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
        std::fs::write(scratch.join(SHIM_DIRECTORY).join(WAIT_FILE), b"").unwrap();
        std::fs::write(scratch.join(SHIM_DIRECTORY).join(HELD_FILE), b"").unwrap();

        assert!(is_waiting(&scratch).is_none());
        assert!(is_holding(&scratch).is_none());

        let wait_guard = mark_waiting(&scratch, "build", 42, Some("attempt-1")).unwrap();
        let waiting = is_waiting(&scratch).unwrap();
        assert_eq!(waiting.class, "build");
        assert_eq!(waiting.pid, 42);
        assert_eq!(waiting.attempt_id.as_deref(), Some("attempt-1"));
        drop(wait_guard);
        assert!(is_waiting(&scratch).is_none());

        let held_guard = mark_held(&scratch, "build", 0, 42, Some("attempt-1")).unwrap();
        let holding = is_holding(&scratch).unwrap();
        assert_eq!(holding.class, "build");
        assert_eq!(holding.slot, 0);
        assert_eq!(holding.pid, 42);
        drop(held_guard);
        assert!(is_holding(&scratch).is_none());
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
