use crate::{
    artifacts::{artifact_path, seal},
    db::{bytes, decode, sql_int},
    *,
};
use provenant_core::{AttemptId, OperationId, RunId, TaskId, WallTime};
use provenant_records::CanonicalV1;
use provenant_supervisor::{custody::ExitObservation, exec::Termination, fs::LockMode};
use rusqlite::{OptionalExtension, params};
use std::ffi::OsStr;

pub struct NewAttempt {
    pub attempt_id: AttemptId,
    pub task_id: TaskId,
    /// Exact, already scanned prompt bytes. Store never opens a source file.
    pub prompt: String,
}
pub struct PreparedRun {
    pub run_id: RunId,
    pub operation_id: OperationId,
    pub request: CanonicalRequest,
    pub attempts: Vec<NewAttempt>,
    pub created_at: WallTime,
}
/// Result and route material is supplied by the trusted provider/confinement
/// composition edge, after secret scanning. This storage API never launches.
pub struct TerminalPublication<'a> {
    pub outcome: Outcome,
    pub finished_at: WallTime,
    pub started_at: Option<WallTime>,
    pub provider_ms: Option<u64>,
    pub reason: Option<String>,
    pub result: &'a str,
    pub complete: bool,
    pub redacted: bool,
    pub resolved_route: Option<RouteRecord>,
    pub applied_route: Option<RouteRecord>,
    pub observed_route: Option<RouteRecord>,
    pub usage: UsageRecord,
}
impl RunStore {
    /// Reserve records first, seal the exact request/prompts, then acknowledge.
    /// Repeating a prepared operation repairs a publication gap with the same
    /// request. No scheduler, admission or launch is implied by acceptance here.
    pub fn create(&self, input: &PreparedRun, call: &Request<'_>) -> Result<RunRecord> {
        validate_request(input)?;
        let digest = input.request.digest()?;
        let context = &self.context;
        let existing = self.operation(&input.operation_id, call)?;
        if let Some(op) = &existing {
            if op.request_digest != digest {
                return Err(RunError::OperationConflict);
            }
            if op.state != OperationState::Prepared {
                return self.run(op.run_id, call);
            }
            if op.run_id != input.run_id {
                return Err(RunError::Conflict);
            }
        }
        let run = RunRecord {
            schema_version: 1,
            run_id: input.run_id,
            project_id: context.project_id,
            actor_id: context.actor_id,
            operation_id: input.operation_id.clone(),
            request_digest: digest,
            task_ids: input.request.tasks.iter().map(|t| t.task_id).collect(),
            parent: input.request.parent,
            children: vec![],
            revision: 1,
            generation: 0,
            created_at: input.created_at,
            closed_at: None,
            closure: Closure::Open,
        };
        let operation = OperationRecord {
            schema_version: 1,
            project_id: context.project_id,
            actor_id: context.actor_id,
            operation_id: input.operation_id.clone(),
            kind: OperationKind::Dispatch,
            request_digest: digest,
            run_id: input.run_id,
            state: OperationState::Prepared,
        };
        let dir = self.run_dir(input.run_id, true)?;
        let _writer = dir.lock(
            OsStr::new("publication.lock"),
            LockMode::Exclusive,
            call.budget,
            call.cancel,
            call.clock,
        )?;
        if let Some(op) = self.operation(&input.operation_id, call)? {
            if op.request_digest != digest {
                return Err(RunError::OperationConflict);
            }
            if op.state != OperationState::Prepared {
                return self.run(op.run_id, call);
            }
        }
        self.write(call, |tx| {
            let prior: Option<Vec<u8>> = tx.query_row("SELECT record FROM operations WHERE project=?1 AND actor=?2 AND operation=?3", params![context.project_id.to_string(), context.actor_id.to_string(), input.operation_id.as_str()], |r| r.get(0)).optional()?;
            if let Some(prior) = prior {
                let prior: OperationRecord = decode(&prior)?;
                if prior.request_digest != digest { return Err(RunError::OperationConflict); }
                if prior.run_id != input.run_id { return Err(RunError::Conflict); }
                return Ok(());
            }
            tx.execute("INSERT INTO operations VALUES (?1,?2,?3,?4,?5,?6)", params![context.project_id.to_string(), context.actor_id.to_string(), input.operation_id.as_str(), digest.to_string(), input.run_id.to_string(), bytes(&operation)?])?;
            tx.execute("INSERT INTO runs VALUES (?1,?2,?3)", params![input.run_id.to_string(), context.project_id.to_string(), bytes(&run)?])?;
            for task in &input.request.tasks {
                let new = input.attempts.iter().find(|a| a.task_id == task.task_id).ok_or(RunError::Validation("missing task prompt"))?;
                let attempt = AttemptRecord {
                    schema_version: 1, attempt_id: new.attempt_id, run_id: run.run_id, project_id: run.project_id, task_id: task.task_id, operation_id: run.operation_id.clone(), attempt_number: 1, revision: 1, generation: 0,
                    delivery_id: task.delivery_id, retry_of: None, requested_route: task.route.clone(), resolved_route: None, applied_route: None, observed_route: None, authority_digest: task.authority_digest, subject: None, head: None, verified_owner: Some(context.actor_id), session: None,
                    queued_at: input.created_at, started_at: None, provider_launched: None, provider_ms: None, admission_deadline: None, runtime_deadline: None, state: AttemptState::Queued, usage: UsageRecord::default(),
                    prompt: ArtifactRecord { path: artifact_path(new.attempt_id, "prompt.txt")?, digest: task.prompt_digest, bytes: new.prompt.len() as u64, complete: true, redacted: false }, result: None, publication_error: None, exit_evidence: None,
                };
                tx.execute("INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) VALUES (?1,?2,?3,?4,1,1,0,?5)", params![attempt.attempt_id.to_string(), run.run_id.to_string(), run.project_id.to_string(), task.task_id.to_string(), bytes(&attempt)?])?;
                insert_event(tx, &attempt)?;
            }
            Ok(())
        })?;
        #[cfg(test)]
        crate::tests::crash_at("reserved");
        seal(&dir, "request.json", &bytes(&input.request)?, RECORD_LIMIT)?;
        for attempt in &input.attempts {
            let recorded = self.record(attempt.attempt_id, call)?;
            if recorded.run_id != input.run_id
                || recorded.task_id != attempt.task_id
                || recorded.prompt.digest != CanonicalV1::file_digest(attempt.prompt.as_bytes())
            {
                return Err(RunError::Conflict);
            }
            seal(
                &self.attempt_dir(input.run_id, attempt.attempt_id, true)?,
                "prompt.txt",
                attempt.prompt.as_bytes(),
                RECORD_LIMIT,
            )?;
        }
        self.write(call, |tx| {
            let mut accepted = operation.clone();
            accepted.state = OperationState::Accepted;
            tx.execute(
                "UPDATE operations SET record=?1 WHERE project=?2 AND actor=?3 AND operation=?4",
                params![
                    bytes(&accepted)?,
                    context.project_id.to_string(),
                    context.actor_id.to_string(),
                    input.operation_id.as_str()
                ],
            )?;
            Ok(())
        })?;
        self.run(input.run_id, call)
    }
    pub fn publish_terminal(
        &self,
        id: AttemptId,
        expected_revision: u64,
        publication: &TerminalPublication<'_>,
        exit: &ExitObservation,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        let old = self.record(id, call)?;
        let dir = self.run_dir(old.run_id, false)?;
        let _writer = dir.lock(
            OsStr::new("publication.lock"),
            LockMode::Exclusive,
            call.budget,
            call.cancel,
            call.clock,
        )?;
        let old = self.record(id, call)?;
        let durable_exit = sealed_exit(&self.attempt_dir(old.run_id, id, false)?, exit)?;
        let outcome_exit = match exit.termination {
            Some(Termination::Exit(code)) => Some(code),
            _ => None,
        };
        if exit.scope.run != old.run_id
            || exit.scope.attempt != id
            || exit.scope.generation != old.generation
            || !exit.terminal_observed
            || exit.unresolved_cleanup
        {
            return Err(RunError::Validation(
                "exit/cleanup evidence is unresolved or out of scope",
            ));
        }
        if publication.result.len() as u64 > RESULT_LIMIT {
            return Err(RunError::TooLarge {
                limit: RESULT_LIMIT,
            });
        }
        if publication.outcome == Outcome::Succeeded
            && (outcome_exit != Some(0)
                || exit.never_spawned
                || !publication.complete
                || publication.result.is_empty()
                || !super::reader::verified_route(
                    publication.resolved_route.as_ref(),
                    publication.applied_route.as_ref(),
                    publication.observed_route.as_ref(),
                ))
        {
            return Err(RunError::Validation(
                "success lacks complete result or verified provider exit/identity",
            ));
        }
        let mut next = old.clone();
        next.revision = expected_revision.checked_add(1).ok_or(RunError::Conflict)?;
        next.started_at = publication.started_at;
        next.provider_launched = Some(!exit.never_spawned);
        next.provider_ms = publication.provider_ms;
        next.resolved_route = publication.resolved_route.clone();
        next.applied_route = publication.applied_route.clone();
        next.observed_route = publication.observed_route.clone();
        next.usage = publication.usage.clone();
        next.result = Some(ArtifactRecord {
            path: artifact_path(id, "result.md")?,
            digest: CanonicalV1::file_digest(publication.result.as_bytes()),
            bytes: publication.result.len() as u64,
            complete: publication.complete,
            redacted: publication.redacted,
        });
        next.state = AttemptState::Terminal {
            outcome: publication.outcome,
            exit_code: outcome_exit,
            finished_at: publication.finished_at,
            reason: publication.reason.clone(),
        };
        next.publication_error = None;
        next.exit_evidence = Some(durable_exit);
        if matches!(old.state, AttemptState::Terminal { .. }) {
            if old == next {
                return Ok(old);
            }
            return Err(RunError::Conflict);
        }
        if old.revision != expected_revision {
            return Err(RunError::Conflict);
        }
        super::reader::validate_attempt(&next)?;
        bytes(&next)?;
        let attempt_dir = self.attempt_dir(old.run_id, id, false)?;
        seal(
            &attempt_dir,
            "result.md",
            publication.result.as_bytes(),
            RESULT_LIMIT,
        )?;
        #[cfg(test)]
        crate::tests::crash_at("sealed");
        self.write(call, |tx| {
            let changed = tx.execute("UPDATE attempts SET revision=?1,terminal=1,record=?2 WHERE id=?3 AND revision=?4 AND terminal=0", params![sql_int(next.revision)?, bytes(&next)?, id.to_string(), sql_int(expected_revision)?])?;
            if changed != 1 { return Err(RunError::Conflict); }
            tx.execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
            insert_event(tx, &next)?;
            let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM attempts WHERE run=?1 AND terminal=0)", [old.run_id.to_string()], |r| r.get(0))?;
            let mut run = self.run_on(tx, old.run_id)?;
            run.revision = run.revision.checked_add(1).ok_or(RunError::Conflict)?;
            if !pending && run.children.is_empty() { run.closure = Closure::Closed; run.closed_at = Some(publication.finished_at); }
            tx.execute("UPDATE runs SET record=?1 WHERE id=?2", params![bytes(&run)?, run.run_id.to_string()])?;
            if run.closure == Closure::Closed {
                let raw: Vec<u8> = tx.query_row("SELECT record FROM operations WHERE run=?1", [run.run_id.to_string()], |r| r.get(0))?;
                let mut op: OperationRecord = decode(&raw)?;
                op.state = OperationState::Terminal;
                tx.execute("UPDATE operations SET record=?1 WHERE run=?2", params![bytes(&op)?, run.run_id.to_string()])?;
            }
            #[cfg(test)]
            crate::tests::crash_at("transaction");
            Ok(())
        })?;
        #[cfg(test)]
        crate::tests::crash_at("committed");
        Ok(next)
    }
}
fn validate_request(input: &PreparedRun) -> Result<()> {
    if input.request.schema_version != 1 {
        return Err(RunError::Version(input.request.schema_version as i64));
    }
    if !(1..=64).contains(&input.request.tasks.len())
        || !(1..=8).contains(&input.request.concurrency)
        || input.attempts.len() != input.request.tasks.len()
    {
        return Err(RunError::Validation("task/concurrency bounds"));
    }
    bytes(&input.request)?;
    let mut tasks = std::collections::BTreeSet::new();
    let mut attempts = std::collections::BTreeSet::new();
    let mut prompt_bytes = 0u64;
    for task in &input.request.tasks {
        if !tasks.insert(task.task_id) {
            return Err(RunError::Validation("duplicate task"));
        }
        let new = input
            .attempts
            .iter()
            .find(|a| a.task_id == task.task_id)
            .ok_or(RunError::Validation("missing task"))?;
        if !attempts.insert(new.attempt_id)
            || task.provider_timeout_seconds == 0
            || task.fallback.len() > 2
            || CanonicalV1::file_digest(new.prompt.as_bytes()) != task.prompt_digest
        {
            return Err(RunError::Validation("attempt or prompt identity"));
        }
        prompt_bytes += new.prompt.len() as u64;
    }
    if prompt_bytes > RECORD_LIMIT {
        return Err(RunError::TooLarge {
            limit: RECORD_LIMIT,
        });
    }
    Ok(())
}
pub(crate) fn insert_event(db: &rusqlite::Connection, attempt: &AttemptRecord) -> Result<()> {
    let mut event = RunEvent {
        schema_version: 1,
        sequence: 0,
        project_id: attempt.project_id,
        run_id: attempt.run_id,
        attempt_id: attempt.attempt_id,
        revision: attempt.revision,
        change: attempt.state.clone(),
    };
    db.execute(
        "INSERT INTO events(project,attempt,revision,record) VALUES (?1,?2,?3,?4)",
        params![
            attempt.project_id.to_string(),
            attempt.attempt_id.to_string(),
            sql_int(attempt.revision)?,
            bytes(&event)?
        ],
    )?;
    event.sequence = u64::try_from(db.last_insert_rowid()).map_err(|_| RunError::Conflict)?;
    db.execute(
        "UPDATE events SET record=?1 WHERE sequence=?2",
        params![bytes(&event)?, sql_int(event.sequence)?],
    )?;
    Ok(())
}

