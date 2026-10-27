//! Host admission ledger. Host and project concurrency and startup spacing are
//! global to the host, so every execution namespace, top-level and nested child
//! stores alike, leases capacity in one SQLite file under the host state root.
//! Per-run concurrency stays in each store's own `admission` table.
//!
//! A lease names its owning store and lives exactly as long as that store's
//! admission row: the terminal commit, including owner recovery, deletes the
//! row and so frees the lease. A full ledger drops leases whose row is gone
//! before refusing, so terminal paths never write here and confined lanes,
//! which may cancel or recover their children, need no grant to this file.
use crate::{
    admission::{HOST_ACTIVE, PROJECT_ACTIVE},
    db::{immediate, open_database, sql_int},
    *,
};
use provenant_core::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};

const FILE: &str = "admission.sqlite3";
/// Each lease and each class's last start carry the lane class. Version 1 had
/// no class and one global spacing row: both read as writers.
const SCHEMA: &str = "
CREATE TABLE leases (
 attempt TEXT PRIMARY KEY, project TEXT NOT NULL, store BLOB NOT NULL,
 class TEXT NOT NULL DEFAULT 'writer' CHECK(class IN ('writer','read_only'))
) STRICT;
CREATE INDEX lease_projects ON leases(project);
CREATE TABLE spacing (
 class TEXT PRIMARY KEY CHECK(class IN ('writer','read_only')),
 last_start INTEGER NOT NULL
) STRICT;
PRAGMA user_version=2;
";
const UPGRADE_V1: &str = "
ALTER TABLE leases ADD COLUMN class TEXT NOT NULL DEFAULT 'writer' CHECK(class IN ('writer','read_only'));
CREATE TABLE spacing_by_class (
 class TEXT PRIMARY KEY CHECK(class IN ('writer','read_only')),
 last_start INTEGER NOT NULL
) STRICT;
INSERT INTO spacing_by_class(class,last_start) SELECT 'writer', last_start FROM spacing;
DROP TABLE spacing;
ALTER TABLE spacing_by_class RENAME TO spacing;
PRAGMA user_version=2;
";

/// Open, creating or upgrading if needed, the ledger under the trusted host
/// state root.
pub(crate) fn open(host: &Path, request: &Request<'_>) -> Result<Connection> {
    let (_, db, _initialisation) = open_database(host, FILE, request)?;
    if request.retry(|| version(&db))? < 2 {
        immediate(&db, request, |tx| {
            match version(tx)? {
                0 => tx.execute_batch(SCHEMA)?,
                1 => tx.execute_batch(UPGRADE_V1)?,
                _ => {}
            }
            Ok(())
        })?;
    }
    Ok(db)
}
fn version(db: &Connection) -> Result<i64> {
    match provenant_sqlite::read_version(db, 2) {
        Ok(version) => Ok(version as i64),
        Err(provenant_sqlite::VersionError::Unsupported(version)) => {
            Err(RunError::Version(version))
        }
        Err(provenant_sqlite::VersionError::Unversioned) => Err(RunError::Version(0)),
        Err(provenant_sqlite::VersionError::Sqlite(error)) => Err(error.into()),
    }
}

/// Take this attempt's lease when host and project counts and startup spacing
/// allow. Spacing is measured only against the last start of the same class.
/// A retried claim finds its own lease already held.
pub(crate) fn lease(
    db: &Connection,
    store: &Path,
    ticket: &AdmissionTicket,
    now_ms: i64,
    request: &Request<'_>,
) -> Result<bool> {
    let (project, class) = (ticket.project_id, ticket.lane_class);
    let spacing_ms = sql_int(ticket.startup_spacing_ms)?;
    immediate(db, request, |tx| {
        let id = ticket.attempt_id.to_string();
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE attempt=?1)",
            [&id],
            |r| r.get(0),
        )?;
        if held {
            return Ok(true);
        }
        if full(tx, project)? {
            prune(tx)?;
            if full(tx, project)? {
                return Ok(false);
            }
        }
        let last: Option<i64> = tx
            .query_row(
                "SELECT last_start FROM spacing WHERE class=?1",
                [class.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        // A wall clock that stepped back does not hold every start.
        if last.is_some_and(|last| last <= now_ms && now_ms - last < spacing_ms) {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO spacing(class,last_start) VALUES (?1,?2) ON CONFLICT(class) DO UPDATE SET last_start=excluded.last_start",
            params![class.as_str(), now_ms],
        )?;
        tx.execute(
            "INSERT INTO leases(attempt,project,store,class) VALUES (?1,?2,?3,?4)",
            params![
                id,
                project.to_string(),
                store.as_os_str().as_bytes(),
                class.as_str()
            ],
        )?;
        Ok(true)
    })
}
fn full(tx: &Connection, project: ProjectId) -> Result<bool> {
    let (host, project): (i64, i64) = tx.query_row(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE project=?1) FROM leases",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(host >= sql_int(HOST_ACTIVE)? || project >= sql_int(PROJECT_ACTIVE)?)
}
/// Drop leases whose owning store no longer holds the attempt's admission row.
fn prune(tx: &Connection) -> Result<()> {
    let leases = {
        let mut statement = tx.prepare("SELECT store, attempt FROM leases ORDER BY store")?;
        statement
            .query_map([], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut current: Option<(Vec<u8>, Owner)> = None;
    for (store, attempt) in leases {
        let owner = match current.take() {
            Some((path, owner)) if path == store => owner,
            _ => Owner::open(&Path::new(OsStr::from_bytes(&store)).join("run.sqlite3")),
        };
        if owner.released(&attempt) {
            tx.execute("DELETE FROM leases WHERE attempt=?1", [&attempt])?;
        }
        current = Some((store, owner));
    }
    Ok(())
}
/// The lease's owning store. Only a definite answer frees a lease: a store
/// file that is gone, or a readable store without the admission row.
enum Owner {
    Gone,
    Store(Connection),
    Unreadable,
}
impl Owner {
    fn open(path: &Path) -> Self {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::Gone,
            Err(_) => return Self::Unreadable,
            Ok(_) => {}
        }
        provenant_sqlite::open_connection(path, true).map_or(Self::Unreadable, Self::Store)
    }
    fn released(&self, attempt: &str) -> bool {
        match self {
            Self::Gone => true,
            Self::Unreadable => false,
            Self::Store(db) => db
                .query_row(
                    "SELECT NOT EXISTS(SELECT 1 FROM admission WHERE attempt=?1)",
                    [attempt],
                    |r| r.get(0),
                )
                .unwrap_or(false),
        }
    }
}
