//! Host admission. Each attempt waits inside its own guardian, before spawn;
//! there is no scheduler daemon. Resource samples are taken outside the store
//! transaction, which then rechecks run concurrency and leases host capacity
//! and spacing from the host ledger while claiming.
use crate::{
    db::{bytes, decode, sql_int},
    store::insert_event,
    *,
};
use provenant_confinement::valid_read_only_writable;
use provenant_core::{AttemptId, CancelToken, Clock, DurationMicros, GitOid, WaitBudget, WallTime};
use provenant_os::{
    fs::Directory,
    sys::resources::{self, MemorySample},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::time::Duration;

/// Write evidence from the prepared confinement plan, never launch authority.
#[derive(Default)]
pub(crate) struct AppliedWriteGrants {
    pub grants: Vec<WriteGrantRecord>,
    pub git_grants_digest: Option<provenant_core::Digest>,
}

/// Hard host limits, independent of batch concurrency.
pub const HOST_ACTIVE: u64 = 64;
pub const PROJECT_ACTIVE: u64 = 32;
pub const HOST_QUEUED: u64 = 256;

/// Floors and windows recorded on each ticket at submission. A task queued
/// behind its own run's concurrency gets one provider timeout per earlier
/// wave on top of the admission window, so deep batches are not refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionPolicy {
    pub memory_floor_basis_points: u64,
    pub disk_floor_bytes: u64,
    /// Spacing between writer starts.
    pub startup_spacing_ms: u64,
    /// Spacing between read-only starts, kept apart from the writer clock.
    pub read_only_spacing_ms: u64,
    pub admission_seconds: u64,
}
impl Default for AdmissionPolicy {
    /// Defaults: five per cent memory, 500 MiB disk, 20 s writer spacing, 2 s
    /// read-only spacing and a one-hour admission deadline.
    fn default() -> Self {
        Self {
            memory_floor_basis_points: 500,
            disk_floor_bytes: 500 * 1024 * 1024,
            startup_spacing_ms: 20_000,
            read_only_spacing_ms: 2_000,
            admission_seconds: 3600,
        }
    }
}
impl AdmissionPolicy {
    /// The spacing a lane of `class` waits out after the last start of its class.
    pub fn spacing_ms(&self, class: LaneClass) -> u64 {
        match class {
            LaneClass::Writer => self.startup_spacing_ms,
            LaneClass::ReadOnly => self.read_only_spacing_ms,
        }
    }
}

pub use provenant_records::run::LaneWait;

/// Resource probes. A failed probe queues; it never admits unchecked.
pub trait Resources {
    fn memory(&self) -> std::io::Result<MemorySample>;
    fn disk(&self, attempt: &Directory) -> std::io::Result<u64>;
}
pub struct SystemResources;
impl Resources for SystemResources {
    fn memory(&self) -> std::io::Result<MemorySample> {
        resources::memory()
    }
    fn disk(&self, attempt: &Directory) -> std::io::Result<u64> {
        attempt.available_bytes()
    }
}

