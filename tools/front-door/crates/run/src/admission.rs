//! Host admission. Each attempt waits inside its own guardian, before spawn;
//! there is no scheduler daemon. Resource samples are taken outside the store
//! transaction, which then rechecks counts and spacing while claiming capacity.
use crate::{
    db::{bytes, decode, sql_int},
    store::insert_event,
    *,
};
use provenant_confinement::valid_read_only_writable;
use provenant_core::{AttemptId, CancelToken, Clock, DurationMicros, GitOid, WaitBudget, WallTime};
use provenant_supervisor::{
    fs::Directory,
    sys::resources::{self, MemorySample},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::time::Duration;

/// Hard host limits, independent of batch concurrency.
pub const HOST_ACTIVE: u64 = 64;
pub const PROJECT_ACTIVE: u64 = 16;
pub const HOST_QUEUED: u64 = 256;

/// Floors and windows recorded on each ticket at submission. A task queued
/// behind its own run's concurrency gets one provider timeout per earlier
/// wave on top of the admission window, so deep batches are not refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionPolicy {
    pub memory_floor_basis_points: u64,
    pub disk_floor_bytes: u64,
    pub startup_spacing_ms: u64,
    pub admission_seconds: u64,
}
impl Default for AdmissionPolicy {
    /// Read-only defaults: five per cent memory, 500 MiB disk, 20 s spacing
    /// and a one-hour admission deadline.
    fn default() -> Self {
        Self {
            memory_floor_basis_points: 500,
            disk_floor_bytes: 500 * 1024 * 1024,
            startup_spacing_ms: 20_000,
            admission_seconds: 3600,
        }
    }
}

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
            startup_spacing_ms: self.policy.startup_spacing_ms,
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
        id: AttemptId,
        route: &RouteRecord,
        writable: &[String],
        head: Option<GitOid>,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        if !valid_read_only_writable(writable) {
            return Err(RunError::Validation("invalid read-only writable paths"));
        }
        self.write(call, |tx| {
            let old = self.record(id, call)?;
            if old.state != AttemptState::Queued {
                return Err(RunError::Conflict);
            }
            let mut ticket = self.ticket(tx, id)?;
            let mut next = old.clone();
            next.revision = next.revision.checked_add(1).ok_or(RunError::Conflict)?;
            next.generation = next.generation.checked_add(1).ok_or(RunError::Conflict)?;
            next.state = AttemptState::Starting;
            next.read_only_writable = writable.to_vec();
            next.head = head;
            next.resolved_route = Some(route.clone());
            next.applied_route = Some(route.clone());
            next.admission_deadline = Some(ticket.admission_deadline);
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
                match self.claim(id, now, &call) {
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
            std::thread::park_timeout(delay.min(Duration::from_micros(left)));
            delay = (delay * 2).min(Duration::from_secs(2));
        }
    }
    pub(crate) fn claim(
        &self,
        id: AttemptId,
        now: WallTime,
        call: &Request<'_>,
    ) -> Result<Option<AttemptRecord>> {
        self.write(call, |tx| {
            let old = self.record(id, call)?;
            if old.state != AttemptState::Starting {
                return Err(RunError::Refused);
            }
            let mut ticket = self.ticket(tx, id)?;
            let (host, project, run): (i64, i64, i64) = tx.query_row(
                "SELECT COUNT(*), COUNT(*) FILTER (WHERE project=?1), COUNT(*) FILTER (WHERE run=?2) FROM admission WHERE active=1",
                params![old.project_id.to_string(), old.run_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            if host >= sql_int(HOST_ACTIVE)?
                || project >= sql_int(PROJECT_ACTIVE)?
                || run >= sql_int(ticket.run_concurrency)?
            {
                return Ok(None);
            }
            let now_ms = now.as_micros() / 1000;
            let last: Option<i64> = tx
                .query_row("SELECT last_start FROM spacing WHERE id=1", [], |r| r.get(0))
                .optional()?;
            let spacing = sql_int(ticket.startup_spacing_ms)?;
            // A wall clock that stepped back does not hold every start.
            if last.is_some_and(|last| last <= now_ms && now_ms - last < spacing) {
                return Ok(None);
            }
            tx.execute(
                "INSERT INTO spacing(id,last_start) VALUES (1,?1) ON CONFLICT(id) DO UPDATE SET last_start=excluded.last_start",
                [now_ms],
            )?;
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
