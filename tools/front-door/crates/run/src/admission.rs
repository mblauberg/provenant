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
use provenant_core::retry::Backoff;
use provenant_core::{AttemptId, CancelToken, Clock, DurationMicros, GitOid, WallTime};
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

/// Fixed ceilings on live lanes of any class, independent of batch
/// concurrency. The writer caps are configurable below these
/// (`domains.fabric.lanes.max_writer_lanes`, default 40, and
/// `max_project_writer_lanes`, default 30).
pub const HOST_ACTIVE: u64 = 64;
pub const PROJECT_ACTIVE: u64 = 32;
pub const HOST_QUEUED: u64 = 256;

/// Floors and windows recorded on each ticket at submission. A task queued
/// behind its own run's concurrency gets one provider timeout per earlier
/// wave on top of the admission window, so deep batches are not refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionPolicy {
    pub memory_floor_basis_points: u64,
    /// Spacing between writer starts.
    pub startup_spacing_ms: u64,
    /// Spacing between read-only starts, kept apart from the writer clock.
    pub read_only_spacing_ms: u64,
    pub admission_seconds: u64,
}
impl Default for AdmissionPolicy {
    /// Defaults: five per cent memory, the GC disk floor, 20 s writer spacing, 2 s
    /// read-only spacing and a one-hour admission deadline.
    fn default() -> Self {
        Self {
            memory_floor_basis_points: 500,
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
    fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
        match provenant_supervisor::census::SystemProcessObserver
            .observe(i64::from(std::process::id()))
        {
            provenant_supervisor::census::ProcessObservation::Present(fact) if !fact.zombie => {
                Some(fact.identity)
            }
            _ => None,
        }
    }
    fn pressure(&self) -> std::io::Result<Option<resources::MemoryPressure>> {
        Ok(None)
    }
    /// The user's process count against its limit. Test probes are unlimited.
    fn process_budget(&self) -> std::io::Result<resources::ProcessBudget> {
        Ok(resources::ProcessBudget::UNLIMITED)
    }
    fn disk(&self, attempt: &Directory) -> std::io::Result<u64>;
    /// Host admission observes the same output volumes as the running guardian.
    /// Test probes may retain their synthetic attempt-level observation.
    fn lane_disk(&self, attempt: &Directory, _: &HolderContext) -> std::io::Result<u64> {
        self.disk(attempt)
    }
    /// Free lane build output when admission finds the disk below its floor.
    /// Best effort; true means space may have been freed, so the caller
    /// samples the disk again.
    fn reclaim_disk(&self, _: &Directory) -> bool {
        false
    }
    /// Reclaim from the output roots the running guardian was sealed with.
    fn reclaim_lane_disk(&self, attempt: &Directory, _: &HolderContext) -> bool {
        self.reclaim_disk(attempt)
    }
}
/// A starved admission retries collection about once a minute: its backoff
/// caps at two seconds, and a sweep walks every lane target.
const RECLAIM_EVERY: u32 = 30;
pub struct SystemResources;
impl Resources for SystemResources {
    fn memory(&self) -> std::io::Result<MemorySample> {
        resources::memory()
    }
    fn pressure(&self) -> std::io::Result<Option<resources::MemoryPressure>> {
        resources::memory_pressure()
    }
    fn process_budget(&self) -> std::io::Result<resources::ProcessBudget> {
        resources::process_budget()
    }
    fn disk(&self, attempt: &Directory) -> std::io::Result<u64> {
        attempt.available_bytes()
    }
    fn lane_disk(&self, _: &Directory, context: &HolderContext) -> std::io::Result<u64> {
        let (state, cwd) = context.disk_roots();
        provenant_confinement::disk::lane_available_bytes(state, &context.roots.home, cwd)
    }
    fn reclaim_lane_disk(&self, _: &Directory, context: &HolderContext) -> bool {
        let (state, _) = context.disk_roots();
        crate::build_dir_gc::reclaim(state, &context.roots.home)
            .is_some_and(|report| !report.removed.is_empty())
    }
}

/// How long a lane may wait to start, and then run.
#[derive(Clone, Copy)]
pub(crate) struct Allowance {
    pub queue_seconds: u64,
    pub runtime_seconds: u64,
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
        allowance: Allowance,
        disk_floor_bytes: u64,
    ) -> Result<()> {
        let sequence: i64 = tx.query_row(
            "SELECT sequence FROM attempts WHERE id=?1",
            [attempt.attempt_id.to_string()],
            |r| r.get(0),
        )?;
        let ticket = AdmissionTicket {
            extra: Default::default(),
            schema_version: 1,
            operation_id: attempt.operation_id.clone(),
            attempt_id: attempt.attempt_id,
            project_id: attempt.project_id,
            sequence: u64::try_from(sequence).map_err(|_| RunError::Conflict)?,
            generation: attempt.generation,
            holder: Some(self.context.actor_id),
            active: false,
            memory_floor_basis_points: self.policy.memory_floor_basis_points,
            disk_floor_bytes,
            startup_spacing_ms: self.policy.spacing_ms(class),
            lane_class: class,
            run_concurrency: concurrency,
            quota_permit: None,
            admission_deadline: deadline(
                attempt.queued_at,
                self.policy
                    .admission_seconds
                    .saturating_add(allowance.queue_seconds),
            )?,
            runtime_seconds: allowance.runtime_seconds,
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
        self.lane_waits_with_resources(lanes, call, &SystemResources)
    }
    pub fn lane_waits_with_resources(
        &self,
        lanes: &[AttemptRecord],
        call: &Request<'_>,
        resources: &dyn Resources,
    ) -> Vec<LaneWait> {
        let pressure_threshold = provenant_config::LaneBuildSettings::read(&self.writer_registry)
            .map(|settings| settings.memory_pressure_threshold)
            .unwrap_or_default();
        lanes
            .iter()
            .filter(|lane| {
                matches!(
                    lane.state,
                    AttemptState::Queued { .. } | AttemptState::Starting { .. }
                ) || matches!(lane.state, AttemptState::Running { .. })
            })
            .filter_map(|lane| {
                let ticket = self.ticket(&self.db, lane.attempt_id).ok()?;
                let class = ticket.lane_class;
                let waiting_for = if matches!(lane.state, AttemptState::Running { .. }) {
                    self.disk_pause_wait(lane)
                        .or_else(|| self.build_slot_wait(lane, call))?
                } else if matches!(lane.state, AttemptState::Queued { .. }) {
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
                    } else if let Ok(Some(pressure)) = resources.pressure()
                        && pressure_threshold.defers(pressure)
                    {
                        format!("host capacity: memory pressure {pressure:?}")
                    } else if let Ok(budget) = resources.process_budget()
                        && !budget.has_room()
                    {
                        format!(
                            "host capacity: user process count {} of {}",
                            budget.used, budget.limit
                        )
                    } else {
                        "host capacity: active lane caps and memory or disk floors".to_owned()
                    }
                };
                Some(LaneWait {
                    extra: Default::default(),
                    attempt_id: lane.attempt_id,
                    lane_class: class,
                    waiting_for,
                })
            })
            .collect()
    }
    /// A gated build in the lane says it waits by writing a marker in the
    /// lane's scratch directory. The lane writes it, so it is read
    /// without following links, bounded, and trusted only for which of two
    /// fixed waits it names and that it is fresh.
    fn build_slot_wait(&self, lane: &AttemptRecord, call: &Request<'_>) -> Option<String> {
        use provenant_os::build_slots::{WAIT_MARKER, WaitKind};
        let attempt = self.attempt_dir(lane.run_id, lane.attempt_id, false).ok()?;
        match attempt.metadata_at("process-exit.json".as_ref(), false) {
            Err(error) if error.is_not_found() => {}
            _ => return None,
        }
        let temporary = attempt.open_directory("tmp".as_ref()).ok()?;
        let now = u64::try_from(call.clock.wall_now().ok()?.as_micros() / 1_000_000).ok()?;
        // The marker sits in the lane's scratch (`tmp/candidate-N`), one file
        // per waiting command. Bound how much a lane can make the host scan.
        let kind = temporary
            .entries()
            .ok()?
            .map_while(|entry| entry.ok())
            .take(32)
            .filter_map(|name| temporary.open_directory(&name).ok())
            .find_map(|scratch| {
                scratch
                    .entries()
                    .ok()?
                    .map_while(|entry| entry.ok())
                    .filter(|name| name.to_string_lossy().starts_with(WAIT_MARKER))
                    .take(32)
                    .find_map(|name| {
                        let raw = scratch.read_stable(&name, 64).ok()?;
                        WaitKind::from_marker(std::str::from_utf8(&raw).ok()?, now)
                    })
            })?;
        match kind {
            WaitKind::Slot => {
                Some("build slot: a cargo build waits for one of the host's build slots".to_owned())
            }
            WaitKind::CargoLock => Some(
                "build slot: a cargo build waits for another build's lock in its target directory"
                    .to_owned(),
            ),
        }
    }
    /// The running guardian publishes this lane's pause in its checkpoint.
    fn disk_pause_wait(&self, lane: &AttemptRecord) -> Option<String> {
        let attempt = self.attempt_dir(lane.run_id, lane.attempt_id, false).ok()?;
        // The periodic checkpoint may predate provider exit while a living
        // guardian retains unresolved cleanup. Exit publication ends pausing.
        match attempt.metadata_at("process-exit.json".as_ref(), false) {
            Err(error) if error.is_not_found() => {}
            _ => return None,
        }
        let raw = attempt
            .read_stable("process.json".as_ref(), RECORD_LIMIT)
            .ok()?;
        let state: serde_json::Value = serde_json::from_slice(&raw).ok()?;
        let guardian: provenant_supervisor::census::ProcessIdentity =
            serde_json::from_value(state.get("guardian")?.clone()).ok()?;
        if !matches!(
            provenant_supervisor::census::SystemProcessObserver.observe(guardian.pid),
            provenant_supervisor::census::ProcessObservation::Present(fact)
                if fact.identity == guardian && !fact.zombie
        ) {
            return None;
        }
        state["disk_paused"].as_bool()?.then(|| {
            "disk pressure paused builds below gc.free_floor; run `provenant gc --apply`".to_owned()
        })
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
            if !matches!(old.state, AttemptState::Queued { .. }) {
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
    #[cfg(test)]
    pub(crate) fn mark_starting(
        &self,
        expected: &AttemptRecord,
        route: &RouteRecord,
        writable: &[String],
        write_grants: &AppliedWriteGrants,
        head: Option<GitOid>,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        self.mark_starting_with_priority(
            expected,
            route,
            writable,
            write_grants,
            head,
            None,
            0,
            call,
        )
    }

    #[expect(clippy::too_many_arguments, reason = "priority is launch evidence")]
    pub(crate) fn mark_starting_with_priority(
        &self,
        expected: &AttemptRecord,
        route: &RouteRecord,
        writable: &[String],
        write_grants: &AppliedWriteGrants,
        head: Option<GitOid>,
        priority: Option<u8>,
        runtime_seconds: u64,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        let id = expected.attempt_id;
        if !valid_read_only_writable(writable) {
            return Err(RunError::Validation("invalid read-only writable paths"));
        }
        if !crate::reader::write_grants_fit(&write_grants.grants) {
            return Err(RunError::Validation("invalid write grants"));
        }
        let next = self.write(call, |tx| {
            let old = self.record(id, call)?;
            if !matches!(old.state, AttemptState::Queued { .. })
                || old.run_id != expected.run_id
                || old.task_id != expected.task_id
            {
                return Err(RunError::Conflict);
            }
            let mut ticket = self.ticket(tx, id)?;
            let mut next = old.clone();
            next.revision = next.revision.checked_add(1).ok_or(RunError::Conflict)?;
            next.generation = next.generation.checked_add(1).ok_or(RunError::Conflict)?;
            next.state = AttemptState::Starting {
                extra: Default::default(),
            };
            next.read_only_writable = writable.to_vec();
            next.write_grants = write_grants.grants.clone();
            next.git_grants_digest = write_grants.git_grants_digest;
            next.head = head;
            next.requested_priority = priority;
            next.priority = None;
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
            // The provider's host cap may tighten the allowance fixed at submission.
            if runtime_seconds > 0 {
                ticket.runtime_seconds = runtime_seconds;
            }
            update_attempt(tx, &old, &next)?;
            tx.execute(
                "UPDATE admission SET record=?1 WHERE attempt=?2",
                params![bytes(&ticket)?, id.to_string()],
            )?;
            Ok(next)
        })?;
        self.contribution_diagnostic(next.run_id, self.retain_contribution_route(&next, call));
        Ok(next)
    }
    /// Wait until host capacity, startup spacing and resource floors admit
    /// this attempt, then mark it running. Expiry or a state change refuses.
    #[allow(clippy::print_stderr)]
    pub fn admit(
        &self,
        id: AttemptId,
        attempt: &Directory,
        resources: &dyn Resources,
        clock: &dyn Clock,
        cancel: &CancelToken,
    ) -> Result<AttemptRecord> {
        let backoff = Backoff::new(Duration::from_millis(100), 2, Duration::from_secs(2));
        let mut retries = 0_u64;
        let mut refusal = None;
        let mut below_floor = 0_u32;
        loop {
            let call = Request { clock, cancel };
            let ticket = self.ticket(&self.db, id)?;
            let now = clock
                .wall_now()
                .map_err(|_| RunError::Validation("wall clock"))?;
            if now >= ticket.admission_deadline {
                return Err(refusal.unwrap_or(RunError::Refused));
            }
            // An explicit cancel before spawn ends the wait; finish records it.
            if crate::artifacts::read_sealed(attempt, "cancel.json", RECORD_LIMIT)?.is_some() {
                return Err(RunError::Cancelled);
            }
            // Disk policy is live configuration, shared with the guardian and
            // settlement. An unreadable current floor queues rather than
            // admitting with the ticket's submission-time observation.
            let disk_floor_bytes = provenant_config::GcSettings::read(&self.writer_registry)
                .and_then(|settings| settings.free_floor_bytes(&self.writer_registry));
            let pressure = resources.pressure().unwrap_or_else(|error| {
                eprintln!("admission: memory pressure probe failed: {error}");
                None
            });
            // Build slots gate builds inside lanes, not admission, and range
            // errors are reported when configuration loads. Only a file that
            // cannot be read as settings refuses here.
            let settings = provenant_config::LaneBuildSettings::read(&self.writer_registry);
            if settings
                .as_ref()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::InvalidInput)
            {
                return Err(RunError::Admission(AdmissionError::BuildSettings));
            }
            // Like pressure, an unreadable process budget is logged and does
            // not hold the lane: the limit is unknown, not exceeded.
            let processes = resources
                .process_budget()
                .inspect_err(|error| eprintln!("admission: process budget probe failed: {error}"))
                .ok()
                .filter(|budget| !budget.has_room());
            let fits = match (
                resources.memory(),
                pressure,
                resources.disk(attempt),
                disk_floor_bytes,
                settings,
            ) {
                (Ok(memory), pressure, Ok(disk), Ok(floor), Ok(settings)) => {
                    let pressure =
                        pressure.filter(|level| settings.memory_pressure_threshold.defers(*level));
                    let mut disk = disk;
                    if disk < floor {
                        if below_floor.is_multiple_of(RECLAIM_EVERY)
                            && resources.reclaim_disk(attempt)
                        {
                            disk = resources.disk(attempt).unwrap_or(disk);
                        }
                        below_floor = below_floor.saturating_add(1);
                    }
                    refusal = if disk < floor {
                        Some(RunError::Admission(AdmissionError::Disk {
                            free: disk,
                            floor,
                        }))
                    } else if let Some(level) = pressure {
                        Some(RunError::Admission(AdmissionError::MemoryPressure {
                            level: format!("{level:?}"),
                        }))
                    } else {
                        processes.map(|budget| {
                            RunError::Admission(AdmissionError::Processes {
                                used: budget.used,
                                limit: budget.limit,
                            })
                        })
                    };
                    pressure.is_none()
                        && processes.is_none()
                        && memory.total_bytes > 0
                        && u128::from(memory.available_bytes) * 10_000
                            >= u128::from(memory.total_bytes)
                                * u128::from(ticket.memory_floor_basis_points)
                        && disk >= floor
                }
                _ => {
                    refusal = None;
                    false
                }
            };
            if fits {
                match self.claim_observed(id, clock, &call, &|| resources.guardian_identity()) {
                    Ok(Some(record)) => return Ok(record),
                    // A busy store is transient; keep waiting to the deadline.
                    Ok(None) => {
                        // Name the writer cap when it is what holds this lane.
                        if ticket.lane_class == LaneClass::Writer
                            && let Some(hit) = self.writer_cap_hit(&ticket, &call)
                        {
                            refusal = Some(RunError::Admission(AdmissionError::LaneCap {
                                active: hit.active,
                                limit: hit.limit,
                                scope: hit.scope,
                            }));
                        }
                    }
                    Err(RunError::Admission(AdmissionError::GuardianIdentity)) => {
                        refusal = Some(RunError::Admission(AdmissionError::GuardianIdentity));
                    }
                    Err(RunError::Contention) => {}
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
            let pause = backoff.delay(retries).min(Duration::from_micros(left));
            let deadline = clock
                .mono_now()
                .checked_add(provenant_core::DurationMicros::from_micros(
                    u64::try_from(pause.as_micros()).unwrap_or(u64::MAX),
                ))
                .map_err(|_| RunError::Validation("admission budget"))?;
            clock
                .wait_until(deadline)
                .map_err(|_| RunError::Validation("admission budget"))?;
            retries = retries.saturating_add(1);
        }
    }
    #[cfg(test)]
    pub(crate) fn claim(
        &self,
        id: AttemptId,
        clock: &dyn Clock,
        call: &Request<'_>,
    ) -> Result<Option<AttemptRecord>> {
        self.claim_observed(id, clock, call, &|| SystemResources.guardian_identity())
    }
    fn lane_caps(&self) -> crate::ledger::LaneCaps {
        crate::ledger::LaneCaps::of(
            &provenant_config::LaneBuildSettings::read(&self.writer_registry).unwrap_or_default(),
        )
    }
    fn writer_cap_hit(
        &self,
        ticket: &AdmissionTicket,
        call: &Request<'_>,
    ) -> Option<crate::ledger::CapHit> {
        let ledger = crate::ledger::open(&self.writer_registry, call).ok()?;
        crate::ledger::cap_hit(
            &ledger,
            ticket.project_id,
            ticket.lane_class,
            self.lane_caps(),
        )
        .ok()?
    }
    fn claim_observed(
        &self,
        id: AttemptId,
        clock: &dyn Clock,
        call: &Request<'_>,
        observe: &dyn Fn() -> Option<provenant_supervisor::census::ProcessIdentity>,
    ) -> Result<Option<AttemptRecord>> {
        // Avoid the ledger and store write locks when this run is already full.
        // The transaction below repeats the count to decide the claim.
        if let (Ok(record), Ok(ticket)) = (self.record(id, call), self.ticket(&self.db, id))
            && matches!(record.state, AttemptState::Starting { .. })
            && let Ok(active) = self.db.query_row(
                "SELECT COUNT(*) FROM admission WHERE active=1 AND run=?1",
                [record.run_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            && active >= sql_int(ticket.run_concurrency)?
        {
            return Ok(None);
        }
        let ledger = crate::ledger::open(&self.writer_registry, call)?;
        let caps = self.lane_caps();
        self.write(call, |tx| {
            let old = self.record(id, call)?;
            if !matches!(old.state, AttemptState::Starting { .. }) {
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
                caps,
                clock,
                call,
                observe,
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
            next.state = AttemptState::Running {
                extra: Default::default(),
            };
            next.started_at = Some(now);
            // The guardian enforces the same allowance on the wall clock; the
            // record lets `lanes` and `reconcile` see it without the guardian.
            next.runtime_deadline = (ticket.runtime_seconds > 0)
                .then(|| deadline(now, ticket.runtime_seconds))
                .transpose()?;
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