// Supervisor owns this extensible v1 file. Only custody/exit fields consumed by
// run are decoded; the bounded codec still rejects duplicate members/version.
#[derive(serde::Deserialize)]
struct SealedExit {
    scope: provenant_supervisor::custody::Scope,
    termination: Option<Termination>,
    never_spawned: bool,
    terminal_observed: bool,
    unresolved_cleanup: bool,
    settlement_evidence: provenant_supervisor::custody::SettlementEvidence,
}
fn sealed_exit(
    dir: &provenant_supervisor::fs::Directory,
    exit: &ExitObservation,
) -> Result<ArtifactRecord> {
    use provenant_supervisor::{
        custody::{PublicationState, SettlementEvidence},
        fs::FileKind,
    };
    if !matches!(exit.publication, PublicationState::Durable)
        || exit.settlement_evidence == SettlementEvidence::Unresolved
    {
        return Err(RunError::Validation(
            "supervisor exit publication is unresolved",
        ));
    }
    let name = OsStr::new("process-exit.json");
    let mut file = dir.open_read(name)?;
    let meta = file.metadata()?;
    if meta.kind != FileKind::Regular || meta.links != 1 || meta.mode & 0o077 != 0 {
        return Err(RunError::UnsafePath);
    }
    let raw = file.read_bounded(RECORD_LIMIT)?;
    let stored: SealedExit = decode(&raw)?;
    if stored.scope != exit.scope
        || stored.termination != exit.termination
        || stored.never_spawned != exit.never_spawned
        || !stored.terminal_observed
        || stored.unresolved_cleanup
        || stored.settlement_evidence != exit.settlement_evidence
    {
        return Err(RunError::Validation(
            "supervisor exit record differs from observation",
        ));
    }
    file.sync()?;
    dir.sync()?;
    Ok(ArtifactRecord {
        path: artifact_path(exit.scope.attempt, "process-exit.json")?,
        digest: CanonicalV1::file_digest(&raw),
        bytes: raw.len() as u64,
        complete: true,
        redacted: true,
    })
}
