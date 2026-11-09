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
use provenant_core::{
    AttemptId, CancelToken, Clock, DurationMicros, GitOid, OperationId, WallTime,
};
use provenant_os::build_slots::{SlotWait, WaitKind};
use provenant_os::{
    fs::Directory,
    sys::resources::{self, MemorySample},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Write evidence from the prepared confinement plan, never launch authority.
#[derive(Default)]
pub(crate) struct AppliedWriteGrants {
    pub grants: Vec<WriteGrantRecord>,
    pub git_grants_digest: Option<provenant_core::Digest>,
}

/// Upper bounds for explicit lane-count settings, independent of batch
/// concurrency. Writer caps are configurable below these
/// (`domains.fabric.lanes.max_writer_lanes` and
/// `max_project_writer_lanes`, both disabled by default).
pub const HOST_ACTIVE: u64 = provenant_config::WRITER_LANES_CEILING;
pub const PROJECT_ACTIVE: u64 = provenant_config::PROJECT_WRITER_LANES_CEILING;
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
    /// Defaults: five per cent memory, no startup spacing and a one-hour deadline.
    fn default() -> Self {
        Self {
            memory_floor_basis_points: 500,
            startup_spacing_ms: 0,
            read_only_spacing_ms: 0,
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

    /// Resolve the effective admission timeout according to priority:
    /// CLI flag (`--admission-timeout`) > host/project config (`[domains.run] admission_seconds`) > shipped default.
    pub fn resolve_timeout(
        flag: Option<u64>,
        configured: Option<u64>,
    ) -> EffectiveAdmissionTimeout {
        if let Some(seconds) = flag {
            EffectiveAdmissionTimeout {
                seconds,
                origin: AdmissionTimeoutOrigin::Flag,
            }
        } else if let Some(seconds) = configured {
            EffectiveAdmissionTimeout {
                seconds,
                origin: AdmissionTimeoutOrigin::HostConfig,
            }
        } else {
            EffectiveAdmissionTimeout {
                seconds: Self::default().admission_seconds,
                origin: AdmissionTimeoutOrigin::ShippedDefault,
            }
        }
    }
}

/// Source of the effective admission timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionTimeoutOrigin {
    /// Passed explicitly via `--admission-timeout`.
    Flag,
    /// Configured under `[domains.run] admission_seconds`.
    HostConfig,
    /// The shipped default (`AdmissionPolicy::default().admission_seconds`).
    ShippedDefault,
}

impl AdmissionTimeoutOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::HostConfig => "host config",
            Self::ShippedDefault => "shipped default",
        }
    }
}

impl std::fmt::Display for AdmissionTimeoutOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Effective admission timeout and its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveAdmissionTimeout {
    pub seconds: u64,
    pub origin: AdmissionTimeoutOrigin,
}

/// Persisted admission window for an operation before admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionWindow {
    /// Absolute end of the admission window on the wall clock.
    pub deadline: WallTime,
    /// Earliest next reprobe / admission retry.
    pub wake_at: Option<WallTime>,
}

pub use provenant_records::run::LaneWait;

/// Resource probes for configured admission guards and advisory observations.
pub trait Resources {
    fn memory(&self) -> std::io::Result<MemorySample>;
    /// Observe one process when deciding whether a recorded guardian is gone.
    fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation;
    fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
        match self.observe_process(i64::from(std::process::id())) {
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
    fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
        provenant_supervisor::census::SystemProcessObserver.observe(pid)
    }
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
    /// Yield admission while waiting on children, or try to reclaim it before
    /// resuming. False means reclaim must be retried; the running attempt and
    /// its output remain intact and its state records the admission wait.
    pub fn set_child_wait(&self, id: AttemptId, waiting: bool, call: &Request<'_>) -> Result<bool> {
        self.set_child_wait_observed(id, waiting, call, &SystemResources)
    }

