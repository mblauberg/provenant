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
    /// Native permission denial: EPERM on macOS, EPERM or EACCES on Linux.
    PermissionDenied,
    Unsupported,
}

/// Why an occupancy observation could not prove non-occupancy. A caller may
/// scope out uncertainty under an explicit process trust policy; positive
/// paths remain occupancy evidence regardless of the gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OccupancyGap {
    /// Every failed query was a descriptor query refused with EPERM; the
    /// executable and each readable descriptor are in `paths`.
    DescriptorsDenied,
    /// The executable could not be identified or inspected.
    ExecutableUnavailable,
    /// The descriptor table or a descriptor could not be read (not EPERM).
    DescriptorsUnavailable,
    /// The table changed between the two snapshots.
    Churn,
    /// The table was larger than the bounded buffer.
    LimitExceeded,
    /// The process could not be observed at all.
    Unobserved,
}

impl OccupancyGap {
    pub fn describe(self) -> &'static str {
        match self {
            Self::DescriptorsDenied => "descriptors unreadable (EPERM)",
            Self::ExecutableUnavailable => "executable unavailable",
            Self::DescriptorsUnavailable => "descriptors unavailable",
            Self::Churn => "descriptors or executable changed during the census",
            Self::LimitExceeded => "descriptor limit exceeded",
            Self::Unobserved => "process not observed",
        }
    }

    /// The more serious of two gaps. A permission refusal ranks below other
    /// failures, so it cannot hide a missing path or unstable observation.
    pub fn worse(self, other: Self) -> Self {
        if self == Self::DescriptorsDenied {
            other
        } else {
            self
        }
    }
}

/// Paths positively observed for an executable and filesystem descriptors.
/// A partial observation retains its paths but cannot establish non-occupancy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OccupancyObservation {
    pub paths: Vec<PathBuf>,
    pub complete: bool,
    /// Why the observation is incomplete; None when complete or not attempted.
    pub gap: Option<OccupancyGap>,
}

/// An executable on the sealed system volume, found through the root-owned,
/// non-writable path check. This supports a caller's risk policy; it is not
/// proof of non-occupancy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SealedSystemImage {
    /// A root-owned setuid file whose process runs with a different real uid:
    /// the kernel refuses its cwd because of the privilege transition.
    pub setuid_transition: bool,
}

/// Host-owner acceptance of the risk that this signed executable occupies a
/// worktree despite an unreadable cwd. Paths and signers are exact matches.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CensusIgnoreEntry {
    pub path: PathBuf,
    /// `platform` for Apple platform code, otherwise the kernel team identifier.
    pub team_id: String,
}

/// Credentials read together from the kernel, never inferred from effective uid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessUids {
    pub real: u32,
    pub effective: u32,
    pub saved: u32,
}

impl ProcessUids {
    pub fn all_system(self) -> bool {
        [self.real, self.effective, self.saved]
            .into_iter()
            .all(|uid| uid < 500)
    }

    pub fn all_differ_from(self, owner: u32) -> bool {
        [self.real, self.effective, self.saved]
            .into_iter()
            .all(|uid| uid != owner)
    }
}

/// A refusal captured from the image observations that decided policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnreadablePolicyFailure {
    pub condition: &'static str,
    /// None means the built-in checks passed, even if a listing-specific check
    /// failed. Native acceptance must reject an unplaceable image in that case.
    pub builtin_condition: Option<&'static str>,
    /// True only when a system-uid process failed on its executable path or
    /// volume alone, which a signed host entry could excuse.
    pub configurable: bool,
}

/// Evidence accepted for a process whose cwd query was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnreadablePolicyExcusal {
    pub uids: ProcessUids,
}

impl From<&'static str> for UnreadablePolicyFailure {
    fn from(condition: &'static str) -> Self {
        Self {
            condition,
            builtin_condition: Some(condition),
            configurable: false,
        }
    }
}

