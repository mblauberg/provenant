//! Durable per-session chair updates.
use crate::{persistence, session::HostSessionId};
use provenant_core::{CancelToken, Clock, OperationId, ProjectId, StateRoot, WaitBudget};
use provenant_os::fs::{Directory, FileKind, FsError, LockGuard, LockMode};
use provenant_records::{CanonicalV1, Limits, decode_v1};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, path::PathBuf};

pub const ENTRY_LIMIT: usize = 16 * 1024;
pub const PAGE_LIMIT: usize = 100;
pub const FUTURE_SKEW_SECONDS: i64 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Append {
    pub operation_id: OperationId,
    /// Unix seconds, fixed by the caller when the operation is first created.
    pub issued_at: i64,
    pub host: String,
    pub kind: String,
    pub markdown: String,
    #[serde(default)]
    pub request_ids: Vec<String>,
    #[serde(default)]
    pub attempt_ids: Vec<String>,
    #[serde(default)]
    pub issue_ids: Vec<String>,
    #[serde(default)]
    pub pr_ids: Vec<String>,
    pub channel: String,
    #[serde(default)]
    pub correction_of: Option<u64>,
}
impl Append {
    pub fn from_json(
        source: &str,
        bytes: &[u8],
    ) -> Result<(Self, Vec<provenant_records::InputWarning>), serde_json::Error> {
        provenant_records::decode_input(source, bytes)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub schema_version: u64,
    pub id: String,
    pub sequence: u64,
    pub project_id: ProjectId,
    pub session_id: HostSessionId,
    pub created_at: i64,
    #[serde(flatten)]
    pub update: Append,
}
#[derive(Debug, Clone, Copy)]
pub struct Retention {
    pub max_age_seconds: u64,
    pub max_entries: u64,
    pub max_bytes: u64,
    pub replay_horizon_seconds: u64,
}
impl Default for Retention {
    fn default() -> Self {
        Self {
            max_age_seconds: 30 * 86400,
            max_entries: 10_000,
            max_bytes: 16 * 1024 * 1024,
            replay_horizon_seconds: 30 * 86400,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub entries: Vec<LogEntry>,
    pub next_cursor: Option<u64>,
    pub total_appended: u64,
    pub total_pruned: u64,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Canonical(#[from] provenant_records::CanonicalError),
    #[error("chair log entry {sequence} could not be read: {source}")]
    Entry {
        sequence: u64,
        source: provenant_records::DecodeError,
    },
    #[error("{0}")]
    Invalid(&'static str),
    #[error("operation ID was already used for a different update")]
    Conflict,
    #[error("chair log operation has expired")]
    Expired,
    #[error("chair log operation issued_at is too far in the future")]
    Future,
}

pub struct LogStore {
    root: StateRoot,
    project: ProjectId,
    session: HostSessionId,
}
impl LogStore {
    pub fn new(root: StateRoot, project: ProjectId, session: HostSessionId) -> Self {
        Self {
            root,
            project,
            session,
        }
    }
    fn dir(&self, create: bool) -> Result<Directory, FsError> {
        persistence::directory(
            &self.root,
            self.project,
            "chair-logs",
            self.session.as_str(),
            create,
        )
    }
    fn path(&self) -> PathBuf {
        self.root
            .as_path()
            .join("workflow/projects")
            .join(self.project.to_string())
            .join("chair-logs")
            .join(self.session.as_str())
            .join("log.sqlite3")
    }
    fn lock(dir: &Directory, write: bool) -> Result<Option<LockGuard>, Error> {
        let clock = provenant_os::clock::SystemClock::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5)
            .map_err(|_| Error::Invalid("invalid lock budget"))?;
        if write {
            Ok(Some(dir.lock(
                OsStr::new("log.lock"),
                LockMode::Exclusive,
                budget,
                &CancelToken::new(),
                &clock,
            )?))
        } else {
            match dir.lock_existing(
                OsStr::new("log.lock"),
                LockMode::Shared,
                budget,
                &CancelToken::new(),
                &clock,
            ) {
                Ok(lock) => Ok(Some(lock)),
                Err(e) if e.is_not_found() => Ok(None),
                Err(e) => Err(e.into()),
            }
        }
    }
    fn check_files(dir: &Directory) -> Result<(), Error> {
        for name in [
            "log.sqlite3",
            "log.sqlite3-journal",
            "log.sqlite3-wal",
            "log.sqlite3-shm",
        ] {
            match dir.metadata_at(OsStr::new(name), false) {
                Ok(m)
                    if m.kind == FileKind::Regular
                        && m.uid == provenant_os::fs::effective_uid()
                        && m.links == 1
                        && m.mode & 0o077 == 0 => {}
                Ok(_) => {
                    return Err(Error::Invalid(
                        "unsafe chair log database or companion file",
                    ));
                }
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    fn open(&self, dir: &Directory, write: bool) -> Result<Option<Connection>, Error> {
        if write {
            match dir.create_new(OsStr::new("log.sqlite3")) {
                Ok(file) => {
                    file.sync()?;
                    dir.sync()?;
                }
                Err(e) if e.is_exists() => {}
                Err(e) => return Err(e.into()),
            }
        } else {
            match dir.metadata_at(OsStr::new("log.sqlite3"), false) {
                Ok(_) => {}
                Err(e) if e.is_not_found() => return Ok(None),
                Err(e) => return Err(e.into()),
            }
        }
        Self::check_files(dir)?;
        let db = provenant_sqlite::open_connection(&self.path(), !write)?;
        let version = match provenant_sqlite::read_version(&db, 1) {
            Ok(version) => version,
            Err(provenant_sqlite::VersionError::Sqlite(error)) => return Err(error.into()),
            Err(_) => {
                return Err(Error::Invalid(
                    "invalid or unsupported chair log database version",
                ));
            }
        };
        if write && version == 0 {
            db.execute_batch("PRAGMA auto_vacuum=FULL; PRAGMA synchronous=FULL; BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS meta (id INTEGER PRIMARY KEY CHECK(id=1), total_appended INTEGER NOT NULL, total_pruned INTEGER NOT NULL, expiry_cutoff INTEGER NOT NULL);
                INSERT OR IGNORE INTO meta VALUES (1, 0, 0, -9223372036854775808);
                CREATE TABLE IF NOT EXISTS entries (sequence INTEGER PRIMARY KEY, created_at INTEGER NOT NULL, bytes INTEGER NOT NULL, payload BLOB NOT NULL);
                CREATE INDEX IF NOT EXISTS entries_age ON entries(created_at);
                CREATE TABLE IF NOT EXISTS receipts (operation_id TEXT PRIMARY KEY, issued_at INTEGER NOT NULL, digest TEXT NOT NULL, sequence INTEGER NOT NULL, created_at INTEGER NOT NULL, host TEXT NOT NULL);
                CREATE INDEX IF NOT EXISTS receipts_age ON receipts(issued_at);
                PRAGMA user_version=1; COMMIT;")?;
        } else if !write && version == 0 {
            return Ok(None);
        }
        if write {
            db.execute_batch("PRAGMA synchronous=FULL;")?;
        }
        Self::check_files(dir)?;
        Ok(Some(db))
    }
    fn decode_entry(sequence: u64, payload: &[u8]) -> Result<LogEntry, Error> {
        let entry: LogEntry = decode_v1(
            payload,
            Limits {
                max_bytes: (ENTRY_LIMIT * 2) as u64,
                ..Limits::default()
            },
        )
        .map_err(|source| Error::Entry { sequence, source })?;
        if entry.sequence != sequence {
            return Err(Error::Invalid("chair log entry sequence mismatch"));
        }
        Ok(entry)
    }
    fn cutoff(now: i64, seconds: u64) -> i64 {
        now.saturating_sub(seconds.min(i64::MAX as u64) as i64)
    }
    fn positive(value: i64) -> Result<u64, Error> {
        u64::try_from(value).map_err(|_| Error::Invalid("invalid chair log database counter"))
    }
    fn empty_page() -> Page {
        Page {
            entries: vec![],
            next_cursor: None,
            total_appended: 0,
            total_pruned: 0,
        }
    }
    fn check_retention(retention: Retention) -> Result<(), Error> {
        if retention.max_entries == 0
            || retention.max_bytes < (ENTRY_LIMIT * 2) as u64
            || retention.max_age_seconds == 0
            || retention.replay_horizon_seconds == 0
            || retention.max_entries > i64::MAX as u64
            || retention.max_bytes > i64::MAX as u64
        {
            return Err(Error::Invalid("invalid chair log retention"));
        }
        Ok(())
    }
    // Select the newest age-eligible suffix that fits both count and byte
    // limits. This is computed before applying a page cursor.
    fn retention_view(
        db: &Connection,
        retention: Retention,
        now: i64,
    ) -> Result<(Option<i64>, u64), Error> {
        let (first, removed): (Option<i64>, i64) = db.query_row(
            "WITH ranked AS (
                 SELECT sequence, row_number() OVER (ORDER BY sequence DESC) AS position,
                        sum(bytes) OVER (ORDER BY sequence DESC ROWS UNBOUNDED PRECEDING) AS used
                 FROM entries WHERE created_at >= ?1
             ), kept AS (SELECT sequence FROM ranked WHERE position <= ?2 AND used <= ?3)
             SELECT (SELECT min(sequence) FROM kept),
                    (SELECT count(*) FROM entries) - (SELECT count(*) FROM kept)",
            params![
                Self::cutoff(now, retention.max_age_seconds),
                retention.max_entries as i64,
                retention.max_bytes as i64
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((first, Self::positive(removed)?))
    }
    fn prune(db: &Connection, retention: Retention, now: i64) -> Result<u64, Error> {
        let cutoff = Self::cutoff(now, retention.replay_horizon_seconds);
        db.execute(
            "UPDATE meta SET expiry_cutoff=max(expiry_cutoff, ?1) WHERE id=1",
            [cutoff],
        )?;
        db.execute(
            "DELETE FROM receipts WHERE issued_at < (SELECT expiry_cutoff FROM meta WHERE id=1)",
            [],
        )?;
        let (first, _) = Self::retention_view(db, retention, now)?;
        let removed = db.execute(
            "DELETE FROM entries WHERE created_at < ?1 OR ?2 IS NULL OR sequence < ?2",
            params![Self::cutoff(now, retention.max_age_seconds), first],
        )?;
        db.execute(
            "UPDATE meta SET total_pruned=total_pruned+?1 WHERE id=1",
            [removed as i64],
        )?;
        Ok(removed as u64)
    }
    /// Apply all payload limits and prune expired receipts. Dry-run does not write or create files.
    pub fn gc(&self, retention: Retention, now: i64, dry_run: bool) -> Result<u64, Error> {
        Self::check_retention(retention)?;
        let dir = match self.dir(false) {
            Ok(dir) => dir,
            Err(e) if e.is_not_found() => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let _lock = Self::lock(&dir, !dry_run)?;
        if _lock.is_none() {
            return Ok(0);
        }
        let Some(mut db) = self.open(&dir, !dry_run)? else {
            return Ok(0);
        };
        if dry_run {
            return Ok(Self::retention_view(&db, retention, now)?.1);
        }
        let tx = db.transaction()?;
        let count = Self::prune(&tx, retention, now)?;
        tx.commit()?;
        Ok(count)
    }
    pub fn append(&self, input: Append, retention: Retention, now: i64) -> Result<LogEntry, Error> {
        let encoded = serde_json::to_vec(&input)?;
        if encoded.len() > ENTRY_LIMIT
            || input.markdown.trim().is_empty()
            || input.host.is_empty()
            || input.kind.is_empty()
            || input.channel.is_empty()
        {
            return Err(Error::Invalid(
                "chair log update must contain host, kind, channel and markdown, and fit in 16 KiB",
            ));
        }
        Self::check_retention(retention)?;
        if input.issued_at > now.saturating_add(FUTURE_SKEW_SECONDS) {
            return Err(Error::Future);
        }
        if input.issued_at < Self::cutoff(now, retention.replay_horizon_seconds) {
            return Err(Error::Expired);
        }
        #[derive(Serialize)]
        struct Payload<'a> {
            issued_at: i64,
            kind: &'a str,
            markdown: &'a str,
            request_ids: &'a [String],
            attempt_ids: &'a [String],
            issue_ids: &'a [String],
            pr_ids: &'a [String],
            channel: &'a str,
            correction_of: Option<u64>,
        }
        let digest = CanonicalV1::record_digest(
            "chair-log-append",
            1,
            &Payload {
                issued_at: input.issued_at,
                kind: &input.kind,
                markdown: &input.markdown,
                request_ids: &input.request_ids,
                attempt_ids: &input.attempt_ids,
                issue_ids: &input.issue_ids,
                pr_ids: &input.pr_ids,
                channel: &input.channel,
                correction_of: input.correction_of,
            },
        )?
        .to_string();
        let dir = self.dir(true)?;
        let _lock = Self::lock(&dir, true)?;
        let mut db = self
            .open(&dir, true)?
            .ok_or(Error::Invalid("missing chair log database"))?;
        let tx = db.transaction()?;
        let (total, floor): (i64, i64) = tx.query_row(
            "SELECT total_appended, expiry_cutoff FROM meta WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if input.issued_at < floor {
            return Err(Error::Expired);
        }
        let old: Option<(String, i64, i64, String)> = tx
            .query_row(
                "SELECT digest,sequence,created_at,host FROM receipts WHERE operation_id=?1",
                [input.operation_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((old_digest, sequence, created_at, host)) = old {
            if old_digest != digest {
                return Err(Error::Conflict);
            }
            let sequence = Self::positive(sequence)?;
            let payload: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT payload FROM entries WHERE sequence=?1",
                    [sequence as i64],
                    |r| r.get(0),
                )
                .optional()?;
            return if let Some(payload) = payload {
                Self::decode_entry(sequence, &payload)
            } else {
                let mut input = input;
                input.host = host;
                Ok(LogEntry {
                    schema_version: 1,
                    id: format!("{}-{sequence}", self.session.as_str()),
                    sequence,
                    project_id: self.project,
                    session_id: self.session.clone(),
                    created_at,
                    update: input,
                })
            };
        }
        let sequence = total
            .checked_add(1)
            .ok_or(Error::Invalid("chair log sequence exhausted"))?;
        if input
            .correction_of
            .is_some_and(|id| id == 0 || id >= sequence as u64)
        {
            return Err(Error::Invalid("correction must refer to an earlier entry"));
        }
        let entry = LogEntry {
            schema_version: 1,
            id: format!("{}-{sequence}", self.session.as_str()),
            sequence: sequence as u64,
            project_id: self.project,
            session_id: self.session.clone(),
            created_at: now,
            update: input,
        };
        let payload = serde_json::to_vec(&entry)?;
        tx.execute(
            "INSERT INTO entries VALUES (?1,?2,?3,?4)",
            params![sequence, now, payload.len() as i64, payload],
        )?;
        tx.execute(
            "INSERT INTO receipts VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                entry.update.operation_id.as_str(),
                entry.update.issued_at,
                digest,
                sequence,
                now,
                entry.update.host
            ],
        )?;
        tx.execute("UPDATE meta SET total_appended=?1 WHERE id=1", [sequence])?;
        Self::prune(&tx, retention, now)?;
        tx.commit()?;
        Ok(entry)
    }
    pub fn page(
        &self,
        after: Option<u64>,
        limit: usize,
        retention: Retention,
        now: i64,
    ) -> Result<Page, Error> {
        if limit == 0 || limit > PAGE_LIMIT {
            return Err(Error::Invalid("chair log page limit must be 1-100"));
        }
        Self::check_retention(retention)?;
        let dir = match self.dir(false) {
            Ok(dir) => dir,
            Err(e) if e.is_not_found() => return Ok(Self::empty_page()),
            Err(e) => return Err(e.into()),
        };
        let _lock = Self::lock(&dir, false)?;
        if _lock.is_none() {
            return Ok(Self::empty_page());
        }
        let Some(db) = self.open(&dir, false)? else {
            return Ok(Self::empty_page());
        };
        let (total, pruned): (i64, i64) = db.query_row(
            "SELECT total_appended,total_pruned FROM meta WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (first, excluded) = Self::retention_view(&db, retention, now)?;
        let cursor = after.unwrap_or(0).min(i64::MAX as u64) as i64;
        let mut stmt = db.prepare("SELECT sequence,bytes,payload FROM entries WHERE sequence>?1 AND created_at>=?2 AND ?4 IS NOT NULL AND sequence>=?4 ORDER BY sequence LIMIT ?3")?;
        let mut rows = stmt.query(params![
            cursor,
            Self::cutoff(now, retention.max_age_seconds),
            (limit + 1) as i64,
            first
        ])?;
        let mut entries: Vec<LogEntry> = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        while let Some(row) = rows.next()? {
            let sequence = Self::positive(row.get(0)?)?;
            let size = usize::try_from(row.get::<_, i64>(1)?)
                .map_err(|_| Error::Invalid("invalid chair log entry size"))?;
            if size > ENTRY_LIMIT * 2 {
                return Err(Error::Invalid("chair log entry exceeds size limit"));
            }
            if entries.len() == limit || bytes + size > 64 * 1024 {
                more = true;
                break;
            }
            let payload: Vec<u8> = row.get(2)?;
            if payload.len() != size {
                return Err(Error::Invalid("invalid chair log entry size"));
            }
            entries.push(Self::decode_entry(sequence, &payload)?);
            bytes += size;
        }
        Ok(Page {
            next_cursor: if more {
                entries.last().map(|entry| entry.sequence)
            } else {
                None
            },
            entries,
            total_appended: Self::positive(total)?,
            total_pruned: Self::positive(pruned)?
                .checked_add(excluded)
                .ok_or(Error::Invalid("invalid chair log counter"))?,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use provenant_core::{PathResolver, RootInputs};
    use std::path::{Path, PathBuf};
    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
            Ok(path.into())
        }
    }
    fn fixture() -> (tempfile::TempDir, LogStore) {
        let tmp = crate::test_support::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let state = base.join("state");
        std::fs::create_dir(&state).unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(state);
        let root = StateRoot::resolve(&inputs, &Resolver).unwrap();
        let store = LogStore::new(
            root,
            ProjectId::from_bytes([1; 16]),
            HostSessionId::parse("chair").unwrap(),
        );
        (tmp, store)
    }
    fn update(op: &str, issued_at: i64, text: &str) -> Append {
        Append {
            operation_id: OperationId::parse(op).unwrap(),
            issued_at,
            host: "host".into(),
            kind: "update".into(),
            markdown: text.into(),
            request_ids: vec![],
            attempt_ids: vec![],
            issue_ids: vec![],
            pr_ids: vec![],
            channel: "chat".into(),
            correction_of: None,
        }
    }
    #[test]
    fn log_input_reports_unknown_fields_and_preserves_required_values() {
        let mut value = serde_json::to_value(update("op-open", 1, "kept")).unwrap();
        value["future"] = serde_json::json!({"nested":true});
        let (input, warnings) =
            Append::from_json("log.json", &serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(input.markdown, "kept");
        assert_eq!(warnings[0].source, "log.json");
        assert_eq!(warnings[0].key, "future");
        value["issued_at"] = serde_json::json!("invalid");
        assert!(Append::from_json("log.json", &serde_json::to_vec(&value).unwrap()).is_err());
    }
    fn retention() -> Retention {
        Retention {
            max_age_seconds: 100,
            max_entries: 2,
            max_bytes: 65536,
            replay_horizon_seconds: 10,
        }
    }
    #[test]
    fn log_operation_replay_pagination_correction_pruning_and_restart() {
        let (_tmp, store) = fixture();
        let retention = retention();
        let first = update("op-1", 1, "first");
        assert_eq!(
            store.append(first.clone(), retention, 1).unwrap().sequence,
            1
        );
        assert_eq!(
            store.append(first.clone(), retention, 2).unwrap().sequence,
            1
        );
        assert!(matches!(
            store.append(update("op-1", 1, "changed"), retention, 2),
            Err(Error::Conflict)
        ));
        assert!(matches!(
            store.append(update("op-1", 2, "first"), retention, 2),
            Err(Error::Conflict)
        ));
        let mut handoff = first.clone();
        handoff.host = "other-host".into();
        assert_eq!(store.append(handoff, retention, 2).unwrap().sequence, 1);
        let mut correction = update("op-2", 3, "correction");
        correction.correction_of = Some(1);
        store.append(correction, retention, 3).unwrap();
        let page = store.page(None, 1, retention, 3).unwrap();
        assert_eq!(page.next_cursor, Some(1));
        assert_eq!(
            store
                .page(page.next_cursor, 1, retention, 3)
                .unwrap()
                .entries[0]
                .update
                .correction_of,
            Some(1)
        );
        store
            .append(update("op-3", 4, "third"), retention, 4)
            .unwrap();
        let page = store.page(None, 100, retention, 4).unwrap();
        assert_eq!(
            page.entries.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!((page.total_appended, page.total_pruned), (3, 1));
        let restarted = LogStore::new(store.root.clone(), store.project, store.session.clone());
        assert_eq!(restarted.append(first, retention, 5).unwrap().sequence, 1);
        assert_eq!(store.gc(retention, 105, true).unwrap(), 2);
        assert_eq!(store.gc(retention, 105, false).unwrap(), 2);
        let page = store.page(None, 100, retention, 105).unwrap();
        assert!(page.entries.is_empty());
        assert_eq!((page.total_appended, page.total_pruned), (3, 3));
    }
    #[test]
    fn replay_horizon_expires_receipts_and_survives_clock_and_config_changes() {
        let (_tmp, store) = fixture();
        let mut retention = retention();
        retention.max_entries = 100;
        let original = update("old", 100, "payload");
        store.append(original.clone(), retention, 100).unwrap();
        assert_eq!(
            store
                .append(original.clone(), retention, 110)
                .unwrap()
                .sequence,
            1
        );
        assert!(matches!(
            store.append(original.clone(), retention, 111),
            Err(Error::Expired)
        ));
        store
            .append(update("new", 111, "new"), retention, 111)
            .unwrap();
        let dir = store.dir(false).unwrap();
        let db = store.open(&dir, false).unwrap().unwrap();
        let old_count: i64 = db
            .query_row(
                "SELECT count(*) FROM receipts WHERE operation_id='old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_count, 0);
        assert_eq!(
            store.page(None, 100, retention, 111).unwrap().entries.len(),
            2
        );
        drop(db);
        retention.replay_horizon_seconds = 1000;
        assert!(matches!(
            store.append(original.clone(), retention, 111),
            Err(Error::Expired)
        ));
        assert!(matches!(
            store.append(original, retention, 101),
            Err(Error::Expired)
        ));
        assert!(matches!(
            store.append(update("future", 412, "future"), retention, 111),
            Err(Error::Future)
        ));
        assert_eq!(
            store
                .append(update("future", 411, "future"), retention, 111)
                .unwrap()
                .sequence,
            3
        );
    }
    #[test]
    fn byte_retention_and_page_budget_remain_oldest_first() {
        let (_tmp, store) = fixture();
        let mut policy = retention();
        policy.max_entries = 100;
        policy.max_bytes = 100_000;
        for n in 0..9 {
            store
                .append(update(&format!("op-{n}"), 1, &"x".repeat(9_000)), policy, 1)
                .unwrap();
        }
        let first = store.page(None, 100, policy, 1).unwrap();
        assert_eq!(first.entries.len(), 7);
        assert_eq!(first.next_cursor, Some(7));
        let second = store.page(first.next_cursor, 100, policy, 1).unwrap();
        assert_eq!(second.entries.len(), 2);
        assert_eq!(second.next_cursor, None);
        policy.max_bytes = 32_768;
        store
            .append(update("last", 2, &"x".repeat(9_000)), policy, 2)
            .unwrap();
        let page = store.page(None, 100, policy, 2).unwrap();
        assert_eq!(
            page.entries.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![8, 9, 10]
        );
        assert_eq!(page.total_pruned, 7);
    }
    #[test]
    fn failed_pruning_rolls_back_append_and_expiry_boundary() {
        let (_tmp, store) = fixture();
        let retention = retention();
        let first = update("first", 1, "first");
        store.append(first.clone(), retention, 1).unwrap();
        let dir = store.dir(false).unwrap();
        let db = store.open(&dir, true).unwrap().unwrap();
        db.execute_batch("CREATE TRIGGER fail_prune BEFORE DELETE ON receipts BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
        drop(db);
        assert!(matches!(
            store.append(update("second", 20, "second"), retention, 20),
            Err(Error::Sqlite(_))
        ));
        let page = store.page(None, 100, retention, 1).unwrap();
        assert_eq!(
            (page.total_appended, page.total_pruned, page.entries.len()),
            (1, 0, 1)
        );
        assert_eq!(store.append(first, retention, 1).unwrap().sequence, 1);
        let db = store.open(&dir, true).unwrap().unwrap();
        db.execute_batch("DROP TRIGGER fail_prune;").unwrap();
        drop(db);
        assert_eq!(
            store
                .append(update("second", 20, "second"), retention, 20)
                .unwrap()
                .sequence,
            2
        );
    }
    #[test]
    fn reads_create_nothing_and_reject_duplicate_record_fields() {
        let (_tmp, store) = fixture();
        let retention = retention();
        assert!(
            store
                .page(None, 10, retention, 1)
                .unwrap()
                .entries
                .is_empty()
        );
        assert!(store.dir(false).unwrap_err().is_not_found());
        store
            .append(update("first", 1, "first"), retention, 1)
            .unwrap();
        let dir = store.dir(false).unwrap();
        let db = store.open(&dir, true).unwrap().unwrap();
        let corrupt = br#"{"schema_version":1,"schema_version":1}"#;
        db.execute(
            "UPDATE entries SET payload=?1, bytes=?2 WHERE sequence=1",
            params![corrupt.as_slice(), corrupt.len() as i64],
        )
        .unwrap();
        drop(db);
        assert!(matches!(
            store.page(None, 10, retention, 1),
            Err(Error::Entry {
                sequence: 1,
                source: provenant_records::DecodeError::DuplicateMember { .. }
            })
        ));
    }
    #[test]
    fn list_returns_empty_if_initialisation_lock_is_missing() {
        let (_tmp, store) = fixture();
        let policy = retention();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let store = &store;
            let writer = scope.spawn(move || {
                let dir = store.dir(true).unwrap();
                // Model a reader that has observed the missing lock just before
                // the first writer creates it and initialises the database.
                let db = store.open(&dir, true).unwrap().unwrap();
                db.execute("UPDATE meta SET total_appended=1 WHERE id=1", [])
                    .unwrap();
                ready_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
                drop(db);
            });
            ready_rx.recv().unwrap();
            let page = store.page(None, 10, policy, 1);
            let gc_count = store.gc(policy, 1, true);
            resume_tx.send(()).unwrap();
            writer.join().unwrap();
            let page = page.unwrap();
            assert_eq!(page.total_appended, 0);
            assert!(page.entries.is_empty());
            assert_eq!(gc_count.unwrap(), 0);
        });
    }
    #[test]
    fn reduced_count_and_byte_limits_apply_to_list_and_gc() {
        for bytes_case in [false, true] {
            let (_tmp, store) = fixture();
            let mut policy = retention();
            policy.max_entries = 10;
            policy.max_bytes = 100_000;
            for n in 0..5 {
                store
                    .append(
                        update(&format!("op-{n}"), n + 1, &"x".repeat(9_000)),
                        policy,
                        n + 1,
                    )
                    .unwrap();
            }
            if bytes_case {
                policy.max_bytes = 32_768;
            } else {
                policy.max_entries = 3;
            }
            let page = store.page(None, 2, policy, 5).unwrap();
            assert_eq!(
                page.entries.iter().map(|e| e.sequence).collect::<Vec<_>>(),
                vec![3, 4]
            );
            assert_eq!(
                (page.total_appended, page.total_pruned, page.next_cursor),
                (5, 2, Some(4))
            );
            assert_eq!(
                store.page(Some(4), 10, policy, 5).unwrap().entries[0].sequence,
                5
            );
            assert_eq!(store.gc(policy, 5, true).unwrap(), 2);
            let dir = store.dir(false).unwrap();
            let db = store.open(&dir, false).unwrap().unwrap();
            assert_eq!(
                db.query_row("SELECT count(*) FROM entries", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                5
            );
            drop(db);
            assert_eq!(store.gc(policy, 5, false).unwrap(), 2);
            assert_eq!(store.gc(policy, 5, false).unwrap(), 0);
            let page = store.page(None, 10, policy, 5).unwrap();
            assert_eq!(
                (page.total_appended, page.total_pruned, page.entries.len()),
                (5, 2, 3)
            );
        }
    }
    #[test]
    fn age_filter_precedes_count_retention_when_clock_moves_backwards() {
        let (_tmp, store) = fixture();
        let mut policy = retention();
        policy.max_entries = 10;
        for (n, now) in [10, 4, 10].into_iter().enumerate() {
            store
                .append(update(&format!("op-{n}"), now, "payload"), policy, now)
                .unwrap();
        }
        policy.max_age_seconds = 5;
        policy.max_entries = 1;
        let page = store.page(None, 10, policy, 10).unwrap();
        assert_eq!(
            page.entries.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![3]
        );
        assert_eq!(page.total_pruned, 2);
        assert_eq!(store.gc(policy, 10, false).unwrap(), 2);
        assert_eq!(store.page(None, 10, policy, 10).unwrap().total_pruned, 2);
    }
    #[test]
    fn replay_after_pruning_preserves_original_host() {
        let (_tmp, store) = fixture();
        let mut policy = retention();
        policy.max_entries = 1;
        let mut first = update("first", 1, "payload");
        first.host = "host-a".into();
        let original = store.append(first.clone(), policy, 1).unwrap();
        store
            .append(update("second", 2, "other"), policy, 2)
            .unwrap();
        first.host = "host-b".into();
        assert_eq!(store.append(first, policy, 2).unwrap(), original);
    }
    #[test]
    fn replay_store_has_no_lifetime_receipt_cap() {
        let (_tmp, store) = fixture();
        let retention = retention();
        store
            .append(update("first", 1, "first"), retention, 1)
            .unwrap();
        let dir = store.dir(false).unwrap();
        let mut db = store.open(&dir, true).unwrap().unwrap();
        let tx = db.transaction().unwrap();
        {
            let mut insert = tx
                .prepare("INSERT INTO receipts VALUES(?1,?2,'digest',?3,?2,'host')")
                .unwrap();
            for n in 0..100_001 {
                insert
                    .execute(params![format!("synthetic-{n}"), 1, n + 2])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        drop(db);
        assert_eq!(
            store
                .append(update("next", 2, "next"), retention, 2)
                .unwrap()
                .sequence,
            2
        );
        assert_eq!(
            store.page(None, 100, retention, 2).unwrap().total_appended,
            2
        );
        assert_eq!(
            store
                .append(update("later", 20, "later"), retention, 20)
                .unwrap()
                .sequence,
            3
        );
        let db = store.open(&dir, false).unwrap().unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
