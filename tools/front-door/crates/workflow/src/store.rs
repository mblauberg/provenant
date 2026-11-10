//! Whole-receipt replacement at an expected revision. No old-state imports.
use crate::{persistence, receipt, risk};
use provenant_core::{
    CancelToken, Clock, DeliveryId, ProjectId, RepoId, StateRoot, UtcMicros, WaitBudget,
};
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode, Publication, PublishMode};
use provenant_records::{
    DecodeError,
    workflow::{ChairDowngrade, DeliveryReceipt, ReceiptHeader, RiskPolicy, Subject, SubjectKey},
};
use std::{ffi::OsStr, io};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("malformed receipt")]
    Malformed(#[source] receipt::ReceiptError),
    #[error("invalid receipt candidate")]
    InvalidCandidate(#[source] receipt::ReceiptError),
    #[error("unsupported receipt version")]
    Version,
    /// A receipt of an earlier shape. Saving the delivery again replaces it.
    #[error(
        "receipt shape retired: save it again (delivery {delivery}, --expected-revision {revision})"
    )]
    Retired { delivery: DeliveryId, revision: u64 },
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
pub(crate) fn cause(error: FsError) -> IoCause {
    let error = io::Error::from(error);
    IoCause {
        kind: error.kind(),
        os_code: error.raw_os_error(),
    }
}
impl From<receipt::ReceiptError> for StoreError {
    fn from(error: receipt::ReceiptError) -> Self {
        match error {
            receipt::ReceiptError::Retired(header) => Self::Retired {
                delivery: header.delivery_id,
                revision: header.revision,
            },
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
impl StoreError {
    /// Maps an error in the caller's candidate; stored data uses `From`.
    fn candidate(error: receipt::ReceiptError) -> Self {
        match Self::from(error) {
            Self::Malformed(error) => Self::InvalidCandidate(error),
            other => other,
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
    pub downgrade_approval: Option<&'a ChairDowngrade>,
    /// Complete changed paths, including both sides of renames, from the edge.
    pub changed_paths: &'a [String],
}
/// The receipts in `project` whose subject is pull request `number` of
/// `repository`.
#[derive(Debug, Default)]
pub struct PullRequestReceipts {
    pub receipts: Vec<DeliveryReceipt>,
    /// Receipts of a retired shape: they approve nothing until saved again.
    pub retired: Vec<StoreError>,
}
/// Every receipt in `project` for pull request `number` of `repository`. A
/// receipt that cannot be decoded at all names no subject and is skipped.
pub fn pull_request_receipts(
    root: &StateRoot,
    project: ProjectId,
    repository: RepoId,
    number: u64,
) -> Result<PullRequestReceipts, StoreError> {
    let mut found = PullRequestReceipts::default();
    let deliveries = match persistence::collection(root, project, "deliveries") {
        Ok(deliveries) => deliveries,
        Err(error) if error.is_not_found() => return Ok(found),
        Err(error) => return Err(error.into()),
    };
    for name in deliveries.entries()? {
        let Some(delivery) = name?.to_str().and_then(|name| DeliveryId::parse(name).ok()) else {
            continue;
        };
        let store = ReceiptStore::new(root.clone(), project, delivery);
        let dir = match store.directory(false) {
            Ok(dir) => dir,
            Err(StoreError::Io { cause, .. }) if cause.kind == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        match store.prior_in(&dir) {
            Ok(Prior::Current(receipt)) => {
                if matches!(receipt.subject(), Subject::Pr { repository: repo, number: pr, .. }
                    if *repo == repository && *pr == number)
                {
                    found.receipts.push(*receipt);
                }
            }
            Ok(Prior::Retired(header)) => {
                if header.subject == (SubjectKey::Pr { repository, number }) {
                    found.retired.push(StoreError::Retired {
                        delivery,
                        revision: header.revision,
                    });
                }
            }
            Ok(Prior::Absent)
            | Err(
                StoreError::Malformed(_)
                | StoreError::Version
                | StoreError::Limit
                | StoreError::Identity,
            ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(found)
}
/// What a save replaces.
enum Prior {
    Absent,
    Current(Box<DeliveryReceipt>),
    Retired(Box<ReceiptHeader>),
}
pub struct ReceiptStore {
    root: StateRoot,
    project: ProjectId,
    delivery: DeliveryId,
    filesystem: Box<dyn StoreFilesystem>,
}
trait StoreFilesystem {
    fn read_bounded(&self, dir: &Directory) -> Result<Vec<u8>, FsError> {
        dir.read_bounded(OsStr::new("receipt.json"), receipt::MAX_RECEIPT_BYTES)
    }
    fn lock(
        &self,
        dir: &Directory,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<LockGuard, FsError> {
        dir.lock(
            OsStr::new("receipt.lock"),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )
    }
    fn atomic_write(&self, dir: &Directory, bytes: &[u8]) -> Result<Publication, FsError> {
        dir.atomic_write(
            OsStr::new("receipt.json"),
            ".receipt.",
            bytes,
            PublishMode::Replace,
        )
    }
}
struct LocalStoreFilesystem;
impl StoreFilesystem for LocalStoreFilesystem {}
impl ReceiptStore {
    pub fn new(root: StateRoot, project: ProjectId, delivery: DeliveryId) -> Self {
        Self {
            root,
            project,
            delivery,
            filesystem: Box::new(LocalStoreFilesystem),
        }
    }
    #[cfg(test)]
    fn set_filesystem(&mut self, filesystem: impl StoreFilesystem + 'static) {
        self.filesystem = Box::new(filesystem);
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
        match self.prior_in(dir)? {
            Prior::Absent => Ok(None),
            Prior::Current(receipt) => Ok(Some(*receipt)),
            Prior::Retired(header) => Err(StoreError::Retired {
                delivery: header.delivery_id,
                revision: header.revision,
            }),
        }
    }
    fn prior_in(&self, dir: &Directory) -> Result<Prior, StoreError> {
        let bytes = match self.filesystem.read_bounded(dir) {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => return Ok(Prior::Absent),
            Err(error) => return Err(error.into()),
        };
        match receipt::read(bytes.as_slice()) {
            Ok(receipt) => {
                self.check_identity(&receipt)?;
                Ok(Prior::Current(Box::new(receipt)))
            }
            Err(receipt::ReceiptError::Retired(header)) => {
                if header.project_id != self.project || header.delivery_id != self.delivery {
                    return Err(StoreError::Identity);
                }
                Ok(Prior::Retired(header))
            }
            Err(error) => Err(error.into()),
        }
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
        let _lock = self
            .filesystem
            .lock(&dir, budget, cancel, clock)
            .map_err(|error| {
                if matches!(error, FsError::LockTimeout { .. })
                    && let Err(stop) = check_wait(budget, cancel, clock)
                {
                    return stop.into();
                }
                StoreError::from(error)
            })?;
        check_wait(budget, cancel, clock)?;
        let prior = self.prior_in(&dir)?;
        let observed = match &prior {
            Prior::Absent => 0,
            Prior::Current(receipt) => receipt.revision(),
            Prior::Retired(header) => header.revision,
        };
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
        receipt::validate(candidate).map_err(StoreError::candidate)?;
        risk::validate(
            candidate.risk(),
            context.risk_policy,
            context.downgrade_approval,
            context.changed_paths,
        )?;
        // Wall time is sampled only after acquiring the lock. The binding layer
        // must use actual now for expiry, never this clamped persistence time.
        let now = clock.wall_now()?;
        if now >= context.valid_until {
            return Err(StoreError::Expired);
        }
        let prior_time = match &prior {
            Prior::Absent => now,
            Prior::Current(receipt) => receipt.updated_at(),
            Prior::Retired(header) => header.updated_at,
        };
        let regression = (now < prior_time).then_some(ClockRegression {
            previous: prior_time,
            observed: now,
        });
        let committed = match &prior {
            Prior::Absent => candidate.clone().with_commit_metadata(None, now),
            Prior::Current(previous) => candidate.clone().with_commit_metadata(Some(previous), now),
            Prior::Retired(header) => candidate.clone().replacing_retired(header, now),
        }
        .map_err(|error| match error {
            provenant_records::workflow::RecordError::RevisionOverflow => {
                StoreError::RevisionOverflow
            }
            error => StoreError::candidate(error.into()),
        })?;
        let bytes = receipt::canonical(&committed).map_err(StoreError::candidate)?;
        // Cancellation/deadline boundary: finish staging, replacement and sync
        // after this check, even if a cancellation subsequently arrives.
        check_wait(budget, cancel, clock)?;
        let publication = self.filesystem.atomic_write(&dir, bytes.as_bytes());
        publication?
            .require_durable()
            .map_err(|error| StoreError::Durability(cause(error)))?;
        Ok(SavedReceipt {
            receipt: committed,
            clock_regression: regression,
        })
    }
}
/// Why a bounded wait stopped before its publication boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitStop {
    Cancelled,
    Deadline,
}
impl From<WaitStop> for StoreError {
    fn from(stop: WaitStop) -> Self {
        match stop {
            WaitStop::Cancelled => Self::Cancelled,
            WaitStop::Deadline => Self::Deadline,
        }
    }
}
pub(crate) fn check_wait(
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<(), WaitStop> {
    if cancel.is_cancelled() {
        return Err(WaitStop::Cancelled);
    }
    if budget.deadline().is_expired(clock.mono_now()) {
        return Err(WaitStop::Deadline);
    }
    Ok(())
}
#[cfg(test)]
mod tests;
