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
use provenant_core::{AttemptId, Clock, ProjectId, WallTime};
use provenant_supervisor::census::{ProcessIdentity, ProcessObservation};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    ffi::OsStr,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};

const FILE: &str = "admission.sqlite3";
/// Each lease carries the lane class. Each class's last start is a reading of
/// the host's monotonic clock since boot, kept with the boot it was read in:
/// wall time is never recorded, so no clock step can move it.
pub(crate) const SCHEMA: provenant_sqlite::Schema = provenant_sqlite::Schema {
    sql: include_str!("admission_schema.sql"),
    version: 7,
};

/// Open, creating if needed, the ledger under the trusted host state root.
pub(crate) fn open(host: &Path, request: &Request<'_>) -> Result<Connection> {
    let (_, db, _initialisation) = open_database(host, FILE, request)?;
    provenant_sqlite::ensure_schema(&db, SCHEMA)?;
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
/// Native and owner-store observations prepared before either admission
/// database takes its write transaction. Existing leases retain their identity.
pub(crate) struct LeaseOwner {
    identity: Option<String>,
    store: Option<String>,
}

pub(crate) fn prepare_lease(
    db: &Connection,
    store: &Path,
    ticket: &AdmissionTicket,
    caps: LaneCaps,
    time: &dyn Clock,
    resources: &dyn Resources,
) -> Result<LeaseOwner> {
    let held: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM leases WHERE attempt=?1)",
        [ticket.attempt_id.to_string()],
        |row| row.get(0),
    )?;
    if held || cap_hit(db, ticket.project_id, ticket.lane_class, caps)?.is_some() {
        prune(db, time, resources)?;
    }
    Ok(LeaseOwner {
        identity: resources
            .guardian_identity()
            .map(|identity| serde_json::to_string(&identity))
            .transpose()
            .map_err(|_| RunError::Refused)?,
        store: store_identity(&store.join("run.sqlite3")),
    })
}

/// Record one custody-backed lease. Capacity and spacing are sampled by the
/// admission policy; neither creates a transaction retry or a hard resource hold.
/// Boot, guardian and store identities remain mandatory integrity evidence.
#[cfg(test)]
pub(crate) fn lease(
    db: &Connection,
    store: &Path,
    ticket: &AdmissionTicket,
    time: &dyn Clock,
    request: &Request<'_>,
    owner: &LeaseOwner,
) -> Result<Option<WallTime>> {
    lease_with_policy(
        db,
        store,
        ticket,
        time,
        request,
        owner,
        LaneCaps::of(&provenant_config::LaneBuildSettings::default()),
        &provenant_records::policy::Policy::default(),
    )
}

