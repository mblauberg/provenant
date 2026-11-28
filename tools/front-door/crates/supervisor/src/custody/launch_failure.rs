//! How far a failed launch got, the never-spawned exit it may owe, and how
//! that exit is recorded before custody is withdrawn.

use super::{
    CustodyError, CustodyLease, EXIT_LIMIT, LaunchState, ProcessExit, Publication,
    PublicationReservation, PublishMode, Recovery, Result, SettlementEvidence, StopReason,
    StoredExit, read_record, write_record,
};
use provenant_core::CancelToken;
use provenant_os::fs::Directory;
use std::ffi::OsStr;
use thiserror::Error;

/// How far a launch progressed on one lease. Only a possible handoff leaves
/// the outcome unknown: a launch no guardian accepted records a never-spawned
/// exit and has nothing to fence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchPhase {
    /// No launch was attempted; the reservation is still prepared.
    Unlaunched,
    /// Launch refused before any guardian held the whole launch message
    /// (cancelled, an invalid capture plan or command, a guardian that failed
    /// to spawn or closed the channel mid-message) or the guardian refused it
    /// before accepting custody. The reservation records `NeverSpawned` and the
    /// failure carries the never-spawned exit for the attempt.
    NeverSpawned,
    /// A guardian holds the launch message and may hold custody; reconcile
    /// owns the outcome.
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
        publish_never_spawned_exit(&self.custody.directory, record)
    }
}

/// Publish (or confirm) a bounded never-spawned exit in the attempt
/// directory. Idempotent; conflicting terminal bytes are never replaced. A
/// full disk releases the attempt's reserve once and retries.
pub(super) fn publish_never_spawned_exit(
    directory: &Directory,
    record: &ProcessExit,
) -> Result<Publication> {
    let bytes = serde_json::to_vec(record).map_err(|_| CustodyError::InvalidRecord)?;
    if bytes.len() as u64 > EXIT_LIMIT {
        return Err(CustodyError::InvalidRecord);
    }
    let publish = || {
        directory.atomic_write(
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
        match directory.unlink(OsStr::new("reserve")) {
            Ok(()) => {
                let _ = directory.sync();
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
            let previous = directory.read_bounded(OsStr::new("process-exit.json"), EXIT_LIMIT)?;
            if previous != bytes {
                return Err(CustodyError::InvalidRecord);
            }
            Ok(match directory.sync() {
                Ok(()) => Publication::Durable,
                Err(sync_error) => Publication::VisibleDurabilityUnconfirmed { sync_error },
            })
        }
        Err(error) => Err(error.into()),
    }
}

/// Settle a launch no guardian accepted. The reservation records
/// `NeverSpawned` and the failure carries the never-spawned exit, so the
/// caller withdraws instead of fencing once that exit is recorded; until then
/// it keeps custody for reconcile. A cancel request names the stop.
pub(super) fn refuse_never_spawned(
    lease: &mut CustodyLease,
    cancel: &CancelToken,
    publication: PublicationReservation,
    error: CustodyError,
) -> LaunchFailure {
    let scope = lease.scope();
    lease.reservation.launch_state = LaunchState::NeverSpawned;
    lease.reservation.unresolved_cleanup = false;
    lease.reservation.census_incomplete = false;
    lease.phase = LaunchPhase::NeverSpawned;
    let (reason, error) = if cancel.is_cancelled() {
        (
            Some(StopReason::ExplicitCancellation),
            CustodyError::Cancelled,
        )
    } else {
        (None, error)
    };
    let failure = LaunchFailure {
        error: Box::new(error),
        custody: publication,
        pending_exit: Some(Box::new(ProcessExit::never_spawned(scope, reason, vec![]))),
    };
    // A failed record leaves the exit pending for the caller's retry.
    let _ = write_record(
        &lease.directory,
        "reservation.json",
        &lease.reservation,
        PublishMode::Replace,
    )
    .and_then(|publication| publication.require_durable().map_err(Into::into))
    .and_then(|()| failure.retry_exit_publication())
    .and_then(|publication| publication.require_durable().map_err(Into::into));
    failure
}

impl Recovery {
    /// Record the never-spawned exit a refused launch could not publish, so
    /// Run withdraws it with its evidence. Only durable `NeverSpawned` custody
    /// bound to this attempt owes one. A never-spawned exit already recorded
    /// for this scope is kept, so a repeated reconcile settles the same way;
    /// any other exit is never replaced.
    pub fn record_never_spawned_exit(
        &self,
        attempt: &Directory,
        reason: Option<StopReason>,
    ) -> Result<Publication> {
        if !self.never_spawned(attempt)? {
            return Err(CustodyError::Fenced);
        }
        match attempt.metadata_at(OsStr::new("process-exit.json"), false) {
            Ok(_) => {
                let exit: StoredExit = read_record(attempt, "process-exit.json", EXIT_LIMIT)?;
                if exit.schema_version != 1
                    || !exit.never_spawned
                    || exit.settlement_evidence != SettlementEvidence::NeverSpawned
                    || exit.scope != self.reservation.scope
                {
                    return Err(CustodyError::InvalidRecord);
                }
                return Ok(match attempt.sync() {
                    Ok(()) => Publication::Durable,
                    Err(sync_error) => Publication::VisibleDurabilityUnconfirmed { sync_error },
                });
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        publish_never_spawned_exit(
            attempt,
            &ProcessExit::never_spawned(self.reservation.scope, reason, vec![]),
        )
    }
}
