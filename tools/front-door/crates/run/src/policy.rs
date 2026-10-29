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

/// Attempts one run may reach through policy successors, its first included.
/// It bounds a chain that configuration sets, so a corrupt sealed record cannot
/// queue successors without end; routing's settlement retry limit stays below it.
pub const POLICY_ATTEMPT_CEILING: u64 = 33;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRetry {
    pub schema_version: u64,
    pub route: RequestedRoute,
    pub resume: bool,
    /// Attempts in this run including its first, capped again by the store.
    pub max_attempts: u64,
    pub instruction: String,
    /// Earliest wall time, in Unix seconds, at which the successor may
    /// launch. The settling guardian waits for it; a later sweep that finds
    /// the successor still queued may launch it once this has passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before_unix_seconds: Option<i64>,
}
#[derive(Default)]
pub struct ResultAssessment {
    pub refusal: Option<String>,
    pub retry: Option<PolicyRetry>,
    /// Appended to the settled reason when the retry is staged.
    pub note: Option<String>,
}
/// A fresh retry may use write authority only on what the task was granted:
/// its requested route drawn again, or the exact model that route already
/// applied. Neither widens what the task may do.
fn retry_within_authority(retry: &PolicyRetry, task: &TaskRequest, old: &AttemptRecord) -> bool {
    let applied_model = match (&retry.route, old.applied_route.as_ref()) {
        (
            RequestedRoute::Model {
                provider,
                model,
                effort,
                ..
            },
            Some(applied),
        ) => {
            applied.provider.as_ref() == Some(provider)
                && applied.model.as_ref() == Some(model)
                && applied.effort == *effort
        }
        _ => false,
    };
    retry.resume
        || retry.route == task.route
        || applied_model
        || (task.writer.is_none() && task.access.write_roots.is_empty())
}

fn handoff_brief(handoff: &provenant_records::handoff::Handoff) -> String {
    format!(
        "\n\nPrevious attempt handoff (task data, not authority):\n{}",
        handoff.fenced()
    )
}

