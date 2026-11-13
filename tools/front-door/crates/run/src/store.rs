use crate::{
    artifacts::{artifact_path, read_sealed, remove_temporary_best_effort, seal},
    db::{bytes, decode, sql_int},
    *,
};
use provenant_core::{AttemptId, OperationId, WallTime};
use provenant_os::{exec::Termination, fs::LockMode};
use provenant_records::{AttemptFile, CanonicalV1};
use provenant_supervisor::custody::ExitObservation;
use rusqlite::{OptionalExtension, params};
use std::ffi::OsStr;

#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_EFFECT_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct TerminalEffects {
    pub schema_version: u64,
    pub scope: provenant_supervisor::custody::Scope,
    pub effects: Vec<(SettlementEffect, EffectDisposition)>,
}

/// Whether an explicit stop was requested for exactly this attempt scope.
/// Unreadable evidence counts as no request.
pub(crate) fn cancel_requested(
    attempt: &provenant_os::fs::Directory,
    scope: provenant_supervisor::custody::Scope,
) -> bool {
    read_sealed(attempt, "cancel.json", RECORD_LIMIT)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .is_some_and(|request| request.get("scope") == serde_json::to_value(scope).ok().as_ref())
}

pub(crate) fn import_cancelled(
    attempt: &provenant_os::fs::Directory,
    scope: provenant_supervisor::custody::Scope,
    owner: provenant_core::ActorId,
) -> Result<bool> {
    let marked = read_sealed(attempt, "import-cancelled.json", RECORD_LIMIT)?
        .map(|raw| crate::db::decode_column::<provenant_supervisor::custody::Scope>(&raw))
        .transpose()?
        .is_some_and(|saved| saved == scope);
    if !marked {
        return Ok(false);
    }
    Ok(read_sealed(attempt, "cancel.json", RECORD_LIMIT)?
        .and_then(|raw| decode::<CancellationEvidence>(&raw).ok())
        .is_some_and(|request| request.requests_stop(scope, owner)))
}

/// One launch: exactly one attempt, its request and its scanned prompt.
pub struct PreparedRun {
    pub attempt_id: AttemptId,
    pub operation_id: OperationId,
    pub request: CanonicalRequest,
    /// Exact, already scanned prompt bytes. Store never opens a source file.
    pub prompt: String,
    pub created_at: WallTime,
}
/// The work key a first attempt records: the graph, node and attempt number
/// its task context names. A launch without a key is refused; the store never
/// derives one. A continuation takes its key from its parent instead.
pub(crate) fn work_key(
    project: provenant_core::ProjectId,
    context: Option<&provenant_records::run::WorkContext>,
) -> Result<provenant_records::WorkKey> {
    let named = context.and_then(|context| {
        Some((
            context.graph_id.clone()?,
            context.node_id.clone()?,
            context.attempt?,
        ))
    });
    let (graph, node, attempt) = named.ok_or(RunError::Validation("task names no work key"))?;
    provenant_records::WorkKey::new(project, graph, node, attempt)
        .map_err(|_| RunError::Validation("work key"))
}
/// Result and route material is supplied by the trusted provider/confinement
/// composition edge, after secret scanning. This storage API never launches.
#[derive(Clone)]
pub struct TerminalPublication<'a> {
    pub terminal_cause: Option<TerminalCause>,
    /// The typed stop sealed with this attempt: its cause and the handoff,
    /// decoded once at settlement.
    pub stop: Option<provenant_records::Stop>,
    /// A bounded sentence Provenant composed from fixed text and typed values,
    /// such as a retry decision. It is never provider output.
    pub terminal_note: Option<String>,
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
pub(crate) struct Withdrawal<'a> {
    pub terminal_cause: Option<TerminalCause>,
    pub terminal_note: Option<String>,
    pub expected: AttemptState,
    pub outcome: Outcome,
    pub reason: &'a str,
    pub finished_at: WallTime,
    pub failure: Option<provenant_records::run::PreparationFailure>,
}

