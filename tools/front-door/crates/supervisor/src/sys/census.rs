//! Bounded native census and held custody-description observations.
//!
//! No function here signals a process. Any proof returned below is an
//! ephemeral observation that a registered process currently holds a lock
//! description; it does not create a process-control capability.
use crate::census::{
    Census, CensusError, ObservationIssue, OwnerRegistration, ProcessFact, ProcessIdentity,
    ProcessObservation, ProcessObserver,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io;
use thiserror::Error;

#[derive(Debug)]
pub(crate) struct HeldCustodyProof {
    owner: ProcessIdentity,
    _device: u64,
    _inode: u64,
}

impl HeldCustodyProof {
    pub(crate) fn owner_identity(&self) -> &ProcessIdentity {
        &self.owner
    }
}

#[derive(Debug, Error)]
pub(crate) enum CustodyProofError {
    #[error("registered process identity does not match the current process")]
    IdentityMismatch,
    #[error("registered process identity cannot be observed")]
    IdentityUnknown,
    #[error("registered process does not hold the matching custody description")]
    NotHeld,
    #[error("custody description observation is unavailable")]
    Unknown,
    #[error("custody descriptor is not a regular file")]
    InvalidDescriptor,
}

/// Issue a non-serialisable proof only after checking the exact registered
/// process identity before and after inspecting its held lock description.
pub(crate) fn prove_nested_custody(
    observer: &dyn ProcessObserver,
    registration: &OwnerRegistration,
    custody_file: &File,
) -> Result<HeldCustodyProof, CustodyProofError> {
    use std::os::unix::fs::MetadataExt;
    let identity = registration.identity();
    identity
        .validate()
        .map_err(|_| CustodyProofError::IdentityMismatch)?;
    if observer
        .boot_identity()
        .map_err(|_| CustodyProofError::Unknown)?
        != identity.boot
    {
        return Err(CustodyProofError::IdentityMismatch);
    }
    let current = observer.observe(identity.pid);
    if !matches!(
        &current,
        ProcessObservation::Present(fact) if fact.identity == *identity
    ) {
        return match current {
            ProcessObservation::Present(_) | ProcessObservation::Absent => {
                Err(CustodyProofError::IdentityMismatch)
            }
            ProcessObservation::Unknown(_) => Err(CustodyProofError::IdentityUnknown),
        };
    }
    let metadata = custody_file
        .metadata()
        .map_err(|_| CustodyProofError::Unknown)?;
    if !metadata.is_file() {
        return Err(CustodyProofError::InvalidDescriptor);
    }
    #[cfg(target_os = "linux")]
    let held = linux_owner_holds_lock(identity.pid, metadata.dev(), metadata.ino())?;
    #[cfg(target_os = "macos")]
    let held = darwin_owner_holds_lock(identity.pid, metadata.dev(), metadata.ino())?;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let held = {
        let _ = (identity, metadata);
        return Err(CustodyProofError::Unknown);
    };
    if !held {
        return Err(CustodyProofError::NotHeld);
    }
    match observer.observe(identity.pid) {
        ProcessObservation::Present(fact) if fact.identity == *identity => {}
        ProcessObservation::Present(_) | ProcessObservation::Absent => {
            return Err(CustodyProofError::IdentityMismatch);
        }
        ProcessObservation::Unknown(_) => return Err(CustodyProofError::IdentityUnknown),
    }
    Ok(HeldCustodyProof {
        owner: identity.clone(),
        _device: metadata.dev(),
        _inode: metadata.ino(),
    })
}

#[cfg(target_os = "linux")]
fn linux_owner_holds_lock(pid: i64, device: u64, inode: u64) -> Result<bool, CustodyProofError> {
    use std::os::unix::fs::MetadataExt;
    let directory = format!("/proc/{pid}/fd");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(CustodyProofError::Unknown);
        }
        Err(_) => return Err(CustodyProofError::Unknown),
    };
    let mut inspected = 0_usize;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return Err(CustodyProofError::Unknown),
        };
        inspected = inspected.saturating_add(1);
        if inspected > 4096 {
            return Err(CustodyProofError::Unknown);
        }
        let Some(fd_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if fd_name.parse::<i32>().ok().filter(|fd| *fd >= 0).is_none() {
            continue;
        }
        let path = entry.path();
        let before = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if before.dev() != device || before.ino() != inode {
            continue;
        }
        let fdinfo = format!("/proc/{pid}/fdinfo/{fd_name}");
        let info = match read_small_text(&fdinfo, 64 * 1024) {
            Ok(info) => info,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                return Err(CustodyProofError::Unknown);
            }
            Err(_) => continue,
        };
        let lock_matches = info
            .lines()
            .any(|line| lock_record_matches(line, device, inode));
        let after = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if lock_matches
            && after.dev() == device
            && after.ino() == inode
            && before.dev() == after.dev()
            && before.ino() == after.ino()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn lock_record_matches(line: &str, device: u64, inode: u64) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(rest) = line.strip_prefix("lock:") else {
        return false;
    };
    let fields: Vec<_> = rest.split_whitespace().collect();
    let Some(file_identity) = fields.get(5) else {
        return false;
    };
    let mut parts = file_identity.split(':');
    let (Some(major), Some(minor), Some(lock_inode), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let major_value = match u64::from_str_radix(major, 16) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let minor_value = match u64::from_str_radix(minor, 16) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let inode_value = match lock_inode.parse::<u64>() {
        Ok(value) => value,
        Err(_) => return false,
    };
    fields.get(1) == Some(&"FLOCK")
        && fields.get(2) == Some(&"ADVISORY")
        && fields.get(3) == Some(&"WRITE")
        && inode_value == inode
        && major_value == linux_device_major(device)
        && minor_value == linux_device_minor(device)
}