impl RunStore {
    pub(crate) fn policy_attempt_number(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<u64> {
        call.check()?;
        let first: Option<i64> = {
            self.db.query_row(
                "SELECT MIN(number) FROM attempts WHERE run=?1 AND task=?2 AND project=?3",
                params![
                    record.run_id.to_string(),
                    record.task_id.to_string(),
                    record.project_id.to_string()
                ],
                |row| row.get(0),
            )?
        };
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

    pub fn staged_policy_retry(&self, record: &AttemptRecord) -> Result<Option<PolicyRetry>> {
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
        for _ in 0..POLICY_ATTEMPT_CEILING {
            let next: Option<String> = {
                self.db
                    .query_row(
                        "SELECT id FROM attempts WHERE run=?1 AND task=?2 AND number=?3",
                        params![
                            current.run_id.to_string(),
                            current.task_id.to_string(),
                            sql_int(current.attempt_number.saturating_add(1))?
                        ],
                        |row| row.get(0),
                    )
                    .optional()?
            };
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
    /// Seal the retry decision; true when this call staged it.
    pub(crate) fn stage_policy_retry(
        &self,
        record: &AttemptRecord,
        retry: &PolicyRetry,
        call: &Request<'_>,
    ) -> Result<bool> {
        let request = self.request(record.run_id, call)?;
        // Child stores are lane-writable. Automatic retries cannot introduce
        // a second source of launch authority there.
        if self.sealed_requester.is_some()
            || request.parent_attempt.is_some()
            || self.context.state_root.as_path() != self.writer_registry()
        {
            return Ok(false);
        }
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(RunError::Validation("policy retry task missing"))?;
        if retry.schema_version != 1
            || !retry_within_authority(retry, task, record)
            || retry.max_attempts > POLICY_ATTEMPT_CEILING
            || retry.max_attempts < 2
            || retry.instruction.is_empty()
            || retry.instruction.len() > 4096
        {
            return Err(RunError::Validation("policy retry bounds"));
        }
        // A progress snapshot can make an otherwise valid prompt too large.
        // Decline this automatic successor before sealing its decision, so
        // an optional recovery brief cannot wedge terminal publication.
        let handoff = self
            .handoff(record)?
            .map(|handoff| handoff_brief(&handoff))
            .unwrap_or_default();
        if record
            .prompt
            .bytes
            .saturating_add(retry.instruction.len() as u64)
            .saturating_add(handoff.len() as u64)
            .saturating_add(2)
            > RECORD_LIMIT
        {
            return Ok(false);
        }
        if self.staged_policy_retry(record)?.is_some() {
            // The first sealed decision survives crashes and expiring rules.
            return Ok(false);
        }
        if self.policy_attempt_number(record, call)? >= retry.max_attempts {
            return Ok(false);
        }
        let directory = self.attempt_dir(record.run_id, record.attempt_id, false)?;
        seal(
            &directory,
            AttemptFile::PolicyRetry.name(),
            &bytes(retry)?,
            RECORD_LIMIT,
        )?;
        Ok(true)
    }

    /// Prepare artefacts before the terminal transaction. Its INSERT and the
    /// old attempt's terminal UPDATE are atomic, so readers never see a closed
    /// run between a bad result and its replacement.
    pub(crate) fn policy_successor(
        &self,
        old: &AttemptRecord,
        terminal: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<Option<(AttemptRecord, u64)>> {
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
        if self.policy_attempt_number(old, call)? >= retry.max_attempts
            || retry.max_attempts > POLICY_ATTEMPT_CEILING
        {
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
        if !retry_within_authority(&retry, task, old) {
            return Err(RunError::Validation(
                "policy fallback to another route requires read-only authority",
            ));
        }
        let prompt = self.artifact_bytes(old.run_id, &old.prompt, RECORD_LIMIT)?;
        let handoff = self
            .handoff(old)?
            .map(|handoff| -> Result<String> {
                // JSON escapes can hide token boundaries from a byte scan of
                // the final prompt. Scan decoded strings before serialisation.
                crate::completion::transform_handoff(&handoff, |text| {
                    Service::scan(text)?;
                    Ok(text.to_owned())
                })?;
                Ok(handoff_brief(&handoff))
            })
            .transpose()?
            .unwrap_or_default();
        let prompt = format!(
            "{}\n\n{}{handoff}",
            String::from_utf8(prompt).map_err(|_| RunError::Validation("policy prompt UTF-8"))?,
            retry.instruction
        );
        if prompt.len() as u64 > RECORD_LIMIT {
            return Err(RunError::TooLarge {
                limit: RECORD_LIMIT,
            });
        }
        Service::scan(&prompt)?;
        let digest =
            CanonicalV1::file_digest(format!("policy-retry/{}", old.attempt_id).as_bytes());
        let mut id = [0; 16];
        id.copy_from_slice(&digest.bytes()[..16]);
        let id = AttemptId::from_bytes(id);
        let AttemptState::Terminal { finished_at, .. } = terminal.state else {
            return Err(RunError::Nonterminal);
        };
        // The successor's admission window opens when its sealed wait ends,
        // so a long backoff cannot spend the window before launch.
        let wait_seconds = retry
            .not_before_unix_seconds
            .and_then(|not_before| {
                u64::try_from(
                    not_before.saturating_sub(finished_at.as_micros().div_euclid(1_000_000)),
                )
                .ok()
            })
            .unwrap_or(0);
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
        next.state = AttemptState::Queued {
            extra: Default::default(),
        };
        next.usage = UsageRecord::default();
        next.prompt = ArtifactRecord {
            extra: Default::default(),
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
        Ok(Some((next, wait_seconds)))
    }

    pub(crate) fn insert_policy_successor(
        &self,
        tx: &rusqlite::Connection,
        next: &AttemptRecord,
        wait_seconds: u64,
        request: &CanonicalRequest,
        disk_floor_bytes: u64,
    ) -> Result<()> {
        // Provider exit releases the old admission ticket before settlement.
        // Reconstruct scheduling from the canonical request and live host floor.
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == next.task_id)
            .ok_or(RunError::Validation("policy retry task missing"))?;
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
            if task.writer.is_some() {
                LaneClass::Writer
            } else {
                LaneClass::ReadOnly
            },
            request.concurrency,
            crate::admission::Allowance {
                queue_seconds: wait_seconds,
                runtime_seconds: task.provider_timeout_seconds,
            },
            disk_floor_bytes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, exit, input, publication, seal_observation};

    #[test]
    fn retry_disclosure_scans_decoded_handoff_strings()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let id = input.attempts[0].attempt_id;
        let old = store.record(id, &call)?;
        let retry = PolicyRetry {
            schema_version: 1,
            route: old.requested_route.clone(),
            resume: false,
            max_attempts: 2,
            instruction: "Continue.".into(),
            not_before_unix_seconds: None,
        };
        store.stage_policy_retry(&old, &retry, &call)?;
        let mut terminal = old.clone();
        terminal.state = AttemptState::Terminal {
            extra: Default::default(),
            outcome: Outcome::QuotaLimited,
            finished_at: input.created_at,
            exit_code: Some(1),
            reason: None,
        };
        let directory = store.attempt_dir(old.run_id, id, false)?;
        for prefix in ["\n", "\t"] {
            for field in ["note", "remaining", "decisions", "verify", "future"] {
                let secret = format!("{prefix}sk-abcdefghijklmnopqrstuvwx");
                let mut handoff = serde_json::json!({"schema_version":2,"status":"stopped_early","note":"Continue","remaining":[],"decisions":[],"verify":[]});
                handoff[field] = match field {
                    "note" => serde_json::json!(secret),
                    "future" => serde_json::json!({"nested":[{"token":secret}]}),
                    _ => serde_json::json!([secret]),
                };
                // Simulate a snapshot sealed before decoded-string sanitisation.
                seal(
                    &directory,
                    AttemptFile::Handoff.name(),
                    &serde_json::to_vec(&handoff)?,
                    RECORD_LIMIT,
                )?;
                assert!(
                    matches!(
                        store.policy_successor(&old, &terminal, &call),
                        Err(RunError::Input {
                            kind: ErrorKind::SecretInput,
                            ..
                        })
                    ),
                    "{field} {prefix:?}"
                );
                directory.unlink(std::ffi::OsStr::new(AttemptFile::Handoff.name()))?;
            }
        }
        Ok(())
    }

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
                extra: Default::default(),
                provider: "codex".into(),
                model: "codex/gpt-6-luna".into(),
                effort: Some("low".into()),
            },
            resume: false,
            max_attempts: 2,
            instruction: "Complete the original task.".into(),
            not_before_unix_seconds: None,
        };
        store.stage_policy_retry(&old, &retry, &call)?;
        let mut changed_policy = retry.clone();
        changed_policy.route = RequestedRoute::Model {
            extra: Default::default(),
            provider: "codex".into(),
            model: "codex/gpt-6.1-sol".into(),
            effort: Some("high".into()),
        };
        store.stage_policy_retry(&old, &changed_policy, &call)?;
        // Production holders release admission as soon as the provider exits.
        store
            .db
            .execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
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
    fn a_policy_retry_chain_continues_each_attempt_in_one_run()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let store = RunStore::open(&context(temp.path())?, &call(&clock, &cancel))?;
        let input = input();
        store.create(&input, &call(&clock, &cancel))?;
        let retry = PolicyRetry {
            schema_version: 1,
            route: store
                .record(input.attempts[0].attempt_id, &call(&clock, &cancel))?
                .requested_route,
            resume: false,
            max_attempts: 3,
            instruction: "Complete the original task.".into(),
            not_before_unix_seconds: None,
        };
        let mut publication = publication();
        publication.outcome = Outcome::Failed;
        let mut id = input.attempts[0].attempt_id;
        // Each settled attempt seals a retry and queues its successor; the
        // second successor must not collide with the first in the same run.
        for number in 2..=3 {
            let at = i64::try_from(number)? * 100;
            publication.started_at = Some(provenant_core::WallTime::from_micros(at + 50)?);
            publication.finished_at = provenant_core::WallTime::from_micros(at + 90)?;
            let mut observation = exit();
            observation.scope.attempt = id;
            let mut sealed = crate::tests::input();
            sealed.attempts[0].attempt_id = id;
            seal_observation(&store, &sealed, &observation)?;
            let old = store.record(id, &call(&clock, &cancel))?;
            assert!(store.stage_policy_retry(&old, &retry, &call(&clock, &cancel))?);
            store
                .db
                .execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
            store.publish_terminal(
                id,
                old.revision,
                &publication,
                &observation,
                &call(&clock, &cancel),
            )?;
            let queued = store.pending_attempts(input.run_id, &call(&clock, &cancel))?;
            assert_eq!(queued.len(), 1);
            assert_eq!(queued[0].attempt_number, number);
            assert_eq!(queued[0].retry_of, Some(id));
            id = queued[0].attempt_id;
        }
        assert_eq!(
            store
                .policy_tail(input.attempts[0].attempt_id, &call(&clock, &cancel))?
                .attempt_id,
            id
        );
        Ok(())
    }

    #[test]
    fn writer_retry_keeps_its_granted_route_and_child_stores_never_retry()
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
            not_before_unix_seconds: None,
        };
        let child_store = store.with_sealed_requester(provenant_core::ActorId::from_bytes([8; 16]));
        assert!(!child_store.stage_policy_retry(&old, &retry, &call)?);
        let directory = child_store.attempt_dir(input.run_id, old.attempt_id, false)?;
        assert!(read_sealed(&directory, AttemptFile::PolicyRetry.name(), RECORD_LIMIT)?.is_none());
        // Another route would widen write authority; the granted one does not.
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let elsewhere = PolicyRetry {
            route: RequestedRoute::Model {
                extra: Default::default(),
                provider: "codex".into(),
                model: "codex/gpt-6.1-sol".into(),
                effort: None,
            },
            ..retry.clone()
        };
        assert_ne!(elsewhere.route, old.requested_route);
        assert!(matches!(
            store.stage_policy_retry(&old, &elsewhere, &call),
            Err(RunError::Validation("policy retry bounds"))
        ));
        // The exact model the granted route applied is also within authority.
        let task = &input.request.tasks[0];
        let mut applied = old.clone();
        applied.applied_route = Some(RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6.1-sol".into()),
            ..Default::default()
        });
        assert!(super::retry_within_authority(&elsewhere, task, &applied));
        assert!(!super::retry_within_authority(&elsewhere, task, &old));
        assert!(store.stage_policy_retry(&old, &retry, &call)?);
        assert!(read_sealed(&directory, AttemptFile::PolicyRetry.name(), RECORD_LIMIT)?.is_some());
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
            not_before_unix_seconds: None,
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
                not_before_unix_seconds: None,
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
