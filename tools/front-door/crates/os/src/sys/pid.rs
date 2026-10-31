use crate::pid::{CwdObservation, PidError, ProcessSignal, ProcessState};
use std::io;

const MAX_EINTR_RETRIES: u64 = 8;

#[cfg(any(target_os = "macos", test))]
fn darwin_thread_state(states: &[Option<i32>]) -> io::Result<ProcessState> {
    // These values are from Darwin's mach/thread_info.h; 5 is HALTED.
    // Missing a stall falls back to recovery; a false stall blocks recovery.
    const RUNNING: i32 = 1;
    const UNINTERRUPTIBLE: i32 = 4;
    if !states.iter().any(Option::is_some)
        || states
            .iter()
            .flatten()
            .any(|state| !(1..=5).contains(state))
    {
        return Err(io::Error::other("invalid thread states"));
    }
    Ok(
        if states.contains(&Some(UNINTERRUPTIBLE)) && !states.contains(&Some(RUNNING)) {
            ProcessState::Uninterruptible
        } else {
            ProcessState::Running
        },
    )
}

#[cfg(target_os = "macos")]
fn darwin_threads(pid: i32) -> io::Result<Vec<u64>> {
    // A full buffer may have been truncated. Do not infer a stall from an incomplete census.
    const MAX_THREADS: usize = 16_384;
    const PROC_PIDLISTTHREADS: i32 = 6;
    let mut threads = vec![0u64; MAX_THREADS];
    let size = i32::try_from(threads.len() * std::mem::size_of::<u64>())
        .map_err(|_| io::Error::other("thread buffer too large"))?;
    // SAFETY: the vector supplies writable storage of size bytes, retained by the caller.
    let count = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDLISTTHREADS,
            0,
            threads.as_mut_ptr().cast(),
            size,
        )
    };
    if count <= 0 || count >= size || !(count as usize).is_multiple_of(std::mem::size_of::<u64>()) {
        return Err(io::Error::other("thread list unavailable or incomplete"));
    }
    threads.truncate(count as usize / std::mem::size_of::<u64>());
    Ok(threads)
}

#[cfg(target_os = "macos")]
fn darwin_thread_states(pid: i32) -> io::Result<ProcessState> {
    let threads = darwin_threads(pid)?;
    let mut states = Vec::with_capacity(threads.len());
    let mut refreshed = None;
    let requested = std::mem::size_of::<libc::proc_threadinfo>() as i32;
    for thread in threads {
        let mut info = std::mem::MaybeUninit::<libc::proc_threadinfo>::uninit();
        // SAFETY: info supplies writable ABI-sized storage and libproc does not retain it.
        let returned = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTHREADINFO,
                thread,
                info.as_mut_ptr().cast(),
                requested,
            )
        };
        if returned != requested {
            // Ignore a failed query only if a fresh census proves the thread vanished.
            if refreshed.is_none() {
                refreshed = Some(darwin_threads(pid)?);
            }
            if refreshed
                .as_ref()
                .is_some_and(|current| current.contains(&thread))
            {
                return Err(io::Error::other("thread state unavailable"));
            }
            states.push(None);
            continue;
        }
        // SAFETY: a full-size successful call initialised the record.
        states.push(Some(unsafe { info.assume_init() }.pth_run_state));
    }
    darwin_thread_state(&states)
}

#[cfg(any(target_os = "linux", test))]
fn linux_stat_state(stat: &str) -> io::Result<ProcessState> {
    // comm is parenthesised and may itself contain spaces or parentheses.
    let (_, tail) = stat
        .rsplit_once(')')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid proc stat"))?;
    let state = tail
        .strip_prefix(' ')
        .and_then(|tail| tail.split_whitespace().next())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing proc state"))?;
    match state {
        "D" => Ok(ProcessState::Uninterruptible),
        "T" | "t" => Ok(ProcessState::Stopped),
        "Z" | "X" | "x" => Ok(ProcessState::Gone),
        "R" | "S" | "I" | "W" | "P" => Ok(ProcessState::Running),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown proc state",
        )),
    }
}