#[cfg(target_os = "linux")]
fn linux_device_major(device: u64) -> u64 {
    ((device >> 8) & 0x0fff) | ((device >> 32) & !0x0fff)
}

#[cfg(target_os = "linux")]
fn linux_device_minor(device: u64) -> u64 {
    (device & 0x00ff) | ((device >> 12) & !0x00ff)
}

#[cfg(target_os = "linux")]
fn read_small_text(path: &str, limit: u64) -> io::Result<String> {
    use std::io::Read;
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    String::from_utf8(bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
}

#[cfg(target_os = "macos")]
fn darwin_owner_holds_lock(
    _pid: i64,
    _device: u64,
    _inode: u64,
) -> Result<bool, CustodyProofError> {
    // libproc reports descriptor and vnode identity on Darwin, but its public
    // record does not report which process owns a `flock` lock. Refuse to issue
    // a custody proof rather than infer lock ownership from an open descriptor.
    Err(CustodyProofError::Unknown)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProcessObserver;

impl ProcessObserver for SystemProcessObserver {
    fn boot_identity(&self) -> Result<String, CensusError> {
        Self::boot_identity()
    }

    fn observe(&self, pid: i64) -> ProcessObservation {
        if pid <= 0 {
            return ProcessObservation::Unknown(ObservationIssue::Malformed);
        }
        system_observe(pid)
    }

    fn census(&self, max_entries: usize) -> Result<Census, CensusError> {
        system_census(max_entries)
    }
}

impl SystemProcessObserver {
    pub fn boot_identity() -> Result<String, CensusError> {
        system_boot_identity()
    }

    pub fn observe(&self, pid: i64) -> ProcessObservation {
        ProcessObserver::observe(self, pid)
    }

    pub fn census(&self, max_entries: usize) -> Result<Census, CensusError> {
        ProcessObserver::census(self, max_entries)
    }
}

/// Internal observer with an explicit stable boot key. The constructor is
/// crate-private so recorded fixtures can drive native process reads without
/// patching environment or process-global state.
#[derive(Clone, Debug)]
pub(crate) struct NativeProcessObserver {
    boot: String,
}

impl NativeProcessObserver {
    pub(crate) fn for_boot(boot: impl Into<String>) -> Result<Self, CensusError> {
        let boot = boot.into();
        if boot.is_empty() || boot.len() > 256 {
            return Err(CensusError::InvalidRecord);
        }
        Ok(Self { boot })
    }

    pub(crate) fn observe(&self, pid: i64) -> ProcessObservation {
        observe_with_boot(pid, &self.boot)
    }

    pub(crate) fn census(&self, max_entries: usize) -> Result<Census, CensusError> {
        census_with_boot(max_entries, &self.boot)
    }
}

impl ProcessObserver for NativeProcessObserver {
    fn boot_identity(&self) -> Result<String, CensusError> {
        Ok(self.boot.clone())
    }

    fn observe(&self, pid: i64) -> ProcessObservation {
        NativeProcessObserver::observe(self, pid)
    }

    fn census(&self, max_entries: usize) -> Result<Census, CensusError> {
        NativeProcessObserver::census(self, max_entries)
    }
}

#[cfg(target_os = "linux")]
fn system_boot_identity() -> Result<String, CensusError> {
    const MAX_BOOT_BYTES: usize = 250;
    let value = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(CensusError::BootIdentity)?;
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_BOOT_BYTES {
        return Err(CensusError::InvalidRecord);
    }
    Ok(format!("linux:{value}"))
}

#[cfg(target_os = "macos")]
fn system_boot_identity() -> Result<String, CensusError> {
    use std::os::raw::c_char;

    const BOOT_UUID_NAME: &[u8] = b"kern.bootsessionuuid\0";
    let mut value = [0_u8; 128];
    let mut length = value.len();
    // SAFETY: BOOT_UUID_NAME is NUL-terminated; value is a live writable
    // buffer whose capacity is passed exactly. sysctl retains no pointer.
    let result = unsafe {
        libc::sysctlbyname(
            BOOT_UUID_NAME.as_ptr().cast::<c_char>(),
            value.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(CensusError::BootIdentity(io::Error::last_os_error()));
    }
    if length == 0 || length > value.len() {
        return Err(CensusError::InvalidRecord);
    }
    let bytes = &value[..length];
    let nul = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    let uuid = std::str::from_utf8(&bytes[..nul]).map_err(|_| CensusError::InvalidRecord)?;
    if uuid.is_empty() || uuid.len() > 100 || uuid.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(CensusError::InvalidRecord);
    }
    Ok(format!("darwin:{uuid}"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn system_boot_identity() -> Result<String, CensusError> {
    Err(CensusError::Unsupported)
}

fn system_observe(pid: i64) -> ProcessObservation {
    if pid <= 0 || i32::try_from(pid).ok().filter(|pid| *pid > 0).is_none() {
        return ProcessObservation::Unknown(ObservationIssue::Malformed);
    }
    let boot = match system_boot_identity() {
        Ok(boot) => boot,
        Err(_) => return ProcessObservation::Unknown(ObservationIssue::Io),
    };
    let observer = match NativeProcessObserver::for_boot(boot) {
        Ok(observer) => observer,
        Err(_) => return ProcessObservation::Unknown(ObservationIssue::Malformed),
    };
    observer.observe(pid)
}

/// Read one native process fact under an explicitly supplied boot key. This
/// crate-private seam lets custody tests use recorded stable boot fixtures;
/// production observation calls it only with the native kernel boot identity.
pub(crate) fn observe_with_boot(pid: i64, boot: &str) -> ProcessObservation {
    if pid <= 0 || boot.is_empty() || boot.len() > 256 {
        return ProcessObservation::Unknown(ObservationIssue::Malformed);
    }
    let Some(pid) = i32::try_from(pid).ok().filter(|pid| *pid > 0) else {
        return ProcessObservation::Unknown(ObservationIssue::Malformed);
    };
    #[cfg(target_os = "linux")]
    {
        let Some(ticks) = linux_clock_ticks() else {
            return ProcessObservation::Unknown(ObservationIssue::Io);
        };
        return match linux_process(pid, boot, ticks) {
            LinuxRead::Present(fact) => ProcessObservation::Present(fact),
            LinuxRead::Absent => ProcessObservation::Absent,
            LinuxRead::Unknown(issue) => ProcessObservation::Unknown(issue),
        };
    }
    #[cfg(target_os = "macos")]
    {
        darwin_process(pid, boot)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (pid, boot);
        ProcessObservation::Unknown(ObservationIssue::Unsupported)
    }
}

fn system_census(max_entries: usize) -> Result<Census, CensusError> {
    let boot = system_boot_identity()?;
    NativeProcessObserver::for_boot(boot)?.census(max_entries)
}

#[cfg(target_os = "linux")]
pub(crate) fn census_with_boot(max_entries: usize, boot: &str) -> Result<Census, CensusError> {
    use std::os::unix::ffi::OsStrExt;
    if max_entries == 0 || max_entries > crate::census::MAX_CENSUS_ENTRIES {
        return Err(CensusError::InvalidBound);
    }
    if boot.is_empty() || boot.len() > 256 {
        return Err(CensusError::InvalidRecord);
    }
    let ticks = linux_clock_ticks().ok_or(CensusError::InvalidRecord)?;
    let directory = std::fs::read_dir("/proc").map_err(CensusError::ProcessList)?;
    let mut pids = Vec::new();
    let mut list_complete = true;
    let mut truncated = false;
    for entry in directory {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                list_complete = false;
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            if name.as_bytes().iter().all(u8::is_ascii_digit) {
                list_complete = false;
            }
            continue;
        };
        let Ok(pid) = name.parse::<i64>() else {
            continue;
        };
        if pid <= 0 {
            continue;
        }
        if pids.len() == max_entries {
            truncated = true;
            list_complete = false;
            break;
        }
        pids.push(pid);
    }

    let mut facts = Vec::new();
    let mut unknown = BTreeMap::new();
    let mut absent = BTreeSet::new();
    for pid in pids {
        let Ok(pid_i32) = i32::try_from(pid) else {
            unknown.insert(pid, ObservationIssue::Malformed);
            continue;
        };
        match linux_process(pid_i32, &boot, ticks) {
            LinuxRead::Present(fact) => facts.push(fact),
            LinuxRead::Absent => {
                absent.insert(pid);
            }
            LinuxRead::Unknown(issue) => {
                unknown.insert(pid, issue);
            }
        }
    }
    Census::from_observations(
        facts,
        unknown,
        absent,
        list_complete,
        truncated,
        max_entries,
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn census_with_boot(max_entries: usize, boot: &str) -> Result<Census, CensusError> {
    if max_entries == 0 || max_entries > crate::census::MAX_CENSUS_ENTRIES {
        return Err(CensusError::InvalidBound);
    }
    if boot.is_empty() || boot.len() > 256 {
        return Err(CensusError::InvalidRecord);
    }
    let (mut pids, truncated) = darwin_process_ids(max_entries)?;
    if truncated {
        pids.truncate(max_entries);
    }
    let mut facts = Vec::new();
    let mut unknown = BTreeMap::new();
    let mut absent = BTreeSet::new();
    for pid in pids {
        match darwin_process(pid, boot) {
            ProcessObservation::Present(fact) => facts.push(fact),
            ProcessObservation::Absent => {
                absent.insert(i64::from(pid));
            }
            ProcessObservation::Unknown(issue) => {
                unknown.insert(i64::from(pid), issue);
            }
        }
    }
    Census::from_observations(facts, unknown, absent, !truncated, truncated, max_entries)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn census_with_boot(_max_entries: usize, _boot: &str) -> Result<Census, CensusError> {
    Err(CensusError::Unsupported)
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
enum LinuxRead {
    Present(ProcessFact),
    Absent,
    Unknown(ObservationIssue),
}

#[cfg(target_os = "linux")]
fn linux_clock_ticks() -> Option<u64> {
    // SAFETY: sysconf reads a scalar system setting and retains no pointer.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(ticks).ok().filter(|ticks| *ticks > 0)
}

#[cfg(target_os = "linux")]
fn linux_process(pid: i32, boot: &str, ticks_per_second: u64) -> LinuxRead {
    use std::io::Read;
    if pid <= 0 || ticks_per_second == 0 {
        return LinuxRead::Unknown(ObservationIssue::Malformed);
    }
    let path = format!("/proc/{pid}/stat");
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return LinuxRead::Absent,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return LinuxRead::Unknown(ObservationIssue::PermissionDenied);
        }
        Err(_) => return LinuxRead::Unknown(ObservationIssue::Io),
    };
    const STAT_BOUND: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    if file.take(STAT_BOUND + 1).read_to_end(&mut bytes).is_err() {
        return LinuxRead::Unknown(ObservationIssue::Io);
    }
    if bytes.len() as u64 > STAT_BOUND {
        return LinuxRead::Unknown(ObservationIssue::Malformed);
    }
    match parse_linux_stat(pid, boot, ticks_per_second, &bytes) {
        Some(fact) => LinuxRead::Present(fact),
        None => LinuxRead::Unknown(ObservationIssue::Malformed),
    }
}

#[cfg(target_os = "linux")]
fn parse_linux_stat(
    pid: i32,
    boot: &str,
    ticks_per_second: u64,
    raw: &[u8],
) -> Option<ProcessFact> {
    let open = raw.iter().position(|byte| *byte == b'(')?;
    let close = raw.windows(2).rposition(|pair| pair == b") ")?;
    if close <= open {
        return None;
    }
    let declared = std::str::from_utf8(raw.get(..open)?)
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()?;
    if declared != i64::from(pid) {
        return None;
    }
    let tail = raw.get(close.checked_add(2)?..)?;
    let fields: Vec<_> = tail
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .collect();
    let parse_i64 = |index: usize| -> Option<i64> {
        std::str::from_utf8(fields.get(index).copied()?)
            .ok()?
            .parse()
            .ok()
    };
    let parse_u64 = |index: usize| -> Option<u64> {
        std::str::from_utf8(fields.get(index).copied()?)
            .ok()?
            .parse()
            .ok()
    };
    let state = fields.first().copied()?;
    let parent_pid = parse_i64(1)?;
    let group_id = parse_i64(2)?;
    let session_id = parse_i64(3)?;
    let user_ticks = parse_u64(11)?;
    let system_ticks = parse_u64(12)?;
    let start_ticks = parse_u64(19)?;
    if parent_pid < 0 || group_id <= 0 || session_id <= 0 || ticks_per_second == 0 {
        return None;
    }
    let cpu_micros = ticks_to_micros(user_ticks.checked_add(system_ticks)?, ticks_per_second)?;
    let command = String::from_utf8_lossy(raw.get(open.checked_add(1)?..close)?)
        .chars()
        .take(128)
        .collect();
    Some(ProcessFact {
        identity: ProcessIdentity {
            boot: boot.to_owned(),
            pid: i64::from(pid),
            native_start: start_ticks,
            group_id,
            session_id,
        },
        parent_pid,
        zombie: state == b"Z",
        cpu_micros: Some(cpu_micros),
        command: Some(command),
    })
}

#[cfg(target_os = "linux")]
fn ticks_to_micros(ticks: u64, ticks_per_second: u64) -> Option<u64> {
    let whole = (ticks / ticks_per_second).checked_mul(1_000_000)?;
    let fractional = (ticks % ticks_per_second).checked_mul(1_000_000)? / ticks_per_second;
    whole.checked_add(fractional)
}

#[cfg(target_os = "macos")]
fn darwin_process_ids(max_entries: usize) -> Result<(Vec<i32>, bool), CensusError> {
    use std::mem::size_of;
    // SAFETY: null buffer and zero size asks libproc for the current count;
    // it writes no memory and retains no pointer.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count < 0 {
        return Err(CensusError::ProcessList(io::Error::last_os_error()));
    }
    if count == 0 {
        return Err(CensusError::ProcessList(io::Error::other(
            "empty process list",
        )));
    }
    let capacity = usize::try_from(count)
        .ok()
        .and_then(|count| count.checked_add(1))
        .map(|capacity| capacity.min(max_entries.saturating_add(1)))
        .ok_or(CensusError::InvalidBound)?;
    let bytes = capacity
        .checked_mul(size_of::<i32>())
        .and_then(|bytes| i32::try_from(bytes).ok())
        .ok_or(CensusError::InvalidBound)?;
    let mut pids = vec![0_i32; capacity];
    // SAFETY: the vector owns the checked writable capacity for i32 PIDs.
    // libproc retains no pointer and the returned count is bounded below.
    let written = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    if written < 0 {
        return Err(CensusError::ProcessList(io::Error::last_os_error()));
    }
    let written = usize::try_from(written).map_err(|_| CensusError::InvalidRecord)?;
    if written == 0 {
        return Err(CensusError::ProcessList(io::Error::other(
            "empty process list",
        )));
    }
    if written > pids.len() {
        return Err(CensusError::InvalidRecord);
    }
    let truncated = written >= pids.len() || written > max_entries;
    pids.truncate(written.min(pids.len()));
    pids.retain(|pid| *pid > 0);
    Ok((pids, truncated))
}

#[cfg(target_os = "macos")]
fn darwin_zero_probe_session(pid: i32) -> (i32, Option<i32>) {
    // SAFETY: __error returns this thread's live errno slot; writing zero
    // ensures a failed getsid result cannot inherit stale errno.
    unsafe { *libc::__error() = 0 };
    // SAFETY: getsid reads only the scalar PID, retains no pointer, and reports
    // failure through this thread's errno slot.
    let session = unsafe { libc::getsid(pid) };
    // SAFETY: __error returns the same live thread-local slot after the call;
    // only the integer errno value is copied out.
    let errno = unsafe { *libc::__error() };
    (session, (errno != 0).then_some(errno))
}

fn classify_darwin_zero_probe(session: i32, errno: Option<i32>) -> ProcessObservation {
    if session > 0 {
        return ProcessObservation::Unknown(ObservationIssue::Io);
    }
    if session == -1 {
        return match errno {
            Some(libc::ESRCH) => ProcessObservation::Absent,
            Some(libc::EACCES | libc::EPERM) => {
                ProcessObservation::Unknown(ObservationIssue::PermissionDenied)
            }
            _ => ProcessObservation::Unknown(ObservationIssue::Io),
        };
    }
    ProcessObservation::Unknown(ObservationIssue::Malformed)
}

#[cfg(target_os = "macos")]
fn darwin_process(pid: i32, boot: &str) -> ProcessObservation {
    use std::mem::{MaybeUninit, size_of};
    if pid <= 0 {
        return ProcessObservation::Unknown(ObservationIssue::Malformed);
    }
    let requested = match i32::try_from(size_of::<libc::proc_bsdinfo>()) {
        Ok(size) => size,
        Err(_) => return ProcessObservation::Unknown(ObservationIssue::Unsupported),
    };
    let mut bsd = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: bsd is aligned writable storage with the exact ABI size for
    // PROC_PIDTBSDINFO. libproc borrows it and retains no pointer.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            bsd.as_mut_ptr().cast(),
            requested,
        )
    };
    if read == 0 {
        let (session, errno) = darwin_zero_probe_session(pid);
        return classify_darwin_zero_probe(session, errno);
    }
    if read < 0 {
        // The public libproc wrapper returns zero on an underlying error. A
        // negative result is outside that contract; errno may be stale.
        return ProcessObservation::Unknown(ObservationIssue::Io);
    }
    if read != requested {
        return ProcessObservation::Unknown(ObservationIssue::Io);
    }
    // SAFETY: exact successful libproc read initialised the whole ABI record.
    let bsd = unsafe { bsd.assume_init() };
    let start_sec = bsd.pbi_start_tvsec;
    let start_usec = bsd.pbi_start_tvusec;
    if start_usec >= 1_000_000 {
        return ProcessObservation::Unknown(ObservationIssue::Malformed);
    }
    let Some(native_start) = start_sec
        .checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(start_usec))
    else {
        return ProcessObservation::Unknown(ObservationIssue::Malformed);
    };
    let group_id = i64::from(bsd.pbi_pgid);
    if group_id <= 0 || i64::from(bsd.pbi_ppid) < 0 {
        return ProcessObservation::Unknown(ObservationIssue::Malformed);
    }
    // SAFETY: getsid takes only a scalar PID, retains no pointer and reports
    // failure through this thread's errno. The BSD record is rechecked below.
    let session = unsafe { libc::getsid(pid) };
    if session <= 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            ProcessObservation::Absent
        } else {
            ProcessObservation::Unknown(ObservationIssue::PermissionDenied)
        };
    }
    let cpu_micros = darwin_cpu_micros(pid);
    let mut verify = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: verify is aligned writable storage with the exact ABI size.
    // libproc borrows it and retains no pointer.
    let verify_read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            verify.as_mut_ptr().cast(),
            requested,
        )
    };
    if verify_read != requested {
        return ProcessObservation::Unknown(ObservationIssue::Io);
    }
    // SAFETY: the exact successful second read initialised the full record.
    let verify = unsafe { verify.assume_init() };
    // SAFETY: getsid again uses a scalar PID, retains no pointer and reports
    // failure through errno; the identity comparison fences PID reuse.
    let verify_session = unsafe { libc::getsid(pid) };
    if verify.pbi_start_tvsec != bsd.pbi_start_tvsec
        || verify.pbi_start_tvusec != bsd.pbi_start_tvusec
        || verify.pbi_ppid != bsd.pbi_ppid
        || verify.pbi_pgid != bsd.pbi_pgid
        || verify_session != session
    {
        return ProcessObservation::Unknown(ObservationIssue::Malformed);
    }
    let name = bsd
        .pbi_name
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    let command = if name.is_empty() {
        let comm = bsd
            .pbi_comm
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as u8)
            .collect::<Vec<_>>();
        String::from_utf8_lossy(&comm).into_owned()
    } else {
        String::from_utf8_lossy(&name).into_owned()
    };
    ProcessObservation::Present(ProcessFact {
        identity: ProcessIdentity {
            boot: boot.to_owned(),
            pid: i64::from(pid),
            native_start,
            group_id,
            session_id: i64::from(session),
        },
        parent_pid: i64::from(bsd.pbi_ppid),
        zombie: bsd.pbi_status == 5,
        cpu_micros,
        command: Some(command.chars().take(128).collect()),
    })
}

