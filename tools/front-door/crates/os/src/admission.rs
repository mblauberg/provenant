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
use crate::fs::{Directory, FsError, LockGuard, LockMode, PublishMode, Result};
use crate::sys::fs as os;
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
const CI_DOMAIN_LOCK: &str = ".ci-domain.lock";
const CI_CAP_MAX_BYTES: u64 = 4096;
static CI_HOLDER_ID: AtomicU64 = AtomicU64::new(0);

/// Requested active-holder cap and its resolved configuration provenance.
/// A zero limit is an explicit uncapped override for the shared `ci` class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionCap {
    pub limit: u64,
    pub source: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CiCapMarker {
    schema: u32,
    id: String,
    cap: AdmissionCap,
}

#[derive(Deserialize)]
struct CiLeaseCapMarker {
    lease: CiLeaseCapRecord,
}

#[derive(Deserialize)]
struct CiLeaseCapRecord {
    #[serde(default = "legacy_ci_cap")]
    cap: AdmissionCap,
}

fn legacy_ci_cap() -> AdmissionCap {
    AdmissionCap {
        limit: 1,
        source: "legacy durable ci lease".into(),
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<LaneIdentity>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<LaneIdentity>,
    /// The masked, bounded argv of the command holding the slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// Host-owned attribution for a lane sharing admission slots with other projects.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaneIdentity {
    pub project: PathBuf,
    pub name: String,
}

#[derive(Serialize, Deserialize)]
struct LaneRegistration {
    attempt_id: provenant_core::AttemptId,
    scratch: PathBuf,
    lane: LaneIdentity,
}

/// Publish a host-wide index of a lane's lock-backed markers before confinement.
/// The lane still writes only its scratch grant; no shared registry write grant
/// is needed. Marker locks, rather than the registration's age, prove liveness.
pub fn register_lane(
    state: &Path,
    class: &str,
    scratch: &Path,
    attempt_id: provenant_core::AttemptId,
    lane: LaneIdentity,
) -> std::result::Result<(), AdmissionError> {
    validate_class_name(class)?;
    let path = class_directory(state, class).join("lanes");
    std::fs::create_dir_all(&path)?;
    let record = LaneRegistration {
        attempt_id,
        scratch: scratch.to_path_buf(),
        lane,
    };
    let bytes = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
    let dir = Directory::open_anchor(&path)?;
    dir.atomic_write(
        std::ffi::OsStr::new(&format!("{attempt_id}.json")),
        ".lane-",
        &bytes,
        crate::fs::PublishMode::Replace,
    )?;
    Ok(())
}

fn registered_lanes(state: &Path, class: &str) -> Vec<LaneRegistration> {
    let Ok(entries) = std::fs::read_dir(class_directory(state, class).join("lanes")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                return None;
            }
            std::fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        })
        .collect()
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
        start_unix_seconds: crate::clock::now_unix_seconds(),
        attempt_id: attempt_id.map(str::to_string),
        lane: None,
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
        start_unix_seconds: crate::clock::now_unix_seconds(),
        attempt_id: attempt_id.map(str::to_string),
        lane: None,
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
        start_unix_seconds: crate::clock::now_unix_seconds(),
        attempt_id: None,
        lane: None,
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
        start_unix_seconds: crate::clock::now_unix_seconds(),
        attempt_id: None,
        lane: None,
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
    let mut holders: Vec<HeldInfo> = live_records(&class_directory(state, class).join("holders"));
    for registration in registered_lanes(state, class) {
        holders.extend(
            lane_holders(&registration.scratch)
                .into_iter()
                .filter(|info| info.class == class)
                .map(|mut info| {
                    info.attempt_id = Some(registration.attempt_id.to_string());
                    info.lane = Some(registration.lane.clone());
                    info
                }),
        );
    }
    holders
}