pub(crate) fn process_state(pid: i32) -> io::Result<ProcessState> {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => linux_stat_state(&stat),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ProcessState::Gone),
            Err(error) => Err(error),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let requested = std::mem::size_of::<libc::proc_bsdshortinfo>() as i32;
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::uninit();
        // SAFETY: info is writable storage of the requested ABI size; libproc retains no pointer.
        let returned = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                requested,
            )
        };
        if returned == 0 {
            return Err(io::Error::other("process status unavailable"));
        }
        if returned != requested {
            return Err(io::Error::other("short process status record"));
        }
        // SAFETY: a full-size successful call initialised the record.
        let info = unsafe { info.assume_init() };
        if info.pbsi_status == libc::SZOMB {
            return Ok(ProcessState::Gone);
        }
        let threads = darwin_thread_states(pid)?;
        Ok(if threads == ProcessState::Uninterruptible {
            threads
        } else if info.pbsi_status == libc::SSTOP {
            ProcessState::Stopped
        } else {
            threads
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process state unsupported",
        ))
    }
}

#[cfg(test)]
#[test]
fn classifies_darwin_thread_states() {
    assert_eq!(
        darwin_thread_state(&[Some(4), Some(3)]).unwrap(),
        ProcessState::Uninterruptible
    );
    assert_eq!(
        darwin_thread_state(&[Some(4), Some(1)]).unwrap(),
        ProcessState::Running
    );
    assert_eq!(
        darwin_thread_state(&[Some(5), Some(3)]).unwrap(),
        ProcessState::Running
    );
    assert_eq!(
        darwin_thread_state(&[None, Some(4), Some(3)]).unwrap(),
        ProcessState::Uninterruptible
    );
    assert_eq!(
        darwin_thread_state(&[None, Some(4), Some(1)]).unwrap(),
        ProcessState::Running
    );
    assert!(darwin_thread_state(&[]).is_err());
    assert!(darwin_thread_state(&[None, None]).is_err());
    assert!(darwin_thread_state(&[Some(4), Some(99)]).is_err());
}

#[cfg(test)]
#[test]
fn parses_linux_stat_states_with_complex_names() {
    for (state, expected) in [
        ("D", ProcessState::Uninterruptible),
        ("T", ProcessState::Stopped),
        ("t", ProcessState::Stopped),
        ("S", ProcessState::Running),
        ("R", ProcessState::Running),
        ("Z", ProcessState::Gone),
    ] {
        assert_eq!(
            linux_stat_state(&format!("123 (name ) with spaces) {state} 1 2 3")).unwrap(),
            expected
        );
    }
    assert!(linux_stat_state("123 (broken").is_err());
    assert!(linux_stat_state("123 (name) ? 1").is_err());
}

/// No credentials-only fallback exists on other platforms.
pub(crate) fn sealed_platform_euid(pid: i32) -> Option<u32> {
    ignored_unreadable_euid(pid, &[])
}

pub(crate) fn ignored_unreadable_euid(
    pid: i32,
    entries: &[crate::pid::CensusIgnoreEntry],
) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let before = darwin_effective_uid(pid)?;
        let flags = darwin_signing_status(pid)?;
        if !darwin_valid_signature(flags) {
            return None;
        }
        let path = darwin_executable_path(pid)?;
        let executable = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .ok()?;
        if !executable.metadata().ok()?.is_file() {
            return None;
        }
        let builtin = before < 500
            && darwin_platform_signature(flags)
            && darwin_daemon_path(&path)
            && darwin_on_sealed_system_volume(&executable) == Some(true);
        let team = if entries
            .iter()
            .any(|entry| entry.path.as_os_str() == path.as_os_str() && entry.team_id != "platform")
        {
            darwin_team_id(pid)
        } else {
            None
        };
        let listed = listed_image_matches(entries, &path, flags, team.as_deref());
        if (!builtin && !listed)
            || darwin_executable_path(pid)?.as_os_str() != path.as_os_str()
            || darwin_signing_status(pid)? != flags
            || darwin_effective_uid(pid)? != before
            || (listed && team.is_some() && darwin_team_id(pid) != team)
        {
            return None;
        }
        Some(before)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, entries);
        None
    }
}

pub(crate) fn process_details(pid: i32) -> String {
    #[cfg(target_os = "macos")]
    {
        let path = darwin_executable_path(pid)
            .map_or_else(|| "unavailable".into(), |path| format!("{path:?}"));
        let uid =
            darwin_effective_uid(pid).map_or_else(|| "unavailable".into(), |uid| uid.to_string());
        let flags = darwin_signing_status(pid)
            .map_or_else(|| "unavailable".into(), |flags| format!("0x{flags:08x}"));
        format!("path={path}, euid={uid}, cs_flags={flags}")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        "path=unavailable, euid=unavailable, cs_flags=unavailable".into()
    }
}

