use crate::{
    artifacts::artifact_path,
    db::{decode, sql_int},
    *,
};
use provenant_confinement::valid_read_only_writable;
use provenant_core::{ActorId, AttemptId, OperationId, ProjectId, WallTime};
use provenant_records::{AttemptFact, AttemptFile, OutputRef, ReviewerFact, ReviewerProvenance};
use rusqlite::{OptionalExtension, params};
use std::collections::VecDeque;

type AttemptIndexKey = (String, String, u32);
type AttemptIndexRow = (Vec<u8>, i64, i64, bool, AttemptIndexKey);

#[cfg(test)]
thread_local! {
    /// Runs once on the next `record_row`, after the attempt row is read
    /// and before the rest of the record: a commit landing mid-read.
    pub(crate) static AFTER_ATTEMPT_ROW: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

pub trait AttemptReader {
    fn attempt(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptFact>;
    /// Every settled attempt of the project, oldest first.
    fn attempts<'a>(&'a self, call: &'a Request<'a>) -> Result<Attempts<'a>>;
}
pub struct Attempts<'a> {
    store: &'a RunStore,
    call: &'a Request<'a>,
    after: i64,
    through: i64,
    page: VecDeque<(i64, String)>,
    done: bool,
}
impl Attempts<'_> {
    fn page(&self) -> Result<VecDeque<(i64, String)>> {
        let mut statement = self.store.db.prepare("SELECT sequence,id FROM attempts WHERE project=?1 AND terminal=1 AND refused=0 AND sequence>?2 AND sequence<=?3 ORDER BY sequence LIMIT 64")?;
        let rows = statement.query_map(
            params![
                self.store.context.project_id.to_string(),
                self.after,
                self.through
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
/// The graph labels of an attempt's work context: requested tier, template and
/// parent node. Only a graph node's attempt sets them.
fn graph_labels(
    context: Option<&provenant_records::run::WorkContext>,
) -> (Option<String>, Option<String>, Option<String>) {
    let context = context.cloned().unwrap_or_default();
    (context.tier, context.template, context.parent_node)
}

/// Milliseconds from queueing to the start of the provider process: the launch
/// slot wait. `None` until the attempt starts, or if the clocks disagree.
fn slot_wait_ms(queued_at: WallTime, started_at: Option<WallTime>) -> Option<u64> {
    let waited = started_at?.as_micros().checked_sub(queued_at.as_micros())?;
    u64::try_from(waited / 1_000).ok()
}

/// The published fact for a terminal record. A record read from a peer takes the
/// same path as one read here.
pub fn fact(r: AttemptRecord) -> Result<AttemptFact> {
    let stop = r.state.stop();
    let account_identity = usage_identity(
        r.resolved_route.as_ref(),
        r.applied_route.as_ref(),
        r.observed_route.as_ref(),
    );
    let usage = r.effective_usage().into();
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
        None if matches!(outcome, Outcome::Lost | Outcome::Unsettled) && exit_code.is_none() => {
            Vec::new()
        }
        None => return Err(RunError::Validation("terminal result missing")),
    };
    let transcript = r
        .applied_route
        .as_ref()
        .and_then(|route| route.provider.clone())
        .zip(r.session.clone())
        .map(|(adapter, session)| provenant_records::TranscriptRef { adapter, session });
    let (tier, template, parent_node) = graph_labels(r.context.as_ref());
    let slot_wait_ms = slot_wait_ms(r.queued_at, r.started_at);
    let cause_source = match stop {
        Some(stop) => stop.cause().source(),
        None => r
            .settlement_reason
            .map(|reason| reason.rule().source)
            .unwrap_or(provenant_records::run::CauseSource::Unavailable),
    };
    Ok(AttemptFact {
        routing_context: r
            .resolved_route
            .as_ref()
            .and_then(|route| route.selection_context.clone()),
        context: r.context,
        attempt_id: r.attempt_id,
        key: r.key,
        attempt_number: r.attempt_number,
        project_id: r.project_id,
        terminal_state: outcome.into(),
        cause_source,
        settlement_reason: r.settlement_reason,
        account_identity,
        exit_code,
        queued_at: r.queued_at,
        started_at: r.started_at,
        provider_ms: r.provider_ms,
        finished_at,
        resolved_route: r.resolved_route.map(Into::into),
        usage,
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
        tier,
        template,
        parent_node,
        slot_wait_ms,
        reason,
        trace: r.trace,
        transcript,
    })
}
/// The observed command ledger for the result projection. The run reader has
/// no adapter command capture, so commands stay explicitly unknown rather
/// than an empty or inferred list.
/// Spec: docs/specs/lane-contracts.md#result-projection
pub fn observed_commands() -> provenant_records::lane_contract::Observed<
    Vec<
        provenant_records::lane_contract::Observed<
            provenant_records::lane_contract::CommandReceipt,
        >,
    >,
> {
    use provenant_records::lane_contract::{Coverage, Observed};
    Observed {
        source: "unavailable".into(),
        completeness: Coverage::Unknown,
        value: None,
        extra: Default::default(),
    }
}

impl RunStore {
    /// Every project with a terminal attempt in this store, whichever project
    /// opened it. The usage backfill reads each one through its own context.
    pub fn terminal_projects(&self, call: &Request<'_>) -> Result<Vec<ProjectId>> {
        call.check()?;
        let mut statement = self.db.prepare(
            "SELECT DISTINCT project FROM attempts WHERE terminal=1 AND refused=0 ORDER BY project",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| {
            ProjectId::parse(&row?).map_err(|_| RunError::Validation("attempt project ID"))
        })
        .collect()
    }
}
impl AttemptReader for RunStore {
    fn attempt(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptFact> {
        fact(self.record(id, call)?)
    }
    fn attempts<'a>(&'a self, call: &'a Request<'a>) -> Result<Attempts<'a>> {
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
/// Optional lane filters: the submitting actor, and terminal lanes already seen.
const UNSEEN_OWNED: &str = " AND (?5 IS NULL OR actor=?5) AND (?6 IS NULL OR terminal=0 OR EXISTS(SELECT 1 FROM events WHERE attempt=attempts.id AND sequence>?6))";

fn inventory_page_sql(all: bool, newest_first: bool, has_cursor: bool) -> String {
    let direction = if newest_first { "DESC" } else { "ASC" };
    let bound = if newest_first && has_cursor {
        "sequence<?2"
    } else if newest_first {
        "?2=0"
    } else {
        "sequence>?2"
    };
    let select = |terminal: u8| {
        format!(
            "SELECT sequence,id FROM attempts INDEXED BY terminal_attempts WHERE project=?1 AND refused=0 AND {bound} AND terminal={terminal}{UNSEEN_OWNED}"
        )
    };
    if all {
        format!(
            "{} UNION ALL {} ORDER BY sequence {direction} LIMIT ?4",
            select(0),
            select(1)
        )
    } else {
        format!("{} ORDER BY sequence {direction} LIMIT ?4", select(0))
    }
}
const UNSEEN_TARGET: &str = " AND (?5 IS NULL OR terminal=0 OR EXISTS(SELECT 1 FROM events WHERE attempt=attempts.id AND sequence>?5))";
#[derive(Clone, Debug)]
pub struct LaneQuery {
    /// Exact selections include terminal lanes. Without them the page holds
    /// active lanes, or every retained lane when `all` is set.
    pub targets: Vec<AttemptId>,
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

/// The two outcomes of a recovery read of an attempt record.
pub(crate) enum AttemptRecordRead {
    // Boxed to keep the enum small: AttemptRecord is large while the unreadable
    // outcome is small (clippy::large_enum_variant).
    Decoded(Box<AttemptRecord>),
    Unreadable(UnreadableAttempt),
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
            .map(|artifact| self.artifact_bytes(artifact, limit))
            .transpose()
    }

    /// The operation record for a caller's operation ID. An admitted
    /// operation reads as terminal once its attempt is.
    pub fn operation(
        &self,
        id: &OperationId,
        call: &Request<'_>,
    ) -> Result<Option<OperationRecord>> {
        call.check()?;
        self.operation_on(&self.db, id)
    }
    pub(crate) fn operation_on(
        &self,
        db: &rusqlite::Connection,
        id: &OperationId,
    ) -> Result<Option<OperationRecord>> {
        let row: Option<(Vec<u8>, bool)> = db
            .query_row(
                "SELECT operation_record, NOT EXISTS(SELECT 1 FROM attempts p WHERE p.project=a.project \
                 AND p.actor=a.actor AND p.operation=a.operation AND p.terminal=0) \
                 FROM attempts a WHERE project=?1 AND actor=?2 AND operation=?3 ORDER BY sequence LIMIT 1",
                params![
                    self.context.project_id.to_string(),
                    self.context.actor_id.to_string(),
                    id.as_str()
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(raw, settled)| {
            let mut op: OperationRecord = decode(&raw)?;
            if op.project_id != self.context.project_id
                || op.actor_id != self.context.actor_id
                || op.operation_id != *id
            {
                return Err(RunError::Validation("operation index identity"));
            }
            if settled && op.state != OperationState::Refused {
                op.state = OperationState::Terminal;
            }
            Ok(op)
        })
        .transpose()
    }

    /// Read the submitting actor's recent refused operations, newest first.
    /// A refusal is a terminal attempt row with no attempt record, so it never
    /// appears in lane queries.
    pub fn recent_refusals(&self, limit: u64, call: &Request<'_>) -> Result<Vec<OperationRecord>> {
        call.check()?;
        if !(1..=100).contains(&limit) {
            return Err(RunError::Validation("refusal page limit"));
        }
        let mut statement = self.db.prepare(
            "SELECT operation,operation_record FROM attempts WHERE project=?1 AND actor=?2 AND refused=1 ORDER BY sequence DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                self.context.project_id.to_string(),
                self.context.actor_id.to_string(),
                sql_int(limit)?
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;
        let mut refusals = Vec::new();
        for row in rows {
            call.check()?;
            let (id, raw) = row?;
            let record: OperationRecord = decode(&raw)?;
            if record.project_id != self.context.project_id
                || record.actor_id != self.context.actor_id
                || record.operation_id.as_str() != id
                || record.state != OperationState::Refused
                || record.refusal.is_none()
                || record.refused_at.is_none()
            {
                return Err(RunError::Validation("refusal index identity"));
            }
            refusals.push(record);
        }
        Ok(refusals)
    }

    /// A bounded page of this actor's refusal notices still owed to a chair.
    pub fn pending_refusal_notices(
        &self,
        limit: u64,
        call: &Request<'_>,
    ) -> Result<Vec<OperationRecord>> {
        Ok(self
            .pending_refusal_notices_after(limit, 0, call)?
            .into_iter()
            .map(|(_, record)| record)
            .collect())
    }

    /// Page by the immutable attempt sequence, so failed notices cannot starve later rows.
    pub fn pending_refusal_notices_after(
        &self,
        limit: u64,
        after: i64,
        call: &Request<'_>,
    ) -> Result<Vec<(i64, OperationRecord)>> {
        call.check()?;
        if !(1..=100).contains(&limit) {
            return Err(RunError::Validation("refusal page limit"));
        }
        let mut statement = self.db.prepare(
            "SELECT sequence,operation,operation_record FROM attempts WHERE project=?1 AND actor=?2 AND refused=1 AND CASE WHEN json_valid(CAST(operation_record AS TEXT)) THEN json_type(CAST(operation_record AS TEXT),'$.refusal_chair')='object' AND COALESCE(json_extract(CAST(operation_record AS TEXT),'$.refusal_notice_done'),0) != 1 ELSE 0 END AND sequence > ?4 ORDER BY sequence LIMIT ?3",
        )?;
        let mut records = Vec::new();
        let mut cursor = after;
        loop {
            call.check()?;
            let rows = statement.query_map(
                params![
                    self.context.project_id.to_string(),
                    self.context.actor_id.to_string(),
                    sql_int(limit)?,
                    cursor
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )?;
            let mut scanned = 0;
            for row in rows {
                call.check()?;
                let (rowid, operation, raw) = row?;
                cursor = rowid;
                scanned += 1;
                // Corrupt evidence cannot produce a notice. Advance by its raw
                // sequence so a full page of it cannot hide later valid notices.
                let Ok(id) = OperationId::parse(&operation) else {
                    continue;
                };
                let Ok(record) = decode::<OperationRecord>(&raw) else {
                    continue;
                };
                if record.project_id != self.context.project_id
                    || record.actor_id != self.context.actor_id
                    || record.operation_id != id
                    || record.state != OperationState::Refused
                    || record.refusal.is_none()
                    || record.refused_at.is_none()
                    || record.refusal_chair.is_none()
                {
                    continue;
                }
                records.push((rowid, record));
                if records.len() as u64 == limit {
                    return Ok(records);
                }
            }
            if scanned < limit {
                return Ok(records);
            }
        }
    }
    /// Whether the store holds any admitted attempt.
    pub fn has_runs(&self, call: &Request<'_>) -> Result<bool> {
        call.check()?;
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE refused=0)",
            [],
            |row| row.get(0),
        )?)
    }
    /// The attempt an operation admitted. A refused operation admitted none.
    pub fn operation_attempt(&self, id: &OperationId, call: &Request<'_>) -> Result<AttemptId> {
        call.check()?;
        let found: String = self
            .db
            .query_row(
                "SELECT id FROM attempts WHERE project=?1 AND actor=?2 AND operation=?3 AND refused=0 \
                 ORDER BY sequence LIMIT 1",
                params![
                    self.context.project_id.to_string(),
                    self.context.actor_id.to_string(),
                    id.as_str()
                ],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(RunError::NotFound)?;
        AttemptId::parse(&found).map_err(|_| RunError::Validation("attempt index ID"))
    }
    /// The operation that admitted an attempt: its submitting actor, request
    /// digest and nested parent.
    pub fn attempt_operation(&self, id: AttemptId, call: &Request<'_>) -> Result<OperationRecord> {
        call.check()?;
        Self::attempt_operation_in(&self.db, self.context.project_id, id)
    }
    pub(crate) fn attempt_operation_in(
        db: &rusqlite::Connection,
        project: ProjectId,
        id: AttemptId,
    ) -> Result<OperationRecord> {
        let raw: Vec<u8> = db
            .query_row(
                "SELECT operation_record FROM attempts WHERE id=?1 AND project=?2 AND refused=0",
                params![id.to_string(), project.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(RunError::NotFound)?;
        let operation: OperationRecord = decode(&raw)?;
        if operation.project_id != project {
            return Err(RunError::Validation("operation index identity"));
        }
        Ok(operation)
    }
    /// The record read used before a mutation or recovery decision. It makes
    /// the process checkpoint durable, so a failed sync surfaces instead of
    /// feeding the checkpoint observation into a write.
    pub fn record(&self, id: AttemptId, call: &Request<'_>) -> Result<AttemptRecord> {
        self.read_record(id, call, RecordRead::Durable)
    }
    /// The admitted attempt recorded under a work key. A refused admission
    /// under the key reads as its recorded refusal.
    pub fn attempt_by_key(
        &self,
        key: &provenant_records::WorkKey,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        call.check()?;
        if key.project() != self.context.project_id {
            return Err(RunError::NotFound);
        }
        let (id, refused, operation): (String, bool, Vec<u8>) = self
            .db
            .query_row(
                "SELECT id,refused,operation_record FROM attempts \
                 WHERE project=?1 AND graph=?2 AND node=?3 AND attempt=?4",
                params![
                    key.project().to_string(),
                    key.graph(),
                    key.node(),
                    key.attempt()
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or(RunError::NotFound)?;
        if refused {
            let op: OperationRecord = decode(&operation)?;
            return Err(RunError::RecordedRefusal(Box::new(
                op.refusal
                    .ok_or(RunError::Validation("refusal diagnostic missing"))?,
            )));
        }
        let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
        self.read_record(id, call, RecordRead::Observed)
    }
    /// The work key of an attempt row, admitted or refused, by its internal ID.
    pub fn work_key(
        &self,
        id: AttemptId,
        call: &Request<'_>,
    ) -> Result<provenant_records::WorkKey> {
        call.check()?;
        let (graph, node, attempt): (String, String, u32) = self
            .db
            .query_row(
                "SELECT graph,node,attempt FROM attempts WHERE id=?1 AND project=?2",
                params![id.to_string(), self.context.project_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or(RunError::NotFound)?;
        provenant_records::WorkKey::new(self.context.project_id, graph, node, attempt)
            .map_err(|_| RunError::Validation("attempt work key"))
    }
    /// Resolve a worktree owner by its globally unique attempt ID. The owner
    /// can belong to another project scope; only its state is used for lineage.
    pub(crate) fn writer_owner_terminal(&self, id: AttemptId, call: &Request<'_>) -> Result<bool> {
        call.check()?;
        let project: String = self
            .db
            .query_row(
                "SELECT project FROM attempts WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(RunError::NotFound)?;
        let mut context = self.context.clone();
        context.project_id = provenant_core::ProjectId::parse(&project)
            .map_err(|_| RunError::Validation("attempt index project"))?;
        let owner_store = Self::open(&context, call)?;
        Ok(matches!(
            owner_store.record(id, call)?.state,
            AttemptState::Terminal { .. }
        ))
    }
    /// Whether attempt `id`, from any project, carries a user-enabled
    /// sandbox floor ([`AttemptRecord::floors_children`]).
    /// Missing evidence does not enable the optional sandbox policy.
    pub fn attempt_floors_children(&self, id: AttemptId, call: &Request<'_>) -> bool {
        let read = || -> Result<bool> {
            call.check()?;
            let project: String = self
                .db
                .query_row(
                    "SELECT project FROM attempts WHERE id=?1",
                    [id.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(RunError::NotFound)?;
            let mut context = self.context.clone();
            context.project_id = provenant_core::ProjectId::parse(&project)
                .map_err(|_| RunError::Validation("attempt index project"))?;
            Ok(Self::open(&context, call)?
                .record(id, call)?
                .floors_children())
        };
        read().unwrap_or(false)
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
    /// Read an attempt record for recovery. A record whose bytes no longer
    /// decode is classified as unreadable instead of being mistaken for an
    /// undecidable custody error: its bytes are kept with the source so the
    /// caller can preserve them and judge the owner's liveness separately.
    pub(crate) fn recovery_record(
        &self,
        id: AttemptId,
        call: &Request<'_>,
    ) -> Result<AttemptRecordRead> {
        let raw: Vec<u8> = self
            .db
            .query_row(
                "SELECT record FROM attempts WHERE id=?1 AND project=?2 AND refused=0",
                params![id.to_string(), self.context.project_id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(RunError::NotFound)?;
        match crate::db::decode_attempt(&raw) {
            Ok(_) => self
                .record(id, call)
                .map(|record| AttemptRecordRead::Decoded(Box::new(record))),
            Err(error) => {
                let (schema_version, message) = match error {
                    RunError::Decode(error) => {
                        let schema_version = match error.root_cause() {
                            provenant_records::DecodeError::Typed { schema_version, .. } => {
                                *schema_version
                            }
                            provenant_records::DecodeError::UnsupportedVersion {
                                found, ..
                            } => Some(*found),
                            _ => None,
                        };
                        (schema_version, error.to_string())
                    }
                    RunError::Version(version) => (
                        Some(version as u64),
                        format!("unsupported schema version {version}"),
                    ),
                    other => return Err(other),
                };
                Ok(AttemptRecordRead::Unreadable(UnreadableAttempt {
                    source: UnreadableSource {
                        attempt: id,
                        path: self
                            .context
                            .state_root
                            .as_path()
                            .join("attempts")
                            .join(id.to_string()),
                        schema_version,
                        error: message,
                    },
                    raw,
                }))
            }
        }
    }
    fn read_record(
        &self,
        id: AttemptId,
        call: &Request<'_>,
        read: RecordRead,
    ) -> Result<AttemptRecord> {
        self.record_row(id, call, read)
    }
    pub(crate) fn same_session_successor(&self, id: AttemptId) -> Result<Option<AttemptId>> {
        self
            .db
            .query_row(
                "SELECT id FROM attempts WHERE continues=?1 AND (operation=?2 OR operation=?3) AND refused=0",
                params![
                    id.to_string(),
                    format!("budget-wrap-up-{id}"),
                    format!("graph-steer-{id}")
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|id| {
                AttemptId::parse(&id).map_err(|_| RunError::Validation("wrap-up successor ID"))
            })
            .transpose()
    }

    /// A terminal review with no writer holds no Git work. A reservation that
    /// outlives its guardian cannot protect commits, staging or an import, so
    /// it is not custody uncertainty and must not strand stop or reconcile
    /// (#3844). A writer, an unreadable request and a retained import fence all
    /// fail closed and keep the fence.
    pub(crate) fn terminal_read_only_without_git_work(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        scope: provenant_records::custody::Scope,
    ) -> bool {
        matches!(record.state, AttemptState::Terminal { .. })
            && self
                .request(record.attempt_id, call)
                .is_ok_and(|request| request.task.writer.is_none())
            && !self.retained_import_fence(scope).unwrap_or(true)
    }

    /// A terminal row may precede the guardian releasing writer custody.
    pub fn custody_released(&self, record: &AttemptRecord, call: &Request<'_>) -> Result<bool> {
        use provenant_os::fs::FsError;
        use provenant_os::fs::LockMode;
        if record.project_id != self.context.project_id {
            return Err(RunError::unsafe_path(
                format!("attempts/{}", record.attempt_id),
                PathCheck::Containment,
            ));
        }
        if !matches!(record.state, AttemptState::Terminal { .. }) {
            return Ok(false);
        }
        let directory = self.attempt_dir(record.attempt_id, false)?;
        let _custody = match directory.try_lock_existing(
            std::ffi::OsStr::new(AttemptFile::CustodyLock.name()),
            LockMode::Exclusive,
        ) {
            Ok(lock) => Some(lock),
            Err(FsError::LockTimeout { .. }) => return Ok(false),
            Err(error) if error.is_not_found() && record.provider_launched == Some(false) => None,
            Err(error) => return Err(error.into()),
        };
        // Custody has one location: the reader's attempt or the writer's
        // persisted worktree slot. Inventorying every slot here made each
        // terminal row pay for the host's entire retained writer history.
        let writer = self.attempt_writer_slot(record.attempt_id, call)?;
        // An inaccessible own slot or reservation cannot prove release, so it
        // remains a fence rather than failing the caller. The supervisor owns
        // the layout; predecessor and sibling slots are not evidence about
        // this attempt's custody.
        let slot = if let Some(writer) = writer {
            match provenant_os::fs::Directory::open_anchor(&writer) {
                Ok(slot) => slot,
                Err(error) if error.is_not_found() => return Ok(true),
                Err(_) => return Ok(false),
            }
        } else {
            directory
        };
        let scope = Self::scope(record);
        match provenant_supervisor::custody::reservation_present(&slot, scope) {
            Ok(false) => Ok(true),
            Ok(true) => {
                // A read-only review holds no Git work, so a reservation left
                // by its departed guardian fences nothing once the row is
                // terminal. Warn and treat custody as released instead of
                // re-recording the same uncertainty on every pass (#3844).
                if self.terminal_read_only_without_git_work(record, call, scope) {
                    use provenant_records::warning::{
                        READ_ONLY_RESERVATION_RETAINED, Warning, warn,
                    };
                    warn(
                        Warning::new(
                            READ_ONLY_RESERVATION_RETAINED,
                            format!(
                                "terminal read-only review retained a reservation at {}; no Git work is fenced, custody treated as released",
                                slot.current_path()?.display()
                            ),
                        )
                        .subject(format!("attempt {}", record.attempt_id)),
                    );
                    return Ok(true);
                }
                // WorkKey restart resolves this retained attempt first. It uses
                // the same scoped decision as worktree admission and observation.
                let decision = provenant_supervisor::custody::attempt_evidence(&slot, scope)
                    .ok()
                    .and_then(|evidence| {
                        provenant_supervisor::custody::retained_import_decision(&evidence, scope)
                            .ok()
                            .flatten()
                    });
                Ok(matches!(
                    decision,
                    Some(provenant_supervisor::custody::CustodyDecision::Settled(_))
                ))
            }
            Err(_) => Ok(false),
        }
    }
    fn record_row(
        &self,
        id: AttemptId,
        call: &Request<'_>,
        read: RecordRead,
    ) -> Result<AttemptRecord> {
        call.check()?;
        // One read snapshot for every row below. Terminal publication writes
        // the attempt and its settlement receipt in one transaction, so reading
        // them in separate statements can pair a pre-terminal attempt with its
        // terminal receipt and misreport that as a revision conflict.
        // A caller already inside a transaction has its own snapshot.
        let snapshot = if self.db.is_autocommit() {
            Some(rusqlite::Transaction::new_unchecked(
                &self.db,
                rusqlite::TransactionBehavior::Deferred,
            )?)
        } else {
            None
        };
        let (raw, number, revision, terminal, key): AttemptIndexRow = self.db.query_row("SELECT record,number,revision,terminal,graph,node,attempt FROM attempts WHERE id=?1 AND project=?2 AND refused=0", params![id.to_string(), self.context.project_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, (r.get(4)?, r.get(5)?, r.get(6)?)))).optional()?.ok_or(RunError::NotFound)?;
        #[cfg(test)]
        if let Some(hook) = AFTER_ATTEMPT_ROW.with(|hook| hook.borrow_mut().take()) {
            hook();
        }
        let mut record: AttemptRecord = crate::db::decode_attempt(&raw)?;
        let index_key =
            provenant_records::WorkKey::new(self.context.project_id, key.0, key.1, key.2)
                .map_err(|_| RunError::Validation("attempt work key"))?;
        if record.attempt_id != id
            || record.project_id != self.context.project_id
            || sql_int(record.attempt_number)? != number
            || sql_int(record.revision)? != revision
            || matches!(record.state, AttemptState::Terminal { .. }) != terminal
            || record.key != index_key
        {
            return Err(RunError::Validation("attempt index identity"));
        }
        validate_attempt(&record)?;
        // Budget wrap-up and graph steering are host-owned same-session
        // successors. Graph observations wait for the successor to settle.
        record.superseded_by = self.same_session_successor(id)?;
        // Steering text stays in its private ledger; status carries only
        // stable operation/message IDs and the graph-owned successor.
        record.steering = self.steering_status(id, call)?;
        let receipt: Option<(i64, Vec<u8>)> = self
            .db
            .query_row(
                "SELECT generation,record FROM settlement_receipts WHERE attempt=?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        drop(snapshot);
        if let Some((generation, raw)) = receipt {
            if generation != sql_int(record.generation)?
                || !matches!(record.state, AttemptState::Terminal { .. })
            {
                return Err(RunError::Conflict);
            }
            record.settlement_receipts = Some(crate::db::decode_column(&raw)?);
        }
        // Outcome-only terminal decisions are immutable. Once the deferred
        // Git import finishes, the writer terminal artifact supplies the
        // committed head and stop-work summary without republishing that
        // decision. A disagreement means the artifact no longer belongs to
        // this terminal outcome and must not be projected as current state.
        if record.settlement_receipts.as_ref().is_some_and(|receipts| {
            receipts.git_import == provenant_records::run::EffectDisposition::Done
        }) && let Ok(attempt) = self.attempt_dir(id, false)
            && let Some(raw) = match read {
                RecordRead::Durable => {
                    crate::artifacts::read_sealed(&attempt, "writer-terminal.json", RECORD_LIMIT)?
                }
                RecordRead::Observed => crate::artifacts::read_sealed_observed(
                    &attempt,
                    "writer-terminal.json",
                    RECORD_LIMIT,
                )?,
            }
        {
            let writer: crate::writer::WriterTerminal = crate::db::decode(&raw)?;
            if record.head.is_some_and(|head| head != writer.head) {
                // An early outcome may still name the launch head. Only its
                // sealed deferred-import disposition permits the later head.
                let raw =
                    crate::artifacts::read_sealed(&attempt, "terminal-effects.json", RECORD_LIMIT)?
                        .ok_or(RunError::Conflict)?;
                let effects: crate::store::TerminalEffects = crate::db::decode(&raw)?;
                if effects.scope != Self::scope(&record)
                    || !effects.effects.iter().any(|(effect, disposition)| {
                        *effect == SettlementEffect::GitImport
                            && matches!(
                                disposition,
                                EffectDisposition::Pending
                                    | EffectDisposition::Failed {
                                        retryable: true,
                                        ..
                                    }
                            )
                    })
                {
                    return Err(RunError::Conflict);
                }
            }
            record.head = Some(writer.head);
            record.stop_work = writer.stop_work;
            record.warnings = writer.warnings;
        }
        if matches!(
            record.state,
            AttemptState::Terminal {
                outcome: Outcome::Succeeded | Outcome::ReviewRequired,
                ..
            }
        ) && let Ok(attempt) = self.attempt_dir(id, false)
            && let Some(raw) = match read {
                RecordRead::Durable => {
                    crate::artifacts::read_sealed(&attempt, "delivery.json", RECORD_LIMIT)?
                }
                // A view never pays a durable flush per settled writer.
                RecordRead::Observed => {
                    crate::artifacts::read_sealed_observed(&attempt, "delivery.json", RECORD_LIMIT)?
                }
            }
        {
            let delivery: WriterDeliveryReceipt = crate::db::decode(&raw)?;
            // Only an integrate receipt may name a head other than the sealed
            // writer head: integration publishes the lane to its destination and
            // the receipt can carry the integrated tip. That exception holds only
            // for the planned destination, so a stray receipt for another branch
            // or remote never settles this attempt. Push and pr receipts keep
            // head equality, so a receipt delivered for H1 cannot settle a sealed
            // H2 that was never delivered.
            let planned = self
                .request_observed(id, call)
                .ok()
                .and_then(|request| request.task.deliver);
            // With no sealed or planned destination there is nothing to bind;
            // only head equality can then admit the receipt.
            let same_destination = record.delivery.is_none() && planned.is_none()
                || record.delivery.as_ref().is_some_and(|sealed| {
                    sealed.branch == delivery.branch && sealed.remote == delivery.remote
                })
                || planned.as_ref().is_some_and(|deliver| {
                    deliver.branch == delivery.branch && deliver.remote == delivery.remote
                });
            let integrate = planned
                .as_ref()
                .is_some_and(|deliver| matches!(deliver.mode, DeliveryMode::Integrate(_)));
            if delivery.head.is_some()
                && same_destination
                && (delivery.head == record.head || integrate)
            {
                record.delivery = Some(delivery);
            }
        }

        if let Ok(attempt) = self.attempt_dir(id, false)
            && let Some(raw) = crate::artifacts::read_sealed_observed(
                &attempt,
                provenant_records::AttemptFile::BudgetOverrun.name(),
                RECORD_LIMIT,
            )?
        {
            let overrun = crate::db::decode::<provenant_records::limit::BudgetOverrun>(&raw)?;
            record.budget_overrun = Some(overrun.clone());
            if let Some(limit) = record
                .limits
                .as_mut()
                .and_then(|limits| limits.token_budget.as_mut())
            {
                limit.overrun = Some(overrun);
            }
        }
        if let Ok(attempt) = self.attempt_dir(id, false)
            && let Some(raw) = crate::artifacts::read_sealed_observed(
                &attempt,
                provenant_records::AttemptFile::BudgetWrapUpRefused.name(),
                RECORD_LIMIT,
            )?
        {
            let refusal = crate::db::decode::<provenant_records::limit::BudgetWrapUpRefused>(&raw)?;
            record.budget_wrap_up_refused = Some(refusal.clone());
            if let Some(limit) = record
                .limits
                .as_mut()
                .and_then(|limits| limits.token_budget.as_mut())
            {
                limit.wrap_up_refused = Some(refusal);
            }
        }
        if let Ok(attempt) = self.attempt_dir(id, false)
            && let Some(raw) = crate::artifacts::read_sealed_observed(
                &attempt,
                "stall-warning.json",
                RECORD_LIMIT,
            )?
        {
            let warning: String = crate::db::decode(&raw)?;
            if !record.warnings.contains(&warning) {
                record.warnings.push(warning);
            }
        }
        if let Ok(attempt) = self.attempt_dir(id, false)
            && let Some(raw) = crate::artifacts::read_sealed_observed(
                &attempt,
                "orphan-warning.json",
                RECORD_LIMIT,
            )?
        {
            let warning: String = crate::db::decode(&raw)?;
            if !record.warnings.contains(&warning) {
                record.warnings.push(warning);
            }
        }
        let (checkpoint, checkpoint_readable) = match self.attempt_dir(id, false) {
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
                match (read_checkpoint, read) {
                    (Ok(bytes), _) => (bytes, true),
                    // Settlement reads stay strict; a live observation must not
                    // turn a damaged or unreadable checkpoint into silence.
                    (Err(error), RecordRead::Durable) => return Err(error),
                    (Err(_), RecordRead::Observed) => (None, false),
                }
            }
            Err(RunError::Filesystem(error)) if error.is_not_found() => (None, true),
            Err(error) if matches!(read, RecordRead::Durable) => return Err(error),
            Err(_) => (None, false),
        };
        record.process_observation_valid.0 = checkpoint_readable;
        if let Some(bytes) = checkpoint {
            record.process_observation_valid.0 = false;
            // This is a read-only projection of the stored observation. It is
            // never serialised back, so newer fields remain untouched in the
            // sealed source record while this reader consumes owned fields.
            #[derive(serde::Deserialize)]
            struct ProcessCheckpoint {
                schema_version: u64,
                scope: provenant_records::custody::Scope,
                #[serde(default)]
                last_output_at: Option<WallTime>,
                #[serde(default)]
                priority: Option<i32>,
            }
            let parsed = serde_json::from_slice::<ProcessCheckpoint>(&bytes);
            let scoped = parsed.as_ref().is_ok_and(|observation| {
                observation.schema_version == 1
                    && observation.scope
                        == (provenant_records::custody::Scope {
                            attempt: record.attempt_id,
                            generation: record.generation,
                        })
            });
            if matches!(read, RecordRead::Durable) {
                parsed
                    .as_ref()
                    .map_err(|_| RunError::Validation("process observation"))?;
                if !scoped {
                    return Err(RunError::Validation("process observation scope"));
                }
            }
            if let Ok(observation) = parsed
                && scoped
            {
                record.process_observation_valid.0 = true;
                record.last_output_at = observation.last_output_at;
                record.priority = observation.priority;
            }
        }
        Ok(record)
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
        let terminal = matches!(record.state, AttemptState::Terminal { .. });
        // A live attempt has no sealed result yet. What has arrived so far is
        // returned as an incomplete page, not a refusal: a caller may read
        // again after settlement. Only a settled attempt can lack an artifact.
        // With nothing arrived the page is empty and the output renderer labels
        // it no-result-yet; the reader never invents bytes or completeness.
        let partial = || {
            (
                Vec::new(),
                false,
                provenant_records::CanonicalV1::file_digest(&[]),
            )
        };
        let (bytes, complete, digest) = match part {
            ArtifactPart::Stderr => {
                let dir = self.attempt_dir(id, false)?;
                let sealed = crate::artifacts::read_sealed_observed(
                    &dir,
                    AttemptFile::Stderr.name(),
                    CAPTURE_LIMIT,
                )?;
                let tail = if sealed.is_none() {
                    crate::artifacts::read_sealed_observed(&dir, "stderr.log.tail", CAPTURE_LIMIT)?
                } else {
                    None
                };
                match (sealed, tail) {
                    (Some(bytes), _) => {
                        let digest = provenant_records::CanonicalV1::file_digest(&bytes);
                        (bytes, true, digest)
                    }
                    (None, Some(bytes)) => {
                        let digest = provenant_records::CanonicalV1::file_digest(&bytes);
                        (bytes, false, digest)
                    }
                    (None, None) if terminal => return Err(RunError::NotFound),
                    (None, None) => partial(),
                }
            }
            ArtifactPart::Transcript => {
                let session = record.session.as_deref().ok_or(RunError::NotFound)?;
                let dir = self.attempt_dir(id, false)?;
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
            ArtifactPart::Prompt => {
                let bytes = self.artifact_bytes(&record.prompt, RECORD_LIMIT)?;
                if std::str::from_utf8(&bytes).is_err() {
                    return Err(RunError::Validation("prompt/result is not UTF-8"));
                }
                (bytes, record.prompt.complete, record.prompt.digest)
            }
            ArtifactPart::Result => match record.result.as_ref() {
                Some(artifact) => {
                    let bytes = self.artifact_bytes(artifact, RESULT_LIMIT)?;
                    if std::str::from_utf8(&bytes).is_err() {
                        return Err(RunError::Validation("prompt/result is not UTF-8"));
                    }
                    (bytes, artifact.complete, artifact.digest)
                }
                None if terminal => return Err(RunError::NotFound),
                None => partial(),
            },
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
        self.lanes_streaming(query, call, |_, _, _| {})
    }

    /// Ids of every live (non-terminal, non-refused) attempt in this store,
    /// across all projects. Read-only inspection that must not depend on the
    /// caller's checkout uses this: an install replaces the shared binary for
    /// attempts in every project under the state root, not just one of them.
    pub fn live_attempt_ids_all_projects(&self, call: &Request<'_>) -> Result<Vec<String>> {
        call.check()?;
        let ids: Vec<String> = {
            let mut statement = self.db.prepare(
                "SELECT id FROM attempts WHERE refused=0 AND terminal=0 ORDER BY sequence",
            )?;
            let rows = statement.query_map([], |row| row.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for id in &ids {
            AttemptId::parse(id).map_err(|_| RunError::Validation("attempt index ID"))?;
        }
        Ok(ids)
    }

    /// Report the selected page before loading its records and then report
    /// each completed record. The cursor identifies the last inspected row.
    pub fn lanes_streaming(
        &self,
        query: &LaneQuery,
        call: &Request<'_>,
        mut report: impl FnMut(usize, Option<(u64, &AttemptRecord)>, Option<u64>),
    ) -> Result<LanePage> {
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
                // Merge ordered terminal partitions using the existing index.
                let sql = inventory_page_sql(query.all, query.newest_first, query.after != 0);
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
                    "SELECT sequence,id FROM attempts WHERE project=?1 AND refused=0 AND (?2=0 OR sequence<?2) AND id=?3".to_owned() + UNSEEN_TARGET + " ORDER BY sequence DESC LIMIT ?4"
                } else {
                    "SELECT sequence,id FROM attempts WHERE project=?1 AND refused=0 AND sequence>?2 AND id=?3".to_owned() + UNSEEN_TARGET + " ORDER BY sequence LIMIT ?4"
                };
                let mut statement = self.db.prepare(&sql)?;
                let page = statement.query_map(
                    params![project, after, target.to_string(), fetch, unseen],
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
        let total = rows.len();
        report(total, None, next);
        let mut lanes = Vec::with_capacity(total);
        for (sequence, id) in rows {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            let lane = self.record_observed(id, call)?;
            let cursor =
                u64::try_from(sequence).map_err(|_| RunError::Validation("attempt sequence"))?;
            report(total, Some((cursor, &lane)), next);
            lanes.push(lane);
        }
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
                "INSERT INTO cursors (project, actor, selection, sequence) VALUES (?1,?2,?3,?4) ON CONFLICT(project,actor,selection) DO UPDATE SET sequence=MAX(sequence,excluded.sequence)",
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
    grants.iter().all(|grant| {
        grant
            .path
            .to_path_buf()
            .is_ok_and(|path| path.is_absolute())
    })
}

pub(crate) fn validate_attempt(r: &AttemptRecord) -> Result<()> {
    if r.repository_admission_binding
        .as_ref()
        .is_some_and(|binding| !binding.valid())
    {
        return Err(RunError::Validation(
            "stored repository admission binding digest",
        ));
    }
    if r.preparation_failure.is_some()
        && (r.provider_launched != Some(false)
            || !matches!(
                r.state,
                AttemptState::Terminal {
                    outcome: Outcome::Rejected | Outcome::AuthRequired,
                    ..
                }
            ))
    {
        return Err(RunError::Validation("preparation failure evidence"));
    }
    if r.context.as_ref().is_some_and(|context| !context.valid()) {
        return Err(RunError::Validation("work context"));
    }
    if let Some(delivery) = &r.delivery {
        // An integrate receipt names the tip it published to the destination
        // ref, which can differ from the sealed writer head while the deferred
        // Git import settles. The read projection that presents such a receipt
        // is gated on the requested `DeliveryMode::Integrate` (see record_row),
        // so a push or pr receipt with a mismatched head is never admitted.
        let head_agrees = delivery.head == r.head
            || (delivery.integration.is_some()
                && matches!(delivery.state, WriterDeliveryState::Delivered { .. }));
        if delivery.schema_version != 1
            || !head_agrees
            || !matches!(
                r.state,
                AttemptState::Terminal {
                    outcome: Outcome::Succeeded,
                    ..
                }
            )
            || matches!(delivery.state, WriterDeliveryState::Delivered { .. })
                && delivery.head.is_none()
        {
            return Err(RunError::Validation("delivery receipt"));
        }
    }
    if !valid_read_only_writable(&r.read_only_writable) {
        return Err(RunError::Validation("invalid read-only writable paths"));
    }
    if !valid_write_grants(&r.write_grants) {
        return Err(RunError::Validation("invalid write grants"));
    }
    if !matches!(r.schema_version, 1 | 2) || r.attempt_number == 0 || r.revision == 0 {
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
        ref reason,
        ..
    } = r.state
    {
        if r.schema_version == 2 {
            match (&r.terminal_evidence, outcome) {
                (Some(TerminalEvidence::Unsettled { reason: evidence }), Outcome::Unsettled)
                    if !evidence.is_empty()
                        && evidence.len() <= 4096
                        && reason.as_ref() == Some(evidence) => {}
                (Some(TerminalEvidence::ProcessExit), outcome)
                    if outcome != Outcome::Unsettled && r.exit_evidence.is_some() => {}
                (
                    Some(TerminalEvidence::PreLaunchRefusal),
                    Outcome::Rejected | Outcome::Cancelled | Outcome::AuthRequired,
                ) if r.provider_launched == Some(false) => {}
                (Some(TerminalEvidence::EffectiveCancellation), Outcome::Cancelled) => {}
                (Some(TerminalEvidence::ImportCancellation), Outcome::Cancelled)
                    if r.exit_evidence.is_some()
                        && r.cancellation_disposition
                            == Some(CancellationDisposition::ImportEffective) => {}
                (Some(TerminalEvidence::Lost { reason: evidence }), Outcome::Lost)
                    if !evidence.is_empty() && reason.as_ref() == Some(evidence) => {}
                _ => return Err(RunError::Validation("terminal decision evidence")),
            }
        }
        // A proven reboot loses the attempt without manufacturing an exit or
        // result. Existing telemetry remains as it was at the crash.
        let lost = matches!(outcome, Outcome::Lost | Outcome::Unsettled) && exit_code.is_none();
        if r.result.is_none() && !lost {
            return Err(RunError::Validation("terminal result missing"));
        }
        match &r.exit_evidence {
            Some(evidence)
                if ![AttemptFile::ProcessExit, AttemptFile::ProviderExit]
                    .into_iter()
                    .map(|file| artifact_path(r.attempt_id, file.name()))
                    .collect::<Result<Vec<_>>>()?
                    .contains(&evidence.path)
                    || evidence.bytes > RECORD_LIMIT
                    || !evidence.complete =>
            {
                return Err(RunError::Validation("terminal exit artifact"));
            }
            Some(_) => {}
            None if matches!(
                outcome,
                Outcome::Rejected | Outcome::Cancelled | Outcome::AuthRequired
            ) && r.started_at.is_none() => {}
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
                || (exit_code != Some(0)
                    && !r.settlement_reason.is_some_and(|reason| {
                        matches!(
                            reason.rule().terminal_outcome,
                            provenant_records::run::SettlementTerminalOutcome::Succeeded
                        )
                    }))
                || !route_identity(
                    r.resolved_route.as_ref(),
                    r.applied_route.as_ref(),
                    r.observed_route.as_ref(),
                )
                .permits_success(&r.partial_signals))
        {
            return Err(RunError::Validation("invalid stored success"));
        }
    }
    Ok(())
}

/// Missing observations are advice; only contradictory observations hold success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouteIdentity {
    Observed,
    Unobserved,
    Mismatch,
}
impl RouteIdentity {
    pub(crate) fn permits_success(self, signals: &[PartialSignal]) -> bool {
        match self {
            Self::Observed => true,
            Self::Unobserved => signals.contains(&PartialSignal::IdentityUnobserved),
            Self::Mismatch => false,
        }
    }
}

pub(crate) fn route_identity(
    resolved: Option<&RouteRecord>,
    applied: Option<&RouteRecord>,
    observed: Option<&RouteRecord>,
) -> RouteIdentity {
    let (Some(r), Some(a)) = (resolved, applied) else {
        return RouteIdentity::Mismatch;
    };
    let valid_model = r.model.as_ref().is_some_and(|v| !v.is_empty())
        || (r.model.is_none()
            && r.effort.is_none()
            && r.provider
                .as_deref()
                .and_then(provenant_records::AdapterSpec::get)
                .is_some_and(|spec| !spec.launch.model));
    if !r.provider.as_ref().is_some_and(|v| !v.is_empty())
        || !valid_model
        || r.provider != a.provider
        || r.model != a.model
        || r.effort != a.effort
        || r.account_key != a.account_key
    {
        return RouteIdentity::Mismatch;
    }
    let Some(o) = observed else {
        return RouteIdentity::Unobserved;
    };
    // Missing fields are not a contradiction. Present fields must agree with
    // the authorised launch, including the account when it was selected.
    if o.provider
        .as_ref()
        .is_some_and(|v| Some(v) != a.provider.as_ref())
        || o.model
            .as_ref()
            .is_some_and(|v| Some(v) != a.model.as_ref())
        || (a.effort.is_some()
            && o.effort
                .as_ref()
                .is_some_and(|v| Some(v) != a.effort.as_ref()))
        || (a.account_key.is_some()
            && o.account_key
                .as_ref()
                .is_some_and(|v| Some(v) != a.account_key.as_ref()))
    {
        return RouteIdentity::Mismatch;
    }
    if o.provider.is_none()
        || (a.model.is_some() && o.model.is_none())
        || (a.effort.is_some() && o.effort.is_none())
        || (a.account_key.is_some() && o.account_key.is_none())
    {
        RouteIdentity::Unobserved
    } else {
        RouteIdentity::Observed
    }
}

fn usage_identity(
    resolved: Option<&RouteRecord>,
    applied: Option<&RouteRecord>,
    observed: Option<&RouteRecord>,
) -> provenant_records::AccountIdentity {
    use provenant_records::AccountIdentity;

    if route_identity(resolved, applied, observed) != RouteIdentity::Observed {
        return AccountIdentity::unknown();
    }
    let Some(applied) = applied else {
        return AccountIdentity::unknown();
    };
    let Some(observed) = observed else {
        return AccountIdentity::unknown();
    };
    let (Some(provider), Some(account_key)) =
        (applied.provider.as_deref(), applied.account_key.as_deref())
    else {
        return AccountIdentity::unknown();
    };
    if observed.account_key.as_deref() != Some(account_key) {
        return AccountIdentity::unknown();
    }
    let Some(spec) = provenant_records::AdapterSpec::get(provider) else {
        return AccountIdentity::unknown();
    };
    if !spec.registered_account || spec.is_unregistered_local(account_key) {
        return AccountIdentity::unknown();
    }
    AccountIdentity::provider_observation(provider, account_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_pages_use_ordered_index_scans_without_project_sort() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("schema.sql")).unwrap();
        for all in [false, true] {
            for newest in [false, true] {
                for cursor in [false, true] {
                    let sql = format!(
                        "EXPLAIN QUERY PLAN {}",
                        inventory_page_sql(all, newest, cursor)
                    );
                    let mut stmt = db.prepare(&sql).unwrap();
                    let plan: Vec<String> = stmt
                        .query_map(
                            rusqlite::params![
                                "project",
                                if cursor { 42 } else { 0 },
                                all,
                                101,
                                Option::<String>::None,
                                Option::<i64>::None
                            ],
                            |row| row.get(3),
                        )
                        .unwrap()
                        .map(|row| row.unwrap())
                        .collect();
                    assert!(
                        !plan.iter().any(|line| line.contains("TEMP B-TREE")),
                        "{plan:?}"
                    );
                    assert!(
                        plan.iter().any(|line| line.contains("terminal_attempts")),
                        "{plan:?}"
                    );
                }
            }
        }
    }

    // Spec: docs/specs/lane-contracts.md#result-projection
    #[test]
    fn observed_commands_are_unknown_when_no_adapter_exposes_them() {
        use provenant_records::lane_contract::Coverage;

        let commands = super::observed_commands();
        assert_eq!(commands.completeness, Coverage::Unknown);
        assert!(commands.value.is_none());
        assert_eq!(commands.source, "unavailable");
    }

    #[test]
    fn usage_identity_requires_a_verified_registered_applied_and_observed_key() {
        use provenant_records::{AccountIdentity, run::RouteRecord};

        let route = RouteRecord {
            provider: Some("claude".into()),
            model: Some("claude/sonnet".into()),
            account_key: Some("verified-key".into()),
            ..Default::default()
        };
        assert_eq!(
            usage_identity(Some(&route), Some(&route), Some(&route)),
            AccountIdentity::provider_observation("claude", "verified-key")
        );

        let requested_only = usage_identity(Some(&route), None, Some(&route));
        assert_eq!(requested_only, AccountIdentity::unknown());

        let mut mismatch = route.clone();
        mismatch.account_key = Some("other-key".into());
        assert_eq!(
            usage_identity(Some(&route), Some(&route), Some(&mismatch)),
            AccountIdentity::unknown()
        );

        let local = RouteRecord {
            provider: Some("agy".into()),
            model: Some("agy/gemini".into()),
            account_key: Some("local".into()),
            ..Default::default()
        };
        assert_eq!(
            usage_identity(Some(&local), Some(&local), Some(&local)),
            AccountIdentity::unknown()
        );
    }

    #[test]
    fn slot_wait_is_queue_to_start_and_absent_until_started() {
        let queued = WallTime::from_micros(1_000_000).unwrap();
        let started = WallTime::from_micros(2_500_000).unwrap();
        assert_eq!(slot_wait_ms(queued, Some(started)), Some(1_500));
        assert_eq!(slot_wait_ms(queued, None), None);
        assert_eq!(slot_wait_ms(started, Some(queued)), None);
    }

    #[test]
    fn graph_labels_come_from_the_work_context() {
        let context = provenant_records::run::WorkContext {
            tier: Some("senior".into()),
            template: Some("review.security".into()),
            parent_node: Some("map".into()),
            ..Default::default()
        };
        assert_eq!(
            graph_labels(Some(&context)),
            (
                Some("senior".into()),
                Some("review.security".into()),
                Some("map".into())
            )
        );
        assert_eq!(graph_labels(None), (None, None, None));
    }

    // Spec: docs/specs/cli.md#output-references-and-bounded-tails
    #[test]
    fn a_nonterminal_attempt_reads_as_an_unsealed_page() {
        use provenant_core::CancelToken;

        let result = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
            let temp = tempfile::tempdir()?;
            let context = crate::tests::context(temp.path())?;
            let clock = crate::tests::clock();
            let cancel = CancelToken::new();
            let call = crate::tests::call(&clock, &cancel);
            let store = RunStore::open(&context, &call)?;
            let input = crate::tests::input();
            let id = input.attempt_id;
            store.create(&input, &call)?;
            assert!(!matches!(
                store.record(id, &call)?.state,
                AttemptState::Terminal { .. }
            ));

            // A live attempt has no sealed result. The reader must not invent
            // bytes, a length or completeness; the CLI labels the empty page as
            // no-result-yet rather than a partial result.
            for part in [ArtifactPart::Result, ArtifactPart::Stderr] {
                let chunk = store.read_output(id, part, ByteRange::default(), &context, &call)?;
                assert!(chunk.bytes.is_empty(), "{part:?}");
                assert_eq!(chunk.len, 0, "{part:?}");
                assert!(!chunk.complete, "{part:?}");
            }
            Ok(())
        })();
        assert!(result.is_ok(), "{result:?}");
    }
}
