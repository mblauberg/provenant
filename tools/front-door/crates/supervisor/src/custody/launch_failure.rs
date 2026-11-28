//! How far a failed launch got, and the never-spawned exit it may owe.

use super::{
    CustodyError, EXIT_LIMIT, ProcessExit, Publication, PublicationReservation, PublishMode, Result,
};
use std::ffi::OsStr;
use thiserror::Error;

/// How far a launch progressed on one lease. Only a possible handoff leaves
/// the outcome unknown: a launch refused before any guardian records a
/// never-spawned exit and has nothing to fence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchPhase {
    /// No launch was attempted; the reservation is still prepared.
    Unlaunched,
    /// Launch refused before contacting a guardian (cancelled, or an invalid
    /// capture plan). The reservation records `NeverSpawned` and the failure
    /// carries the never-spawned exit for the attempt.
    NeverSpawned,
    /// A guardian may hold custody; reconcile owns the outcome.
    HandedOff,
}

impl LaunchPhase {
    /// No guardian was contacted, so the lease may be withdrawn.
    pub fn withdrawable(self) -> bool {
        !matches!(self, Self::HandedOff)
    }
}

#[derive(Debug, Error)]
#[error("{error}")]
pub struct LaunchFailure {
    /// The cause. For a never-spawned launch this is the refusal itself
    /// (cancellation or the capture plan), never a publication error.
    pub error: Box<CustodyError>,
    pub custody: PublicationReservation,
    pub(super) pending_exit: Option<Box<ProcessExit>>,
}

impl LaunchFailure {
    /// Publish (or confirm) the bounded never-spawned exit while retaining
    /// attempt custody. Idempotent; conflicting terminal bytes are never replaced.
    pub fn retry_exit_publication(&self) -> Result<Publication> {
        let record = self
            .pending_exit
            .as_ref()
            .ok_or_else(|| CustodyError::HandoffUnknown("no exit pending".into()))?;
        let bytes = serde_json::to_vec(record).map_err(|_| CustodyError::InvalidRecord)?;
        if bytes.len() as u64 > EXIT_LIMIT {
            return Err(CustodyError::InvalidRecord);
        }
        let publish = || {
            self.custody.directory.atomic_write(
                OsStr::new("process-exit.json"),
                ".custody-",
                &bytes,
                PublishMode::NoReplace,
            )
        };
        let mut result = publish();
        if result
            .as_ref()
            .is_err_and(|error| error.raw_os_error() == Some(provenant_os::sys::errno::ENOSPC))
        {
            match self.custody.directory.unlink(OsStr::new("reserve")) {
                Ok(()) => {
                    let _ = self.custody.directory.sync();
                    result = publish();
                }
                Err(error) if error.is_not_found() => {
                    result = publish();
                }
                Err(_) => {}
            }
        }
        match result {
            Ok(publication) => Ok(publication),
            Err(error) if error.is_exists() => {
                let previous = self
                    .custody
                    .directory
                    .read_bounded(OsStr::new("process-exit.json"), EXIT_LIMIT)?;
                if previous != bytes {
                    return Err(CustodyError::InvalidRecord);
                }
                Ok(match self.custody.directory.sync() {
                    Ok(()) => Publication::Durable,
                    Err(sync_error) => Publication::VisibleDurabilityUnconfirmed { sync_error },
                })
            }
            Err(error) => Err(error.into()),
        }
    }
}
