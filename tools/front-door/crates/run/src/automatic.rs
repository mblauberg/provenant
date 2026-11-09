//! A sealed successor decision, committed with its predecessor's terminal row.
use crate::{
    artifacts::{artifact_path, read_sealed, seal},
    *,
};
use provenant_core::AttemptId;
use provenant_os::fs::Directory;
use provenant_records::handoff::{Handoff, HandoffStatus};
use provenant_records::{AttemptFile, CanonicalV1};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

const DECISION: &str = "automatic-continuation.json";

#[derive(Serialize, Deserialize)]
pub(crate) struct AutomaticContinuation {
    record: AttemptRecord,
}

#[derive(Serialize, Deserialize)]
struct ContinuationDecision {
    schema_version: u64,
    continuation: Option<AutomaticContinuation>,
}

impl RunStore {
    /// Decide once, before sealing terminal intent. Recovery replays this exact
    /// choice, including the policy allowance observed at the original launch.
    pub(crate) fn seal_automatic_continuation(
        &self,
        old: &AttemptRecord,
        attempt: &Directory,
        eligible: bool,
        handoff: Option<&Handoff>,
        call: &Request<'_>,
    ) -> Result<()> {
        if read_sealed(attempt, DECISION, TERMINAL_RECORD_LIMIT)?.is_some() {
            return Ok(());
        }
        let request = self.request(old.run_id, call)?;
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == old.task_id)
            .ok_or(RunError::NotFound)?;
        let used = old
            .extra
            .get("automatic_continuations")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let limit = old
            .applied_route
            .as_ref()
            .and_then(|route| route.extra.get("max_continuations"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let decision = if eligible
            && task.writer.is_some()
            && used < limit
            && handoff.is_some_and(|h| h.status == HandoffStatus::StoppedEarly)
        {
            let mut original = old.clone();
            while let Some(parent) = original.retry_of {
                let previous = self.record(parent, call)?;
                if previous.run_id != old.run_id {
                    break;
                }
                original = previous;
            }
            let original = self.artifact_bytes(old.run_id, &original.prompt, RECORD_LIMIT)?;
            let original = String::from_utf8(original)
                .map_err(|_| RunError::Validation("original prompt UTF-8"))?;
            let prompt = original;
            let id = AttemptId::from_bytes(provenant_os::fs::entropy_128()?);
            let directory = self.attempt_dir(old.run_id, id, true)?;
            seal(
                &directory,
                AttemptFile::Prompt.name(),
                prompt.as_bytes(),
                RECORD_LIMIT,
            )?;
            let mut record = old.clone();
            record.attempt_id = id;
            record.attempt_number += 1;
            record.revision = 1;
            record.generation = 0;
            record.extra = Default::default();
            record
                .extra
                .insert("automatic_continuations".into(), (used + 1).into());
            record.retry_of = Some(old.attempt_id);
            record.superseded_by = None;
            record.continuation = Some("automatic".into());
            record.state = AttemptState::Queued {
                extra: Default::default(),
            };
            record.queued_at = call
                .clock
                .wall_now()
                .map_err(|_| RunError::Validation("wall clock"))?;
            record.started_at = None;
            record.last_output_at = None;
            record.resolved_route = None;
            record.applied_route = None;
            record.observed_route = None;
            record.session = None;
            record.result = None;
            record.head = None;
            record.subject = None;
            record.delivery = None;
            record.usage = Default::default();
            record.partial_signals.clear();
            record.read_only_writable.clear();
            record.write_grants.clear();
            record.git_grants_digest = None;
            record.granted_tracking_ref = None;
            record.protected_paths.clear();
            record.priority = None;
            record.requested_priority = None;
            record.provider_launched = None;
            record.provider_ms = None;
            record.preparation_failure = None;
            record.admission_deadline = None;
            record.runtime_deadline = None;
            record.terminal_evidence = None;
            record.settlement_reason = None;
            record.cancellation_disposition = None;
            record.settlement_receipts = None;
            record.custody_pending = None;
            record.publication_error = None;
            record.exit_evidence = None;
            record.skipped_routes.clear();
            record.prompt = ArtifactRecord {
                extra: Default::default(),
                path: artifact_path(id, AttemptFile::Prompt.name())?,
                digest: CanonicalV1::file_digest(prompt.as_bytes()),
                bytes: prompt.len() as u64,
                complete: true,
                redacted: false,
            };
            record.trace.attempt = Some(id);
            record.trace.parent = Some(old.attempt_id);
            Some(AutomaticContinuation { record })
        } else {
            None
        };
        let decision = ContinuationDecision {
            schema_version: 1,
            continuation: decision,
        };
        seal(
            attempt,
            DECISION,
            &crate::db::bytes(&decision)?,
            TERMINAL_RECORD_LIMIT,
        )
    }

    pub(crate) fn automatic_continuation(
        &self,
        record: &AttemptRecord,
    ) -> Result<Option<AutomaticContinuation>> {
        let attempt = self.attempt_dir(record.run_id, record.attempt_id, false)?;
        read_sealed(&attempt, DECISION, TERMINAL_RECORD_LIMIT)?
            .map(|raw| {
                let decision: ContinuationDecision = crate::db::decode(&raw)?;
                Ok(decision.continuation)
            })
            .transpose()
            .map(Option::flatten)
    }

    pub(crate) fn queue_automatic_continuation(
        &self,
        tx: &Connection,
        successor: &AutomaticContinuation,
        request: &CanonicalRequest,
    ) -> Result<()> {
        let record = &successor.record;
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(RunError::NotFound)?;
        tx.execute("INSERT INTO attempts(id,run,project,task,number,revision,terminal,record) VALUES (?1,?2,?3,?4,?5,1,0,?6)",
            params![record.attempt_id.to_string(), record.run_id.to_string(), record.project_id.to_string(), record.task_id.to_string(), crate::db::sql_int(record.attempt_number)?, crate::db::attempt_bytes(record)?])?;
        crate::store::insert_event(tx, record)?;
        let floor = provenant_config::GcSettings::read(&self.writer_registry)?
            .free_floor_bytes(&self.writer_registry)?;
        self.queue_ticket(
            tx,
            record,
            LaneClass::Writer,
            request.concurrency,
            crate::admission::Allowance {
                queue_seconds: 0,
                runtime_seconds: task.provider_timeout_seconds,
            },
            floor,
        )?;
        Ok(())
    }
}

impl AutomaticContinuation {
    pub(crate) fn id(&self) -> AttemptId {
        self.record.attempt_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, exit, input, publication, route, seal_observation};