    pub(crate) fn set_child_wait_observed(
        &self,
        id: AttemptId,
        waiting: bool,
        call: &Request<'_>,
        resources: &dyn Resources,
    ) -> Result<bool> {
        let ledger = crate::ledger::open(&self.writer_registry, call)?;
        let owner = if waiting {
            None
        } else {
            Some(crate::ledger::prepare_lease(
                &ledger,
                self.context.state_root.as_path(),
                &self.ticket(&self.db, id)?,
                self.lane_caps(),
                call.clock,
                resources,
            )?)
        };

        self.write(call, |tx| {
            let old = self.record(id, call)?;
            let AttemptState::Running { .. } = &old.state else {
                return Err(RunError::Refused);
            };
            let mut ticket = self.ticket(tx, id)?;
            let mut next = old.clone();
            let AttemptState::Running { extra } = &mut next.state else {
                unreachable!();
            };
            let admitted = if waiting {
                crate::ledger::yield_lease(&ledger, self.context.state_root.as_path(), id, call)?;
                ticket.active = false;
                extra.insert("child_wait".into(), "children".into());
                true
            } else {
                // The ledger may have committed a yield before a failed store
                // commit. Always recheck it, even if this ticket says active.
                let active: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM admission WHERE active=1 AND run=?1 AND attempt<>?2",
                    params![old.run_id.to_string(), id.to_string()],
                    |r| r.get(0),
                )?;
                let reclaimed = active < sql_int(ticket.run_concurrency)?
                    && crate::ledger::lease(
                        &ledger,
                        self.context.state_root.as_path(),
                        &ticket,
                        self.lane_caps(),
                        call.clock,
                        call,
                        owner.as_ref().ok_or(RunError::Conflict)?,
                    )?
                    .is_some();
                ticket.active = reclaimed;
                if reclaimed {
                    extra.remove("child_wait");
                } else {
                    extra.insert("child_wait".into(), "admission".into());
                }
                reclaimed
            };
            tx.execute(
                "UPDATE admission SET active=?1,record=?2 WHERE attempt=?3",
                params![ticket.active, bytes(&ticket)?, id.to_string()],
            )?;
            if next.state != old.state {
                next.revision = next.revision.checked_add(1).ok_or(RunError::Conflict)?;
                update_attempt(tx, &old, &next)?;
            }
            Ok(admitted)
        })
    }

    pub fn admission_policy(&self) -> AdmissionPolicy {
        self.policy
    }
    pub fn set_admission_policy(&mut self, policy: AdmissionPolicy) {
        self.policy = policy;
    }
    /// Return the persisted admission window for an operation, if one was recorded.
    pub fn admission_window(
        &self,
        operation: &OperationId,
        call: &Request<'_>,
    ) -> Result<Option<AdmissionWindow>> {
        call.check()?;
        let row: Option<(i64, Option<i64>)> = self
            .db
            .query_row(
                "SELECT deadline, wake_at FROM admission_windows WHERE project=?1 AND actor=?2 AND operation=?3",
                params![
                    self.context.project_id.to_string(),
                    self.context.actor_id.to_string(),
                    operation.as_str(),
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let Some((deadline_micros, wake_micros)) = row else {
            return Ok(None);
        };
        let deadline =
            WallTime::from_micros(deadline_micros).map_err(|_| RunError::Validation("deadline"))?;
        let wake_at = wake_micros
            .map(|micros| {
                WallTime::from_micros(micros).map_err(|_| RunError::Validation("wake_at"))
            })
            .transpose()?;
        Ok(Some(AdmissionWindow { deadline, wake_at }))
    }

    /// Save or update an admission window for an operation before admission.
    /// If an admission window already exists for this operation, its original
    /// deadline is preserved (never renewed across restarts/reprobes), and only
    /// `wake_at` is updated.
    pub fn save_admission_window(
        &self,
        operation: &OperationId,
        window: AdmissionWindow,
        call: &Request<'_>,
    ) -> Result<AdmissionWindow> {
        self.write(call, |tx| {
            let deadline_int = window.deadline.as_micros();
            let wake_int = window.wake_at.map(|w| w.as_micros());
            tx.execute(
                "INSERT INTO admission_windows (project, actor, operation, deadline, wake_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(project, actor, operation) DO UPDATE SET wake_at = excluded.wake_at",
                params![
                    self.context.project_id.to_string(),
                    self.context.actor_id.to_string(),
                    operation.as_str(),
                    deadline_int,
                    wake_int,
                ],
            )?;
            let (effective_deadline, effective_wake): (i64, Option<i64>) = tx.query_row(
                "SELECT deadline, wake_at FROM admission_windows WHERE project=?1 AND actor=?2 AND operation=?3",
                params![
                    self.context.project_id.to_string(),
                    self.context.actor_id.to_string(),
                    operation.as_str(),
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let deadline = WallTime::from_micros(effective_deadline)
                .map_err(|_| RunError::Validation("deadline"))?;
            let wake_at = effective_wake
                .map(|micros| {
                    WallTime::from_micros(micros).map_err(|_| RunError::Validation("wake_at"))
                })
                .transpose()?;
            Ok(AdmissionWindow { deadline, wake_at })
        })
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
            startup_spacing_ms: provenant_config::LaneBuildSettings::read(&self.writer_registry)
                .unwrap_or_default()
                .start_spacing_ms
                .unwrap_or_else(|| self.policy.spacing_ms(class)),
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

    /// Sample the admission inputs for a fixture timeout. These are current
    /// observations, not a record of which condition held an earlier probe.
    #[cfg(any(test, feature = "fixture"))]
    pub fn admission_observations(
        &self,
        lane: &AttemptRecord,
        call: &Request<'_>,
        resources: &dyn Resources,
    ) -> Result<String> {
        let ticket = self.ticket(&self.db, lane.attempt_id)?;
        let attempt = self.attempt_dir(lane.run_id, lane.attempt_id, false)?;
        let memory = resources.memory();
        let disk = resources.disk(&attempt);
        let floor = provenant_config::GcSettings::read(&self.writer_registry)
            .and_then(|settings| settings.free_floor_bytes(&self.writer_registry));
        let pressure = resources.pressure();
        let settings = provenant_config::LaneBuildSettings::read(&self.writer_registry);
        let budget = resources.process_budget();
        let spacing = crate::ledger::spacing_left(
            &self.writer_registry,
            ticket.lane_class,
            ticket.startup_spacing_ms,
            call.clock,
        );
        let cap = self.lane_cap_hit(&ticket, call);
        let caps = self.lane_caps();
        let active: rusqlite::Result<i64> = self.db.query_row(
            "SELECT COUNT(*) FROM admission WHERE active=1 AND run=?1",
            [lane.run_id.to_string()],
            |row| row.get(0),
        );
        Ok(format!(
            "memory={memory:?}, floor={} basis points; disk={disk:?}, floor={floor:?} bytes; pressure={pressure:?}, threshold={:?}; process budget={budget:?}, reserved headroom={:?}; startup spacing remaining={spacing:?} ms (window={} ms); run active={active:?}, limit={}; writer cap={cap:?}, host limit={}, project limit={}",
            ticket.memory_floor_basis_points,
            settings.map(|settings| settings.memory_pressure_threshold()),
            budget.as_ref().ok().map(|budget| budget.headroom()),
            ticket.startup_spacing_ms,
            ticket.run_concurrency,
            caps.host_writers,
            caps.project_writers,
        ))
    }
    pub fn lane_waits_with_resources(
        &self,
        lanes: &[AttemptRecord],
        call: &Request<'_>,
        resources: &dyn Resources,
    ) -> Vec<LaneWait> {
        let pressure_threshold = provenant_config::LaneBuildSettings::read(&self.writer_registry)
            .map(|settings| settings.memory_pressure_threshold())
            .unwrap_or_default();
        // One observational view shares a host census across its rows. Actual
        // admission keeps taking fresh resource samples at its own boundary.
        let process_budget = std::cell::OnceCell::new();
        let mut stalled_cache = std::collections::HashMap::new();
        let slot_queue = self.slot_queue(lanes, call);
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
                let stalled = !matches!(lane.state, AttemptState::Queued { .. })
                    && *stalled_cache.entry(lane.attempt_id).or_insert_with(|| {
                        self.attempt_dir(lane.run_id, lane.attempt_id, false)
                            .is_ok_and(|attempt| {
                                self.supervisor_stalled(&attempt, Self::scope(lane))
                            })
                    });
                let waiting_for = if stalled {
                    "host_stalled: supervisor is in uninterruptible sleep".to_owned()
                } else if matches!(lane.state, AttemptState::Running { .. }) {
                    match &lane.state {
                        AttemptState::Running { extra } => extra.get("child_wait")
                            .and_then(|wait| wait.as_str())
                            .map(|wait| if wait == "children" {
                                "children: host and project admission yielded".to_owned()
                            } else {
                                "admission: reclaiming host and project capacity or startup spacing".to_owned()
                            }),
                        _ => None,
                    }
                        .or_else(|| self.disk_pause_wait(lane, resources))
                        .or_else(|| self.build_slot_wait(lane, call, &slot_queue))
                        .or_else(|| {
                            lane.provider_silent_for_ms.0
                                .map(provenant_records::run::provider_silence_reason)
                        })?
                } else if matches!(lane.state, AttemptState::Queued { .. }) {
                    self.recovery_process_wait(lane.run_id, call)
                        .unwrap_or_else(|| match process_budget.get_or_init(|| resources.process_budget()) {
                            Ok(budget) if !budget.has_room() => format!(
                                "host capacity: user process count {} of {}",
                                budget.used, budget.limit
                            ),
                            Err(error) => format!(
                                "launch: not yet handed to a guardian; process budget probe failed, not holding the lane: {}",
                                probe_error(error)
                            ),
                            _ => "launch: not yet handed to a guardian".to_owned(),
                        })
                } else if matches!(lane.state, AttemptState::Starting { .. })
                    && self.attempt_dir(lane.run_id, lane.attempt_id, false).is_ok_and(|attempt| {
                        matches!(
                            attempt.try_lock_existing(
                                std::ffi::OsStr::new(crate::writer::SEED_WAIT_LOCK),
                                provenant_os::fs::LockMode::Exclusive,
                            ),
                            Err(provenant_os::fs::FsError::LockTimeout { .. })
                        )
                    })
                {
                    "launch preparation: seeding the Cargo cache".to_owned()
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
                    } else if let Some(hit) = self.lane_cap_hit(&ticket, call) {
                        format!(
                            "host capacity: {}: {} of {} active",
                            hit.kind.description(),
                            hit.active,
                            hit.limit
                        )
                    } else if let Some(reason) = self.admission_disk_wait(lane, class, resources) {
                        reason
                    } else {
                        match resources.pressure() {
                            Ok(Some(pressure)) if pressure_threshold.defers(pressure) => {
                                format!("host capacity: memory pressure {pressure:?}")
                            }
                            pressure => {
                                let budget = process_budget.get_or_init(|| resources.process_budget());
                                let pressure_error = pressure.as_ref().err();
                                let budget_error = budget.as_ref().err();
                                let full = budget.as_ref().ok().filter(|budget| !budget.has_room());
                                if let Some(budget) = full {
                                    let mut reason = format!(
                                        "host capacity: user process count {} of {}",
                                        budget.used, budget.limit
                                    );
                                    if let Some(error) = pressure_error {
                                        reason.push_str(
                                            "; memory pressure probe failed, not holding the lane: ",
                                        );
                                        reason.push_str(&probe_error(error));
                                    }
                                    reason
                                } else if let (Some(pressure), Some(budget)) =
                                    (pressure_error, budget_error)
                                {
                                    format!(
                                        "host capacity: probes failed, not holding the lane: memory pressure: {}; process budget: {}",
                                        probe_error(pressure),
                                        probe_error(budget)
                                    )
                                } else if let Some(error) = pressure_error {
                                    format!(
                                        "host capacity: memory pressure probe failed, not holding the lane: {}",
                                        probe_error(error)
                                    )
                                } else if let Some(error) = budget_error {
                                    format!(
                                        "host capacity: process budget probe failed, not holding the lane: {}",
                                        probe_error(error)
                                    )
                                } else {
                                    "launch preparation: preparing the lane"
                                        .to_owned()
                                }
                            }
                        }
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
    /// fixed waits it names, that it is fresh and, for a slot wait, plausible
    /// slot counts and a start time.
    fn wait_marker(
        &self,
        lane: &AttemptRecord,
        call: &Request<'_>,
    ) -> Option<(WaitKind, Option<SlotWait>)> {
        use provenant_os::build_slots::WAIT_MARKER;
        let attempt = self.attempt_dir(lane.run_id, lane.attempt_id, false).ok()?;
        match attempt.metadata_at("process-exit.json".as_ref(), false) {
            Err(error) if error.is_not_found() => {}
            _ => return None,
        }
        let temporary = attempt.open_directory("tmp".as_ref()).ok()?;
        let now = u64::try_from(call.clock.wall_now().ok()?.as_micros() / 1_000_000).ok()?;
        // The marker sits in the lane's scratch (`tmp/candidate-N`), one file
        // per waiting command. Bound how much a lane can make the host scan.
        temporary
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
                        let text = std::str::from_utf8(&raw).ok()?;
                        Some((
                            WaitKind::from_marker(text, now)?,
                            WaitKind::slot_wait(text, now),
                        ))
                    })
            })
    }
    /// The running lanes waiting for a build slot, oldest wait first, with the
    /// slot counts they last saw. Only lanes of this project are visible; the
    /// slots are shared by every project on the host.
    fn slot_queue(
        &self,
        lanes: &[AttemptRecord],
        call: &Request<'_>,
    ) -> Vec<(AttemptId, SlotWait)> {
        let mut queue: Vec<_> = lanes
            .iter()
            .filter(|lane| matches!(lane.state, AttemptState::Running { .. }))
            .filter_map(|lane| match self.wait_marker(lane, call)? {
                (WaitKind::Slot, Some(wait)) => Some((lane.attempt_id, wait)),
                _ => None,
            })
            .collect();
        queue.sort_by_key(|(attempt, wait)| (wait.since, attempt.to_string()));
        queue
    }
    fn build_slot_wait(
        &self,
        lane: &AttemptRecord,
        call: &Request<'_>,
        queue: &[(AttemptId, SlotWait)],
    ) -> Option<String> {
        let (kind, _) = self.wait_marker(lane, call)?;
        match kind {
            WaitKind::Slot => Some(
                match queue.iter().position(|(id, _)| *id == lane.attempt_id) {
                    Some(index) => {
                        let wait = queue[index].1;
                        format!(
                            "build slot: waiting {} of {} (longest first); {} of {} slots held",
                            index + 1,
                            queue.len(),
                            wait.held,
                            wait.limit
                        )
                    }
                    None => "build slot: a cargo build waits for one of the host's build slots"
                        .to_owned(),
                },
            ),
            WaitKind::CargoLock => Some(
                "build slot: a cargo build waits for another build's lock in its target directory"
                    .to_owned(),
            ),
        }
    }
    /// Only writers wait on the live disk floor; readers warn in `admit`.
    fn admission_disk_wait(
        &self,
        lane: &AttemptRecord,
        class: LaneClass,
        resources: &dyn Resources,
    ) -> Option<String> {
        if class != LaneClass::Writer {
            return None;
        }
        let attempt = self.attempt_dir(lane.run_id, lane.attempt_id, false).ok()?;
        // The guardian samples sealed state, cwd and cache volumes. A listing
        // cannot reconstruct those private launch inputs from the attempt path.
        let observed =
            crate::artifacts::read_sealed_observed(&attempt, "admission-disk.json", 1024)
                .ok()
                .flatten()
                .and_then(|raw| serde_json::from_slice::<AdmissionDiskSample>(&raw).ok())
                .filter(|sample| sample.generation == lane.generation);
        let free = observed
            .map(|sample| sample.free)
            .or_else(|| resources.disk(&attempt).ok())?;
        let (floor, origin) = disk_floor(&self.writer_registry).ok()?;
        (free < floor).then(|| format!(
            "host capacity: disk space: {free} bytes available; gc.free_floor is {floor} bytes (source: {})",
            origin.description()
        ))
    }

    /// The running guardian publishes this lane's pause in its checkpoint.
    fn disk_pause_wait(&self, lane: &AttemptRecord, resources: &dyn Resources) -> Option<String> {
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
            resources.observe_process(guardian.pid),
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
    /// Advance an integration fixture through the real launch transition without
    /// spawning a guardian. Custody fixtures require a nonzero generation.
    #[cfg(feature = "fixture")]
    #[doc(hidden)]
    pub fn mark_starting_for_fixture(
        &self,
        id: AttemptId,
        route: &RouteRecord,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        self.mark_starting_with_priority(
            &self.record(id, call)?,
            route,
            &[],
            &AppliedWriteGrants::default(),
            None,
            None,
            0,
            call,
        )
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
        if !crate::reader::valid_write_grants(&write_grants.grants) {
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
            let class = Service::lane_class(
                ticket.lane_class == LaneClass::Writer,
                &[],
                &write_grants.grants,
            );
            if class != ticket.lane_class {
                // Preparation may reveal tool grants or project exceptions
                // absent from the submitted task. Promote before any claim.
                ticket.lane_class = class;
                ticket.startup_spacing_ms =
                    provenant_config::LaneBuildSettings::read(&self.writer_registry)
                        .unwrap_or_default()
                        .start_spacing_ms
                        .unwrap_or_else(|| self.policy.spacing_ms(class));
            }
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
        let mut warned_memory_floor = false;
        let mut warned_disk_floor = false;
        let mut last_disk_sample = None;
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
            let disk_floor_bytes = disk_floor(&self.writer_registry);
            let pressure = resources.pressure().unwrap_or_else(|error| {
                eprintln!(
                    "admission: memory pressure probe failed: {}",
                    probe_error(&error)
                );
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
                .inspect_err(|error| {
                    eprintln!(
                        "admission: process budget probe failed: {}",
                        probe_error(error)
                    );
                })
                .ok()
                .filter(|budget| !budget.has_room());
            if !warned_memory_floor
                && let Some(warning) =
                    memory_floor_warning(resources.memory(), ticket.memory_floor_basis_points)
            {
                eprintln!(
                    "admission warning [{}]: {}; next: {}",
                    warning.code,
                    warning
                        .detail
                        .as_deref()
                        .unwrap_or("memory availability is low"),
                    warning.next.as_deref().unwrap_or("inspect host memory")
                );
                warned_memory_floor = true;
            }
            let fits = match (
                pressure,
                resources.disk(attempt),
                disk_floor_bytes,
                settings,
            ) {
                (pressure, Ok(disk), Ok((floor, origin)), Ok(settings)) => {
                    let threshold = settings.memory_pressure_threshold();
                    let pressure = pressure.filter(|level| threshold.defers(*level));
                    let mut disk = disk;
                    let writer = ticket.lane_class == LaneClass::Writer;
                    if !writer
                        && let Some(warning) =
                            read_only_disk_warning(&mut warned_disk_floor, disk, floor, origin)
                    {
                        eprintln!(
                            "admission warning [{}]: {}; next: {}",
                            warning.code,
                            warning.detail.as_deref().unwrap_or("disk space is low"),
                            warning.next.as_deref().unwrap_or("free disk space")
                        );
                    }
                    if writer && disk < floor {
                        if below_floor.is_multiple_of(RECLAIM_EVERY)
                            && resources.reclaim_disk(attempt)
                        {
                            disk = resources.disk(attempt).unwrap_or(disk);
                        }
                        below_floor = below_floor.saturating_add(1);
                    }
                    if writer {
                        let sample = AdmissionDiskSample {
                            generation: ticket.generation,
                            free: disk,
                        };
                        if last_disk_sample != Some(sample) {
                            // Status evidence only: publication failure must not
                            // alter admission. No paths or private context escape.
                            if let Ok(raw) = serde_json::to_vec(&sample) {
                                let _ = attempt.atomic_write(
                                    "admission-disk.json".as_ref(),
                                    ".admission-disk-",
                                    &raw,
                                    provenant_os::fs::PublishMode::Replace,
                                );
                            }
                            last_disk_sample = Some(sample);
                        }
                    }
                    refusal = if writer && disk < floor {
                        Some(RunError::Admission(AdmissionError::Disk {
                            free: disk,
                            floor,
                            origin,
                        }))
                    } else if let Some(level) = pressure {
                        Some(RunError::Admission(AdmissionError::MemoryPressure {
                            level: match level {
                                resources::MemoryPressure::Normal => {
                                    provenant_records::diagnostic::PressureLevel::Normal
                                }
                                resources::MemoryPressure::Warning => {
                                    provenant_records::diagnostic::PressureLevel::Warning
                                }
                                resources::MemoryPressure::Critical => {
                                    provenant_records::diagnostic::PressureLevel::Critical
                                }
                            },
                            threshold: threshold.diagnostic_value(),
                            origin: settings.memory_pressure_source(),
                        }))
                    } else {
                        processes.map(|budget| {
                            RunError::Admission(AdmissionError::Processes {
                                used: budget.used,
                                limit: budget.limit,
                            })
                        })
                    };
                    pressure.is_none() && processes.is_none() && (!writer || disk >= floor)
                }
                _ => {
                    refusal = None;
                    false
                }
            };
            if fits {
                match self.claim_observed(id, clock, &call, resources) {
                    Ok(Some(record)) => return Ok(record),
                    // A busy store is transient; keep waiting to the deadline.
                    Ok(None) => {
                        // Name the applicable total or writer cap for either lane class.
                        if let Some(hit) = self.lane_cap_hit(&ticket, &call) {
                            refusal = Some(RunError::Admission(AdmissionError::LaneCap {
                                active: hit.active,
                                limit: hit.limit,
                                kind: hit.kind,
                                origin: hit.origin,
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
        self.claim_observed(id, clock, call, &SystemResources)
    }
    fn lane_caps(&self) -> crate::ledger::LaneCaps {
        crate::ledger::LaneCaps::of(
            &provenant_config::LaneBuildSettings::read(&self.writer_registry).unwrap_or_default(),
        )
    }
    fn lane_cap_hit(
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
    pub(crate) fn claim_observed(
        &self,
        id: AttemptId,
        clock: &dyn Clock,
        call: &Request<'_>,
        resources: &dyn Resources,
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
        let owner = crate::ledger::prepare_lease(
            &ledger,
            self.context.state_root.as_path(),
            &self.ticket(&self.db, id)?,
            caps,
            clock,
            resources,
        )?;
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
                &owner,
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
/// A failed resource probe as one bounded line. Control and Unicode format
/// characters (including bidi controls) become spaces, runs of whitespace
/// collapse, and the text is capped as it is written, so the reason stays
/// within the bound however long the probe's error is.
fn probe_error(error: &std::io::Error) -> String {
    use std::fmt::Write as _;

    const LIMIT: usize = 200;
    struct Bounded {
        text: String,
        pending_space: bool,
        truncated: bool,
    }
    impl std::fmt::Write for Bounded {
        fn write_str(&mut self, input: &str) -> std::fmt::Result {
            for character in input.chars() {
                if self.truncated {
                    return Ok(());
                }
                let character = if character.is_control() || is_format_character(character) {
                    ' '
                } else {
                    character
                };
                if character.is_whitespace() {
                    self.pending_space = !self.text.is_empty();
                    continue;
                }
                let separator = usize::from(self.pending_space && !self.text.is_empty());
                if self.text.len() + separator + character.len_utf8() > LIMIT {
                    self.truncated = true;
                    return Ok(());
                }
                if separator == 1 {
                    self.text.push(' ');
                }
                self.pending_space = false;
                self.text.push(character);
            }
            Ok(())
        }
    }
    let mut bounded = Bounded {
        text: String::with_capacity(LIMIT + 3),
        pending_space: false,
        truncated: false,
    };
    let _ = write!(bounded, "{error}");
    if bounded.text.is_empty() {
        bounded.text.push_str("unknown error");
    }
    if bounded.truncated {
        bounded.text.push_str("...");
    }
    bounded.text
}

/// True for Unicode `Cf` (format) characters, including bidi controls and
/// zero-width characters, which can reorder or hide text in a terminal.
fn is_format_character(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
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

/// Bounded status evidence from the guardian's sealed multi-volume probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AdmissionDiskSample {
    generation: u64,
    free: u64,
}

/// Resolve the live host disk floor and its source for admission and status.
fn disk_floor(
    registry: &std::path::Path,
) -> std::io::Result<(u64, provenant_records::diagnostic::SettingSource)> {
    use provenant_records::diagnostic::SettingSource;
    let settings = provenant_config::GcSettings::read(registry)?;
    let floor = settings.free_floor_bytes(registry)?;
    let origin = if settings.free_floor.is_some() {
        SettingSource::Configuration
    } else {
        SettingSource::ShippedDefault
    };
    Ok((floor, origin))
}

/// A read-only lane writes little enough to proceed, warning once per admission.
fn read_only_disk_warning(
    warned: &mut bool,
    free: u64,
    floor: u64,
    origin: provenant_records::diagnostic::SettingSource,
) -> Option<provenant_records::diagnostic::Diagnostic> {
    if *warned || free >= floor {
        return None;
    }
    *warned = true;
    Some(
        ErrorKind::DiskFloor.diagnostic().category("warning")
            .detail(format!(
                "disk space: {free} bytes available; gc.free_floor is {floor} bytes (source: {}); read-only lane is proceeding",
                origin.description()
            ))
            .next("free disk space or run `provenant gc --apply`"),
    )
}

/// The shipped memory floor has no host setting, so it can only warn.
fn memory_floor_warning(
    sample: std::io::Result<MemorySample>,
    floor_basis_points: u64,
) -> Option<provenant_records::diagnostic::Diagnostic> {
    let detail = match sample {
        Ok(sample) if sample.total_bytes > 0 => {
            if u128::from(sample.available_bytes) * 10_000
                >= u128::from(sample.total_bytes) * u128::from(floor_basis_points)
            {
                return None;
            }
            format!(
                "available memory is {} of {} bytes, below the shipped advisory floor of {floor_basis_points} basis points",
                sample.available_bytes, sample.total_bytes
            )
        }
        _ => "host memory availability could not be observed for the shipped advisory floor".into(),
    };
    Some(
        ErrorKind::MemoryFloor
            .diagnostic()
            .detail(detail)
            .next("reduce host memory use or inspect host memory before continuing"),
    )
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
            crate::db::attempt_bytes(next)?,
            next.attempt_id.to_string(),
            sql_int(old.revision)?
        ],
    )?;
    if changed != 1 {
        return Err(RunError::Conflict);
    }
    insert_event(tx, next)
}

#[cfg(test)]
mod process_wait_tests {
    use super::*;

    #[test]
    fn memory_floor_warning_has_safe_code_detail_and_action() {
        let warning = memory_floor_warning(
            Ok(MemorySample {
                available_bytes: 1,
                total_bytes: 100,
            }),
            500,
        )
        .unwrap();
        assert_eq!(warning.code, "MEMORY_FLOOR");
        assert_eq!(warning.category.as_deref(), Some("warning"));
        assert!(warning.detail.unwrap().contains("500 basis points"));
        assert!(warning.next.is_some());
        assert!(
            memory_floor_warning(
                Ok(MemorySample {
                    available_bytes: 5,
                    total_bytes: 100
                }),
                500
            )
            .is_none()
        );
        let warning =
            memory_floor_warning(Err(std::io::Error::other("TOKEN_SENTINEL")), 500).unwrap();
        assert!(
            !serde_json::to_string(&warning)
                .unwrap()
                .contains("TOKEN_SENTINEL")
        );
    }

    #[test]
    fn read_only_disk_floor_warns_once_with_setting_and_source() {
        use provenant_records::diagnostic::SettingSource;
        let mut warned = false;
        assert!(
            read_only_disk_warning(&mut warned, 100, 100, SettingSource::Configuration).is_none()
        );
        assert!(!warned);
        let warning =
            read_only_disk_warning(&mut warned, 99, 100, SettingSource::Configuration).unwrap();
        assert_eq!(warning.code, "DISK_FLOOR");
        assert_eq!(warning.category.as_deref(), Some("warning"));
        assert_eq!(
            warning.detail.as_deref(),
            Some(
                "disk space: 99 bytes available; gc.free_floor is 100 bytes (source: host configuration); read-only lane is proceeding"
            )
        );
        assert!(warning.next.is_some());
        assert!(
            read_only_disk_warning(&mut warned, 0, 100, SettingSource::Configuration).is_none()
        );
    }

    struct LowMemory;
    impl Resources for LowMemory {
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            Ok(MemorySample {
                available_bytes: 1,
                total_bytes: 100,
            })
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(u64::MAX)
        }
    }

    #[test]
    fn admission_observations_name_memory_sample_and_floor()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let lane = store.record(run.attempts[0].attempt_id, &call)?;
        let message = store.admission_observations(&lane, &call, &LowMemory)?;
        assert!(
            message.contains("memory=Ok(MemorySample { available_bytes: 1, total_bytes: 100 })"),
            "{message}"
        );
        assert!(message.contains("floor=500 basis points"), "{message}");
        Ok(())
    }

    struct Full;
    impl Resources for Full {
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            crate::tests::HostFacts::new(100).guardian_identity()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            Ok(MemorySample {
                available_bytes: 100,
                total_bytes: 100,
            })
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(u64::MAX)
        }
        fn process_budget(&self) -> std::io::Result<resources::ProcessBudget> {
            Ok(resources::ProcessBudget {
                used: 950,
                limit: 1000,
            })
        }
    }

    #[test]
    fn queued_lane_reports_process_capacity() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let lane = store.record(run.attempts[0].attempt_id, &call)?;
        let waits = store.lane_waits_with_resources(&[lane], &call, &Full);
        assert_eq!(
            waits[0].waiting_for,
            "host capacity: user process count 950 of 1000"
        );
        Ok(())
    }

    /// The process budget a probe source reports.
    #[derive(Clone, Copy)]
    enum Budget {
        Unlimited,
        Full { used: u64, limit: u64 },
        Failed(&'static str),
    }
    /// A probe source whose results are the cases under test.
    struct Probes {
        pressure: Option<&'static str>,
        budget: Budget,
    }
    impl Resources for Probes {
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            crate::tests::HostFacts::new(100).guardian_identity()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            Ok(MemorySample {
                available_bytes: 100,
                total_bytes: 100,
            })
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(u64::MAX)
        }
        fn pressure(&self) -> std::io::Result<Option<resources::MemoryPressure>> {
            match self.pressure {
                Some(message) => Err(std::io::Error::other(message)),
                None => Ok(None),
            }
        }
        fn process_budget(&self) -> std::io::Result<resources::ProcessBudget> {
            match self.budget {
                Budget::Unlimited => Ok(resources::ProcessBudget::UNLIMITED),
                Budget::Full { used, limit } => Ok(resources::ProcessBudget { used, limit }),
                Budget::Failed(message) => Err(std::io::Error::other(message)),
            }
        }
    }

    fn starting_lane(
        store: &RunStore,
        call: &Request<'_>,
    ) -> std::result::Result<AttemptRecord, Box<dyn std::error::Error>> {
        let run = crate::tests::input();
        store.create(&run, call)?;
        let queued = store.record(run.attempts[0].attempt_id, call)?;
        Ok(store.mark_starting(
            &queued,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            call,
        )?)
    }

    #[test]
    fn lane_wait_view_samples_process_budget_once_and_refreshes_the_next_view()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use std::cell::Cell;
        struct Counting {
            budget: Cell<Budget>,
            calls: Cell<usize>,
        }
        impl Resources for Counting {
            fn observe_process(
                &self,
                pid: i64,
            ) -> provenant_supervisor::census::ProcessObservation {
                crate::tests::HostFacts::new(100).observe_process(pid)
            }

            fn memory(&self) -> std::io::Result<MemorySample> {
                Ok(MemorySample {
                    available_bytes: 100,
                    total_bytes: 100,
                })
            }
            fn disk(&self, _: &Directory) -> std::io::Result<u64> {
                Ok(u64::MAX)
            }
            fn process_budget(&self) -> std::io::Result<resources::ProcessBudget> {
                self.calls.set(self.calls.get() + 1);
                Probes {
                    pressure: None,
                    budget: self.budget.get(),
                }
                .process_budget()
            }
        }
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let mut lanes = Vec::new();
        for n in 1..=4u8 {
            let mut run = crate::tests::input();
            run.run_id = provenant_core::RunId::from_bytes([n; 16]);
            run.operation_id = provenant_core::OperationId::parse(&format!("view-census-{n}"))?;
            run.attempts[0].attempt_id = provenant_core::AttemptId::from_bytes([n; 16]);
            store.create(&run, &call)?;
            let queued = store.record(run.attempts[0].attempt_id, &call)?;
            lanes.push(if n % 2 == 0 {
                store.mark_starting(
                    &queued,
                    &crate::tests::route(),
                    &[],
                    &Default::default(),
                    None,
                    &call,
                )?
            } else {
                queued
            });
        }
        let resources = Counting {
            budget: Cell::new(Budget::Unlimited),
            calls: Cell::new(0),
        };
        for (view, budget) in [
            Budget::Full {
                used: 950,
                limit: 1000,
            },
            Budget::Failed("unavailable"),
            Budget::Unlimited,
        ]
        .into_iter()
        .enumerate()
        {
            resources.budget.set(budget);
            let waits = store.lane_waits_with_resources(&lanes, &call, &resources);
            assert_eq!(waits.len(), 4);
            assert_eq!(
                resources.calls.get(),
                view + 1,
                "every view samples once, including failed censuses"
            );
            for (lane, wait) in lanes.iter().zip(waits) {
                assert_eq!(wait.attempt_id, lane.attempt_id);
                let queued = matches!(lane.state, AttemptState::Queued { .. });
                let expected = match (budget, queued) {
                    (Budget::Full { .. }, _) => "host capacity: user process count 950 of 1000",
                    (Budget::Failed(_), true) => {
                        "launch: not yet handed to a guardian; process budget probe failed, not holding the lane: unavailable"
                    }
                    (Budget::Failed(_), false) => {
                        "host capacity: process budget probe failed, not holding the lane: unavailable"
                    }
                    (Budget::Unlimited, true) => "launch: not yet handed to a guardian",
                    (Budget::Unlimited, false) => "launch preparation: preparing the lane",
                };
                assert_eq!(wait.waiting_for, expected);
            }
        }
        Ok(())
    }

    #[test]
    fn admission_starting_lane_reports_launch_preparation_without_a_limit()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let lane = starting_lane(&store, &call)?;
        let waits = store.lane_waits_with_resources(
            &[lane],
            &call,
            &Probes {
                pressure: None,
                budget: Budget::Unlimited,
            },
        );
        assert_eq!(
            waits[0].waiting_for,
            "launch preparation: preparing the lane"
        );
        Ok(())
    }

    #[test]
    fn queued_lane_reports_failed_process_budget_probe()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let lane = store.record(run.attempts[0].attempt_id, &call)?;
        let waits = store.lane_waits_with_resources(
            &[lane],
            &call,
            &Probes {
                pressure: None,
                budget: Budget::Failed("unavailable"),
            },
        );
        assert_eq!(
            waits[0].waiting_for,
            "launch: not yet handed to a guardian; process budget probe failed, \
             not holding the lane: unavailable"
        );
        Ok(())
    }

    #[test]
    fn starting_lane_reports_failed_process_budget_probe()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let lane = starting_lane(&store, &call)?;
        let waits = store.lane_waits_with_resources(
            &[lane],
            &call,
            &Probes {
                pressure: None,
                budget: Budget::Failed("unavailable"),
            },
        );
        assert_eq!(
            waits[0].waiting_for,
            "host capacity: process budget probe failed, not holding the lane: unavailable"
        );
        Ok(())
    }

    #[test]
    fn starting_lane_reports_failed_memory_pressure_probe()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let lane = starting_lane(&store, &call)?;
        let waits = store.lane_waits_with_resources(
            &[lane],
            &call,
            &Probes {
                pressure: Some("unavailable"),
                budget: Budget::Unlimited,
            },
        );
        assert_eq!(
            waits[0].waiting_for,
            "host capacity: memory pressure probe failed, not holding the lane: unavailable"
        );
        Ok(())
    }

    #[test]
    fn starting_lane_names_full_process_budget_when_pressure_probe_fails()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let lane = starting_lane(&store, &call)?;
        let waits = store.lane_waits_with_resources(
            &[lane],
            &call,
            &Probes {
                pressure: Some("unavailable"),
                budget: Budget::Full {
                    used: 512,
                    limit: 512,
                },
            },
        );
        assert_eq!(
            waits[0].waiting_for,
            "host capacity: user process count 512 of 512; memory pressure probe failed, \
             not holding the lane: unavailable"
        );
        Ok(())
    }

    #[test]
    fn starting_lane_names_both_failed_probes()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let lane = starting_lane(&store, &call)?;
        let waits = store.lane_waits_with_resources(
            &[lane],
            &call,
            &Probes {
                pressure: Some("unreadable"),
                budget: Budget::Failed("exhausted"),
            },
        );
        assert_eq!(
            waits[0].waiting_for,
            "host capacity: probes failed, not holding the lane: memory pressure: unreadable; \
             process budget: exhausted"
        );
        Ok(())
    }

    #[test]
    fn probe_failure_reason_is_bounded_and_single_line() {
        let error = std::io::Error::other(format!("first\nsecond {}", "x".repeat(500)));
        let text = probe_error(&error);
        assert!(!text.contains('\n'));
        assert!(text.ends_with("..."));
        assert!(text.len() <= 203);
    }

    #[test]
    fn probe_failure_reason_strips_format_characters() {
        let error = std::io::Error::other("a\u{202E}b\u{200B}c\u{FEFF}d");
        assert_eq!(probe_error(&error), "a b c d");
    }

    #[test]
    fn probe_error_cuts_multibyte_text_on_a_character_boundary() {
        // Cap at 200 bytes, so a 3-byte or 4-byte character that would cross
        // the cap is dropped whole and the ellipsis is appended.
        for unit in ["€", "😀"] {
            for prefix in [199, 200, 201] {
                let error =
                    std::io::Error::other(format!("{}{}", "a".repeat(prefix), unit.repeat(8)));
                let text = probe_error(&error);
                assert_eq!(text, format!("{}{}", "a".repeat(prefix.min(200)), "..."));
            }
        }
    }

    #[test]
    fn admission_timeout_resolves_with_origin_precedence() {
        assert_eq!(
            AdmissionPolicy::resolve_timeout(Some(120), Some(300)),
            EffectiveAdmissionTimeout {
                seconds: 120,
                origin: AdmissionTimeoutOrigin::Flag,
            }
        );
        assert_eq!(
            AdmissionPolicy::resolve_timeout(None, Some(300)),
            EffectiveAdmissionTimeout {
                seconds: 300,
                origin: AdmissionTimeoutOrigin::HostConfig,
            }
        );
        assert_eq!(
            AdmissionPolicy::resolve_timeout(None, None),
            EffectiveAdmissionTimeout {
                seconds: AdmissionPolicy::default().admission_seconds,
                origin: AdmissionTimeoutOrigin::ShippedDefault,
            }
        );
    }

    #[test]
    fn admission_window_deadline_is_preserved_on_restart()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let op = OperationId::parse("admission-op-1")?;

        let initial_deadline = WallTime::from_micros(1_000_000)?;
        let window = AdmissionWindow {
            deadline: initial_deadline,
            wake_at: None,
        };
        let saved = store.save_admission_window(&op, window, &call)?;
        assert_eq!(saved.deadline, initial_deadline);
        assert_eq!(saved.wake_at, None);

        // Reprobe / retry: update wake_at with a later deadline
        let later_deadline = WallTime::from_micros(5_000_000)?;
        let wake = WallTime::from_micros(1_500_000)?;
        let updated = store.save_admission_window(
            &op,
            AdmissionWindow {
                deadline: later_deadline,
                wake_at: Some(wake),
            },
            &call,
        )?;

        // The original deadline must be preserved, never renewed.
        assert_eq!(updated.deadline, initial_deadline);
        assert_eq!(updated.wake_at, Some(wake));

        // Read back directly
        let read = store
            .admission_window(&op, &call)?
            .expect("window must exist");
        assert_eq!(read.deadline, initial_deadline);
        assert_eq!(read.wake_at, Some(wake));
        Ok(())
    }
}
