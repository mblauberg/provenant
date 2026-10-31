//! Bounded native census and held custody-description observations.
//!
//! No function here signals a process. Any proof returned below is an
//! ephemeral observation that a registered process currently holds a lock
//! description; it does not create a process-control capability.
//!
//! Darwin's public libproc API does not report which process owns a `flock`,
//! so nested custody checks return `Unknown` there, and on every other
//! platform, where custody is unsupported. Darwin can also report an
//! exited zombie as absent; `ProcessFact::zombie` does not cover every zombie.
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
    if !owner_holds_lock(identity.pid, &metadata)? {
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
    })
}

#[cfg(target_os = "macos")]
fn owner_holds_lock(pid: i64, metadata: &std::fs::Metadata) -> Result<bool, CustodyProofError> {
    use std::os::unix::fs::MetadataExt;
    darwin_owner_holds_lock(pid, metadata.dev(), metadata.ino())
}

/// Custody is unsupported off macOS.
#[cfg(not(target_os = "macos"))]
fn owner_holds_lock(_pid: i64, _metadata: &std::fs::Metadata) -> Result<bool, CustodyProofError> {
    Err(CustodyProofError::Unknown)
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

fn system_boot_identity() -> Result<String, CensusError> {
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(boot) = BOOT.get() {
        return Ok(boot.clone());
    }
    let boot = read_system_boot_identity()?;
    // Boot identity cannot change during this process's lifetime. Do not cache
    // transient read failures; concurrent successful readers have the same key.
    let _ = BOOT.set(boot.clone());
    Ok(boot)
}

#[cfg(target_os = "linux")]
fn read_system_boot_identity() -> Result<String, CensusError> {
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
fn read_system_boot_identity() -> Result<String, CensusError> {
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
fn read_system_boot_identity() -> Result<String, CensusError> {
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
        match linux_process(pid, boot, ticks) {
            LinuxRead::Present(fact) => ProcessObservation::Present(fact),
            LinuxRead::OutsideProcessGroups => {
                ProcessObservation::Unknown(ObservationIssue::Unsupported)
            }
            LinuxRead::Absent => ProcessObservation::Absent,
            LinuxRead::Unknown(issue) => ProcessObservation::Unknown(issue),
        }
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
        match linux_process(pid_i32, boot, ticks) {
            LinuxRead::Present(fact) => facts.push(fact),
            LinuxRead::OutsideProcessGroups => {}
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
    OutsideProcessGroups,
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
        Some(result) => result,
        None => LinuxRead::Unknown(ObservationIssue::Malformed),
    }
}

#[cfg(target_os = "linux")]
fn parse_linux_stat(pid: i32, boot: &str, ticks_per_second: u64, raw: &[u8]) -> Option<LinuxRead> {
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
    if parent_pid < 0 || group_id < 0 || session_id < 0 || ticks_per_second == 0 {
        return None;
    }
    let cpu_micros = ticks_to_micros(user_ticks.checked_add(system_ticks)?, ticks_per_second)?;
    // Kernel tasks and namespace ancestors can expose a zero group or session.
    // A provider with positive IDs cannot create such a descendant: fork
    // inherits membership, and setsid/setpgid assign positive IDs. Exclude
    // these unrelated tasks only from the global custody census; a direct
    // observation of their PID remains unknown, never absent.
    if group_id == 0 || session_id == 0 {
        return Some(LinuxRead::OutsideProcessGroups);
    }
    let command = String::from_utf8_lossy(raw.get(open.checked_add(1)?..close)?)
        .chars()
        .take(128)
        .collect();
    Some(LinuxRead::Present(ProcessFact {
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
    }))
}

#[cfg(target_os = "linux")]
fn ticks_to_micros(ticks: u64, ticks_per_second: u64) -> Option<u64> {
    let whole = ticks
        .checked_div(ticks_per_second)?
        .checked_mul(1_000_000)?;
    let fractional = ticks
        .checked_rem(ticks_per_second)?
        .checked_mul(1_000_000)?
        .checked_div(ticks_per_second)?;
    whole.checked_add(fractional)
}

#[cfg(target_os = "macos")]
fn darwin_process_ids(max_entries: usize) -> Result<(Vec<i32>, bool), CensusError> {
    use std::mem::size_of;
    darwin_process_ids_with(max_entries, |buffer| {
        let result = match buffer {
            None => {
                // SAFETY: null buffer and zero size asks libproc for the current
                // count; it writes no memory and retains no pointer.
                unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) }
            }
            Some(pids) => {
                let bytes = pids
                    .len()
                    .checked_mul(size_of::<i32>())
                    .and_then(|bytes| i32::try_from(bytes).ok())
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "process list buffer too large")
                    })?;
                // SAFETY: the helper provides the checked writable slice whose
                // byte length is passed below. libproc retains no pointer.
                unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) }
            }
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            usize::try_from(result).map_err(|_| io::Error::other("negative process count"))
        }
    })
}

