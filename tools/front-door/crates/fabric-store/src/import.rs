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
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

const MAX_IMPORT_ROWS: usize = 10_000;
const MAX_IMPORT_BYTES: usize = 8_388_608;
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
    pub skipped_rows: BTreeMap<ImportSkipReason, u64>,
}
/// A row contributes once, using its first failing validation reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSkipReason {
    NullValue,
    InvalidValue,
    EmptyText,
    OversizeText,
    Nul,
    UnmappedActor,
    DuplicateRecipient,
    TooManyRecipients,
    RecordSize,
}
type RowResult<T> = std::result::Result<T, ImportSkipReason>;
fn required<T: rusqlite::types::FromSql>(row: &rusqlite::Row<'_>, column: usize) -> RowResult<T> {
    row.get::<_, Option<T>>(column)
        .map_err(|_| ImportSkipReason::InvalidValue)?
        .ok_or(ImportSkipReason::NullValue)
}
fn required_text(row: &rusqlite::Row<'_>, column: usize) -> RowResult<String> {
    match row
        .get_ref(column)
        .map_err(|_| ImportSkipReason::InvalidValue)?
    {
        rusqlite::types::ValueRef::Null => Err(ImportSkipReason::NullValue),
        rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) => {
            std::str::from_utf8(bytes)
                .map(str::to_owned)
                .map_err(|_| ImportSkipReason::InvalidValue)
        }
        _ => Err(ImportSkipReason::InvalidValue),
    }
}
fn optional(row: &rusqlite::Row<'_>, column: usize) -> RowResult<Option<String>> {
    if matches!(
        row.get_ref(column)
            .map_err(|_| ImportSkipReason::InvalidValue)?,
        rusqlite::types::ValueRef::Null
    ) {
        Ok(None)
    } else {
        required_text(row, column).map(Some)
    }
}
fn row_text(value: &str) -> RowResult<()> {
    if value.is_empty() {
        return Err(ImportSkipReason::EmptyText);
    }
    if value.len() > MAX_BODY_BYTES {
        return Err(ImportSkipReason::OversizeText);
    }
    if value.contains('\0') {
        return Err(ImportSkipReason::Nul);
    }
    Ok(())
}
fn count_skip(counts: &mut BTreeMap<ImportSkipReason, u64>, reason: ImportSkipReason) {
    *counts.entry(reason).or_default() += 1;
}
fn charge(row: &rusqlite::Row<'_>, columns: usize, budget: &mut usize) -> Result<()> {
    for column in 0..columns {
        if let rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) =
            row.get_ref(column)?
        {
            *budget = budget.saturating_add(bytes.len());
        }
    }
    if *budget > MAX_IMPORT_BYTES {
        return Err(Error::ImportLimit);
    }
    Ok(())
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
        // SQLite validates its own header through the same connection used for
        // all row reads. Immutable mode is only safe for the supplied, stopped,
        // sidecar-free source; unlike plain READ_ONLY it cannot create WAL/SHM.
        source_ready(&plan.snapshot)?;
        let mut old = Connection::open_with_flags(
            source_uri(&plan.snapshot)?,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|source| Error::ImportReadOnly {
            path: plan.snapshot.clone(),
            source,
        })?;
        old.busy_timeout(std::time::Duration::ZERO)?;
        let snapshot = old.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        let mut budget = 0_usize;
        let mut tasks = Vec::new();
        let mut scanned_tasks = 0;
        let mut skipped_rows = BTreeMap::new();
        {
            let mut statement=snapshot.prepare("SELECT CASE WHEN length(CAST(task_id AS BLOB))<=65536 THEN task_id ELSE substr(CAST(task_id AS BLOB),1,65537) END,CASE WHEN length(CAST(objective AS BLOB))<=65536 THEN objective ELSE substr(CAST(objective AS BLOB),1,65537) END,state,created_at FROM tasks WHERE project=? AND state IN ('open','blocked') AND created_at<=? ORDER BY task_id LIMIT 10001")?;
            let mut rows = statement.query(params![
                plan.legacy_project,
                plan.cutover.as_micros().div_euclid(1000)
            ])?;
            while let Some(row) = rows.next()? {
                scanned_tasks += 1;
                charge(row, 4, &mut budget)?;
                if scanned_tasks > MAX_IMPORT_ROWS {
                    return Err(Error::ImportLimit);
                }
                let parsed = (|| -> RowResult<OldTask> {
                    let task = OldTask {
                        id: required_text(row, 0)?,
                        objective: required_text(row, 1)?,
                        state: required_text(row, 2)?,
                        created: required(row, 3)?,
                    };
                    row_text(&task.id)?;
                    row_text(&task.objective)?;
                    old_time(task.created).map_err(|_| ImportSkipReason::InvalidValue)?;
                    encode(&convert_task(plan, &task).map_err(|_| ImportSkipReason::InvalidValue)?)
                        .map_err(|_| ImportSkipReason::RecordSize)?;
                    Ok(task)
                })();
                match parsed {
                    Ok(task) => tasks.push(task),
                    Err(reason) => count_skip(&mut skipped_rows, reason),
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
        let mut rejected = BTreeSet::new();
        {
            let mut statement=snapshot.prepare("SELECT CASE WHEN length(CAST(m.message_id AS BLOB))<=65536 THEN m.message_id ELSE substr(CAST(m.message_id AS BLOB),1,65537) END,CASE WHEN length(CAST(m.sender_id AS BLOB))<=65536 THEN m.sender_id ELSE substr(CAST(m.sender_id AS BLOB),1,65537) END,CASE WHEN length(CAST(m.body AS BLOB))<=65536 THEN m.body ELSE substr(CAST(m.body AS BLOB),1,65537) END,CASE WHEN length(CAST(m.conversation_id AS BLOB))<=65536 THEN m.conversation_id ELSE substr(CAST(m.conversation_id AS BLOB),1,65537) END,CASE WHEN length(CAST(m.reply_to AS BLOB))<=65536 THEN m.reply_to ELSE substr(CAST(m.reply_to AS BLOB),1,65537) END,CASE WHEN length(CAST(m.task_id AS BLOB))<=65536 THEN m.task_id ELSE substr(CAST(m.task_id AS BLOB),1,65537) END,CASE WHEN length(CAST(m.output_path AS BLOB))<=65536 THEN m.output_path ELSE substr(CAST(m.output_path AS BLOB),1,65537) END,m.created_at,CASE WHEN length(CAST(d.recipient_id AS BLOB))<=65536 THEN d.recipient_id ELSE substr(CAST(d.recipient_id AS BLOB),1,65537) END FROM messages m JOIN deliveries d ON d.message_id=m.message_id AND d.project=m.project WHERE m.project=? AND m.kind='request' AND m.created_at>=? AND m.created_at<=? AND d.read_at IS NULL ORDER BY m.message_id,d.recipient_id LIMIT 10001")?;
            let mut rows = statement.query(params![
                plan.legacy_project,
                cutoff_ms,
                plan.cutover.as_micros().div_euclid(1000)
            ])?;
            while let Some(row) = rows.next()? {
                deliveries += 1;
                charge(row, 9, &mut budget)?;
                if scanned_tasks + deliveries > MAX_IMPORT_ROWS {
                    return Err(Error::ImportLimit);
                }
                let key: String = match required_text(row, 0) {
                    Ok(key) => key,
                    Err(reason) => {
                        count_skip(&mut skipped_rows, reason);
                        continue;
                    }
                };
                if rejected.contains(&key) {
                    continue;
                }
                if let Err(reason) = row_text(&key) {
                    count_skip(&mut skipped_rows, reason);
                    continue;
                }
                if messages.last().is_none_or(|message| message.id != key) {
                    let parsed = (|| -> RowResult<OldMessage> {
                        let message = OldMessage {
                            id: key.clone(),
                            sender: required_text(row, 1)?,
                            body: required_text(row, 2)?,
                            conversation: required_text(row, 3)?,
                            reply: optional(row, 4)?,
                            task: optional(row, 5)?,
                            output: optional(row, 6)?,
                            created: required(row, 7)?,
                            recipients: Vec::new(),
                        };
                        for field in [&message.sender, &message.body, &message.conversation]
                            .into_iter()
                            .chain(message.reply.iter())
                            .chain(message.task.iter())
                            .chain(message.output.iter())
                        {
                            row_text(field)?;
                        }
                        actor(plan, &message.sender)
                            .map_err(|_| ImportSkipReason::UnmappedActor)?;
                        old_time(message.created).map_err(|_| ImportSkipReason::InvalidValue)?;
                        let record =
                            convert_message(plan, &message, &BTreeSet::new(), &BTreeSet::new())
                                .map_err(|_| ImportSkipReason::InvalidValue)?;
                        encode(&record).map_err(|_| ImportSkipReason::RecordSize)?;
                        Ok(message)
                    })();
                    match parsed {
                        Ok(message) => messages.push(message),
                        Err(reason) => {
                            rejected.insert(key);
                            count_skip(&mut skipped_rows, reason);
                            continue;
                        }
                    }
                }
                // A bad delivery does not discard valid siblings for the same
                // message. Validate before appending, then undo only this row
                // if adding its recipient would exceed the record bound.
                let message = messages.last_mut().ok_or(Error::ImportSource)?;
                let recipient = (|| -> RowResult<String> {
                    let recipient = required_text(row, 8)?;
                    row_text(&recipient)?;
                    let mapped =
                        actor(plan, &recipient).map_err(|_| ImportSkipReason::UnmappedActor)?;
                    if message
                        .recipients
                        .iter()
                        .any(|label| plan.actors.get(label) == Some(&mapped))
                    {
                        return Err(ImportSkipReason::DuplicateRecipient);
                    }
                    if message.recipients.len() == 100 {
                        return Err(ImportSkipReason::TooManyRecipients);
                    }
                    Ok(recipient)
                })();
                match recipient {
                    Err(reason) => count_skip(&mut skipped_rows, reason),
                    Ok(recipient) => {
                        message.recipients.push(recipient);
                        let record =
                            convert_message(plan, message, &BTreeSet::new(), &BTreeSet::new())?;
                        if encode(&record).is_err() {
                            message.recipients.pop();
                            count_skip(&mut skipped_rows, ImportSkipReason::RecordSize);
                        }
                    }
                }
            }
        }
        messages.retain(|message| !message.recipients.is_empty());
        let task_ids: BTreeSet<_> = tasks.iter().map(|task| task.id.clone()).collect();
        let message_ids: BTreeSet<_> = messages.iter().map(|message| message.id.clone()).collect();
        // Release the source read transaction before any destination retry waits.
        snapshot.commit()?;
        drop(old);
        source_ready(&plan.snapshot)?;
        self.write(request,|tx| {
            if let Some(receipt)=imported(tx,plan)? {return Ok(receipt);}
            let existing:i64=tx.query_row("SELECT (SELECT count(*) FROM messages WHERE project=?1)+(SELECT count(*) FROM tasks WHERE project=?1)",[plan.project.to_string()],|r|r.get(0))?;
            if existing!=0 {return Err(Error::Conflict);}
            for old in &tasks {
                let task=convert_task(plan,old)?;
                insert_task(tx,&task)?;
            }
            for old in &messages {
                let message=convert_message(plan,old,&task_ids,&message_ids)?;
                insert_message(tx,&message)?;
            }
            tx.execute("INSERT INTO mail_cursors SELECT project,recipient,max(seq) FROM deliveries WHERE project=? GROUP BY recipient ON CONFLICT(project,recipient) DO UPDATE SET seq=max(seq,excluded.seq)",[plan.project.to_string()])?;
            let tasks=tasks.len() as u64;let messages=messages.len() as u64;
            append(tx,&event(plan.project,None,EventKind::Imported,None,"imported".into(),plan.cutover),None)?;
            tx.execute("INSERT INTO imports VALUES(?,?,?,?,?,?,?)",params![plan.source,plan.project.to_string(),plan.legacy_project,plan.cutover.as_micros(),tasks as i64,messages as i64,encode(&skipped_rows)?])?;
            Ok(ImportReceipt {tasks,messages,skipped:false,skipped_rows:skipped_rows.clone()})
        })
    }
    fn imported(&self, plan: &ImportPlan, request: &Request<'_>) -> Result<Option<ImportReceipt>> {
        request.retry(|| imported(&self.db, plan))
    }
}
fn imported(db: &Connection, plan: &ImportPlan) -> Result<Option<ImportReceipt>> {
    let row = db
        .query_row(
            "SELECT project,legacy_project,tasks,messages,skipped_rows FROM imports WHERE source=?",
            [&plan.source],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()?;
    row.map(|(project, legacy, tasks, messages, skipped_rows)| {
        if project != plan.project.to_string() || legacy != plan.legacy_project {
            return Err(Error::Conflict);
        }
        Ok(ImportReceipt {
            tasks: u64::try_from(tasks).map_err(|_| Error::ImportSource)?,
            messages: u64::try_from(messages).map_err(|_| Error::ImportSource)?,
            skipped: true,
            skipped_rows: decode(&skipped_rows)?,
        })
    })
    .transpose()
}

fn source_ready(path: &std::path::Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| Error::Io {
        path: path.into(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(Error::UnsafePath(path.into()));
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        match std::fs::symlink_metadata(PathBuf::from(sidecar)) {
            Ok(_) => return Err(Error::ImportNotQuiescent),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Io {
                    path: path.into(),
                    source,
                });
            }
        }
    }
    Ok(())
}
fn source_uri(path: &std::path::Path) -> Result<String> {
    let absolute = std::path::absolute(path).map_err(|source| Error::Io {
        path: path.into(),
        source,
    })?;
    let mut uri = String::from("file:");
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in absolute.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"/._-~".contains(byte) {
            uri.push(char::from(*byte));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[usize::from(byte >> 4)]));
            uri.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    Ok(uri)
}