#[cfg(any(target_os = "macos", test))]
fn darwin_valid_signature(flags: u32) -> bool {
    const CS_VALID: u32 = 0x0000_0001;
    const CS_SIGNED: u32 = 0x2000_0000;
    const UNTRUSTED: u32 = 0x0000_0002 | 0x0000_0004 | 0x0000_0020 | 0x1000_0000;
    let required = CS_VALID | CS_SIGNED;
    flags & required == required && flags & UNTRUSTED == 0
}

#[cfg(any(target_os = "macos", test))]
fn darwin_platform_signature(flags: u32) -> bool {
    // CS_PLATFORM_PATH alone is not signing evidence.
    darwin_valid_signature(flags) && flags & 0x0400_0000 != 0
}

#[cfg(any(target_os = "macos", test))]
fn listed_image_matches(
    entries: &[crate::pid::CensusIgnoreEntry],
    path: &std::path::Path,
    flags: u32,
    team: Option<&str>,
) -> bool {
    darwin_valid_signature(flags)
        && entries.iter().any(|entry| {
            entry.is_valid()
                && entry.path.as_os_str() == path.as_os_str()
                && if entry.team_id == "platform" {
                    darwin_platform_signature(flags)
                } else {
                    team == Some(entry.team_id.as_str())
                }
        })
}

#[cfg(any(target_os = "macos", test))]
fn darwin_daemon_path(path: &std::path::Path) -> bool {
    path.components().all(|part| {
        matches!(
            part,
            std::path::Component::RootDir | std::path::Component::Normal(_)
        )
    }) && ["/sbin", "/usr/sbin", "/usr/libexec", "/System/Library"]
        .iter()
        .any(|directory| path.starts_with(directory) && path != std::path::Path::new(directory))
}

#[cfg(target_os = "macos")]
fn darwin_executable_path(pid: i32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut path = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: path supplies writable storage of the supplied size, not retained.
    let size = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    if size <= 0 {
        return None;
    }
    let end = path.iter().position(|byte| *byte == 0)?;
    let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&path[..end]));
    path.is_absolute().then_some(path)
}

#[cfg(any(target_os = "macos", test))]
fn darwin_sealed_root_volume(flags: u32, valid: u32, capabilities: u32) -> bool {
    // XNU bsd/sys/{mount,attr}.h: read-only root filesystem and a valid seal.
    const REQUIRED_MOUNT: u32 = 0x0000_0001 | 0x0000_4000;
    const SEALED: u32 = 0x0200_0000;
    flags & REQUIRED_MOUNT == REQUIRED_MOUNT && valid & SEALED != 0 && capabilities & SEALED != 0
}

#[cfg(target_os = "macos")]
fn darwin_csops(pid: i32, operation: u32, buffer: &mut [u8]) -> bool {
    unsafe extern "C" {
        fn csops(
            pid: libc::pid_t,
            ops: libc::c_uint,
            buffer: *mut libc::c_void,
            size: usize,
        ) -> libc::c_int;
    }
    // SAFETY: the supplied slice is live writable storage of the declared
    // length. csops copies bytes to userspace and retains no pointer.
    let result = unsafe { csops(pid, operation, buffer.as_mut_ptr().cast(), buffer.len()) };
    result == 0
}

#[cfg(target_os = "macos")]
fn darwin_signing_status(pid: i32) -> Option<u32> {
    let mut flags = [0u8; 4];
    darwin_csops(pid, 0, &mut flags).then(|| u32::from_ne_bytes(flags))
}

#[cfg(target_os = "macos")]
fn darwin_team_id(pid: i32) -> Option<String> {
    const CS_OPS_TEAMID: u32 = 14;
    let mut bytes = [0xff; 8 + 64 + 1];
    if !darwin_csops(pid, CS_OPS_TEAMID, &mut bytes) {
        return None;
    }
    decode_darwin_team_id(&bytes).map(str::to_owned)
}

