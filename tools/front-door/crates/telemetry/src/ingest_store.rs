//! Verified JSONL handles and SQLite cursor persistence.

use crate::{
    Error, Result,
    ingest::{self, Cursor, CursorStore, ParserState, Snapshot},
    pending,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

/// The opened file and the stat values captured by the trusted opener.
pub struct VerifiedFile {
    pub stream: BufReader<File>,
    pub snapshot: Snapshot,
}

/// Safe owner seam for `open_verified`.
///
/// Implementations must refuse a final symlink, non-regular files and files
/// whose device/inode differs from `expected`. A supervisor-backed
/// implementation is blocked because R2 does not allow telemetry to depend on
/// supervisor.
pub trait VerifiedFileOpener {
    fn open_verified(&self, path: &Path, expected: Option<(u64, u64)>) -> Result<VerifiedFile>;
}

/// Run the JSONL cursor policy against a handle returned by the verified-open
/// owner. This keeps path discovery and OS-specific no-follow behavior outside
/// the telemetry crate.
pub fn ingest_verified_jsonl(
    opener: &impl VerifiedFileOpener,
    store: &mut impl CursorStore,
    source: &str,
    path: &Path,
    expected: Option<(u64, u64)>,
    parse: impl FnMut(&Value, &mut ParserState, u64) -> Result<()>,
) -> Result<()> {
    let mut opened = opener.open_verified(path, expected)?;
    ingest::ingest_stream(
        store,
        source,
        path,
        &mut opened.stream,
        opened.snapshot,
        parse,
    )
}

/// JSON form of a ledger cursor, also used for OpenCode's source-specific
/// watermark state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonCursor {
    pub source: String,
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub offset: u64,
    pub state: Value,
}

/// Storage contract shared by line cursors and source-specific cursors.
pub trait JsonCursorStore {
    fn load_json(&mut self, source: &str, path: &Path) -> Result<Option<JsonCursor>>;
    fn clear_file(&mut self, source: &str, path: &Path) -> Result<()>;
    fn save_json(&mut self, cursor: JsonCursor) -> Result<()>;
}

/// SQLite cursor owner. Use it inside the caller's ledger transaction.
pub struct SqliteCursorStore<'a> {
    connection: &'a Connection,
}

impl<'a> SqliteCursorStore<'a> {
    pub fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    fn path_text(path: &Path) -> Result<&str> {
        path.to_str()
            .ok_or_else(|| Error::Invalid("telemetry cursor path is not valid UTF-8".into()))
    }
}

impl JsonCursorStore for SqliteCursorStore<'_> {
    fn load_json(&mut self, source: &str, path: &Path) -> Result<Option<JsonCursor>> {
        let path_text = Self::path_text(path)?;
        let row = self
            .connection
            .query_row(
                "SELECT source,path,size,mtime,offset,state FROM cursors WHERE source=?1 AND path=?2",
                params![source, path_text],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(Error::Sqlite)?;
        row.map(|(source, path, size, mtime, offset, state)| {
            Ok(JsonCursor {
                source,
                path,
                size: u64::try_from(size)
                    .map_err(|_| Error::Invalid("negative telemetry cursor size".into()))?,
                mtime,
                offset: u64::try_from(offset)
                    .map_err(|_| Error::Invalid("negative telemetry cursor offset".into()))?,
                state: pending::decode_json(state.as_bytes())?,
            })
        })
        .transpose()
    }

    fn clear_file(&mut self, source: &str, path: &Path) -> Result<()> {
        let path = Self::path_text(path)?;
        let mut statement = self
            .connection
            .prepare("SELECT event_id,host FROM usage_files WHERE source=?1 AND path=?2")
            .map_err(Error::Sqlite)?;
        let rows = statement
            .query_map(params![source, path], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(Error::Sqlite)?;
        let mut owned = Vec::new();
        for row in rows {
            owned.push(row.map_err(Error::Sqlite)?);
        }
        drop(statement);
        self.connection
            .execute(
                "DELETE FROM usage_files WHERE source=?1 AND path=?2",
                params![source, path],
            )
            .map_err(Error::Sqlite)?;
        for (event_id, host) in owned {
            self.connection
                .execute(
                    "DELETE FROM usage WHERE source=?1 AND event_id=?2 AND host=?3 AND NOT EXISTS \
                     (SELECT 1 FROM usage_files f WHERE f.source=usage.source \
                     AND f.event_id=usage.event_id AND f.host=usage.host)",
                    params![source, event_id, host],
                )
                .map_err(Error::Sqlite)?;
        }
        self.connection
            .execute(
                "DELETE FROM observations WHERE source=?1 AND origin_path=?2",
                params![source, path],
            )
            .map_err(Error::Sqlite)?;
        Ok(())
    }

    fn save_json(&mut self, cursor: JsonCursor) -> Result<()> {
        let state = serde_json::to_string(&cursor.state)?;
        let size = i64::try_from(cursor.size).map_err(|_| Error::NumberOutOfRange)?;
        let offset = i64::try_from(cursor.offset).map_err(|_| Error::NumberOutOfRange)?;
        self.connection
            .execute(
                "INSERT OR REPLACE INTO cursors(source,path,size,mtime,offset,state) \
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    cursor.source,
                    cursor.path,
                    size,
                    cursor.mtime,
                    offset,
                    state
                ],
            )
            .map_err(Error::Sqlite)?;
        Ok(())
    }
}

impl CursorStore for SqliteCursorStore<'_> {
    fn load(&mut self, source: &str, path: &Path) -> Result<Option<Cursor>> {
        self.load_json(source, path)?
            .map(|cursor| {
                Ok(Cursor {
                    source: cursor.source,
                    path: cursor.path,
                    size: cursor.size,
                    mtime: cursor.mtime,
                    offset: cursor.offset,
                    state: serde_json::from_value(cursor.state)?,
                })
            })
            .transpose()
    }

    fn clear_file(&mut self, source: &str, path: &Path) -> Result<()> {
        JsonCursorStore::clear_file(self, source, path)
    }

    fn save(&mut self, cursor: Cursor) -> Result<()> {
        self.save_json(JsonCursor {
            source: cursor.source,
            path: cursor.path,
            size: cursor.size,
            mtime: cursor.mtime,
            offset: cursor.offset,
            state: serde_json::to_value(cursor.state)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_cursor_clear_keeps_rows_owned_by_another_file() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE cursors(source TEXT,path TEXT,size INTEGER,mtime INTEGER,offset INTEGER,state TEXT,PRIMARY KEY(source,path));\
                 CREATE TABLE usage(source TEXT,event_id TEXT,host TEXT,PRIMARY KEY(source,event_id,host));\
                 CREATE TABLE usage_files(source TEXT,event_id TEXT,host TEXT,path TEXT,PRIMARY KEY(source,event_id,host,path));\
                 CREATE TABLE observations(source TEXT,origin_path TEXT);\
                 INSERT INTO usage VALUES('claude','e','h');\
                 INSERT INTO usage_files VALUES('claude','e','h','one');\
                 INSERT INTO usage_files VALUES('claude','e','h','two');\
                 INSERT INTO observations VALUES('claude','one');",
            )
            .unwrap();
        let mut store = SqliteCursorStore::new(&connection);
        JsonCursorStore::clear_file(&mut store, "claude", Path::new("one")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM usage", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM observations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