#[cfg(target_os = "macos")]
fn darwin_cpu_micros(pid: i32) -> Option<u64> {
    use std::mem::{MaybeUninit, size_of};
    #[allow(deprecated)]
    fn timebase() -> Option<(u32, u32)> {
        let mut value = MaybeUninit::<libc::mach_timebase_info>::uninit();
        // SAFETY: mach_timebase_info writes its exact pointer-free ABI record
        // to aligned live storage and retains no pointer.
        if unsafe { libc::mach_timebase_info(value.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: successful mach_timebase_info initialises both fields.
        let value = unsafe { value.assume_init() };
        (value.denom != 0).then_some((value.numer, value.denom))
    }
    let requested = i32::try_from(size_of::<libc::proc_taskinfo>()).ok()?;
    let mut task = MaybeUninit::<libc::proc_taskinfo>::uninit();
    // SAFETY: task is aligned writable storage with the exact ABI size for
    // PROC_PIDTASKINFO. libproc borrows it and retains no pointer.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            task.as_mut_ptr().cast(),
            requested,
        )
    };
    if read != requested {
        return None;
    }
    // SAFETY: exact successful libproc read initialised the entire record.
    let task = unsafe { task.assume_init() };
    let ticks = task.pti_total_user.checked_add(task.pti_total_system)?;
    darwin_cpu_micros_from_values(Some(ticks), timebase())
}