#[cfg(any(target_os = "macos", test))]
fn decode_darwin_team_id(bytes: &[u8]) -> Option<&str> {
    if bytes.get(..4)? != [0; 4] {
        return None;
    }
    let length = u32::from_be_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
    if !(10..=8 + 64 + 1).contains(&length) {
        return None;
    }
    let payload = bytes.get(8..length)?;
    let (&0, team) = payload.split_last()? else {
        return None;
    };
    if !team.iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    std::str::from_utf8(team).ok()
}

#[cfg(target_os = "macos")]
fn darwin_effective_uid(pid: i32) -> Option<u32> {
    let requested = std::mem::size_of::<libc::proc_bsdshortinfo>() as i32;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::uninit();
    // SAFETY: aligned writable storage has the exact ABI size; no pointer is retained.
    let returned = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            requested,
        )
    };
    if returned != requested {
        return None;
    }
    // SAFETY: the exact-size successful read initialised the whole record.
    let info = unsafe { info.assume_init() };
    (info.pbsi_pid == pid as u32).then_some(info.pbsi_uid)
}

#[cfg(target_os = "macos")]
fn darwin_on_sealed_system_volume(executable: &std::fs::File) -> Option<bool> {
    use std::os::fd::AsRawFd;
    let mut volume = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: volume supplies aligned ABI-sized storage for this borrowed fd.
    if unsafe { libc::fstatfs(executable.as_raw_fd(), volume.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: fstatfs successfully initialised the record.
    let volume = unsafe { volume.assume_init() };
    let required = (libc::MNT_RDONLY | libc::MNT_ROOTFS) as u32;
    if volume.f_flags & required != required {
        return Some(false);
    }
    // Query the executable's mount point, supplied by fstatfs, rather than
    // guessing trust from its pathname (which may traverse a Data firmlink).
    let mut attrs = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    #[repr(C)]
    struct Capabilities {
        length: u32,
        capabilities: libc::vol_capabilities_attr_t,
    }
    let mut capabilities = Capabilities {
        length: 0,
        capabilities: libc::vol_capabilities_attr_t {
            capabilities: [0; 4],
            valid: [0; 4],
        },
    };
    // SAFETY: the kernel's mount-point array is NUL-terminated; attrs and
    // result are aligned ABI-sized storage and no pointers are retained.
    let result = unsafe {
        libc::getattrlist(
            volume.f_mntonname.as_ptr(),
            (&mut attrs as *mut libc::attrlist).cast(),
            (&mut capabilities as *mut Capabilities).cast(),
            std::mem::size_of::<Capabilities>(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    if capabilities.length as usize != std::mem::size_of::<Capabilities>() {
        return None;
    }
    let format = libc::VOL_CAPABILITIES_FORMAT;
    Some(darwin_sealed_root_volume(
        volume.f_flags,
        capabilities.capabilities.valid[format],
        capabilities.capabilities.capabilities[format],
    ))
}

pub(crate) fn signal_process(pid: i32, signal: ProcessSignal) -> io::Result<()> {
    // Keep group signalling impossible even if another internal caller is added.
    if pid <= 0 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let signal = match signal {
        ProcessSignal::Stop => libc::SIGSTOP,
        ProcessSignal::Continue => libc::SIGCONT,
    };
    // SAFETY: pid is positive so kill addresses only that process; signal is
    // one of the two valid constants above. No pointers or ownership transfer.
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn failure(pid: i32, source: io::Error) -> PidError {
    PidError::Io { pid, source }
}
pub(crate) fn cwd(pid: i32) -> Result<CwdObservation, PidError> {
    #[cfg(target_os = "linux")]
    {
        linux_cwd(pid)
    }
    #[cfg(target_os = "macos")]
    {
        darwin_cwd(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        Ok(CwdObservation::Unsupported)
    }
}

#[cfg(target_os = "linux")]
fn linux_cwd(pid: i32) -> Result<CwdObservation, PidError> {
    let mut retries = 0;
    loop {
        match std::fs::read_link(format!("/proc/{pid}/cwd")) {
            Ok(path) if path.is_absolute() => return Ok(CwdObservation::Path(path)),
            Ok(_) => {
                return Err(PidError::MalformedCwdPath {
                    pid,
                    reason: "path is not absolute",
                });
            }
            Err(error)
                if error.kind() == io::ErrorKind::Interrupted && retries < MAX_EINTR_RETRIES =>
            {
                retries += 1;
            }
            Err(error) => return map_cwd_error(pid, error),
        }
    }
}

#[cfg(target_os = "linux")]
fn map_cwd_error(pid: i32, error: io::Error) -> Result<CwdObservation, PidError> {
    match error.raw_os_error() {
        Some(libc::ESRCH | libc::ENOENT) => Ok(CwdObservation::Gone),
        Some(libc::EPERM) => Ok(CwdObservation::PermissionDenied),
        Some(errno) if errno == libc::ENOTSUP || errno == libc::ENOSYS => {
            Ok(CwdObservation::Unsupported)
        }
        _ => Err(failure(pid, error)),
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
enum DarwinCwdCall {
    Gone,
    PermissionDenied,
    Unsupported,
    Retry,
    Record,
}

/// The result of probing a process whose cwd libproc would not report.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DarwinLiveness {
    Alive,
    Gone,
    Unknown,
}

#[cfg(any(target_os = "macos", test))]
fn classify_darwin_result(
    pid: i32,
    size: i32,
    expected: usize,
    error: Option<io::Error>,
    liveness: impl FnOnce() -> DarwinLiveness,
) -> Result<DarwinCwdCall, PidError> {
    if size == 0 {
        // Zero alone proves neither absence nor EPERM. Only verified ESRCH
        // establishes disappearance; only the cwd syscall's EPERM can reach
        // the daemon policy. Production clears errno before the syscall.
        if liveness() == DarwinLiveness::Gone {
            return Ok(DarwinCwdCall::Gone);
        }
        return match error {
            Some(error) if error.raw_os_error() == Some(libc::EPERM) => {
                Ok(DarwinCwdCall::PermissionDenied)
            }
            Some(error) if error.raw_os_error().is_some_and(|errno| errno != 0) => {
                Err(failure(pid, error))
            }
            _ => Err(failure(pid, io::Error::from_raw_os_error(libc::EIO))),
        };
    }
    if size < 0 {
        let error = match error {
            Some(error) => error,
            None => io::Error::from_raw_os_error(libc::EIO),
        };
        match error.raw_os_error() {
            Some(libc::EINTR) => return Ok(DarwinCwdCall::Retry),
            Some(libc::ESRCH) => return Ok(DarwinCwdCall::Gone),
            Some(libc::EPERM) => return Ok(DarwinCwdCall::PermissionDenied),
            Some(errno)
                if errno == libc::ENOTSUP || errno == libc::EOPNOTSUPP || errno == libc::ENOSYS =>
            {
                return Ok(DarwinCwdCall::Unsupported);
            }
            _ => return Err(failure(pid, error)),
        }
    }
    let returned = size as usize;
    if returned != expected {
        return Err(PidError::CwdShortRecord {
            pid,
            returned,
            expected,
        });
    }
    Ok(DarwinCwdCall::Record)
}

#[cfg(any(target_os = "macos", test))]
fn decode_darwin_path(pid: i32, raw: &[libc::c_char]) -> Result<std::path::PathBuf, PidError> {
    use std::os::unix::ffi::OsStringExt;

    let bytes: Vec<u8> = raw.iter().map(|byte| *byte as u8).collect();
    let Some(end) = bytes.iter().position(|byte| *byte == 0) else {
        return Err(PidError::MalformedCwdPath {
            pid,
            reason: "path is not NUL terminated",
        });
    };
    let Some(path) = bytes.get(..end) else {
        return Err(PidError::MalformedCwdPath {
            pid,
            reason: "path range is invalid",
        });
    };
    if path.first() != Some(&b'/') {
        return Err(PidError::MalformedCwdPath {
            pid,
            reason: "path is empty or not absolute",
        });
    }
    Ok(std::ffi::OsString::from_vec(path.to_vec()).into())
}

#[cfg(target_os = "macos")]
fn darwin_liveness(pid: i32) -> DarwinLiveness {
    // Mirror the census rule: zero from libproc is ambiguous, so getsid decides
    // absence. Clear errno first because a failed getsid may inherit stale errno.
    // SAFETY: __error returns this thread's live errno slot; writing zero
    // ensures a failed getsid result cannot inherit stale errno.
    let errno_slot = unsafe { libc::__error() };
    // SAFETY: errno_slot is the live thread-local slot returned immediately
    // above; writing it cannot affect another thread.
    unsafe { *errno_slot = 0 };
    // SAFETY: getsid reads only the scalar PID, retains no pointer, and reports
    // failure through this thread's errno slot.
    let session = unsafe { libc::getsid(pid) };
    // SAFETY: __error returns the same live thread-local slot after the call.
    let errno_slot = unsafe { libc::__error() };
    // SAFETY: errno_slot is the live thread-local slot returned immediately
    // above; only its integer value is read.
    let errno = unsafe { *errno_slot };
    if session > 0 {
        DarwinLiveness::Alive
    } else if session == -1 && errno == libc::ESRCH {
        DarwinLiveness::Gone
    } else {
        // Another failure, including EPERM/EACCES, cannot prove absence.
        DarwinLiveness::Unknown
    }
}

#[cfg(target_os = "macos")]
fn darwin_cwd(pid: i32) -> Result<CwdObservation, PidError> {
    let expected = std::mem::size_of::<libc::proc_vnodepathinfo>();
    let mut retries = 0;
    loop {
        let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        let buffer_size = i32::try_from(expected)
            .map_err(|_| failure(pid, io::Error::from_raw_os_error(libc::EOVERFLOW)))?;
        // SAFETY: __error returns this thread's live errno slot; clearing it
        // prevents a zero libproc result from inheriting stale EPERM.
        let errno_slot = unsafe { libc::__error() };
        // SAFETY: this is the live thread-local slot returned just above.
        unsafe { *errno_slot = 0 };
        // SAFETY: info is aligned writable storage of exactly the requested
        // vnodepathinfo size. proc_pidinfo only borrows it; full success writes
        // all fields. No handles transfer or global state is exposed.
        let size = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast(),
                buffer_size,
            )
        };
        let error = (size <= 0).then(io::Error::last_os_error);
        match classify_darwin_result(pid, size, expected, error, || darwin_liveness(pid))? {
            DarwinCwdCall::Gone => return Ok(CwdObservation::Gone),
            DarwinCwdCall::PermissionDenied => return Ok(CwdObservation::PermissionDenied),
            DarwinCwdCall::Unsupported => return Ok(CwdObservation::Unsupported),
            DarwinCwdCall::Retry if retries < MAX_EINTR_RETRIES => {
                retries += 1;
            }
            DarwinCwdCall::Retry => {
                return Err(failure(pid, io::Error::from(io::ErrorKind::Interrupted)));
            }
            DarwinCwdCall::Record => {
                // SAFETY: classification accepted only a full-size success,
                // which initialises the complete record.
                let info = unsafe { info.assume_init() };
                let raw: Vec<_> = info.pvi_cdir.vip_path.into_iter().flatten().collect();
                return decode_darwin_path(pid, &raw).map(CwdObservation::Path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_listing_requires_exact_path_signer_and_valid_live_signature() {
        use crate::pid::CensusIgnoreEntry;
        let entries = [CensusIgnoreEntry {
            path: "/Library/Daemon".into(),
            team_id: "TEAM123".into(),
        }];
        let path = std::path::Path::new("/Library/Daemon");
        let signed = 0x2000_0001;
        assert!(
            listed_image_matches(&entries, path, signed, Some("TEAM123")),
            "accepted listing"
        );
        assert!(
            !listed_image_matches(&entries, path, signed, Some("TEAM999")),
            "team mismatch"
        );
        assert!(
            !listed_image_matches(
                &entries,
                std::path::Path::new("/Library/Daemon-child"),
                signed,
                Some("TEAM123")
            ),
            "exact path mismatch"
        );
        assert!(
            !listed_image_matches(
                &entries,
                std::path::Path::new("/Library//Daemon"),
                signed,
                Some("TEAM123")
            ),
            "do not normalize a listed path match"
        );
        assert!(
            !listed_image_matches(&entries, path, signed, None),
            "unreadable team"
        );
        for invalid in [
            signed & !1,
            signed | 2,
            signed | 4,
            signed | 0x20,
            signed | 0x1000_0000,
        ] {
            assert!(!listed_image_matches(
                &entries,
                path,
                invalid,
                Some("TEAM123")
            ));
        }
        let platform = [CensusIgnoreEntry {
            path: path.into(),
            team_id: "platform".into(),
        }];
        assert!(!listed_image_matches(
            &platform,
            path,
            signed,
            Some("platform")
        ));
        assert!(listed_image_matches(
            &platform,
            path,
            signed | 0x0400_0000,
            None
        ));
    }

    #[test]
    fn team_id_blob_requires_bounded_network_length_and_exact_nul_terminated_identity() {
        let mut bytes = vec![0; 8];
        bytes.extend_from_slice(b"TEAM123\0");
        let length = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&length.to_be_bytes());
        assert_eq!(decode_darwin_team_id(&bytes), Some("TEAM123"));
        for length in [0u32, 9, 74, u32::MAX] {
            let mut malformed = bytes.clone();
            malformed[4..8].copy_from_slice(&length.to_be_bytes());
            assert_eq!(decode_darwin_team_id(&malformed), None);
        }
        for (index, value) in [(0, 1), (9, 0), (15, 1), (10, 0xff)] {
            let mut malformed = bytes.clone();
            malformed[index] = value;
            assert_eq!(decode_darwin_team_id(&malformed), None);
        }
        assert_eq!(decode_darwin_team_id(&bytes[..7]), None);
        bytes.extend_from_slice(b"IGNORED");
        assert_eq!(decode_darwin_team_id(&bytes), Some("TEAM123"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_current_process_is_not_an_eligible_platform_daemon() {
        assert_eq!(sealed_platform_euid(std::process::id() as i32), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_same_user_sleep_is_not_an_eligible_daemon() -> Result<(), Box<dyn std::error::Error>>
    {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Child(std::process::Command::new("/bin/sleep").arg("30").spawn()?);
        assert!(child.0.try_wait()?.is_none());
        assert_eq!(sealed_platform_euid(child.0.id() as i32), None);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[allow(clippy::print_stderr)] // An optional live off-volume fixture must report its availability.
    fn native_offsealed_platform_process_is_refused_when_present() {
        let mut pids = vec![0i32; 65_536];
        // SAFETY: the vector supplies writable storage of the supplied byte
        // size and libproc does not retain the pointer.
        let count = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr().cast(),
                (pids.len() * std::mem::size_of::<i32>()) as i32,
            )
        };
        if count <= 0 || count as usize >= pids.len() {
            eprintln!(
                "SKIP: optional off-sealed platform fixture: process enumeration unavailable or incomplete"
            );
            return;
        }
        for &pid in &pids[..count as usize] {
            if !darwin_signing_status(pid).is_some_and(darwin_platform_signature) {
                continue;
            }
            let Some(path) = darwin_executable_path(pid) else {
                continue;
            };
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            if darwin_on_sealed_system_volume(&file) != Some(false) {
                continue;
            }
            assert_eq!(
                sealed_platform_euid(pid),
                None,
                "off-sealed platform process {pid}: {path:?}"
            );
            eprintln!(
                "ASSERTED: off-sealed platform process {pid} {path:?} refused by native daemon policy"
            );
            return;
        }
        eprintln!("SKIP: no observable off-sealed platform process is present");
    }

    #[test]
    fn platform_policy_requires_valid_signed_platform_code() {
        let required = 0x2000_0000 | 0x0400_0000 | 1;
        assert!(darwin_platform_signature(required));
        for missing in [1, 0x0400_0000, 0x2000_0000] {
            assert!(!darwin_platform_signature(required & !missing));
        }
        for untrusted in [2, 4, 0x20, 0x1000_0000] {
            assert!(!darwin_platform_signature(required | untrusted));
        }
        assert!(!darwin_platform_signature(0x0800_0001)); // path is not signing evidence
    }

    #[test]
    fn platform_policy_requires_a_valid_seal_on_the_readonly_root_volume() {
        // Real root mounts carry unrelated flags; these must not turn a
        // bitmask requirement into an exact-equality check.
        const MNT_JOURNALED: u32 = 0x0080_0000;
        const MNT_MULTILABEL: u32 = 0x0400_0000;
        let root_mount = 0x4001 | MNT_JOURNALED | MNT_MULTILABEL;
        let seal = 0x0200_0000;
        assert!(darwin_sealed_root_volume(root_mount, seal, seal));
        assert!(
            !darwin_sealed_root_volume(root_mount & !1, seal, seal),
            "writable root"
        );
        assert!(
            !darwin_sealed_root_volume(root_mount & !0x4000, seal, seal),
            "other sealed volume"
        );
        assert!(
            !darwin_sealed_root_volume(root_mount, 0, seal),
            "seal capability not valid"
        );
        assert!(
            !darwin_sealed_root_volume(root_mount, seal, 0),
            "unsealed root"
        );
    }

    #[test]
    fn daemon_paths_allow_system_library_bundles_but_exclude_cli_and_other_apps() {
        for path in [
            "/sbin/launchd",
            "/usr/sbin/daemon",
            "/usr/libexec/daemon",
            "/System/Library/PrivateFrameworks/X/daemon",
            "/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder",
            "/System/Library/X.appex/daemon",
        ] {
            assert!(darwin_daemon_path(std::path::Path::new(path)), "{path}");
        }
        for path in [
            "/bin/sleep",
            "/usr/bin/zsh",
            "/Library/Apple/usr/libexec/usbmuxd",
            "/Applications/X.app/daemon",
            "/System/Applications/X.app/daemon",
            "/usr/libexec-evil/daemon",
            "/usr/libexec/../bin/sleep",
            "/sbin",
            "sbin/daemon",
        ] {
            assert!(!darwin_daemon_path(std::path::Path::new(path)), "{path}");
        }
    }

    #[test]
    fn darwin_zero_result_ignores_stale_errno_and_short_records_are_typed() {
        assert!(matches!(
            classify_darwin_result(
                42,
                0,
                24,
                Some(io::Error::from_raw_os_error(libc::EACCES)),
                || DarwinLiveness::Gone
            ),
            Ok(DarwinCwdCall::Gone)
        ));
        assert!(matches!(
            classify_darwin_result(42, 23, 24, None, || DarwinLiveness::Gone),
            Err(PidError::CwdShortRecord {
                pid: 42,
                returned: 23,
                expected: 24
            })
        ));
    }

    #[test]
    fn darwin_zero_result_needs_a_proven_absence_before_reporting_gone() {
        // A live process whose cwd libproc refuses to report is unplaceable.
        for liveness in [DarwinLiveness::Alive, DarwinLiveness::Unknown] {
            let result = classify_darwin_result(42, 0, 24, None, || liveness);
            assert!(
                matches!(result, Err(PidError::Io { .. })),
                "{liveness:?} returned {result:?}"
            );
        }
        // Only a probe that proves ESRCH reports the process gone.
        assert!(matches!(
            classify_darwin_result(42, 0, 24, None, || DarwinLiveness::Gone),
            Ok(DarwinCwdCall::Gone)
        ));
    }

    #[test]
    fn only_actual_eperm_can_reach_the_daemon_exception() {
        for size in [0, -1] {
            assert!(matches!(
                classify_darwin_result(
                    42,
                    size,
                    24,
                    Some(io::Error::from_raw_os_error(libc::EPERM)),
                    || DarwinLiveness::Alive
                ),
                Ok(DarwinCwdCall::PermissionDenied)
            ));
            for errno in [libc::EACCES, libc::EIO, libc::EINVAL] {
                assert!(matches!(
                    classify_darwin_result(
                        42,
                        size,
                        24,
                        Some(io::Error::from_raw_os_error(errno)),
                        || DarwinLiveness::Alive
                    ),
                    Err(PidError::Io { .. })
                ));
            }
        }
        assert!(matches!(
            classify_darwin_result(42, 0, 24, Some(io::Error::from_raw_os_error(0)), || {
                DarwinLiveness::Alive
            }),
            Err(PidError::Io { .. })
        ));
    }

    #[test]
    fn darwin_cwd_classifies_permission_unsupported_and_malformed_paths() {
        assert!(matches!(
            classify_darwin_result(
                42,
                -1,
                24,
                Some(io::Error::from_raw_os_error(libc::EPERM)),
                || DarwinLiveness::Alive
            ),
            Ok(DarwinCwdCall::PermissionDenied)
        ));
        assert!(matches!(
            classify_darwin_result(
                42,
                -1,
                24,
                Some(io::Error::from_raw_os_error(libc::ENOTSUP)),
                || DarwinLiveness::Alive
            ),
            Ok(DarwinCwdCall::Unsupported)
        ));
        let malformed = [b'x' as libc::c_char, 0];
        assert!(matches!(
            decode_darwin_path(42, &malformed),
            Err(PidError::MalformedCwdPath { pid: 42, .. })
        ));
    }
}