#[allow(clippy::too_many_arguments)] // Store ownership, clocks and resolved policy remain explicit.
pub(crate) fn lease_with_policy(
    db: &Connection,
    store: &Path,
    ticket: &AdmissionTicket,
    time: &dyn Clock,
    request: &Request<'_>,
    owner: &LeaseOwner,
    configured: LaneCaps,
    policy: &provenant_records::policy::Policy,
) -> Result<Option<WallTime>> {
    let (project, class) = (ticket.project_id, ticket.lane_class);
    let id = ticket.attempt_id.to_string();
    immediate(db, request, |tx| {
        let now = time
            .wall_now()
            .map_err(|_| RunError::Validation("wall clock"))?;
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE attempt=?1 AND yielded=0)",
            [&id],
            |r| r.get(0),
        )?;
        if held {
            return Ok(Some(now));
        }
        // Reusing this attempt's lease adds no capacity. For new leases, count
        // and publish in this same transaction so concurrent stores cannot
        // both take the last strict slot.
        use provenant_records::diagnostic::LaneLimit;
        use provenant_records::policy::{Effect, Facts, Strength};
        for strength in [Strength::Refuse, Strength::Warn] {
            let caps = configured.under_strength(policy, strength);
            let Some(hit) = cap_hit(tx, project, class, caps)? else {
                continue;
            };
            let code = match hit.kind {
                LaneLimit::HostWriters => "ADMISSION_WRITERS",
                LaneLimit::ProjectWriters => "ADMISSION_PROJECT_WRITERS",
                LaneLimit::HostTotal => "ADMISSION_LANES",
                LaneLimit::ProjectTotal => "ADMISSION_PROJECT_LANES",
            };
            match policy.check(
                code,
                Facts::violation(format!("active={}, limit={}", hit.active, hit.limit)),
            ) {
                Effect::Proceed => {}
                Effect::Warn(message) => {
                    use std::io::Write;
                    let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
                }
                Effect::Refuse(_) => {
                    return Err(RunError::Admission(AdmissionError::LaneCap {
                        active: hit.active,
                        limit: hit.limit,
                        kind: hit.kind,
                        origin: hit.origin,
                    }));
                }
            }
        }

        let host = HostInstant::read(time)?;
        tx.execute(
            "INSERT INTO spacing(class,boot,last_start) VALUES (?1,?2,?3) ON CONFLICT(class) DO UPDATE SET boot=excluded.boot, last_start=excluded.last_start",
            params![class.as_str(), &host.boot, sql_int(host.micros)?],
        )?;
        // A yielded lease retains its original guardian and store identity.
        // Reclaim runs in the relay, which need not be the guardian process.
        if tx.execute("UPDATE leases SET yielded=0 WHERE attempt=?1", [&id])? > 0 {
            return Ok(Some(now));
        }
        let identity = owner.identity.as_ref().ok_or_else(|| {
            policy.boundary_error(
                "ADMISSION_RUNSTORE_ADMIT_AUTHORITY",
                provenant_records::policy::Facts::violation(
                    "guardian process identity is unavailable for lease ownership",
                ),
                RunError::Admission(AdmissionError::GuardianIdentity),
            )
        })?;
        let owner_id = owner.store.as_ref().ok_or_else(|| {
            policy.boundary_error(
                "ADMISSION_RUNSTORE_ADMIT_AUTHORITY",
                provenant_records::policy::Facts::violation(
                    "run-store identity is unavailable for lease ownership",
                ),
                RunError::Admission(AdmissionError::GuardianIdentity),
            )
        })?;
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
/// Give up both host and project capacity, retaining custody for reclaim.
pub(crate) fn yield_lease(
    db: &Connection,
    store: &Path,
    id: AttemptId,
    request: &Request<'_>,
) -> Result<()> {
    immediate(db, request, |tx| {
        tx.execute(
            "UPDATE leases SET yielded=1 WHERE attempt=?1 AND store=?2",
            params![id.to_string(), store.as_os_str().as_bytes()],
        )?;
        Ok(())
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
    pub(crate) fn under_strength(
        mut self,
        policy: &provenant_records::policy::Policy,
        strength: provenant_records::policy::Strength,
    ) -> Self {
        use provenant_records::policy::{Effect, Facts, Strength};
        for (code, value) in [
            ("ADMISSION_WRITERS", &mut self.host_writers),
            ("ADMISSION_PROJECT_WRITERS", &mut self.project_writers),
            ("ADMISSION_LANES", &mut self.host_total),
            ("ADMISSION_PROJECT_LANES", &mut self.project_total),
        ] {
            let effect = policy.check(code, Facts::violation("configured lane cap"));
            let applies = match strength {
                Strength::Refuse => matches!(effect, Effect::Refuse(_)),
                Strength::Warn => matches!(effect, Effect::Warn(_)),
                Strength::Off => false,
            };
            if !applies {
                *value = u64::MAX;
            }
        }
        self
    }
    pub(crate) fn under_policy(mut self, policy: &provenant_records::policy::Policy) -> Self {
        use provenant_records::policy::Facts;
        for (code, value) in [
            ("ADMISSION_WRITERS", &mut self.host_writers),
            ("ADMISSION_PROJECT_WRITERS", &mut self.project_writers),
            ("ADMISSION_LANES", &mut self.host_total),
            ("ADMISSION_PROJECT_LANES", &mut self.project_total),
        ] {
            if !policy
                .check(code, Facts::violation("configured lane cap"))
                .blocks()
            {
                *value = u64::MAX;
            }
        }
        self
    }
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
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE project=?1), COUNT(*) FILTER (WHERE class='writer'), COUNT(*) FILTER (WHERE class='writer' AND project=?1) FROM leases WHERE yielded=0",
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
/// Inspect leases, active ones first. Yielded parents do not count against caps,
/// so they must not hide a dead active lease from this scan.
fn prune(tx: &Connection, time: &dyn Clock, resources: &dyn Resources) -> Result<()> {
    let boot = time
        .boot_identity()
        .map_err(|_| RunError::Admission(AdmissionError::Clock))?;
    let leases = {
        let mut statement = tx.prepare(
            "SELECT rowid, CAST(store AS BLOB), CAST(attempt AS BLOB), CAST(boot AS BLOB), CAST(guardian AS BLOB), CAST(owner_id AS BLOB) FROM leases ORDER BY yielded, rowid",
        )?;
        statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<Vec<u8>>>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                    r.get::<_, Option<Vec<u8>>>(3)?,
                    r.get::<_, Option<Vec<u8>>>(4)?,
                    r.get::<_, Option<Vec<u8>>>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (rowid, store, attempt, lease_boot, guardian, owner_id) in leases {
        let text = |raw: &Option<Vec<u8>>| {
            raw.as_deref()
                .and_then(|raw| std::str::from_utf8(raw).ok())
                .map(str::to_owned)
        };
        let attempt_text = text(&attempt).filter(|value| !value.is_empty());
        let owner = match (store.as_deref(), attempt_text.as_deref()) {
            (Some(store), Some(_)) => Owner::open(
                &Path::new(OsStr::from_bytes(store)).join("run.sqlite3"),
                text(&owner_id).as_deref(),
            ),
            _ => {
                malformed_row(rowid);
                Owner::Unreadable
            }
        };
        if released(
            &owner,
            attempt_text.as_deref().unwrap_or_default(),
            text(&lease_boot).as_deref(),
            text(&guardian).as_deref(),
            &boot,
            resources,
        ) {
            // Observation ran without a write transaction. Delete only the
            // exact lease observed, never a replacement reusing its rowid.
            tx.execute("DELETE FROM leases WHERE rowid=?1 AND CAST(store AS BLOB) IS ?2 AND CAST(attempt AS BLOB) IS ?3 AND CAST(boot AS BLOB) IS ?4 AND CAST(guardian AS BLOB) IS ?5 AND CAST(owner_id AS BLOB) IS ?6", params![rowid, store, attempt, lease_boot, guardian, owner_id])?;
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
        db.execute_batch(SCHEMA.sql).unwrap();
        let project = ProjectId::from_bytes([1; 16]);
        for index in 0..PROJECT_ACTIVE {
            db.execute(
                "INSERT INTO leases (attempt,project,store,class,boot,guardian,owner_id) VALUES (?1,?2,x'00','read_only','boot','guardian','owner')",
                params![index.to_string(), project.to_string()],
            ).unwrap();
        }
        assert!(
            cap_hit(&db, project, LaneClass::Writer, defaults)
                .unwrap()
                .is_none()
        );
        let configured = LaneCaps::of(&provenant_config::LaneBuildSettings {
            max_project_lanes: Some(PROJECT_ACTIVE),
            ..Default::default()
        });
        let hit = cap_hit(&db, project, LaneClass::Writer, configured)
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
        assert!(detail.contains("configuration"));
        assert!(projection.next.unwrap().contains("max_project_lanes"));
        let settings = provenant_config::LaneBuildSettings {
            max_lanes: Some(HOST_ACTIVE),
            max_project_lanes: Some(10),
            ..Default::default()
        };
        let configured = LaneCaps::of(&settings);
        // Isolate the configured project cap before filling the host cap,
        // which takes precedence when both limits are reached.
        let hit = cap_hit(&db, project, LaneClass::ReadOnly, configured)
            .unwrap()
            .unwrap();
        assert_eq!(hit.kind, LaneLimit::ProjectTotal);
        assert_eq!(hit.limit, 10);
        assert_eq!(hit.origin, SettingSource::Configuration);
        let other = ProjectId::from_bytes([2; 16]);
        for index in PROJECT_ACTIVE..HOST_ACTIVE {
            db.execute(
                "INSERT INTO leases (attempt,project,store,class,boot,guardian,owner_id) VALUES (?1,?2,x'00','read_only','boot','guardian','owner')",
                params![index.to_string(), other.to_string()],
            ).unwrap();
        }
        let third = ProjectId::from_bytes([3; 16]);
        // Without a configured limit, a full host still admits a writer.
        assert!(
            cap_hit(&db, third, LaneClass::Writer, defaults)
                .unwrap()
                .is_none()
        );
        let hit = cap_hit(&db, third, LaneClass::Writer, configured)
            .unwrap()
            .unwrap();
        assert_eq!(hit.kind, LaneLimit::HostTotal);
        assert_eq!(hit.limit, HOST_ACTIVE);
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
            db.execute_batch(SCHEMA.sql).unwrap();
            let clock = TestClock::new(WallTime::from_micros(0).unwrap(), MonoTime::from_micros(0));
            let boot = clock.boot_identity().unwrap();
            let live = match SystemProcessObserver.observe(i64::from(std::process::id())) {
                ProcessObservation::Present(fact) => fact.identity,
                _ => panic!("current guardian unavailable"),
            };
            let identity = serde_json::to_string(&live).unwrap();
            let owner_id = store_identity(&path).unwrap();
            db.execute(
                "INSERT INTO leases VALUES ('live','p',?1,'writer',?2,?3,?4,0)",
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
        // The production reader refuses symlinked path components, including
        // a host's short TMPDIR alias. Give it the canonical fixture root.
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let path = root.join("run.sqlite3");
        let owner = Connection::open(&path).unwrap();
        owner.execute_batch("CREATE TABLE admission (attempt TEXT); INSERT INTO admission VALUES ('old'), ('live');").unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA.sql).unwrap();
        let clock = TestClock::new(WallTime::from_micros(0).unwrap(), MonoTime::from_micros(0));
        let boot = clock.boot_identity().unwrap();
        let live = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) => fact.identity,
            _ => panic!("current guardian unavailable"),
        };
        let guardian = serde_json::to_string(&live).unwrap();
        let store = root.as_os_str().as_bytes();
        let owner_id = store_identity(&path).unwrap();
        db.execute(
            "INSERT INTO leases VALUES ('old','p',?1,'writer','old boot',?2,?3,0)",
            params![store, guardian, owner_id],
        )
        .unwrap();
        db.execute(
            "INSERT INTO leases VALUES ('live','p',?1,'writer',?2,?3,?4,0)",
            params![store, boot, guardian, owner_id],
        )
        .unwrap();
        db.execute(
            "INSERT INTO leases VALUES ('malformed','p',?1,'writer',?2,'bad',?3,0)",
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
