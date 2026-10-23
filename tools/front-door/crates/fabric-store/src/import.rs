//! An explicit, bounded copy from a supplied quiescent old-schema snapshot.
//! No root discovery, fallback, source migration, journal or checkpoint writes.
use crate::*;
use crate::{
    events::{append, event},
    messages::insert_message,
    tasks::insert_task,
};
use provenant_core::{MessageId, TaskId};
use provenant_records::RecordHeader;
use provenant_supervisor::fs::FileHandle;
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::PathBuf,
};

const MAX_IMPORT_ROWS: usize = 10_000;
const MAX_IMPORT_BYTES: usize = 8_388_608;
const MAX_SNAPSHOT_BYTES: u64 = 67_108_864;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportPlan {
    pub snapshot: PathBuf,
    pub source: String,
    pub legacy_project: String,
    pub project: ProjectId,
    /// Supplied by the identity edge: old labels bind to current actor IDs.
    pub actors: BTreeMap<String, ActorId>,
    pub cutover: WallTime,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportReceipt {
    pub tasks: u64,
    pub messages: u64,
    pub skipped: bool,
}
struct OldTask {
    id: String,
    objective: String,
    state: String,
    created: i64,
}
struct OldMessage {
    id: String,
    sender: String,
    body: String,
    conversation: String,
    reply: Option<String>,
    task: Option<String>,
    output: Option<String>,
    created: i64,
    recipients: Vec<String>,
}
fn old_time(ms: i64) -> Result<WallTime> {
    Ok(WallTime::from_micros(
        ms.checked_mul(1000)
            .ok_or(Error::Invalid("legacy timestamp"))?,
    )?)
}
fn id(plan: &ImportPlan, kind: &str, id: &str) -> Result<[u8; 16]> {
    let digest = CanonicalV1::record_digest(kind, 1, &(&plan.source, &plan.legacy_project, id))?;
    let mut bytes = [0; 16];
    bytes.copy_from_slice(
        digest
            .as_bytes()
            .get(..16)
            .ok_or(Error::Invalid("digest length"))?,
    );
    Ok(bytes)
}
fn legacy(plan: &ImportPlan, id: &str) -> LegacyRef {
    LegacyRef {
        source: plan.source.clone(),
        project: plan.legacy_project.clone(),
        id: id.into(),
    }
}
fn actor(plan: &ImportPlan, label: &str) -> Result<ActorId> {
    plan.actors
        .get(label)
        .copied()
        .ok_or(Error::Invalid("unmapped legacy actor"))
}
fn message_ref(plan: &ImportPlan, key: &str, subset: &BTreeSet<String>) -> Result<MessageRef> {
    if subset.contains(key) {
        Ok(MessageRef::Local {
            id: MessageId::from_bytes(id(plan, "fabric-import-message", key)?),
        })
    } else {
        Ok(MessageRef::Legacy {
            reference: legacy(plan, key),
        })
    }
}
impl Store {
    pub fn import_snapshot(
        &self,
        plan: &ImportPlan,
        request: &Request<'_>,
    ) -> Result<ImportReceipt> {
        text(&plan.source, "source")?;
        text(&plan.legacy_project, "legacy project")?;
        if let Some(receipt) = self.imported(plan, request)? {
            return Ok(receipt);
        }
        // Reject WAL *before* SQLite opens it. Read-only SQLite alone may need
        // writable SHM. Never use immutable=1 against a potentially changing DB.
        let mut file = FileHandle::open_source(&plan.snapshot)?;
        if file.metadata()?.size > MAX_SNAPSHOT_BYTES {
            return Err(Error::ImportLimit);
        }
        let mut header = [0; 100];
        file.read_exact(&mut header).map_err(|source| Error::Io {
            path: plan.snapshot.clone(),
            source,
        })?;
        if header.get(..16) != Some(b"SQLite format 3\0")
            || header.get(18) != Some(&1)
            || header.get(19) != Some(&1)
        {
            return Err(Error::ImportSource);
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = plan.snapshot.as_os_str().to_os_string();
            sidecar.push(suffix);
            match std::fs::symlink_metadata(PathBuf::from(sidecar)) {
                Ok(_) => return Err(Error::ImportSource),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(Error::Io {
                        path: plan.snapshot.clone(),
                        source,
                    });
                }
            }
        }
        let mut old = Connection::open_with_flags(
            &plan.snapshot,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        old.busy_timeout(std::time::Duration::ZERO)?;
        let snapshot = old.transaction()?;
        let mut budget = 0_usize;
        let mut tasks = Vec::new();
        {
            let mut statement=snapshot.prepare("SELECT task_id,objective,state,created_at FROM tasks WHERE project=? AND state IN ('open','blocked') AND created_at<=? ORDER BY task_id LIMIT 10001")?;
            let mut rows = statement.query(params![
                plan.legacy_project,
                plan.cutover.as_micros().div_euclid(1000)
            ])?;
            while let Some(row) = rows.next()? {
                let task = OldTask {
                    id: row.get(0)?,
                    objective: row.get(1)?,
                    state: row.get(2)?,
                    created: row.get(3)?,
                };
                budget =
                    budget.saturating_add(task.id.len() + task.objective.len() + task.state.len());
                tasks.push(task);
                if tasks.len() > MAX_IMPORT_ROWS || budget > MAX_IMPORT_BYTES {
                    return Err(Error::ImportLimit);
                }
            }
        }
        let cutoff = plan
            .cutover
            .as_micros()
            .checked_sub(14 * 86_400_000_000)
            .ok_or(Error::Invalid("import cutoff"))?;
        let cutoff_ms = cutoff.div_euclid(1000) + i64::from(cutoff.rem_euclid(1000) != 0);
        let mut messages = Vec::<OldMessage>::new();
        let mut deliveries = 0_usize;
        {
            let mut statement=snapshot.prepare("SELECT m.message_id,m.sender_id,m.body,m.conversation_id,m.reply_to,m.task_id,m.output_path,m.created_at,d.recipient_id FROM messages m JOIN deliveries d ON d.message_id=m.message_id AND d.project=m.project WHERE m.project=? AND m.kind='request' AND m.created_at>=? AND m.created_at<=? AND d.read_at IS NULL ORDER BY m.message_id,d.recipient_id LIMIT 10001")?;
            let mut rows = statement.query(params![
                plan.legacy_project,
                cutoff_ms,
                plan.cutover.as_micros().div_euclid(1000)
            ])?;
            while let Some(row) = rows.next()? {
                let key: String = row.get(0)?;
                let recipient: String = row.get(8)?;
                deliveries += 1;
                if messages.last().is_none_or(|message| message.id != key) {
                    messages.push(OldMessage {
                        id: key,
                        sender: row.get(1)?,
                        body: row.get(2)?,
                        conversation: row.get(3)?,
                        reply: row.get(4)?,
                        task: row.get(5)?,
                        output: row.get(6)?,
                        created: row.get(7)?,
                        recipients: Vec::new(),
                    });
                }
                let message = messages.last_mut().ok_or(Error::ImportSource)?;
                budget = budget.saturating_add(
                    recipient.len()
                        + message.id.len()
                        + message.body.len()
                        + message.sender.len()
                        + message.conversation.len()
                        + message.reply.as_ref().map_or(0, String::len)
                        + message.task.as_ref().map_or(0, String::len)
                        + message.output.as_ref().map_or(0, String::len),
                );
                message.recipients.push(recipient);
                if tasks.len() + deliveries > MAX_IMPORT_ROWS || budget > MAX_IMPORT_BYTES {
                    return Err(Error::ImportLimit);
                }
            }
        }
        let task_ids: BTreeSet<_> = tasks.iter().map(|task| task.id.clone()).collect();
        let message_ids: BTreeSet<_> = messages.iter().map(|message| message.id.clone()).collect();
        // Release the source read transaction before any destination retry waits.
        snapshot.commit()?;
        drop(old);
        self.write(request,|tx| {
            if let Some(receipt)=imported(tx,plan)? {return Ok(receipt);}
            let existing:i64=tx.query_row("SELECT (SELECT count(*) FROM messages WHERE project=?1)+(SELECT count(*) FROM tasks WHERE project=?1)",[plan.project.to_string()],|r|r.get(0))?;
            if existing!=0 {return Err(Error::Conflict);}
            for old in &tasks {
                let task=Task {header:RecordHeader::V1,project:plan.project,id:TaskId::from_bytes(id(plan,"fabric-import-task",&old.id)?),objective:old.objective.clone(),note:(old.state=="blocked").then(||"Imported blocked task; blocker requires chair review".into()),state:TaskState::from_old(&old.state)?,owner:None,generation:0,expires_at:None,created_at:old_time(old.created)?,updated_at:plan.cutover,legacy:Some(legacy(plan,&old.id))};
                insert_task(tx,&task)?;
            }
            for old in &messages {
                let task=old.task.as_ref().map(|key| -> Result<TaskRef> { if task_ids.contains(key) {Ok(TaskRef::Local {id:TaskId::from_bytes(id(plan,"fabric-import-task",key)?)})}else{Ok(TaskRef::Legacy {reference:legacy(plan,key)})} }).transpose()?;
                let message=Message {header:RecordHeader::V1,project:plan.project,id:MessageId::from_bytes(id(plan,"fabric-import-message",&old.id)?),sender:actor(plan,&old.sender)?,recipients:old.recipients.iter().map(|label|actor(plan,label)).collect::<Result<_>>()?,kind:MessageKind::Request,body:old.body.clone(),artifacts:Vec::new(),conversation:Some(message_ref(plan,&old.conversation,&message_ids)?),reply_to:old.reply.as_ref().map(|key|message_ref(plan,key,&message_ids)).transpose()?,task,legacy_output_path:old.output.clone(),created_at:old_time(old.created)?,legacy:Some(legacy(plan,&old.id))};
                insert_message(tx,&message)?;
            }
            tx.execute("INSERT INTO mail_cursors SELECT project,recipient,max(seq) FROM deliveries WHERE project=? GROUP BY recipient ON CONFLICT(project,recipient) DO UPDATE SET seq=max(seq,excluded.seq)",[plan.project.to_string()])?;
            let tasks=tasks.len() as u64;let messages=messages.len() as u64;
            append(tx,&event(plan.project,None,EventKind::Imported,None,"imported".into(),plan.cutover),None)?;
            tx.execute("INSERT INTO imports VALUES(?,?,?,?,?,?)",params![plan.source,plan.project.to_string(),plan.legacy_project,plan.cutover.as_micros(),tasks as i64,messages as i64])?;
            Ok(ImportReceipt {tasks,messages,skipped:false})
        })
    }
    fn imported(&self, plan: &ImportPlan, request: &Request<'_>) -> Result<Option<ImportReceipt>> {
        request.retry(|| imported(&self.db, plan))
    }
}
fn imported(db: &Connection, plan: &ImportPlan) -> Result<Option<ImportReceipt>> {
    let row = db
        .query_row(
            "SELECT project,legacy_project,tasks,messages FROM imports WHERE source=?",
            [&plan.source],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    row.map(|(project, legacy, tasks, messages)| {
        if project != plan.project.to_string() || legacy != plan.legacy_project {
            return Err(Error::Conflict);
        }
        Ok(ImportReceipt {
            tasks: u64::try_from(tasks).map_err(|_| Error::ImportSource)?,
            messages: u64::try_from(messages).map_err(|_| Error::ImportSource)?,
            skipped: true,
        })
    })
    .transpose()
}
