//! Durable execution records and bounded, project-scoped readers.
#![forbid(unsafe_code)]
mod admission;
mod artifacts;
mod controls;
mod db;
mod delivery;
mod holder;
mod launch;
mod plan;
mod reader;
mod store;
mod writer;

pub use admission::{
    AdmissionPolicy, HOST_ACTIVE, HOST_QUEUED, PROJECT_ACTIVE, Resources, SystemResources,
};
pub use controls::{CancelReceipt, Reconciled, Resume};
pub use delivery::HostDeliveryContext;
pub use holder::{HolderContext, HostLaunchContext, RootSeed, RunHolder};
pub use launch::{
    Decoded, LaunchPaths, PreparedLaunch, Providers, RefusalKind, Started, capture_plan,
    concurrency,
};
pub use plan::read_only_plan;
pub use writer::{LaunchConfinement, WriterFence, WriterLaunch, prepare_writer, resolve_writer};

pub use db::{ExecutionContext, Request, RunStore};
pub use provenant_records::run::*;
pub use reader::{
    ArtifactPart, AttemptFilter, AttemptReader, Attempts, ByteRange, LanePage, LaneQuery,
    LaneTarget, OutputChunk, RunWorkSnapshot,
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
    #[error("host writer delivery failed at {0:?}; inspect the attempt delivery receipt")]
    Delivery(WriterDeliveryStage),
    #[error("provider evidence could not be committed: {0}")]
    ProviderEvidence(String),
    #[error("confinement refused: {0}")]
    Confinement(#[from] provenant_confinement::permissions::PermissionError),
    #[error("writer scope refused: {0}")]
    Writer(#[from] provenant_confinement::worktrees::WorktreeError),
    #[error("local worktree {0} does not exist; create it with `provenant worktree create <name>`")]
    WorktreeMissing(std::path::PathBuf),
    #[error(transparent)]
    Plan(#[from] provenant_confinement::PlanError),
    #[error("invalid run record: {0}")]
    Validation(&'static str),
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
    #[error("run record revision conflicts or is immutable")]
    Conflict,
    #[error("read or record exceeds byte limit {limit}")]
    TooLarge { limit: u64 },
    #[error("artifact digest or length does not match its record")]
    ArtifactMismatch,
    #[error("terminal publication is already sealed; recover its original facts and result")]
    TerminalRecoveryRequired,
    #[error("operation refused by the execution context")]
    Refused,
    #[error("nested lane depth {depth} exceeds configured maximum {max_depth}")]
    DepthExceeded { depth: u64, max_depth: u64 },
    #[error("nested lane depth overflowed")]
    DepthOverflow,
    #[error("host queue is full")]
    Capacity,
    #[error("run store is contended or its call budget expired")]
    Contention,
    #[error("run-store call was cancelled")]
    Cancelled,
    #[error("publication is visible but not durably synced")]
    Publication(#[source] provenant_supervisor::fs::FsError),
    #[error(transparent)]
    Filesystem(#[from] provenant_supervisor::fs::FsError),
    #[error("run database operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Decode(#[from] provenant_records::DecodeError),
    #[error(transparent)]
    Canonical(#[from] provenant_records::CanonicalError),
    #[error("run filesystem I/O failed")]
    Io(#[from] std::io::Error),
    #[error("run record path is invalid")]
    Path(#[from] provenant_records::PathWireError),
}
pub type Result<T> = std::result::Result<T, RunError>;
#[cfg(test)]
mod delivery_tests;
#[cfg(test)]
mod dispatch_tests;
#[cfg(test)]
mod tests;