#[cfg(target_os = "macos")]
fn darwin_cpu_micros_from_values(ticks: Option<u64>, timebase: Option<(u32, u32)>) -> Option<u64> {
    let ticks = ticks?;
    let (numerator, denominator) = timebase?;
    if denominator == 0 {
        return None;
    }
    let nanos = u128::from(ticks).checked_mul(u128::from(numerator))? / u128::from(denominator);
    u64::try_from(nanos / 1_000).ok()
}

#[cfg(test)]
mod native_tests {
    use super::*;

    #[test]
    fn darwin_zero_probe_requires_fresh_esrch_and_never_guesses_from_a_live_pid() {
        assert_eq!(
            classify_darwin_zero_probe(-1, Some(libc::ESRCH)),
            ProcessObservation::Absent
        );
        assert_eq!(
            classify_darwin_zero_probe(-1, Some(libc::EACCES)),
            ProcessObservation::Unknown(ObservationIssue::PermissionDenied)
        );
        assert_eq!(
            classify_darwin_zero_probe(-1, Some(libc::EPERM)),
            ProcessObservation::Unknown(ObservationIssue::PermissionDenied)
        );
        assert_eq!(
            classify_darwin_zero_probe(-1, Some(libc::EINVAL)),
            ProcessObservation::Unknown(ObservationIssue::Io)
        );
        assert_eq!(
            classify_darwin_zero_probe(-1, None),
            ProcessObservation::Unknown(ObservationIssue::Io)
        );
        assert_eq!(
            classify_darwin_zero_probe(42, None),
            ProcessObservation::Unknown(ObservationIssue::Io)
        );
        assert_eq!(
            classify_darwin_zero_probe(0, None),
            ProcessObservation::Unknown(ObservationIssue::Malformed)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_cpu_conversion_is_checked_integer_microseconds() {
        assert_eq!(ticks_to_micros(123, 100), Some(1_230_000));
        assert_eq!(ticks_to_micros(u64::MAX, 1), None);
        assert_eq!(ticks_to_micros(1, 0), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_cpu_conversion_is_integer_and_checked() {
        assert_eq!(
            darwin_cpu_micros_from_values(Some(1_000), Some((1, 1))),
            Some(1)
        );
        assert_eq!(
            darwin_cpu_micros_from_values(Some(u64::MAX), Some((u32::MAX, 1))),
            None
        );
        assert_eq!(darwin_cpu_micros_from_values(Some(1), Some((1, 0))), None);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_boot_observer_uses_fixture_key_without_global_overrides() {
        let observer_result = NativeProcessObserver::for_boot("fixture-boot");
        assert!(
            observer_result.is_ok(),
            "valid fixture boot key was rejected"
        );
        let Some(observer) = observer_result.ok() else {
            return;
        };
        let pid = i64::from(std::process::id());
        let observed = observer.observe(pid);
        assert!(!matches!(observed, ProcessObservation::Absent));
        if let ProcessObservation::Present(fact) = observed {
            assert_eq!(fact.identity.boot, "fixture-boot");
            assert_eq!(fact.identity.pid, pid);
        }
        assert!(matches!(
            NativeProcessObserver::for_boot(String::new()),
            Err(CensusError::InvalidRecord)
        ));
    }

    #[test]
    fn native_observation_never_turns_boot_identity_failure_into_absence() {
        let pid = i64::from(std::process::id());
        match SystemProcessObserver::boot_identity() {
            Ok(boot) => assert!(matches!(
                SystemProcessObserver.observe(pid),
                ProcessObservation::Present(ref fact)
                    if fact.identity.pid == pid && fact.identity.boot == boot
            )),
            Err(_) => assert!(matches!(
                SystemProcessObserver.observe(pid),
                ProcessObservation::Unknown(_)
            )),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lock_record_requires_exact_file_identity_and_write_flock() {
        let metadata = match std::fs::metadata("/proc/self/exe") {
            Ok(metadata) => metadata,
            Err(_) => return,
        };
        let major = linux_device_major(metadata.dev());
        let minor = linux_device_minor(metadata.dev());
        let locked = format!(
            "lock: 1: FLOCK ADVISORY WRITE 1234 {major:x}:{minor:x}:{} 0 EOF",
            metadata.ino()
        );
        let unlocked = format!(
            "lock: 1: POSIX ADVISORY WRITE 1234 {major:x}:{minor:x}:{} 0 EOF",
            metadata.ino()
        );
        assert!(lock_record_matches(&locked, metadata.dev(), metadata.ino()));
        assert!(!lock_record_matches(
            &unlocked,
            metadata.dev(),
            metadata.ino()
        ));
        assert!(!lock_record_matches(
            &locked,
            metadata.dev(),
            metadata.ino().saturating_add(1)
        ));
    }
}
