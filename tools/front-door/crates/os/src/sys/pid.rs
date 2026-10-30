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

pub(crate) fn uid(pid: i32) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        // /proc/PID ownership can change to root for a nondumpable process;
        // status instead reports its real, effective, saved and filesystem UIDs.
        const LIMIT: u64 = 65_536;
        let file = std::fs::File::open(format!("/proc/{pid}/status")).ok()?;
        let mut status = String::new();
        file.take(LIMIT + 1).read_to_string(&mut status).ok()?;
        if status.len() as u64 > LIMIT {
            return None;
        }
        linux_status_uid(&status)
    }
    #[cfg(target_os = "macos")]
    {
        let requested = i32::try_from(std::mem::size_of::<libc::proc_bsdshortinfo>()).ok()?;
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::uninit();
        // SAFETY: info is aligned writable storage of the exact ABI size.
        // libproc borrows it for this call and retains no pointer.
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
        (info.pbsi_pid == pid as u32
            && info.pbsi_uid == info.pbsi_ruid
            && info.pbsi_uid == info.pbsi_svuid)
            .then_some(info.pbsi_uid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(any(target_os = "linux", test))]
fn linux_status_uid(status: &str) -> Option<u32> {
    let mut records = status.lines().filter_map(|line| line.strip_prefix("Uid:"));
    let record = records.next()?;
    if records.next().is_some() {
        return None;
    }
    let mut fields = record.split_whitespace();
    let uid = fields.next()?.parse::<u32>().ok()?;
    for _ in 0..3 {
        if fields.next()?.parse::<u32>().ok()? != uid {
            return None;
        }
    }
    fields.next().is_none().then_some(uid)
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
        Some(libc::EPERM | libc::EACCES) => Ok(CwdObservation::PermissionDenied),
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

#[cfg(any(target_os = "macos", test))]
fn classify_darwin_result(
    pid: i32,
    size: i32,
    expected: usize,
    error: Option<io::Error>,
) -> Result<DarwinCwdCall, PidError> {
    if size == 0 {
        // proc_pidinfo uses zero to mean the process is gone. errno may be stale.
        return Ok(DarwinCwdCall::Gone);
    }
    if size < 0 {
        let error = match error {
            Some(error) => error,
            None => io::Error::from_raw_os_error(libc::EIO),
        };
        match error.raw_os_error() {
            Some(libc::EINTR) => return Ok(DarwinCwdCall::Retry),
            Some(libc::ESRCH) => return Ok(DarwinCwdCall::Gone),
            Some(libc::EPERM | libc::EACCES) => return Ok(DarwinCwdCall::PermissionDenied),
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
fn darwin_cwd(pid: i32) -> Result<CwdObservation, PidError> {
    let expected = std::mem::size_of::<libc::proc_vnodepathinfo>();
    let mut retries = 0;
    loop {
        let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        let buffer_size = i32::try_from(expected)
            .map_err(|_| failure(pid, io::Error::from_raw_os_error(libc::EOVERFLOW)))?;
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
        let error = (size < 0).then(io::Error::last_os_error);
        match classify_darwin_result(pid, size, expected, error)? {
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
    fn uid_observation_requires_complete_matching_credentials() {
        assert_eq!(
            linux_status_uid("Name:\twriter\nUid:\t1000\t1000\t1000\t1000\n"),
            Some(1000)
        );
        for status in [
            "",
            "Uid:\t1000\t1000\t1000",
            "Uid:\t1000\t1001\t1000\t1000",
            "Uid:\t1000\t1000\t0\t1000",
            "Uid:\t1000\t1000\t1000\t0",
            "Uid:\t1000\t1000\t1000\t1000\t1000",
            "Uid:\t1000\t1000\tinvalid\t1000",
            "Uid:\t1000\t1000\t1000\t1000\nUid:\t1000\t1000\t1000\t1000",
        ] {
            assert_eq!(linux_status_uid(status), None, "{status}");
        }
    }

    #[test]
    fn darwin_zero_result_ignores_stale_errno_and_short_records_are_typed() {
        assert!(matches!(
            classify_darwin_result(42, 0, 24, Some(io::Error::from_raw_os_error(libc::EACCES))),
            Ok(DarwinCwdCall::Gone)
        ));
        assert!(matches!(
            classify_darwin_result(42, 23, 24, None),
            Err(PidError::CwdShortRecord {
                pid: 42,
                returned: 23,
                expected: 24
            })
        ));
    }

    #[test]
    fn darwin_cwd_classifies_permission_unsupported_and_malformed_paths() {
        assert!(matches!(
            classify_darwin_result(42, -1, 24, Some(io::Error::from_raw_os_error(libc::EPERM))),
            Ok(DarwinCwdCall::PermissionDenied)
        ));
        assert!(matches!(
            classify_darwin_result(
                42,
                -1,
                24,
                Some(io::Error::from_raw_os_error(libc::ENOTSUP))
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
