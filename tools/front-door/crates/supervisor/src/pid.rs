//! Per-PID signal authority backed by an owning child, never a census PID.
use crate::exec::Termination;
use crate::identity::Signal;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Mutex, MutexGuard};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PidResult {
    Delivered,
    Alive,
    Gone,
    PermissionDenied,
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
    #[error("PID {pid} custody mutex is poisoned")]
    CustodyPoisoned { pid: i32 },
}
/// Only consuming a Child grants signal authority. An unreaped direct child
/// reserves its PID, and the mutex serializes observation and signalling.
/// This is a utility API, not S2 provider custody. Drop never signals: callers
/// retain their explicit wait/cleanup responsibility and must not abandon it.
#[derive(Debug)]
pub struct OwnedPid {
    child: Mutex<Child>,
    pid: i32,
}
impl OwnedPid {
    pub fn from_child(child: Child) -> Result<Self, PidError> {
        let pid = i64::from(child.id());
        if pid <= 1 {
            return Err(PidError::Refused { pid });
        }
        let pid = i32::try_from(pid).map_err(|_| PidError::NumberOutOfRange { pid })?;
        Ok(Self {
            child: Mutex::new(child),
            pid,
        })
    }
    pub fn pid(&self) -> i32 {
        self.pid
    }
    fn custody(&self) -> Result<MutexGuard<'_, Child>, PidError> {
        self.child
            .lock()
            .map_err(|_| PidError::CustodyPoisoned { pid: self.pid })
    }
    /// Nonblocking observation. Cached terminal status never permits another
    /// signal, even after the numeric PID has been recycled by the OS.
    pub fn try_wait(&self) -> Result<Option<Termination>, PidError> {
        self.custody()?
            .try_wait()
            .map(|status| {
                status.map(|status| match status.code() {
                    Some(code) => Termination::Exit(code),
                    None => Termination::Signal(status.signal().unwrap_or(0)),
                })
            })
            .map_err(|source| PidError::Io {
                pid: self.pid,
                source,
            })
    }
}
/// Implement this for fake owned-process handles; callers need no OS Child.
pub trait PidAuthority: Send + Sync {
    fn signal(&self, signal: Signal) -> Result<PidResult, PidError>;
    fn probe(&self) -> Result<PidResult, PidError>;
}
impl PidAuthority for OwnedPid {
    fn signal(&self, signal: Signal) -> Result<PidResult, PidError> {
        let mut child = self.custody()?;
        if child
            .try_wait()
            .map_err(|source| PidError::Io {
                pid: self.pid,
                source,
            })?
            .is_some()
        {
            return Ok(PidResult::Gone);
        }
        crate::sys::pid::signal(self.pid, Some(signal))
    }
    fn probe(&self) -> Result<PidResult, PidError> {
        let mut child = self.custody()?;
        if child
            .try_wait()
            .map_err(|source| PidError::Io {
                pid: self.pid,
                source,
            })?
            .is_some()
        {
            return Ok(PidResult::Gone);
        }
        crate::sys::pid::signal(self.pid, None)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CwdObservation {
    Path(PathBuf),
    Gone,
    PermissionDenied,
    Unsupported,
}
pub trait CwdObserver: Send + Sync {
    fn cwd(&self, pid: i64) -> Result<CwdObservation, PidError>;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCwdObserver;
impl CwdObserver for SystemCwdObserver {
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
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
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
    }
    #[test]
    fn signals_owned_children_only_and_reaped_status_fences_recycled_pids() {
        for signal in [Signal::Terminate, Signal::Kill] {
            let child = Command::new("/bin/cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            let owned = OwnedPid::from_child(child).unwrap();
            assert_eq!(owned.probe().unwrap(), PidResult::Alive);
            assert_eq!(owned.signal(signal).unwrap(), PidResult::Delivered);
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(status) = owned.try_wait().unwrap() {
                    assert!(matches!(status, Termination::Signal(_)));
                    break;
                }
                assert!(Instant::now() < deadline, "owned child did not exit");
                std::thread::yield_now();
            }
            assert_eq!(owned.signal(Signal::Kill).unwrap(), PidResult::Gone);
            assert_eq!(owned.probe().unwrap(), PidResult::Gone);
        }
    }
}
