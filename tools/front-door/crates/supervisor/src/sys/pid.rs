use crate::identity::Signal;
use crate::pid::{CwdObservation, PidError, PidResult};
use std::io;

fn failure(pid: i32, source: io::Error) -> PidError {
    PidError::Io { pid, source }
}
pub(crate) fn signal(pid: i32, signal: Option<Signal>) -> Result<PidResult, PidError> {
    // Defence in depth: this function must never turn a PID into group authority.
    if pid <= 1 {
        return Err(PidError::Refused {
            pid: i64::from(pid),
        });
    }
    let probe = signal.is_none();
    let signal = match signal {
        None => 0,
        Some(Signal::Terminate) => libc::SIGTERM,
        Some(Signal::Kill) => libc::SIGKILL,
    };
    loop {
        // SAFETY: positive single PID comes from the explicit caller grant; its
        // lifetime/custody contract prevents reuse. kill retains no resources
        // or pointers; errno is thread-local, concurrent calls are independent.
        if unsafe { libc::kill(pid, signal) } == 0 {
            return Ok(if probe {
                PidResult::Alive
            } else {
                PidResult::Delivered
            });
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ESRCH) => return Ok(PidResult::Gone),
            Some(libc::EPERM) => return Ok(PidResult::PermissionDenied),
            _ => return Err(failure(pid, io::Error::last_os_error())),
        }
    }
}
pub(crate) fn cwd(pid: i32) -> Result<CwdObservation, PidError> {
    #[cfg(target_os = "linux")]
    let path = std::fs::read_link(format!("/proc/{pid}/cwd"));
    #[cfg(target_os = "macos")]
    let path = darwin_cwd(pid);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        return Ok(CwdObservation::Unsupported);
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    match path {
        Ok(path) => Ok(CwdObservation::Path(path)),
        Err(error) => match error.raw_os_error() {
            Some(libc::ESRCH | libc::ENOENT) => Ok(CwdObservation::Gone),
            Some(libc::EPERM | libc::EACCES) => Ok(CwdObservation::PermissionDenied),
            _ => Err(failure(pid, error)),
        },
    }
}
#[cfg(target_os = "macos")]
fn darwin_cwd(pid: i32) -> io::Result<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    loop {
        let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        // SAFETY: info is aligned writable storage of exactly the requested
        // vnodepathinfo size. proc_pidinfo only borrows it; full success writes
        // all fields. No handles transfer or global state is exposed.
        let size = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast(),
                std::mem::size_of::<libc::proc_vnodepathinfo>() as i32,
            )
        };
        if size <= 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if size as usize != std::mem::size_of::<libc::proc_vnodepathinfo>() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short proc_pidinfo cwd record",
            ));
        }
        // SAFETY: the full-size successful call initialised the record. Copy
        // fixed-array path bytes without assuming libc supplied a terminator.
        let info = unsafe { info.assume_init() };
        let raw: Vec<_> = info.pvi_cdir.vip_path.into_iter().flatten().collect();
        let end = raw
            .iter()
            .position(|&byte| byte == 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unterminated cwd"))?;
        return Ok(std::ffi::OsString::from_vec(
            raw[..end].iter().map(|&byte| byte as u8).collect(),
        )
        .into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_signal_rejects_group_and_init_targets_before_syscall() {
        for pid in [-100, -1, 0, 1] {
            for signal_value in [None, Some(Signal::Terminate), Some(Signal::Kill)] {
                assert!(matches!(
                    signal(pid, signal_value),
                    Err(PidError::Refused { .. })
                ));
            }
        }
        assert_eq!(signal(i32::MAX, None).unwrap(), PidResult::Gone);
    }
}
