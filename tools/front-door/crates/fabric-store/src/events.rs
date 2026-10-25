use crate::*;
use provenant_records::RecordHeader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Note,
    MailSent,
    MailRead,
    TaskCreated,
    TaskClaimed,
    TaskUpdated,
    TaskRenewed,
    TaskReleased,
    TaskFinished,
    RunChanged,
    ClaimAcquired,
    ClaimRenewed,
    ClaimReleased,
    LandingAcquired,
    LandingRenewed,
    LandingReleased,
    PushChanged,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventRecord {
    pub header: RecordHeader,
    pub project: ProjectId,
    pub actor: Option<ActorId>,
    pub kind: EventKind,
    pub entity: Option<String>,
    pub detail: String,
    pub at: WallTime,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub seq: i64,
    pub record: EventRecord,
}

pub(crate) fn append(
    tx: &rusqlite::Connection,
    record: &EventRecord,
    dedup: Option<&str>,
) -> Result<i64> {
    text(&record.detail, "event detail")?;
    if let Some(key) = dedup {
        text(key, "dedup key")?;
    }
    let bytes = encode(record)?;
    if let Some(key) = dedup
        && let Some((seq, previous)) = tx
            .query_row(
                "SELECT seq,substr(record,1,?3) FROM events WHERE project=?1 AND dedup=?2",
                params![record.project.to_string(), key, MAX_RECORD_READ_BYTES],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?
    {
        if previous != bytes {
            return Err(Error::Conflict);
        }
        return Ok(seq);
    }
    tx.execute(
        "INSERT INTO events(project,dedup,record) VALUES(?,?,?)",
        params![record.project.to_string(), dedup, bytes],
    )?;
    Ok(tx.last_insert_rowid())
}
pub(crate) fn event(
    project: ProjectId,
    actor: Option<ActorId>,
    kind: EventKind,
    entity: Option<String>,
    detail: String,
    at: WallTime,
) -> EventRecord {
    EventRecord {
        header: RecordHeader::V1,
        project,
        actor,
        kind,
        entity,
        detail,
        at,
    }
}
impl Store {
    pub fn note(
        &self,
        record: &EventRecord,
        dedup: Option<&str>,
        request: &Request<'_>,
    ) -> Result<i64> {
        if record.kind != EventKind::Note {
            return Err(Error::Invalid("note kind"));
        }
        self.write(request, |tx| append(tx, record, dedup))
    }
    /// Read-only replay. The final returned seq is the next `after` value.
    pub fn events(
        &self,
        project: ProjectId,
        after: i64,
        limit: u64,
        request: &Request<'_>,
    ) -> Result<Page<Event>> {
        let limit = page(after, limit)?;
        request.retry(|| {
            let mut statement = self.db.prepare(
                "SELECT seq,substr(record,1,?4) FROM events WHERE project=?1 AND seq>?2 ORDER BY seq LIMIT ?3",
            )?;
            let mut rows = statement.query(params![
                project.to_string(),
                after,
                limit,
                MAX_RECORD_READ_BYTES
            ])?;
            let mut output = Vec::new();
            let mut total = 0;
            let mut truncated = false;
            while let Some(row) = rows.next()? {
                let bytes: Vec<u8> = row.get(1)?;
                total += bytes.len();
                if total > MAX_PAGE_BYTES {
                    truncated = true;
                    break;
                }
                output.push(Event {
                    seq: row.get(0)?,
                    record: decode(&bytes)?,
                });
            }
            Ok(Page { items: output, truncated })
        })
    }
    pub fn event_cursor(
        &self,
        project: ProjectId,
        consumer: ActorId,
        request: &Request<'_>,
    ) -> Result<i64> {
        request.retry(|| {
            Ok(self
                .db
                .query_row(
                    "SELECT seq FROM event_cursors WHERE project=? AND consumer=?",
                    params![project.to_string(), consumer.to_string()],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0))
        })
    }
    pub fn mark_events_read(
        &self,
        project: ProjectId,
        consumer: ActorId,
        seq: i64,
        request: &Request<'_>,
    ) -> Result<()> {
        if seq < 0 {
            return Err(Error::Invalid("event cursor"));
        }
        self.write(request,|tx| {
            let highest:i64=tx.query_row("SELECT coalesce(max(seq),0) FROM events WHERE project=?",[project.to_string()],|r|r.get(0))?;
            if seq>highest { return Err(Error::Invalid("event cursor")); }
            tx.execute("INSERT INTO event_cursors VALUES(?,?,?) ON CONFLICT(project,consumer) DO UPDATE SET seq=max(seq,excluded.seq)",params![project.to_string(),consumer.to_string(),seq])?;
            Ok(())
        })
    }
}