impl RunStore {
    pub fn set_admission_policy(&mut self, policy: AdmissionPolicy) {
        self.policy = policy;
    }
    pub(crate) fn queue_ticket(
        &self,
        tx: &Connection,
        attempt: &AttemptRecord,
        class: LaneClass,
        concurrency: u64,
        queue_seconds: u64,
    ) -> Result<()> {
        let sequence: i64 = tx.query_row(
            "SELECT sequence FROM attempts WHERE id=?1",
            [attempt.attempt_id.to_string()],
            |r| r.get(0),
        )?;
        let ticket = AdmissionTicket {
            schema_version: 1,
            operation_id: attempt.operation_id.clone(),
            attempt_id: attempt.attempt_id,
            project_id: attempt.project_id,
            sequence: u64::try_from(sequence).map_err(|_| RunError::Conflict)?,
            generation: attempt.generation,
            holder: Some(self.context.actor_id),
            active: false,
            memory_floor_basis_points: self.policy.memory_floor_basis_points,
            disk_floor_bytes: self.policy.disk_floor_bytes,
            startup_spacing_ms: self.policy.spacing_ms(class),
            lane_class: class,
            run_concurrency: concurrency,
            quota_permit: None,
            admission_deadline: deadline(
                attempt.queued_at,
                self.policy.admission_seconds.saturating_add(queue_seconds),
            )?,
        };
        tx.execute(
            "INSERT INTO admission(attempt,project,run,active,record) VALUES (?1,?2,?3,0,?4)",
            params![
                attempt.attempt_id.to_string(),
                attempt.project_id.to_string(),
                attempt.run_id.to_string(),
                bytes(&ticket)?
            ],
        )?;
        Ok(())
    }
    /// What each queued or starting lane in `lanes` is waiting for, in order.
    /// It reads this store and the host ledger without taking a lock, so it
    /// names a wait only when it can see one: run concurrency, then startup
    /// spacing, then the shared host and project caps and resource floors.
    pub fn lane_waits(&self, lanes: &[AttemptRecord], call: &Request<'_>) -> Vec<LaneWait> {
        let guard = self.disk_guard_wait();
        lanes
            .iter()
            .filter(|lane| {
                matches!(lane.state, AttemptState::Queued | AttemptState::Starting)
                    || (lane.state == AttemptState::Running && guard.is_some())
            })
            .filter_map(|lane| {
                let ticket = call.retry(|| self.ticket(&self.db, lane.attempt_id)).ok()?;
                let class = ticket.lane_class;
                let waiting_for = if lane.state == AttemptState::Running {
                    guard.clone()?
                } else if lane.state == AttemptState::Queued {
                    "launch: not yet handed to a guardian".to_owned()
                } else {
                    let active: i64 = self
                        .db
                        .query_row(
                            "SELECT COUNT(*) FROM admission WHERE active=1 AND run=?1",
                            [lane.run_id.to_string()],
                            |r| r.get(0),
                        )
                        .ok()?;
                    let spacing = ticket.startup_spacing_ms;
                    let left = crate::ledger::spacing_left(
                        &self.writer_registry,
                        class,
                        spacing,
                        call.clock,
                    );
                    if active >= i64::try_from(ticket.run_concurrency).ok()? {
                        format!(
                            "run concurrency: {active} of {} lanes of this run are active",
                            ticket.run_concurrency
                        )
                    } else if let Some(left) = left.filter(|left| *left > 0) {
                        format!(
                            "{} startup spacing: {} s left of {} s",
                            class_name(class),
                            tenths(left),
                            tenths(spacing)
                        )
                    } else {
                        "host capacity: active lane caps and memory or disk floors".to_owned()
                    }
                };
                Some(LaneWait {
                    attempt_id: lane.attempt_id,
                    lane_class: class,
                    waiting_for,
                })
            })
            .collect()
    }
    /// The disk guard's saved state names its pause while it holds builds
    /// stopped: the free bytes it last saw and the level it resumes at.
    fn disk_guard_wait(&self) -> Option<String> {
        let path = self.writer_registry.join("disk-guard/paused.json");
        let state: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        if state["paused"].as_array()?.is_empty() {
            return None;
        }
        let gib = |key: &str| {
            let tenths = state[key].as_u64()? * 10 / 1024_u64.pow(3);
            Some(format!("{}.{}", tenths / 10, tenths % 10))
        };
        Some(format!(
            "disk guard paused builds (free {} GiB, resumes at {} GiB)",
            gib("free")?,
            gib("resume_at")?
        ))
    }
    fn ticket(&self, db: &Connection, id: AttemptId) -> Result<AdmissionTicket> {
        let raw: Vec<u8> = db
            .query_row(
                "SELECT record FROM admission WHERE attempt=?1 AND project=?2",
                params![id.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(RunError::NotFound)?;
        let ticket: AdmissionTicket = decode(&raw)?;
        if ticket.attempt_id != id || ticket.project_id != self.context.project_id {
            return Err(RunError::Validation("admission index identity"));
        }
        Ok(ticket)
    }
    /// Record fallback candidates that did not launch, while the attempt is queued.
    pub(crate) fn record_skipped_routes(
        &self,
        id: AttemptId,
        skipped: &[SkippedRoute],
        call: &Request<'_>,
    ) -> Result<()> {
        self.write(call, |tx| {
            let old = self.record(id, call)?;
            if old.state != AttemptState::Queued {
                return Err(RunError::Conflict);
            }
            let mut next = old.clone();
            next.revision = next.revision.checked_add(1).ok_or(RunError::Conflict)?;
            next.skipped_routes = skipped.to_vec();
            update_attempt(tx, &old, &next)
        })
    }
    /// Move a queued attempt to `starting` with a fresh custody generation
    /// and its applied route, just before supervisor handoff. `head` is the
    /// checkout HEAD the host observed for a read-only lane; never lane output.
    pub(crate) fn mark_starting(
        &self,
        expected: &AttemptRecord,
        route: &RouteRecord,
        writable: &[String],
        write_grants: &AppliedWriteGrants,
        head: Option<GitOid>,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        let id = expected.attempt_id;
        if !valid_read_only_writable(writable) {
            return Err(RunError::Validation("invalid read-only writable paths"));
        }
        if !crate::reader::write_grants_fit(&write_grants.grants) {
            return Err(RunError::Validation("invalid write grants"));
        }
        self.write(call, |tx| {
            let old = self.record(id, call)?;
            if old.state != AttemptState::Queued
                || old.run_id != expected.run_id
                || old.task_id != expected.task_id
            {
                return Err(RunError::Conflict);
            }
            let mut ticket = self.ticket(tx, id)?;
            let mut next = old.clone();
            next.revision = next.revision.checked_add(1).ok_or(RunError::Conflict)?;
            next.generation = next.generation.checked_add(1).ok_or(RunError::Conflict)?;
            next.state = AttemptState::Starting;
            next.read_only_writable = writable.to_vec();
            next.write_grants = write_grants.grants.clone();
            next.git_grants_digest = write_grants.git_grants_digest;
            next.head = head;
            next.resolved_route = Some(route.clone());
            next.applied_route = Some(route.clone());
            next.admission_deadline = Some(ticket.admission_deadline);
            // A started child is owned by the host's sealed requester, not the
            // lane actor that wrote its records. The lane's operation keeps its
            // own actor, which scopes its replay and listing.
            if let Some(requester) = self.sealed_requester {
                next.verified_owner = Some(requester);
                ticket.holder = Some(requester);
                let mut run = self.run_on(tx, old.run_id)?;
                if run.actor_id != requester {
                    run.actor_id = requester;
                    run.revision = run.revision.checked_add(1).ok_or(RunError::Conflict)?;
                    tx.execute(
                        "UPDATE runs SET record=?1 WHERE id=?2",
                        params![bytes(&run)?, run.run_id.to_string()],
                    )?;
                }
            }
            ticket.generation = next.generation;
            update_attempt(tx, &old, &next)?;
            tx.execute(
                "UPDATE admission SET record=?1 WHERE attempt=?2",
                params![bytes(&ticket)?, id.to_string()],
            )?;
            Ok(next)
        })
    }
    /// Wait until host capacity, startup spacing and resource floors admit
    /// this attempt, then mark it running. Expiry or a state change refuses.
    pub fn admit(
        &self,
        id: AttemptId,
        attempt: &Directory,
        resources: &dyn Resources,
        clock: &dyn Clock,
        cancel: &CancelToken,
    ) -> Result<AttemptRecord> {
        let mut delay = Duration::from_millis(100);
        loop {
            let call = Request {
                clock,
                budget: WaitBudget::cli(clock.mono_now(), 30)
                    .map_err(|_| RunError::Validation("admission budget"))?,
                cancel,
            };
            let ticket = call.retry(|| self.ticket(&self.db, id))?;
            let now = clock
                .wall_now()
                .map_err(|_| RunError::Validation("wall clock"))?;
            if now >= ticket.admission_deadline {
                return Err(RunError::Refused);
            }
            // An explicit cancel before spawn ends the wait; finish records it.
            if crate::artifacts::read_sealed(attempt, "cancel.json", RECORD_LIMIT)?.is_some() {
                return Err(RunError::Cancelled);
            }
            let fits = match (resources.memory(), resources.disk(attempt)) {
                (Ok(memory), Ok(disk)) => {
                    memory.total_bytes > 0
                        && u128::from(memory.available_bytes) * 10_000
                            >= u128::from(memory.total_bytes)
                                * u128::from(ticket.memory_floor_basis_points)
                        && disk >= ticket.disk_floor_bytes
                }
                _ => false,
            };
            if fits {
                match self.claim(id, clock, &call) {
                    Ok(Some(record)) => return Ok(record),
                    // A busy store is transient; keep waiting to the deadline.
                    Ok(None) | Err(RunError::Contention) => {}
                    Err(error) => return Err(error),
                }
            }
            if cancel.is_cancelled() {
                return Err(RunError::Cancelled);
            }
            let left = u64::try_from(
                ticket
                    .admission_deadline
                    .as_micros()
                    .saturating_sub(now.as_micros()),
            )
            .unwrap_or(0);
            let pause = delay.min(Duration::from_micros(left));
            let deadline = clock
                .mono_now()
                .checked_add(provenant_core::DurationMicros::from_micros(
                    u64::try_from(pause.as_micros()).unwrap_or(u64::MAX),
                ))
                .map_err(|_| RunError::Validation("admission budget"))?;
            clock
                .wait_until(deadline)
                .map_err(|_| RunError::Validation("admission budget"))?;
            delay = (delay * 2).min(Duration::from_secs(2));
        }
    }
    pub(crate) fn claim(
        &self,
        id: AttemptId,
        clock: &dyn Clock,
        call: &Request<'_>,
    ) -> Result<Option<AttemptRecord>> {
        let ledger = crate::ledger::open(&self.writer_registry, call)?;
        self.write(call, |tx| {
            let old = self.record(id, call)?;
            if old.state != AttemptState::Starting {
                return Err(RunError::Refused);
            }
            let mut ticket = self.ticket(tx, id)?;
            let run: i64 = tx.query_row(
                "SELECT COUNT(*) FROM admission WHERE active=1 AND run=?1",
                [old.run_id.to_string()],
                |r| r.get(0),
            )?;
            if run >= sql_int(ticket.run_concurrency)? {
                return Ok(None);
            }
            // Host and project counts and spacing span every execution
            // namespace, so they live in the host ledger, not this store.
            // The ledger reads the clock under its own lock; the lane starts at
            // the time spacing was judged and recorded.
            let Some(now) = crate::ledger::lease(
                &ledger,
                self.context.state_root.as_path(),
                &ticket,
                clock,
                call,
            )?
            else {
                return Ok(None);
            };
            ticket.active = true;
            tx.execute(
                "UPDATE admission SET active=1, record=?1 WHERE attempt=?2",
                params![bytes(&ticket)?, id.to_string()],
            )?;
            let mut next = old.clone();
            next.revision = next.revision.checked_add(1).ok_or(RunError::Conflict)?;
            next.state = AttemptState::Running;
            next.started_at = Some(now);
            update_attempt(tx, &old, &next)?;
            Ok(Some(next))
        })
    }
}
fn class_name(class: LaneClass) -> &'static str {
    match class {
        LaneClass::Writer => "writer",
        LaneClass::ReadOnly => "read-only",
    }
}
/// Milliseconds as seconds to one decimal place, rounded up so a wait never
/// reads as zero while it holds.
fn tenths(ms: u64) -> String {
    let tenths = ms.div_ceil(100);
    format!("{}.{}", tenths / 10, tenths % 10)
}
fn deadline(from: WallTime, seconds: u64) -> Result<WallTime> {
    DurationMicros::from_seconds(seconds)
        .and_then(|duration| from.checked_add(duration))
        .map_err(|_| RunError::Validation("admission deadline"))
}
pub(crate) fn update_attempt(
    tx: &Connection,
    old: &AttemptRecord,
    next: &AttemptRecord,
) -> Result<()> {
    crate::reader::validate_attempt(next)?;
    let changed = tx.execute(
        "UPDATE attempts SET revision=?1, record=?2 WHERE id=?3 AND revision=?4 AND terminal=0",
        params![
            sql_int(next.revision)?,
            bytes(next)?,
            next.attempt_id.to_string(),
            sql_int(old.revision)?
        ],
    )?;
    if changed != 1 {
        return Err(RunError::Conflict);
    }
    insert_event(tx, next)
}
