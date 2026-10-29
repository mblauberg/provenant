use crate::*;
use provenant_core::{HostId, MessageId, TaskId};
use provenant_records::{ArtifactRef, RecordHeader};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Note,
    Request,
    Response,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum MessageRef {
    Local { id: MessageId },
    Remote { host: HostId, id: MessageId },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskRef {
    Local { id: TaskId },
    Remote { host: HostId, id: TaskId },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub header: RecordHeader,
    pub project: ProjectId,
    pub id: MessageId,
    pub sender: ActorId,
    pub recipients: Vec<ActorId>,
    pub conversation: Option<MessageRef>,
    pub kind: MessageKind,
    pub body: String,
    pub artifacts: Vec<ArtifactRef>,
    pub reply_to: Option<MessageRef>,
    pub task: Option<TaskRef>,
    pub created_at: WallTime,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub seq: i64,
    pub message: Message,
}

pub(crate) fn insert_message(tx: &rusqlite::Connection, message: &Message) -> Result<bool> {
    text(&message.body, "body")?;
    if message.recipients.is_empty()
        || message.recipients.len() > 100
        || message
            .recipients
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            != message.recipients.len()
        || message.artifacts.len() > 100
    {
        return Err(Error::Invalid("recipients or artifacts"));
    }
    for artifact in &message.artifacts {
        if artifact.path.as_path_text().is_none() {
            return Err(Error::Invalid("artifact path encoding"));
        }
        artifact
            .path
            .to_relative_path()
            .map_err(|_| Error::Invalid("artifact path"))?;
    }
    let bytes = encode(message)?;
    let previous: Option<Vec<u8>> = tx
        .query_row(
            "SELECT substr(record,1,?3) FROM messages WHERE project=?1 AND id=?2",
            params![
                message.project.to_string(),
                message.id.to_string(),
                MAX_RECORD_READ_BYTES
            ],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(previous) = previous {
        if previous != bytes {
            return Err(Error::Conflict);
        }
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO messages VALUES(?,?,?)",
        params![message.project.to_string(), message.id.to_string(), bytes],
    )?;
    tx.execute(
        "INSERT INTO delivery_sequences VALUES(?,0) ON CONFLICT(project) DO NOTHING",
        [message.project.to_string()],
    )?;
    for recipient in &message.recipients {
        let seq: i64 = tx.query_row(
            "UPDATE delivery_sequences SET seq=seq+1 WHERE project=? AND seq<9223372036854775807 RETURNING seq",
            [message.project.to_string()], |row| row.get(0),
        ).optional()?.ok_or(Error::Invalid("delivery sequence exhausted"))?;
        tx.execute(
            "INSERT INTO deliveries(project,message,recipient,seq) VALUES(?,?,?,?)",
            params![
                message.project.to_string(),
                message.id.to_string(),
                recipient.to_string(),
                seq
            ],
        )?;
    }
    append(
        tx,
        &event(
            message.project,
            Some(message.sender),
            EventKind::MailSent,
            Some(message.id.to_string()),
            "sent".into(),
            message.created_at,
        ),
        None,
    )?;
    Ok(true)
}
impl Store {
    /// IDs and timestamps are frozen by the caller before any BUSY retry.
    pub fn send(&self, message: &Message, request: &Request<'_>) -> Result<()> {
        self.write(request, |tx| insert_message(tx, message).map(|_| ()))
    }
    pub fn mail_cursor(
        &self,
        project: ProjectId,
        recipient: ActorId,
        request: &Request<'_>,
    ) -> Result<i64> {
        request.retry(|| {
            Ok(self
                .db
                .query_row(
                    "SELECT seq FROM mail_cursors WHERE project=? AND recipient=?",
                    params![project.to_string(), recipient.to_string()],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0))
        })
    }
    /// `None` reads after the durable cursor; `Some` explicitly replays history.
    pub fn read_mail(
        &self,
        project: ProjectId,
        recipient: ActorId,
        after: Option<i64>,
        limit: u64,
        request: &Request<'_>,
    ) -> Result<Page<Delivery>> {
        let limit = page(after.unwrap_or(0), limit)?;
        request.retry(|| {
            let mut statement=self.db.prepare("SELECT d.seq,substr(m.record,1,?5),m.id FROM deliveries d JOIN messages m ON m.project=d.project AND m.id=d.message WHERE d.project=?1 AND d.recipient=?2 AND d.seq>coalesce(?3,(SELECT seq FROM mail_cursors WHERE project=?1 AND recipient=?2),0) ORDER BY d.seq LIMIT ?4")?;
            let mut rows=statement.query(params![project.to_string(),recipient.to_string(),after,limit,MAX_RECORD_READ_BYTES])?; let mut output=Vec::new();let mut total=0; let mut truncated=false;
            while let Some(row)=rows.next()? { let bytes:Vec<u8>=row.get(1)?;total+=bytes.len();if total>MAX_PAGE_BYTES { truncated=true; break; }
                let message: Message=decode(&bytes)?;
                let id: String=row.get(2)?;
                if message.project != project || message.id.to_string() != id { return Err(Error::Invalid("stored message identity")); }
                output.push(Delivery {seq:row.get(0)?,message}); }
            Ok(Page { items: output, truncated })
        })
    }
    /// The service supplies the authenticated recipient, never a mail sender.
    pub fn mark_mail_read(
        &self,
        project: ProjectId,
        recipient: ActorId,
        seq: i64,
        request: &Request<'_>,
    ) -> Result<()> {
        if seq < 0 {
            return Err(Error::Invalid("mail cursor"));
        }
        self.write(request,|tx| {
            let at = request.clock.wall_now()?;
            let highest:i64=tx.query_row("SELECT coalesce(max(seq),0) FROM deliveries WHERE project=? AND recipient=?",params![project.to_string(),recipient.to_string()],|r|r.get(0))?;
            if seq>highest { return Err(Error::Invalid("mail cursor")); }
            let previous:i64=tx.query_row("SELECT seq FROM mail_cursors WHERE project=? AND recipient=?",params![project.to_string(),recipient.to_string()],|r|r.get(0)).optional()?.unwrap_or(0);
            if seq>previous {
                tx.execute("INSERT INTO mail_cursors VALUES(?,?,?) ON CONFLICT(project,recipient) DO UPDATE SET seq=max(seq,excluded.seq)",params![project.to_string(),recipient.to_string(),seq])?;
                append(tx,&event(project,Some(recipient),EventKind::MailRead,None,seq.to_string(),at),None)?;
            }
            Ok(())
        })
    }
}
