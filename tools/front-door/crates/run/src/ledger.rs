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
use provenant_core::{Clock, ProjectId, WallTime};
use provenant_supervisor::census::{ProcessIdentity, ProcessObservation, SystemProcessObserver};
use rusqlite::{Connection, OptionalExtension, params};
use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};

const FILE: &str = "admission.sqlite3";
/// Each lease carries the lane class. Version 1 had no class: every lease read
/// as a writer's.
const LEASES: &str = "
CREATE TABLE leases (
 attempt TEXT PRIMARY KEY, project TEXT NOT NULL, store BLOB NOT NULL,
 class TEXT NOT NULL DEFAULT 'writer' CHECK(class IN ('writer','read_only')),
 boot TEXT, guardian TEXT
) STRICT;
CREATE INDEX lease_projects ON leases(project);
";
/// Each class's last start is a reading of the host's monotonic clock since
/// boot, kept with the boot it was read in: wall time is never recorded, so no
/// clock step can move it. Version 1 kept one global wall-time row, which no
/// boot can vouch for, so an upgrade carries it across as a start under the
/// current boot as a start of now, holding each class one full spacing.
const SPACING: &str = "
CREATE TABLE spacing (
 class TEXT PRIMARY KEY CHECK(class IN ('writer','read_only')),
 boot TEXT NOT NULL,
 last_start INTEGER NOT NULL CHECK(last_start>=0)
) STRICT;
";

/// Open, creating or upgrading if needed, the ledger under the trusted host
/// state root. Each upgrade runs in one immediate transaction, schema version
/// last, so a crash leaves the old ledger whole or the new one.
pub(crate) fn open(host: &Path, request: &Request<'_>) -> Result<Connection> {
    let (_, db, _initialisation) = open_database(host, FILE, request)?;
    if request.retry(|| version(&db))? < 3 || !request.retry(|| timed_spacing(&db))? {
        immediate(&db, request, |tx| {
            let current_version = version(tx)?;
            match current_version {
                0 => {
                    tx.execute_batch(LEASES)?;
                    tx.execute_batch(SPACING)?;
                }
                1 => {
                    let carried = carried_starts(tx, request)?;
                    tx.execute_batch(
                        "ALTER TABLE leases ADD COLUMN class TEXT NOT NULL DEFAULT 'writer' CHECK(class IN ('writer','read_only'));
                         DROP TABLE spacing;",
                    )?;
                    tx.execute_batch(SPACING)?;
                    for (class, boot, start) in carried {
                        tx.execute(
                            "INSERT INTO spacing(class,boot,last_start) VALUES (?1,?2,?3)",
                            params![class.as_str(), boot, sql_int(start)?],
                        )?;
                    }
                }
                // A version 2 ledger written before spacing carried its boot
                // held wall milliseconds.
                _ if !timed_spacing(tx)? => {
                    tx.execute_batch("DROP TABLE spacing;")?;
                    tx.execute_batch(SPACING)?;
                }
                _ => {}
            }
            if current_version > 0 && current_version < 3 {
                tx.execute_batch("ALTER TABLE leases ADD COLUMN boot TEXT; ALTER TABLE leases ADD COLUMN guardian TEXT;")?;
            }
            tx.execute_batch("PRAGMA user_version=3;")?;
            Ok(())
        })?;
    }
    Ok(db)
}
/// Each class's version 1 start as a start under the current boot. Version 1
/// kept one wall-time start for every class, which no boot can vouch for and
/// which a wall clock step can move. The wall clock is not read: any version 1
/// row, of any recorded age, is carried as a start of now, so each class is
/// held one full spacing from the upgrade. A ledger with no row holds nothing.
/// The boot is read once, inside the upgrade transaction, and an unreadable
/// host clock refuses the upgrade rather than drop the start.
fn carried_starts(tx: &Connection, request: &Request<'_>) -> Result<Vec<(LaneClass, String, u64)>> {
    let recorded: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM spacing)", [], |r| r.get(0))?;
    let now = HostInstant::read(request.clock)?;
    if !recorded {
        return Ok(Vec::new());
    }
    Ok([LaneClass::Writer, LaneClass::ReadOnly]
        .into_iter()
        .map(|class| (class, now.boot.clone(), now.micros))
        .collect())
}
/// Whether the spacing table records starts against a boot.
fn timed_spacing(db: &Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('spacing') WHERE name='boot')",
        [],
        |r| r.get(0),
    )?)
}
fn version(db: &Connection) -> Result<i64> {
    match provenant_sqlite::read_version(db, 3) {
        Ok(version) => Ok(version as i64),
        Err(provenant_sqlite::VersionError::Unsupported(version)) => {
            Err(RunError::Version(version))
        }
        Err(provenant_sqlite::VersionError::Unversioned) => Err(RunError::Version(0)),
        Err(provenant_sqlite::VersionError::Sqlite(error)) => Err(error.into()),
    }
}

