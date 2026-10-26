//! Explicit controls: cancel, resume and reconcile. None of them guesses at
//! process state; uncertain custody stays fenced and is reported as such.
use crate::{artifacts::read_sealed, launch::Providers, *};
use provenant_core::{AttemptId, OperationId, RunId, WallTime};
use provenant_supervisor::custody::{
    self, CustodyError, Observation, RecoveryState, Scope, StopReceipt,
};
use provenant_supervisor::machine::StopReason;
use rusqlite::params;
use serde::Serialize;

/// What a cancel request achieved. Requested does not prove exit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CancelReceipt {
    /// Queued work ended without reaching a provider.
    Cancelled,
    AlreadyTerminal,
    Requested,
    AlreadyRequested,
    /// No launch summary or custody to address; reconcile first.
    Unobservable {
        reason: String,
    },
}
/// A continuation of one terminal attempt with new input, as a new run.
pub struct Resume {
    pub prior: AttemptId,
    pub expected_generation: u64,
    pub operation_id: OperationId,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub prompt: String,
    pub created_at: WallTime,
}
/// What reconcile found for one attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reconciled {
    /// Not yet handed off; `start` launches it.
    Queued,
    /// A live guardian still holds custody.
    Held,
    /// A durable exit was committed and released.
    Finished,
    /// Leftover temporary data was removed from a terminal attempt.
    TemporaryRemoved,
    /// The attempt is terminal, but temporary data could not be removed.
    TemporaryRetained {
        path: std::path::PathBuf,
        reason: String,
    },
    /// Launch never reached a guardian; ended as rejected.
    Withdrawn,
    /// Cleanup ended with these descendants possibly alive (for example one
    /// that left the process group); custody stays fenced until they exit.
    Escaped { pids: Vec<i64> },
    /// Custody cannot be proven either way; it stays fenced.
    Uncertain { reason: UncertainReason },
}
/// Why custody could not be proven. The text of an underlying error rides
/// along as `error`; the kind is what a caller matches on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UncertainReason {
    /// Reconciling the attempt failed before it could be settled.
    ReconcileFailed { error: String },
    /// Custody could not be reattached, acquired or observed.
    CustodyUnavailable { error: String },
    /// A running attempt has no launch summary.
    NoLaunchSummary,
    /// The launch custody is neither held nor provably absent.
    CustodyNotProvablyAbsent,
    /// A process exit exists although no launch was recorded.
    ExitWithoutLaunchSummary,
    /// The guardian is neither alive nor finished as far as can be seen.
    GuardianNotObservable,
    /// A launch was handed to a writer whose custody is unconfirmed.
    WriterCustodyUnconfirmed { reservation: std::path::PathBuf },
    /// A writer reservation outlived the attempt's terminal commit.
    WriterReservationRetained { reservation: std::path::PathBuf },
}
impl std::fmt::Display for UncertainReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReconcileFailed { error } | Self::CustodyUnavailable { error } => {
                f.write_str(error)
            }
            Self::NoLaunchSummary => f.write_str("running attempt has no launch summary"),
            Self::CustodyNotProvablyAbsent => f.write_str("launch custody is not provably absent"),
            Self::ExitWithoutLaunchSummary => f.write_str("exit exists without a launch summary"),
            Self::GuardianNotObservable => f.write_str("guardian is not observable"),
            Self::WriterCustodyUnconfirmed { reservation } => write!(
                f,
                "writer custody is unconfirmed; reservation retained at {}; chair recovery required",
                reservation.display()
            ),
            Self::WriterReservationRetained { reservation } => write!(
                f,
                "writer reservation retained after terminal commit: {}; chair recovery required",
                reservation.display()
            ),
        }
    }
}

/// How long recovery waits after each of TERM and KILL for an orphan to go.
const ORPHAN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

impl Reconciled {
    fn uncertain(reason: UncertainReason) -> Self {
        Self::Uncertain { reason }
    }
    fn custody_unavailable(error: &dyn std::fmt::Display) -> Self {
        Self::uncertain(UncertainReason::CustodyUnavailable {
            error: error.to_string(),
        })
    }
}

