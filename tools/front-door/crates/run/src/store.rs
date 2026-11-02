use crate::{
    artifacts::{artifact_path, read_sealed, remove_temporary_best_effort, seal},
    db::{bytes, decode, sql_int},
    *,
};
use provenant_core::{AttemptId, OperationId, RunId, TaskId, WallTime};
use provenant_os::{exec::Termination, fs::LockMode};
use provenant_records::{AttemptFile, CanonicalV1};
use provenant_supervisor::custody::ExitObservation;
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
#[derive(Clone)]
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
    /// Provider session handle, kept so the attempt can be continued.
    pub session: Option<String>,
    /// Why the attempt is flagged as a partial finish, if it is.
    pub partial_signals: Vec<PartialSignal>,
}
impl RunStore {
    /// A result-only orphan predates durable terminal intent. The trusted
    /// provider/confinement edge must revalidate its terminal facts; the file's
    /// presence alone never establishes success, route identity or completeness.
    /// The supplied result rendering is ignored in favour of the sealed bytes.
    pub fn recover_result_only(
        &self,
        id: AttemptId,
        facts: &TerminalPublication<'_>,
        exit: &ExitObservation,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        let old = self.record(id, call)?;
        let dir = self.attempt_dir(old.run_id, id, false)?;
        if read_sealed(&dir, AttemptFile::TerminalPending.name(), RECORD_LIMIT)?.is_some() {
            return Err(RunError::TerminalRecoveryRequired);
        }
        let raw = read_sealed(&dir, AttemptFile::Result.name(), RESULT_LIMIT)?
            .ok_or(RunError::Nonterminal)?;
        let text = std::str::from_utf8(&raw)
            .map_err(|_| RunError::Validation("sealed result is not UTF-8"))?;
        let publication = TerminalPublication {
            result: text,
            // The recomputed rendering is not the orphan's bytes. Preserve
            // its identity without claiming unsafe bytes were redacted.
            redacted: facts.redacted
                && provenant_confinement::redact::known_shapes(&raw).is_ok_and(|safe| safe == raw),
            ..facts.clone()
        };
        self.publish_terminal(id, old.revision, &publication, exit, call)
    }
    /// Recover an interrupted terminal publication from sealed evidence.
    pub fn recover_terminal(
        &self,
        id: AttemptId,
        exit: &ExitObservation,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        let old = self.record(id, call)?;
        let dir = self.attempt_dir(old.run_id, id, false)?;
        let pending: AttemptRecord = crate::db::decode_attempt(
            &read_sealed(&dir, AttemptFile::TerminalPending.name(), RECORD_LIMIT)?
                .ok_or(RunError::TerminalRecoveryRequired)?,
        )?;
        super::reader::validate_attempt(&pending)?;
        if pending.attempt_id != id
            || pending.run_id != old.run_id
            || pending.project_id != old.project_id
            || pending.task_id != old.task_id
            || pending.operation_id != old.operation_id
            || pending.generation != old.generation
        {
            return Err(RunError::Conflict);
        }
        if sealed_exit(&dir, exit)? != pending.exit_evidence.ok_or(RunError::Conflict)? {
            return Err(RunError::Conflict);
        }
        self.replay_terminal(id, call)
    }
    /// Commit the exact scoped decision sealed before a failed transaction.
    pub fn replay_terminal(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptRecord> {
        let current = self.record(id, call)?;
        let run = self.run_dir(current.run_id, false)?;
        let guard = run.lock(
            OsStr::new("publication.lock"),
            LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        let (record, committed) = self.replay_terminal_locked(id, call)?;
        drop(guard);
        if committed {
            self.settle_cleanup(&record, call);
        }
        Ok(record)
    }
    /// Replay with `publication.lock` already held. The flag reports whether
    /// this call committed the row, so the caller owes post-commit cleanup.
    fn replay_terminal_locked(
        &self,
        id: AttemptId,
        call: &Request<'_>,
    ) -> Result<(AttemptRecord, bool)> {
        let current = self.record(id, call)?;
        let dir = self.attempt_dir(current.run_id, id, false)?;
        let pending: AttemptRecord = crate::db::decode_attempt(
            &read_sealed(&dir, AttemptFile::TerminalPending.name(), RECORD_LIMIT)?
                .ok_or(RunError::TerminalRecoveryRequired)?,
        )?;
        super::reader::validate_attempt(&pending)?;
        if pending.attempt_id != id
            || pending.run_id != current.run_id
            || pending.project_id != current.project_id
            || pending.task_id != current.task_id
            || pending.operation_id != current.operation_id
            || pending.generation != current.generation
            || !matches!(pending.state, AttemptState::Terminal { .. })
            || pending.settlement_receipts.is_some()
            || pending.custody_pending.is_some()
        {
            return Err(RunError::Conflict);
        }
        if let Some(result) = &pending.result {
            self.artifact_bytes(current.run_id, result, RESULT_LIMIT)?;
        }
        if let Some(exit) = &pending.exit_evidence {
            let raw = self.artifact_bytes(current.run_id, exit, RECORD_LIMIT)?;
            let stored: SealedExit = decode(&raw)?;
            let AttemptState::Terminal { exit_code, .. } = pending.state else {
                return Err(RunError::Conflict);
            };
            let observed = match stored.termination {
                Some(Termination::Exit(code)) => Some(code),
                _ => None,
            };
            if stored.scope != Self::scope(&current)
                || stored.unresolved_cleanup
                || stored.settlement_evidence
                    == provenant_supervisor::custody::SettlementEvidence::Unresolved
                || observed != exit_code
                || pending.provider_launched != Some(!stored.never_spawned)
            {
                return Err(RunError::Conflict);
            }
            if pending.schema_version == 2 {
                let AttemptState::Terminal { outcome, .. } = pending.state else {
                    return Err(RunError::Conflict);
                };
                if (outcome == Outcome::Cancelled
                    && !stored.never_spawned
                    && stored.cancel_delivered_before_exit != Some(true))
                    || (outcome == Outcome::Cancelled
                        && stored.never_spawned
                        && stored.stop_reason
                            != Some(
                                provenant_supervisor::machine::StopReason::ExplicitCancellation,
                            ))
                    || (stored.cancel_delivered_before_exit == Some(true)
                        && outcome != Outcome::Cancelled)
                {
                    return Err(RunError::Conflict);
                }
            }
        }
        if matches!(current.state, AttemptState::Terminal { .. }) {
            let mut decision = current.clone();
            decision.settlement_receipts = None;
            decision.custody_pending = None;
            return if decision == pending {
                Ok((current, false))
            } else {
                Err(RunError::Conflict)
            };
        }
        if current.revision.checked_add(1) != Some(pending.revision) {
            return Err(RunError::Conflict);
        }
        self.commit_terminal_record(&current, &pending, call)?;
        Ok((self.record(id, call)?, true))
    }
    /// Persist one independently retryable effect without modifying the decision.
    pub fn record_effect(
        &self,
        id: AttemptId,
        generation: u64,
        effect: SettlementEffect,
        disposition: EffectDisposition,
        call: &Request<'_>,
    ) -> Result<SettlementReceipts> {
        if let EffectDisposition::Failed { reason, .. } = &disposition
            && (reason.is_empty() || reason.len() > 4096)
        {
            return Err(RunError::Validation("invalid effect failure reason"));
        }
        let receipts = self.write(call, |tx| {
            let raw: Vec<u8> = tx.query_row("SELECT record FROM attempts WHERE id=?1 AND project=?2 AND terminal=1", params![id.to_string(), self.context.project_id.to_string()], |row| row.get(0)).optional()?.ok_or(RunError::NotFound)?;
            let attempt: AttemptRecord = crate::db::decode_attempt(&raw)?;
            if attempt.generation != generation { return Err(RunError::Conflict); }
            let existing: Option<(i64, Vec<u8>)> = tx.query_row("SELECT generation,record FROM settlement_receipts WHERE attempt=?1", [id.to_string()], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
            let mut receipts: SettlementReceipts = match existing {
                Some((saved, raw)) if saved == sql_int(generation)? => crate::db::decode_unversioned(&raw)?,
                Some(_) => return Err(RunError::Conflict),
                None => SettlementReceipts::default(),
            };
            let slot = match effect {
                SettlementEffect::GitImport => &mut receipts.git_import,
                SettlementEffect::ManifestConflict => &mut receipts.manifest_conflict,
                SettlementEffect::LockTimeout => &mut receipts.lock_timeout,
                SettlementEffect::CleanupCustodyRelease => &mut receipts.cleanup_custody_release,
                SettlementEffect::Delivery => &mut receipts.delivery,
            };
            if matches!(slot, EffectDisposition::Done) && *slot != disposition { return Err(RunError::Conflict); }
            *slot = disposition.clone();
            tx.execute("INSERT INTO settlement_receipts(attempt,generation,record) VALUES (?1,?2,?3) ON CONFLICT(attempt) DO UPDATE SET record=excluded.record", params![id.to_string(), sql_int(generation)?, bytes(&receipts)?])?;
            Ok(receipts)
        })?;
        provenant_os::changes::hint(
            self.context.state_root.as_path(),
            &[provenant_os::changes::Scope::Board],
        );
        Ok(receipts)
    }
    /// Record whether a cancellation was effective, without rewriting the decision.
    pub fn record_cancellation(
        &self,
        id: AttemptId,
        generation: u64,
        disposition: CancellationDisposition,
        call: &Request<'_>,
    ) -> Result<()> {
        self.write(call, |tx| {
            let raw: Vec<u8> = tx.query_row("SELECT record FROM attempts WHERE id=?1 AND project=?2 AND terminal=1", params![id.to_string(), self.context.project_id.to_string()], |row| row.get(0)).optional()?.ok_or(RunError::NotFound)?;
            let attempt = crate::db::decode_attempt(&raw)?;
            if attempt.generation != generation { return Err(RunError::Conflict); }
            if disposition == CancellationDisposition::Effective && attempt.cancellation_disposition != Some(CancellationDisposition::Effective) { return Err(RunError::Conflict); }
            let raw: Option<Vec<u8>> = tx.query_row("SELECT record FROM settlement_receipts WHERE attempt=?1 AND generation=?2", params![id.to_string(), sql_int(generation)?], |row| row.get(0)).optional()?;
            let mut receipts: SettlementReceipts = raw.as_deref().map(crate::db::decode_unversioned).transpose()?.unwrap_or_default();
            if disposition == CancellationDisposition::AcknowledgedIneffective {
                receipts.late_cancellation_acknowledged = true;
            }
            if receipts.cancellation.is_none() || disposition == CancellationDisposition::Effective {
                receipts.cancellation = Some(disposition);
            }
            let changed = tx.execute("INSERT INTO settlement_receipts(attempt,generation,record) VALUES (?1,?2,?3) ON CONFLICT(attempt) DO UPDATE SET record=excluded.record WHERE generation=excluded.generation", params![id.to_string(), sql_int(generation)?, bytes(&receipts)?])?;
            if changed != 1 { return Err(RunError::Conflict); }
            Ok(())
        })?;
        provenant_os::changes::hint(
            self.context.state_root.as_path(),
            &[provenant_os::changes::Scope::Board],
        );
        Ok(())
    }
    /// Seal an inconclusive terminal decision without inventing an exit.
    /// Custody and its fence remain independent of this decision.
    pub fn settle_unsettled(
        &self,
        id: AttemptId,
        expected_revision: u64,
        reason: &str,
        finished_at: WallTime,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        if reason.is_empty() || reason.len() > 4096 {
            return Err(RunError::Validation("invalid unsettled reason"));
        }
        let current = self.record(id, call)?;
        let run = self.run_dir(current.run_id, false)?;
        let guard = run.lock(
            OsStr::new("publication.lock"),
            LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        let current = self.record(id, call)?;
        if matches!(current.state, AttemptState::Terminal { .. })
            || current.revision != expected_revision
        {
            return Err(RunError::Conflict);
        }
        let mut next = current.clone();
        next.schema_version = 2;
        next.revision = expected_revision.checked_add(1).ok_or(RunError::Conflict)?;
        next.terminal_evidence = Some(TerminalEvidence::Unsettled {
            reason: reason.into(),
        });
        next.state = AttemptState::Terminal {
            extra: Default::default(),
            outcome: Outcome::Unsettled,
            exit_code: None,
            finished_at,
            reason: Some(reason.into()),
        };
        super::reader::validate_attempt(&next)?;
        let attempt = self.attempt_dir(current.run_id, id, false)?;
        let intent = bytes(&next)?;
        if let Some(saved) =
            read_sealed(&attempt, AttemptFile::TerminalPending.name(), RECORD_LIMIT)?
        {
            if saved != intent {
                let name = format!(
                    "terminal-pending.conflict-{}.json",
                    provenant_core::hex(CanonicalV1::file_digest(&intent).bytes())
                );
                seal(&attempt, &name, &intent, RECORD_LIMIT)?;
                return Err(RunError::TerminalRecoveryRequired);
            }
        } else {
            seal(
                &attempt,
                AttemptFile::TerminalPending.name(),
                &intent,
                RECORD_LIMIT,
            )?;
        }
        self.commit_terminal_record(&current, &next, call)?;
        drop(guard);
        self.record(id, call)
    }
    /// Reserve records first, seal the exact request/prompts, then acknowledge.
    /// Repeating a prepared operation repairs a publication gap with the same
    /// request. No scheduler, admission or launch is implied by acceptance here.
    pub fn create(&self, input: &PreparedRun, call: &Request<'_>) -> Result<RunRecord> {
        validate_request(input)?;
        // A crash before sealing left the operation prepared under its own
        // IDs; a replay with fresh IDs adopts them rather than conflicting.
        if let Some(op) = self.operation(&input.operation_id, call)?
            && op.state == OperationState::Prepared
            && op.request_digest == input.request.digest()?
            && op.run_id != input.run_id
        {
            return self.create_exact(&self.adopt(op.run_id, input, call)?, call);
        }
        self.create_exact(input, call)
    }
    fn adopt(&self, run: RunId, input: &PreparedRun, call: &Request<'_>) -> Result<PreparedRun> {
        call.check()?;
        let ids: Vec<(String, String)> = {
            let mut query = self
                .db
                .prepare("SELECT task,id FROM attempts WHERE run=?1 AND project=?2")?;
            let rows = query.query_map(
                params![run.to_string(), self.context.project_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let attempts = input
            .attempts
            .iter()
            .map(|new| {
                let (_, id) = ids
                    .iter()
                    .find(|(task, _)| *task == new.task_id.to_string())
                    .ok_or(RunError::Conflict)?;
                Ok(NewAttempt {
                    attempt_id: AttemptId::parse(id)
                        .map_err(|_| RunError::Validation("attempt index ID"))?,
                    task_id: new.task_id,
                    prompt: new.prompt.clone(),
                })
            })
            .collect::<Result<_>>()?;
        Ok(PreparedRun {
            run_id: run,
            operation_id: input.operation_id.clone(),
            request: input.request.clone(),
            attempts,
            created_at: input.created_at,
        })
    }
    fn create_exact(&self, input: &PreparedRun, call: &Request<'_>) -> Result<RunRecord> {
        // Configuration and volume probes happen before the store transaction.
        let disk_floor_bytes = provenant_config::GcSettings::read(&self.writer_registry)?
            .free_floor_bytes(&self.writer_registry)?;
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
            extra: Default::default(),
            schema_version: 1,
            run_id: input.run_id,
            project_id: context.project_id,
            actor_id: context.actor_id,
            operation_id: input.operation_id.clone(),
            request_digest: digest,
            task_ids: input.request.tasks.iter().map(|t| t.task_id).collect(),
            parent: input.request.parent,
            parent_attempt: input.request.parent_attempt,
            children: vec![],
            revision: 1,
            generation: 0,
            created_at: input.created_at,
            closed_at: None,
            closure: Closure::Open,
        };
        let operation = OperationRecord {
            extra: Default::default(),
            schema_version: 1,
            project_id: context.project_id,
            actor_id: context.actor_id,
            operation_id: input.operation_id.clone(),
            kind: if input.request.parent.is_some() {
                OperationKind::Resume
            } else {
                OperationKind::Dispatch
            },
            request_digest: digest,
            run_id: input.run_id,
            state: OperationState::Prepared,
        };
        let dir = self.run_dir(input.run_id, true)?;
        let _writer = dir.lock(
            OsStr::new("publication.lock"),
            LockMode::Exclusive,
            call.lock_budget(),
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
            let queued: i64 = tx.query_row("SELECT COUNT(*) FROM admission WHERE active=0", [], |r| r.get(0))?;
            if queued + input.request.tasks.len() as i64 > sql_int(crate::admission::HOST_QUEUED)? { return Err(RunError::Capacity); }
            tx.execute("INSERT INTO operations VALUES (?1,?2,?3,?4,?5,?6)", params![context.project_id.to_string(), context.actor_id.to_string(), input.operation_id.as_str(), digest.to_string(), input.run_id.to_string(), bytes(&operation)?])?;
            tx.execute("INSERT INTO runs VALUES (?1,?2,?3)", params![input.run_id.to_string(), context.project_id.to_string(), bytes(&run)?])?;
            if let Some(target) = input.request.parent {
                // Each terminal attempt is continued at most once.
                let taken = tx.execute("INSERT INTO continuations(target,run) VALUES (?1,?2) ON CONFLICT DO NOTHING", params![target.to_string(), input.run_id.to_string()])?;
                if taken != 1 { return Err(RunError::Conflict); }
            }
            // A continuation is the next attempt of the prior task.
            let number: i64 = match input.request.parent {
                None => 1,
                Some(target) => {
                    let (task, number): (String, i64) = tx.query_row("SELECT task,number FROM attempts WHERE id=?1 AND project=?2", params![target.to_string(), context.project_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?))).optional()?.ok_or(RunError::NotFound)?;
                    if input.request.tasks.iter().any(|t| t.task_id.to_string() != task) { return Err(RunError::Validation("continuation must keep its task")); }
                    number.checked_add(1).ok_or(RunError::Validation("attempt number"))?
                }
            };
            for (index, task) in input.request.tasks.iter().enumerate() {
                let new = input.attempts.iter().find(|a| a.task_id == task.task_id).ok_or(RunError::Validation("missing task prompt"))?;
                let attempt = AttemptRecord {extra: Default::default(),
                    context: task.context.clone(),
                    title: task.title.clone(),
                    continuation: input.request.fresh_continuation.as_ref().map(|_| "fresh".into()),
                    profile: task.profile.clone(),
                    schema_version: 2, attempt_id: new.attempt_id, run_id: run.run_id, project_id: run.project_id, task_id: task.task_id, operation_id: run.operation_id.clone(), attempt_number: u64::try_from(number).map_err(|_| RunError::Validation("attempt number"))?, revision: 1, generation: 0,
                    delivery_id: task.delivery_id, retry_of: input.request.parent, requested_route: task.route.clone(), resolved_route: None, applied_route: None, observed_route: None, authority_digest: task.authority_digest, read_only_writable: Vec::new(), write_grants: Vec::new(), git_grants_digest: None, granted_tracking_ref: None, subject: None, head: None, protected_paths: Vec::new(), delivery: None, verified_owner: Some(context.actor_id), session: None,
                queued_at: input.created_at, started_at: None, priority: None, requested_priority: None, last_output_at: None, provider_silent_for_ms: provenant_records::run::Transient(None), process_observation_valid: provenant_records::run::Transient(false), provider_silence_suppressed: provenant_records::run::Transient(false), provider_launched: None, provider_ms: None, admission_deadline: None, runtime_deadline: None, partial_signals: Vec::new(), state: AttemptState::Queued { extra: Default::default() }, usage: UsageRecord::default(),
                    terminal_evidence: None, cancellation_disposition: None, settlement_receipts: None, custody_pending: None,
                    prompt: ArtifactRecord {extra: Default::default(),  path: artifact_path(new.attempt_id, AttemptFile::Prompt.name())?, digest: task.prompt_digest, bytes: new.prompt.len() as u64, complete: true, redacted: false }, result: None, publication_error: None, exit_evidence: None, skipped_routes: Vec::new(),
                    trace: provenant_records::Trace {extra: Default::default(),
                        host: self.host,
                        project: Some(run.project_id),
                        run: Some(run.run_id),
                        attempt: Some(new.attempt_id),
                        operation: Some(run.operation_id.clone()),
                        actor: Some(run.actor_id),
                        parent: input.request.parent.or(input.request.parent_attempt),
                    },
                };
                tx.execute("INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) VALUES (?1,?2,?3,?4,?5,1,0,?6)", params![attempt.attempt_id.to_string(), run.run_id.to_string(), run.project_id.to_string(), task.task_id.to_string(), number, bytes(&attempt)?])?;
                insert_event(tx, &attempt)?;
                let waves = index as u64 / input.request.concurrency.max(1);
                let class = if task.writer.is_some() { LaneClass::Writer } else { LaneClass::ReadOnly };
                self.queue_ticket(tx, &attempt, class, input.request.concurrency, crate::admission::Allowance { queue_seconds: waves.saturating_mul(task.provider_timeout_seconds), runtime_seconds: task.provider_timeout_seconds }, disk_floor_bytes)?;
            }
            Ok(())
        })?;
        provenant_os::changes::hint(
            self.context.state_root.as_path(),
            &[provenant_os::changes::Scope::Board],
        );
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
                AttemptFile::Prompt.name(),
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
        let writer = dir.lock(
            OsStr::new("publication.lock"),
            LockMode::Exclusive,
            call.lock_budget(),
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
            || exit.unresolved_cleanup
        {
            return Err(RunError::Validation(
                "exit/cleanup evidence is unresolved or out of scope",
            ));
        }
        if matches!(
            publication.outcome,
            Outcome::Succeeded | Outcome::ReviewRequired
        ) && (outcome_exit != Some(0) || exit.never_spawned)
        {
            return Err(RunError::Validation("success lacks verified provider exit"));
        }
        if publication.outcome == Outcome::Cancelled
            && !exit.never_spawned
            && exit.cancel_delivered_before_exit != Some(true)
        {
            return Err(RunError::Validation(
                "cancellation delivery before exit is unproven",
            ));
        }
        if publication.outcome == Outcome::Cancelled
            && exit.never_spawned
            && exit.stop_reason
                != Some(provenant_supervisor::machine::StopReason::ExplicitCancellation)
        {
            return Err(RunError::Validation("prelaunch cancellation is unproven"));
        }
        if exit.cancel_delivered_before_exit == Some(true)
            && publication.outcome != Outcome::Cancelled
        {
            return Err(RunError::Validation(
                "effective cancellation contradicts outcome",
            ));
        }
        let result = self.commit_terminal(
            old,
            expected_revision,
            publication,
            Some(durable_exit),
            outcome_exit,
            !exit.never_spawned,
            exit.cancel_delivered_before_exit,
            call,
        );
        drop(writer);
        if let Ok(record) = &result {
            self.settle_cleanup(record, call);
        }
        result
    }
    /// End an attempt that never reached a provider: refused before spawn or
    /// cancelled while queued. It has no custody to release and no exit.
    /// `expected` names the state the caller observed; any other state is a
    /// conflict, so a cancelled queued attempt never races a starting launch.
    pub fn withdraw(
        &self,
        id: AttemptId,
        expected: AttemptState,
        outcome: Outcome,
        reason: &str,
        finished_at: WallTime,
        call: &Request<'_>,
    ) -> Result<Option<AttemptRecord>> {
        if !matches!(outcome, Outcome::Rejected | Outcome::Cancelled) {
            return Err(RunError::Validation("withdrawal outcome"));
        }
        let old = self.record(id, call)?;
        let dir = self.run_dir(old.run_id, false)?;
        let writer = dir.lock(
            OsStr::new("publication.lock"),
            LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        let old = self.record(id, call)?;
        if let AttemptState::Terminal { .. } = old.state {
            return Ok(None);
        }
        if old.state != expected
            || !matches!(
                old.state,
                AttemptState::Queued { .. } | AttemptState::Starting { .. }
            )
        {
            return Err(RunError::Conflict);
        }
        let revision = old.revision;
        let attempt_dir = self.attempt_dir(old.run_id, id, true)?;
        // An interrupted withdrawal resumes its own sealed facts, so a retry
        // never renders a second finished_at or reason.
        let (outcome, finished_at, reason) = match read_sealed(
            &attempt_dir,
            AttemptFile::TerminalPending.name(),
            RECORD_LIMIT,
        )? {
            Some(raw) => {
                let saved: AttemptRecord = crate::db::decode_attempt(&raw)?;
                super::reader::validate_attempt(&saved)?;
                match saved.state {
                    AttemptState::Terminal {
                        outcome: saved_outcome @ (Outcome::Rejected | Outcome::Cancelled),
                        exit_code: None,
                        finished_at,
                        reason,
                        ..
                    } if saved.attempt_id == id
                        && saved.run_id == old.run_id
                        && saved.generation == old.generation
                        && saved.exit_evidence.is_none()
                        && saved.provider_launched == Some(false)
                        && Some(saved.revision) == revision.checked_add(1) =>
                    {
                        (saved_outcome, finished_at, reason)
                    }
                    _ => return Err(RunError::TerminalRecoveryRequired),
                }
            }
            None => (
                outcome,
                finished_at,
                Some(reason.chars().take(256).collect()),
            ),
        };
        let publication = TerminalPublication {
            outcome,
            finished_at,
            started_at: None,
            provider_ms: None,
            reason,
            result: "",
            complete: true,
            redacted: false,
            resolved_route: None,
            applied_route: None,
            observed_route: None,
            usage: UsageRecord::default(),
            session: None,
            partial_signals: Vec::new(),
        };
        let result = self
            .commit_terminal(old, revision, &publication, None, None, false, None, call)
            .map(Some);
        drop(writer);
        if let Ok(Some(record)) = &result {
            self.settle_cleanup(record, call);
        }
        result
    }
    fn settle_cleanup(&self, record: &AttemptRecord, call: &Request<'_>) {
        // The terminal record retains tmp before cleanup; clear that hint only
        // after removal succeeds.
        if let Ok(attempt_dir) = self.attempt_dir(record.run_id, record.attempt_id, false) {
            // A resource refusal has no graph certificate. Keep all private
            // evidence until its retained writer reservation is recovered.
            match crate::holder::object_import_refused(&attempt_dir, Self::scope(record)) {
                Ok(None) => {}
                Ok(Some(_)) | Err(_) => {
                    let _ = self.set_temporary_retained(record.attempt_id, true, call);
                    return;
                }
            }
            let _ = crate::retention::prune_caches(
                &attempt_dir,
                record,
                false,
                &mut ProviderStateGc::default(),
            );
            let _ = self.set_temporary_retained(record.attempt_id, true, call);
            if remove_temporary_best_effort(&attempt_dir).is_ok() {
                let _ = self.set_temporary_retained(record.attempt_id, false, call);
            }
        }
    }
    /// Seal result and terminal intent, then commit the attempt, run summary,
    /// operation state, released admission and event together. The caller
    /// holds the run's publication lock.
    #[expect(
        clippy::too_many_arguments,
        reason = "one commit point for every terminal path"
    )]
    fn commit_terminal(
        &self,
        old: AttemptRecord,
        expected_revision: u64,
        publication: &TerminalPublication<'_>,
        durable_exit: Option<ArtifactRecord>,
        outcome_exit: Option<i64>,
        launched: bool,
        cancellation_delivered: Option<bool>,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        let id = old.attempt_id;
        if publication.result.len() as u64 > RESULT_LIMIT {
            return Err(RunError::TooLarge {
                limit: RESULT_LIMIT,
            });
        }
        if matches!(
            publication.outcome,
            Outcome::Succeeded | Outcome::ReviewRequired
        ) && (!publication.complete
            || publication.result.is_empty()
            || !super::reader::verified_route(
                publication.resolved_route.as_ref(),
                publication.applied_route.as_ref(),
                publication.observed_route.as_ref(),
            ))
        {
            return Err(RunError::Validation(
                "success lacks complete result or verified provider identity",
            ));
        }
        let mut next = old.clone();
        next.revision = expected_revision.checked_add(1).ok_or(RunError::Conflict)?;
        next.started_at = publication.started_at;
        next.provider_launched = Some(launched);
        next.provider_ms = publication.provider_ms;
        next.resolved_route = publication.resolved_route.clone();
        next.applied_route = publication.applied_route.clone();
        next.observed_route = publication.observed_route.clone();
        next.usage = publication.usage.clone();
        next.partial_signals
            .clone_from(&publication.partial_signals);
        next.session = publication.session.clone();
        next.result = Some(ArtifactRecord {
            extra: Default::default(),
            path: artifact_path(id, AttemptFile::Result.name())?,
            digest: CanonicalV1::file_digest(publication.result.as_bytes()),
            bytes: publication.result.len() as u64,
            complete: publication.complete,
            redacted: publication.redacted,
        });
        next.state = AttemptState::Terminal {
            extra: Default::default(),
            outcome: publication.outcome,
            exit_code: outcome_exit,
            finished_at: publication.finished_at,
            reason: publication.reason.clone(),
        };
        next.schema_version = 2;
        next.terminal_evidence = Some(if publication.outcome == Outcome::Unsettled {
            TerminalEvidence::Unsettled {
                reason: publication
                    .reason
                    .clone()
                    .ok_or(RunError::Validation("unsettled reason missing"))?,
            }
        } else if publication.outcome == Outcome::Cancelled && cancellation_delivered == Some(true)
        {
            TerminalEvidence::EffectiveCancellation
        } else if durable_exit.is_some() {
            TerminalEvidence::ProcessExit
        } else {
            TerminalEvidence::PreLaunchRefusal
        });
        next.publication_error = None;
        next.exit_evidence = durable_exit;
        let attempt = self.attempt_dir(old.run_id, id, false)?;
        if publication.outcome == Outcome::Cancelled
            && (!launched || cancellation_delivered == Some(true))
        {
            next.cancellation_disposition = Some(CancellationDisposition::Effective);
        } else if let Some(raw) = read_sealed(&attempt, "cancel.json", RECORD_LIMIT)? {
            if let Ok(request) = decode::<CancellationEvidence>(&raw) {
                if request.scope == Self::scope(&old)
                    && request.requester == old.verified_owner.unwrap_or(self.context.actor_id)
                    && request.reason
                        == provenant_supervisor::machine::StopReason::ExplicitCancellation
                {
                    next.cancellation_disposition = Some(match cancellation_delivered {
                        Some(true) => CancellationDisposition::Effective,
                        Some(false) => CancellationDisposition::AcknowledgedIneffective,
                        None => CancellationDisposition::Unknown,
                    });
                }
            } else {
                next.cancellation_disposition = Some(CancellationDisposition::Unknown);
            }
        }
        if let Some(raw) = read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)? {
            let writer: crate::writer::WriterTerminal = decode(&raw)?;
            next.head = Some(writer.head);
            next.protected_paths = writer.protected_paths;
        }
        if publication.outcome == Outcome::Succeeded {
            // Recovery reports uncertainty from the sealed start marker without
            // repeating an external effect or inferring success from prose.
            let raw = match read_sealed(&attempt, "delivery.json", RECORD_LIMIT)? {
                Some(raw) => Some(raw),
                None => read_sealed(&attempt, "delivery-started.json", RECORD_LIMIT)?,
            };
            if let Some(raw) = raw {
                let receipt: WriterDeliveryReceipt = decode(&raw)?;
                next.head = receipt.head;
                next.delivery = Some(receipt);
            } else if let Some(deliver) = self
                .request(old.run_id, call)?
                .tasks
                .into_iter()
                .find(|task| task.task_id == old.task_id)
                .and_then(|task| task.deliver)
            {
                // Requested delivery that never ran must not read as success
                // with no trace.
                next.delivery = Some(WriterDeliveryReceipt {
                    extra: Default::default(),
                    schema_version: 1,
                    remote: deliver.remote,
                    branch: deliver.branch,
                    head: None,
                    review_acceptance: None,
                    state: WriterDeliveryState::NotAttempted {
                        extra: Default::default(),
                    },
                });
            }
        }
        if matches!(old.state, AttemptState::Terminal { .. }) {
            if old == next {
                // The caller releases publication.lock before settlement cleanup.
                return Ok(old);
            }
            return Err(RunError::Conflict);
        }
        if old.revision != expected_revision {
            return Err(RunError::Conflict);
        }
        super::reader::validate_attempt(&next)?;
        let intent = bytes(&next)?;
        let attempt_dir = self.attempt_dir(old.run_id, id, false)?;
        // Seal the result, then the exact terminal intent that names it. A
        // crash between them leaves a result-only orphan, which the edge
        // recovers with revalidated facts; intent never names missing bytes.
        match read_sealed(
            &attempt_dir,
            AttemptFile::TerminalPending.name(),
            RECORD_LIMIT,
        )? {
            Some(saved) if saved != intent => {
                let name = format!(
                    "terminal-pending.conflict-{}.json",
                    provenant_core::hex(CanonicalV1::file_digest(&intent).bytes())
                );
                seal(&attempt_dir, &name, &intent, RECORD_LIMIT)?;
                return Err(RunError::TerminalRecoveryRequired);
            }
            Some(_) => {}
            None => {
                if let Some(saved) =
                    read_sealed(&attempt_dir, AttemptFile::Result.name(), RESULT_LIMIT)?
                    && saved != publication.result.as_bytes()
                {
                    let name = format!(
                        "result.conflict-{}.txt",
                        provenant_core::hex(
                            CanonicalV1::file_digest(publication.result.as_bytes()).bytes()
                        )
                    );
                    seal(
                        &attempt_dir,
                        &name,
                        publication.result.as_bytes(),
                        RESULT_LIMIT,
                    )?;
                    // The conflicting rendering is retained above; sealing the
                    // original path next reports the typed mismatch.
                }
                seal(
                    &attempt_dir,
                    AttemptFile::Result.name(),
                    publication.result.as_bytes(),
                    RESULT_LIMIT,
                )?;
                #[cfg(test)]
                crate::tests::crash_at("result");
                seal(
                    &attempt_dir,
                    AttemptFile::TerminalPending.name(),
                    &intent,
                    RECORD_LIMIT,
                )?;
            }
        }
        #[cfg(test)]
        crate::tests::crash_at("sealed");
        self.commit_terminal_record(&old, &next, call)?;
        #[cfg(test)]
        crate::tests::crash_at("committed");
        // Return the projection readers see, including the receipt row.
        self.record(id, call)
    }

    /// A reboot proves loss without inventing provider output or exit facts.
    /// The caller holds both exclusions until this transaction and release.
    pub(crate) fn commit_reboot_lost(
        &self,
        proof: &provenant_supervisor::custody::RebootRecovery,
        call: &Request<'_>,
    ) -> Result<()> {
        self.commit_lost(
            proof.scope(),
            "machine rebooted before the guardian settled the attempt",
            call,
        )
    }

    pub(crate) fn commit_lost(
        &self,
        scope: provenant_supervisor::custody::Scope,
        reason: &str,
        call: &Request<'_>,
    ) -> Result<()> {
        let directory = self.run_dir(scope.run, false)?;
        let _publication = directory.lock(
            OsStr::new("publication.lock"),
            LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        let old = self.record(scope.attempt, call)?;
        if Self::scope(&old) != scope {
            return Err(RunError::Conflict);
        }
        if matches!(old.state, AttemptState::Terminal { .. }) {
            return Ok(());
        }
        let mut next = old.clone();
        next.revision = old.revision.checked_add(1).ok_or(RunError::Conflict)?;
        next.schema_version = 2;
        next.terminal_evidence = Some(TerminalEvidence::Lost {
            reason: reason.into(),
        });
        next.state = AttemptState::Terminal {
            extra: Default::default(),
            outcome: Outcome::Lost,
            exit_code: None,
            finished_at: call
                .clock
                .wall_now()
                .map_err(|_| RunError::Validation("wall clock"))?,
            reason: Some(reason.into()),
        };
        super::reader::validate_attempt(&next)?;
        let attempt = self.attempt_dir(old.run_id, old.attempt_id, false)?;
        let name = AttemptFile::TerminalPending.name();
        let next = match read_sealed(&attempt, name, RECORD_LIMIT)? {
            Some(raw) => {
                let saved: AttemptRecord = crate::db::decode_attempt(&raw)?;
                super::reader::validate_attempt(&saved)?;
                // Any scoped decision already sealed for this generation wins.
                // A non-lost decision is replayed as itself, never as lost, and
                // a mismatch is a conflict that keeps the sealed evidence.
                if matches!(
                    saved.state,
                    AttemptState::Terminal { outcome, .. } if outcome != Outcome::Lost
                ) {
                    let (record, committed) = self.replay_terminal_locked(old.attempt_id, call)?;
                    if committed {
                        self.settle_cleanup(&record, call);
                    }
                    return Ok(());
                }
                if saved.attempt_id != old.attempt_id
                    || saved.run_id != old.run_id
                    || saved.project_id != old.project_id
                    || saved.task_id != old.task_id
                    || saved.operation_id != old.operation_id
                    || saved.generation != old.generation
                    || saved.revision != next.revision
                    || !matches!(
                        saved.state,
                        AttemptState::Terminal {
                            outcome: Outcome::Lost,
                            ..
                        }
                    )
                {
                    return Err(RunError::Conflict);
                }
                if let Some(result) = &saved.result {
                    self.artifact_bytes(old.run_id, result, RESULT_LIMIT)?;
                }
                if let Some(exit) = &saved.exit_evidence {
                    self.artifact_bytes(old.run_id, exit, RECORD_LIMIT)?;
                }
                saved
            }
            None => {
                seal(&attempt, name, &bytes(&next)?, RECORD_LIMIT)?;
                next
            }
        };
        // Keep recovery discoverable until fence release, even for readers
        // whose reservations live outside the writer registry. Reconcile
        // recognises this lost state and preserves scratch on every retry.
        self.commit_terminal_record(&old, &next, call)
    }

    /// Run the post-commit Git pin cleanup of a committed terminal attempt and
    /// persist the result: `Failed{retryable}` on error, `Done` on success.
    /// Safe to repeat; callers retry while the receipt is not `Done`.
    pub fn retry_git_cleanup(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<SettlementReceipts> {
        let request = self.request(record.run_id, call)?;
        let writer = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .and_then(|task| task.writer.as_ref())
            .ok_or(RunError::Validation("attempt has no writer to clean up"))?;
        let cleanup = (|| -> Result<()> {
            let attempt = self.attempt_dir(record.run_id, record.attempt_id, false)?;
            let head = crate::settlement::pinned_head(&attempt, Self::scope(record))?;
            crate::writer::cleanup_git_pins(
                self.context.state_root.as_path(),
                writer,
                &attempt,
                Self::scope(record),
                head,
            )
        })();
        let disposition = match cleanup {
            Ok(()) => EffectDisposition::Done,
            Err(error) => EffectDisposition::Failed {
                reason: error.to_string().chars().take(1024).collect(),
                retryable: true,
            },
        };
        self.record_effect(
            record.attempt_id,
            record.generation,
            SettlementEffect::CleanupCustodyRelease,
            disposition,
            call,
        )
    }

    fn commit_terminal_record(
        &self,
        old: &AttemptRecord,
        next: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<()> {
        let AttemptState::Terminal { finished_at, .. } = next.state else {
            return Err(RunError::Validation("terminal state missing"));
        };
        let id = old.attempt_id;
        let request = self.request(old.run_id, call)?;
        let successor = self.policy_successor(old, next, call)?;
        let disk_floor_bytes = if successor.is_some() {
            provenant_config::GcSettings::read(&self.writer_registry)?
                .free_floor_bytes(&self.writer_registry)?
        } else {
            0
        };
        let staged = successor.as_ref().map(|(next, _)| {
            next.continuation
                .clone()
                .unwrap_or_else(|| "successor".into())
        });
        let reaction = self.freeze_reaction(
            next,
            request
                .tasks
                .iter()
                .find(|task| task.task_id == old.task_id)
                .is_some_and(|task| task.writer.is_some()),
            staged.as_deref(),
        )?;
        self.write(call, |tx| {
            let changed = tx.execute("UPDATE attempts SET revision=?1,terminal=1,tmp_retained=1,record=?2 WHERE id=?3 AND revision=?4 AND terminal=0", params![sql_int(next.revision)?, bytes(next)?, id.to_string(), sql_int(old.revision)?])?;
            if changed != 1 { return Err(RunError::Conflict); }
            if next.schema_version >= 2 {
                let receipts = SettlementReceipts { cancellation: next.cancellation_disposition, ..Default::default() };
                tx.execute("INSERT INTO settlement_receipts(attempt,generation,record) VALUES (?1,?2,?3)", params![id.to_string(), sql_int(next.generation)?, bytes(&receipts)?])?;
            }
            if let Some((successor, wait_seconds)) = &successor { self.insert_policy_successor(tx, successor, *wait_seconds, &request, disk_floor_bytes)?; }
            tx.execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
            Self::insert_reaction(tx, &reaction)?;
            insert_event(tx, next)?;
            let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM attempts WHERE run=?1 AND terminal=0)", [old.run_id.to_string()], |r| r.get(0))?;
            let mut run = self.run_on(tx, old.run_id)?;
            run.revision = run.revision.checked_add(1).ok_or(RunError::Conflict)?;
            if !pending && run.children.is_empty() { run.closure = Closure::Closed; run.closed_at = Some(finished_at); }
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
        provenant_os::changes::hint(
            self.context.state_root.as_path(),
            &[provenant_os::changes::Scope::Board],
        );
        // A stop accepted after the decision read cancel.json (but before this
        // commit became visible) is acknowledged here from the host-sealed,
        // scope- and requester-checked request, not from mutable lane state.
        // A stop sealed later is acknowledged by `cancel` seeing the terminal row.
        if next.cancellation_disposition.is_none()
            && let Ok(attempt) = self.attempt_dir(old.run_id, id, false)
            && let Ok(Some(raw)) = read_sealed(&attempt, "cancel.json", RECORD_LIMIT)
            && decode::<CancellationEvidence>(&raw).is_ok_and(|request| {
                request.scope == Self::scope(old)
                    && request.requester == old.verified_owner.unwrap_or(self.context.actor_id)
                    && request.reason
                        == provenant_supervisor::machine::StopReason::ExplicitCancellation
            })
        {
            self.record_cancellation(
                id,
                next.generation,
                CancellationDisposition::AcknowledgedIneffective,
                call,
            )?;
        }
        if request
            .tasks
            .iter()
            .any(|task| task.task_id == old.task_id && task.writer.is_some())
        {
            // A failure is persisted as a retryable effect, never reported done.
            self.retry_git_cleanup(next, call)?;
        }
        self.contribution_diagnostic(next.run_id, self.retain_contribution_route(next, call));
        Ok(())
    }

    pub(crate) fn set_temporary_retained(
        &self,
        id: AttemptId,
        retained: bool,
        call: &Request<'_>,
    ) -> Result<()> {
        self.write(call, |tx| {
            tx.execute(
                "UPDATE attempts SET tmp_retained=?1 WHERE id=?2 AND project=?3 AND terminal=1",
                params![
                    retained,
                    id.to_string(),
                    self.context.project_id.to_string()
                ],
            )?;
            Ok(())
        })
    }
}
fn validate_request(input: &PreparedRun) -> Result<()> {
    if input.request.schema_version != 1 {
        return Err(RunError::Version(input.request.schema_version as i64));
    }
    if input.request.parent.is_some() && input.request.tasks.len() != 1 {
        return Err(RunError::Validation("a continuation has one task"));
    }
    if input
        .request
        .fresh_continuation
        .as_ref()
        .is_some_and(|reason| {
            reason.is_empty() || reason.len() > 4096 || input.request.parent.is_none()
        })
    {
        return Err(RunError::Validation(
            "fresh continuation needs a bounded reason and parent",
        ));
    }
    if !(1..=64).contains(&input.request.tasks.len())
        || !(1..=16).contains(&input.request.concurrency)
        || input.attempts.len() != input.request.tasks.len()
    {
        return Err(RunError::Validation("task/concurrency bounds"));
    }
    if input
        .request
        .tasks
        .iter()
        .any(|task| task.context.as_ref().is_some_and(|c| !c.accepted()))
    {
        return Err(RunError::Validation("work context"));
    }
    bytes(&input.request)?;
    let mut tasks = std::collections::BTreeSet::new();
    let mut attempts = std::collections::BTreeSet::new();
    let mut prompt_bytes = 0u64;
    for task in &input.request.tasks {
        if task
            .context
            .as_ref()
            .is_some_and(|context| !context.valid())
        {
            return Err(RunError::Validation("work context"));
        }
        if let Some(deliver) = &task.deliver {
            if task.writer.is_none() {
                return Err(RunError::Validation("delivery requires a writer"));
            }
            crate::delivery::validate(deliver)?;
        }
        if task.account.as_ref().is_some_and(|s| {
            s == "unknown"
                || s.is_empty()
                || s.len() > 64
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        }) {
            return Err(RunError::Validation("account override"));
        }
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
        extra: Default::default(),
        schema_version: 1,
        sequence: 0,
        project_id: attempt.project_id,
        run_id: attempt.run_id,
        attempt_id: attempt.attempt_id,
        revision: attempt.revision,
        change: attempt.state.clone(),
        trace: attempt.trace.clone(),
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
    stop_reason: Option<provenant_supervisor::machine::StopReason>,
    #[serde(default)]
    cancel_delivered_before_exit: Option<bool>,
    terminal_observed: bool,
    unresolved_cleanup: bool,
    settlement_evidence: provenant_supervisor::custody::SettlementEvidence,
}
#[derive(serde::Deserialize)]
pub(crate) struct CancellationEvidence {
    #[serde(rename = "request_id")]
    pub(crate) _request_id: provenant_core::OperationId,
    pub(crate) scope: provenant_supervisor::custody::Scope,
    pub(crate) requester: provenant_core::ActorId,
    pub(crate) reason: provenant_supervisor::machine::StopReason,
}
fn sealed_exit(
    dir: &provenant_os::fs::Directory,
    exit: &ExitObservation,
) -> Result<ArtifactRecord> {
    use provenant_supervisor::custody::PublicationState;
    use provenant_supervisor::custody::SettlementEvidence;
    if !matches!(exit.publication, PublicationState::Durable)
        || exit.settlement_evidence == SettlementEvidence::Unresolved
    {
        return Err(RunError::Validation(
            "supervisor exit publication is unresolved",
        ));
    }
    let name = OsStr::new(AttemptFile::ProcessExit.name());
    let mut file = dir.open_read(name)?;
    let meta = file.metadata()?;
    if let Some(check) = PathCheck::file(meta) {
        return Err(RunError::unsafe_entry(dir, name, check));
    }
    let raw = file.read_bounded(RECORD_LIMIT)?;
    let stored: SealedExit = decode(&raw)?;
    if stored.scope != exit.scope
        || stored.termination != exit.termination
        || stored.never_spawned != exit.never_spawned
        || stored.stop_reason != exit.stop_reason
        || stored.cancel_delivered_before_exit != exit.cancel_delivered_before_exit
        || stored.terminal_observed != exit.terminal_observed
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
        extra: Default::default(),
        path: artifact_path(exit.scope.attempt, AttemptFile::ProcessExit.name())?,
        digest: CanonicalV1::file_digest(&raw),
        bytes: raw.len() as u64,
        complete: true,
        redacted: true,
    })
}
