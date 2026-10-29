//! Project-scoped mail, tasks and events. One concrete connection per worker.
#![forbid(unsafe_code)]

mod chairs;
mod claims;
mod db;
mod events;
mod federation;
mod human_inbox;
mod landing;
mod local_delivery;
mod messages;
mod operations;
mod tasks;
mod viewer;

pub use chairs::*;
pub use claims::*;
pub use db::{CURRENT_SCHEMA_VERSION, Request, Store};
pub use events::*;
pub use federation::*;
pub use human_inbox::{
    HumanAcknowledgement, HumanClosure, PendingHumanRequest, PendingHumanRequests,
};
pub use landing::*;
pub use messages::*;
pub use tasks::*;
pub use viewer::*;

use provenant_core::{ActorId, ProjectId, WallTime};
use provenant_records::{CanonicalV1, Limits};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub type Result<T> = std::result::Result<T, Error>;
pub const MAX_BODY_BYTES: usize = 65_536;
pub const MAX_RECORD_BYTES: usize = 262_144;
pub const MAX_PAGE_ROWS: u64 = 100;
pub(crate) const MAX_RECORD_READ_BYTES: i64 = (MAX_RECORD_BYTES + 1) as i64;
pub(crate) const MAX_PAGE_BYTES: usize = 1_048_576;

/// Bounded reads expose byte-cap truncation; resume after the last item's cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub items: Vec<T>,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQLite operation failed: {}", sqlite_diagnostic(.0))]
    Sqlite(rusqlite::Error),
    #[error("safe filesystem operation failed")]
    Filesystem(#[from] provenant_os::fs::FsError),
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
    #[error("viewer state limit reached")]
    ViewerLimit,
    #[error("viewer request is not open")]
    ViewerNotOpen,
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
    #[error("SQLite remained busy past its busy timeout")]
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
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        if provenant_sqlite::is_busy(&error) {
            Self::Busy
        } else {
            Self::Sqlite(error)
        }
    }
}

// SQLite's detailed message can contain SQL values, paths, or other private
// data. The result code is bounded and provides a safe, useful diagnosis.
fn sqlite_diagnostic(error: &rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(code, _) => code.to_string(),
        rusqlite::Error::QueryReturnedNoRows => "query returned no rows".into(),
        rusqlite::Error::QueryReturnedMoreThanOneRow => "query returned more than one row".into(),
        rusqlite::Error::ExecuteReturnedResults => "execute returned results".into(),
        rusqlite::Error::InvalidQuery => "query is not read-only".into(),
        _ => "rusqlite error".into(),
    }
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
    let decoded = provenant_records::decode(
        bytes,
        Limits {
            max_bytes: MAX_RECORD_BYTES as u64,
            max_depth: 64,
            max_members: 10_000,
        },
    );
    match decoded {
        Err(provenant_records::DecodeError::Typed { line, column }) => {
            // Store records carry their version in a nested header. Inspect only
            // that header, not version fields in embedded application data.
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes)
                && let Some(found) = value
                    .get("header")
                    .and_then(|h| h.get("schema_version"))
                    .and_then(serde_json::Value::as_u64)
                && found > 1
            {
                return Err(provenant_records::DecodeError::UnsupportedVersion {
                    found,
                    line,
                    column,
                }
                .into());
            }
            Err(provenant_records::DecodeError::Typed { line, column }.into())
        }
        result => Ok(result?),
    }
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

struct BoundedRows<T> {
    items: Vec<T>,
    truncated: bool,
}

fn bounded_rows<T, U>(
    rows: impl Iterator<Item = rusqlite::Result<(Vec<u8>, T)>>,
    mut map: impl FnMut(Vec<u8>, T) -> Result<U>,
) -> Result<BoundedRows<U>> {
    let mut items = Vec::new();
    let mut total = 0;
    let mut truncated = false;
    for row in rows {
        let (bytes, value) = row?;
        total += bytes.len();
        if total > MAX_PAGE_BYTES {
            truncated = true;
            break;
        }
        items.push(map(bytes, value)?);
    }
    Ok(BoundedRows { items, truncated })
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