/// Take this attempt's lease when host and project counts and startup spacing
/// allow, returning the wall time it started at, for display only. Spacing is
/// judged on the host's monotonic clock, never the wall clock, and only
/// against the last start of the same class: within one boot no two lanes of a
/// class start less than one spacing apart. The clock is read inside the
/// transaction that checks and records spacing, so lock order is clock order
/// across every process on the host. A clock or boot identity that cannot be
/// read refuses the claim. A retried claim finds its own lease already held.
pub(crate) fn lease(
    db: &Connection,
    store: &Path,
    ticket: &AdmissionTicket,
    max_builds: u64,
    time: &dyn Clock,
    request: &Request<'_>,
) -> Result<Option<WallTime>> {
    let (project, class) = (ticket.project_id, ticket.lane_class);
    let spacing = spacing_micros(ticket.startup_spacing_ms)?;
    immediate(db, request, |tx| {
        let now = time
            .wall_now()
            .map_err(|_| RunError::Validation("wall clock"))?;
        let id = ticket.attempt_id.to_string();
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE attempt=?1)",
            [&id],
            |r| r.get(0),
        )?;
        if held {
            return Ok(Some(now));
        }
        if full(tx, project)? || (class == LaneClass::Writer && writers_full(tx, max_builds)?) {
            prune(tx, time)?;
            if full(tx, project)? || (class == LaneClass::Writer && writers_full(tx, max_builds)?) {
                return Ok(None);
            }
        }
        let host = HostInstant::read(time)?;
        let last = recorded_start(tx, class)?;
        if spacing_wait(last.as_ref(), &host, spacing) > 0 {
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO spacing(class,boot,last_start) VALUES (?1,?2,?3) ON CONFLICT(class) DO UPDATE SET boot=excluded.boot, last_start=excluded.last_start",
            params![class.as_str(), &host.boot, sql_int(host.micros)?],
        )?;
        let identity = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) if !fact.zombie => fact.identity,
            _ => return Err(RunError::Refused),
        };
        let identity = serde_json::to_string(&identity).map_err(|_| RunError::Refused)?;
        tx.execute(
            "INSERT INTO leases(attempt,project,store,class,boot,guardian) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                id,
                project.to_string(),
                store.as_os_str().as_bytes(),
                class.as_str(),
                host.boot,
                identity
            ],
        )?;
        Ok(Some(now))
    })
}
fn writers_full(tx: &Connection, max: u64) -> Result<bool> {
    let writers: i64 = tx.query_row(
        "SELECT COUNT(*) FROM leases WHERE class='writer'",
        [],
        |r| r.get(0),
    )?;
    Ok(writers >= sql_int(max)?)
}
pub(crate) fn writer_count(host: &Path, request: &Request<'_>) -> Option<u64> {
    let db = provenant_sqlite::open_connection(&host.join(FILE), true).ok()?;
    let boot = request.clock.boot_identity().ok()?;
    let mut statement = db
        .prepare("SELECT store, attempt, boot, guardian FROM leases WHERE class='writer'")
        .ok()?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .ok()?;
    let mut count = 0;
    for row in rows {
        let (store, attempt, lease_boot, guardian) = row.ok()?;
        let owner = Owner::open(&Path::new(OsStr::from_bytes(&store)).join("run.sqlite3"));
        if !released(
            &owner,
            &attempt,
            lease_boot.as_deref(),
            guardian.as_deref(),
            &boot,
        ) {
            count += 1;
        }
    }
    Some(count)
}
/// A reading of the host's monotonic clock and the boot it belongs to.
struct HostInstant {
    boot: String,
    micros: u64,
}
impl HostInstant {
    fn read(time: &dyn Clock) -> Result<Self> {
        Ok(Self {
            boot: time.boot_identity().map_err(|_| RunError::HostClock)?,
            micros: time
                .host_monotonic_micros()
                .map_err(|_| RunError::HostClock)?,
        })
    }
}
fn recorded_start(db: &Connection, class: LaneClass) -> Result<Option<HostInstant>> {
    let row: Option<(String, i64)> = db
        .query_row(
            "SELECT boot, last_start FROM spacing WHERE class=?1",
            [class.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.map(|(boot, micros)| HostInstant {
        boot,
        micros: u64::try_from(micros).unwrap_or(u64::MAX),
    }))
}
fn spacing_micros(spacing_ms: u64) -> Result<u64> {
    spacing_ms
        .checked_mul(1000)
        .ok_or(RunError::Validation("startup spacing"))
}
/// Milliseconds before spacing lets a lane of `class` start, read without
/// creating or upgrading the ledger. `None` when the ledger is absent,
/// unreadable or older than boot-keyed spacing, or when the host clock cannot
/// be read, so a caller states no wait it cannot see.
pub(crate) fn spacing_left(
    host: &Path,
    class: LaneClass,
    spacing_ms: u64,
    time: &dyn Clock,
) -> Option<u64> {
    let db = provenant_sqlite::open_connection(&host.join(FILE), true).ok()?;
    let last = recorded_start(&db, class).ok()?;
    let now = HostInstant::read(time).ok()?;
    let wait = spacing_wait(last.as_ref(), &now, spacing_micros(spacing_ms).ok()?);
    Some(wait.div_ceil(1000))
}
/// Microseconds before a start may follow the recorded `last`. A start from
/// another boot, or none, leaves the class free: that boot's clock says
/// nothing about this one's. Within a boot the clock never steps back, so
/// `now - last` is the real time since the start; a start somehow ahead of the
/// clock holds longer rather than freeing the class.
fn spacing_wait(last: Option<&HostInstant>, now: &HostInstant, spacing: u64) -> u64 {
    match last {
        Some(last) if last.boot == now.boot => last
            .micros
            .saturating_add(spacing)
            .saturating_sub(now.micros),
        _ => 0,
    }
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
fn prune(tx: &Connection, time: &dyn Clock) -> Result<()> {
    let boot = time.boot_identity().map_err(|_| RunError::HostClock)?;
    let leases = {
        let mut statement =
            tx.prepare("SELECT store, attempt, boot, guardian FROM leases ORDER BY store")?;
        statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut current: Option<(Vec<u8>, Owner)> = None;
    for (store, attempt, lease_boot, guardian) in leases {
        let owner = match current.take() {
            Some((path, owner)) if path == store => owner,
            _ => Owner::open(&Path::new(OsStr::from_bytes(&store)).join("run.sqlite3")),
        };
        if released(
            &owner,
            &attempt,
            lease_boot.as_deref(),
            guardian.as_deref(),
            &boot,
        ) {
            tx.execute("DELETE FROM leases WHERE attempt=?1", [&attempt])?;
        }
        current = Some((store, owner));
    }
    Ok(())
}
fn released(
    owner: &Owner,
    attempt: &str,
    lease_boot: Option<&str>,
    guardian: Option<&str>,
    boot: &str,
) -> bool {
    let other_boot = lease_boot.is_some_and(|old| old != boot);
    let guardian_gone = guardian
        .and_then(|raw| serde_json::from_str::<ProcessIdentity>(raw).ok())
        .is_some_and(
            |identity| match SystemProcessObserver.observe(identity.pid) {
                ProcessObservation::Absent => true,
                ProcessObservation::Present(fact) => fact.identity != identity || fact.zombie,
                ProcessObservation::Unknown(_) => false,
            },
        );
    other_boot || guardian_gone || owner.released(attempt)
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
    #[allow(clippy::print_stderr)]
    fn released(&self, attempt: &str) -> bool {
        match self {
            Self::Gone => true,
            Self::Unreadable => {
                eprintln!("admission: releasing lease {attempt}: owner store is unreadable");
                true
            }
            Self::Store(db) => db
                .query_row(
                    "SELECT NOT EXISTS(SELECT 1 FROM admission WHERE attempt=?1)",
                    [attempt],
                    |r| r.get(0),
                )
                .unwrap_or_else(|error| {
                    eprintln!(
                        "admission: releasing lease {attempt}: owner store query failed: {error}"
                    );
                    true
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{MonoTime, TestClock};

    #[test]
    fn prune_releases_old_boot_and_unreadable_owners() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good");
        let bad = dir.path().join("bad");
        std::fs::create_dir(&good).unwrap();
        std::fs::create_dir(&bad).unwrap();
        let owner = Connection::open(good.join("run.sqlite3")).unwrap();
        owner.execute_batch("CREATE TABLE admission (attempt TEXT); INSERT INTO admission VALUES ('old'), ('dead'), ('live');").unwrap();
        std::fs::write(bad.join("run.sqlite3"), b"not sqlite").unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(LEASES).unwrap();
        let store = good.as_os_str().as_bytes();
        db.execute(
            "INSERT INTO leases VALUES ('old','p',?1,'writer','another boot',NULL)",
            [store],
        )
        .unwrap();
        db.execute(
            "INSERT INTO leases VALUES ('bad','p',?1,'writer',NULL,NULL)",
            [bad.as_os_str().as_bytes()],
        )
        .unwrap();
        let clock = TestClock::new(WallTime::from_micros(0).unwrap(), MonoTime::from_micros(0));
        let identity = ProcessIdentity {
            boot: clock.boot_identity().unwrap(),
            pid: i64::from(std::process::id()),
            native_start: 0,
            group_id: 0,
            session_id: 0,
        };
        db.execute(
            "INSERT INTO leases VALUES ('dead','p',?1,'writer',?2,?3)",
            params![
                store,
                identity.boot,
                serde_json::to_string(&identity).unwrap()
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO leases VALUES ('live','p',?1,'writer',NULL,NULL)",
            [store],
        )
        .unwrap();
        assert!(writers_full(&db, 4).unwrap());
        prune(&db, &clock).unwrap();
        assert!(writers_full(&db, 1).unwrap());
        assert!(!writers_full(&db, 2).unwrap());
        assert!(Owner::Gone.released("gone"));
        assert!(Owner::Unreadable.released("bad"));
    }
}
