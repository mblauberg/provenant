use crate::{
    artifacts::artifact_path,
    db::{decode, sql_int},
    *,
};
use provenant_core::{ActorId, AttemptId, OperationId, RunId, TaskId, WallTime, WorkScope};
use provenant_records::{AttemptFact, OutputRef, ReviewerFact, ReviewerProvenance};
use rusqlite::{OptionalExtension, params};
use std::collections::VecDeque;

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
impl Iterator for Attempts<'_> {
    type Item = Result<AttemptFact>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.page.is_empty() {
            let result = self.call.retry(|| {
                let mut statement = self.store.db.prepare("SELECT sequence,id FROM attempts WHERE project=?1 AND terminal=1 AND sequence>?2 AND sequence<=?3 AND (?4 IS NULL OR run=?4) AND (?5 IS NULL OR task=?5) ORDER BY sequence LIMIT 64")?;
                let rows = statement.query_map(params![self.store.context.project_id.to_string(), self.after, self.through, self.filter.run_id.map(|id| id.to_string()), self.filter.task_id.map(|id| id.to_string())], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(rows.collect::<std::result::Result<VecDeque<_>, _>>()?)
            });
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
/// Reviewer attribution from the stored observed route: the owning actor and
/// the model family the route reports, or `Unknown` without an observed route.
fn reviewer_fact(owner: Option<ActorId>, observed: Option<&RouteRecord>) -> Option<ReviewerFact> {
    let actor_id = owner?;
    let route = observed.filter(|r| r.provider.is_some() || r.model.is_some());
    let family = route.map(|r| {
        let model = r.model.as_deref().unwrap_or_default().to_ascii_lowercase();
        let model = model.rsplit('/').next().unwrap_or_default();
        let known = if model.starts_with("claude") {
            Some("anthropic")
        } else if model.starts_with("gpt") || model.starts_with("o1") || model.starts_with("o3") {
            Some("openai")
        } else if model.starts_with("gemini") {
            Some("google")
        } else {
            None
        };
        known
            .map(str::to_owned)
            .or_else(|| r.provider.as_ref().map(|p| p.to_ascii_lowercase()))
            .unwrap_or_default()
    });
    Some(ReviewerFact {
        provenance: if family.is_some() {
            ReviewerProvenance::Observed
        } else {
            ReviewerProvenance::Unknown
        },
        actor_id,
        family: family.filter(|f| !f.is_empty()),
        model: route.and_then(|r| r.model.clone()),
        outcome: None,
    })
}
impl AttemptReader for RunStore {
    fn attempt(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptFact> {
        let r = self.record(id, call)?;
        let AttemptState::Terminal {
            outcome,
            exit_code,
            finished_at,
            reason,
        } = r.state
        else {
            return Err(RunError::Nonterminal);
        };
        let output = r
            .result
            .ok_or(RunError::Validation("terminal result missing"))?;
        Ok(AttemptFact {
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
            output_paths: vec![output.path.clone()],
            reviewer: reviewer_fact(r.verified_owner, r.observed_route.as_ref()),
            delivery_id: r.delivery_id,
            subject: r.subject,
            head: r.head,
            output_refs: vec![OutputRef {
                path: output.path,
                digest: output.digest,
            }],
            requested_route: Some(r.requested_route.projection()),
            applied_route: r.applied_route.map(Into::into),
            observed_route: r.observed_route.map(Into::into),
            verified_owner: r.verified_owner,
            lineage: r.retry_of.into_iter().collect(),
            reason,
        })
    }
    fn attempts<'a>(
        &'a self,
        filter: AttemptFilter,
        call: &'a Request<'a>,
    ) -> Result<Attempts<'a>> {
        let through = call.retry(|| {
            Ok(self.db.query_row(
                "SELECT COALESCE(MAX(sequence),0) FROM attempts WHERE project=?1",
                [self.context.project_id.to_string()],
                |r| r.get(0),
            )?)
        })?;
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
}
#[derive(Debug)]
pub struct OutputChunk {
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub next_offset: u64,
    pub eof: bool,
    pub clipped: bool,
    pub complete: bool,
    pub encoding: &'static str,
    pub digest: provenant_records::FileDigest,
}
#[derive(Debug)]
pub struct RunWorkSnapshot {
    pub generation: u64,
    pub observed_at: WallTime,
    pub active_count: u64,
    pub active_attempts: Vec<AttemptId>,
}
impl RunStore {
    pub fn operation(
        &self,
        id: &OperationId,
        call: &Request<'_>,
    ) -> Result<Option<OperationRecord>> {
        call.retry(|| {
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
        })
    }
    pub fn run(&self, id: RunId, call: &Request<'_>) -> Result<RunRecord> {
        call.retry(|| self.run_on(&self.db, id))
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
    pub fn record(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptRecord> {
        call.retry(|| {
            let (raw, run_id, task_id, number, revision, terminal): (Vec<u8>, String, String, i64, i64, bool) = self.db.query_row("SELECT record,run,task,number,revision,terminal FROM attempts WHERE id=?1 AND project=?2", params![id.to_string(), self.context.project_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).optional()?.ok_or(RunError::NotFound)?;
            let record: AttemptRecord = decode(&raw)?;
            if record.attempt_id != id || record.project_id != self.context.project_id || record.run_id.to_string() != run_id || record.task_id.to_string() != task_id || sql_int(record.attempt_number)? != number || sql_int(record.revision)? != revision || matches!(record.state, AttemptState::Terminal { .. }) != terminal { return Err(RunError::Validation("attempt index identity")); }
            let run = self.run_on(&self.db, record.run_id)?;
            if run.operation_id != record.operation_id || !run.task_ids.contains(&record.task_id) { return Err(RunError::Validation("attempt run association")); }
            validate_attempt(&record)?;
            Ok(record)
        })
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
        let record = self.record(id, call)?;
        let (artifact, limit) = match part {
            ArtifactPart::Prompt => (&record.prompt, RECORD_LIMIT),
            ArtifactPart::Result => (
                record.result.as_ref().ok_or(RunError::Nonterminal)?,
                RESULT_LIMIT,
            ),
        };
        let bytes = self.artifact_bytes(record.run_id, artifact, limit)?;
        if std::str::from_utf8(&bytes).is_err() {
            return Err(RunError::Validation("prompt/result is not UTF-8"));
        }
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
        } else {
            "utf8-fragment"
        };
        Ok(OutputChunk {
            bytes: chunk,
            offset: range.offset,
            next_offset: end,
            eof: end == bytes.len() as u64,
            clipped: end < bytes.len() as u64,
            complete: artifact.complete,
            encoding,
            digest: artifact.digest,
        })
    }
    pub fn events(&self, after: u64, limit: usize, call: &Request<'_>) -> Result<Vec<RunEvent>> {
        if !(1..=64).contains(&limit) {
            return Err(RunError::Validation("event page bounds"));
        }
        call.retry(|| {
            let mut query = self.db.prepare("SELECT sequence,record FROM events WHERE project=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?;
            let rows = query.query_map(params![self.context.project_id.to_string(), sql_int(after)?, limit as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
            let mut page = Vec::new();
            let mut size = 0;
            for row in rows {
                let (sequence, raw) = row?;
                size += raw.len() as u64;
                if size > RECORD_LIMIT { return Err(RunError::TooLarge { limit: RECORD_LIMIT }); }
                let event: RunEvent = decode(&raw)?;
                if sql_int(event.sequence)? != sequence || event.project_id != self.context.project_id { return Err(RunError::Validation("event index identity")); }
                page.push(event);
            }
            Ok(page)
        })
    }
    pub fn work_snapshot(&self, scope: WorkScope, call: &Request<'_>) -> Result<RunWorkSnapshot> {
        let run = match scope {
            WorkScope::Project(project) if project == self.context.project_id => None,
            WorkScope::Run { project, run } if project == self.context.project_id => Some(run),
            _ => return Err(RunError::Refused),
        };
        call.retry(|| {
            let mut query = self.db.prepare("SELECT id FROM attempts WHERE project=?1 AND terminal=0 AND (?2 IS NULL OR run=?2) ORDER BY sequence LIMIT 257")?;
            let rows = query.query_map(params![self.context.project_id.to_string(), run.map(|id| id.to_string())], |r| r.get::<_, String>(0))?;
            let mut active = Vec::new();
            for row in rows { active.push(AttemptId::parse(&row?).map_err(|_| RunError::Validation("attempt index ID"))?); }
            if active.len() > 256 { return Err(RunError::Validation("snapshot exceeds active bound")); }
            let generation = if let Some(run) = run { self.run_on(&self.db, run)?.generation } else { 0 };
            Ok(RunWorkSnapshot { generation, observed_at: call.clock.wall_now().map_err(|_| RunError::Validation("wall clock"))?, active_count: active.len() as u64, active_attempts: active })
        })
    }
}
pub(crate) fn validate_attempt(r: &AttemptRecord) -> Result<()> {
    if r.schema_version != 1 || r.attempt_number == 0 || r.revision == 0 {
        return Err(RunError::Validation("attempt version/ordinal"));
    }
    if r.prompt.path != artifact_path(r.attempt_id, "prompt.txt")?
        || r.prompt.bytes > RECORD_LIMIT
        || !r.prompt.complete
    {
        return Err(RunError::Validation("prompt artifact"));
    }
    if r.usage.context_basis_points.is_some_and(|p| p > 10000) {
        return Err(RunError::Validation("context basis points"));
    }
    if let Some(result) = &r.result
        && (result.path != artifact_path(r.attempt_id, "result.md")? || result.bytes > RESULT_LIMIT)
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
        let result = r
            .result
            .as_ref()
            .ok_or(RunError::Validation("terminal result missing"))?;
        let evidence = r
            .exit_evidence
            .as_ref()
            .ok_or(RunError::Validation("terminal exit evidence missing"))?;
        if evidence.path != artifact_path(r.attempt_id, "process-exit.json")?
            || evidence.bytes > RECORD_LIMIT
            || !evidence.complete
        {
            return Err(RunError::Validation("terminal exit artifact"));
        }
        if finished_at < r.queued_at
            || r.started_at
                .is_some_and(|t| t < r.queued_at || t > finished_at)
        {
            return Err(RunError::Validation("attempt timestamps"));
        }
        if outcome == Outcome::Succeeded
            && (!result.complete
                || result.bytes == 0
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
                && r.model.as_ref().is_some_and(|v| !v.is_empty())
                && r.provider == a.provider
                && a.provider == o.provider
                && r.model == a.model
                && a.model == o.model
        }
        _ => false,
    }
}
