//! Durable execution records and bounded, project-scoped readers.
#![forbid(unsafe_code)]
mod admission;
mod artifacts;
mod board;
pub use board::{BoardLane, BoardPage};
mod controls;
mod correlation;
pub use correlation::{WORK_INDEX_FILE, WorkCoverage, WorkIndex, WorkSelector, validate_work};
mod db;
mod delivery;
mod disk;
mod git_context;
mod holder;
mod launch;
mod ledger;
mod objects;
mod overview;
mod plan;
mod pr_contribution;
pub use pr_contribution::{Contribution, Contributions, PullRequest, WEIGHT_ONE};
mod reader;
mod retention;
mod service;
#[cfg(test)]
mod service_tests;
mod store;
mod transcript;
pub use service::{DispatchRequest, DispatchTask, ErrorKind, Service};
pub use transcript::locate as locate_transcript;
pub mod build_dir_gc;
mod cargo_seed;
pub mod target_seed;
pub mod target_seed_gc;
mod writer;

pub use admission::{
    AdmissionPolicy, HOST_ACTIVE, HOST_QUEUED, LaneWait, PROJECT_ACTIVE, Resources, SystemResources,
};
pub use controls::{CancelReceipt, Reconciled, Resume, UncertainReason};
pub use delivery::HostDeliveryContext;
pub use git_context::{GitContext, continuation_history, resolve_git_base, review_merge_base};
pub use holder::{HolderContext, HostLaunchContext, RootSeed, RunHolder, SealedTask};
pub use launch::{
    Decoded, LaunchPaths, PreparedLaunch, PreparedStart, Providers, RefusalKind, Started,
    ValidatedChild, capture_plan, concurrency,
};
pub use overview::{OverviewAttempt, OverviewState, RunOverview};
pub use plan::read_only_plan;
pub use retention::{
    AttemptLogRemoval, AttemptTmpRemoval, ProviderStateGc, ProviderStateIssue, TOMBSTONE,
};
pub use writer::{
    LaunchConfinement, WriterFence, WriterLaunch, prepare_writer, resolve_writer,
    resolve_writer_proposal,
};

pub use db::{ExecutionContext, Request, RunStore};
pub use provenant_records::run::*;
pub use reader::{
    ArtifactPart, AttemptFilter, AttemptReader, Attempts, ByteRange, LanePage, LaneQuery,
    LaneTarget, OutputChunk, fact,
};
pub use store::{NewAttempt, PreparedRun, TerminalPublication};
use thiserror::Error;

pub const LANE_DEPTH_ENV: &str = "PROVENANT_LANE_DEPTH";
pub const PARENT_ATTEMPT_ENV: &str = "PROVENANT_PARENT_ATTEMPT_ID";

/// Return the depth of the run being dispatched under this lane.
pub fn next_lane_depth(parent_depth: u64, max_depth: u64) -> Result<u64> {
    let depth = parent_depth.checked_add(1).ok_or(RunError::DepthOverflow)?;
    if depth > max_depth {
        return Err(RunError::DepthExceeded { depth, max_depth });
    }
    Ok(depth)
}

