//! Atomic persistence of human requests, immutable answers and delivery intent.
use crate::*;
use provenant_core::{HumanAnswerId, HumanRequestId};
use provenant_records::inbox::*;

/// Preserve history and pending deliveries; refuse growth rather than pruning them.
pub const MAX_HUMAN_REQUESTS: i64 = 10_000;

/// The fields needed by an overview; request bodies never leave the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingHumanRequest {
    pub id: String,
    pub project_id: String,
    pub project_key: Option<String>,
    pub title: String,
    pub chair_session_id: Option<String>,
}

pub struct PendingHumanRequests {
    pub items: Vec<PendingHumanRequest>,
    pub truncated: bool,
    pub bytes_read: usize,
}

pub struct HumanClosure<'a> {
    pub project: ProjectId,
    pub id: HumanRequestId,
    pub actor: ActorId,
    pub revision: u64,
    pub state: RequestState,
    pub reason: &'a str,
    pub at: WallTime,
}
pub struct HumanAcknowledgement<'a> {
    pub project: ProjectId,
    pub id: HumanRequestId,
    pub answer: HumanAnswerId,
    pub host: provenant_core::HostId,
    pub actor: ActorId,
    pub session: &'a str,
    pub at: WallTime,
}

impl Store {
    /// Exclude answers that cannot fit a hook envelope without consuming mail.
    /// Re-read each row so acknowledgement and destination changes win races.
    pub fn mark_hook_undeliverable_once(
        root: &provenant_core::StateRoot,
        target: &AnswerTarget,
        ids: &[String],
        deadline: std::time::Instant,
    ) -> Result<Vec<String>> {
        use crate::local_delivery::before;
        before(deadline)?;
        let store = Self::open_hook_delivery(root, deadline)?.ok_or(Error::NotFound)?;
        let tx = rusqlite::Transaction::new_unchecked(
            &store.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let mut marked = Vec::new();
        for id in ids {
            before(deadline)?;
            let bytes: Option<Vec<u8>> = tx.query_row(
                "SELECT substr(record,1,?3) FROM human_answer_outbox WHERE project=?1 AND answer=?2",
                params![target.project.to_string(), id, MAX_RECORD_READ_BYTES],
                |row| row.get(0),
            ).optional()?;
            let Some(Ok(mut delivery)) = bytes.as_deref().map(decode::<AnswerDelivery>) else {
                continue;
            };
            if delivery.schema_version != 1
                || delivery.answer_id.to_string() != *id
                || delivery.target_host != target.host
                || delivery.target_actor != target.actor
                || delivery.target_session != target.session
                || delivery.state == AnswerDeliveryState::Acknowledged
                || delivery.error.is_some()
            {
                continue;
            }
            delivery.error = Some(crate::local_delivery::HOOK_ENVELOPE_TOO_LARGE.to_owned());
            tx.execute(
                "UPDATE human_answer_outbox SET record=?3 WHERE project=?1 AND answer=?2",
                params![target.project.to_string(), id, encode(&delivery)?],
            )?;
            marked.push(id.clone());
        }
        before(deadline)?;
        tx.commit()?;
        Ok(marked)
    }

    /// Record selected hook candidates once per exact destination. Offered
    /// snapshots can be replayed idempotently until acknowledgement, so a late
    /// commit or failed pipe write never consumes the answer. No creation,
    /// migration, busy retries or history scan.
    pub fn offer_human_answers_once(
        root: &provenant_core::StateRoot,
        target: &AnswerTarget,
        candidates: Page<AnswerOffer>,
        at: WallTime,
        deadline: std::time::Instant,
    ) -> Result<Page<AnswerOffer>> {
        use crate::local_delivery::before;
        before(deadline)?;
        if candidates.items.len() as u64 > MAX_LOCAL_OFFER_ITEMS {
            return Err(Error::Invalid("offer count"));
        }
        let store = Self::open_local_delivery(root, deadline, true)?.ok_or(Error::NotFound)?;
        let tx = rusqlite::Transaction::new_unchecked(
            &store.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let mut offered = Page {
            items: Vec::new(),
            truncated: candidates.truncated,
        };
        for mut candidate in candidates.items {
            before(deadline)?;
            let bytes: Option<Vec<u8>> = tx.query_row("SELECT substr(record,1,?3) FROM human_answer_outbox WHERE project=?1 AND answer=?2",
                params![target.project.to_string(), candidate.answer.id.to_string(), MAX_RECORD_READ_BYTES], |row| row.get(0)).optional()?;
            let delivery = bytes.as_deref().map(decode::<AnswerDelivery>);
            let Some(Ok(mut delivery)) = delivery else {
                crate::local_delivery::invalid_answer(&candidate.answer.id.to_string());
                continue;
            };
            if delivery.schema_version != 1
                || delivery.answer_id != candidate.answer.id
                || delivery.target_host != target.host
                || delivery.target_actor != target.actor
                || delivery.target_session != target.session
            {
                crate::local_delivery::invalid_answer(&candidate.answer.id.to_string());
                continue;
            }
            if delivery.state == AnswerDeliveryState::Offered && candidate.delivery == delivery {
                offered.items.push(candidate);
                continue;
            }
            if delivery.state != AnswerDeliveryState::Queued {
                continue;
            }
            delivery.state = AnswerDeliveryState::Offered;
            delivery.attempts = delivery
                .attempts
                .checked_add(1)
                .ok_or(Error::Invalid("offer attempts"))?;
            delivery.last_attempt_at = Some(at);
            tx.execute(
                "UPDATE human_answer_outbox SET record=?3 WHERE project=?1 AND answer=?2",
                params![
                    target.project.to_string(),
                    candidate.answer.id.to_string(),
                    encode(&delivery)?
                ],
            )?;
            candidate.delivery = delivery;
            offered.items.push(candidate);
        }
        before(deadline)?;
        tx.commit()?;
        Ok(offered)
    }

    /// Read a bounded batch across projects, without loading request bodies.
    /// The session column is authoritative for routing and this projection.
    pub fn pending_human_requests(
        &self,
        limit: u64,
        max_bytes: usize,
        request: &Request<'_>,
    ) -> Result<PendingHumanRequests> {
        let rows_limit = page(0, limit)?;
        request.retry(|| {
            let mut statement = self.db.prepare(
                "WITH recent AS (SELECT project,id,length(record) AS bytes FROM human_requests
                   WHERE state='open' ORDER BY project,id LIMIT ?1),
                 sized AS (SELECT project,id,bytes,sum(bytes) OVER (ORDER BY project,id) AS total FROM recent)
                 SELECT s.project,s.id,s.bytes,s.total,
                 CASE WHEN s.total<=?3 AND s.bytes<=?2 AND json_valid(CAST(r.record AS TEXT))
                   THEN json_extract(CAST(r.record AS TEXT),'$.schema_version') END,
                 CASE WHEN s.total<=?3 AND s.bytes<=?2 AND json_valid(CAST(r.record AS TEXT))
                   THEN json_extract(CAST(r.record AS TEXT),'$.project_id') END,
                 CASE WHEN s.total<=?3 AND s.bytes<=?2 AND json_valid(CAST(r.record AS TEXT))
                   THEN json_extract(CAST(r.record AS TEXT),'$.id') END,
                 CASE WHEN s.total<=?3 AND s.bytes<=?2 AND json_valid(CAST(r.record AS TEXT))
                   THEN json_extract(CAST(r.record AS TEXT),'$.project_key') END,
                 CASE WHEN s.total<=?3 AND s.bytes<=?2 AND json_valid(CAST(r.record AS TEXT))
                   THEN json_extract(CAST(r.record AS TEXT),'$.request.title') END,
                 CASE WHEN s.total<=?3 AND s.bytes<=?2 AND json_valid(CAST(r.record AS TEXT))
                   THEN r.session END,
                 CASE WHEN s.total<=?3 AND s.bytes<=?2 AND json_valid(CAST(r.record AS TEXT))
                   THEN json_type(CAST(r.record AS TEXT),'$.chair_session_id') END
                 FROM sized s JOIN human_requests r ON r.project=s.project AND r.id=s.id ORDER BY s.project,s.id",
            )?;
            let mut rows = statement.query(params![rows_limit + 1, MAX_RECORD_BYTES as i64, max_bytes.min(1_048_576) as i64])?;
            let mut items = Vec::new();
            let mut bytes_read = 0;
            let mut truncated = false;
            while let Some(row) = rows.next()? {
                request.check()?;
                if items.len() == limit as usize {
                    truncated = true;
                    break;
                }
                let size: i64 = row.get(2)?;
                let total: i64 = row.get(3)?;
                if size < 0 || total < 0 || size > MAX_RECORD_BYTES as i64 || total > max_bytes.min(1_048_576) as i64 {
                    truncated = true;
                    break;
                }
                bytes_read = total as usize;
                let project_id: String = row.get(0)?;
                let id: String = row.get(1)?;
                let version: Option<i64> = row.get(4)?;
                let record_project: Option<String> = row.get(5)?;
                let record_id: Option<String> = row.get(6)?;
                let project_key: Option<String> = row.get(7)?;
                let title: Option<String> = row.get(8)?;
                let chair_session_id: Option<String> = row.get(9)?;
                let session_type: Option<String> = row.get(10)?;
                if version != Some(1)
                    || record_project.as_deref() != Some(&project_id)
                    || record_id.as_deref() != Some(&id)
                    || title.is_none()
                    || chair_session_id.is_none()
                    || !matches!(session_type.as_deref(), None | Some("text"))
                {
                    return Err(Error::Invalid("stored request identity or version"));
                }
                let title = title.unwrap_or_default();
                items.push(PendingHumanRequest {
                    id,
                    project_id,
                    project_key,
                    title,
                    chair_session_id,
                });
            }
            Ok(PendingHumanRequests { items, truncated, bytes_read })
        })
    }

    /// Atomically record bounded at-least-once offers. Overflow is untouched;
    /// reading ordinary mail never advances or acknowledges this outbox.
    pub fn offer_human_answers(
        &self,
        target: &AnswerTarget,
        limit: u64,
        max_bytes: usize,
        at: WallTime,
        request: &Request<'_>,
    ) -> Result<Page<AnswerOffer>> {
        page(0, limit)?;
        if !(64..=MAX_OFFER_BYTES).contains(&max_bytes) {
            return Err(Error::Invalid("offer byte limit"));
        }
        self.write(request, |tx| {
            let mut statement = tx.prepare(
                "SELECT substr(o.record,1,?3),substr(a.record,1,?3)
                 FROM human_answer_outbox o
                 JOIN human_answers a ON a.project=o.project AND a.id=o.answer
                 JOIN human_requests r ON r.project=a.project AND r.id=a.request
                 WHERE r.project=?1 AND r.session=?2 ORDER BY o.answer LIMIT ?4",
            )?;
            let mut rows = statement.query(params![
                target.project.to_string(),
                target.session,
                MAX_RECORD_READ_BYTES,
                MAX_HUMAN_REQUESTS + 1
            ])?;
            let mut batch = Page {
                items: Vec::new(),
                truncated: false,
            };
            let mut scanned = 0;
            while let Some(row) = rows.next()? {
                request.check()?;
                scanned += 1;
                if scanned > MAX_HUMAN_REQUESTS {
                    return Err(Error::Invalid("human request retention limit"));
                }
                let mut delivery: AnswerDelivery = decode(&row.get::<_, Vec<u8>>(0)?)?;
                if delivery.target_host != target.host
                    || delivery.target_actor != target.actor
                    || delivery.target_session != target.session
                    || delivery.state == AnswerDeliveryState::Acknowledged
                {
                    continue;
                }
                if batch.items.len() == limit as usize {
                    batch.truncated = true;
                    break;
                }
                let answer: HumanAnswer = decode(&row.get::<_, Vec<u8>>(1)?)?;
                if delivery.schema_version != 1
                    || answer.schema_version != 1
                    || delivery.answer_id != answer.id
                {
                    return Err(Error::Invalid("stored answer identity or version"));
                }
                delivery.state = AnswerDeliveryState::Offered;
                delivery.attempts = delivery
                    .attempts
                    .checked_add(1)
                    .ok_or(Error::Invalid("offer attempts"))?;
                delivery.last_attempt_at = Some(at);
                batch.items.push(AnswerOffer { answer, delivery });
                // Reserve the entire JSON envelope, including escaping and the
                // longer `false` spelling of the final truncation flag.
                if encode(&batch)?.len() > max_bytes {
                    batch.items.pop();
                    batch.truncated = true;
                    continue;
                }
            }
            for offer in &batch.items {
                tx.execute(
                    "UPDATE human_answer_outbox SET record=?3 WHERE project=?1 AND answer=?2",
                    params![
                        target.project.to_string(),
                        offer.answer.id.to_string(),
                        encode(&offer.delivery)?
                    ],
                )?;
            }
            Ok(batch)
        })
    }

    pub fn human_generation(&self, project: ProjectId, request: &Request<'_>) -> Result<u64> {
        request.retry(|| {
            let generation: i64 = self.db.query_row(
                "SELECT coalesce(sum(revision),0) FROM human_requests WHERE project=?1",
                [project.to_string()],
                |row| row.get(0),
            )?;
            u64::try_from(generation).map_err(|_| Error::Invalid("inbox generation"))
        })
    }
    pub fn human_request(
        &self,
        project: ProjectId,
        id: HumanRequestId,
        request: &Request<'_>,
    ) -> Result<HumanRequest> {
        request.retry(|| load(&self.db, project, id))
    }

    pub fn human_item(
        &self,
        project: ProjectId,
        id: HumanRequestId,
        request: &Request<'_>,
    ) -> Result<InboxItem> {
        request.retry(|| {
            // One SQLite snapshot for the request, answer, outbox and history.
            let tx = self.db.unchecked_transaction()?;
            let item = item(&tx, project, id)?;
            tx.commit()?;
            Ok(item)
        })
    }

    pub fn human_requests(
        &self,
        project: ProjectId,
        session: Option<&str>,
        all: bool,
        after: Option<HumanRequestId>,
        limit: u64,
        request: &Request<'_>,
    ) -> Result<Page<HumanRequest>> {
        let rows_limit = page(0, limit)?;
        request.retry(|| {
            let mut statement = self.db.prepare("SELECT substr(record,1,?6) FROM human_requests WHERE project=?1 AND (?2 IS NULL OR session=?2) AND (?3 OR state='open') AND (?4 IS NULL OR id>?4) ORDER BY id LIMIT ?5")?;
            let mut rows = statement.query(params![project.to_string(),session,all,after.map(|id|id.to_string()),rows_limit + 1,MAX_RECORD_READ_BYTES])?;
            let mut items = Vec::new();
            let mut bytes = 0;
            let mut truncated = false;
            while let Some(row) = rows.next()? {
                let record: Vec<u8> = row.get(0)?;
                if items.len() == limit as usize || bytes + record.len() > 512 * 1024 {
                    truncated = true;
                    break;
                }
                let value: HumanRequest = decode(&record)?;
                if value.project_id != project { return Err(Error::Invalid("stored request identity")); }
                bytes += record.len();
                items.push(value);
            }
            Ok(Page { items, truncated })
        })
    }

    /// Called inside coordination_operation so the effect and replay receipt commit together.
    pub fn create_human_request(&self, value: &HumanRequest, request: &Request<'_>) -> Result<()> {
        self.write(request, |tx| {
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM human_requests WHERE project=?1",
                [value.project_id.to_string()],
                |row| row.get(0),
            )?;
            if count >= MAX_HUMAN_REQUESTS {
                return Err(Error::Invalid("human request retention limit"));
            }
            tx.execute(
                "INSERT INTO human_requests VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    value.project_id.to_string(),
                    value.id.to_string(),
                    revision(value.revision)?,
                    value.chair_session_id,
                    state_text(value.state),
                    encode(value)?
                ],
            )?;
            history(tx, value)
        })
    }

    /// All four effects roll back together, including the ordinary Fabric mail.
    pub fn answer_human_request(
        &self,
        answer: &HumanAnswer,
        delivery: &AnswerDelivery,
        message: &Message,
        request: &Request<'_>,
    ) -> Result<HumanRequest> {
        self.write(request, |tx| {
            let mut value = load(tx, message.project, answer.request_id)?;
            if value.revision != answer.request_revision || value.state != RequestState::Open {
                return Err(Error::Conflict);
            }
            value.revision += 1;
            value.state = match answer.action {
                AnswerAction::Confirm => RequestState::Confirmed,
                AnswerAction::Override => RequestState::Overridden,
                _ => RequestState::Answered,
            };
            value.updated_at.clone_from(&answer.answered_at);
            value.answer_id = Some(answer.id);
            tx.execute(
                "INSERT INTO human_answers VALUES(?1,?2,?3,?4)",
                params![
                    value.project_id.to_string(),
                    answer.id.to_string(),
                    answer.request_id.to_string(),
                    encode(answer)?
                ],
            )?;
            tx.execute(
                "INSERT INTO human_answer_outbox VALUES(?1,?2,?3)",
                params![
                    value.project_id.to_string(),
                    answer.id.to_string(),
                    encode(delivery)?
                ],
            )?;
            crate::messages::insert_message(tx, message)?;
            update(tx, &value)?;
            Ok(value)
        })
    }

    pub fn close_human_request(
        &self,
        closure: HumanClosure<'_>,
        request: &Request<'_>,
    ) -> Result<HumanRequest> {
        let HumanClosure {
            project,
            id,
            actor,
            revision,
            state,
            reason,
            at,
        } = closure;
        self.write(request, |tx| {
            let mut value = load(tx, project, id)?;
            if value.revision != revision || value.chair_actor != actor {
                return Err(Error::Conflict);
            }
            if !matches!(
                state,
                RequestState::Withdrawn | RequestState::Superseded | RequestState::Expired
            ) {
                return Err(Error::Invalid("closure state"));
            }
            if matches!(
                value.state,
                RequestState::Withdrawn | RequestState::Superseded | RequestState::Expired
            ) {
                return Err(Error::Conflict);
            }
            value.revision += 1;
            value.state = state;
            value.updated_at = at;
            value.closure_reason = Some(reason.into());
            update(tx, &value)?;
            Ok(value)
        })
    }

    pub fn acknowledge_human_answer(
        &self,
        acknowledgement: HumanAcknowledgement<'_>,
        request: &Request<'_>,
    ) -> Result<AnswerDelivery> {
        let HumanAcknowledgement {
            project,
            id,
            answer,
            host,
            actor,
            session,
            at,
        } = acknowledgement;
        self.write(request, |tx| {
            let value = load(tx, project, id)?;
            if value.answer_id != Some(answer) {
                return Err(Error::Conflict);
            }
            let mut delivery = delivery(tx, project, answer)?;
            if delivery.target_host != host
                || delivery.target_actor != actor
                || delivery.target_session != session
            {
                return Err(Error::Conflict);
            }
            if delivery.state != AnswerDeliveryState::Acknowledged {
                delivery.state = AnswerDeliveryState::Acknowledged;
                delivery.acknowledged_at = Some(at);
                tx.execute(
                    "UPDATE human_answer_outbox SET record=?3 WHERE project=?1 AND answer=?2",
                    params![project.to_string(), answer.to_string(), encode(&delivery)?],
                )?;
            }
            Ok(delivery)
        })
    }
}

