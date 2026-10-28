//! Durable execution records and bounded, project-scoped readers.
#![forbid(unsafe_code)]
mod admission;
mod artifacts;
mod controls;
mod correlation;
pub use correlation::{WORK_INDEX_FILE, WorkCoverage, WorkIndex, WorkSelector};
mod db;
mod delivery;
mod disk;
mod git_context;
mod holder;
mod launch;
mod ledger;
mod overview;
mod plan;
mod reader;
mod retention;
mod service;
#[cfg(test)]
mod service_tests;
mod store;
mod transcript;
pub use service::{DispatchRequest, DispatchTask, ErrorKind, Service};
pub use transcript::locate as locate_transcript;
mod cargo_seed;
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
    capture_plan, concurrency,
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
    #[error("writer scope refused: {0}")]
    Writer(#[from] provenant_confinement::worktrees::WorktreeError),
    #[error("local worktree {0} does not exist; create it with `provenant worktree create <name>`")]
    WorktreeMissing(std::path::PathBuf),
    #[error(
        "worktree {0} is not registered with Provenant; create it with \
         `provenant worktree create --base <branch> <branch>`"
    )]
    WorktreeUnregistered(std::path::PathBuf),
    #[error(transparent)]
    Plan(#[from] provenant_confinement::PlanError),
    #[error("invalid run record: {0}")]
    Validation(&'static str),
    #[error("invalid Git base {0:?}: expected a resolvable commit reference")]
    InvalidBase(String),
    #[error("run-store schema version {0} is unsupported")]
    Version(i64),
    #[error("run-store path is not private or contained")]
    UnsafePath,
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