impl RunStore {
    /// Writer exclusion for an attempt, taken without waiting: failure means a
    /// guardian (or an inheritor of its lease) still holds custody.
    fn orphan_recovery(
        &self,
        record: &AttemptRecord,
        attempt: provenant_supervisor::fs::Directory,
        call: &Request<'_>,
    ) -> Result<custody::Recovery> {
        let request = self.request(record.run_id, call)?;
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(RunError::Validation("recovery task missing"))?;
        let directory = match &task.writer {
            Some(writer) => self.writer_custody_directory(record, writer)?,
            None => attempt,
        };
        custody::acquire_recovery(
            directory,
            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
            call.cancel,
            call.clock,
        )
        .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))
    }
    pub(crate) fn scope(record: &AttemptRecord) -> Scope {
        Scope {
            run: record.run_id,
            attempt: record.attempt_id,
            generation: record.generation,
        }
    }
    /// Bind a cancel operation to its target before any attempt is touched.
    /// The same ID for the same target is a replay; for another target it is
    /// an operation conflict.
    pub fn bind_cancel(
        &self,
        operation: &OperationId,
        target: &str,
        call: &Request<'_>,
    ) -> Result<()> {
        let (project, actor) = (
            self.context.project_id.to_string(),
            self.context.actor_id.to_string(),
        );
        let bound: String = call.retry(|| {
            self.db.execute(
                "INSERT INTO cancellations VALUES (?1,?2,?3,?4) ON CONFLICT DO NOTHING",
                params![project, actor, operation.as_str(), target],
            )?;
            Ok(self.db.query_row(
                "SELECT target FROM cancellations WHERE project=?1 AND actor=?2 AND operation=?3",
                params![project, actor, operation.as_str()],
                |r| r.get(0),
            )?)
        })?;
        if bound != target {
            return Err(RunError::OperationConflict);
        }
        Ok(())
    }
    /// Cancel one attempt. Queued work is withdrawn; started work gets a stop
    /// request. A handoff that has not yet published its launch summary is
    /// retried within the call budget.
    /// Stop a parent's children on its explicit stop, returning each failure.
    /// Every child handed to a guardian stops from the scope held at
    /// hand-off, including one start launched after the parent's scan read
    /// its snapshot. The lane-writable record decides only for `accepted`
    /// children, with the generation the scan expected, that never reached
    /// a guardian.
    pub fn stop_children(
        &self,
        accepted: std::collections::BTreeSet<(AttemptId, u64)>,
        reason: &str,
        call: &Request<'_>,
    ) -> Vec<String> {
        let held = self.held_attempts();
        let unheld = accepted
            .into_iter()
            .filter(|(id, _)| !held.contains(id))
            .map(|(id, generation)| (id, Some(generation)));
        let mut errors = Vec::new();
        for (id, generation) in held.iter().map(|id| (*id, None)).chain(unheld) {
            if let Some(generation) = generation {
                let record = match self.record(id, call) {
                    Ok(record) => record,
                    Err(error) => {
                        errors.push(error.to_string());
                        continue;
                    }
                };
                if matches!(record.state, AttemptState::Terminal { .. }) {
                    continue;
                }
                let still_queued = record.state == AttemptState::Queued
                    && record.generation.checked_add(1) == Some(generation);
                if record.generation != generation && !still_queued {
                    errors.push("accepted child generation changed".into());
                    continue;
                }
            }
            match self.cancel(id, reason, call) {
                Ok(CancelReceipt::Unobservable { reason }) => errors.push(reason),
                Err(error) => errors.push(error.to_string()),
                Ok(_) => {}
            }
        }
        errors
    }
    // Cancellation coordinates with a real guardian handoff under a hard cap.
    #[allow(clippy::disallowed_methods)]
    pub fn cancel(&self, id: AttemptId, reason: &str, call: &Request<'_>) -> Result<CancelReceipt> {
        // Half the budget for waiting on the handoff; the rest for the store.
        let remaining = call.budget.remaining(call.clock.mono_now()).as_micros();
        let deadline = std::time::Instant::now() + std::time::Duration::from_micros(remaining / 2);
        // A child this host handed to a guardian stops through the directory,
        // scope and requester held then. The lane can rewrite the child's
        // record and launch summary, so neither decides whether or for whom
        // it stops.
        if let Some(held) = self
            .launched
            .borrow()
            .iter()
            .rev()
            .find(|held| held.scope.attempt == id)
        {
            let Some(handle) = &held.handle else {
                // The latest handle is dropped only after observing guardian
                // exit. Earlier superseded generations cannot be selected.
                return Ok(CancelReceipt::AlreadyTerminal);
            };
            let stopped = custody::stop(handle, StopReason::ExplicitCancellation)
                .map_err(|error| error.to_string());
            return Ok(match stopped {
                Ok(StopReceipt::Requested) => CancelReceipt::Requested,
                Ok(StopReceipt::AlreadyRequested) => CancelReceipt::AlreadyRequested,
                Err(reason) => CancelReceipt::Unobservable { reason },
            });
        }
        loop {
            let record = self.record(id, call)?;
            match record.state {
                AttemptState::Terminal { .. } => return Ok(CancelReceipt::AlreadyTerminal),
                AttemptState::Queued => {
                    let now = call
                        .clock
                        .wall_now()
                        .map_err(|_| RunError::Validation("wall clock"))?;
                    match self.withdraw(
                        id,
                        AttemptState::Queued,
                        Outcome::Cancelled,
                        reason,
                        now,
                        call,
                    ) {
                        Ok(Some(_)) => return Ok(CancelReceipt::Cancelled),
                        Ok(None) => return Ok(CancelReceipt::AlreadyTerminal),
                        // A concurrent start took it; it is no longer queued.
                        Err(RunError::Conflict) => continue,
                        Err(error) => return Err(error),
                    }
                }
                _ => {}
            }
            let dir = self.attempt_dir(record.run_id, id, false)?;
            match custody::reattach(dir, Self::scope(&record)) {
                Ok(handle) => {
                    return Ok(
                        match custody::stop(&handle, StopReason::ExplicitCancellation) {
                            Ok(StopReceipt::Requested) => CancelReceipt::Requested,
                            Ok(StopReceipt::AlreadyRequested) => CancelReceipt::AlreadyRequested,
                            Err(error) => CancelReceipt::Unobservable {
                                reason: error.to_string(),
                            },
                        },
                    );
                }
                Err(CustodyError::Filesystem(error))
                    if error.is_not_found()
                        && record.state == AttemptState::Starting
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::park_timeout(std::time::Duration::from_millis(100));
                }
                // The handoff window is spent and there is still no launch
                // summary: settle a launch nobody holds, as reconcile does.
                Err(CustodyError::Filesystem(error))
                    if error.is_not_found() && record.state == AttemptState::Starting =>
                {
                    // A busy or unreadable custody lock leaves it untouched.
                    let settled =
                        self.settle_unlaunched(id, &record, call)
                            .unwrap_or_else(|error| {
                                Reconciled::uncertain(UncertainReason::ReconcileFailed {
                                    error: error.to_string(),
                                })
                            });
                    return Ok(match settled {
                        Reconciled::Withdrawn => CancelReceipt::Cancelled,
                        Reconciled::Finished => CancelReceipt::AlreadyTerminal,
                        Reconciled::Uncertain { reason } => CancelReceipt::Unobservable {
                            reason: reason.to_string(),
                        },
                        other => CancelReceipt::Unobservable {
                            reason: format!("{other:?}"),
                        },
                    });
                }
                Err(error) => {
                    return Ok(CancelReceipt::Unobservable {
                        reason: error.to_string(),
                    });
                }
            }
        }
    }

    /// Accept a continuation run. Each terminal attempt can be continued
    /// once; `expected_generation` is 0 before that and 1 after. The route,
    /// access and timeout come from the prior task, never from the caller.
    pub fn resume(&self, input: &Resume, call: &Request<'_>) -> Result<RunRecord> {
        let prior = self.record(input.prior, call)?;
        if !matches!(prior.state, AttemptState::Terminal { .. }) {
            return Err(RunError::Nonterminal);
        }
        let replay = self.operation(&input.operation_id, call)?.is_some();
        let _retention = if replay {
            None
        } else {
            Some(self.retain_provider_state(input.prior, call)?)
        };
        let request = self.request(prior.run_id, call)?;
        let task = request
            .tasks
            .iter()
            .find(|t| t.task_id == prior.task_id)
            .ok_or(RunError::Validation("prior task missing from request"))?;
        let prepared = PreparedRun {
            run_id: input.run_id,
            operation_id: input.operation_id.clone(),
            request: CanonicalRequest {
                schema_version: 1,
                tasks: vec![TaskRequest {
                    base: task.base.clone(),
                    base_ref: task.base_ref.clone(),
                    profile: task.profile.clone(),
                    task_id: prior.task_id,
                    prompt_digest: provenant_records::CanonicalV1::file_digest(
                        input.prompt.as_bytes(),
                    ),
                    route: prior.requested_route.clone(),
                    account: task.account.clone(),
                    fallback: vec![],
                    authority_digest: prior.authority_digest,
                    writer: task.writer.clone(),
                    deliver: task.deliver.clone(),
                    provider_timeout_seconds: task.provider_timeout_seconds,
                    delivery_id: prior.delivery_id,
                    access: task.access.clone(),
                }],
                concurrency: 1,
                parent: Some(input.prior),
                lane_depth: request.lane_depth,
                parent_attempt: request.parent_attempt,
            },
            attempts: vec![NewAttempt {
                attempt_id: input.attempt_id,
                task_id: prior.task_id,
                prompt: input.prompt.clone(),
            }],
            created_at: input.created_at,
        };
        // A replay, prepared or accepted, is answered by `create`, which
        // compares the request digest and adopts a prepared operation's IDs.
        if !replay {
            let generation: i64 = call.retry(|| {
                Ok(self.db.query_row(
                    "SELECT COUNT(*) FROM continuations WHERE target=?1",
                    [input.prior.to_string()],
                    |r| r.get(0),
                )?)
            })?;
            if u64::try_from(generation).ok() != Some(input.expected_generation) {
                return Err(RunError::Conflict);
            }
        }
        self.create(&prepared, call)
    }

    /// Up to `limit` runs needing recovery or terminal temporary cleanup in
    /// this project. Nonterminal work takes priority over temporary cleanup.
    pub fn open_runs(&self, limit: u64, call: &Request<'_>) -> Result<Vec<RunId>> {
        let limit = crate::db::sql_int(limit)?;
        let ids: Vec<String> = call.retry(|| {
            let mut query = self.db.prepare(
                "SELECT run FROM attempts WHERE project=?1 AND terminal=0 GROUP BY run ORDER BY MIN(sequence) LIMIT ?2",
            )?;
            let rows = query.query_map(params![self.context.project_id.to_string(), limit], |r| r.get(0))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        let mut runs = ids
            .iter()
            .map(|id| RunId::parse(id).map_err(|_| RunError::Validation("run index ID")))
            .collect::<Result<Vec<_>>>()?;
        for fence in self.writer_fences()? {
            if !runs.contains(&fence.scope.run) && runs.len() < limit as usize {
                runs.push(fence.scope.run);
            }
        }
        if runs.len() < limit as usize {
            call.retry(|| {
                let mut query = self.db.prepare(
                    "SELECT run FROM attempts WHERE project=?1 AND terminal=1 AND tmp_retained=1 GROUP BY run ORDER BY MIN(sequence) LIMIT ?2",
                )?;
                let mut rows = query.query(params![self.context.project_id.to_string(), limit])?;
                while runs.len() < limit as usize {
                    let Some(row) = rows.next()? else { break };
                    let run = RunId::parse(&row.get::<_, String>(0)?)
                        .map_err(|_| RunError::Validation("run index ID"))?;
                    if runs.contains(&run) {
                        continue;
                    }
                    runs.push(run);
                }
                Ok(())
            })?;
        }
        Ok(runs)
    }

    /// Check every attempt of `run` against supervisor custody.
    /// A durable exit is committed and released; a launch that provably never
    /// reached a guardian is withdrawn. Terminal temporary data is retried
    /// only after reservations are released; removal failures are reported.
    pub fn reconcile(
        &self,
        run: RunId,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<Vec<(AttemptId, Reconciled)>> {
        let mut ids: Vec<String> = call.retry(|| {
            let mut query = self
                .db
                .prepare("SELECT id FROM attempts WHERE run=?1 AND project=?2 AND (terminal=0 OR tmp_retained=1) ORDER BY sequence")?;
            let rows = query.query_map(
                params![run.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        for fence in self.writer_fences()? {
            if fence.scope.run == run && !ids.contains(&fence.scope.attempt.to_string()) {
                ids.push(fence.scope.attempt.to_string());
            }
        }
        let mut found = Vec::new();
        for id in ids {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            let state = self
                .reconcile_attempt(id, providers, call)
                .unwrap_or_else(|error| {
                    Some(Reconciled::uncertain(UncertainReason::ReconcileFailed {
                        error: error.to_string(),
                    }))
                });
            if let Some(state) = state {
                found.push((id, state));
            }
        }
        Ok(found)
    }
    fn withdraw_prepared_writer(&self, fence: &WriterFence, call: &Request<'_>) -> Result<bool> {
        let directory = fence.reservation.parent().ok_or(RunError::UnsafePath)?;
        let recovery = custody::acquire_recovery(
            provenant_supervisor::fs::Directory::open_anchor(directory)?,
            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
            call.cancel,
            call.clock,
        );
        Ok(match recovery {
            Ok(recovery) if recovery.scope() == fence.scope => recovery.withdraw_prepared().is_ok(),
            _ => false,
        })
    }
    /// Settle an attempt that has no launch summary. Only a reservation or
    /// handoff that provably never spawned is withdrawn; a held custody lock
    /// or a live provider leaves it untouched and uncertain.
    fn settle_unlaunched(
        &self,
        id: AttemptId,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<Reconciled> {
        let scope = Self::scope(record);
        let dir = || self.attempt_dir(record.run_id, id, false);
        if record.state != AttemptState::Starting {
            return Ok(Reconciled::uncertain(UncertainReason::NoLaunchSummary));
        }
        if let Some(fence) = self
            .writer_fences()?
            .into_iter()
            .find(|fence| fence.scope == scope)
        {
            if self.withdraw_prepared_writer(&fence, call)? {
                let now = call
                    .clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))?;
                self.withdraw(
                    id,
                    AttemptState::Starting,
                    Outcome::Rejected,
                    "launch interrupted: writer preparation never reached a guardian",
                    now,
                    call,
                )?;
                return Ok(Reconciled::Withdrawn);
            }
            return Ok(Reconciled::uncertain(
                UncertainReason::WriterCustodyUnconfirmed {
                    reservation: fence.reservation.clone(),
                },
            ));
        }
        // The submitter died before reserving: the held lease proves no
        // handoff began and keeps a late reservation out.
        let attempt = dir()?;
        let unreserved = custody::prove_unreserved(&attempt, call.budget, call.cancel, call.clock)
            .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
        let _recovery = match unreserved {
            Some(lease) => Held::Unreserved(lease),
            None => {
                let recovery =
                    match custody::acquire_recovery(dir()?, call.budget, call.cancel, call.clock) {
                        Ok(recovery) => recovery,
                        Err(error) => return Ok(Reconciled::custody_unavailable(&error)),
                    };
                if recovery.scope() != scope || recovery.observe() != RecoveryState::Absent {
                    return Ok(Reconciled::uncertain(
                        UncertainReason::CustodyNotProvablyAbsent,
                    ));
                }
                if read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some() {
                    return Ok(Reconciled::uncertain(
                        UncertainReason::ExitWithoutLaunchSummary,
                    ));
                }
                Held::Reserved(Box::new(recovery))
            }
        };
        let now = call
            .clock
            .wall_now()
            .map_err(|_| RunError::Validation("wall clock"))?;
        let done = self.withdraw(
            id,
            AttemptState::Starting,
            Outcome::Rejected,
            "launch interrupted: never reached a guardian",
            now,
            call,
        )?;
        Ok(match done {
            Some(_) => Reconciled::Withdrawn,
            None => Reconciled::Finished,
        })
    }
    fn reconcile_terminal_temporary(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<Option<Reconciled>> {
        let id = record.attempt_id;
        let scope = Self::scope(record);
        let path = self
            .context
            .state_root
            .as_path()
            .join(format!("runs/{}/attempts/{id}/tmp", record.run_id));
        let listing = match self.writer_fence_listing() {
            Ok(listing) => listing,
            Err(error) => {
                return Ok(Some(Reconciled::TemporaryRetained {
                    path,
                    reason: error.to_string(),
                }));
            }
        };
        let retained = listing
            .reservations
            .into_iter()
            .find(|fence| fence.scope.run == record.run_id && fence.scope.attempt == id);
        match retained {
            Some(fence)
                if fence.scope == scope
                    && record.provider_launched == Some(false)
                    && record.exit_evidence.is_none()
                    && self.withdraw_prepared_writer(&fence, call)? =>
            {
                // Another retained reservation may name this attempt.
                // Retry temporary cleanup on the next scan.
                return Ok(Some(Reconciled::Withdrawn));
            }
            Some(fence) => {
                return Ok(Some(Reconciled::uncertain(
                    UncertainReason::WriterReservationRetained {
                        reservation: fence.reservation.clone(),
                    },
                )));
            }
            None => {}
        }
        let attempt = match self
            .attempt_dir(record.run_id, id, false)
            .and_then(|attempt| {
                attempt.metadata_at(std::ffi::OsStr::new("tmp"), false)?;
                Ok(attempt)
            }) {
            Ok(attempt) => attempt,
            Err(RunError::Filesystem(error)) if error.is_not_found() => {
                self.set_temporary_retained(id, false, call)?;
                return Ok(None);
            }
            Err(error) => {
                return Ok(Some(Reconciled::TemporaryRetained {
                    path,
                    reason: error.to_string(),
                }));
            }
        };
        // An unreadable reservation cannot establish absence of
        // custody for this attempt, so leave temporary data alone.
        if let Some(issue) = listing.issues.first() {
            return Ok(Some(Reconciled::TemporaryRetained {
                path,
                reason: format!(
                    "writer reservation unreadable at {}: {}",
                    issue.reservation.display(),
                    issue.reason
                ),
            }));
        }
        let remove = || -> Result<()> {
            match attempt.metadata_at(std::ffi::OsStr::new("tmp"), false) {
                Err(error) if error.is_not_found() => return Ok(()),
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
            // A read-only guardian reserves the attempt itself. Hold
            // its exclusion lock and refuse any retained reservation.
            let _lease = attempt.try_lock(
                std::ffi::OsStr::new("writer.lock"),
                provenant_supervisor::fs::LockMode::Exclusive,
            )?;
            match attempt.metadata_at(std::ffi::OsStr::new("reservation.json"), false) {
                Ok(_) => return Err(RunError::Validation("attempt reservation retained")),
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error.into()),
            }
            artifacts::remove_unreserved_terminal_temporary(&attempt)?;
            Ok(())
        };
        Ok(Some(match remove() {
            Ok(()) => {
                self.set_temporary_retained(id, false, call)?;
                Reconciled::TemporaryRemoved
            }
            Err(error) => Reconciled::TemporaryRetained {
                path,
                reason: error.to_string(),
            },
        }))
    }

    fn reconcile_attempt(
        &self,
        id: AttemptId,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<Option<Reconciled>> {
        let record = self.record(id, call)?;
        if matches!(record.state, AttemptState::Terminal { .. }) {
            return self.reconcile_terminal_temporary(&record, call);
        }
        self.reconcile_live_attempt(&record, providers, call)
            .map(Some)
    }

    fn reconcile_live_attempt(
        &self,
        record: &AttemptRecord,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<Reconciled> {
        let id = record.attempt_id;
        let scope = Self::scope(record);
        if record.state == AttemptState::Queued {
            return Ok(Reconciled::Queued);
        }
        let dir = || self.attempt_dir(record.run_id, id, false);
        match custody::reattach(dir()?, scope) {
            Ok(handle) => {
                let budget = provenant_core::WaitBudget::cli(call.clock.mono_now(), 1)
                    .map_err(|_| RunError::Validation("observation budget"))?;
                match custody::observe(&handle, budget, call.cancel, call.clock) {
                    Ok(Observation::Exited(exit))
                        if exit.unresolved_cleanup && !exit.unresolved_pids.is_empty() =>
                    {
                        let still_named = || {
                            if custody::any_pid_alive(&exit.unresolved_pids) {
                                Reconciled::Escaped {
                                    pids: exit.unresolved_pids.clone(),
                                }
                            } else {
                                // A live guardian republishes the settled exit;
                                // the next pass commits it.
                                Reconciled::Held
                            }
                        };
                        // The writer lock is free only when no guardian remains
                        // to settle this exit, so recovery ends the orphans.
                        let Ok(mut recovery) = self.orphan_recovery(record, dir()?, call) else {
                            return Ok(still_named());
                        };
                        match recovery.end_orphans(&dir()?, ORPHAN_PATIENCE) {
                            Ok(report) if report.survivors.is_empty() => {
                                drop(recovery);
                                // The exit is settled: commit it as any other.
                                self.reconcile_attempt(id, providers, call)
                                    .map(|state| state.unwrap_or(Reconciled::Finished))
                            }
                            Ok(report) => Ok(Reconciled::Escaped {
                                pids: report.survivors.iter().map(|s| s.pid).collect(),
                            }),
                            Err(CustodyError::Fenced) => Ok(still_named()),
                            Err(error) => Ok(Reconciled::custody_unavailable(&error)),
                        }
                    }
                    Ok(Observation::Exited(exit)) => {
                        let attempt = dir()?;
                        self.finish(id, &attempt, &exit, providers, call)?;
                        custody::publish_release(&attempt, scope)
                            .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
                        Ok(Reconciled::Finished)
                    }
                    Ok(Observation::Live | Observation::PublicationPending) => Ok(Reconciled::Held),
                    Ok(Observation::Unknown) => Ok(Reconciled::uncertain(
                        UncertainReason::GuardianNotObservable,
                    )),
                    Err(error) => Ok(Reconciled::custody_unavailable(&error)),
                }
            }
            // No launch summary: the handoff never confirmed. Only a
            // reservation that provably never spawned may be withdrawn.
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => {
                self.settle_unlaunched(id, record, call)
            }
            Err(error) => Ok(Reconciled::custody_unavailable(&error)),
        }
    }
}
/// Writer exclusion held across a withdrawal.
#[expect(dead_code, reason = "held only for its lock")]
enum Held {
    Unreserved(custody::Unreserved),
    Reserved(Box<custody::Recovery>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertain_reasons_serialise_by_kind_and_keep_their_text() {
        let reservation = std::path::PathBuf::from("/state/custody/reservation.json");
        let found = Reconciled::uncertain(UncertainReason::WriterCustodyUnconfirmed {
            reservation: reservation.clone(),
        });
        let value = serde_json::to_value(&found).unwrap();
        assert_eq!(value["state"], "uncertain");
        assert_eq!(value["reason"]["kind"], "writer_custody_unconfirmed");
        assert_eq!(
            value["reason"]["reservation"],
            "/state/custody/reservation.json"
        );
        let Reconciled::Uncertain { reason } = found else {
            unreachable!()
        };
        assert!(reason.to_string().contains("chair recovery required"));
        let unit = serde_json::to_value(UncertainReason::GuardianNotObservable).unwrap();
        assert_eq!(
            unit,
            serde_json::json!({ "kind": "guardian_not_observable" })
        );
        let custody = Reconciled::custody_unavailable(&"lock is held");
        assert_eq!(
            custody,
            Reconciled::Uncertain {
                reason: UncertainReason::CustodyUnavailable {
                    error: "lock is held".into()
                }
            }
        );
    }
}
