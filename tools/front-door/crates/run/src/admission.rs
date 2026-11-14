//! Single-pass host admission. Resource policy is advisory by default;
//! custody and durable store claims retain their integrity boundaries.
use crate::{
    db::{bytes, decode, sql_int},
    store::insert_event,
    *,
};
use provenant_confinement::valid_read_only_writable;
use provenant_core::{AttemptId, CancelToken, Clock, DurationMicros, GitOid, WallTime};
use provenant_os::{
    fs::Directory,
    sys::resources::{self, MemorySample},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Write evidence from the prepared confinement plan, never launch authority.
#[derive(Default)]
pub(crate) struct AppliedWriteGrants {
    pub grants: Vec<WriteGrantRecord>,
    pub git_grants_digest: Option<provenant_core::Digest>,
}

/// Upper bounds for explicit lane-count settings. Writer caps are configurable below these
/// (`domains.fabric.lanes.max_writer_lanes` and
/// `max_project_writer_lanes`, both disabled by default).
pub const HOST_ACTIVE: u64 = provenant_config::WRITER_LANES_CEILING;
pub const PROJECT_ACTIVE: u64 = provenant_config::PROJECT_WRITER_LANES_CEILING;
pub const HOST_QUEUED: u64 = 256;

/// Floors and windows recorded on each ticket at submission.
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
}

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
        let request = self.request(id, call)?;
        let policy = crate::launch_policy(
            &self.writer_registry,
            &self.context.project_id.to_string(),
            &request.task,
        );
        let caps = self.lane_caps().under_policy(&policy);
        let owner = if waiting {
            None
        } else {
            Some(crate::ledger::prepare_lease(
                &ledger,
                self.context.state_root.as_path(),
                &self.ticket(&self.db, id)?,
                caps,
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
                let reclaimed = crate::ledger::lease_with_policy(
                    &ledger,
                    self.context.state_root.as_path(),
                    &ticket,
                    call.clock,
                    call,
                    owner.as_ref().ok_or(RunError::Conflict)?,
                    &policy,
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
    pub(crate) fn queue_ticket(
        &self,
        tx: &Connection,
        attempt: &AttemptRecord,
        class: LaneClass,
        runtime_seconds: u64,
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
            quota_permit: None,
            admission_deadline: deadline(attempt.queued_at, self.policy.admission_seconds)?,
            runtime_seconds,
        };
        tx.execute(
            "INSERT INTO admission(attempt,project,active,record) VALUES (?1,?2,0,?3)",
            params![
                attempt.attempt_id.to_string(),
                attempt.project_id.to_string(),
                bytes(&ticket)?
            ],
        )?;
        Ok(())
    }
    /// What each queued or starting lane in `lanes` is waiting for, in order.
    /// It reads this store and the host ledger without taking a lock, so it
    /// names a wait only when it can see one: startup spacing, then the
    /// shared host and project caps and resource floors.
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
        let attempt = self.attempt_dir(lane.attempt_id, false)?;
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
        Ok(format!(
            "memory={memory:?}, floor={} basis points; disk={disk:?}, floor={floor:?} bytes; pressure={pressure:?}, threshold={:?}; process budget={budget:?}, reserved headroom={:?}; startup spacing remaining={spacing:?} ms (window={} ms); writer cap={cap:?}, host limit={}, project limit={}",
            ticket.memory_floor_basis_points,
            settings.map(|settings| settings.memory_pressure_threshold()),
            budget.as_ref().ok().map(|budget| budget.headroom()),
            ticket.startup_spacing_ms,
            caps.host_writers,
            caps.project_writers,
        ))
    }
    pub fn lane_waits_with_resources(
        &self,
        lanes: &[AttemptRecord],
        call: &Request<'_>,
        _resources: &dyn Resources,
    ) -> Vec<LaneWait> {
        lanes
            .iter()
            .filter_map(|lane| {
                if !matches!(
                    lane.state,
                    AttemptState::Queued { .. }
                        | AttemptState::Starting { .. }
                        | AttemptState::Running { .. }
                ) {
                    return None;
                }
                let waiting_for = if !matches!(lane.state, AttemptState::Queued { .. })
                    && self
                        .attempt_dir(lane.attempt_id, false)
                        .is_ok_and(|attempt| {
                            self.supervisor_stalled(&attempt, Self::scope(lane), call)
                        }) {
                    "host_stalled: supervisor is in uninterruptible sleep".to_owned()
                } else if let AttemptState::Running { extra } = &lane.state {
                    if extra.get("child_wait").and_then(|v| v.as_str()) == Some("children") {
                        "children: host and project admission yielded".to_owned()
                    } else if let Some(ms) = lane.provider_silent_for_ms.0 {
                        provenant_records::run::provider_silence_reason(ms)
                    } else {
                        return None;
                    }
                } else {
                    self.recovery_process_wait(lane.attempt_id, call)
                        .unwrap_or_else(|| "launch preparation".to_owned())
                };
                Some(LaneWait {
                    extra: Default::default(),
                    attempt_id: lane.attempt_id,
                    lane_class: self.ticket(&self.db, lane.attempt_id).ok()?.lane_class,
                    waiting_for,
                })
            })
            .collect()
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
            &Default::default(),
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
            &Default::default(),
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
        limits: &provenant_records::LaneLimits,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        let id = expected.attempt_id;
        let runtime_seconds = limits
            .deadline_seconds
            .as_ref()
            .map_or(0, |limit| limit.value);
        if !valid_read_only_writable(writable) {
            return Err(RunError::Validation("invalid read-only writable paths"));
        }
        if !crate::reader::valid_write_grants(&write_grants.grants) {
            return Err(RunError::Validation("invalid write grants"));
        }
        let next = self.write(call, |tx| {
            let old = self.record(id, call)?;
            if !matches!(old.state, AttemptState::Queued { .. })
                || old.attempt_id != expected.attempt_id
                || old.key != expected.key
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
            next.limits = Some(limits.clone());
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
        self.contribution_diagnostic(next.attempt_id, self.retain_contribution_route(&next, call));
        Ok(next)
    }
    /// Sample advisory resources once, then claim custody without a resource poll loop.
    #[allow(clippy::print_stderr)]
    pub fn admit(
        &self,
        id: AttemptId,
        attempt: &Directory,
        resources: &dyn Resources,
        clock: &dyn Clock,
        cancel: &CancelToken,
    ) -> Result<AttemptRecord> {
        let call = Request { clock, cancel };
        call.check()?;
        if crate::artifacts::read_sealed(attempt, "cancel.json", RECORD_LIMIT)?.is_some() {
            return Err(RunError::Cancelled);
        }
        let ticket = self.ticket(&self.db, id)?;
        let request = self.request(id, &call)?;
        let policy = crate::launch_policy(
            &self.writer_registry,
            &self.context.project_id.to_string(),
            &request.task,
        );
        let settings = provenant_config::LaneBuildSettings::read(&self.writer_registry)
            .inspect_err(|error| {
                eprintln!(
                    "admission: settings unavailable: {}; proceeding with shipped policy",
                    probe_error(error)
                )
            })
            .unwrap_or_default();
        let pressure = resources
            .pressure()
            .inspect_err(|error| {
                eprintln!(
                    "admission: memory pressure probe failed: {}",
                    probe_error(error)
                )
            })
            .ok()
            .flatten()
            .filter(|level| settings.memory_pressure_threshold().defers(*level));
        let processes = resources
            .process_budget()
            .inspect_err(|error| {
                eprintln!(
                    "admission: process budget probe failed: {}",
                    probe_error(error)
                )
            })
            .ok()
            .filter(|budget| !budget.has_room());
        let disk = resources
            .disk(attempt)
            .inspect_err(|error| eprintln!("admission: disk probe failed: {}", probe_error(error)))
            .ok();
        let floor = disk_floor(&self.writer_registry)
            .inspect_err(|error| {
                eprintln!("admission: disk floor unavailable: {error}; proceeding")
            })
            .ok();
        if let Some(warning) =
            memory_floor_warning(resources.memory(), ticket.memory_floor_basis_points)
        {
            eprintln!(
                "admission warning [{}]: {}; next: {}",
                warning.code,
                warning
                    .detail
                    .as_deref()
                    .unwrap_or("available memory is low"),
                warning.next.as_deref().unwrap_or("inspect host memory")
            );
        }
        let spacing = crate::ledger::spacing_left(
            &self.writer_registry,
            ticket.lane_class,
            ticket.startup_spacing_ms,
            clock,
        )
        .filter(|left| *left > 0);
        let low_disk = disk.zip(floor).filter(|(free, (floor, _))| free < floor);
        if ticket.lane_class == LaneClass::ReadOnly
            && let Some((free, (floor, origin))) = low_disk
            && let Some(warning) = read_only_disk_warning(&mut false, free, floor, origin)
        {
            eprintln!(
                "admission warning [{}]: {}; next: {}",
                warning.code,
                warning
                    .detail
                    .as_deref()
                    .unwrap_or("free disk space is low"),
                warning.next.as_deref().unwrap_or("free disk space")
            );
        }
        let writer_disk = low_disk.filter(|_| ticket.lane_class == LaneClass::Writer);
        let observed = |error: RunError| (error.to_string(), error);
        // Each observation names its typed refusal; the policy decides its effect.
        let observations: [(&str, Option<(String, RunError)>); 4] = [
            (
                "ADMISSION_DISK",
                writer_disk.map(|(free, (floor, origin))| {
                    observed(RunError::Admission(AdmissionError::Disk {
                        free,
                        floor,
                        origin,
                    }))
                }),
            ),
            (
                "ADMISSION_PRESSURE",
                pressure.map(|level| {
                    use provenant_records::diagnostic::PressureLevel;
                    observed(RunError::Admission(AdmissionError::MemoryPressure {
                        level: match level {
                            resources::MemoryPressure::Normal => PressureLevel::Normal,
                            resources::MemoryPressure::Warning => PressureLevel::Warning,
                            resources::MemoryPressure::Critical => PressureLevel::Critical,
                        },
                        threshold: settings.memory_pressure_threshold().diagnostic_value(),
                        origin: settings.memory_pressure_source(),
                    }))
                }),
            ),
            (
                "PROCESS_CENSUS",
                processes.map(|budget| {
                    observed(RunError::Admission(AdmissionError::Processes {
                        used: budget.used,
                        limit: budget.limit,
                    }))
                }),
            ),
            (
                "ADMISSION_SPACING",
                spacing.map(|left| {
                    (
                        format!(
                            "a lane of this class started within domains.fabric.lanes.start_spacing_ms; {left} ms remain"
                        ),
                        RunError::Refused,
                    )
                }),
            ),
        ];
        for (code, observation) in observations {
            use provenant_records::policy::{Effect, Facts};
            let Some((detail, error)) = observation else {
                continue;
            };
            match policy.check(code, Facts::violation(detail)) {
                Effect::Proceed => {}
                Effect::Warn(message) => eprintln!("warning: {message}"),
                Effect::Refuse(_) => return Err(error),
            }
        }
        self.claim_observed(id, clock, &call, resources)?
            .ok_or(RunError::Refused)
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
        let ledger = crate::ledger::open(&self.writer_registry, call)?;
        let request = self.request(id, call)?;
        let policy = crate::launch_policy(
            &self.writer_registry,
            &self.context.project_id.to_string(),
            &request.task,
        );
        use provenant_records::diagnostic::LaneLimit;
        use provenant_records::policy::{Effect, Facts, Strength};
        let configured = self.lane_caps();
        let ticket = self.ticket(&self.db, id)?;
        // Prune dead leases before a hard cap can refuse this admission.
        // `prepare_lease` samples the owner and consults the injected process observer.
        let caps = configured.under_policy(&policy);
        let owner = crate::ledger::prepare_lease(
            &ledger,
            self.context.state_root.as_path(),
            &ticket,
            caps,
            clock,
            resources,
        )?;
        for strength in [Strength::Refuse, Strength::Warn] {
            let caps = configured.under_strength(&policy, strength);
            let Some(hit) =
                crate::ledger::cap_hit(&ledger, self.context.project_id, ticket.lane_class, caps)?
            else {
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
        self.write(call, |tx| {
            let old = self.record(id, call)?;
            if !matches!(old.state, AttemptState::Starting { .. }) {
                return Err(RunError::Refused);
            }
            let mut ticket = self.ticket(tx, id)?;
            // Host and project counts and spacing span every execution
            // namespace, so they live in the host ledger, not this store.
            // The ledger reads the clock under its own lock; the lane starts at
            // the time spacing was judged and recorded.
            let Some(now) = crate::ledger::lease_with_policy(
                &ledger,
                self.context.state_root.as_path(),
                &ticket,
                clock,
                call,
                &owner,
                &policy,
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

/// Milliseconds as seconds to one decimal place, rounded up so a wait never
/// reads as zero while it holds.

fn deadline(from: WallTime, seconds: u64) -> Result<WallTime> {
    DurationMicros::from_seconds(seconds)
        .and_then(|duration| from.checked_add(duration))
        .map_err(|_| RunError::Validation("admission deadline"))
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
        let lane = store.record(run.attempt_id, &call)?;
        let message = store.admission_observations(&lane, &call, &LowMemory)?;
        assert!(
            message.contains("memory=Ok(MemorySample { available_bytes: 1, total_bytes: 100 })"),
            "{message}"
        );
        assert!(message.contains("floor=500 basis points"), "{message}");
        Ok(())
    }

    fn starting_lane(
        store: &RunStore,
        call: &Request<'_>,
    ) -> std::result::Result<AttemptRecord, Box<dyn std::error::Error>> {
        let run = crate::tests::input();
        store.create(&run, call)?;
        let queued = store.record(run.attempt_id, call)?;
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
    fn admission_starting_lane_reports_launch_preparation_without_a_limit()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?;
        let lane = starting_lane(&store, &call)?;
        let waits =
            store.lane_waits_with_resources(&[lane], &call, &crate::tests::HostFacts::new(100));
        assert!(
            waits
                .iter()
                .any(|wait| wait.waiting_for.starts_with("launch preparation"))
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
}
