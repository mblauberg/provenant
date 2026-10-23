//! Inert Fabric SQLite owner. Connections stay on their creating thread.
#![forbid(unsafe_code)]

mod diagnostics;
mod landing;
mod messages;
mod pending;
mod sessions;
mod tasks;

pub use diagnostics::*;
pub use landing::*;
pub use messages::*;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
pub use sessions::*;
use std::{
    marker::PhantomData,
    path::Path,
    rc::Rc,
    time::{Duration, Instant},
};
pub use tasks::*;

pub const SCHEMA: &str = include_str!("../../../../../runtime/fabric/schema.sql");
pub const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5000;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("{STALE_LANDING}")]
    StaleLanding,
    #[error("landing lease held by {holder} until {until}")]
    LandingHeld { holder: String, until: String },
    #[error("number out of range")]
    NumberOutOfRange,
    #[error("Invalid time value")]
    InvalidTime,
}

pub fn is_sqlite_contention(error: &Error) -> bool {
    matches!(error, Error::Sqlite(rusqlite::Error::SqliteFailure(e, _)) if matches!(e.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

pub use pending::{IdSource, Identity};

/// Time is sampled inside write transactions where the source samples it there.
pub trait Clock {
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
    // rusqlite connections can move threads; Fabric's owner contract is stronger.
    _thread_owner: PhantomData<Rc<()>>,
}
impl Store {
    /// The runtime owns directory creation. This opens exactly the supplied path.
    pub fn open(path: &Path, busy_timeout_ms: u64, clock: impl Clock + 'static) -> Result<Self> {
        validate_timeout(busy_timeout_ms)?;
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_millis(busy_timeout_ms))?;
        let store = Self {
            db,
            clock: Box::new(clock),
            _thread_owner: PhantomData,
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
fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
fn validate_timeout(ms: u64) -> Result<()> {
    if !(1..=5000).contains(&ms) {
        return Err(invalid(
            "busy timeout must be between 1 and 5000 milliseconds",
        ));
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
        return Err(invalid("session id must be 1 to 128 characters"));
    }
    if session.contains('/') {
        return Err(invalid("session id must not contain /"));
    }
    Ok(format!("{}/{session}", who.agent_id))
}
fn expiry(seconds: i64, now: i64) -> Result<i64> {
    if !(1..=3600).contains(&seconds) {
        return Err(invalid("lease seconds must be between 1 and 3600"));
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
