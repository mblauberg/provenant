//! Opaque utility handles and diagnostic CWD; observations grant no authority.
use std::io;
use std::path::PathBuf;
use thiserror::Error;

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
pub use crate::sys::raw_exec::OwnedPid;

#[cfg(test)]
mod tests {
    use super::*;
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
}
