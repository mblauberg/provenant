//! Project-scoped mail, tasks and events. One concrete connection per worker.
#![forbid(unsafe_code)]

mod db;
mod events;
mod import;
mod messages;
mod tasks;

pub use db::{Request, RetryWait, Store, ThreadWait};
pub use events::*;
pub use import::*;
pub use messages::*;
pub use tasks::*;

use provenant_core::{ActorId, ProjectId, WallTime};
use provenant_records::{CanonicalV1, Limits};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub type Result<T> = std::result::Result<T, Error>;
pub const MAX_BODY_BYTES: usize = 65_536;
pub const MAX_RECORD_BYTES: usize = 262_144;
pub const MAX_PAGE_ROWS: u64 = 100;

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
    #[error("task ownership fence is stale")]
    StaleFence,
    #[error("SQLite remained busy until the request deadline")]
    Busy,
    #[error("request cancelled")]
    Cancelled,
    #[error("unsupported database version {0}")]
    SchemaVersion(i64),
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
    #[error("import source must be a self-contained, quiescent snapshot")]
    ImportSource,
}

fn encode<T: Serialize>(record: &T) -> Result<Vec<u8>> {
    let bytes = CanonicalV1::bytes(record)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(Error::Invalid("record size"));
    }
    Ok(bytes)
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
