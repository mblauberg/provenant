//! Project-scoped mail, tasks and events. One concrete connection per worker.
#![forbid(unsafe_code)]

mod claims;
mod db;
mod events;
mod federation;
mod import;
mod landing;
mod messages;
mod operations;
mod tasks;

pub use claims::*;
pub use db::{Request, RetryWait, Store, ThreadWait};
pub use events::*;
pub use federation::*;
pub use import::*;
pub use landing::*;
pub use messages::*;
pub use tasks::*;

use provenant_core::{ActorId, ProjectId, WallTime};
use provenant_records::{CanonicalV1, Limits};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub type Result<T> = std::result::Result<T, Error>;
pub const MAX_BODY_BYTES: usize = 65_536;
pub const MAX_RECORD_BYTES: usize = 262_144;
pub const MAX_PAGE_ROWS: u64 = 100;

/// Sequence pages expose byte-bound truncation. Resume after the last item's seq.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQLite operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("safe filesystem operation failed")]
    Filesystem(#[from] provenant_supervisor::fs::FsError),
    #[error("filesystem I/O failed at {path}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid field: {0}")]
    Invalid(&'static str),
    #[error("record not found")]
    NotFound,
    #[error("record conflicts with existing state")]
    Conflict,
    #[error("push operation {operation} requires reconciliation")]
    PushUnresolved {
        operation: provenant_core::OperationId,
    },
    #[error("invalid push transition from {from:?} using {action:?}")]
    PushTransition { from: PushPhase, action: PushAction },
    #[error("claim overlaps active ownership {claim}")]
    Overlap { claim: provenant_core::ClaimId },
    #[error("path cannot be represented as UTF-8")]
    PathEncoding,
    #[error("task ownership fence is stale")]
    StaleFence,
    #[error("SQLite remained busy until the request deadline")]
    Busy,
    #[error("request cancelled")]
    Cancelled,
    #[error("unsupported database version {0}")]
    SchemaVersion(i64),
    #[error("non-empty database has no schema version")]
    UnversionedDatabase,
    #[error("SQLite refused WAL journal mode (returned {0})")]
    JournalMode(String),
    #[error("unsafe state component: {0}")]
    UnsafePath(std::path::PathBuf),
    #[error("stored record is invalid")]
    Decode(#[from] provenant_records::DecodeError),
    #[error("record cannot be canonically encoded")]
    Encode(#[from] provenant_records::CanonicalError),
    #[error("time is outside the supported range")]
    Time(#[from] provenant_core::TimeError),
    #[error("import exceeds its row or byte bound")]
    ImportLimit,
    #[error("stop the old Fabric and retry with a cleanly closed source (journal sidecars remain)")]
    ImportNotQuiescent,
    #[error("cannot open import source read-only at {path}; stop the old Fabric and retry")]
    ImportReadOnly {
        path: std::path::PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("import source must be a self-contained, quiescent snapshot")]
    ImportSource,
}

fn encode<T: Serialize>(record: &T) -> Result<Vec<u8>> {
    let canonical = CanonicalV1::bytes(record)?;
    let bytes = canonical.as_bytes();
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(Error::Invalid("record size"));
    }
    Ok(bytes.to_vec())
}
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(provenant_records::decode(
        bytes,
        Limits {
            max_bytes: MAX_RECORD_BYTES as u64,
            max_depth: 64,
            max_members: 10_000,
        },
    )?)
}
fn text(value: &str, field: &'static str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_BODY_BYTES || value.contains('\0') {
        return Err(Error::Invalid(field));
    }
    Ok(())
}
fn page(after: i64, limit: u64) -> Result<i64> {
    if after < 0 || !(1..=MAX_PAGE_ROWS).contains(&limit) {
        return Err(Error::Invalid("cursor or page limit"));
    }
    i64::try_from(limit).map_err(|_| Error::Invalid("page limit"))
}

#[cfg(test)]
mod tests;
