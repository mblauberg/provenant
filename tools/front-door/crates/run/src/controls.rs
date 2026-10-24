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
    /// Custody cannot be proven either way; it stays fenced.
    Uncertain { reason: String },
}

impl RunStore {
    fn scope(record: &AttemptRecord) -> Scope {
        Scope {
            run: record.run_id,
            attempt: record.attempt_id,
            generation: record.generation,
        }
    }
    pub fn cancel(&self, id: AttemptId, reason: &str, call: &Request<'_>) -> Result<CancelReceipt> {
        let record = self.record(id, call)?;
        match record.state {
            AttemptState::Terminal { .. } => return Ok(CancelReceipt::AlreadyTerminal),
            AttemptState::Queued => {
                let now = call
                    .clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))?;
                return match self.withdraw(
                    id,
                    AttemptState::Queued,
                    Outcome::Cancelled,
                    reason,
                    now,
                    call,
                ) {
                    Ok(_) => Ok(CancelReceipt::Cancelled),
                    // A concurrent start took it; ask its guardian instead.
                    Err(RunError::Conflict) => self.cancel(id, reason, call),
                    Err(error) => Err(error),
                };
            }
            _ => {}
        }
        let dir = self.attempt_dir(record.run_id, id, false)?;
        let receipt = custody::reattach(dir, Self::scope(&record))
            .and_then(|handle| custody::stop(&handle, StopReason::ExplicitCancellation));
        Ok(match receipt {
            Ok(StopReceipt::Requested) => CancelReceipt::Requested,
            Ok(StopReceipt::AlreadyRequested) => CancelReceipt::AlreadyRequested,
            Err(error) => CancelReceipt::Unobservable {
                reason: error.to_string(),
            },
        })
    }

    /// Accept a continuation run. Each terminal attempt can be continued
    /// once; `expected_generation` is 0 before that and 1 after. The route,
    /// access and timeout come from the prior task, never from the caller.
    pub fn resume(&self, input: &Resume, call: &Request<'_>) -> Result<RunRecord> {
        if let Some(op) = self.operation(&input.operation_id, call)?
            && op.state != OperationState::Prepared
        {
            let run = self.run(op.run_id, call)?;
            if run.parent != Some(input.prior) {
                return Err(RunError::OperationConflict);
            }
            return Ok(run);
        }
        let prior = self.record(input.prior, call)?;
        if !matches!(prior.state, AttemptState::Terminal { .. }) {
            return Err(RunError::Nonterminal);
        }
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
                    task_id: prior.task_id,
                    prompt_digest: provenant_records::CanonicalV1::file_digest(
                        input.prompt.as_bytes(),
                    ),
                    route: prior.requested_route.clone(),
                    fallback: vec![],
                    authority_digest: prior.authority_digest,
                    provider_timeout_seconds: task.provider_timeout_seconds,
                    delivery_id: prior.delivery_id,
                }],
                concurrency: 1,
                parent: Some(input.prior),
            },
            attempts: vec![NewAttempt {
                attempt_id: input.attempt_id,
                task_id: prior.task_id,
                prompt: input.prompt.clone(),
            }],
            created_at: input.created_at,
        };
        self.create(&prepared, call)
    }

    /// Nonterminal runs in this project, oldest first, for a recovery scan.
    pub fn open_runs(&self, call: &Request<'_>) -> Result<Vec<RunId>> {
        let ids: Vec<String> = call.retry(|| {
            let mut query = self.db.prepare(
                "SELECT run FROM attempts WHERE project=?1 AND terminal=0 GROUP BY run ORDER BY MIN(sequence) LIMIT 320",
            )?;
            let rows = query.query_map([self.context.project_id.to_string()], |r| r.get(0))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        ids.iter()
            .map(|id| RunId::parse(id).map_err(|_| RunError::Validation("run index ID")))
            .collect()
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
        let ids: Vec<String> = call.retry(|| {
            let mut query = self.db.prepare(
                "SELECT id FROM attempts WHERE run=?1 AND project=?2 AND terminal=0 ORDER BY sequence",
            )?;
            let rows = query.query_map(
                params![run.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        let mut found = Vec::new();
        for id in ids {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            found.push((id, self.reconcile_attempt(id, providers, call)?));
        }
        Ok(found)
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
            AttemptState::Terminal { .. } => return Ok(Reconciled::Finished),
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
                let recovery =
                    match custody::acquire_recovery(dir()?, call.budget, call.cancel, call.clock) {
                        Ok(recovery) => recovery,
                        Err(error) => return Ok(uncertain(&error)),
                    };
                if recovery.scope() != scope || recovery.observe() != RecoveryState::Absent {
                    return Ok(uncertain(&"launch custody is not provably absent"));
                }
                if read_sealed(&dir()?, "process-exit.json", RECORD_LIMIT)?.is_some() {
                    return Ok(uncertain(&"exit exists without a launch summary"));
                }
                let now = call
                    .clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))?;
                self.withdraw(
                    id,
                    AttemptState::Starting,
                    Outcome::Rejected,
                    "launch never reached a guardian",
                    now,
                    call,
                )?;
                Ok(Reconciled::Withdrawn)
            }
            Err(error) => Ok(uncertain(&error)),
        }
    }
}