impl CensusIgnoreEntry {
    pub fn is_valid(&self) -> bool {
        let path = &self.path;
        path.is_absolute()
            && path.file_name().is_some()
            && path.as_os_str().len() <= 4096
            && path
                .to_str()
                .is_some_and(|text| !text.chars().any(char::is_control))
            && path.components().all(|part| {
                matches!(
                    part,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
            && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
            && !self.team_id.is_empty()
            && self.team_id.len() <= 64
            && self
                .team_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric())
    }
}

pub trait CwdObserver: Send + Sync {
    fn cwd(&self, pid: i64) -> Result<CwdObservation, PidError>;

    /// All kernel credentials, read together. Missing evidence never excludes
    /// a process from a caller's writer cohort.
    fn process_uids(&self, _pid: i64) -> Option<ProcessUids> {
        None
    }

    /// Unimplemented observers must not accidentally prove non-occupancy.
    fn occupancy(&self, _pid: i64) -> OccupancyObservation {
        OccupancyObservation::default()
    }

    /// Deadline-bound occupancy; native implementations poll during descriptor reads.
    fn occupancy_while(&self, pid: i64, keep_going: &dyn Fn() -> bool) -> OccupancyObservation {
        if !keep_going() {
            return OccupancyObservation::default();
        }
        let observation = self.occupancy(pid);
        if keep_going() {
            observation
        } else {
            OccupancyObservation::default()
        }
    }

    /// Retry transient gaps within the caller's budget, retaining every path
    /// found on earlier attempts as positive occupancy evidence.
    fn occupancy_with_retries_while(
        &self,
        pid: i64,
        keep_going: &dyn Fn() -> bool,
    ) -> OccupancyObservation {
        crate::sys::pid::occupancy_with_retries_while(
            || self.occupancy_while(pid, keep_going),
            keep_going,
        )
    }

    /// Executable image of the process, if known.
    fn executable(&self, _pid: i64) -> Option<PathBuf> {
        None
    }

    /// The process's executable on the sealed system volume, or None when any
    /// evidence is missing. Missing evidence never grants an exception.
    fn sealed_system_image(&self, _pid: i64) -> Option<SealedSystemImage> {
        None
    }

    /// A Linux kernel thread, which holds no files or cwd in any worktree.
    fn kernel_thread(&self, _pid: i64) -> bool {
        false
    }

    /// The caller must first observe an actual permission denial. Missing
    /// evidence never grants an exception.
    fn unreadable_policy_uids(
        &self,
        _pid: i64,
        _entries: &[CensusIgnoreEntry],
    ) -> Result<UnreadablePolicyExcusal, UnreadablePolicyFailure> {
        Err("image policy evidence unavailable".into())
    }

    /// Diagnostic only; failure to read any field cannot grant the exception.
    fn process_details(&self, _pid: i64) -> Option<String> {
        None
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCwdObserver;
impl CwdObserver for SystemCwdObserver {
    fn process_uids(&self, pid: i64) -> Option<ProcessUids> {
        let pid = i32::try_from(pid).ok().filter(|pid| *pid > 0)?;
        crate::sys::pid::process_uids(pid)
    }

    fn occupancy(&self, pid: i64) -> OccupancyObservation {
        let Some(pid) = i32::try_from(pid).ok().filter(|pid| *pid > 0) else {
            return OccupancyObservation::default();
        };
        crate::sys::pid::occupancy(pid)
    }

    fn occupancy_while(&self, pid: i64, keep_going: &dyn Fn() -> bool) -> OccupancyObservation {
        let Some(pid) = i32::try_from(pid).ok().filter(|pid| *pid > 0) else {
            return OccupancyObservation::default();
        };
        crate::sys::pid::occupancy_while(pid, keep_going)
    }

    fn executable(&self, pid: i64) -> Option<PathBuf> {
        let pid = i32::try_from(pid).ok().filter(|pid| *pid > 0)?;
        crate::sys::pid::executable_path(pid)
    }

    fn sealed_system_image(&self, pid: i64) -> Option<SealedSystemImage> {
        let pid = i32::try_from(pid).ok().filter(|pid| *pid > 0)?;
        crate::sys::pid::sealed_system_image(pid)
    }

    fn kernel_thread(&self, pid: i64) -> bool {
        i32::try_from(pid)
            .ok()
            .filter(|pid| *pid > 0)
            .is_some_and(crate::sys::pid::kernel_thread)
    }

    fn unreadable_policy_uids(
        &self,
        pid: i64,
        entries: &[CensusIgnoreEntry],
    ) -> Result<UnreadablePolicyExcusal, UnreadablePolicyFailure> {
        let pid = i32::try_from(pid)
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or("positive OS pid unavailable")?;
        crate::sys::pid::unreadable_policy_uids(pid, entries)
    }

    fn process_details(&self, pid: i64) -> Option<String> {
        let pid = i32::try_from(pid).ok().filter(|pid| *pid > 0)?;
        Some(crate::sys::pid::process_details(pid))
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn kernel_credentials_are_complete_and_invalid_pids_supply_no_evidence() {
        let uids = SystemCwdObserver
            .process_uids(i64::from(std::process::id()))
            .unwrap();
        assert_eq!(uids.effective, crate::fs::effective_uid());
        for pid in [0, -1, i64::MAX] {
            assert_eq!(SystemCwdObserver.process_uids(pid), None);
        }
    }

    #[test]
    fn all_three_uids_must_be_system_accounts_and_foreign_to_the_owner() {
        let windowserver = ProcessUids {
            real: 0,
            effective: 88,
            saved: 0,
        };
        assert!(windowserver.all_system());
        assert!(windowserver.all_differ_from(501));
        for uids in [
            ProcessUids {
                real: 88,
                effective: 0,
                saved: 0,
            },
            ProcessUids {
                real: 0,
                effective: 88,
                saved: 0,
            },
            ProcessUids {
                real: 0,
                effective: 0,
                saved: 88,
            },
        ] {
            assert!(uids.all_system());
            assert!(!uids.all_differ_from(88));
        }
        for uids in [
            ProcessUids {
                real: 500,
                effective: 0,
                saved: 0,
            },
            ProcessUids {
                real: 0,
                effective: 500,
                saved: 0,
            },
            ProcessUids {
                real: 0,
                effective: 0,
                saved: 500,
            },
        ] {
            assert!(!uids.all_system());
        }
    }

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
            assert!(SystemCwdObserver.unreadable_policy_uids(pid, &[]).is_err());
        }
    }
}