pub fn host_waiters(state: &Path, class: &str) -> Vec<WaitInfo> {
    let mut waiters: Vec<WaitInfo> = live_records(&class_directory(state, class).join("waiters"));
    for registration in registered_lanes(state, class) {
        waiters.extend(
            lane_waiters(&registration.scratch)
                .into_iter()
                .filter(|info| info.class == class)
                .map(|mut info| {
                    info.attempt_id = Some(registration.attempt_id.to_string());
                    info.lane = Some(registration.lane.clone());
                    info
                }),
        );
    }
    waiters
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

pub fn all_holders(state: &Path) -> Vec<HeldInfo> {
    active_classes(state)
        .iter()
        .flat_map(|class| host_holders(state, class))
        .collect()
}

pub fn all_waiters(state: &Path) -> Vec<WaitInfo> {
    active_classes(state)
        .iter()
        .flat_map(|class| host_waiters(state, class))
        .collect()
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

fn durable_lease_name(index: u64) -> OsString {
    format!("slot-{index}.lease.json").into()
}

fn ci_holder_marker(index: u64) -> OsString {
    format!("slot-{index}.holder.json").into()
}

fn ci_slot_indices(dir: &Directory) -> Result<Vec<u64>> {
    let mut indices = Vec::new();
    for entry in dir.entries()? {
        let name = entry?;
        let Some(index) = name
            .to_str()
            .and_then(|name| name.strip_prefix("slot-"))
            .and_then(|name| name.strip_suffix(".lock"))
            .and_then(|index| index.parse::<u64>().ok())
        else {
            continue;
        };
        indices.push(index);
    }
    indices.sort_unstable();
    indices.dedup();
    Ok(indices)
}

fn read_ci_lease_cap(dir: &Directory, index: u64) -> Result<Option<AdmissionCap>> {
    let name = durable_lease_name(index);
    let bytes = match dir.read_stable(&name, CI_CAP_MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error),
    };
    let record: CiLeaseCapMarker =
        serde_json::from_slice(&bytes).map_err(|source| FsError::Io {
            operation: "decode durable ci admission cap",
            path: PathBuf::from(&name),
            source: std::io::Error::other(source),
        })?;
    Ok(Some(record.lease.cap))
}

fn read_ci_holder_cap(dir: &Directory, index: u64) -> Result<Option<CiCapMarker>> {
    let name = ci_holder_marker(index);
    let bytes = match dir.read_stable(&name, CI_CAP_MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error),
    };
    let record: CiCapMarker = serde_json::from_slice(&bytes).map_err(|source| FsError::Io {
        operation: "decode active ci admission cap",
        path: PathBuf::from(&name),
        source: std::io::Error::other(source),
    })?;
    if record.schema != 1 {
        return Err(FsError::Io {
            operation: "validate active ci admission cap",
            path: PathBuf::from(&name),
            source: std::io::Error::other("unsupported ci admission cap schema"),
        });
    }
    Ok(Some(record))
}

fn ci_holder_id(index: u64) -> String {
    let sequence = CI_HOLDER_ID.fetch_add(1, Ordering::Relaxed);
    format!("{}-{index}-{sequence}", std::process::id())
}

