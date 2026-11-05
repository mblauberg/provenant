//! Host admission ledger. Host and project concurrency and startup spacing are
//! global to the host, so every execution namespace, top-level and nested child
//! stores alike, leases capacity in one SQLite file under the host state root.
//! Per-run concurrency stays in each store's own `admission` table.
//!
//! A lease names its owning store, boot and guardian. Terminal publication
//! deletes the store admission row; pruning also releases leases after a boot
//! change, confirmed guardian death, or a verified owner row removal.
use crate::{
    db::{immediate, open_database, sql_int},
    *,
};
use provenant_core::{Clock, ProjectId, WallTime};
use provenant_supervisor::census::{ProcessIdentity, ProcessObservation};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    ffi::OsStr,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};

const FILE: &str = "admission.sqlite3";
/// Each lease carries the lane class. Version 1 had no class: every lease read
/// as a writer's.
const LEASES: &str = "
CREATE TABLE leases (
 attempt TEXT PRIMARY KEY, project TEXT NOT NULL, store BLOB NOT NULL,
 class TEXT NOT NULL DEFAULT 'writer' CHECK(class IN ('writer','read_only')),
 boot TEXT NOT NULL, guardian TEXT NOT NULL, owner_id TEXT NOT NULL
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
    if version(&db)? < 5 || !timed_spacing(&db)? {
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
            if current_version > 0 && current_version < 5 {
                // Old lease schemas were never installed; retain only spacing history.
                tx.execute_batch("DROP TABLE leases;")?;
                tx.execute_batch(LEASES)?;
            }
            tx.execute_batch("PRAGMA user_version=5;")?;
            Ok(())
        })?;
    }
    Ok(db)
}
/// Run `action` while holding the ledger's write lock. Every claim takes that
/// lock, so no lane is admitted until `action` returns; leases already held,
/// and the lanes running under them, are untouched. The caller waits within
/// `request`'s budget and gets `Contention` when another holder keeps the lock.
pub(crate) fn while_admission_held<T>(
    host: &Path,
    request: &Request<'_>,
    action: impl FnOnce() -> T,
) -> Result<T> {
    let db = open(host, request)?;
    let tx =
        { rusqlite::Transaction::new_unchecked(&db, rusqlite::TransactionBehavior::Immediate)? };
    let value = action();
    tx.rollback()?;
    Ok(value)
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
    Ok(provenant_sqlite::read_version(db, 5)? as i64)
}

