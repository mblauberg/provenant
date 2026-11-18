use crate::{
    artifacts::{artifact_path, read_sealed, seal},
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
    pub scope: provenant_records::custody::Scope,
    pub effects: Vec<(SettlementEffect, EffectDisposition)>,
}

// A delivered stop remains a fact even when identity evidence prevents a
// cancelled terminal decision. Publication and crash replay use the same rule.
fn cancellation_outcome_is_consistent(
    delivered: Option<bool>,
    identity: ProviderIdentityEvidence,
    outcome: Outcome,
    cause: Option<TerminalCause>,
) -> bool {
    if delivered != Some(true) || outcome == Outcome::Cancelled {
        return true;
    }
    if identity.has_conflict() {
        outcome == Outcome::Failed && cause == Some(TerminalCause::IdentityMismatch)
    } else {
        identity.is_incomplete() && outcome == Outcome::Unsettled && cause.is_none()
    }
}

/// Writer and admission gates consume the supervisor's one stop protocol.
pub(crate) fn cancel_requested(
    attempt: &provenant_os::fs::Directory,
    scope: provenant_records::custody::Scope,
) -> bool {
    StopMarker::read(attempt, scope).map_or(true, StopMarker::stops)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopMarker {
    Absent,
    Steered { requester: provenant_core::ActorId },
    Stop,
}
impl StopMarker {
    pub(crate) fn read(
        attempt: &provenant_os::fs::Directory,
        scope: provenant_records::custody::Scope,
    ) -> Result<Self> {
        use provenant_supervisor::custody::{StopIntentState, read_stop_intent};
        Ok(
            match read_stop_intent(attempt, scope)
                .map_err(|error| RunError::Io(std::io::Error::other(error)))?
            {
                StopIntentState::Absent => Self::Absent,
                StopIntentState::Requested(request)
                    if request.reason == provenant_supervisor::machine::StopReason::Steered =>
                {
                    Self::Steered {
                        requester: request.requester,
                    }
                }
                _ => Self::Stop,
            },
        )
    }
    pub(crate) fn stops(self) -> bool {
        self == Self::Stop
    }
}

pub(crate) fn import_cancelled(
    attempt: &provenant_os::fs::Directory,
    scope: provenant_records::custody::Scope,
) -> Result<bool> {
    let marked = read_sealed(attempt, "import-cancelled.json", RECORD_LIMIT)?
        .map(|raw| crate::db::decode_column::<provenant_records::custody::Scope>(&raw))
        .transpose()?
        .is_some_and(|saved| saved == scope);
    if !marked {
        return Ok(false);
    }
    Ok(StopMarker::read(attempt, scope)?.stops())
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
    /// Provider handoff evidence decoded by the common terminal classifier.
    pub handoff: provenant_records::handoff::HandoffResult,
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
                || current.refusal_notice != expected.refusal_notice
            {
                return Err(RunError::Conflict);
            }
            current.refusal_notice_done = true;
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
        // The chair is a per-submission proposal, not request identity: a
        // replay of the same refused operation from a new chair session is
        // the same request, and the first record keeps its original chair.
        let digest = CanonicalRequest {
            chair: None,
            ..input.request.clone()
        }
        .digest()?;
        let refusal_notice = input
            .request
            .task
            .profile
            .knobs
            .get("chair.refusal_notice")
            .map(|notice| {
                serde_json::from_value(notice.clone())
                    .map_err(|_| RunError::Validation("refusal notice policy"))
            })
            .transpose()?;
        let operation = OperationRecord {
            extra: Default::default(),
            preflight_digest_version: None,
            refusal_notice,
            refusal_code: None,
            refusal_notice_done: false,
            origin_intent_digest: context.intent_digest,
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
            parent_attempt: None,
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
        let current = self.record(record.attempt_id, call)?;
        let Some(receipts) = current.settlement_receipts else {
            return Ok(true);
        };
        if matches!(
            receipts.git_import,
            EffectDisposition::Failed {
                retryable: true,
                ..
            } | EffectDisposition::Cancelled {
                retryable: true,
                ..
            }
        ) {
            return Ok(false);
        }
        if receipts.git_import == EffectDisposition::Pending
            && let Some(raw) = read_sealed(&attempt, "terminal-effects.json", RECORD_LIMIT)?
        {
            let effects: TerminalEffects = decode(&raw)?;
            if effects.schema_version != 1 || effects.scope != Self::scope(record) {
                return Err(RunError::Conflict);
            }
            // Unlike an untracked prelaunch receipt, outcome-only settlement
            // explicitly defers Git import. It must run before release.
            if effects.effects.iter().any(|(effect, disposition)| {
                *effect == SettlementEffect::GitImport && *disposition == EffectDisposition::Pending
            }) {
                return Ok(false);
            }
        }
        Ok(true)
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
        attempt: AttemptId,
        call: &Request<'_>,
    ) -> Result<Vec<AttemptId>> {
        let mut query = self.db.prepare("SELECT a.id,r.record FROM attempts a JOIN settlement_receipts r ON r.attempt=a.id WHERE a.id=?1 AND a.project=?2 AND a.terminal=1")?;
        let rows = query.query_map(
            params![attempt.to_string(), self.context.project_id.to_string()],
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

    pub(crate) fn effects_pending(
        &self,
        id: AttemptId,
        raw: &[u8],
        call: &Request<'_>,
    ) -> Result<bool> {
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
            // Recovery cannot establish that the orphan was projected.
            redacted: false,
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
        let verified = sealed_exit(&dir, exit)?;
        let intended = pending.exit_evidence.ok_or(RunError::Conflict)?;
        if verified != intended {
            // Both declared exit artifact classes carry the same scoped
            // provider observation. Its verified bytes, not its storage name,
            // identify a pre-existing terminal intent during crash recovery.
            if self.artifact_bytes(&verified, RECORD_LIMIT)?
                != self.artifact_bytes(&intended, RECORD_LIMIT)?
            {
                return Err(RunError::Conflict);
            }
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
        } else {
            self.release_admission(&record, call);
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
                || (stored.termination.is_none() && !stored.never_spawned)
                || observed != exit_code
                || pending.provider_launched != Some(!stored.never_spawned)
            {
                return Err(RunError::Conflict);
            }
            if pending.schema_version == 2 {
                let AttemptState::Terminal {
                    outcome,
                    terminal_cause,
                    ..
                } = pending.state
                else {
                    return Err(RunError::Conflict);
                };
                if (outcome == Outcome::Cancelled
                    && !stored.never_spawned
                    && stored.cancel_delivered_before_exit != Some(true)
                    && !(pending.terminal_evidence == Some(TerminalEvidence::ImportCancellation)
                        && import_cancelled(&dir, Self::scope(&current))?))
                    || (outcome == Outcome::Cancelled
                        && stored.never_spawned
                        && stored.stop_reason
                            != Some(
                                provenant_supervisor::machine::StopReason::ExplicitCancellation,
                            ))
                    || !cancellation_outcome_is_consistent(
                        stored.cancel_delivered_before_exit,
                        ProviderIdentityEvidence::classify(
                            stored.provider_identity_conflict,
                            stored.provider_identity_evidence_incomplete,
                            stored.provider_identity_eof_fragment,
                            stored.stop_reason,
                            stored.cancel_delivered_before_exit,
                        ),
                        outcome,
                        terminal_cause,
                    )
                {
                    return Err(RunError::Conflict);
                }
            }
        }
        if matches!(current.state, AttemptState::Terminal { .. }) {
            let mut decision = current.clone();
            decision.settlement_receipts = None;
            decision.custody_pending = None;
            // Delivery is a post-commit receipt. Admission phase stays frozen
            // in the terminal decision; release outcome lives in the port receipt.
            decision.delivery = pending.delivery.clone();
            let pending_decision = pending.clone();
            return if decision == pending_decision {
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
        let task = &input.request.task;
        let telemetry_context = crate::telemetry_context::capture(task, input.created_at);
        let operation = OperationRecord {
            extra: Default::default(),
            preflight_digest_version: None,
            refusal_notice: None,
            refusal_code: None,
            refusal_notice_done: false,
            origin_intent_digest: None,
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
            parent_attempt: input.request.parent_attempt,
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
            let attempt = AttemptRecord {extra: Default::default(), peer: Default::default(),
                budget_overrun: None, budget_wrap_up_refused: None,
                repository_admission_binding: None,
                key: key.clone(),
                context: task.context.clone(),
                title: task.title.clone(),
                continuation: input.request.fresh_continuation.as_ref().map(|_| "fresh".into()),
                profile: task.profile.clone(),
                limits: None,
                schema_version: 2, attempt_id, project_id: context.project_id, attempt_number: u64::from(key.attempt()), revision: 1, generation: 0,
                delivery_id: task.delivery_id, retry_of: input.request.parent, superseded_by: None, steering: None, requested_route: task.route.clone(), resolved_route: None, applied_route: None, observed_route: None, authority_digest: task.authority_digest, read_only_writable: Vec::new(), write_grants: Vec::new(), git_grants_digest: None, subject: None, head: None, protected_paths: Vec::new(), delivery: None, stop_work: None, verified_owner: Some(context.actor_id), session: None,
                queued_at: input.created_at, started_at: None, priority: None, requested_priority: None, last_output_at: None, provider_silent_for_ms: provenant_records::run::Transient(None), process_observation_valid: provenant_records::run::Transient(false), provider_launched: None, preparation_failure: None, provider_ms: None, admission_deadline: None, runtime_deadline: None, partial_signals: Vec::new(), state: AttemptState::Queued { extra: Default::default() }, usage: UsageRecord::default(),
                terminal_evidence: None, settlement_reason: None, cancellation_disposition: None, settlement_receipts: None, custody_pending: None,
                prompt: ArtifactRecord {extra: Default::default(),  path: artifact_path(attempt_id, AttemptFile::Prompt.name())?, digest: task.prompt_digest, bytes: input.prompt.len() as u64, complete: true, redacted: false }, result: None, publication_error: None, exit_evidence: None, skipped_routes: Vec::new(),
                trace: provenant_records::Trace {extra: Default::default(),
                    telemetry_context: Some(telemetry_context.clone()),
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
            self.queue_ticket(tx, &attempt, class, task.provider_timeout_seconds)?;
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
        {
            return Err(RunError::Validation(
                "exit/cleanup evidence is unresolved or out of scope",
            ));
        }
        if matches!(
            publication.outcome,
            Outcome::Succeeded | Outcome::ReviewRequired
        ) && (exit.never_spawned
            || (outcome_exit != Some(0)
                && !matches!(
                    SettlementReason::derive(
                        publication.outcome,
                        Some(&TerminalEvidence::ProcessExit),
                        &publication.partial_signals,
                        publication.terminal_cause,
                        publication.handoff.as_ref().map(Option::as_ref),
                    )
                    .rule()
                    .terminal_outcome,
                    provenant_records::run::SettlementTerminalOutcome::Succeeded
                )))
        {
            return Err(RunError::Validation("success lacks verified provider exit"));
        }
        if publication.outcome == Outcome::Cancelled
            && !exit.never_spawned
            && exit.cancel_delivered_before_exit != Some(true)
            && !import_cancelled(&self.attempt_dir(id, false)?, Self::scope(&old))?
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
        if !cancellation_outcome_is_consistent(
            exit.cancel_delivered_before_exit,
            ProviderIdentityEvidence::classify(
                exit.provider_identity_conflict,
                exit.provider_identity_evidence_incomplete,
                exit.provider_identity_eof_fragment,
                exit.stop_reason,
                exit.cancel_delivered_before_exit,
            ),
            publication.outcome,
            publication.terminal_cause,
        ) {
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
        if failure.is_some()
            && !matches!(
                expected,
                AttemptState::Queued { .. } | AttemptState::Starting { .. }
            )
        {
            return Err(RunError::Validation(
                "preparation evidence requires an unlaunched attempt",
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
        // The queued DB transaction can outlive an interrupted directory
        // publication. A starting attempt must retain its launch evidence.
        let dir = self.attempt_dir(id, matches!(expected, AttemptState::Queued { .. }))?;
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
                            terminal_cause,
                            terminal_note,
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
                                terminal_cause,
                                terminal_note,
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
        let safe_state = AttemptState::Terminal {
            extra: Default::default(),
            terminal_cause,
            terminal_note: terminal_note.clone(),
            stop: None,
            outcome,
            exit_code: None,
            finished_at,
            reason: reason.clone(),
        };
        let refusal_result = provenant_records::wire::lane::LaneRow::state_and_reason(&safe_state)
            .1
            .map(|text| provenant_redact::bounded_line(&text, 256))
            .unwrap_or_default();
        let publication = TerminalPublication {
            handoff: Ok(None),
            terminal_cause,
            terminal_note,
            outcome,
            finished_at,
            started_at: None,
            provider_ms: None,
            reason,
            result: &refusal_result,
            complete: true,
            redacted: true,
            resolved_route: old.resolved_route.clone(),
            applied_route: old.applied_route.clone(),
            observed_route: old.observed_route.clone(),
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
                    self.release_admission(&record, call);
                    return self.record(id, call).map(Some);
                }
                if !preserve_evidence {
                    self.settle_cleanup(&record, None, call);
                } else {
                    self.release_admission(&record, call);
                }
                self.record(id, call).map(Some)
            }
            other => other,
        }
    }
    pub(crate) fn record_admission_binding(
        &self,
        expected: &AttemptRecord,
        binding: RepositoryAdmissionBinding,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        if !matches!(expected.state, AttemptState::Queued { .. })
            || expected.repository_admission_binding.is_some()
        {
            return Err(RunError::Conflict);
        }
        let mut next = expected.clone();
        next.revision = expected.revision.checked_add(1).ok_or(RunError::Conflict)?;
        next.repository_admission_binding = Some(binding);
        super::reader::validate_attempt(&next)?;
        self.write(call, |tx| {
            let changed = tx.execute(
                "UPDATE attempts SET revision=?1,record=?2 WHERE id=?3 AND revision=?4 AND terminal=0",
                params![
                    sql_int(next.revision)?,
                    crate::db::attempt_bytes(&next)?,
                    next.attempt_id.to_string(),
                    sql_int(expected.revision)?,
                ],
            )?;
            if changed != 1 {
                return Err(RunError::Conflict);
            }
            insert_event(tx, &next)?;
            Ok(())
        })?;
        self.record(expected.attempt_id, call)
    }

    pub(crate) fn record_admission_phase(
        &self,
        expected: &AttemptRecord,
        phase: RepositoryAdmissionPhase,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        let mut binding = expected
            .repository_admission_binding
            .clone()
            .ok_or(RunError::Conflict)?;
        if !binding.valid() {
            return Err(RunError::Validation(
                "stored repository admission binding digest",
            ));
        }
        if binding.phase == phase {
            return Ok(expected.clone());
        }
        if matches!(expected.state, AttemptState::Terminal { .. }) {
            return Err(RunError::Conflict);
        }
        let active_owner = matches!(
            &expected.state,
            AttemptState::Queued { .. } | AttemptState::Starting { .. }
        );
        if !binding.can_transition_to(phase, active_owner) {
            return Err(RunError::Conflict);
        }
        if binding.phase == RepositoryAdmissionPhase::Released
            && phase == RepositoryAdmissionPhase::AcquirePending
        {
            binding.lease_epoch = binding
                .lease_epoch
                .checked_add(1)
                .ok_or(RunError::Conflict)?;
        }
        binding.phase = phase;
        let mut next = expected.clone();
        next.revision = expected.revision.checked_add(1).ok_or(RunError::Conflict)?;
        // These terminal projections have independent durable owners.
        next.settlement_receipts = None;
        next.custody_pending = None;
        next.repository_admission_binding = Some(binding);
        super::reader::validate_attempt(&next)?;
        let terminal = matches!(&expected.state, AttemptState::Terminal { .. });
        self.write(call, |tx| {
            let changed = tx.execute(
                "UPDATE attempts SET revision=?1,record=?2 WHERE id=?3 AND project=?4 AND revision=?5 AND terminal=?6",
                params![
                    sql_int(next.revision)?,
                    crate::db::attempt_bytes(&next)?,
                    next.attempt_id.to_string(),
                    self.context.project_id.to_string(),
                    sql_int(expected.revision)?,
                    terminal,
                ],
            )?;
            if changed != 1 {
                return Err(RunError::Conflict);
            }
            insert_event(tx, &next)?;
            Ok(())
        })?;
        self.record(expected.attempt_id, call)
    }

    pub(crate) fn settle_cleanup(
        &self,
        record: &AttemptRecord,
        recovery: Option<&provenant_supervisor::custody::Recovery>,
        call: &Request<'_>,
    ) {
        if !self.release_admission(record, call) {
            let _ = self.set_temporary_retained(record.attempt_id, true, call);
            return;
        }
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
        // Reuse verified same-scope custody, including a read-only guardian's
        // attempt lock. A second open would contend with our own description.
        let held = match recovery.filter(|recovery| recovery.scope() == Self::scope(record)) {
            Some(recovery) => match recovery.holds_writer_lock(&attempt_dir) {
                Ok(held) => held,
                Err(_) => {
                    let _ = self.set_temporary_retained(record.attempt_id, true, call);
                    return;
                }
            },
            None => false,
        };
        let _lease = if held {
            None
        } else {
            let Ok(observation) =
                provenant_supervisor::custody::writer_admission_guard(&attempt_dir)
            else {
                let _ = self.set_temporary_retained(record.attempt_id, true, call);
                return;
            };
            let lease = attempt_dir.try_lock(OsStr::new("writer.lock"), LockMode::Exclusive);
            drop(observation);
            let Ok(lease) = lease else {
                let _ = self.set_temporary_retained(record.attempt_id, true, call);
                return;
            };
            Some(lease)
        };
        if !matches!(
            provenant_supervisor::custody::reservation_present(&attempt_dir, Self::scope(record)),
            Ok(false)
        ) {
            let _ = self.set_temporary_retained(record.attempt_id, true, call);
            return;
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
            || !super::reader::route_identity(
                publication.resolved_route.as_ref(),
                publication.applied_route.as_ref(),
                publication.observed_route.as_ref(),
            )
            .permits_success(&publication.partial_signals))
        {
            return Err(RunError::Validation(
                "success lacks complete result or verified provider identity",
            ));
        }
        let mut next = old.clone();
        next.revision = expected_revision.checked_add(1).ok_or(RunError::Conflict)?;
        crate::launch::mark_admission_release_pending(&mut next)?;
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
        let import_cancellation = publication.outcome == Outcome::Cancelled
            && import_cancelled(&self.attempt_dir(id, false)?, Self::scope(&old))?;
        let terminal_evidence = if import_cancellation {
            TerminalEvidence::ImportCancellation
        } else if publication.outcome == Outcome::Unsettled {
            TerminalEvidence::Unsettled {
                reason: publication
                    .reason
                    .clone()
                    .ok_or(RunError::Validation("unsettled reason missing"))?,
            }
        } else if publication.outcome == Outcome::Cancelled
            && publication.terminal_cause != Some(TerminalCause::Steered)
            && cancellation_delivered == Some(true)
        {
            TerminalEvidence::EffectiveCancellation
        } else if durable_exit.is_some() {
            TerminalEvidence::ProcessExit
        } else {
            TerminalEvidence::PreLaunchRefusal
        };
        let classification = SettlementReason::classify(
            publication.outcome,
            Some(&terminal_evidence),
            &next.partial_signals,
            publication.terminal_cause,
            publication.handoff.as_ref().map(Option::as_ref),
        )
        .map_err(|_| RunError::Validation("terminal classification could not be sealed"))?;
        next.state = AttemptState::Terminal {
            extra: Default::default(),
            terminal_cause: publication.terminal_cause,
            terminal_note: publication
                .terminal_note
                .as_deref()
                .map(provenant_records::diagnostic::safe_text),
            stop: classification.stop,
            outcome: classification.outcome,
            exit_code: outcome_exit,
            finished_at: publication.finished_at,
            reason: publication.reason.clone(),
        };
        next.schema_version = 2;
        next.terminal_evidence = Some(terminal_evidence);
        next.settlement_reason = Some(classification.reason);
        next.publication_error = None;
        next.exit_evidence = durable_exit;
        let attempt = self.attempt_dir(id, false)?;
        if import_cancellation {
            next.cancellation_disposition = Some(CancellationDisposition::ImportEffective);
        } else if publication.outcome == Outcome::Cancelled
            && publication.terminal_cause != Some(TerminalCause::Steered)
            && (!launched || cancellation_delivered == Some(true))
        {
            next.cancellation_disposition = Some(CancellationDisposition::Effective);
        } else {
            use provenant_supervisor::custody::{StopIntentState, read_stop_intent};
            match read_stop_intent(&attempt, Self::scope(&old)) {
                Ok(StopIntentState::Requested(request))
                    if request.reason
                        == provenant_supervisor::machine::StopReason::ExplicitCancellation
                        && request.requester
                            == old.verified_owner.unwrap_or(self.context.actor_id) =>
                {
                    next.cancellation_disposition = Some(
                        if publication.terminal_cause == Some(TerminalCause::Steered) {
                            CancellationDisposition::Unknown
                        } else {
                            match cancellation_delivered {
                                Some(true) => CancellationDisposition::Effective,
                                Some(false) => CancellationDisposition::AcknowledgedIneffective,
                                None => CancellationDisposition::Unknown,
                            }
                        },
                    );
                }
                Err(_) | Ok(StopIntentState::Invalid) => {
                    next.cancellation_disposition = Some(CancellationDisposition::Unknown);
                }
                _ => {}
            }
        }
        if let Some(raw) = read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)? {
            let writer: crate::writer::WriterTerminal = decode(&raw)?;
            next.head = Some(writer.head);
            next.stop_work = writer.stop_work;
        }
        if classification.outcome == Outcome::Succeeded {
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
        scope: provenant_records::custody::Scope,
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
            drop(_publication);
            self.release_admission(&old, call);
            return Ok(());
        }
        let mut next = old.clone();
        next.revision = old.revision.checked_add(1).ok_or(RunError::Conflict)?;
        crate::launch::mark_admission_release_pending(&mut next)?;
        next.schema_version = 2;
        next.terminal_evidence = Some(TerminalEvidence::Lost {
            reason: reason.into(),
        });
        next.settlement_reason = Some(SettlementReason::Lost);
        next.state = AttemptState::Terminal {
            extra: Default::default(),
            terminal_cause: None,
            terminal_note: None,
            stop: None,
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
                    drop(_publication);
                    if committed {
                        self.settle_cleanup(&record, None, call);
                    } else {
                        self.release_admission(&record, call);
                    }
                    return Ok(());
                }
                if saved.attempt_id != old.attempt_id
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
        let result = self.commit_terminal_record(&old, &next, call);
        drop(_publication);
        if result.is_ok() {
            self.release_admission(&next, call);
        }
        result
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
        if !self.release_admission(record, call) {
            self.set_temporary_retained(record.attempt_id, true, call)?;
            return Err(RunError::Conflict);
        }
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

    fn terminal_steer_disposition(
        &self,
        child: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<Option<crate::controls::SteerContinuationDisposition>> {
        let Some(parent) = self.steer_parent_for_successor(child.attempt_id, call)? else {
            return Ok(None);
        };
        if child.provider_launched != Some(true) {
            return Ok(Some(crate::controls::SteerContinuationDisposition {
                parent,
                successor: child.attempt_id,
                state: "not_continued",
                cause: Some(provenant_records::run::SteeringCause::ContinuationRefused),
            }));
        }

        let exit = child
            .exit_evidence
            .as_ref()
            .and_then(|evidence| self.artifact_bytes(evidence, RECORD_LIMIT).ok())
            .and_then(|raw| decode::<SealedExit>(&raw).ok());
        let live = self.steer_session_ready(child.attempt_id, call)?;
        let sealed_route_digest = child
            .applied_route
            .as_ref()
            .map(crate::controls::steer_route_digest)
            .transpose()?;
        let terminal_unsettled = matches!(
            child.state,
            AttemptState::Terminal {
                outcome: Outcome::Unsettled,
                ..
            }
        );

        let live_conflict = live.as_ref().is_some_and(|ready| {
            ready.conflict
                || ready.generation != child.generation
                || sealed_route_digest.as_deref() != Some(ready.route_digest.as_str())
                || ready.session.as_deref().is_some_and(|session| {
                    child
                        .session
                        .as_deref()
                        .is_some_and(|terminal| terminal != session)
                })
        });
        let identity_evidence = exit.as_ref().map(|exit| {
            ProviderIdentityEvidence::classify(
                exit.provider_identity_conflict,
                exit.provider_identity_evidence_incomplete,
                exit.provider_identity_eof_fragment,
                exit.stop_reason,
                exit.cancel_delivered_before_exit,
            )
        });
        let identity_conflict =
            live_conflict || identity_evidence.is_some_and(ProviderIdentityEvidence::has_conflict);
        let strict_identity = self
            .resume_policy(child.attempt_id, call)?
            .rule("PROVIDER_IDENTITY_EVIDENCE_INCOMPLETE")
            .rule
            .strength
            == provenant_records::policy::Strength::Refuse;
        let evidence_incomplete = terminal_unsettled
            || identity_evidence.is_none()
            || (strict_identity
                && identity_evidence.is_some_and(ProviderIdentityEvidence::is_incomplete));
        let disposition = self.steer_successor_disposition(
            child.attempt_id,
            child,
            crate::controls::SteerSuccessorProof {
                session: child.session.as_deref(),
                route_digest: sealed_route_digest.as_deref(),
                identity_conflict,
                evidence_incomplete,
            },
            call,
        )?;
        if disposition
            .as_ref()
            .is_some_and(|disposition| disposition.parent != parent)
        {
            return Err(RunError::Validation("steer successor parent changed"));
        }
        Ok(disposition)
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
        let writer = match self.request(old.attempt_id, call) {
            Ok(request) => request.task.writer.is_some(),
            // A queued row may survive before its request directory is
            // published. With no launch there are no writer effects to report.
            Err(RunError::NotFound) if matches!(old.state, AttemptState::Queued { .. }) => false,
            Err(error) => return Err(error),
        };
        let reaction = self.freeze_reaction(next, writer)?;
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
            // Read the last live identity row under the same immediate
            // transaction that commits the terminal disposition. A readiness
            // or conflict event can otherwise race between classification and
            // the attempt's terminal update.
            let steer_disposition = self.terminal_steer_disposition(next, call)?;
            let changed = tx.execute("UPDATE attempts SET revision=?1,terminal=1,tmp_retained=1,record=?2 WHERE id=?3 AND revision=?4 AND terminal=0", params![sql_int(next.revision)?, crate::db::attempt_bytes(next)?, id.to_string(), sql_int(old.revision)?])?;
            if changed != 1 { return Err(RunError::Conflict); }
            if next.schema_version >= 2 {
                tx.execute("INSERT INTO settlement_receipts(attempt,generation,record) VALUES (?1,?2,?3)", params![id.to_string(), sql_int(next.generation)?, bytes(&receipts)?])?;
            }
            tx.execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
            if let Some(disposition) = &steer_disposition {
                crate::controls::record_steer_disposition(tx, disposition)?;
            }
            Self::insert_reaction(tx, &reaction)?;
            insert_event(tx, next)?;
            #[cfg(test)]
            crate::tests::crash_at("transaction");
            Ok(())
        })?;
        // A stop accepted after the decision read cancel.json (but before this
        // commit became visible) is acknowledged here from the host-sealed,
        // scope- and requester-checked request, not from mutable lane state.
        // A stop sealed later is acknowledged by `cancel` seeing the terminal row.
        if next.cancellation_disposition.is_none()
            && let Ok(attempt) = self.attempt_dir(id, false)
            && StopMarker::read(&attempt, Self::scope(old)).is_ok_and(StopMarker::stops)
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

// Supervisor owns this extensible file. Run decodes the fields it consumes;
// the bounded codec still rejects duplicate members and malformed records.
#[derive(serde::Deserialize)]
struct SealedExit {
    scope: provenant_records::custody::Scope,
    termination: Option<Termination>,
    never_spawned: bool,
    stop_reason: Option<provenant_supervisor::machine::StopReason>,
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    cancel_delivered_before_exit: Option<bool>,
    provider_identity_conflict: bool,
    provider_identity_evidence_incomplete: bool,
    provider_identity_eof_fragment: bool,
    terminal_observed: bool,
    unresolved_cleanup: bool,
    settlement_evidence: provenant_supervisor::custody::SettlementEvidence,
}
fn sealed_exit(
    dir: &provenant_os::fs::Directory,
    exit: &ExitObservation,
) -> Result<ArtifactRecord> {
    use provenant_supervisor::custody::PublicationState;
    if !matches!(exit.publication, PublicationState::Durable)
        || (exit.termination.is_none() && !exit.never_spawned)
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
        || stored.provider_identity_conflict != exit.provider_identity_conflict
        || stored.provider_identity_evidence_incomplete
            != exit.provider_identity_evidence_incomplete
        || stored.provider_identity_eof_fragment != exit.provider_identity_eof_fragment
        || stored.terminal_observed != exit.terminal_observed
        || stored.unresolved_cleanup != exit.unresolved_cleanup
        || stored.settlement_evidence != exit.settlement_evidence
    {
        return Err(RunError::Validation(
            "supervisor exit record differs from observation",
        ));
    }
    file.sync()?;
    dir.sync()?;
    // Cleanup may later replace process-exit.json. The outcome names its own
    // immutable provider snapshot so crash replay never follows mutable bytes.
    let name = AttemptFile::ProviderExit.name();
    let saved = if let Some(saved) = read_sealed(dir, name, RECORD_LIMIT)? {
        let snapshot: SealedExit = decode(&saved)?;
        if snapshot.scope != stored.scope
            || snapshot.termination != stored.termination
            || snapshot.never_spawned != stored.never_spawned
            || snapshot.stop_reason != stored.stop_reason
            || snapshot.cancel_delivered_before_exit != stored.cancel_delivered_before_exit
            || snapshot.provider_identity_conflict != stored.provider_identity_conflict
            || snapshot.provider_identity_evidence_incomplete
                != stored.provider_identity_evidence_incomplete
            || snapshot.provider_identity_eof_fragment != stored.provider_identity_eof_fragment
        {
            return Err(RunError::Conflict);
        }
        saved
    } else {
        seal(dir, name, &raw, RECORD_LIMIT)?;
        raw
    };
    Ok(ArtifactRecord {
        extra: Default::default(),
        path: artifact_path(exit.scope.attempt, name)?,
        digest: CanonicalV1::file_digest(&saved),
        bytes: saved.len() as u64,
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
