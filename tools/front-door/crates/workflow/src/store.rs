//! Whole-receipt replacement at an expected revision. No old-state imports.
use crate::{persistence, receipt, risk};
use provenant_core::{CancelToken, Clock, DeliveryId, ProjectId, StateRoot, UtcMicros, WaitBudget};
use provenant_records::{
    DecodeError,
    workflow::{DeliveryReceipt, RiskPolicy, Subject, UserApproval},
};
use provenant_supervisor::fs::{Directory, FsError, LockMode, Publication, PublishMode};
use std::{ffi::OsStr, io};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("malformed receipt")]
    Malformed(#[source] receipt::ReceiptError),
    #[error("unsupported receipt version")]
    Version,
    #[error("receipt exceeds 64 KiB or 256 review references")]
    Limit,
    #[error("receipt identity does not match the store key")]
    Identity,
    #[error("receipt subject is stale")]
    StaleSubject,
    #[error("receipt facts have expired")]
    Expired,
    #[error("receipt revision conflict: expected {expected}, observed {observed}")]
    Conflict { expected: u64, observed: u64 },
    #[error("receipt revision overflow")]
    RevisionOverflow,
    #[error("receipt lock is contended")]
    Contention,
    #[error("receipt save cancelled")]
    Cancelled,
    #[error("receipt save deadline expired")]
    Deadline,
    #[error(transparent)]
    Risk(#[from] risk::RiskError),
    #[error("receipt clock failed")]
    Clock(#[from] provenant_core::TimeError),
    #[error("filesystem {operation} failed at <workflow receipt>: {cause}")]
    Io {
        operation: &'static str,
        #[source]
        cause: IoCause,
    },
    #[error("receipt replaced but directory sync failed; reread before retrying: {0}")]
    Durability(#[source] IoCause),
}
/// OS category/errno, without paths or arbitrary error text.
#[derive(Debug, Error)]
#[error("{kind:?} (OS code {os_code:?})")]
pub struct IoCause {
    pub kind: io::ErrorKind,
    pub os_code: Option<i32>,
}
impl From<FsError> for StoreError {
    fn from(error: FsError) -> Self {
        let operation = match &error {
            FsError::Cancelled { .. } => return Self::Cancelled,
            FsError::LockTimeout { .. } => return Self::Contention,
            FsError::TooLarge { .. } => return Self::Limit,
            FsError::Io { operation, .. } => *operation,
            FsError::Rename { .. } => "rename",
            FsError::Symlink { .. } => "refuse symlink",
            _ => "validate filesystem entry",
        };
        Self::Io {
            operation,
            cause: cause(error),
        }
    }
}
fn cause(error: FsError) -> IoCause {
    let error = io::Error::from(error);
    IoCause {
        kind: error.kind(),
        os_code: error.raw_os_error(),
    }
}
impl From<receipt::ReceiptError> for StoreError {
    fn from(error: receipt::ReceiptError) -> Self {
        match error {
            receipt::ReceiptError::Version
            | receipt::ReceiptError::Decode(DecodeError::UnsupportedVersion { .. }) => {
                Self::Version
            }
            receipt::ReceiptError::Limit
            | receipt::ReceiptError::Decode(DecodeError::TooLarge { .. }) => Self::Limit,
            error => Self::Malformed(error),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockRegression {
    pub previous: UtcMicros,
    pub observed: UtcMicros,
}
/// The edge renders clock_regression as a diagnostic, separate from output.
#[derive(Debug)]
pub struct SavedReceipt {
    pub receipt: DeliveryReceipt,
    pub clock_regression: Option<ClockRegression>,
}
/// Subject and earliest fact expiry gathered by the trusted composition edge.
/// Facts are rechecked under the lock using actual wall time.
pub struct SaveContext<'a> {
    pub subject: &'a Subject,
    pub valid_until: UtcMicros,
    pub risk_policy: &'a RiskPolicy,
    pub downgrade_approval: Option<&'a UserApproval>,
}
pub struct ReceiptStore {
    root: StateRoot,
    project: ProjectId,
    delivery: DeliveryId,
    #[cfg(test)]
    faults: Option<Box<dyn tests::FilesystemFaults>>,
}
impl ReceiptStore {
    pub fn new(root: StateRoot, project: ProjectId, delivery: DeliveryId) -> Self {
        Self {
            root,
            project,
            delivery,
            #[cfg(test)]
            faults: None,
        }
    }
    fn directory(&self, create: bool) -> Result<Directory, StoreError> {
        Ok(persistence::directory(
            &self.root,
            self.project,
            "deliveries",
            &self.delivery.to_string(),
            create,
        )?)
    }
    fn read_in(&self, dir: &Directory) -> Result<Option<DeliveryReceipt>, StoreError> {
        #[cfg(test)]
        if let Some(faults) = &self.faults {
            faults.read()?;
        }
        let bytes = match dir.read_bounded(OsStr::new("receipt.json"), receipt::MAX_RECEIPT_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let receipt = receipt::read(bytes.as_slice())?;
        self.check_identity(&receipt)?;
        Ok(Some(receipt))
    }
    fn check_identity(&self, receipt: &DeliveryReceipt) -> Result<(), StoreError> {
        if receipt.project_id() != self.project || receipt.delivery_id() != self.delivery {
            return Err(StoreError::Identity);
        }
        Ok(())
    }
    /// Stored claims only. Atomic replacement permits a consistent unlocked read.
    pub fn read(&self) -> Result<Option<DeliveryReceipt>, StoreError> {
        match self.directory(false) {
            Ok(dir) => self.read_in(&dir),
            Err(StoreError::Io { cause, .. }) if cause.kind == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
    /// Subject, risk policy, originating approval and fact expiry come from the
    /// trusted edge. G owns actor/review binding; save rechecks policy and expiry.
    /// IDs and creation time are immutable. A changed subject resets decision
    /// and review references. Revision and updated_at are store-owned.
    /// Never run routing probes, provider waits or GitHub reads under this lock.
    #[allow(clippy::too_many_arguments)]
    pub fn save(
        &self,
        candidate: &DeliveryReceipt,
        expected_revision: u64,
        context: SaveContext<'_>,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<SavedReceipt, StoreError> {
        check_wait(budget, cancel, clock)?;
        let dir = self.directory(true)?;
        #[cfg(test)]
        if let Some(faults) = &self.faults {
            faults.lock()?;
        }
        let _lock = dir.lock(
            OsStr::new("receipt.lock"),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )?;
        check_wait(budget, cancel, clock)?;
        let previous = self.read_in(&dir)?;
        let observed = previous.as_ref().map_or(0, DeliveryReceipt::revision);
        if observed != expected_revision {
            return Err(StoreError::Conflict {
                expected: expected_revision,
                observed,
            });
        }
        self.check_identity(candidate)?;
        if candidate.subject() != context.subject {
            return Err(StoreError::StaleSubject);
        }
        receipt::validate(candidate)?;
        risk::validate(
            candidate.risk(),
            context.risk_policy,
            context.downgrade_approval,
        )?;
        // Wall time is sampled only after acquiring the lock. The binding layer
        // must use actual now for expiry, never this clamped persistence time.
        let now = clock.wall_now()?;
        if now >= context.valid_until {
            return Err(StoreError::Expired);
        }
        let prior_time = previous.as_ref().map_or(now, DeliveryReceipt::updated_at);
        let regression = (now < prior_time).then_some(ClockRegression {
            previous: prior_time,
            observed: now,
        });
        let created_at = previous.as_ref().map_or(now, DeliveryReceipt::created_at);
        let committed = candidate
            .clone()
            .with_commit_metadata(
                observed
                    .checked_add(1)
                    .ok_or(StoreError::RevisionOverflow)?,
                created_at,
                prior_time.max(now),
                previous
                    .as_ref()
                    .is_some_and(|r| r.subject() != candidate.subject()),
            )
            .map_err(receipt::ReceiptError::from)?;
        let bytes = receipt::canonical(&committed)?;
        // Cancellation/deadline boundary: finish staging, replacement and sync
        // after this check, even if a cancellation subsequently arrives.
        check_wait(budget, cancel, clock)?;
        match dir.atomic_write(
            OsStr::new("receipt.json"),
            ".receipt.",
            bytes.as_bytes(),
            PublishMode::Replace,
        )? {
            Publication::Durable => Ok(SavedReceipt {
                receipt: committed,
                clock_regression: regression,
            }),
            Publication::VisibleDurabilityUnconfirmed { sync_error } => {
                Err(StoreError::Durability(cause(sync_error)))
            }
        }
    }
}
fn check_wait(
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<(), StoreError> {
    if cancel.is_cancelled() {
        return Err(StoreError::Cancelled);
    }
    if budget.deadline().is_expired(clock.mono_now()) {
        return Err(StoreError::Deadline);
    }
    Ok(())
}
#[cfg(test)]
mod tests;
