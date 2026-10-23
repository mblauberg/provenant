//! Inert Fabric SQLite owner. Each connection has one owning thread at a time.
#![forbid(unsafe_code)]

mod diagnostics;
mod filesystem;
mod landing;
mod messages;
mod pending;
mod sessions;
mod tasks;
pub use filesystem::FilesystemError;
mod legacy;
pub use legacy::*;
mod work;
pub use work::*;
mod backend;
pub use backend::*;

pub use diagnostics::*;
pub use landing::*;
pub use messages::*;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
pub use sessions::*;
use std::{
    path::Path,
    time::{Duration, Instant},
};
pub use tasks::*;

pub const SCHEMA: &str = include_str!("../../../../../runtime/fabric/schema.sql");
pub const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5000;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Cannot read properties of null (reading 'some')")]
    NullClaimPaths,
    #[error("existing.paths.some is not a function")]
    ClaimPathsNotArray,
    #[error("Cannot read properties of null (reading 'toLowerCase')")]
    NullClaimPath,
    #[error("right.toLowerCase is not a function")]
    ClaimPathNotString,
    #[error("database changed while creating a diagnostic snapshot")]
    SnapshotChanged,
    #[error("could not reserve a private diagnostic snapshot directory")]
    SnapshotNameExhausted,
    #[error(transparent)]
    Filesystem(#[from] FilesystemError),
    #[error("Cannot convert object to primitive value")]
    PrimitiveConversion,
    #[error("SQLite3 can only bind numbers, strings, bigints, buffers, and null")]
    SqliteBindingType,
    #[error(transparent)]
    CheckedJson(#[from] provenant_records::checked_json::DecodeError),
    #[error("stale work claim")]
    StaleWork,
    #[error("claim requires an issue or paths")]
    EmptyWorkScope,
    #[error("at most 100 paths may be claimed")]
    TooManyPaths,
    #[error("invalid repository-relative path: {path}")]
    InvalidWorkPath { path: String },
    #[error("work already claimed by {holder} until {until}")]
    WorkHeld { holder: String, until: String },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("identifier source failed: {0}")]
    IdSource(String),
    #[error("{STALE_LANDING}")]
    StaleLanding,
    #[error("landing lease held by {holder} until {until}")]
    LandingHeld { holder: String, until: String },
    #[error("number out of range")]
    NumberOutOfRange,
    #[error("Invalid time value")]
    InvalidTime,
    #[error("busy timeout must be between 1 and 5000 milliseconds")]
    BusyTimeout,
    #[error("session id must be 1 to 128 characters")]
    SessionLength,
    #[error("session id must not contain /")]
    SessionSlash,
    #[error("lease seconds must be between 1 and 3600")]
    LeaseSeconds,
    #[error("expected integration SHA must be 40 lowercase hex digits")]
    ExpectedSha,
    #[error("integration SHA does not match landing lease")]
    ShaMismatch,
    #[error("landing push already in progress")]
    PushInProgress,
    #[error("task id must not be empty")]
    EmptyTaskId,
    #[error("output path must not be empty")]
    EmptyOutputPath,
    #[error("inbox limit must be positive")]
    InboxLimit,
    #[error("claim TTL must be between 1 and 3600000 milliseconds")]
    ClaimTtl,
    #[error("invalid notice busy timeout")]
    NoticeTimeout,
    #[error("recipient id \"all\" is reserved for broadcast")]
    ReservedRecipient,
    #[error("state claimed is reserved for atomic task claim")]
    ReservedClaimedState,
    #[error("activity cursor must be a non-negative integer")]
    ActivityCursor,
    #[error("activity limit must be positive")]
    ActivityLimit,
    #[error("session {name} no longer holds this turn")]
    SessionTurnLost { name: String },
    #[error("reply parent {id} does not exist in project {project}")]
    ReplyParentMissing { id: String, project: String },
    #[error("task {task} does not exist in project {project}")]
    LinkedTaskMissing { task: String, project: String },
    #[error("no delivery {message} for {agent} in {project}")]
    DeliveryMissing {
        message: String,
        agent: String,
        project: String,
    },
    #[error("claim {claim} does not own delivery {message}")]
    DeliveryNotOwned { claim: String, message: String },
    #[error("claim {claim} for delivery {message} has expired")]
    DeliveryExpired { claim: String, message: String },
    #[error("agent label {agent} collides with an existing team id in {project}")]
    AgentTeamCollision { agent: String, project: String },
    #[error("agent label {agent} in {project} already belongs to client seat {existing}; refusing seat {refused}")]
    SeatMismatch {
        agent: String,
        project: String,
        existing: String,
        refused: String,
    },
    #[error("team id {team} collides with an agent label in {project}")]
    TeamAgentCollision { team: String, project: String },
    #[error("no task {task} in {project}")]
    TaskMissing { task: String, project: String },
    #[error("task {task} is already assigned to {owner}")]
    TaskAssigned { task: String, owner: String },
    #[error("task {task} is not open and unowned in {project}")]
    TaskNotOpen { task: String, project: String },
    #[error("task {task} vanished during update")]
    TaskVanished { task: String },
    #[error("unbound recipient \"{to}\"; fix: pass to:<seat> from these valid recipients: labels/seats: {}; teams: {}; all", recipient_list(.agents), recipient_list(.teams))]
    UnboundRecipient {
        to: String,
        agents: Vec<String>,
        teams: Vec<String>,
    },
}
fn recipient_list(values: &[String]) -> String {
    if values.is_empty() {
        "none".into()
    } else {
        values.join(", ")
    }
}