/// The privacy or containment check that refused a run-store path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathCheck {
    /// The entry belongs to another user.
    Owner,
    /// The entry grants group or other access.
    Mode,
    /// The entry is a symlink.
    Symlink,
    /// The entry is not the regular file or directory the store requires.
    Kind,
    /// The entry has more than one hard link.
    Links,
    /// The entry resolves outside the directory it must stay within.
    Containment,
    /// The entry changed identity while it was being checked.
    Identity,
    /// The entry was expected and is absent.
    Missing,
}
impl PathCheck {
    fn name(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Mode => "mode",
            Self::Symlink => "symlink",
            Self::Kind => "kind",
            Self::Links => "link count",
            Self::Containment => "outside the root",
            Self::Identity => "identity mismatch",
            Self::Missing => "missing",
        }
    }
    /// The first privacy check a regular file's metadata fails, if any.
    pub(crate) fn file(metadata: provenant_os::fs::Metadata) -> Option<Self> {
        use provenant_os::fs::FileKind;
        match metadata.kind {
            FileKind::Regular => {}
            FileKind::Symlink => return Some(Self::Symlink),
            _ => return Some(Self::Kind),
        }
        if metadata.links != 1 {
            Some(Self::Links)
        } else if metadata.uid != provenant_os::fs::effective_uid() {
            Some(Self::Owner)
        } else if metadata.mode & 0o077 != 0 {
            Some(Self::Mode)
        } else {
            None
        }
    }
    /// The first privacy check a directory's metadata fails, if any.
    pub(crate) fn directory(metadata: provenant_os::fs::Metadata) -> Option<Self> {
        if metadata.uid != provenant_os::fs::effective_uid() {
            Some(Self::Owner)
        } else if metadata.mode & 0o077 != 0 {
            Some(Self::Mode)
        } else {
            None
        }
    }
}
impl From<provenant_sqlite::CompanionFault> for PathCheck {
    fn from(fault: provenant_sqlite::CompanionFault) -> Self {
        use provenant_sqlite::CompanionFault;
        match fault {
            CompanionFault::NotRegular => Self::Kind,
            CompanionFault::Symlink => Self::Symlink,
            CompanionFault::Owner => Self::Owner,
            CompanionFault::Mode => Self::Mode,
            CompanionFault::Links => Self::Links,
        }
    }
}
impl std::fmt::Display for PathCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error("{message}")]
    Input { kind: ErrorKind, message: String },
    #[error("host writer delivery failed at {0:?}")]
    Delivery(WriterDeliveryStage),
    #[error("host writer delivery failed at Prepare ({step}): {reason}")]
    DeliveryPrepare { step: &'static str, reason: String },
    #[error("provider evidence could not be committed: {0}")]
    ProviderEvidence(String),
    #[error("confinement refused: {0}")]
    Confinement(#[from] provenant_confinement::permissions::PermissionError),
    #[error("writer scope refused: {}", .0.with_repair())]
    Writer(#[from] provenant_confinement::worktrees::WorktreeError),
    #[error("local worktree {0} does not exist; create it with `provenant worktree create <name>`")]
    WorktreeMissing(std::path::PathBuf),
    #[error(
        "worktree {0} is not registered with Provenant; create it with \
         `provenant worktree create --base <branch> <branch>`"
    )]
    WorktreeUnregistered(std::path::PathBuf),
    #[error("writer Git ref lock {} already exists; check for a running Git process, then remove the stale lock before retrying", .0.display())]
    WriterRefLocked(std::path::PathBuf),
    #[error(transparent)]
    Plan(#[from] provenant_confinement::PlanError),
    #[error("invalid run record: {0}")]
    Validation(&'static str),
    #[error("lane object import refused: {0}")]
    ObjectImportLimit(&'static str),
    #[error(
        "object-store lock wait timed out; retry settlement (custody and private objects retained)"
    )]
    ObjectStoreBusy,
    #[error("object verification deferred: {0}")]
    ObjectVerificationRetry(&'static str),
    #[error("invalid Git base {0:?}: expected a resolvable commit reference")]
    InvalidBase(String),
    #[error("run-store schema version {0} is unsupported")]
    Version(i64),
    #[error("run-store path {} is not private or contained: {check}", path.display())]
    UnsafePath {
        path: std::path::PathBuf,
        check: PathCheck,
    },
    #[error("run-store operation conflicts with its immutable request")]
    OperationConflict,
    #[error("run record was not found in this scope")]
    NotFound,
    #[error("attempt is not terminal")]
    Nonterminal,
    #[error(
        "provider state has expired under retention.provider_state_days; this attempt can no longer be resumed; dispatch a new lane with its retained result or transcripts as context"
    )]
    ProviderStateExpired,
    #[error("run record revision conflicts or is immutable")]
    Conflict,
    #[error("read or record exceeds byte limit {limit}")]
    TooLarge { limit: u64 },
    #[error("artifact digest or length does not match its record")]
    ArtifactMismatch,
    #[error("terminal publication is already sealed; recover its original facts and result")]
    TerminalRecoveryRequired,
    #[error(
        "insufficient disk space: {free} bytes available, gc.free_floor {floor} bytes; run `provenant gc --apply`"
    )]
    DiskFloor { free: u64, floor: u64 },
    #[error("memory pressure is {level}; wait for host memory pressure to fall")]
    MemoryPressure { level: String },
    #[error(
        "writer build slots full: {active} of {limit} active; wait for a writer to finish or adjust lanes.max_concurrent_builds"
    )]
    BuildSlots { active: u64, limit: u64 },
    #[error(
        "invalid host build settings: check max_concurrent_builds, cargo_build_jobs and cargo_debug in domains.fabric.lanes"
    )]
    BuildSettings,
    #[error("guardian process identity unavailable; retry when process observation is available")]
    GuardianIdentityUnavailable,
    #[error("operation refused by the execution context")]
    Refused,
    #[error("nested lane depth {depth} exceeds configured maximum {max_depth}")]
    DepthExceeded { depth: u64, max_depth: u64 },
    #[error("nested lane depth overflowed")]
    DepthOverflow,
    #[error("host queue is full")]
    Capacity,
    #[error(
        "startup spacing cannot be judged: the host monotonic clock or boot identity cannot be read"
    )]
    HostClock,
    #[error("run store is contended or its call budget expired")]
    Contention,
    #[error("run-store call was cancelled")]
    Cancelled,
    #[error("publication is visible but not durably synced")]
    Publication(#[source] provenant_os::fs::FsError),
    #[error(transparent)]
    Filesystem(#[from] provenant_os::fs::FsError),
    #[error("run database operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Decode(#[from] provenant_records::DecodeError),
    #[error(transparent)]
    Canonical(#[from] provenant_records::CanonicalError),
    #[error("run filesystem I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("run I/O operation {operation:?} failed at {path:?}: {source}")]
    OperationIo {
        operation: &'static str,
        path: Option<std::path::PathBuf>,
        #[source]
        source: std::io::Error,
    },
    #[error("run record path is invalid")]
    Path(#[from] provenant_records::PathWireError),
}
pub type Result<T> = std::result::Result<T, RunError>;
impl RunError {
    /// Refuse `path`, which is relative to the state root where the store owns it
    /// and as given for a project or worktree path outside the state root.
    pub(crate) fn unsafe_path(path: impl Into<std::path::PathBuf>, check: PathCheck) -> Self {
        let path = path.into();
        Self::UnsafePath {
            path: if path.as_os_str().is_empty() {
                ".".into()
            } else {
                path
            },
            check,
        }
    }
    /// Refuse the entry `name` of a held directory, named relative to the state root.
    pub(crate) fn unsafe_entry(
        directory: &provenant_os::fs::Directory,
        name: &std::ffi::OsStr,
        check: PathCheck,
    ) -> Self {
        Self::unsafe_path(directory.anchored_path().join(name), check)
    }
    pub fn code(&self) -> &'static str {
        ErrorKind::from(self).code()
    }
}
#[cfg(test)]
mod delivery_tests;
#[cfg(test)]
mod dispatch_tests;
#[cfg(test)]
mod tests;
