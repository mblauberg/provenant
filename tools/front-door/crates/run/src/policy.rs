//! Bounded policy retries stay in the same run, preserving each failed receipt.
use crate::{
    artifacts::{artifact_path, read_sealed, seal},
    db::{bytes, decode, sql_int},
    store::insert_event,
    *,
};
use provenant_core::AttemptId;
use provenant_records::{AttemptFile, CanonicalV1};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRetry {
    pub schema_version: u64,
    pub route: RequestedRoute,
    pub resume: bool,
    /// Attempts in this run including its first, capped again by the store.
    pub max_attempts: u64,
    pub instruction: String,
}
#[derive(Default)]
pub struct ResultAssessment {
    pub refusal: Option<String>,
    pub retry: Option<PolicyRetry>,
}

impl RunStore {
    pub(crate) fn policy_attempt_number(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<u64> {
        let first: Option<i64> = call.retry(|| {
            Ok(self.db.query_row(
                "SELECT MIN(number) FROM attempts WHERE run=?1 AND task=?2 AND project=?3",
                params![
                    record.run_id.to_string(),
                    record.task_id.to_string(),
                    record.project_id.to_string()
                ],
                |row| row.get(0),
            )?)
        })?;
        record
            .attempt_number
            .checked_sub(
                first
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or(RunError::Validation("policy first attempt"))?,
            )
            .and_then(|n| n.checked_add(1))
            .ok_or(RunError::Validation("policy attempt ordinal"))
    }

    pub(crate) fn staged_policy_retry(
        &self,
        record: &AttemptRecord,
    ) -> Result<Option<PolicyRetry>> {
        let directory = self.attempt_dir(record.run_id, record.attempt_id, false)?;
        read_sealed(&directory, AttemptFile::PolicyRetry.name(), RECORD_LIMIT)?
            .map(|raw| decode(&raw))
            .transpose()
    }
    /// Follow only host-authored policy successors. Exact record/output APIs
    /// continue to expose every attempt independently.
    pub fn policy_tail(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptRecord> {
        use rusqlite::OptionalExtension;
        let mut current = self.record(id, call)?;
        if self.request(current.run_id, call)?.parent_attempt.is_some() {
            return Ok(current);
        }
        for _ in 0..9 {
            let next: Option<String> = call.retry(|| {
                Ok(self
                    .db
                    .query_row(
                        "SELECT id FROM attempts WHERE run=?1 AND task=?2 AND number=?3",
                        params![
                            current.run_id.to_string(),
                            current.task_id.to_string(),
                            sql_int(current.attempt_number.saturating_add(1))?
                        ],
                        |row| row.get(0),
                    )
                    .optional()?)
            })?;
            let Some(next) = next else {
                return Ok(current);
            };
            let next = self.record(
                AttemptId::parse(&next).map_err(|_| RunError::Validation("policy successor ID"))?,
                call,
            )?;
            if next.retry_of != Some(current.attempt_id)
                || !matches!(
                    next.continuation.as_deref(),
                    Some("policy_continuation" | "policy_fallback")
                )
            {
                return Ok(current);
            }
            current = next;
        }
        Err(RunError::Validation("policy successor chain limit"))
    }
    pub(crate) fn stage_policy_retry(
        &self,
        record: &AttemptRecord,
        retry: &PolicyRetry,
        call: &Request<'_>,
    ) -> Result<()> {
        let request = self.request(record.run_id, call)?;
        // Child stores are lane-writable. Automatic retries cannot introduce
        // a second source of launch authority there.
        if self.sealed_requester.is_some()
            || request.parent_attempt.is_some()
            || self.context.state_root.as_path() != self.writer_registry()
        {
            return Ok(());
        }
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(RunError::Validation("policy retry task missing"))?;
        if retry.schema_version != 1
            || (!retry.resume && (task.writer.is_some() || !task.access.write_roots.is_empty()))
            || retry.max_attempts > 9
            || retry.max_attempts < 2
            || retry.instruction.is_empty()
            || retry.instruction.len() > 4096
        {
            return Err(RunError::Validation("policy retry bounds"));
        }
        if self.staged_policy_retry(record)?.is_some() {
            // The first sealed decision survives crashes and expiring rules.
            return Ok(());
        }
        if self.policy_attempt_number(record, call)? >= retry.max_attempts {
            return Ok(());
        }
        let directory = self.attempt_dir(record.run_id, record.attempt_id, false)?;
        seal(
            &directory,
            AttemptFile::PolicyRetry.name(),
            &bytes(retry)?,
            RECORD_LIMIT,
        )
    }

    /// Prepare artefacts before the terminal transaction. Its INSERT and the
    /// old attempt's terminal UPDATE are atomic, so readers never see a closed
    /// run between a bad result and its replacement.
    pub(crate) fn policy_successor(
        &self,
        old: &AttemptRecord,
        terminal: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<Option<AttemptRecord>> {
        let directory = self.attempt_dir(old.run_id, old.attempt_id, false)?;
        if matches!(
            terminal.state,
            AttemptState::Terminal {
                outcome: Outcome::Cancelled | Outcome::TimedOut | Outcome::Lost,
                ..
            }
        ) || read_sealed(&directory, AttemptFile::Cancel.name(), RECORD_LIMIT)?.is_some()
        {
            return Ok(None);
        }
        if let Some(exit) = read_sealed(&directory, AttemptFile::ProcessExit.name(), RECORD_LIMIT)?
        {
            let exit: serde_json::Value = decode(&exit)?;
            if exit
                .get("stop_reason")
                .is_some_and(|reason| !reason.is_null())
            {
                return Ok(None);
            }
        }
        let Some(raw) = read_sealed(&directory, AttemptFile::PolicyRetry.name(), RECORD_LIMIT)?
        else {
            return Ok(None);
        };
        let retry: PolicyRetry = decode(&raw)?;
        if self.policy_attempt_number(old, call)? >= retry.max_attempts || retry.max_attempts > 9 {
            return Err(RunError::Validation("policy retry attempt limit"));
        }
        let request = self.request(old.run_id, call)?;
        if self.sealed_requester.is_some()
            || request.parent_attempt.is_some()
            || self.context.state_root.as_path() != self.writer_registry()
        {
            return Err(RunError::Validation(
                "policy retry requires a host-owned store",
            ));
        }
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == old.task_id)
            .ok_or(RunError::Validation("policy retry task missing"))?;
        if !retry.resume && (task.writer.is_some() || !task.access.write_roots.is_empty()) {
            return Err(RunError::Validation(
                "policy fallback requires read-only authority",
            ));
        }
        let prompt = self.artifact_bytes(old.run_id, &old.prompt, RECORD_LIMIT)?;
        let prompt = format!(
            "{}\n\n{}",
            String::from_utf8(prompt).map_err(|_| RunError::Validation("policy prompt UTF-8"))?,
            retry.instruction
        );
        if prompt.len() as u64 > RECORD_LIMIT {
            return Err(RunError::TooLarge {
                limit: RECORD_LIMIT,
            });
        }
        let digest =
            CanonicalV1::file_digest(format!("policy-retry/{}", old.attempt_id).as_bytes());
        let mut id = [0; 16];
        id.copy_from_slice(&digest.bytes()[..16]);
        let id = AttemptId::from_bytes(id);
        let AttemptState::Terminal { finished_at, .. } = terminal.state else {
            return Err(RunError::Nonterminal);
        };
        let mut next = old.clone();
        next.attempt_id = id;
        next.attempt_number = old
            .attempt_number
            .checked_add(1)
            .ok_or(RunError::Conflict)?;
        next.revision = 1;
        next.generation = 0;
        next.continuation = Some(
            if retry.resume {
                "policy_continuation"
            } else {
                "policy_fallback"
            }
            .into(),
        );
        next.retry_of = Some(old.attempt_id);
        next.requested_route = retry.route;
        next.resolved_route = None;
        next.applied_route = None;
        next.observed_route = None;
        next.read_only_writable.clear();
        next.write_grants.clear();
        next.git_grants_digest = None;
        next.granted_tracking_ref = None;
        next.subject = None;
        next.head = None;
        next.protected_paths.clear();
        next.delivery = None;
        next.session = None;
        next.queued_at = finished_at;
        next.started_at = None;
        next.priority = None;
        next.requested_priority = None;
        next.last_output_at = None;
        next.provider_launched = None;
        next.provider_ms = None;
        next.admission_deadline = None;
        next.runtime_deadline = None;
        next.state = AttemptState::Queued;
        next.usage = UsageRecord::default();
        next.prompt = ArtifactRecord {
            path: artifact_path(id, "prompt.txt")?,
            digest: CanonicalV1::file_digest(prompt.as_bytes()),
            bytes: prompt.len() as u64,
            complete: true,
            redacted: false,
        };
        next.result = None;
        next.publication_error = None;
        next.exit_evidence = None;
        next.skipped_routes.clear();
        next.trace.attempt = Some(id);
        next.trace.parent = Some(old.attempt_id);
        crate::reader::validate_attempt(&next)?;
        let directory = self.attempt_dir(old.run_id, id, true)?;
        seal(&directory, "prompt.txt", prompt.as_bytes(), RECORD_LIMIT)?;
        Ok(Some(next))
    }

    pub(crate) fn insert_policy_successor(
        &self,
        tx: &rusqlite::Connection,
        next: &AttemptRecord,
    ) -> Result<()> {
        let raw: Vec<u8> = tx.query_row(
            "SELECT record FROM admission WHERE attempt=?1",
            [next
                .retry_of
                .ok_or(RunError::Validation("policy parent missing"))?
                .to_string()],
            |row| row.get(0),
        )?;
        let ticket: AdmissionTicket = decode(&raw)?;
        tx.execute(
            "INSERT INTO continuations(target,run) VALUES (?1,?2)",
            params![
                next.retry_of
                    .ok_or(RunError::Validation("policy parent missing"))?
                    .to_string(),
                next.run_id.to_string()
            ],
        )?;
        tx.execute("INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) VALUES (?1,?2,?3,?4,?5,1,0,?6)", params![next.attempt_id.to_string(), next.run_id.to_string(), next.project_id.to_string(), next.task_id.to_string(), sql_int(next.attempt_number)?, bytes(next)?])?;
        insert_event(tx, next)?;
        self.queue_ticket(
            tx,
            next,
            ticket.lane_class,
            ticket.run_concurrency,
            0,
            ticket.disk_floor_bytes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, exit, input, publication, seal_observation};

    #[test]
    fn policy_retry_is_atomic_bounded_and_preserves_the_failed_receipt()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        seal_observation(&store, &input, &exit())?;
        let id = input.attempts[0].attempt_id;
        let old = store.record(id, &call)?;
        let retry = PolicyRetry {
            schema_version: 1,
            route: RequestedRoute::Model {
                provider: "codex".into(),
                model: "codex/gpt-6-luna".into(),
                effort: Some("low".into()),
            },
            resume: false,
            max_attempts: 2,
            instruction: "Complete the original task.".into(),
        };
        store.stage_policy_retry(&old, &retry, &call)?;
        let mut changed_policy = retry.clone();
        changed_policy.route = RequestedRoute::Model {
            provider: "codex".into(),
            model: "codex/gpt-6.1-sol".into(),
            effort: Some("high".into()),
        };
        store.stage_policy_retry(&old, &changed_policy, &call)?;
        let mut publication = publication();
        publication.outcome = Outcome::Failed;
        publication.reason = Some("empty restricted result".into());
        let terminal = store.publish_terminal(id, 1, &publication, &exit(), &call)?;
        assert!(matches!(
            terminal.state,
            AttemptState::Terminal {
                outcome: Outcome::Failed,
                ..
            }
        ));
        assert_eq!(store.run(input.run_id, &call)?.closure, Closure::Open);
        let queued = store.pending_attempts(input.run_id, &call)?;
        assert_eq!(queued.len(), 1);
        let next = &queued[0];
        assert_eq!(next.attempt_number, 2);
        assert_eq!(store.policy_tail(id, &call)?.attempt_id, next.attempt_id);
        assert_eq!(next.retry_of, Some(id));
        assert_eq!(next.authority_digest, old.authority_digest);
        assert_eq!(next.requested_route, retry.route);
        assert!(next.applied_route.is_none());
        assert_eq!(
            store.artifact_bytes(input.run_id, &next.prompt, RECORD_LIMIT)?,
            b"exact prompt\n\nComplete the original task."
        );
        // Replaying publication cannot enqueue a second replacement.
        assert_eq!(
            store.publish_terminal(id, 1, &publication, &exit(), &call)?,
            terminal
        );
        assert_eq!(store.pending_attempts(input.run_id, &call)?.len(), 1);
        store.stage_policy_retry(next, &retry, &call)?;
        let directory = store.attempt_dir(input.run_id, next.attempt_id, false)?;
        assert!(read_sealed(&directory, AttemptFile::PolicyRetry.name(), RECORD_LIMIT)?.is_none());
        Ok(())
    }

    #[test]
    fn policy_fallback_cannot_use_write_authority_or_child_stores()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut input = input();
        input.request.tasks[0]
            .access
            .write_roots
            .push(temp.path().to_owned());
        store.create(&input, &call)?;
        let old = store.record(input.attempts[0].attempt_id, &call)?;
        let retry = PolicyRetry {
            schema_version: 1,
            route: old.requested_route.clone(),
            resume: false,
            max_attempts: 2,
            instruction: "Retry.".into(),
        };
        assert!(matches!(
            store.stage_policy_retry(&old, &retry, &call),
            Err(RunError::Validation("policy retry bounds"))
        ));
        let child_store = store.with_sealed_requester(provenant_core::ActorId::from_bytes([8; 16]));
        child_store.stage_policy_retry(&old, &retry, &call)?;
        let directory = child_store.attempt_dir(input.run_id, old.attempt_id, false)?;
        assert!(read_sealed(&directory, AttemptFile::PolicyRetry.name(), RECORD_LIMIT)?.is_none());
        Ok(())
    }

    #[test]
    fn a_fresh_resumed_run_gets_its_own_policy_retry_allowance()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        seal_observation(&store, &input, &exit())?;
        let id = input.attempts[0].attempt_id;
        store.publish_terminal(id, 1, &publication(), &exit(), &call)?;
        let resume = Resume {
            prior: id,
            expected_generation: 0,
            operation_id: provenant_core::OperationId::parse("policy-new-run")?,
            run_id: provenant_core::RunId::from_bytes([10; 16]),
            attempt_id: AttemptId::from_bytes([11; 16]),
            prompt: "new work".into(),
            created_at: provenant_core::WallTime::from_micros(300)?,
        };
        store.resume_fresh(&resume, "new work", &call)?;
        let old = store.record(resume.attempt_id, &call)?;
        assert_eq!(old.attempt_number, 2);
        let retry = PolicyRetry {
            schema_version: 1,
            route: old.requested_route.clone(),
            resume: false,
            max_attempts: 2,
            instruction: "Retry the new task.".into(),
        };
        store.stage_policy_retry(&old, &retry, &call)?;
        let directory = store.attempt_dir(old.run_id, old.attempt_id, false)?;
        assert!(read_sealed(&directory, AttemptFile::PolicyRetry.name(), RECORD_LIMIT)?.is_some());
        Ok(())
    }

    #[test]
    fn cancellation_suppresses_an_interrupted_policy_retry()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        seal_observation(&store, &input, &exit())?;
        let id = input.attempts[0].attempt_id;
        let old = store.record(id, &call)?;
        store.stage_policy_retry(
            &old,
            &PolicyRetry {
                schema_version: 1,
                route: old.requested_route.clone(),
                resume: false,
                max_attempts: 2,
                instruction: "Retry.".into(),
            },
            &call,
        )?;
        let directory = store.attempt_dir(input.run_id, id, false)?;
        seal(
            &directory,
            AttemptFile::Cancel.name(),
            b"{\"schema_version\":1}",
            RECORD_LIMIT,
        )?;
        let mut publication = publication();
        publication.outcome = Outcome::Cancelled;
        store.publish_terminal(id, 1, &publication, &exit(), &call)?;
        assert!(store.pending_attempts(input.run_id, &call)?.is_empty());
        assert_eq!(store.run(input.run_id, &call)?.closure, Closure::Closed);
        Ok(())
    }
}
