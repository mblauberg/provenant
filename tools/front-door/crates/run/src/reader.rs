use crate::{
    artifacts::artifact_path,
    db::{decode, sql_int},
    *,
};
use provenant_confinement::valid_read_only_writable;
use provenant_core::{ActorId, AttemptId, OperationId, RunId, TaskId, WallTime};
use provenant_records::{AttemptFact, AttemptFile, OutputRef, ReviewerFact, ReviewerProvenance};
use rusqlite::{OptionalExtension, params};
use std::collections::VecDeque;

const PROCESS_OBSERVATION_MAX_AGE_MICROS: i64 = 5_000_000;

#[derive(Clone, Copy, Debug, Default)]
pub struct AttemptFilter {
    pub run_id: Option<RunId>,
    pub task_id: Option<TaskId>,
}
pub trait AttemptReader {
    fn attempt(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptFact>;
    fn attempts<'a>(&'a self, filter: AttemptFilter, call: &'a Request<'a>)
    -> Result<Attempts<'a>>;
}
pub struct Attempts<'a> {
    store: &'a RunStore,
    call: &'a Request<'a>,
    filter: AttemptFilter,
    after: i64,
    through: i64,
    page: VecDeque<(i64, String)>,
    done: bool,
}
impl Attempts<'_> {
    fn page(&self) -> Result<VecDeque<(i64, String)>> {
        let mut statement = self.store.db.prepare("SELECT sequence,id FROM attempts WHERE project=?1 AND terminal=1 AND sequence>?2 AND sequence<=?3 AND (?4 IS NULL OR run=?4) AND (?5 IS NULL OR task=?5) ORDER BY sequence LIMIT 64")?;
        let rows = statement.query_map(
            params![
                self.store.context.project_id.to_string(),
                self.after,
                self.through,
                self.filter.run_id.map(|id| id.to_string()),
                self.filter.task_id.map(|id| id.to_string())
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(rows.collect::<std::result::Result<VecDeque<_>, _>>()?)
    }
}
impl Iterator for Attempts<'_> {
    type Item = Result<AttemptFact>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.page.is_empty() {
            let result = self.page();
            match result {
                Ok(page) => self.page = page,
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
        let Some((sequence, id)) = self.page.pop_front() else {
            self.done = true;
            return None;
        };
        self.after = sequence;
        let fact = AttemptId::parse(&id)
            .map_err(|_| RunError::Validation("attempt index ID"))
            .and_then(|id| self.store.attempt(id, self.call));
        if fact.is_err() {
            self.done = true;
        }
        Some(fact)
    }
}
/// Reviewer attribution. The submitter owns the attempt but is not the
/// reviewer: each dispatched attempt is its own execution identity, derived
/// from the attempt ID, so a chair's dispatched reviews are never its own
/// decision-maker reviews and distinct attempts stay distinct reviewers.
/// An observed provider is an adapter, not a model family; run holds no
/// catalogue, so the family stays unknown here and the edge fills it from the
/// routing catalogue.
fn reviewer_fact(
    attempt: AttemptId,
    owner: Option<ActorId>,
    observed: Option<&RouteRecord>,
) -> Option<ReviewerFact> {
    owner?;
    let digest = provenant_records::CanonicalV1::file_digest(
        &[
            b"provenant.reviewer-execution.v1:".as_slice(),
            attempt.as_bytes(),
        ]
        .concat(),
    );
    let mut id = [0; 16];
    id.copy_from_slice(&digest.bytes()[..16]);
    let observed_provider = observed
        .and_then(|r| r.provider.as_deref())
        .is_some_and(|p| !p.is_empty());
    Some(ReviewerFact {
        provenance: if observed_provider {
            ReviewerProvenance::Observed
        } else {
            ReviewerProvenance::Unknown
        },
        actor_id: ActorId::from_bytes(id),
        family: None,
        model: observed.and_then(|r| r.model.clone()),
    })
}
/// The published fact for a terminal record. A record read from a peer takes the
/// same path as one read here.
pub fn fact(r: AttemptRecord) -> Result<AttemptFact> {
    let AttemptState::Terminal {
        outcome,
        exit_code,
        finished_at,
        reason,
        ..
    } = r.state
    else {
        return Err(RunError::Nonterminal);
    };
    let output_refs = match r.result {
        Some(output) => vec![OutputRef {
            path: output.path,
            digest: output.digest,
        }],
        None if outcome == Outcome::Lost && exit_code.is_none() => Vec::new(),
        None => return Err(RunError::Validation("terminal result missing")),
    };
    let transcript = r
        .applied_route
        .as_ref()
        .and_then(|route| route.provider.clone())
        .zip(r.session.clone())
        .map(|(adapter, session)| provenant_records::TranscriptRef { adapter, session });
    Ok(AttemptFact {
        context: r.context,
        attempt_id: r.attempt_id,
        task_id: r.task_id,
        attempt_number: r.attempt_number,
        operation_id: r.operation_id,
        project_id: r.project_id,
        terminal_state: outcome.into(),
        exit_code,
        queued_at: r.queued_at,
        started_at: r.started_at,
        provider_ms: r.provider_ms,
        finished_at,
        resolved_route: r.resolved_route.map(Into::into),
        usage: r.usage.into(),
        reviewer: reviewer_fact(r.attempt_id, r.verified_owner, r.observed_route.as_ref()),
        delivery_id: r.delivery_id,
        subject: r.subject,
        head: r.head,
        output_refs,
        requested_route: Some(r.requested_route.projection()),
        applied_route: r.applied_route.map(Into::into),
        observed_route: r.observed_route.map(Into::into),
        verified_owner: r.verified_owner,
        lineage: r.retry_of.into_iter().collect(),
        reason,
        trace: r.trace,
        transcript,
    })
}
impl AttemptReader for RunStore {
    fn attempt(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptFact> {
        fact(self.record(id, call)?)
    }
    fn attempts<'a>(
        &'a self,
        filter: AttemptFilter,
        call: &'a Request<'a>,
    ) -> Result<Attempts<'a>> {
        let through = {
            self.db.query_row(
                "SELECT COALESCE(MAX(sequence),0) FROM attempts WHERE project=?1",
                [self.context.project_id.to_string()],
                |r| r.get(0),
            )?
        };
        Ok(Attempts {
            store: self,
            call,
            filter,
            after: 0,
            through,
            page: VecDeque::new(),
            done: false,
        })
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ByteRange {
    pub offset: u64,
    pub max_bytes: u64,
}
impl Default for ByteRange {
    fn default() -> Self {
        Self {
            offset: 0,
            max_bytes: OUTPUT_LIMIT,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub enum ArtifactPart {
    Prompt,
    Result,
    /// Supervisor's bounded stderr capture; possibly not UTF-8.
    Stderr,
    /// The provider's own transcript, found in the attempt's retained provider state.
    Transcript,
}
#[derive(Debug)]
pub struct OutputChunk {
    pub bytes: Vec<u8>,
    /// Total length of the artifact, for tail reads.
    pub len: u64,
    pub offset: u64,
    pub next_offset: u64,
    pub eof: bool,
    pub clipped: bool,
    pub complete: bool,
    pub encoding: &'static str,
    pub digest: provenant_records::FileDigest,
}
/// Exact lane selectors; task and attempt narrow a run.
#[derive(Clone, Copy, Debug)]
pub struct LaneTarget {
    pub run: RunId,
    pub task: Option<TaskId>,
    pub attempt: Option<AttemptId>,
}
/// Optional lane filters: the submitting actor, and terminal lanes already seen.
const UNSEEN_OWNED: &str = " AND (?5 IS NULL OR run IN (SELECT run FROM operations WHERE project=?1 AND actor=?5)) AND (?6 IS NULL OR terminal=0 OR EXISTS(SELECT 1 FROM events WHERE attempt=attempts.id AND sequence>?6))";
const UNSEEN_TARGET: &str = " AND (?7 IS NULL OR terminal=0 OR EXISTS(SELECT 1 FROM events WHERE attempt=attempts.id AND sequence>?7))";
#[derive(Clone, Debug)]
pub struct LaneQuery {
    /// Exact selections include terminal lanes. Without them the page holds
    /// active lanes, or every retained lane when `all` is set.
    pub targets: Vec<LaneTarget>,
    pub all: bool,
    pub limit: u64,
    /// Exclusive attempt-sequence cursor.
    pub after: u64,
    /// Return highest sequence first; `after` then acts as an exclusive upper bound.
    pub newest_first: bool,
    /// Without targets, restrict the page to runs this actor submitted.
    pub owner: Option<ActorId>,
    /// Drop terminal lanes whose last event is at or before this cursor.
    pub unseen_after: Option<u64>,
}
#[derive(Debug)]
pub struct LanePage {
    pub lanes: Vec<AttemptRecord>,
    pub next: Option<u64>,
}
/// How a record read treats the sealed process checkpoint. The mode is chosen
/// explicitly by the caller: mutations and recovery need a durable read, views
/// only need to report what is already published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordRead {
    Durable,
    Observed,
}
impl RunStore {
    /// Read only the stored prompt or result for a console search. Other
    /// artifact classes can contain credentials or provider state and are
    /// deliberately outside this API.
    pub fn console_text_artifact(
        &self,
        record: &AttemptRecord,
        part: ArtifactPart,
        limit: u64,
        call: &Request<'_>,
    ) -> Result<Option<Vec<u8>>> {
        call.check()?;
        let artifact = match part {
            ArtifactPart::Prompt => Some(&record.prompt),
            ArtifactPart::Result => record.result.as_ref(),
            ArtifactPart::Stderr | ArtifactPart::Transcript => {
                return Err(RunError::Validation("console text artifact part"));
            }
        };
        artifact
            .map(|artifact| self.artifact_bytes(record.run_id, artifact, limit))
            .transpose()
    }

    pub fn operation(
        &self,
        id: &OperationId,
        call: &Request<'_>,
    ) -> Result<Option<OperationRecord>> {
        call.check()?;
        let raw: Option<Vec<u8>> = self
            .db
            .query_row(
                "SELECT record FROM operations WHERE project=?1 AND actor=?2 AND operation=?3",
                params![
                    self.context.project_id.to_string(),
                    self.context.actor_id.to_string(),
                    id.as_str()
                ],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|raw| {
            let op: OperationRecord = decode(&raw)?;
            if op.project_id != self.context.project_id
                || op.actor_id != self.context.actor_id
                || op.operation_id != *id
            {
                return Err(RunError::Validation("operation index identity"));
            }
            Ok(op)
        })
        .transpose()
    }
    /// Whether the store holds any run record.
    pub fn has_runs(&self, call: &Request<'_>) -> Result<bool> {
        call.check()?;
        Ok(self
            .db
            .query_row("SELECT EXISTS(SELECT 1 FROM runs)", [], |row| row.get(0))?)
    }
    pub fn run(&self, id: RunId, call: &Request<'_>) -> Result<RunRecord> {
        call.check()?;
        let run = self.run_on(&self.db, id)?;
        self.held_owner(&run, None)?;
        Ok(run)
    }
    /// Every attempt this host view handed to a guardian, once each.
    pub fn held_attempts(&self) -> Vec<AttemptId> {
        let mut held: Vec<_> = self
            .launched
            .borrow()
            .iter()
            .map(|held| held.scope.attempt)
            .collect();
        held.sort();
        held.dedup();
        held
    }
    /// The lane can rewrite a child's records after the host stamps them, so
    /// the host's view refuses a launched run or attempt naming another owner.
    fn held_owner(&self, run: &RunRecord, attempt: Option<&AttemptRecord>) -> Result<()> {
        let Some(requester) = self.sealed_requester else {
            return Ok(());
        };
        let launched = self.launched.borrow();
        let run_renamed =
            run.actor_id != requester && launched.iter().any(|held| held.scope.run == run.run_id);
        let attempt_renamed = attempt.is_some_and(|attempt| {
            attempt.verified_owner != Some(requester)
                && launched
                    .iter()
                    .any(|held| held.scope.attempt == attempt.attempt_id)
        });
        if run_renamed || attempt_renamed {
            return Err(RunError::Validation("child record names another owner"));
        }
        Ok(())
    }
    pub(crate) fn run_on(&self, db: &rusqlite::Connection, id: RunId) -> Result<RunRecord> {
        let raw: Vec<u8> = db
            .query_row(
                "SELECT record FROM runs WHERE id=?1 AND project=?2",
                params![id.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(RunError::NotFound)?;
        let run: RunRecord = decode(&raw)?;
        if run.run_id != id || run.project_id != self.context.project_id {
            return Err(RunError::Validation("run index identity"));
        }
        Ok(run)
    }
    /// Every settled attempt of a run, oldest first.
    pub fn terminal_attempts(&self, run: RunId, call: &Request<'_>) -> Result<Vec<AttemptRecord>> {
        let ids: Vec<String> = {
            let mut query = self.db.prepare("SELECT id FROM attempts WHERE run=?1 AND project=?2 AND terminal=1 ORDER BY sequence")?;
            let rows = query.query_map(
                params![run.to_string(), self.context.project_id.to_string()],
                |row| row.get(0),
            )?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        ids.into_iter()
            .map(|id| {
                let id =
                    AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
                self.record(id, call)
            })
            .collect()
    }
    /// Snapshot nonterminal refs before a host accepts a child's launch. The
    /// refs survive partial handoff errors and authorise only that child scope.
    pub fn pending_attempts(&self, run: RunId, call: &Request<'_>) -> Result<Vec<AttemptRecord>> {
        let ids: Vec<String> = {
            let mut query = self.db.prepare("SELECT id FROM attempts WHERE run=?1 AND project=?2 AND terminal=0 ORDER BY sequence")?;
            let rows = query.query_map(
                params![run.to_string(), self.context.project_id.to_string()],
                |row| row.get(0),
            )?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        ids.into_iter()
            .map(|id| {
                let id =
                    AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
                self.record(id, call)
            })
            .collect()
    }
    /// The record read used before a mutation or recovery decision. It makes
    /// the process checkpoint durable, so a failed sync surfaces instead of
    /// feeding the checkpoint observation into a write.
    pub fn record(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptRecord> {
        self.read_record(id, call, RecordRead::Durable)
    }
    /// The record read for view paths. It reports the published checkpoint
    /// without syncing it, so status, lane listings and output paging do not
    /// pay a durable flush per history row.
    pub(crate) fn record_observed(
        &self,
        id: AttemptId,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        self.read_record(id, call, RecordRead::Observed)
    }
    fn read_record(
        &self,
        id: AttemptId,
        call: &Request<'_>,
        read: RecordRead,
    ) -> Result<AttemptRecord> {
        let (run, record) = self.record_with_run(id, call, read)?;
        self.held_owner(&run, Some(&record))?;
        Ok(record)
    }
    /// A terminal row may precede the guardian releasing writer custody.
    pub fn custody_released(&self, record: &AttemptRecord, call: &Request<'_>) -> Result<bool> {
        use provenant_os::fs::FsError;
        use provenant_os::fs::LockMode;
        if record.project_id != self.context.project_id {
            return Err(RunError::unsafe_path(
                format!("runs/{}/attempts/{}", record.run_id, record.attempt_id),
                PathCheck::Containment,
            ));
        }
        if !matches!(record.state, AttemptState::Terminal { .. }) {
            return Ok(false);
        }
        let directory = self.attempt_dir(record.run_id, record.attempt_id, false)?;
        let _custody = match directory.try_lock_existing(
            std::ffi::OsStr::new(AttemptFile::CustodyLock.name()),
            LockMode::Exclusive,
        ) {
            Ok(lock) => Some(lock),
            Err(FsError::LockTimeout { .. }) => return Ok(false),
            Err(error) if error.is_not_found() && record.provider_launched == Some(false) => None,
            Err(error) => return Err(error.into()),
        };
        let fences = self.writer_fence_listing()?;
        if fences
            .reservations
            .iter()
            .any(|fence| fence.scope == Self::scope(record))
        {
            return Ok(false);
        }
        if fences.issues.is_empty() {
            return Ok(true);
        }
        // An unreadable reservation names no scope. It holds this attempt's
        // custody only in this writer's directory; other entries block only
        // a writer that needs them.
        let request = self.request(record.run_id, call)?;
        let Some(writer) = request
            .tasks
            .iter()
            .find(|task| task.task_id == record.task_id)
            .and_then(|task| task.writer.as_ref())
        else {
            return Ok(true);
        };
        let slot = self
            .writer_registry
            .join("worktrees")
            .join(record.project_id.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(
                &writer.worktree,
            ));
        let evidence = slot
            .join(record.attempt_id.to_string())
            .join("reservation.json");
        Ok(!fences.issues.iter().any(|issue| {
            issue.reservation == evidence || issue.reservation == slot.join("reservation.json")
        }))
    }
    fn record_with_run(
        &self,
        id: AttemptId,
        call: &Request<'_>,
        read: RecordRead,
    ) -> Result<(RunRecord, AttemptRecord)> {
        call.check()?;
        let (raw, run_id, task_id, number, revision, terminal): (Vec<u8>, String, String, i64, i64, bool) = self.db.query_row("SELECT record,run,task,number,revision,terminal FROM attempts WHERE id=?1 AND project=?2", params![id.to_string(), self.context.project_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).optional()?.ok_or(RunError::NotFound)?;
        let mut record: AttemptRecord = decode(&raw)?;
        if record.attempt_id != id
            || record.project_id != self.context.project_id
            || record.run_id.to_string() != run_id
            || record.task_id.to_string() != task_id
            || sql_int(record.attempt_number)? != number
            || sql_int(record.revision)? != revision
            || matches!(record.state, AttemptState::Terminal { .. }) != terminal
        {
            return Err(RunError::Validation("attempt index identity"));
        }
        let run = self.run_on(&self.db, record.run_id)?;
        if run.operation_id != record.operation_id || !run.task_ids.contains(&record.task_id) {
            return Err(RunError::Validation("attempt run association"));
        }
        validate_attempt(&record)?;
        let (checkpoint, checkpoint_readable) = match self.attempt_dir(record.run_id, id, false) {
            Ok(directory) => {
                let read_checkpoint = match read {
                    RecordRead::Durable => crate::artifacts::read_sealed(
                        &directory,
                        AttemptFile::Process.name(),
                        RECORD_LIMIT,
                    ),
                    RecordRead::Observed => crate::artifacts::read_sealed_observed(
                        &directory,
                        AttemptFile::Process.name(),
                        RECORD_LIMIT,
                    ),
                };
                match read_checkpoint {
                    Ok(bytes) => (bytes, true),
                    // This file is observational. A damaged or unreadable
                    // checkpoint must not turn an attempt read into silence.
                    Err(_) => (None, false),
                }
            }
            Err(RunError::Filesystem(error)) if error.is_not_found() => (None, true),
            Err(_) => (None, false),
        };
        record.process_observation_valid = checkpoint_readable;
        if let Some(bytes) = checkpoint {
            record.process_observation_valid = false;
            #[derive(serde::Deserialize)]
            struct ProcessCheckpoint {
                schema_version: u64,
                scope: provenant_supervisor::custody::Scope,
                #[serde(default)]
                last_output_at: Option<WallTime>,
                #[serde(default)]
                observed_at: Option<WallTime>,
                #[serde(default)]
                priority: Option<i32>,
                #[serde(default)]
                silent: bool,
                #[serde(default)]
                silent_for_ms: Option<u64>,
            }
            if let Ok(observation) = serde_json::from_slice::<ProcessCheckpoint>(&bytes)
                && observation.schema_version == 1
                && observation.scope
                    == (provenant_supervisor::custody::Scope {
                        run: record.run_id,
                        attempt: record.attempt_id,
                        generation: record.generation,
                    })
            {
                record.process_observation_valid = true;
                record.last_output_at = observation.last_output_at;
                record.priority = observation.priority;
                let fresh = observation
                    .observed_at
                    .zip(call.clock.wall_now().ok())
                    .is_some_and(|(observed, now)| {
                        observed <= now
                            && now.as_micros().saturating_sub(observed.as_micros())
                                <= PROCESS_OBSERVATION_MAX_AGE_MICROS
                    });
                if observation.silent
                    && fresh
                    && matches!(record.state, AttemptState::Running { .. })
                {
                    record.provider_silent_for_ms = observation.silent_for_ms;
                }
            }
        }
        Ok((run, record))
    }
    pub fn read_output(
        &self,
        id: AttemptId,
        part: ArtifactPart,
        range: ByteRange,
        context: &ExecutionContext,
        call: &Request<'_>,
    ) -> Result<OutputChunk> {
        if context.project_id != self.context.project_id
            || context.actor_id != self.context.actor_id
            || context.state_root != self.context.state_root
            || matches!(part, ArtifactPart::Prompt)
                && (!context.read_prompts || !self.context.read_prompts)
        {
            return Err(RunError::Refused);
        }
        if !(1..=OUTPUT_LIMIT).contains(&range.max_bytes) {
            return Err(RunError::TooLarge {
                limit: OUTPUT_LIMIT,
            });
        }
        let record = self.record_observed(id, call)?;
        let missing_output = if matches!(record.state, AttemptState::Terminal { .. }) {
            RunError::NotFound
        } else {
            RunError::Nonterminal
        };
        let (bytes, complete, digest) = match part {
            ArtifactPart::Stderr => {
                let dir = self.attempt_dir(record.run_id, id, false)?;
                let (bytes, complete) = match crate::artifacts::read_sealed_observed(
                    &dir,
                    AttemptFile::Stderr.name(),
                    CAPTURE_LIMIT,
                )? {
                    Some(bytes) => (bytes, true),
                    None => (
                        crate::artifacts::read_sealed_observed(
                            &dir,
                            "stderr.log.tail",
                            CAPTURE_LIMIT,
                        )?
                        .ok_or(missing_output)?,
                        false,
                    ),
                };
                let digest = provenant_records::CanonicalV1::file_digest(&bytes);
                (bytes, complete, digest)
            }
            ArtifactPart::Transcript => {
                let session = record.session.as_deref().ok_or(RunError::NotFound)?;
                let dir = self.attempt_dir(record.run_id, id, false)?;
                let (dir, name, _) = crate::transcript::find(&dir, session, &record.write_grants)
                    .ok_or(RunError::NotFound)?;
                let (bytes, complete) = crate::transcript::tail(&dir, &name).ok_or_else(|| {
                    let check = match dir.metadata_at(&name, false) {
                        Ok(meta) if meta.kind == provenant_os::fs::FileKind::Symlink => {
                            PathCheck::Symlink
                        }
                        Ok(meta) if meta.kind != provenant_os::fs::FileKind::Regular => {
                            PathCheck::Kind
                        }
                        Ok(_) => PathCheck::Identity,
                        Err(_) => PathCheck::Missing,
                    };
                    RunError::unsafe_entry(&dir, &name, check)
                })?;
                let digest = provenant_records::CanonicalV1::file_digest(&bytes);
                (bytes, complete, digest)
            }
            ArtifactPart::Prompt | ArtifactPart::Result => {
                let (artifact, limit) = match part {
                    ArtifactPart::Prompt => (&record.prompt, RECORD_LIMIT),
                    _ => (record.result.as_ref().ok_or(missing_output)?, RESULT_LIMIT),
                };
                let bytes = self.artifact_bytes(record.run_id, artifact, limit)?;
                if std::str::from_utf8(&bytes).is_err() {
                    return Err(RunError::Validation("prompt/result is not UTF-8"));
                }
                (bytes, artifact.complete, artifact.digest)
            }
        };
        // Scan the whole bounded artifact before paging: slicing first can
        // expose a token fragment whose marker is on the previous page.
        let (bytes, digest) = if matches!(part, ArtifactPart::Prompt) {
            (bytes, digest)
        } else {
            let safe = provenant_confinement::redact::known_shapes(&bytes)
                .map_err(|_| RunError::Validation("output redaction failed"))?;
            let digest = provenant_records::CanonicalV1::file_digest(&safe);
            (safe, digest)
        };
        if range.offset > bytes.len() as u64 {
            return Err(RunError::Validation("offset past EOF"));
        }
        let end = range
            .offset
            .saturating_add(range.max_bytes)
            .min(bytes.len() as u64);
        let start =
            usize::try_from(range.offset).map_err(|_| RunError::Validation("offset overflow"))?;
        let end_index =
            usize::try_from(end).map_err(|_| RunError::Validation("offset overflow"))?;
        // Raw bytes preserve original offsets, including a cut through UTF-8.
        // The edge must not claim an independently decodable UTF-8 string.
        let chunk = bytes
            .get(start..end_index)
            .ok_or(RunError::Validation("byte range"))?
            .to_vec();
        let encoding = if std::str::from_utf8(&chunk).is_ok() {
            "utf8"
        } else if matches!(part, ArtifactPart::Stderr) {
            "utf8-lossy"
        } else {
            "utf8-fragment"
        };
        Ok(OutputChunk {
            bytes: chunk,
            len: bytes.len() as u64,
            offset: range.offset,
            next_offset: end,
            eof: end == bytes.len() as u64,
            clipped: end < bytes.len() as u64,
            complete,
            encoding,
            digest,
        })
    }
    /// A connection-local change token for commits made by other connections.
    /// Compare tokens only on the same retained store handle.
    pub fn data_version(&self, call: &Request<'_>) -> Result<u64> {
        call.check()?;
        let version: i64 = self
            .db
            .pragma_query_value(None, "data_version", |row| row.get(0))?;
        u64::try_from(version).map_err(|_| RunError::Validation("database data version"))
    }

    /// Translate an attempt cursor only within this verified project.
    pub fn attempt_sequence(&self, id: AttemptId, call: &Request<'_>) -> Result<u64> {
        self.record(id, call)?;
        let sequence: i64 = {
            self.db.query_row(
                "SELECT sequence FROM attempts WHERE id=?1 AND project=?2",
                params![id.to_string(), self.context.project_id.to_string()],
                |row| row.get(0),
            )?
        };
        u64::try_from(sequence).map_err(|_| RunError::Validation("attempt sequence"))
    }
    pub fn lanes(&self, query: &LaneQuery, call: &Request<'_>) -> Result<LanePage> {
        if !(1..=100).contains(&query.limit) || query.targets.len() > 100 {
            return Err(RunError::Validation("lane page bounds"));
        }
        let project = self.context.project_id.to_string();
        let after = sql_int(query.after)?;
        let fetch = sql_int(query.limit + 1)?;
        let owner = query.owner.map(|actor| actor.to_string());
        let unseen = query.unseen_after.map(sql_int).transpose()?;
        let mut rows: Vec<(i64, String)> = {
            let mut found = Vec::new();
            if query.targets.is_empty() {
                let sql = if query.newest_first {
                    "SELECT sequence,id FROM attempts WHERE project=?1 AND (?2=0 OR sequence<?2) AND (?3 OR terminal=0)".to_owned() + UNSEEN_OWNED + " ORDER BY sequence DESC LIMIT ?4"
                } else {
                    "SELECT sequence,id FROM attempts WHERE project=?1 AND sequence>?2 AND (?3 OR terminal=0)".to_owned() + UNSEEN_OWNED + " ORDER BY sequence LIMIT ?4"
                };
                let mut statement = self.db.prepare(&sql)?;
                let page = statement.query_map(
                    params![project, after, query.all, fetch, owner, unseen],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                for row in page {
                    found.push(row?);
                }
            }
            for target in &query.targets {
                let sql = if query.newest_first {
                    "SELECT sequence,id FROM attempts WHERE project=?1 AND (?2=0 OR sequence<?2) AND run=?3 AND (?4 IS NULL OR task=?4) AND (?5 IS NULL OR id=?5)".to_owned() + UNSEEN_TARGET + " ORDER BY sequence DESC LIMIT ?6"
                } else {
                    "SELECT sequence,id FROM attempts WHERE project=?1 AND sequence>?2 AND run=?3 AND (?4 IS NULL OR task=?4) AND (?5 IS NULL OR id=?5)".to_owned() + UNSEEN_TARGET + " ORDER BY sequence LIMIT ?6"
                };
                let mut statement = self.db.prepare(&sql)?;
                let page = statement.query_map(
                    params![
                        project,
                        after,
                        target.run.to_string(),
                        target.task.map(|t| t.to_string()),
                        target.attempt.map(|a| a.to_string()),
                        fetch,
                        unseen
                    ],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                for row in page {
                    found.push(row?);
                }
            }
            found
        };
        if query.newest_first {
            rows.sort_by(|a, b| b.cmp(a));
        } else {
            rows.sort();
        }
        rows.dedup();
        let more = rows.len() as u64 > query.limit;
        rows.truncate(query.limit as usize);
        let next = more
            .then(|| rows.last().map(|(sequence, _)| *sequence as u64))
            .flatten();
        let lanes = rows
            .into_iter()
            .map(|(_, id)| {
                AttemptId::parse(&id)
                    .map_err(|_| RunError::Validation("attempt index ID"))
                    .and_then(|id| self.record_observed(id, call))
            })
            .collect::<Result<_>>()?;
        Ok(LanePage { lanes, next })
    }
    /// The seen cursor (an event sequence) for one actor's wait selection. A
    /// selection never acknowledged starts at the log's end when `from_now`,
    /// else at the beginning.
    pub fn seen_cursor(&self, selection: &str, from_now: bool, call: &Request<'_>) -> Result<u64> {
        call.check()?;
        let stored = self
            .db
            .query_row(
                "SELECT sequence FROM cursors WHERE project=?1 AND actor=?2 AND selection=?3",
                params![
                    self.context.project_id.to_string(),
                    self.context.actor_id.to_string(),
                    selection
                ],
                |r| r.get(0),
            )
            .optional()?;
        let found: Option<i64> = if stored.is_some() || !from_now {
            stored
        } else {
            self.db.query_row(
                "SELECT MAX(sequence) FROM events WHERE project=?1",
                params![self.context.project_id.to_string()],
                |r| r.get(0),
            )?
        };
        Ok(found.map_or(0, |sequence| sequence as u64))
    }
    /// Latest event sequence of each attempt, for acknowledging displayed rows.
    pub fn last_event_sequence(&self, id: AttemptId, call: &Request<'_>) -> Result<u64> {
        call.check()?;
        let found: Option<i64> = self.db.query_row(
            "SELECT MAX(sequence) FROM events WHERE attempt=?1 AND project=?2",
            params![id.to_string(), self.context.project_id.to_string()],
            |r| r.get(0),
        )?;
        Ok(found.map_or(0, |sequence| sequence as u64))
    }
    /// Acknowledge rows up to `sequence`; the cursor never moves back.
    pub fn ack_seen(&self, selection: &str, sequence: u64, call: &Request<'_>) -> Result<()> {
        let sequence = sql_int(sequence)?;
        self.write(call, |tx| {
            tx.execute(
                "INSERT INTO cursors VALUES (?1,?2,?3,?4) ON CONFLICT(project,actor,selection) DO UPDATE SET sequence=MAX(sequence,excluded.sequence)",
                params![self.context.project_id.to_string(), self.context.actor_id.to_string(), selection, sequence],
            )?;
            Ok(())
        })
    }
    pub fn events(
        &self,
        after: u64,
        limit: usize,
        attempt: Option<AttemptId>,
        call: &Request<'_>,
    ) -> Result<Vec<RunEvent>> {
        call.check()?;
        if !(1..=64).contains(&limit) {
            return Err(RunError::Validation("event page bounds"));
        }
        {
            let mut query = self.db.prepare("SELECT sequence,record FROM events WHERE project=?1 AND sequence>?2 AND (?4 IS NULL OR attempt=?4) ORDER BY sequence LIMIT ?3")?;
            let rows = query.query_map(
                params![
                    self.context.project_id.to_string(),
                    sql_int(after)?,
                    limit as i64,
                    attempt.map(|a| a.to_string())
                ],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )?;
            let mut page = Vec::new();
            let mut size = 0;
            for row in rows {
                let (sequence, raw) = row?;
                size += raw.len() as u64;
                if size > RECORD_LIMIT {
                    return Err(RunError::TooLarge {
                        limit: RECORD_LIMIT,
                    });
                }
                let event: RunEvent = decode(&raw)?;
                if sql_int(event.sequence)? != sequence
                    || event.project_id != self.context.project_id
                {
                    return Err(RunError::Validation("event index identity"));
                }
                page.push(event);
            }
            Ok(page)
        }
    }
}
pub(crate) fn valid_write_grants(grants: &[WriteGrantRecord]) -> bool {
    grants.len() <= provenant_confinement::MAX_WRITE_GRANTS
        && grants.iter().all(|grant| {
            grant
                .path
                .to_path_buf()
                .is_ok_and(|path| path.is_absolute())
        })
}

/// Launch-time bound only; stored records are validated by count.
pub(crate) fn write_grants_fit(grants: &[WriteGrantRecord]) -> bool {
    valid_write_grants(grants)
        && provenant_records::CanonicalV1::bytes(&grants).is_ok_and(|bytes| {
            bytes.as_bytes().len() as u64 <= provenant_records::run::MAX_WRITE_GRANT_BYTES
        })
}

pub(crate) fn validate_attempt(r: &AttemptRecord) -> Result<()> {
    if r.context.as_ref().is_some_and(|context| !context.valid()) {
        return Err(RunError::Validation("work context"));
    }
    if let Some(delivery) = &r.delivery
        && (delivery.schema_version != 1
            || delivery.head != r.head
            || !matches!(
                r.state,
                AttemptState::Terminal {
                    outcome: Outcome::Succeeded,
                    ..
                }
            )
            || matches!(delivery.state, WriterDeliveryState::Delivered { .. })
                && delivery.head.is_none())
    {
        return Err(RunError::Validation("delivery receipt"));
    }
    if !valid_read_only_writable(&r.read_only_writable) {
        return Err(RunError::Validation("invalid read-only writable paths"));
    }
    if !valid_write_grants(&r.write_grants) {
        return Err(RunError::Validation("invalid write grants"));
    }
    if r.schema_version != 1 || r.attempt_number == 0 || r.revision == 0 {
        return Err(RunError::Validation("attempt version/ordinal"));
    }
    if r.prompt.path != artifact_path(r.attempt_id, AttemptFile::Prompt.name())?
        || r.prompt.bytes > RECORD_LIMIT
        || !r.prompt.complete
    {
        return Err(RunError::Validation("prompt artifact"));
    }
    if r.usage.context_basis_points.is_some_and(|p| p > 10000) {
        return Err(RunError::Validation("context basis points"));
    }
    if let Some(result) = &r.result
        && (result.path != artifact_path(r.attempt_id, AttemptFile::Result.name())?
            || result.bytes > RESULT_LIMIT)
    {
        return Err(RunError::Validation("result artifact"));
    }
    if let AttemptState::Terminal {
        outcome,
        exit_code,
        finished_at,
        ..
    } = r.state
    {
        // A proven reboot loses the attempt without manufacturing an exit or
        // result. Existing telemetry remains as it was at the crash.
        let lost = outcome == Outcome::Lost && exit_code.is_none();
        if r.result.is_none() && !lost {
            return Err(RunError::Validation("terminal result missing"));
        }
        match &r.exit_evidence {
            Some(evidence)
                if evidence.path
                    != artifact_path(r.attempt_id, AttemptFile::ProcessExit.name())?
                    || evidence.bytes > RECORD_LIMIT
                    || !evidence.complete =>
            {
                return Err(RunError::Validation("terminal exit artifact"));
            }
            Some(_) => {}
            None if matches!(outcome, Outcome::Rejected | Outcome::Cancelled)
                && r.started_at.is_none() => {}
            None if lost => {}
            None => return Err(RunError::Validation("terminal exit evidence missing")),
        }
        if finished_at < r.queued_at
            || r.started_at
                .is_some_and(|t| t < r.queued_at || t > finished_at)
        {
            return Err(RunError::Validation("attempt timestamps"));
        }
        if outcome == Outcome::Succeeded
            && (r
                .result
                .as_ref()
                .is_none_or(|result| !result.complete || result.bytes == 0)
                || exit_code != Some(0)
                || !verified_route(
                    r.resolved_route.as_ref(),
                    r.applied_route.as_ref(),
                    r.observed_route.as_ref(),
                ))
        {
            return Err(RunError::Validation("invalid stored success"));
        }
    }
    Ok(())
}

/// Success must refer to the same exact provider/model at all three stages.
/// Unknown effort/account observations remain absent, never inferred.
pub(crate) fn verified_route(
    resolved: Option<&RouteRecord>,
    applied: Option<&RouteRecord>,
    observed: Option<&RouteRecord>,
) -> bool {
    match (resolved, applied, observed) {
        (Some(r), Some(a), Some(o)) => {
            r.provider.as_ref().is_some_and(|v| !v.is_empty())
                && (r.model.as_ref().is_some_and(|v| !v.is_empty())
                    || (r.model.is_none()
                        && r.effort.is_none()
                        && a.effort.is_none()
                        && o.effort.is_none()
                        && r.provider
                            .as_deref()
                            .and_then(provenant_records::AdapterSpec::get)
                            .is_some_and(|spec| !spec.launch.model)))
                && r.provider == a.provider
                && a.provider == o.provider
                && r.model == a.model
                && a.model == o.model
                && a.effort
                    .as_ref()
                    .is_none_or(|effort| Some(effort) == o.effort.as_ref())
        }
        _ => false,
    }
}