fn load(db: &rusqlite::Connection, project: ProjectId, id: HumanRequestId) -> Result<HumanRequest> {
    let bytes: Vec<u8> = db
        .query_row(
            "SELECT substr(record,1,?3) FROM human_requests WHERE project=?1 AND id=?2",
            params![project.to_string(), id.to_string(), MAX_RECORD_READ_BYTES],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let value: HumanRequest = decode(&bytes)?;
    if value.project_id != project || value.id != id || value.schema_version != 1 {
        return Err(Error::Invalid("stored request identity or version"));
    }
    Ok(value)
}

fn delivery(
    db: &rusqlite::Connection,
    project: ProjectId,
    id: HumanAnswerId,
) -> Result<AnswerDelivery> {
    let bytes: Vec<u8> = db.query_row(
        "SELECT substr(record,1,?3) FROM human_answer_outbox WHERE project=?1 AND answer=?2",
        params![project.to_string(), id.to_string(), MAX_RECORD_READ_BYTES],
        |r| r.get(0),
    )?;
    decode(&bytes)
}

fn item(db: &rusqlite::Connection, project: ProjectId, id: HumanRequestId) -> Result<InboxItem> {
    let value = load(db, project, id)?;
    let (answer, delivery) = if let Some(id) = value.answer_id {
        let bytes: Vec<u8> = db.query_row(
            "SELECT substr(record,1,?3) FROM human_answers WHERE project=?1 AND id=?2",
            params![project.to_string(), id.to_string(), MAX_RECORD_READ_BYTES],
            |r| r.get(0),
        )?;
        (Some(decode(&bytes)?), Some(delivery(db, project, id)?))
    } else {
        (None, None)
    };
    // A request can only open, resolve once, and close once. No unbounded history read.
    let mut statement = db.prepare("SELECT substr(record,1,?3) FROM human_request_history WHERE project=?1 AND request=?2 ORDER BY revision LIMIT 4")?;
    let rows = statement.query_map(
        params![project.to_string(), id.to_string(), MAX_RECORD_READ_BYTES],
        |r| r.get::<_, Vec<u8>>(0),
    )?;
    let history = rows
        .map(|r| decode(&r?))
        .collect::<Result<Vec<HumanRequest>>>()?;
    if history.len() > 3 {
        return Err(Error::Invalid("request history limit"));
    }
    Ok(InboxItem {
        request: value,
        answer,
        delivery,
        history,
    })
}

fn state_text(state: RequestState) -> &'static str {
    match state {
        RequestState::Open => "open",
        RequestState::Answered => "answered",
        RequestState::Confirmed => "confirmed",
        RequestState::Overridden => "overridden",
        RequestState::Withdrawn => "withdrawn",
        RequestState::Superseded => "superseded",
        RequestState::Expired => "expired",
    }
}
fn history(tx: &rusqlite::Connection, value: &HumanRequest) -> Result<()> {
    tx.execute(
        "INSERT INTO human_request_history VALUES(?1,?2,?3,?4)",
        params![
            value.project_id.to_string(),
            value.id.to_string(),
            revision(value.revision)?,
            encode(value)?
        ],
    )?;
    Ok(())
}
fn update(tx: &rusqlite::Connection, value: &HumanRequest) -> Result<()> {
    tx.execute(
        "UPDATE human_requests SET revision=?3,state=?4,record=?5 WHERE project=?1 AND id=?2",
        params![
            value.project_id.to_string(),
            value.id.to_string(),
            revision(value.revision)?,
            state_text(value.state),
            encode(value)?
        ],
    )?;
    history(tx, value)
}
fn revision(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Invalid("revision"))
}