/// Take this attempt's lease when host and project counts and startup spacing
/// allow, returning the wall time it started at, for display only. Idle
/// writers are admitted whatever `max_concurrent_builds` says: that limit gates
/// builds (`provenant_os::build_slots`), not live lanes. Spacing is
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
    caps: LaneCaps,
    time: &dyn Clock,
    request: &Request<'_>,
    resources: &dyn Resources,
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
            // A retry must not preserve a malformed lease merely because its
            // attempt key matches. Recheck it through the same pruning rules.
            prune(tx, time, resources)?;
            let still_held: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM leases WHERE attempt=?1)",
                [&id],
                |r| r.get(0),
            )?;
            if still_held {
                return Ok(Some(now));
            }
        }
        if cap_hit(tx, project, class, caps)?.is_some() {
            prune(tx, time, resources)?;
            if cap_hit(tx, project, class, caps)?.is_some() {
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
        let identity = resources
            .guardian_identity()
            .ok_or(RunError::Admission(AdmissionError::GuardianIdentity))?;
        let identity = serde_json::to_string(&identity).map_err(|_| RunError::Refused)?;
        let owner_id = store_identity(&store.join("run.sqlite3"))
            .ok_or(RunError::Admission(AdmissionError::GuardianIdentity))?;
        tx.execute(
            "INSERT INTO leases(attempt,project,store,class,boot,guardian,owner_id) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                id,
                project.to_string(),
                store.as_os_str().as_bytes(),
                class.as_str(),
                host.boot,
                identity,
                owner_id
            ],
        )?;
        Ok(Some(now))
    })
}
/// A reading of the host's monotonic clock and the boot it belongs to.
struct HostInstant {
    boot: String,
    micros: u64,
}
impl HostInstant {
    fn read(time: &dyn Clock) -> Result<Self> {
        Ok(Self {
            boot: time
                .boot_identity()
                .map_err(|_| RunError::Admission(AdmissionError::Clock))?,
            micros: time
                .host_monotonic_micros()
                .map_err(|_| RunError::Admission(AdmissionError::Clock))?,
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
/// Host and project caps for total lanes and writer leases.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LaneCaps {
    pub host_writers: u64,
    pub project_writers: u64,
    host_total: u64,
    project_total: u64,
    host_total_source: provenant_records::diagnostic::SettingSource,
    project_total_source: provenant_records::diagnostic::SettingSource,
    host_source: provenant_records::diagnostic::SettingSource,
    project_source: provenant_records::diagnostic::SettingSource,
}
impl LaneCaps {
    pub(crate) fn of(settings: &provenant_config::LaneBuildSettings) -> Self {
        Self {
            host_writers: settings.writer_lanes(),
            project_writers: settings.project_writer_lanes(),
            host_total: settings.total_lanes(),
            project_total: settings.project_total_lanes(),
            host_total_source: if settings.max_lanes.is_some() {
                provenant_records::diagnostic::SettingSource::Configuration
            } else {
                provenant_records::diagnostic::SettingSource::ShippedDefault
            },
            project_total_source: if settings.max_project_lanes.is_some() {
                provenant_records::diagnostic::SettingSource::Configuration
            } else {
                provenant_records::diagnostic::SettingSource::ShippedDefault
            },
            host_source: if settings.max_writer_lanes.is_some() {
                provenant_records::diagnostic::SettingSource::Configuration
            } else {
                provenant_records::diagnostic::SettingSource::ShippedDefault
            },
            project_source: if settings.max_project_writer_lanes.is_some() {
                provenant_records::diagnostic::SettingSource::Configuration
            } else {
                provenant_records::diagnostic::SettingSource::ShippedDefault
            },
        }
    }
}
/// The limit a new lease would exceed, with its scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CapHit {
    pub active: u64,
    pub limit: u64,
    pub kind: provenant_records::diagnostic::LaneLimit,
    pub origin: provenant_records::diagnostic::SettingSource,
}
/// Whether a lease of `class` for `project` would exceed a limit. Every lane
/// counts against the configured host and project caps; writers also count
/// against the configured writer caps.
pub(crate) fn cap_hit(
    tx: &Connection,
    project: ProjectId,
    class: LaneClass,
    caps: LaneCaps,
) -> Result<Option<CapHit>> {
    let (host, in_project, host_writers, project_writers): (i64, i64, i64, i64) = tx.query_row(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE project=?1), COUNT(*) FILTER (WHERE class='writer'), COUNT(*) FILTER (WHERE class='writer' AND project=?1) FROM leases",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    use provenant_records::diagnostic::LaneLimit;
    let hit = |active: i64, limit: u64, kind, origin| {
        let active = u64::try_from(active).unwrap_or(u64::MAX);
        (active >= limit).then_some(CapHit {
            active,
            limit,
            kind,
            origin,
        })
    };
    if class == LaneClass::Writer
        && let Some(found) = hit(
            host_writers,
            caps.host_writers,
            LaneLimit::HostWriters,
            caps.host_source,
        )
        .or_else(|| {
            hit(
                project_writers,
                caps.project_writers,
                LaneLimit::ProjectWriters,
                caps.project_source,
            )
        })
    {
        return Ok(Some(found));
    }
    Ok(hit(
        host,
        caps.host_total,
        LaneLimit::HostTotal,
        caps.host_total_source,
    )
    .or_else(|| {
        hit(
            in_project,
            caps.project_total,
            LaneLimit::ProjectTotal,
            caps.project_total_source,
        )
    }))
}
/// Inspect at most 256 of the oldest existing leases. Normal admission caps
/// the ledger at no more than 64, so every lease is covered without an unbounded scan.
fn prune(tx: &Connection, time: &dyn Clock, resources: &dyn Resources) -> Result<()> {
    let boot = time
        .boot_identity()
        .map_err(|_| RunError::Admission(AdmissionError::Clock))?;
    let leases = {
        let mut statement = tx.prepare(
            "SELECT rowid, CAST(store AS BLOB), CAST(attempt AS BLOB), CAST(boot AS BLOB), CAST(guardian AS BLOB), CAST(owner_id AS BLOB) FROM leases ORDER BY rowid LIMIT 256",
        )?;
        statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<Vec<u8>>>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?
                        .and_then(|bytes| String::from_utf8(bytes).ok()),
                    r.get::<_, Option<Vec<u8>>>(3)?
                        .and_then(|bytes| String::from_utf8(bytes).ok()),
                    r.get::<_, Option<Vec<u8>>>(4)?
                        .and_then(|bytes| String::from_utf8(bytes).ok()),
                    r.get::<_, Option<Vec<u8>>>(5)?
                        .and_then(|bytes| String::from_utf8(bytes).ok()),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (rowid, store, attempt, lease_boot, guardian, owner_id) in leases {
        let (Some(store), Some(attempt)) = (store, attempt.filter(|value| !value.is_empty()))
        else {
            malformed_row(rowid);
            if released(
                &Owner::Unreadable,
                "",
                lease_boot.as_deref(),
                guardian.as_deref(),
                &boot,
                resources,
            ) {
                tx.execute("DELETE FROM leases WHERE rowid=?1", [rowid])?;
            }
            continue;
        };
        let owner = Owner::open(
            &Path::new(OsStr::from_bytes(&store)).join("run.sqlite3"),
            owner_id.as_deref(),
        );
        if released(
            &owner,
            &attempt,
            lease_boot.as_deref(),
            guardian.as_deref(),
            &boot,
            resources,
        ) {
            tx.execute("DELETE FROM leases WHERE rowid=?1", [rowid])?;
        }
    }
    Ok(())
}
#[allow(clippy::print_stderr)]
fn malformed_row(rowid: i64) {
    eprintln!("admission: malformed lease row {rowid}");
}
#[allow(clippy::print_stderr)]
fn released(
    owner: &Owner,
    attempt: &str,
    lease_boot: Option<&str>,
    guardian: Option<&str>,
    boot: &str,
    resources: &dyn Resources,
) -> bool {
    if lease_boot.is_some_and(|lease_boot| !lease_boot.is_empty() && lease_boot != boot) {
        return true;
    }
    let identity = guardian
        .and_then(|raw| serde_json::from_str::<ProcessIdentity>(raw).ok())
        .filter(|value| !value.boot.is_empty() && value.pid > 0 && value.native_start > 0);
    let guardian_gone =
        identity.is_some_and(|identity| match resources.observe_process(identity.pid) {
            ProcessObservation::Absent => true,
            ProcessObservation::Present(fact) => fact.identity != identity || fact.zombie,
            ProcessObservation::Unknown(_) => false,
        });
    guardian_gone || owner.released(attempt)
}
/// Store file identity fences replacements before trusting row absence.
fn store_identity(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}
enum Owner {
    Store {
        db: Box<Connection>,
        path: std::path::PathBuf,
        identity: String,
    },
    Unreadable,
}
impl Owner {
    fn open(path: &Path, expected: Option<&str>) -> Self {
        let Some(identity) = expected.filter(|id| !id.is_empty()) else {
            return Self::Unreadable;
        };
        if store_identity(path).as_deref() != Some(identity) {
            return Self::Unreadable;
        }
        match provenant_sqlite::open_connection(path, true) {
            Ok(db) if store_identity(path).as_deref() == Some(identity) => Self::Store {
                db: Box::new(db),
                path: path.to_owned(),
                identity: identity.to_owned(),
            },
            _ => Self::Unreadable,
        }
    }
    fn released(&self, attempt: &str) -> bool {
        match self {
            Self::Unreadable => false,
            Self::Store { db, path, identity } => {
                let gone = db
                    .query_row(
                        "SELECT NOT EXISTS(SELECT 1 FROM admission WHERE attempt=?1)",
                        [attempt],
                        |r| r.get::<_, bool>(0),
                    )
                    .unwrap_or(false);
                gone && store_identity(path).as_deref() == Some(identity.as_str())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{MonoTime, TestClock};
    use provenant_supervisor::census::SystemProcessObserver;

    #[test]
    fn lane_cap_identifies_writer_and_total_settings_with_sources() {
        use provenant_records::diagnostic::{LaneLimit, SettingSource};
        let defaults = LaneCaps::of(&provenant_config::LaneBuildSettings::default());
        assert_eq!(defaults.host_source, SettingSource::ShippedDefault);
        assert_eq!(defaults.project_source, SettingSource::ShippedDefault);
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(LEASES).unwrap();
        let project = ProjectId::from_bytes([1; 16]);
        for index in 0..PROJECT_ACTIVE {
            db.execute(
                "INSERT INTO leases (attempt,project,store,class,boot,guardian,owner_id) VALUES (?1,?2,x'00','read_only','boot','guardian','owner')",
                params![index.to_string(), project.to_string()],
            ).unwrap();
        }
        let hit = cap_hit(&db, project, LaneClass::Writer, defaults)
            .unwrap()
            .unwrap();
        assert_eq!(hit.kind, LaneLimit::ProjectTotal);
        assert_eq!(hit.limit, PROJECT_ACTIVE);
        assert_eq!(hit.kind.setting(), "domains.fabric.lanes.max_project_lanes");
        let fact = provenant_records::diagnostic::ResourceFact::LaneCap {
            active: hit.active,
            limit: hit.limit,
            lane_limit: hit.kind,
            origin: hit.origin,
        };
        let projection = fact.project(provenant_records::diagnostic::Diagnostic::new("LANE_CAP"));
        let detail = projection.detail.unwrap();
        assert!(detail.contains("project lanes"));
        assert!(detail.contains("max_project_lanes"));
        assert!(detail.contains("shipped default"));
        assert!(projection.next.unwrap().contains("max_project_lanes"));
        let other = ProjectId::from_bytes([2; 16]);
        for index in PROJECT_ACTIVE..HOST_ACTIVE {
            db.execute(
                "INSERT INTO leases (attempt,project,store,class,boot,guardian,owner_id) VALUES (?1,?2,x'00','read_only','boot','guardian','owner')",
                params![index.to_string(), other.to_string()],
            ).unwrap();
        }
        let third = ProjectId::from_bytes([3; 16]);
        let hit = cap_hit(&db, third, LaneClass::Writer, defaults)
            .unwrap()
            .unwrap();
        assert_eq!(hit.kind, LaneLimit::HostTotal);
        assert_eq!(hit.limit, HOST_ACTIVE);
        let settings = provenant_config::LaneBuildSettings {
            max_lanes: Some(HOST_ACTIVE),
            max_project_lanes: Some(10),
            ..Default::default()
        };
        let configured = LaneCaps::of(&settings);
        let hit = cap_hit(&db, project, LaneClass::ReadOnly, configured)
            .unwrap()
            .unwrap();
        assert_eq!(hit.kind, LaneLimit::ProjectTotal);
        assert_eq!(hit.limit, 10);
        assert_eq!(hit.origin, SettingSource::Configuration);
        let hit = cap_hit(&db, third, LaneClass::ReadOnly, configured)
            .unwrap()
            .unwrap();
        assert_eq!(hit.kind, LaneLimit::HostTotal);
        assert_eq!(hit.limit, HOST_ACTIVE);
        assert_eq!(hit.origin, SettingSource::Configuration);
    }

    #[test]
    fn injected_process_observation_fences_live_and_unknown_guardians() {
        use provenant_supervisor::census::{ObservationIssue, ProcessFact};
        struct Facts(ProcessObservation);
        impl Resources for Facts {
            fn memory(&self) -> std::io::Result<provenant_os::sys::resources::MemorySample> {
                unreachable!("lease pruning does not sample memory")
            }
            fn disk(&self, _: &provenant_os::fs::Directory) -> std::io::Result<u64> {
                unreachable!("lease pruning does not sample disk")
            }
            fn observe_process(&self, _: i64) -> ProcessObservation {
                self.0.clone()
            }
        }
        let identity = ProcessIdentity {
            boot: "fixture-boot".into(),
            pid: 42,
            native_start: 7,
            group_id: 42,
            session_id: 42,
        };
        let fact = ProcessFact {
            identity: identity.clone(),
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        };
        let mut reused = fact.clone();
        reused.identity.native_start += 1;
        let mut zombie = fact.clone();
        zombie.zombie = true;
        let guardian = serde_json::to_string(&identity).unwrap();
        for (observation, expected) in [
            (ProcessObservation::Present(fact), false),
            (ProcessObservation::Unknown(ObservationIssue::Io), false),
            (ProcessObservation::Absent, true),
            (ProcessObservation::Present(reused), true),
            (ProcessObservation::Present(zombie), true),
        ] {
            assert_eq!(
                released(
                    &Owner::Unreadable,
                    "attempt",
                    Some("fixture-boot"),
                    Some(&guardian),
                    "fixture-boot",
                    &Facts(observation)
                ),
                expected
            );
        }
    }

    /// Whether `max` or more leases remain, as the removed writer cap counted.
    fn writers_full(tx: &Connection, max: i64) -> Result<bool> {
        let held: i64 = tx.query_row(
            "SELECT COUNT(*) FROM leases WHERE class='writer'",
            [],
            |r| r.get(0),
        )?;
        Ok(held >= max)
    }

    #[test]
    fn live_guardian_fences_busy_replaced_and_corrupt_store() {
        provenant_sqlite::with_busy_timeout(std::time::Duration::from_millis(50), || {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("run.sqlite3");
            let original = Connection::open(&path).unwrap();
            original.execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE admission (attempt TEXT); INSERT INTO admission VALUES ('live');").unwrap();
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(LEASES).unwrap();
            let clock = TestClock::new(WallTime::from_micros(0).unwrap(), MonoTime::from_micros(0));
            let boot = clock.boot_identity().unwrap();
            let live = match SystemProcessObserver.observe(i64::from(std::process::id())) {
                ProcessObservation::Present(fact) => fact.identity,
                _ => panic!("current guardian unavailable"),
            };
            let identity = serde_json::to_string(&live).unwrap();
            let owner_id = store_identity(&path).unwrap();
            db.execute(
                "INSERT INTO leases VALUES ('live','p',?1,'writer',?2,?3,?4)",
                params![dir.path().as_os_str().as_bytes(), boot, identity, owner_id],
            )
            .unwrap();
            original.execute_batch("BEGIN EXCLUSIVE").unwrap();
            prune(&db, &clock, &SystemResources).unwrap();
            assert!(
                writers_full(&db, 1).unwrap(),
                "busy store retains a live lease"
            );
            original.execute_batch("ROLLBACK").unwrap();
            drop(original);
            std::fs::write(&path, b"corrupt").unwrap();
            prune(&db, &clock, &SystemResources).unwrap();
            assert!(
                writers_full(&db, 1).unwrap(),
                "corrupt original store retains a live lease"
            );
            std::fs::rename(&path, dir.path().join("original.sqlite3")).unwrap();
            prune(&db, &clock, &SystemResources).unwrap();
            assert!(
                writers_full(&db, 1).unwrap(),
                "missing store retains a live lease"
            );
            let replacement = Connection::open(&path).unwrap();
            replacement
                .execute_batch("CREATE TABLE admission (attempt TEXT)")
                .unwrap();
            drop(replacement);
            prune(&db, &clock, &SystemResources).unwrap();
            assert!(
                writers_full(&db, 1).unwrap(),
                "replacement cannot prove row removal"
            );
            let mut dead = live;
            dead.native_start = dead.native_start.saturating_add(1);
            db.execute(
                "UPDATE leases SET guardian=?1",
                [serde_json::to_string(&dead).unwrap()],
            )
            .unwrap();
            prune(&db, &clock, &SystemResources).unwrap();
            assert!(
                !writers_full(&db, 1).unwrap(),
                "absent guardian releases the lease"
            );
        })
    }

    #[test]
    fn cross_boot_malformed_and_readable_row_removal_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.sqlite3");
        let owner = Connection::open(&path).unwrap();
        owner.execute_batch("CREATE TABLE admission (attempt TEXT); INSERT INTO admission VALUES ('old'), ('live');").unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(LEASES).unwrap();
        let clock = TestClock::new(WallTime::from_micros(0).unwrap(), MonoTime::from_micros(0));
        let boot = clock.boot_identity().unwrap();
        let live = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) => fact.identity,
            _ => panic!("current guardian unavailable"),
        };
        let guardian = serde_json::to_string(&live).unwrap();
        let store = dir.path().as_os_str().as_bytes();
        let owner_id = store_identity(&path).unwrap();
        db.execute(
            "INSERT INTO leases VALUES ('old','p',?1,'writer','old boot',?2,?3)",
            params![store, guardian, owner_id],
        )
        .unwrap();
        db.execute(
            "INSERT INTO leases VALUES ('live','p',?1,'writer',?2,?3,?4)",
            params![store, boot, guardian, owner_id],
        )
        .unwrap();
        db.execute(
            "INSERT INTO leases VALUES ('malformed','p',?1,'writer',?2,'bad',?3)",
            params![store, boot, owner_id],
        )
        .unwrap();
        prune(&db, &clock, &SystemResources).unwrap();
        assert!(writers_full(&db, 1).unwrap());
        assert!(!writers_full(&db, 2).unwrap());
        owner
            .execute("DELETE FROM admission WHERE attempt='live'", [])
            .unwrap();
        prune(&db, &clock, &SystemResources).unwrap();
        assert!(!writers_full(&db, 1).unwrap());
    }
}