#[cfg(any(target_os = "macos", test))]
fn darwin_process_ids_with(
    max_entries: usize,
    mut list: impl FnMut(Option<&mut [i32]>) -> io::Result<usize>,
) -> Result<(Vec<i32>, bool), CensusError> {
    const MAX_ATTEMPTS: usize = 3;
    if max_entries == 0 || max_entries > crate::census::MAX_CENSUS_ENTRIES {
        return Err(CensusError::InvalidBound);
    }
    let max_capacity = max_entries
        .checked_add(1)
        .ok_or(CensusError::InvalidBound)?;
    let mut count = list(None).map_err(CensusError::ProcessList)?;
    if count == 0 {
        return Err(CensusError::ProcessList(io::Error::other(
            "empty process list",
        )));
    }
    let mut grow_to = 0_usize;
    for attempt in 0..MAX_ATTEMPTS {
        let count_capacity = count
            .checked_mul(2)
            .and_then(|capacity| capacity.checked_add(64))
            .unwrap_or(max_capacity);
        let capacity = count_capacity.max(grow_to).min(max_capacity);
        let mut pids = vec![0_i32; capacity];
        let written = list(Some(&mut pids)).map_err(CensusError::ProcessList)?;
        if written == 0 {
            return Err(CensusError::ProcessList(io::Error::other(
                "empty process list",
            )));
        }
        if written > pids.len() {
            return Err(CensusError::InvalidRecord);
        }
        let filled = written == pids.len();
        if filled && attempt + 1 < MAX_ATTEMPTS {
            count = list(None).map_err(CensusError::ProcessList)?;
            if count == 0 {
                return Err(CensusError::ProcessList(io::Error::other(
                    "empty process list",
                )));
            }
            grow_to = capacity.saturating_mul(2).max(capacity.saturating_add(1));
            continue;
        }
        let truncated = filled || written > max_entries;
        pids.truncate(written);
        pids.retain(|pid| *pid > 0);
        return Ok((pids, truncated));
    }
    Err(CensusError::InvalidRecord)
}

#[cfg(target_os = "macos")]
fn darwin_zero_probe_session(pid: i32) -> (i32, Option<i32>) {
    // SAFETY: __error returns this thread's live errno slot; writing zero
    // ensures a failed getsid result cannot inherit stale errno.
    let errno_slot = unsafe { libc::__error() };
    // SAFETY: errno_slot is the live thread-local slot returned immediately
    // above; writing it cannot affect another thread.
    unsafe { *errno_slot = 0 };
    // SAFETY: getsid reads only the scalar PID, retains no pointer, and reports
    // failure through this thread's errno slot.
    let session = unsafe { libc::getsid(pid) };
    // SAFETY: __error returns the same live thread-local slot after the call;
    // only the integer errno value is copied out.
    let errno_slot = unsafe { libc::__error() };
    // SAFETY: errno_slot is the live thread-local slot returned immediately
    // above; only its integer value is read.
    let errno = unsafe { *errno_slot };
    (session, (errno != 0).then_some(errno))
}

#[cfg(target_os = "macos")]
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
        if matches!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::EACCES | libc::EPERM)
        ) {
            return darwin_short_process(pid, boot);
        }
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

// libproc denies full BSD records for foreign-UID services on Darwin. Its
// short record is public, but lacks start time. Supplement only that missing
// field from the public sysctl kinfo_proc prefix; never invent an identity.
#[cfg(target_os = "macos")]
fn darwin_short_process(pid: i32, boot: &str) -> ProcessObservation {
    let read = || -> Option<ProcessFact> {
        let first_start = darwin_start_stamp(pid)?;
        let first = darwin_short_info(pid)?;
        // SAFETY: getsid accepts a scalar PID and retains no pointers.
        let session = unsafe { libc::getsid(pid) };
        let second = darwin_short_info(pid)?;
        let second_start = darwin_start_stamp(pid)?;
        // SAFETY: getsid again accepts a scalar PID and retains no pointers.
        let second_session = unsafe { libc::getsid(pid) };
        if first_start != second_start
            || session <= 0
            || session != second_session
            || first.pbsi_pid != u32::try_from(pid).ok()?
            || first.pbsi_pid != second.pbsi_pid
            || first.pbsi_ppid != second.pbsi_ppid
            || first.pbsi_pgid == 0
            || first.pbsi_pgid != second.pbsi_pgid
            || first.pbsi_status != second.pbsi_status
        {
            return None;
        }
        let command: Vec<_> = first
            .pbsi_comm
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as u8)
            .collect();
        Some(ProcessFact {
            identity: ProcessIdentity {
                boot: boot.to_owned(),
                pid: i64::from(pid),
                native_start: first_start.0,
                group_id: i64::from(first.pbsi_pgid),
                session_id: i64::from(session),
            },
            parent_pid: i64::from(first.pbsi_ppid),
            zombie: first.pbsi_status == 5,
            cpu_micros: None,
            command: Some(String::from_utf8_lossy(&command).into_owned()),
        })
    };
    match read() {
        Some(fact) => ProcessObservation::Present(fact),
        None => {
            let (session, errno) = darwin_zero_probe_session(pid);
            classify_darwin_zero_probe(session, errno)
        }
    }
}