fn convert_task(plan: &ImportPlan, old: &OldTask) -> Result<Task> {
    Ok(Task {
        header: RecordHeader::V1,
        project: plan.project,
        id: TaskId::from_bytes(id(plan, "fabric-import-task", &old.id)?),
        objective: old.objective.clone(),
        note: (old.state == "blocked")
            .then(|| "Imported blocked task; blocker requires chair review".into()),
        state: TaskState::from_old(&old.state)?,
        owner: None,
        generation: 0,
        expires_at: None,
        created_at: old_time(old.created)?,
        updated_at: plan.cutover,
        legacy: Some(legacy(plan, &old.id)),
    })
}
fn convert_message(
    plan: &ImportPlan,
    old: &OldMessage,
    task_ids: &BTreeSet<String>,
    message_ids: &BTreeSet<String>,
) -> Result<Message> {
    let task = old
        .task
        .as_ref()
        .map(|key| -> Result<TaskRef> {
            if task_ids.contains(key) {
                Ok(TaskRef::Local {
                    id: TaskId::from_bytes(id(plan, "fabric-import-task", key)?),
                })
            } else {
                Ok(TaskRef::Legacy {
                    reference: legacy(plan, key),
                })
            }
        })
        .transpose()?;
    Ok(Message {
        header: RecordHeader::V1,
        project: plan.project,
        id: MessageId::from_bytes(id(plan, "fabric-import-message", &old.id)?),
        sender: actor(plan, &old.sender)?,
        recipients: old
            .recipients
            .iter()
            .map(|label| actor(plan, label))
            .collect::<Result<_>>()?,
        kind: MessageKind::Request,
        body: old.body.clone(),
        artifacts: Vec::new(),
        conversation: Some(message_ref(plan, &old.conversation, message_ids)?),
        reply_to: old
            .reply
            .as_ref()
            .map(|key| message_ref(plan, key, message_ids))
            .transpose()?,
        task,
        legacy_output_path: old.output.clone(),
        created_at: old_time(old.created)?,
        legacy: Some(legacy(plan, &old.id)),
    })
}