    #[test]
    fn stopped_early_queues_one_visible_successor_in_the_terminal_transaction()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for (status, writer, eligible, limit, used, expected) in [
            (Some("stopped_early"), true, true, 2, 0, true),
            (Some("stopped_early"), true, true, 2, 1, true),
            (Some("stopped_early"), true, true, 2, 2, false),
            (Some("stopped_early"), true, true, 0, 0, false),
            (Some("done"), true, true, 2, 0, false),
            (Some("blocked"), true, true, 2, 0, false),
            (None, true, true, 2, 0, false),
            (Some("malformed"), true, true, 2, 0, false),
            (Some("stopped_early"), false, true, 2, 0, false),
            (Some("stopped_early"), true, false, 2, 0, false),
        ] {
            let temp = tempfile::tempdir()?;
            let clock = clock();
            let cancel = provenant_core::CancelToken::new();
            let call = call(&clock, &cancel);
            let store = RunStore::open(&context(temp.path())?, &call)?;
            let mut input = input();
            if writer {
                input.request.tasks[0].writer = Some(WriterRequest {
                    extra: Default::default(),
                    project: temp.path().into(),
                    worktree: temp.path().join("worktree"),
                });
            }
            input.request.tasks[0].context = Some(WorkContext {
                graph_id: Some("graph-fixture".into()),
                node_id: Some("writer".into()),
                ..Default::default()
            });
            store.create(&input, &call)?;
            let id = input.attempts[0].attempt_id;
            let mut old = store.record(id, &call)?;
            let mut applied = route();
            applied
                .extra
                .insert("max_continuations".into(), limit.into());
            old.applied_route = Some(applied);
            old.extra
                .insert("automatic_continuations".into(), used.into());
            let attempt = store.attempt_dir(old.run_id, id, false)?;
            let handoff = status.and_then(|status| Handoff::from_json(serde_json::json!({
                "schema_version":2,"status":status,"note":"Continue renderer", "remaining":["Finish renderer"],"decisions":[],"verify":[]
            }).to_string().as_bytes()).ok());
            store.seal_automatic_continuation(&old, &attempt, eligible, handoff.as_ref(), &call)?;
            // A repeated settlement cannot spend the allowance again or change
            // an already sealed decision when policy changes.
            store.seal_automatic_continuation(&old, &attempt, false, None, &call)?;
            seal_observation(&store, &input, &exit())?;
            let mut publication = publication();
            publication.outcome = Outcome::ReviewRequired;
            let terminal = store.publish_terminal(id, 1, &publication, &exit(), &call)?;
            let pending = store.pending_attempts(old.run_id, &call)?;
            assert_eq!(
                pending.len(),
                usize::from(expected),
                "{status:?}, {limit}, {used}"
            );
            assert_eq!(terminal.superseded_by.is_some(), expected);
            assert_eq!(
                store.run(old.run_id, &call)?.closure == Closure::Closed,
                !expected
            );
            if let Some(successor) = pending.first() {
                assert_eq!(terminal.superseded_by, Some(successor.attempt_id));
                assert_eq!(successor.retry_of, Some(id));
                assert_eq!(successor.context, input.request.tasks[0].context);
                assert_eq!(successor.extra["automatic_continuations"], used + 1);
                assert_eq!(
                    store.artifact_bytes(old.run_id, &successor.prompt, RECORD_LIMIT)?,
                    b"exact prompt"
                );
                assert_eq!(successor.trace.parent, Some(id));
                let events =
                    Service::new(&store).events(0, 64, Some(successor.attempt_id), &call)?;
                assert_eq!(events.len(), 1);
                assert!(matches!(events[0].change, AttemptState::Queued { .. }));
            }
            // Publication replay keeps the same successor and terminal record.
            assert_eq!(
                store.replay_terminal(id, &call)?.superseded_by,
                terminal.superseded_by
            );
        }
        Ok(())
    }

    #[test]
    fn automatic_continuation_decision_schema_round_trips()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let record = store.record(input.attempts[0].attempt_id, &call)?;
        for successor in [false, true] {
            let continuation = successor.then(|| AutomaticContinuation {
                record: record.clone(),
            });
            let decision = ContinuationDecision {
                schema_version: 1,
                continuation,
            };
            let bytes = crate::db::bytes(&decision)?;
            let decoded: ContinuationDecision = crate::db::decode(&bytes)?;
            assert_eq!(decoded.schema_version, 1);
            assert_eq!(decoded.continuation.is_some(), successor);
        }
        Ok(())
    }
}