#[cfg(target_os = "macos")]
fn darwin_short_info(pid: i32) -> Option<libc::proc_bsdshortinfo> {
    let requested = i32::try_from(std::mem::size_of::<libc::proc_bsdshortinfo>()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::uninit();
    // SAFETY: info is aligned writable storage of the exact short BSD ABI size;
    // libproc borrows it for this call and retains no pointer.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            requested,
        )
    };
    if read != requested {
        return None;
    }
    // SAFETY: the exact-size successful read initialized the complete record.
    Some(unsafe { info.assume_init() })
}

#[cfg(target_os = "macos")]
fn darwin_start_stamp(pid: i32) -> Option<(u64, usize)> {
    // Public Darwin SDK: sys/sysctl.h kinfo_proc begins with kp_proc;
    // sys/proc.h extern_proc begins with the p_starttime timeval union.
    // Only this prefix is interpreted. The opaque tail is bounded, not a
    // hand-maintained copy of the architecture-dependent kinfo_proc layout.
    #[repr(C)]
    struct KinfoPrefix {
        start: libc::timeval,
        opaque: [u8; 4096],
    }
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    let mut size = 0;
    // SAFETY: the four-element MIB is live; null output requests the record size.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || size < std::mem::size_of::<libc::timeval>() || size > 4096 {
        return None;
    }
    let expected = size;
    let mut prefix = KinfoPrefix {
        start: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        opaque: [0; 4096],
    };
    // SAFETY: prefix is aligned initialized storage larger than the checked
    // requested record. The public ABI places a timeval at offset zero; all
    // remaining bytes stay opaque. sysctl borrows both buffers for this call.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            std::ptr::from_mut(&mut prefix).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    // KERN_PROC sizing includes kernel headroom (currently six records for
    // this one-PID query). Validate the actual prefix and bound; the caller
    // requires the actual record length to agree across both identity reads.
    if status != 0 || size < std::mem::size_of::<libc::timeval>() || size > expected {
        return None;
    }
    let seconds = u64::try_from(prefix.start.tv_sec).ok()?;
    let micros = u64::try_from(prefix.start.tv_usec).ok()?;
    if micros >= 1_000_000 {
        return None;
    }
    Some((seconds.checked_mul(1_000_000)?.checked_add(micros)?, size))
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    // Exported by libSystem (libsystem_sandbox). Variadic: the filter value
    // follows `filter_type`, and is absent when `filter_type` is 0.
    fn sandbox_check(
        pid: libc::pid_t,
        operation: *const libc::c_char,
        filter_type: libc::c_int,
        ...
    ) -> libc::c_int;
}

