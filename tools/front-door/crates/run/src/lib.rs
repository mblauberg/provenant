//! Durable execution records and bounded, project-scoped readers.
#![forbid(unsafe_code)]
mod admission;
mod artifacts;
mod db;
mod holder;
mod launch;
mod reader;
mod store;

pub use admission::{
    AdmissionPolicy, HOST_ACTIVE, HOST_QUEUED, PROJECT_ACTIVE, Resources, SystemResources,
};
pub use holder::{HolderContext, RootSeed, RunHolder};
pub use launch::{Decoded, PreparedLaunch, Providers, Started};

pub use db::{ExecutionContext, Request, RunStore};
pub use provenant_records::run::*;
pub use reader::{
    ArtifactPart, AttemptFilter, AttemptReader, Attempts, ByteRange, OutputChunk, RunWorkSnapshot,
};
pub use store::{NewAttempt, PreparedRun, TerminalPublication};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RunError {
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
mod dispatch_tests;
#[cfg(test)]
mod tests;