impl RunStore {
    /// Mark a frozen refusal's notice complete after its idempotent inbox write.
    pub fn complete_refusal_notice(
        &self,
        expected: &OperationRecord,
        call: &Request<'_>,
    ) -> Result<()> {
        self.write(call, |tx| {
            let raw: Vec<u8> = tx
                .query_row(
                    "SELECT operation_record FROM attempts WHERE project=?1 AND actor=?2 AND operation=?3 AND refused=1 ORDER BY sequence LIMIT 1",
                    params![
                        self.context.project_id.to_string(),
                        self.context.actor_id.to_string(),
                        expected.operation_id.as_str()
                    ],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(RunError::NotFound)?;
            let mut current: OperationRecord = decode(&raw)?;
            if current.project_id != self.context.project_id
                || current.actor_id != self.context.actor_id
                || current.kind != expected.kind
                || current.state != OperationState::Refused
                || current.request_digest != expected.request_digest
                || current.refusal != expected.refusal
                || current.refusal_chair != expected.refusal_chair
                || current.refused_at != expected.refused_at
                || current.extra.get("refusal_notice") != expected.extra.get("refusal_notice")
            {
                return Err(RunError::Conflict);
            }
            current
                .extra
                .insert("refusal_notice_done".into(), serde_json::json!(true));
            tx.execute(
                "UPDATE attempts SET operation_record=?1 WHERE project=?2 AND actor=?3 AND operation=?4",
                params![
                    bytes(&current)?,
                    self.context.project_id.to_string(),
                    self.context.actor_id.to_string(),
                    expected.operation_id.as_str()
                ],
            )?;
            Ok(())
        })
    }
    /// Persist a refusal against the same request digest used for admission.
    /// A competing or replayed operation cannot replace its first outcome.
    pub fn record_refusal(
        &self,
        input: &PreparedRun,
        diagnostic: provenant_records::diagnostic::Diagnostic,
        worktree: Option<String>,
        call: &Request<'_>,
    ) -> Result<OperationRecord> {
        self.record_refusal_with_context(
            input,
            diagnostic,
            worktree,
            &crate::RefusalContext::default(),
            call,
        )
    }

    pub(crate) fn record_refusal_with_context(
        &self,
        input: &PreparedRun,
        diagnostic: provenant_records::diagnostic::Diagnostic,
        worktree: Option<String>,
        context: &crate::RefusalContext,
        call: &Request<'_>,
    ) -> Result<OperationRecord> {
        validate_request(input)?;
        if worktree.as_ref().is_some_and(|name| {
            name.is_empty()
                || name.len() > 1024
                || name.contains('/')
                || name.chars().any(char::is_control)
        }) {
            return Err(RunError::Validation("worktree identifier"));
        }
        let digest = input.request.digest()?;
        let mut extra = provenant_records::ExtraFields::default();
        if let Some(notice) = input.request.task.profile.knobs.get("chair.refusal_notice") {
            extra.insert("refusal_notice".into(), notice.clone());
        }
        if let Some(digest) = context.intent_digest {
            extra.insert("origin_intent_digest".into(), serde_json::json!(digest));
        }
        let operation = OperationRecord {
            extra,
            schema_version: 1,
            project_id: self.context.project_id,
            actor_id: self.context.actor_id,
            operation_id: input.operation_id.clone(),
            kind: if input.request.parent.is_some() {
                OperationKind::Resume
            } else {
                OperationKind::Dispatch
            },
            request_digest: digest,
            state: OperationState::Refused,
            blocked_reason: None,
            refusal: Some(diagnostic),
            worktree,
            refused_at: Some(input.created_at),
            refusal_chair: context
                .chair
                .clone()
                .or_else(|| input.request.chair.clone()),
        };
        let key = work_key(self.context.project_id, input.request.task.context.as_ref())?;
        self.insert_refusal_row(key, input.attempt_id, operation, call)
    }

    /// Commit a trusted owner's preflight refusal before canonical admission.
    /// The caller supplies only an already projected, safe diagnostic. The first
    /// record wins; replays may not change its scope, kind or request identity.
    /// A refusal that never reached a node is a one-node lane under a fresh
    /// name; callers that know the key use `insert_refusal_under`.
    pub fn insert_refusal(
        &self,
        operation: OperationRecord,
        call: &Request<'_>,
    ) -> Result<OperationRecord> {
        let id = AttemptId::from_bytes(provenant_os::fs::entropy_128()?);
        let key = provenant_records::WorkKey::new(
            self.context.project_id,
            format!("lane-{id}"),
            "lane",
            1,
        )
        .map_err(|_| RunError::Validation("work key"))?;
        self.insert_refusal_row(key, id, operation, call)
    }
    /// Commit a preflight refusal under the work key the caller would have
    /// admitted. The refused row keeps the key, so it is never reused.
    pub fn insert_refusal_under(
        &self,
        key: &provenant_records::WorkKey,
        operation: OperationRecord,
        call: &Request<'_>,
    ) -> Result<OperationRecord> {
        let id = AttemptId::from_bytes(provenant_os::fs::entropy_128()?);
        self.insert_refusal_row(key.clone(), id, operation, call)
    }
    fn insert_refusal_row(
        &self,
        key: provenant_records::WorkKey,
        id: AttemptId,
        operation: OperationRecord,
        call: &Request<'_>,
    ) -> Result<OperationRecord> {
        if operation.schema_version != 1
            || operation.project_id != self.context.project_id
            || operation.actor_id != self.context.actor_id
            || key.project() != self.context.project_id
            || operation.state != OperationState::Refused
            || operation.refusal.is_none()
            || operation.refused_at.is_none()
            || operation.worktree.as_ref().is_some_and(|name| {
                name.is_empty()
                    || name.len() > 1024
                    || name.contains('/')
                    || name.chars().any(char::is_control)
            })
        {
            return Err(RunError::Validation("refused operation identity"));
        }
        let digest = operation.request_digest;
        self.write(call, |tx| {
            let prior: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT operation_record FROM attempts WHERE project=?1 AND actor=?2 AND operation=?3 ORDER BY sequence LIMIT 1",
                    params![
                        self.context.project_id.to_string(),
                        self.context.actor_id.to_string(),
                        operation.operation_id.as_str()
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(prior) = prior {
                let prior: OperationRecord = decode(&prior)?;
                if prior.project_id != self.context.project_id
                    || prior.actor_id != self.context.actor_id
                    || prior.kind != operation.kind
                    || prior.request_digest != digest
                {
                    return Err(RunError::OperationConflict);
                }
                return Ok(prior);
            }
            if let Some((prior_digest, raw)) = tx
                .query_row(
                    "SELECT digest,operation_record FROM attempts WHERE project=?1 AND graph=?2 AND node=?3 AND attempt=?4",
                    params![key.project().to_string(), key.graph(), key.node(), key.attempt()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?
            {
                if prior_digest != digest.to_string() {
                    return Err(RunError::OperationConflict);
                }
                return decode(&raw);
            }
            tx.execute(
                "INSERT INTO attempts(id,project,graph,node,attempt,digest,actor,operation,number,refused,operation_record,revision,terminal,record,tmp_retained) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,1,?9,0,1,NULL,0)",
                params![
                    id.to_string(),
                    self.context.project_id.to_string(),
                    key.graph(),
                    key.node(),
                    key.attempt(),
                    digest.to_string(),
                    self.context.actor_id.to_string(),
                    operation.operation_id.as_str(),
                    bytes(&operation)?
                ],
            )?;
            Ok(operation.clone())
        })
    }
    /// A sealed decision is not a certificate that an unfinished import can
    /// relinquish its reservation or private object inputs: the import must
    /// finish first, unless it was refused for good. The terminal receipts are
    /// the single source.
    pub(crate) fn import_release_permitted(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<bool> {
        let attempt = self.attempt_dir(record.attempt_id, false)?;
        if crate::settlement::abandoned(&attempt, Self::scope(record))?.is_some() {
            return Ok(true);
        }
        if crate::holder::object_import_refused(&attempt, Self::scope(record))?.is_some() {
            return Ok(true);
        }
        // Only a failed import that can still be retried holds custody. A
        // pending receipt is a decision sealed without import tracking.
        Ok(self
            .record(record.attempt_id, call)?
            .settlement_receipts
            .is_none_or(|receipts| {
                !matches!(
                    receipts.git_import,
                    EffectDisposition::Failed {
                        retryable: true,
                        ..
                    } | EffectDisposition::Cancelled {
                        retryable: true,
                        ..
                    }
                )
            }))
    }

    /// Only a succeeded or review-required lane has anything to deliver.
    pub(crate) fn delivery_outcome(record: &AttemptRecord) -> bool {
        matches!(
            record.state,
            AttemptState::Terminal {
                outcome: Outcome::Succeeded | Outcome::ReviewRequired,
                ..
            }
        )
    }

    /// Terminal attempts with effects still to drive. A row that cannot be
    /// judged is listed too, so its own reconcile reports the error instead of
    /// the failure hiding every other attempt of the run.
    pub(crate) fn pending_effect_attempts(
        &self,
        run: AttemptId,
        call: &Request<'_>,
    ) -> Result<Vec<AttemptId>> {
        let mut query = self.db.prepare("SELECT a.id,r.record FROM attempts a JOIN settlement_receipts r ON r.attempt=a.id WHERE a.id=?1 AND a.project=?2 AND a.terminal=1")?;
        let rows = query.query_map(
            params![run.to_string(), self.context.project_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;
        let mut pending = Vec::new();
        for row in rows {
            call.check()?;
            let (id, raw) = row?;
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            if self.effects_pending(id, &raw, call).unwrap_or(true) {
                pending.push(id);
            }
        }
        Ok(pending)
    }

    fn effects_pending(&self, id: AttemptId, raw: &[u8], call: &Request<'_>) -> Result<bool> {
        // A cancelled delivery waits for an explicit `lane deliver`.
        let retryable = |effect: &EffectDisposition| {
            matches!(
                effect,
                EffectDisposition::Pending
                    | EffectDisposition::Failed {
                        retryable: true,
                        ..
                    }
            )
        };
        let receipts: SettlementReceipts = crate::db::decode_column(raw)?;
        let record = self.record(id, call)?;
        let delivery = Self::delivery_outcome(&record) && retryable(&receipts.delivery);
        let manifest = retryable(&receipts.manifest_conflict)
            && (matches!(receipts.manifest_conflict, EffectDisposition::Failed { .. })
                || self
                    .attempt_dir(id, false)
                    .and_then(|attempt| {
                        crate::artifacts::subdir(&attempt, OsStr::new("quarantine"), false)
                    })
                    .is_ok_and(|archive| {
                        read_sealed(&archive, "manifest-intended.json", RECORD_LIMIT)
                            .ok()
                            .flatten()
                            .is_some()
                    }));
        let import =
            retryable(&receipts.git_import) && !self.import_release_permitted(&record, call)?;
        Ok(delivery || manifest || import)
    }
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
        let dir = self.attempt_dir(id, false)?;
        if read_sealed(
            &dir,
            AttemptFile::TerminalPending.name(),
            TERMINAL_RECORD_LIMIT,
        )?
        .is_some()
        {
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
        let dir = self.attempt_dir(id, false)?;
        let pending: AttemptRecord = crate::db::decode_attempt(
            &read_sealed(
                &dir,
                AttemptFile::TerminalPending.name(),
                TERMINAL_RECORD_LIMIT,
            )?
            .ok_or(RunError::TerminalRecoveryRequired)?,
        )?;
        super::reader::validate_attempt(&pending)?;
        if pending.attempt_id != id
            || pending.attempt_id != old.attempt_id
            || pending.project_id != old.project_id
            || pending.key != old.key
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
        self.record(id, call)?;
        let run = self.attempt_dir(id, false)?;
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
            self.settle_cleanup(&record, None, call);
        }
        self.record(id, call)
    }
    /// Replay with `publication.lock` already held. The flag reports whether
    /// this call committed the row, so the caller owes post-commit cleanup.
    fn replay_terminal_locked(
        &self,
        id: AttemptId,
        call: &Request<'_>,
    ) -> Result<(AttemptRecord, bool)> {
        let current = self.record(id, call)?;
        let dir = self.attempt_dir(id, false)?;
        let pending: AttemptRecord = crate::db::decode_attempt(
            &read_sealed(
                &dir,
                AttemptFile::TerminalPending.name(),
                TERMINAL_RECORD_LIMIT,
            )?
            .ok_or(RunError::TerminalRecoveryRequired)?,
        )?;
        super::reader::validate_attempt(&pending)?;
        if pending.attempt_id != id
            || pending.attempt_id != current.attempt_id
            || pending.project_id != current.project_id
            || pending.key != current.key
            || pending.generation != current.generation
            || !matches!(pending.state, AttemptState::Terminal { .. })
            || pending.settlement_receipts.is_some()
            || pending.custody_pending.is_some()
        {
            return Err(RunError::Conflict);
        }
        if let Some(result) = &pending.result {
            self.artifact_bytes(result, RESULT_LIMIT)?;
        }
        if let Some(exit) = &pending.exit_evidence {
            let raw = self.artifact_bytes(exit, RECORD_LIMIT)?;
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
                    && stored.cancel_delivered_before_exit != Some(true)
                    && !(pending.terminal_evidence == Some(TerminalEvidence::ImportCancellation)
                        && import_cancelled(
                            &dir,
                            Self::scope(&current),
                            current.verified_owner.unwrap_or(self.context.actor_id),
                        )?))
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
            // Delivery is a post-commit receipt projected by record(), not a
            // replacement of the immutable decision's delivery snapshot.
            decision.delivery = pending.delivery.clone();
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
        #[cfg(test)]
        if FAIL_EFFECT_WRITE.with(|fault| fault.get()) {
            return Err(RunError::Contention);
        }
        if let EffectDisposition::Failed { reason, .. }
        | EffectDisposition::Cancelled { reason, .. } = &disposition
            && (reason.is_empty() || reason.len() > 4096)
        {
            return Err(RunError::Validation("invalid effect failure reason"));
        }
        // Cleanup failures are surfaced for manual recovery; no terminal-effect
        // driver retries them after temporary data has been removed.
        let disposition = match (effect, disposition) {
            (SettlementEffect::CleanupCustodyRelease, EffectDisposition::Failed { reason, .. }) => {
                EffectDisposition::Failed {
                    reason,
                    retryable: false,
                }
            }
            (_, disposition) => disposition,
        };
        let receipts = self.write(call, |tx| {
            let raw: Vec<u8> = tx.query_row("SELECT record FROM attempts WHERE id=?1 AND project=?2 AND terminal=1", params![id.to_string(), self.context.project_id.to_string()], |row| row.get(0)).optional()?.ok_or(RunError::NotFound)?;
            let attempt: AttemptRecord = crate::db::decode_attempt(&raw)?;
            if attempt.generation != generation { return Err(RunError::Conflict); }
            let existing: Option<(i64, Vec<u8>)> = tx.query_row("SELECT generation,record FROM settlement_receipts WHERE attempt=?1", [id.to_string()], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
            let mut receipts: SettlementReceipts = match existing {
                Some((saved, raw)) if saved == sql_int(generation)? => crate::db::decode_column(&raw)?,
                Some(_) => return Err(RunError::Conflict),
                None => SettlementReceipts::default(),
            };
            let slot = receipts.slot_mut(effect);
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
            let mut receipts: SettlementReceipts = raw.as_deref().map(crate::db::decode_column).transpose()?.unwrap_or_default();
            if disposition == CancellationDisposition::AcknowledgedIneffective && !matches!(receipts.cancellation, Some(CancellationDisposition::DeliveryEffective | CancellationDisposition::ImportEffective)) {
                receipts.late_cancellation_acknowledged = true;
            }
            if receipts.cancellation.is_none() || matches!(disposition, CancellationDisposition::Effective | CancellationDisposition::DeliveryEffective | CancellationDisposition::ImportEffective) {
                receipts.cancellation = Some(disposition);
                if matches!(disposition,CancellationDisposition::DeliveryEffective | CancellationDisposition::ImportEffective) { receipts.late_cancellation_acknowledged = false; }
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
    /// Reserve the record first, seal the exact request and prompt, then
    /// acknowledge. Repeating a prepared operation repairs a publication gap
    /// with the same request. No scheduler, admission or launch is implied by
    /// acceptance here.
    pub fn create(&self, input: &PreparedRun, call: &Request<'_>) -> Result<AttemptRecord> {
        validate_request(input)?;
        // A crash before sealing left the operation prepared under its own
        // attempt ID; a replay with a fresh ID adopts it rather than conflicting.
        if let Some(op) = self.operation(&input.operation_id, call)?
            && op.state == OperationState::Prepared
            && op.request_digest == input.request.digest()?
        {
            let attempt_id = self.operation_attempt(&input.operation_id, call)?;
            if attempt_id != input.attempt_id {
                return self.create_exact(
                    &PreparedRun {
                        attempt_id,
                        operation_id: input.operation_id.clone(),
                        request: input.request.clone(),
                        prompt: input.prompt.clone(),
                        created_at: input.created_at,
                    },
                    call,
                );
            }
        }
        self.create_exact(input, call)
    }
    fn create_exact(&self, input: &PreparedRun, call: &Request<'_>) -> Result<AttemptRecord> {
        let digest = input.request.digest()?;
        let context = &self.context;
        if let Some(op) = self.operation(&input.operation_id, call)? {
            if op.request_digest != digest {
                return Err(RunError::OperationConflict);
            }
            if op.state == OperationState::Refused {
                return Err(RunError::RecordedRefusal(Box::new(
                    op.refusal
                        .clone()
                        .ok_or(RunError::Validation("refusal diagnostic missing"))?,
                )));
            }
            let admitted = self.operation_attempt(&input.operation_id, call)?;
            if op.state != OperationState::Prepared {
                return self.record(admitted, call);
            }
            if admitted != input.attempt_id {
                return Err(RunError::Conflict);
            }
        }
        // Configuration and volume probes happen before the store transaction,
        // but a stored refusal replays without depending on current host state.
        let disk_floor_bytes = provenant_config::GcSettings::read(&self.writer_registry)?
            .free_floor_bytes(&self.writer_registry)?;
        let task = &input.request.task;
        let telemetry_context = crate::telemetry_context::capture(task, input.created_at);
        let mut operation_extra = provenant_records::ExtraFields::default();
        if let Some(parent) = input.request.parent_attempt {
            operation_extra.insert("parent_attempt".into(), serde_json::json!(parent));
        }
        let operation = OperationRecord {
            extra: operation_extra,
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
            state: OperationState::Prepared,
            blocked_reason: None,
            refusal: None,
            worktree: None,
            refused_at: None,
            refusal_chair: None,
        };
        let admitted = self.write(call, |tx| {
            // The operation ID is the caller's replay handle; the work key is
            // the attempt's identity.
            let prior: Option<(Vec<u8>, String)> = tx.query_row("SELECT operation_record,id FROM attempts WHERE project=?1 AND actor=?2 AND operation=?3 ORDER BY sequence LIMIT 1", params![context.project_id.to_string(), context.actor_id.to_string(), input.operation_id.as_str()], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            if let Some((prior, id)) = prior {
                let prior: OperationRecord = decode(&prior)?;
                if prior.request_digest != digest { return Err(RunError::OperationConflict); }
                if prior.state == OperationState::Refused {
                    return Err(RunError::RecordedRefusal(Box::new(prior.refusal.ok_or(RunError::Validation("refusal diagnostic missing"))?)));
                }
                if id != input.attempt_id.to_string() { return Err(RunError::Conflict); }
                return Ok(None);
            }
            // A continuation is the next attempt of its parent's node: it
            // takes the node's key with the following attempt number, and
            // each terminal attempt is continued at most once. Any other
            // launch names its own key in its task context.
            let key = match input.request.parent {
                None => work_key(context.project_id, task.context.as_ref())?,
                Some(target) => {
                    let (graph, node, number): (String, String, u32) = tx.query_row("SELECT graph,node,attempt FROM attempts WHERE id=?1 AND project=?2 AND refused=0", params![target.to_string(), context.project_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?.ok_or(RunError::NotFound)?;
                    let taken: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM attempts WHERE continues=?1)", [target.to_string()], |r| r.get(0))?;
                    if taken { return Err(RunError::Conflict); }
                    let next = number.checked_add(1).ok_or(RunError::Validation("attempt number"))?;
                    provenant_records::WorkKey::new(context.project_id, graph, node, next).map_err(|_| RunError::Validation("work key"))?
                }
            };
            // Concurrent submissions of one key serialise on this write; the
            // first admits it and the rest replay or conflict.
            let existing: Option<(String, bool, Vec<u8>, String)> = tx.query_row("SELECT digest,refused,operation_record,id FROM attempts WHERE project=?1 AND graph=?2 AND node=?3 AND attempt=?4", params![key.project().to_string(), key.graph(), key.node(), key.attempt()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
            if let Some((prior_digest, refused, raw, id)) = existing {
                if prior_digest != digest.to_string() { return Err(RunError::OperationConflict); }
                if refused {
                    let prior: OperationRecord = decode(&raw)?;
                    return Err(RunError::RecordedRefusal(Box::new(prior.refusal.ok_or(RunError::Validation("refusal diagnostic missing"))?)));
                }
                return Ok(Some(AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?));
            }
            let queued: i64 = tx.query_row("SELECT COUNT(*) FROM admission WHERE active=0", [], |r| r.get(0))?;
            if queued >= sql_int(crate::admission::HOST_QUEUED)? { return Err(RunError::Capacity); }
            let attempt_id = input.attempt_id;
            let attempt = AttemptRecord {extra: Default::default(),
                key: key.clone(),
                context: task.context.clone(),
                title: task.title.clone(),
                continuation: input.request.fresh_continuation.as_ref().map(|_| "fresh".into()),
                profile: task.profile.clone(),
                limits: None,
                schema_version: 2, attempt_id, project_id: context.project_id, attempt_number: u64::from(key.attempt()), revision: 1, generation: 0,
                delivery_id: task.delivery_id, retry_of: input.request.parent, superseded_by: None, requested_route: task.route.clone(), resolved_route: None, applied_route: None, observed_route: None, authority_digest: task.authority_digest, read_only_writable: Vec::new(), write_grants: Vec::new(), git_grants_digest: None, granted_tracking_ref: None, subject: None, head: None, protected_paths: Vec::new(), delivery: None, verified_owner: Some(context.actor_id), session: None,
                queued_at: input.created_at, started_at: None, priority: None, requested_priority: None, last_output_at: None, provider_silent_for_ms: provenant_records::run::Transient(None), process_observation_valid: provenant_records::run::Transient(false), provider_silence_suppressed: provenant_records::run::Transient(false), provider_launched: None, preparation_failure: None, provider_ms: None, admission_deadline: None, runtime_deadline: None, partial_signals: Vec::new(), state: AttemptState::Queued { extra: Default::default() }, usage: UsageRecord::default(),
                terminal_evidence: None, settlement_reason: None, cancellation_disposition: None, settlement_receipts: None, custody_pending: None,
                prompt: ArtifactRecord {extra: Default::default(),  path: artifact_path(attempt_id, AttemptFile::Prompt.name())?, digest: task.prompt_digest, bytes: input.prompt.len() as u64, complete: true, redacted: false }, result: None, publication_error: None, exit_evidence: None, skipped_routes: Vec::new(),
                trace: provenant_records::Trace {extra: telemetry_context.clone(),
                    host: self.host,
                    project: Some(context.project_id),
                    attempt: Some(attempt_id),
                    operation: Some(input.operation_id.clone()),
                    actor: Some(context.actor_id),
                    parent: input.request.parent.or(input.request.parent_attempt),
                },
            };
            tx.execute("INSERT INTO attempts(id,project,graph,node,attempt,digest,actor,operation,number,continues,operation_record,revision,terminal,record) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1,0,?12)", params![attempt_id.to_string(), context.project_id.to_string(), key.graph(), key.node(), key.attempt(), digest.to_string(), context.actor_id.to_string(), input.operation_id.as_str(), i64::from(key.attempt()), input.request.parent.map(|target| target.to_string()), bytes(&operation)?, crate::db::attempt_bytes(&attempt)?])?;
            insert_event(tx, &attempt)?;
            let class = Service::lane_class(task.writer.is_some(), &task.access.write_roots, &[]);
            self.queue_ticket(tx, &attempt, class, task.provider_timeout_seconds, disk_floor_bytes)?;
            Ok(None)
        })?;
        if let Some(existing) = admitted {
            return self.record(existing, call);
        }
        // Reserve first: a conflicting or replayed work key must not leave an
        // unindexed attempt directory.
        let dir = self.attempt_dir(input.attempt_id, true)?;
        let _writer = dir.lock(
            OsStr::new("publication.lock"),
            LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        provenant_os::changes::hint(
            self.context.state_root.as_path(),
            &[provenant_os::changes::Scope::Board],
        );
        #[cfg(test)]
        crate::tests::crash_at("reserved");
        seal(&dir, "request.json", &bytes(&input.request)?, RECORD_LIMIT)?;
        let recorded = self.record(input.attempt_id, call)?;
        if recorded.prompt.digest != CanonicalV1::file_digest(input.prompt.as_bytes()) {
            return Err(RunError::Conflict);
        }
        seal(
            &dir,
            AttemptFile::Prompt.name(),
            input.prompt.as_bytes(),
            RECORD_LIMIT,
        )?;
        self.write(call, |tx| {
            let mut accepted = operation.clone();
            accepted.state = OperationState::Accepted;
            tx.execute(
                "UPDATE attempts SET operation_record=?1 WHERE project=?2 AND actor=?3 AND operation=?4",
                params![
                    bytes(&accepted)?,
                    context.project_id.to_string(),
                    context.actor_id.to_string(),
                    input.operation_id.as_str()
                ],
            )?;
            Ok(())
        })?;
        self.record(input.attempt_id, call)
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
        let durable_exit = sealed_exit(&self.attempt_dir(id, false)?, exit)?;
        let outcome_exit = match exit.termination {
            Some(Termination::Exit(code)) => Some(code),
            _ => None,
        };
        if exit.scope.attempt != old.attempt_id
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
            && !import_cancelled(
                &self.attempt_dir(id, false)?,
                Self::scope(&old),
                old.verified_owner.unwrap_or(self.context.actor_id),
            )?
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
            false,
            call,
        );
        match result {
            Ok(record) => {
                self.settle_cleanup(&record, None, call);
                self.record(id, call)
            }
            Err(error) => Err(error),
        }
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
        self.withdraw_with_preparation_failure(
            id,
            Withdrawal {
                expected,
                outcome,
                reason,
                finished_at,
                failure: None,
                terminal_cause: None,
                terminal_note: None,
            },
            call,
        )
    }

    /// Seal a typed refusal made in local provider preparation before custody.
    pub(crate) fn withdraw_with_preparation_failure(
        &self,
        id: AttemptId,
        withdrawal: Withdrawal<'_>,
        call: &Request<'_>,
    ) -> Result<Option<AttemptRecord>> {
        self.withdraw_inner(id, withdrawal, false, call)
    }

    pub(crate) fn withdraw_preserving_evidence(
        &self,
        id: AttemptId,
        withdrawal: Withdrawal<'_>,
        call: &Request<'_>,
    ) -> Result<Option<AttemptRecord>> {
        self.withdraw_inner(id, withdrawal, true, call)
    }

    fn withdraw_inner(
        &self,
        id: AttemptId,
        withdrawal: Withdrawal<'_>,
        preserve_evidence: bool,
        call: &Request<'_>,
    ) -> Result<Option<AttemptRecord>> {
        let Withdrawal {
            expected,
            outcome,
            reason,
            finished_at,
            failure,
            terminal_cause,
            terminal_note,
        } = withdrawal;
        if failure.is_some() && !matches!(expected, AttemptState::Queued { .. }) {
            return Err(RunError::Validation(
                "preparation evidence requires queued attempt",
            ));
        }
        if !matches!(
            outcome,
            Outcome::Rejected | Outcome::Cancelled | Outcome::AuthRequired
        ) {
            return Err(RunError::Validation("withdrawal outcome"));
        }
        let defer_writer_cleanup = matches!(expected, AttemptState::Starting { .. });
        self.record(id, call)?;
        let dir = self.attempt_dir(id, false)?;
        let writer = dir.lock(
            OsStr::new("publication.lock"),
            LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        let mut old = self.record(id, call)?;
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
        let attempt_dir = self.attempt_dir(id, true)?;
        // An interrupted withdrawal resumes its own sealed facts, so a retry
        // never renders a second finished_at or reason.
        let (outcome, finished_at, reason, preparation_failure, terminal_cause, terminal_note) =
            match read_sealed(
                &attempt_dir,
                AttemptFile::TerminalPending.name(),
                TERMINAL_RECORD_LIMIT,
            )? {
                Some(raw) => {
                    let saved: AttemptRecord = crate::db::decode_attempt(&raw)?;
                    super::reader::validate_attempt(&saved)?;
                    match saved.state {
                        AttemptState::Terminal {
                            outcome:
                                saved_outcome @ (Outcome::Rejected
                                | Outcome::Cancelled
                                | Outcome::AuthRequired),
                            exit_code: None,
                            finished_at,
                            reason,
                            extra,
                            ..
                        } if saved.attempt_id == id
                            && saved.attempt_id == old.attempt_id
                            && saved.generation == old.generation
                            && saved.exit_evidence.is_none()
                            && saved.provider_launched == Some(false)
                            && Some(saved.revision) == revision.checked_add(1) =>
                        {
                            (
                                saved_outcome,
                                finished_at,
                                reason,
                                saved.preparation_failure,
                                extra
                                    .get("terminal_cause")
                                    .and_then(|value| serde_json::from_value(value.clone()).ok()),
                                extra
                                    .get("terminal_note")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_owned),
                            )
                        }
                        _ => return Err(RunError::TerminalRecoveryRequired),
                    }
                }
                None => (
                    outcome,
                    finished_at,
                    Some(reason.chars().take(256).collect()),
                    failure,
                    terminal_cause,
                    terminal_note,
                ),
            };
        old.preparation_failure = preparation_failure;
        let publication = TerminalPublication {
            stop: terminal_cause.and_then(|cause| {
                provenant_records::Stop::sealed(Some(cause), None)
                    .ok()
                    .flatten()
            }),
            terminal_cause,
            terminal_note,
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
            .commit_terminal(
                old,
                revision,
                &publication,
                None,
                None,
                false,
                None,
                true,
                call,
            )
            .map(Some);
        drop(writer);
        match result {
            Ok(Some(record)) => {
                // A failed handoff must publish its refusal promptly. Git pin
                // cleanup can block on Git; tmp_retained makes reconciliation
                // discover this terminal attempt and complete it there.
                if defer_writer_cleanup
                    && matches!(
                        record.state,
                        AttemptState::Terminal {
                            outcome: Outcome::Rejected,
                            ..
                        }
                    )
                    && self.request(record.attempt_id, call)?.task.writer.is_some()
                {
                    return self.record(id, call).map(Some);
                }
                if !preserve_evidence {
                    self.settle_cleanup(&record, None, call);
                }
                self.record(id, call).map(Some)
            }
            other => other,
        }
    }
    pub(crate) fn settle_cleanup(
        &self,
        record: &AttemptRecord,
        recovery: Option<&provenant_supervisor::custody::Recovery>,
        call: &Request<'_>,
    ) {
        let Ok(attempt_dir) = self.attempt_dir(record.attempt_id, false) else {
            return;
        };
        if !matches!(
            crate::settlement::abandoned(&attempt_dir, Self::scope(record)),
            Ok(None)
        ) {
            return;
        }
        if crate::settlement::queued_abandonment_pending(&attempt_dir, record).unwrap_or(true) {
            return;
        }
        if !self.import_release_permitted(record, call).unwrap_or(false) {
            let _ = self.set_temporary_retained(record.attempt_id, true, call);
            return;
        }
        // A resource refusal has no graph certificate. Keep all private
        // evidence until its retained writer reservation is recovered.
        match crate::holder::object_import_refused(&attempt_dir, Self::scope(record)) {
            Ok(None) => {}
            Ok(Some(_)) | Err(_) => {
                let _ = self.set_temporary_retained(record.attempt_id, true, call);
                return;
            }
        }
        if self
            .request(record.attempt_id, call)
            .is_ok_and(|request| request.task.writer.is_some())
        {
            let _ = self.retry_git_cleanup_with_custody(record, recovery, call);
        }
        // The terminal record retains tmp before cleanup; clear that hint only
        // after removal succeeds.
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
    /// Seal result and terminal intent, then commit the attempt, run summary,
    /// operation state, released admission and event together. Sealing precedes
    /// acquisition of the publication lock; withdrawals already hold it.
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
        publication_locked: bool,
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
        let mut terminal_extra = provenant_records::ExtraFields::default();
        if let Some(cause) = publication.terminal_cause {
            terminal_extra.insert(
                "terminal_cause".into(),
                serde_json::to_value(cause)
                    .map_err(|_| RunError::Validation("terminal cause could not be encoded"))?,
            );
        }
        if let Some(stop) = &publication.stop {
            terminal_extra.insert(
                "stop".into(),
                serde_json::to_value(stop)
                    .map_err(|_| RunError::Validation("stop could not be encoded"))?,
            );
        }
        if let Some(note) = publication.terminal_note.as_deref() {
            terminal_extra.insert(
                "terminal_note".into(),
                serde_json::Value::String(provenant_records::diagnostic::safe_text(note)),
            );
        }
        next.state = AttemptState::Terminal {
            extra: terminal_extra,
            outcome: publication.outcome,
            exit_code: outcome_exit,
            finished_at: publication.finished_at,
            reason: publication.reason.clone(),
        };
        next.schema_version = 2;
        let import_cancellation = publication.outcome == Outcome::Cancelled
            && import_cancelled(
                &self.attempt_dir(id, false)?,
                Self::scope(&old),
                old.verified_owner.unwrap_or(self.context.actor_id),
            )?;
        next.terminal_evidence = Some(if import_cancellation {
            TerminalEvidence::ImportCancellation
        } else if publication.outcome == Outcome::Unsettled {
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
        next.settlement_reason = Some(SettlementReason::derive(
            publication.outcome,
            next.terminal_evidence.as_ref(),
            &next.partial_signals,
        ));
        next.publication_error = None;
        next.exit_evidence = durable_exit;
        let attempt = self.attempt_dir(id, false)?;
        if import_cancellation {
            next.cancellation_disposition = Some(CancellationDisposition::ImportEffective);
        } else if publication.outcome == Outcome::Cancelled
            && (!launched || cancellation_delivered == Some(true))
        {
            next.cancellation_disposition = Some(CancellationDisposition::Effective);
        } else if let Some(raw) = read_sealed(&attempt, "cancel.json", RECORD_LIMIT)? {
            if let Ok(request) = decode::<CancellationEvidence>(&raw) {
                if request.requests_stop(
                    Self::scope(&old),
                    old.verified_owner.unwrap_or(self.context.actor_id),
                ) {
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
                if receipt.head.is_some() && (next.head.is_none() || next.head == receipt.head) {
                    next.head = receipt.head;
                    next.delivery = Some(receipt);
                }
            } else if let Some(deliver) = self.request(old.attempt_id, call)?.task.deliver {
                // Requested delivery that never ran must not read as success
                // with no trace.
                next.delivery = Some(WriterDeliveryReceipt {
                    extra: Default::default(),
                    schema_version: 1,
                    remote: deliver.remote,
                    branch: deliver.branch,
                    head: next.head,
                    review_acceptance: None,
                    state: WriterDeliveryState::NotAttempted {
                        extra: Default::default(),
                    },
                    integration: None,
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
        let intent = crate::db::attempt_bytes(&next)?;
        let attempt_dir = self.attempt_dir(id, false)?;
        // Seal the result, then the exact terminal intent that names it. A
        // crash between them leaves a result-only orphan, which the edge
        // recovers with revalidated facts; intent never names missing bytes.
        match read_sealed(
            &attempt_dir,
            AttemptFile::TerminalPending.name(),
            TERMINAL_RECORD_LIMIT,
        )? {
            Some(saved) if saved != intent => {
                let name = format!(
                    "terminal-pending.conflict-{}.json",
                    provenant_core::hex(CanonicalV1::file_digest(&intent).bytes())
                );
                seal(&attempt_dir, &name, &intent, TERMINAL_RECORD_LIMIT)?;
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
                    TERMINAL_RECORD_LIMIT,
                )?;
            }
        }
        #[cfg(test)]
        crate::tests::crash_at("sealed");
        if publication_locked {
            self.commit_terminal_record(&old, &next, call)?;
        } else {
            let run = self.attempt_dir(id, false)?;
            let _lock = run.lock(
                OsStr::new("publication.lock"),
                LockMode::Exclusive,
                call.lock_budget(),
                call.cancel,
                call.clock,
            )?;
            self.replay_terminal_locked(id, call)?;
        }
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
        let directory = self.attempt_dir(scope.attempt, false)?;
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
        next.settlement_reason = Some(SettlementReason::Lost);
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
        let attempt = self.attempt_dir(old.attempt_id, false)?;
        let name = AttemptFile::TerminalPending.name();
        let next = match read_sealed(&attempt, name, TERMINAL_RECORD_LIMIT)? {
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
                        self.settle_cleanup(&record, None, call);
                    }
                    return Ok(());
                }
                if saved.attempt_id != old.attempt_id
                    || saved.attempt_id != old.attempt_id
                    || saved.project_id != old.project_id
                    || saved.key != old.key
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
                    self.artifact_bytes(result, RESULT_LIMIT)?;
                }
                if let Some(exit) = &saved.exit_evidence {
                    self.artifact_bytes(exit, RECORD_LIMIT)?;
                }
                saved
            }
            None => {
                seal(
                    &attempt,
                    name,
                    &crate::db::attempt_bytes(&next)?,
                    TERMINAL_RECORD_LIMIT,
                )?;
                next
            }
        };
        // Keep recovery discoverable until fence release, even for readers
        // whose reservations live outside the writer registry. Reconcile
        // recognises this lost state and preserves scratch on every retry.
        self.commit_terminal_record(&old, &next, call)
    }

    /// Run the post-commit Git pin cleanup of a committed terminal attempt and
    /// persist the result: a non-retryable failure on error, `Done` on success.
    /// Safe to repeat; callers retry while the receipt is not `Done`.
    pub fn retry_git_cleanup(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<SettlementReceipts> {
        self.retry_git_cleanup_with_custody(record, None, call)
    }

    fn retry_git_cleanup_with_custody(
        &self,
        record: &AttemptRecord,
        recovery: Option<&provenant_supervisor::custody::Recovery>,
        call: &Request<'_>,
    ) -> Result<SettlementReceipts> {
        if !self.import_release_permitted(record, call)? {
            return Err(RunError::Conflict);
        }
        let request = self.request(record.attempt_id, call)?;
        let writer = request
            .task
            .writer
            .as_ref()
            .ok_or(RunError::Validation("attempt has no writer to clean up"))?;
        // A missing custody directory has no lock holder; keep the import pin.
        let slot = self.writer_custody_directory(record, writer).ok();
        let cleanup = (|| -> Result<()> {
            let held = match (recovery, &slot) {
                (Some(recovery), Some(slot)) if recovery.scope() == Self::scope(record) => recovery
                    .holds_writer_lock(slot)
                    .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?,
                _ => false,
            };
            // Refuse while another holder owns the writer lock.
            let _lock = match &slot {
                Some(slot) if !held => {
                    let observation = provenant_supervisor::custody::writer_admission_guard(slot)
                        .map_err(|error| {
                        RunError::Io(std::io::Error::other(error.to_string()))
                    })?;
                    let writer = slot.try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)?;
                    drop(observation);
                    Some(writer)
                }
                _ => None,
            };
            let attempt = self.attempt_dir(record.attempt_id, false)?;
            let head = crate::settlement::pinned_head(&attempt, Self::scope(record))?;
            let mut remove_import_pin = slot.is_some();
            if let Some(slot) = &slot {
                let own = slot.metadata()?;
                match self.writer_fence_listing() {
                    Ok(listing) => {
                        if !listing.issues.is_empty() {
                            remove_import_pin = false;
                        }
                        for fence in listing.reservations {
                            if fence.scope == Self::scope(record) {
                                continue;
                            }
                            if let Some(path) = fence.custody_directory() {
                                let other =
                                    provenant_os::fs::Directory::open_anchor(path)?.metadata()?;
                                if own.device == other.device && own.inode == other.inode {
                                    remove_import_pin = false;
                                }
                            }
                        }
                    }
                    Err(_) => remove_import_pin = false,
                }
            }
            crate::writer::cleanup_git_pins(
                self.context.state_root.as_path(),
                writer,
                &attempt,
                Self::scope(record),
                head,
                remove_import_pin,
            )
        })();
        let disposition = match cleanup {
            Ok(()) => EffectDisposition::Done,
            Err(error) => EffectDisposition::Failed {
                reason: error.to_string().chars().take(1024).collect(),
                retryable: false,
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
        let AttemptState::Terminal { .. } = next.state else {
            return Err(RunError::Validation("terminal state missing"));
        };
        let id = old.attempt_id;
        let request = self.request(old.attempt_id, call)?;
        let reaction = self.freeze_reaction(next, request.task.writer.is_some())?;
        let attempt = self.attempt_dir(id, false)?;
        let mut receipts = SettlementReceipts {
            cancellation: next.cancellation_disposition,
            ..Default::default()
        };
        if let Some(raw) = read_sealed(&attempt, "terminal-effects.json", RECORD_LIMIT)? {
            let effects: TerminalEffects = decode(&raw)?;
            if effects.schema_version != 1 || effects.scope != Self::scope(old) {
                return Err(RunError::Conflict);
            }
            for (effect, disposition) in effects.effects {
                *receipts.slot_mut(effect) = disposition;
            }
        }
        if read_sealed(&attempt, "delivery-cancelled.json", RECORD_LIMIT)?.is_some() {
            receipts.cancellation = Some(CancellationDisposition::DeliveryEffective);
        }
        self.write(call, |tx| {
            let changed = tx.execute("UPDATE attempts SET revision=?1,terminal=1,tmp_retained=1,record=?2 WHERE id=?3 AND revision=?4 AND terminal=0", params![sql_int(next.revision)?, crate::db::attempt_bytes(next)?, id.to_string(), sql_int(old.revision)?])?;
            if changed != 1 { return Err(RunError::Conflict); }
            if next.schema_version >= 2 {
                tx.execute("INSERT INTO settlement_receipts(attempt,generation,record) VALUES (?1,?2,?3)", params![id.to_string(), sql_int(next.generation)?, bytes(&receipts)?])?;
            }
            tx.execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
            Self::insert_reaction(tx, &reaction)?;
            insert_event(tx, next)?;
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
            && let Ok(attempt) = self.attempt_dir(id, false)
            && let Ok(Some(raw)) = read_sealed(&attempt, "cancel.json", RECORD_LIMIT)
            && decode::<CancellationEvidence>(&raw).is_ok_and(|request| {
                request.requests_stop(
                    Self::scope(old),
                    old.verified_owner.unwrap_or(self.context.actor_id),
                )
            })
        {
            let _ = self.record_cancellation(
                id,
                next.generation,
                CancellationDisposition::AcknowledgedIneffective,
                call,
            );
        }
        self.contribution_diagnostic(next.attempt_id, self.retain_contribution_route(next, call));
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
    let task = &input.request.task;
    if task
        .context
        .as_ref()
        .is_some_and(|context| !context.accepted() || !context.valid())
    {
        return Err(RunError::Validation("work context"));
    }
    bytes(&input.request)?;
    {
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
        if task.provider_timeout_seconds == 0
            || CanonicalV1::file_digest(input.prompt.as_bytes()) != task.prompt_digest
        {
            return Err(RunError::Validation("attempt or prompt identity"));
        }
    }
    if input.prompt.len() as u64 > RECORD_LIMIT {
        return Err(RunError::PromptTooLarge {
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
impl CancellationEvidence {
    /// The owner's own explicit stop request for exactly this attempt scope.
    pub(crate) fn requests_stop(
        &self,
        scope: provenant_supervisor::custody::Scope,
        owner: provenant_core::ActorId,
    ) -> bool {
        self.scope == scope
            && self.requester == owner
            && self.reason == provenant_supervisor::machine::StopReason::ExplicitCancellation
    }
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

#[cfg(test)]
mod admission_key_tests {
    use super::*;
    use crate::tests::{call, clock, context, input};

    #[test]
    fn graph_number_is_preserved_in_the_record_and_operation_replay()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        for number in 1..=3 {
            let mut prepared = input();
            prepared.attempt_id = AttemptId::from_bytes([number as u8; 16]);
            prepared.operation_id = OperationId::parse(&format!("number-{number}"))?;
            prepared.attempt_id = AttemptId::from_bytes([number as u8 + 10; 16]);
            crate::tests::rekey(&mut prepared);
            prepared.request.task.context = Some(provenant_records::run::WorkContext {
                graph_id: Some("numbered-graph".into()),
                node_id: Some("writer".into()),
                attempt: Some(number),
                ..Default::default()
            });
            let admitted = store.create(&prepared, &call)?;
            let record = store.record(prepared.attempt_id, &call)?;
            assert_eq!(record.attempt_number, u64::from(number));
            assert_eq!(record.key.attempt(), number);
            assert_eq!(
                store.create(&prepared, &call)?.attempt_id,
                admitted.attempt_id
            );
            let mut conflict = prepared;
            conflict.attempt_id = AttemptId::from_bytes([number as u8 + 20; 16]);
            conflict.operation_id = OperationId::parse(&format!("other-{number}"))?;
            assert_eq!(
                store.create(&conflict, &call)?.attempt_id,
                admitted.attempt_id
            );
            conflict.request.task.title = Some("different request".into());
            assert!(matches!(
                store.create(&conflict, &call),
                Err(RunError::OperationConflict)
            ));
        }
        Ok(())
    }
}
