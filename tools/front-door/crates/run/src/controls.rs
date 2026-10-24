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
/// What reconcile found for one nonterminal attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reconciled {
    /// Not yet handed off; `start` launches it.
    Queued,
    /// A live guardian still holds custody.
    Held,
    /// A durable exit was committed and released.
    Finished,
    /// Launch never reached a guardian; ended as rejected.
    Withdrawn,
    /// Cleanup ended with these descendants possibly alive (for example one
    /// that left the process group); custody stays fenced until they exit.
    Escaped { pids: Vec<i64> },
    /// Custody cannot be proven either way; it stays fenced.
    Uncertain { reason: String },
}

impl RunStore {
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
    pub fn cancel(&self, id: AttemptId, reason: &str, call: &Request<'_>) -> Result<CancelReceipt> {
        // Half the budget for waiting on the handoff; the rest for the store.
        let remaining = call.budget.remaining(call.clock.mono_now()).as_micros();
        let deadline = std::time::Instant::now() + std::time::Duration::from_micros(remaining / 2);
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
        // A replay is answered by `create`, which compares the request digest.
        let replay = self
            .operation(&input.operation_id, call)?
            .is_some_and(|op| op.state != OperationState::Prepared);
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

    /// Up to `limit` runs with nonterminal work in this project, oldest
    /// first, for a recovery scan.
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
        Ok(runs)
    }

    /// Check every nonterminal attempt of `run` against supervisor custody.
    /// A durable exit is committed and released; a launch that provably never
    /// reached a guardian is withdrawn; anything else is left as found.
    pub fn reconcile(
        &self,
        run: RunId,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<Vec<(AttemptId, Reconciled)>> {
        let mut ids: Vec<String> = call.retry(|| {
            let mut query = self.db.prepare(
                "SELECT id FROM attempts WHERE run=?1 AND project=?2 AND terminal=0 ORDER BY sequence",
            )?;
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
                .unwrap_or_else(|error| Reconciled::Uncertain {
                    reason: error.to_string(),
                });
            found.push((id, state));
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
    fn reconcile_attempt(
        &self,
        id: AttemptId,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<Reconciled> {
        let record = self.record(id, call)?;
        let scope = Self::scope(&record);
        match record.state {
            AttemptState::Queued => return Ok(Reconciled::Queued),
            AttemptState::Terminal { .. } => {
                let retained = self
                    .writer_fences()?
                    .into_iter()
                    .find(|fence| fence.scope == scope);
                return Ok(match retained {
                    Some(fence)
                        if record.provider_launched == Some(false)
                            && record.exit_evidence.is_none()
                            && self.withdraw_prepared_writer(&fence, call)? =>
                    {
                        Reconciled::Withdrawn
                    }
                    Some(fence) => Reconciled::Uncertain {
                        reason: format!(
                            "writer reservation retained after terminal commit: {}; chair recovery required",
                            fence.reservation.display()
                        ),
                    },
                    None => Reconciled::Finished,
                });
            }
            _ => {}
        }
        let uncertain = |error: &dyn std::fmt::Display| Reconciled::Uncertain {
            reason: error.to_string(),
        };
        let dir = || self.attempt_dir(record.run_id, id, false);
        match custody::reattach(dir()?, scope) {
            Ok(handle) => {
                let budget = provenant_core::WaitBudget::cli(call.clock.mono_now(), 1)
                    .map_err(|_| RunError::Validation("observation budget"))?;
                match custody::observe(&handle, budget, call.cancel, call.clock) {
                    Ok(Observation::Exited(exit))
                        if exit.unresolved_cleanup && !exit.unresolved_pids.is_empty() =>
                    {
                        if custody::any_pid_alive(&exit.unresolved_pids) {
                            return Ok(Reconciled::Escaped {
                                pids: exit.unresolved_pids,
                            });
                        }
                        // None of the named processes is alive. A live guardian
                        // republishes the settled exit; the next pass commits it.
                        // Without one, only chair recovery can clear the fence.
                        match custody::acquire_recovery(
                            dir()?,
                            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                            call.cancel,
                            call.clock,
                        ) {
                            Ok(_) => Ok(uncertain(
                                &"descendants are gone but no guardian can settle the exit; chair recovery required",
                            )),
                            Err(_) => Ok(Reconciled::Held),
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
                    Ok(Observation::Unknown) => Ok(uncertain(&"guardian is not observable")),
                    Err(error) => Ok(uncertain(&error)),
                }
            }
            // No launch summary: the handoff never confirmed. Only a
            // reservation that provably never spawned may be withdrawn.
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => {
                if record.state != AttemptState::Starting {
                    return Ok(uncertain(&"running attempt has no launch summary"));
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
                            "writer preparation never reached a guardian",
                            now,
                            call,
                        )?;
                        return Ok(Reconciled::Withdrawn);
                    }
                    return Ok(uncertain(&format!(
                        "writer custody is unconfirmed; reservation retained at {}; chair recovery required",
                        fence.reservation.display()
                    )));
                }
                // The submitter died before reserving: the held lease proves no
                // handoff began and keeps a late reservation out.
                let attempt = dir()?;
                let unreserved =
                    custody::prove_unreserved(&attempt, call.budget, call.cancel, call.clock)
                        .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
                let _recovery = match unreserved {
                    Some(lease) => Held::Unreserved(lease),
                    None => {
                        let recovery = match custody::acquire_recovery(
                            dir()?,
                            call.budget,
                            call.cancel,
                            call.clock,
                        ) {
                            Ok(recovery) => recovery,
                            Err(error) => return Ok(uncertain(&error)),
                        };
                        if recovery.scope() != scope || recovery.observe() != RecoveryState::Absent
                        {
                            return Ok(uncertain(&"launch custody is not provably absent"));
                        }
                        if read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some() {
                            return Ok(uncertain(&"exit exists without a launch summary"));
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
                    "launch never reached a guardian",
                    now,
                    call,
                )?;
                Ok(match done {
                    Some(_) => Reconciled::Withdrawn,
                    None => Reconciled::Finished,
                })
            }
            Err(error) => Ok(uncertain(&error)),
        }
    }
}
/// Writer exclusion held across a withdrawal.
#[expect(dead_code, reason = "held only for its lock")]
enum Held {
    Unreserved(custody::Unreserved),
    Reserved(Box<custody::Recovery>),
}