/// Whether a process is still inside the sandbox of the attempt that owns
/// `lookup_name`: sandboxed at all, allowed to look that name up, yet denied an
/// unrelated name. Only the attempt's own profile grants that one name while
/// denying others; system daemons' sandboxes allow lookups broadly and fail the
/// control. A seatbelt sandbox is inherited and cannot be dropped, so this
/// survives reparenting and `setsid`. Unsandboxed processes, other sandboxes
/// are not members. Failed native checks remain errors so callers can either
/// fail an observation or conservatively withhold cleanup authority.
#[cfg(target_os = "macos")]
pub(crate) fn darwin_in_attempt_sandbox(
    pid: i32,
    lookup_name: &std::ffi::CStr,
) -> io::Result<bool> {
    const SANDBOX_FILTER_GLOBAL_NAME: libc::c_int = 2;
    const CONTROL_NAME: &std::ffi::CStr = c"dev.provenant.attempt.control";
    let lookup = |name: &std::ffi::CStr| {
        // SAFETY: both strings are live NUL-terminated C strings for the call,
        // and the variadic argument is the single char pointer that filter type
        // 2 (global name) expects. Nothing is retained.
        unsafe {
            sandbox_check(
                pid,
                c"mach-lookup".as_ptr(),
                SANDBOX_FILTER_GLOBAL_NAME,
                name.as_ptr(),
            )
        }
    };
    // SAFETY: a null operation with filter type 0 asks only whether the pid is
    // sandboxed; sandbox_check reads no pointer and retains none.
    let sandboxed = unsafe { sandbox_check(pid, std::ptr::null(), 0) };
    // Capture errno immediately after each failing check: a subsequent lookup
    // must not replace ESRCH (process churn) with an unrelated diagnostic.
    let checked = |value| match value {
        0 | 1 => Ok(value),
        _ => Err(io::Error::last_os_error()),
    };
    let sandboxed = checked(sandboxed)?;
    let permitted = checked(lookup(lookup_name))?;
    let control = checked(lookup(CONTROL_NAME))?;
    Ok((sandboxed, permitted, control) == (1, 0, 1))
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

/// Native identity of this management process; not a custody handle.
pub fn current_process() -> crate::census::ProcessObservation {
    SystemProcessObserver.observe(i64::from(std::process::id()))
}

#[cfg(test)]
mod native_tests {
    use super::*;

    #[test]
    fn darwin_pid_listing_retries_when_a_process_arrives_after_the_count() {
        let mut count_calls = 0_usize;
        let mut buffer_calls = 0_usize;
        let result = darwin_process_ids_with(256, |buffer| match buffer {
            None => {
                count_calls += 1;
                Ok(if count_calls == 1 { 4 } else { 72 })
            }
            Some(buffer) => {
                buffer_calls += 1;
                let written = if buffer_calls == 1 { buffer.len() } else { 73 };
                for (slot, pid) in buffer.iter_mut().take(written).zip(1_i32..) {
                    *slot = pid;
                }
                Ok(written)
            }
        });
        let (pids, truncated) = match result {
            Ok(result) => result,
            Err(error) => panic!("injected process listing failed: {error}"),
        };
        assert_eq!(pids.len(), 73);
        assert!(!truncated);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn darwin_nested_owner_lock_observation_is_unknown() {
        assert!(matches!(
            darwin_owner_holds_lock(1, 1, 1),
            Err(CustodyProofError::Unknown)
        ));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn darwin_short_record_and_sysctl_start_match_the_full_native_identity() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let full = darwin_process(pid, "recorded-boot");
        let fallback = darwin_short_process(pid, "recorded-boot");
        match (full, fallback) {
            (ProcessObservation::Present(full), ProcessObservation::Present(fallback)) => {
                assert_eq!(full.identity, fallback.identity);
                assert_eq!(full.identity.group_id, fallback.identity.group_id);
                assert_eq!(full.identity.session_id, fallback.identity.session_id);
                assert_eq!(full.parent_pid, fallback.parent_pid);
            }
            other => panic!("native identity unavailable: {other:?}"),
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
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
    fn linux_tasks_outside_provider_membership_do_not_poison_the_census() {
        for raw in [
            b"2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 0 0 0 20 0 1 0 1\n".as_slice(),
            b"2 (task) S 1 0 2 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 1",
            b"2 (containerd-shim) S 1 2 0 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 1",
        ] {
            assert!(matches!(
                parse_linux_stat(2, "boot", 100, raw),
                Some(LinuxRead::OutsideProcessGroups)
            ));
        }
        for raw in [
            b"2 (task) S 1 -2 -2 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 1".as_slice(),
            b"2 (task) S 0 0 0",
        ] {
            assert!(parse_linux_stat(2, "boot", 100, raw).is_none());
        }
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

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn custody_proof_is_unknown_off_macos() {
        let Ok(boot) = SystemProcessObserver::boot_identity() else {
            return;
        };
        let observer = match NativeProcessObserver::for_boot(boot) {
            Ok(observer) => observer,
            Err(error) => panic!("boot key rejected: {error}"),
        };
        let ProcessObservation::Present(fact) = observer.observe(i64::from(std::process::id()))
        else {
            panic!("current process not observed");
        };
        let registration = OwnerRegistration::from_validated_registration(fact.identity);
        let path = std::env::temp_dir().join(format!("census-proof-{}", std::process::id()));
        let regular = match File::create(&path) {
            Ok(file) => file,
            Err(error) => panic!("create failed: {error}"),
        };
        let result = prove_nested_custody(&observer, &registration, &regular);
        let _ = std::fs::remove_file(&path);
        assert!(matches!(result, Err(CustodyProofError::Unknown)));
    }
}
