//! Diagnostic CWD observations and explicit process stop/resume operations.
use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// An observed process state. Errors remain distinct from a vanished process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState {
    Running,
    Stopped,
    Uninterruptible,
    Gone,
}

pub fn process_state(pid: i64) -> Result<ProcessState, PidError> {
    if pid <= 0 {
        return Err(PidError::Refused { pid });
    }
    let pid = i32::try_from(pid).map_err(|_| PidError::NumberOutOfRange { pid })?;
    crate::sys::pid::process_state(pid).map_err(|source| PidError::Io { pid, source })
}

#[derive(Debug, Error)]
pub enum PidError {
    #[error("refusing PID {pid}")]
    Refused { pid: i64 },
    #[error("PID {pid} is outside the OS PID range")]
    NumberOutOfRange { pid: i64 },
    #[error("PID {pid} operation failed")]
    Io {
        pid: i32,
        #[source]
        source: io::Error,
    },
    #[error("PID {pid} CWD record was short ({returned} bytes; expected {expected})")]
    CwdShortRecord {
        pid: i32,
        returned: usize,
        expected: usize,
    },
    #[error("PID {pid} CWD path record is malformed: {reason}")]
    MalformedCwdPath { pid: i32, reason: &'static str },
}

/// Signals used by disk pressure protection. Callers must establish which
/// processes they are authorised to pause or resume before invoking this API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessSignal {
    Stop,
    Continue,
}

/// Signal one positive PID, never a process group. Returns false if the PID no
/// longer exists; permissions and other syscall failures remain errors.
pub fn signal_process(pid: i64, signal: ProcessSignal) -> Result<bool, PidError> {
    signal_process_with(pid, signal, crate::sys::pid::signal_process)
}

fn signal_process_with(
    pid: i64,
    signal: ProcessSignal,
    call: impl FnOnce(i32, ProcessSignal) -> io::Result<()>,
) -> Result<bool, PidError> {
    if pid <= 0 {
        return Err(PidError::Refused { pid });
    }
    let pid = i32::try_from(pid).map_err(|_| PidError::NumberOutOfRange { pid })?;
    match call(pid, signal) {
        Ok(()) => Ok(true),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(false),
        Err(source) => Err(PidError::Io { pid, source }),
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CwdObservation {
    Path(PathBuf),
    Gone,
    /// The cwd syscall failed with EPERM; other failures remain errors.
    PermissionDenied,
    Unsupported,
}
pub trait CwdObserver: Send + Sync {
    fn cwd(&self, pid: i64) -> Result<CwdObservation, PidError>;

    /// System effective uid only after kernel signing status, pinned daemon
    /// image and sealed system-volume checks. Missing evidence returns None.
    /// This supports a caller's explicit risk policy, not non-occupancy proof.
    fn sealed_platform_euid(&self, _pid: i64) -> Option<u32> {
        None
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCwdObserver;
impl CwdObserver for SystemCwdObserver {
    fn sealed_platform_euid(&self, pid: i64) -> Option<u32> {
        let pid = i32::try_from(pid).ok().filter(|pid| *pid > 0)?;
        crate::sys::pid::sealed_platform_euid(pid)
    }

    fn cwd(&self, pid: i64) -> Result<CwdObservation, PidError> {
        if pid <= 0 {
            return Err(PidError::Refused { pid });
        }
        let pid = i32::try_from(pid).map_err(|_| PidError::NumberOutOfRange { pid })?;
        crate::sys::pid::cwd(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_signals_validate_pid_before_calling_the_syscall_boundary() {
        for pid in [i64::MIN, -1, 0, i64::from(i32::MAX) + 1, i64::MAX] {
            let result = signal_process_with(pid, ProcessSignal::Stop, |_, _| {
                panic!("invalid PID reached signal syscall")
            });
            assert!(result.is_err());
        }
        for signal in [ProcessSignal::Stop, ProcessSignal::Continue] {
            assert!(
                signal_process_with(42, signal, |pid, actual| {
                    assert_eq!(pid, 42);
                    assert_eq!(actual, signal);
                    Ok(())
                })
                .unwrap()
            );
        }
    }

    #[test]
    fn process_signals_distinguish_gone_processes_from_syscall_failures() {
        assert!(
            !signal_process_with(42, ProcessSignal::Continue, |_, _| {
                Err(io::Error::from_raw_os_error(libc::ESRCH))
            })
            .unwrap()
        );
        for errno in [libc::EPERM, libc::EINVAL] {
            let error = signal_process_with(42, ProcessSignal::Stop, |_, _| {
                Err(io::Error::from_raw_os_error(errno))
            })
            .unwrap_err();
            assert!(
                matches!(error, PidError::Io { pid: 42, source } if source.raw_os_error() == Some(errno))
            );
        }
    }

    #[test]
    fn observes_this_cwd_without_granting_signal_authority_to_the_observer() {
        let CwdObservation::Path(path) = SystemCwdObserver
            .cwd(i64::from(std::process::id()))
            .unwrap()
        else {
            panic!("cwd missing")
        };
        assert_eq!(
            std::fs::canonicalize(path).unwrap(),
            std::fs::canonicalize(".").unwrap()
        );
        assert!(matches!(
            SystemCwdObserver.cwd(i64::MAX),
            Err(PidError::NumberOutOfRange { .. })
        ));
        assert!(matches!(
            SystemCwdObserver.cwd(0),
            Err(PidError::Refused { .. })
        ));
        for pid in [-1, 0, i64::MAX] {
            assert_eq!(SystemCwdObserver.sealed_platform_euid(pid), None);
        }
    }
}