fn bounded_cap_source(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect()
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
        let slot_count = if class == "ci" { limit.max(1) } else { limit };
        for index in 0..slot_count {
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
        if class == "ci" {
            let file = path.join(CI_DOMAIN_LOCK);
            match std::fs::symlink_metadata(&file) {
                Ok(meta) if meta.is_file() && meta.nlink() == 1 && meta.uid() == owner => {
                    if meta.permissions().mode() & 0o7777 != 0o600 {
                        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
                    }
                }
                Ok(meta) if meta.is_dir() => std::fs::remove_dir(&file)?,
                Ok(_) => std::fs::remove_file(&file)?,
                Err(_) => {}
            }
            if !file.exists() {
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&file)?;
            }
        }
        Ok(())
    }

    /// Open the slot directory without creating anything.
    pub fn open(state: &Path, class: &str) -> std::result::Result<Self, AdmissionError> {
        validate_class_name(class)?;
        let path = class_directory(state, class);
        if class == "ci" && path.is_dir() && !path.join(CI_DOMAIN_LOCK).exists() {
            Self::provision(state, class, 1)?;
        }
        Ok(Self {
            dir: Directory::open_anchor(&path)?,
            class: class.to_string(),
        })
    }

    pub fn class(&self) -> &str {
        &self.class
    }

    /// Take the first free slot below `limit`, or none when all are held.
    pub fn try_acquire(&self, limit: u64) -> Result<Option<SlotGuard>> {
        if self.class == "ci" {
            self.try_acquire_ci(limit, "AdmissionSlots::try_acquire")
        } else {
            self.try_acquire_indices(0..limit)
        }
    }

    /// Try the supplied indices in order.
    pub fn try_acquire_ordered(&self, order: &[u64]) -> Result<Option<SlotGuard>> {
        if self.class == "ci" {
            self.try_acquire_ci_ordered(
                order,
                u64::try_from(order.len()).unwrap_or(u64::MAX),
                "AdmissionSlots::try_acquire_ordered",
            )
        } else {
            self.try_acquire_indices(order.iter().copied())
        }
    }

    /// Try to acquire a `ci` slot using the shared effective cap. Positive
    /// requests are constrained by the smallest active/request cap; zero is an
    /// explicit uncapped override. Every returned guard is visible to direct
    /// `AdmissionSlots` users and durable queue leases in the same domain.
    pub fn try_acquire_ci(&self, limit: u64, source: &str) -> Result<Option<SlotGuard>> {
        self.try_acquire_ci_with_order(limit, source, None)
    }

    /// Try supplied slot indices first while applying the shared class cap.
    pub fn try_acquire_ci_ordered(
        &self,
        order: &[u64],
        limit: u64,
        source: &str,
    ) -> Result<Option<SlotGuard>> {
        self.try_acquire_ci_with_order(limit, source, Some(order))
    }

    fn try_acquire_ci_with_order(
        &self,
        limit: u64,
        source: &str,
        preferred: Option<&[u64]>,
    ) -> Result<Option<SlotGuard>> {
        if self.class != "ci" {
            return Err(FsError::Io {
                operation: "acquire ci admission slot",
                path: self.dir.anchored_path(),
                source: std::io::Error::other("CI caps require the ci class"),
            });
        }
        let _domain = match self
            .dir
            .try_lock_existing(OsStr::new(CI_DOMAIN_LOCK), LockMode::Exclusive)
        {
            Ok(guard) => guard,
            Err(FsError::LockTimeout { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        let indices = ci_slot_indices(&self.dir)?;
        let mut active = 0u64;
        let mut active_caps = Vec::new();
        let mut available = Vec::new();
        for index in &indices {
            let slot = slot_name(*index);
            let locked = match self.dir.try_lock_existing(&slot, LockMode::Exclusive) {
                Ok(guard) => {
                    drop(guard);
                    false
                }
                Err(FsError::LockTimeout { .. }) => true,
                Err(error) if error.is_not_found() => continue,
                Err(error) => return Err(error),
            };
            let lease_cap = read_ci_lease_cap(&self.dir, *index)?;
            let holder = read_ci_holder_cap(&self.dir, *index)?;
            if lease_cap.is_some() || locked {
                active = active.saturating_add(1);
                active_caps.push(
                    lease_cap
                        .or_else(|| holder.map(|marker| marker.cap))
                        .unwrap_or_else(legacy_ci_cap),
                );
            } else if let Some(stale) = holder {
                let marker = ci_holder_marker(*index);
                if let Ok(bytes) = self.dir.read_stable(&marker, CI_CAP_MAX_BYTES)
                    && serde_json::from_slice::<CiCapMarker>(&bytes)
                        .is_ok_and(|record| record.id == stale.id)
                {
                    self.dir.unlink(&marker)?;
                }
                available.push(*index);
            } else {
                available.push(*index);
            }
        }

        let uncapped = limit == 0;
        let effective_cap = if uncapped {
            None
        } else {
            active_caps
                .iter()
                .filter(|cap| cap.limit > 0)
                .map(|cap| cap.limit)
                .chain(std::iter::once(limit))
                .min()
        };
        if effective_cap.is_some_and(|cap| active >= cap) {
            return Ok(None);
        }

        let mut candidates = Vec::with_capacity(available.len());
        if let Some(preferred) = preferred {
            for index in preferred.iter().copied() {
                if available.contains(&index) && !candidates.contains(&index) {
                    candidates.push(index);
                }
            }
        } else {
            candidates = available;
        }
        let mut selected = None;
        for index in candidates {
            match self
                .dir
                .try_lock_existing(&slot_name(index), LockMode::Exclusive)
            {
                Ok(guard) => {
                    selected = Some((index, guard));
                    break;
                }
                Err(FsError::LockTimeout { .. }) => continue,
                Err(error) if error.is_not_found() => continue,
                Err(error) => return Err(error),
            }
        }
        if selected.is_none() && preferred.is_none() {
            let index = indices.last().map_or(0, |last| last.saturating_add(1));
            let name = slot_name(index);
            // Lock names are immutable through the general filesystem API.
            // Create the next slot through the owner-lock API, which verifies
            // the inode and acquires it while the CI domain lock is held.
            // Sync the directory so the slot survives a host restart.
            let guard = self.dir.try_lock(&name, LockMode::Exclusive)?;
            self.dir.sync()?;
            selected = Some((index, guard));
        }
        let Some((index, guard)) = selected else {
            return Ok(None);
        };
        let source = bounded_cap_source(source);
        let id = ci_holder_id(index);
        let marker = ci_holder_marker(index);
        let bytes = serde_json::to_vec(&CiCapMarker {
            schema: 1,
            id: id.clone(),
            cap: AdmissionCap { limit, source },
        })
        .map_err(|source| FsError::Io {
            operation: "encode active ci admission cap",
            path: PathBuf::from(&marker),
            source: std::io::Error::other(source),
        })?;
        let directory = self.dir.try_clone()?;
        self.dir
            .atomic_write(&marker, ".ci-holder-", &bytes, PublishMode::NoReplace)?
            .require_durable()?;
        Ok(Some(SlotGuard {
            guard,
            index,
            class: self.class.clone(),
            ci_holder: Some((directory, marker, id)),
        }))
    }

    /// Hold the shared `ci` admission domain while changing a durable lease.
    pub fn lock_ci_domain(
        &self,
        budget: provenant_core::WaitBudget,
        cancel: &provenant_core::CancelToken,
        clock: &dyn provenant_core::Clock,
    ) -> Result<LockGuard> {
        if self.class != "ci" {
            return Err(FsError::Io {
                operation: "lock ci admission domain",
                path: self.dir.anchored_path(),
                source: std::io::Error::other("CI domain lock requires the ci class"),
            });
        }
        self.dir.lock(
            OsStr::new(CI_DOMAIN_LOCK),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )
    }

    fn try_acquire_indices(&self, order: impl Iterator<Item = u64>) -> Result<Option<SlotGuard>> {
        let mut count = 0;
        let mut missing = None;
        let mut absent = 0;
        for index in order {
            count += 1;
            match self
                .dir
                .try_lock_existing(&slot_name(index), LockMode::Exclusive)
            {
                Ok(guard) => {
                    let leased = match self.dir.read_stable(&durable_lease_name(index), 64 * 1024) {
                        Ok(_) => true,
                        Err(error) if error.is_not_found() => false,
                        Err(error) => return Err(error),
                    };
                    if leased {
                        drop(guard);
                        continue;
                    }
                    return Ok(Some(SlotGuard {
                        guard,
                        index,
                        class: self.class.clone(),
                        ci_holder: None,
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
            Some(error) if absent == count => Err(error),
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
        if self.class == "ci" {
            return ci_slot_indices(&self.dir)
                .unwrap_or_default()
                .into_iter()
                .filter(|index| {
                    read_ci_lease_cap(&self.dir, *index)
                        .ok()
                        .flatten()
                        .is_some()
                        || matches!(
                            self.dir
                                .try_lock_existing(&slot_name(*index), LockMode::Shared),
                            Err(FsError::LockTimeout { .. })
                        )
                })
                .count() as u64;
        }
        (0..limit)
            .filter(|index| {
                self.dir
                    .read_stable(&durable_lease_name(*index), 64 * 1024)
                    .is_ok()
                    || matches!(
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
    ci_holder: Option<(Directory, OsString, String)>,
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

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if let Some((directory, marker, id)) = self.ci_holder.take()
            && let Ok(bytes) = directory.read_stable(&marker, CI_CAP_MAX_BYTES)
            && serde_json::from_slice::<CiCapMarker>(&bytes).is_ok_and(|record| record.id == id)
        {
            let _ = directory.unlink(&marker);
            let _ = directory.sync();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
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
    fn ordered_acquisition_prefers_requested_indices() {
        let (_state, slots) = provisioned("build", 3);
        let first = slots.try_acquire_ordered(&[2, 0, 1]).unwrap().unwrap();
        assert_eq!(first.index(), 2);
        let second = slots.try_acquire_ordered(&[2, 0, 1]).unwrap().unwrap();
        assert_eq!(second.index(), 0);
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
    fn durable_ci_reservation_blocks_direct_slot_users_until_removed() {
        let _serial = serial();
        let (state, slots) = provisioned("ci", 1);
        let guard = slots.try_acquire(1).unwrap().unwrap();
        let directory = Directory::open_anchor(&class_directory(state.path(), "ci")).unwrap();
        directory
            .atomic_write(
                OsStr::new("slot-0.lease.json"),
                ".test-lease-",
                br#"{"lease":{"cap":{"limit":1,"source":"test"}}}"#,
                crate::fs::PublishMode::NoReplace,
            )
            .unwrap()
            .require_durable()
            .unwrap();
        drop(guard);

        assert!(slots.try_acquire(1).unwrap().is_none());
        assert_eq!(slots.held(1), 1);
        directory.unlink(OsStr::new("slot-0.lease.json")).unwrap();
        directory.sync().unwrap();
        assert!(slots.try_acquire(1).unwrap().is_some());
    }

    #[test]
    fn active_ci_holder_cap_constrains_later_requests() {
        let _serial = serial();
        let (_state, slots) = provisioned("ci", 2);
        let holder = slots.try_acquire_ci(1, "project limit").unwrap().unwrap();

        assert!(slots.try_acquire_ci(2, "launch limit").unwrap().is_none());
        drop(holder);
        assert!(slots.try_acquire_ci(2, "launch limit").unwrap().is_some());
    }

    #[test]
    fn ci_active_caps_are_shared_across_concurrent_mixed_limits() {
        let _serial = serial();
        let (state, slots) = provisioned("ci", 4);
        let first = slots
            .try_acquire_ci(4, "queue.ci_slots=4 from launch")
            .unwrap()
            .unwrap();
        let second = slots
            .try_acquire_ci(4, "queue.ci_slots=4 from launch")
            .unwrap()
            .unwrap();
        let third = slots
            .try_acquire_ci(4, "queue.ci_slots=4 from launch")
            .unwrap()
            .unwrap();
        let path = state.path().to_owned();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        std::thread::scope(|scope| {
            let high = scope.spawn(move || {
                let slots = AdmissionSlots::open(&path, "ci").unwrap();
                let guard = slots
                    .try_acquire_ci(4, "queue.ci_slots=4 from launch")
                    .unwrap();
                let Some(guard) = guard else {
                    entered_tx.send(None).unwrap();
                    return;
                };
                entered_tx.send(Some(guard.index())).unwrap();
                release_rx.recv().unwrap();
                drop(guard);
            });
            assert_eq!(entered_rx.recv().unwrap(), Some(3));
            assert!(
                slots
                    .try_acquire_ci(1, "queue.ci_slots=1 from launch")
                    .unwrap()
                    .is_none()
            );
            drop(first);
            drop(second);
            drop(third);
            assert!(
                slots
                    .try_acquire_ci(1, "queue.ci_slots=1 from launch")
                    .unwrap()
                    .is_none()
            );
            release_tx.send(()).unwrap();
            high.join().unwrap();
        });

        let low = slots
            .try_acquire_ci(1, "queue.ci_slots=1 from launch")
            .unwrap()
            .unwrap();
        assert!(
            slots
                .try_acquire_ci(4, "queue.ci_slots=4 from launch")
                .unwrap()
                .is_none()
        );
        drop(low);
        assert!(
            slots
                .try_acquire_ci(4, "queue.ci_slots=4 from launch")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn zero_ci_cap_is_uncapped_for_its_request_but_remains_counted() {
        let _serial = serial();
        let (_state, slots) = provisioned("ci", 1);
        let low = slots
            .try_acquire_ci(1, "queue.ci_slots=1 from project")
            .unwrap()
            .unwrap();
        let uncapped = slots
            .try_acquire_ci(0, "queue.ci_slots=0 from launch")
            .unwrap()
            .unwrap();
        assert!(
            slots
                .try_acquire_ci(1, "queue.ci_slots=1 from project")
                .unwrap()
                .is_none()
        );
        drop(uncapped);
        drop(low);
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