pub fn is_sqlite_contention(mut error: &(dyn std::error::Error + 'static)) -> bool {
    loop {
        let sqlite = match error.downcast_ref::<Error>() {
            Some(Error::Sqlite(sqlite)) => Some(sqlite),
            _ => error.downcast_ref::<rusqlite::Error>(),
        };
        if matches!(sqlite, Some(rusqlite::Error::SqliteFailure(e, _)) if matches!(e.extended_code, 5 | 6))
        {
            return true;
        }
        match error.source() {
            Some(source) => error = source,
            None => return false,
        }
    }
}

pub use pending::{IdSource, Identity};

/// Time is sampled inside write transactions where the source samples it there.
pub trait Clock: Send {
    fn now_ms(&self) -> i64;
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        jiff::Timestamp::now().as_millisecond()
    }
}

pub struct Store {
    pub(crate) db: Connection,
    clock: Box<dyn Clock>,
}
impl Store {
    /// Create missing parents with mode 0700, then bootstrap exactly this path.
    pub fn open(path: &Path, busy_timeout_ms: u64, clock: impl Clock + 'static) -> Result<Self> {
        Self::open_with_filesystem(
            path,
            busy_timeout_ms,
            clock,
            &provenant_supervisor::fs::LocalFs,
        )
    }
    pub fn open_with_filesystem(
        path: &Path,
        busy_timeout_ms: u64,
        clock: impl Clock + 'static,
        fs: &dyn provenant_supervisor::fs::SafeFs,
    ) -> Result<Self> {
        validate_timeout(busy_timeout_ms)?;
        let parent = filesystem::parent_and_name(path).0;
        filesystem::directory(fs, parent, true)
            .map_err(|e| FilesystemError::new("mkdir", parent, e))?;
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_millis(busy_timeout_ms))?;
        let store = Self {
            db,
            clock: Box::new(clock),
        };
        let deadline = Instant::now() + Duration::from_millis(busy_timeout_ms);
        loop {
            let bootstrap = store
                .db
                .execute_batch(SCHEMA)
                .map_err(Error::from)
                .and_then(|_| store.ensure_message_link_columns());
            match bootstrap {
                Ok(()) => return Ok(store),
                Err(error) if is_sqlite_contention(&error) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        }
    }
    pub fn restore_default_busy_timeout(&self) -> Result<()> {
        self.db
            .busy_timeout(Duration::from_millis(DEFAULT_BUSY_TIMEOUT_MS))?;
        Ok(())
    }
    fn transaction<T>(&self, immediate: bool, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            if immediate {
                TransactionBehavior::Immediate
            } else {
                TransactionBehavior::Deferred
            },
        )?;
        let result = f()?;
        tx.commit()?;
        Ok(result)
    }
    fn now(&self) -> i64 {
        self.clock.now_ms()
    }
    fn log(&self, who: &Identity, kind: &str, detail: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO activity(project, agent_id, at, kind, detail) VALUES (?, ?, ?, ?, ?)",
            params![who.project, who.agent_id, self.now(), kind, detail],
        )?;
        Ok(())
    }
    fn ensure_message_link_columns(&self) -> Result<()> {
        let names = self
            .db
            .prepare("PRAGMA table_info(messages)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for name in ["task_id", "output_path"] {
            if names.iter().any(|column| column == name) {
                continue;
            }
            if let Err(error) = self
                .db
                .execute_batch(&format!("ALTER TABLE messages ADD COLUMN {name} TEXT"))
            {
                if !error
                    .to_string()
                    .to_lowercase()
                    .contains("duplicate column name")
                {
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }
}
fn validate_timeout(ms: u64) -> Result<()> {
    if !(1..=5000).contains(&ms) {
        return Err(Error::BusyTimeout);
    }
    Ok(())
}
fn iso(ms: i64) -> Result<String> {
    pending::js_iso(ms)
}
fn holder(who: &Identity, session: &str) -> Result<String> {
    if session.trim_matches(pending::js_whitespace).is_empty()
        || session.encode_utf16().count() > 128
    {
        return Err(Error::SessionLength);
    }
    if session.contains('/') {
        return Err(Error::SessionSlash);
    }
    Ok(format!("{}/{session}", who.agent_id))
}
fn expiry(seconds: i64, now: i64) -> Result<i64> {
    if !(1..=3600).contains(&seconds) {
        return Err(Error::LeaseSeconds);
    }
    now.checked_add(seconds * 1000)
        .ok_or(Error::NumberOutOfRange)
}

#[cfg(test)]
mod differential;
#[cfg(test)]
mod oracle;
#[cfg(test)]
mod tests;
