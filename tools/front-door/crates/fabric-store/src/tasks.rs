use crate::events::{append, event};
use crate::*;
use provenant_core::{DurationMicros, TaskId};
use provenant_records::RecordHeader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Open,
    Active,
    Done,
    Cancelled,
}
impl TaskState {
    fn sql(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Active => "active",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }
    /// Historical state mapping is data for the one-shot importer, not a live
    /// compatibility API. Only Open and blocked rows are eligible for import.
    pub(crate) fn from_old(state: &str) -> Result<Self> {
        match state {
            "open" | "blocked" => Ok(Self::Open),
            _ => Err(Error::Invalid("old task state")),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub header: RecordHeader,
    pub project: ProjectId,
    pub id: TaskId,
    pub objective: String,
    pub note: Option<String>,
    pub state: TaskState,
    pub owner: Option<ActorId>,
    pub generation: u64,
    pub expires_at: Option<WallTime>,
    pub created_at: WallTime,
    pub updated_at: WallTime,
    pub legacy: Option<LegacyRef>,
}
#[derive(Clone)]
pub struct TaskClaim {
    pub project: ProjectId,
    pub task: TaskId,
    pub actor: ActorId,
    pub token: [u8; 16],
    pub lease: DurationMicros,
}
/// Store-specific task fence. Tokens are supplied by the entropy-owning edge,
/// returned only to the claimant and never included in task lists or events.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFence {
    pub task: TaskId,
    pub actor: ActorId,
    pub generation: u64,
    pub token: [u8; 16],
}
impl std::fmt::Debug for TaskFence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskFence")
            .field("task", &self.task)
            .field("actor", &self.actor)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
pub(crate) fn insert_task(tx: &rusqlite::Connection, task: &Task) -> Result<bool> {
    text(&task.objective, "objective")?;
    if let Some(note) = &task.note {
        text(note, "note")?;
    }
    if task.state != TaskState::Open
        || task.owner.is_some()
        || task.generation != 0
        || task.expires_at.is_some()
    {
        return Err(Error::Invalid("initial task state"));
    }
    let bytes = encode(task)?;
    if let Some(previous) = tx
        .query_row(
            "SELECT substr(record,1,?3) FROM tasks WHERE project=?1 AND id=?2",
            params![
                task.project.to_string(),
                task.id.to_string(),
                MAX_RECORD_READ_BYTES
            ],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .optional()?
    {
        if previous != bytes {
            return Err(Error::Conflict);
        }
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO tasks VALUES(?,?,'open',0,NULL,NULL,NULL,?)",
        params![task.project.to_string(), task.id.to_string(), bytes],
    )?;
    append(
        tx,
        &event(
            task.project,
            None,
            EventKind::TaskCreated,
            Some(task.id.to_string()),
            "created".into(),
            task.created_at,
        ),
        None,
    )?;
    Ok(true)
}
fn load(db: &rusqlite::Connection, project: ProjectId, id: TaskId) -> Result<Task> {
    let bytes: Vec<u8> = db
        .query_row(
            "SELECT substr(record,1,?3) FROM tasks WHERE project=?1 AND id=?2",
            params![project.to_string(), id.to_string(), MAX_RECORD_READ_BYTES],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let task: Task = decode(&bytes)?;
    if task.project != project || task.id != id {
        return Err(Error::Invalid("stored task identity"));
    }
    Ok(task)
}
fn fenced(
    tx: &rusqlite::Connection,
    project: ProjectId,
    fence: &TaskFence,
    at: WallTime,
    require_live: bool,
) -> Result<Task> {
    let task = load(tx, project, fence.task)?;
    let token: Option<Vec<u8>> = tx.query_row(
        "SELECT token FROM tasks WHERE project=? AND id=?",
        params![project.to_string(), fence.task.to_string()],
        |r| r.get(0),
    )?;
    if task.state != TaskState::Active
        || task.owner != Some(fence.actor)
        || task.generation != fence.generation
        || (require_live && task.expires_at.is_none_or(|expiry| expiry <= at))
        || token.as_deref() != Some(fence.token.as_slice())
    {
        return Err(Error::StaleFence);
    }
    Ok(task)
}
fn save(tx: &rusqlite::Connection, task: &Task, token: Option<&[u8]>) -> Result<()> {
    let generation =
        i64::try_from(task.generation).map_err(|_| Error::Invalid("task generation"))?;
    tx.execute("UPDATE tasks SET state=?,generation=?,owner=?,token=?,expires=?,record=? WHERE project=? AND id=?",params![task.state.sql(),generation,task.owner.map(|id|id.to_string()),token,task.expires_at.map(|t|t.as_micros()),encode(task)?,task.project.to_string(),task.id.to_string()])?;
    Ok(())
}
impl Store {
    pub fn create_task(&self, task: &Task, request: &Request<'_>) -> Result<()> {
        self.write(request, |tx| insert_task(tx, task).map(|_| ()))
    }
    pub fn task(&self, project: ProjectId, id: TaskId, request: &Request<'_>) -> Result<Task> {
        request.retry(|| load(&self.db, project, id))
    }
    /// Lists in ID order; resume a truncated page after its last task ID.
    pub fn tasks(
        &self,
        project: ProjectId,
        state: Option<TaskState>,
        after: Option<TaskId>,
        limit: u64,
        request: &Request<'_>,
    ) -> Result<Page<Task>> {
        let limit = page(0, limit)?;
        request.retry(|| {
            let mut statement=self.db.prepare("SELECT substr(record,1,?7) FROM tasks WHERE project=?1 AND (?2 IS NULL OR state=?3) AND (?4 IS NULL OR id>?5) ORDER BY id LIMIT ?6")?;
            let state=state.map(TaskState::sql);let after=after.map(|id|id.to_string());let mut rows=statement.query(params![project.to_string(),state,state,after,after,limit,MAX_RECORD_READ_BYTES])?;
            let mut items=Vec::new();let mut total=0;let mut truncated=false;
            while let Some(row)=rows.next()? {let bytes:Vec<u8>=row.get(0)?;total+=bytes.len();if total>MAX_PAGE_BYTES {truncated=true;break;} items.push(decode(&bytes)?);}
            Ok(Page {items,truncated})
        })
    }
    pub fn claim_task(&self, claim: &TaskClaim, request: &Request<'_>) -> Result<TaskFence> {
        if !(1_000_000..=3_600_000_000).contains(&claim.lease.as_micros()) {
            return Err(Error::Invalid("task lease"));
        }
        self.write(request, |tx| {
            let at = request.clock.wall_now()?;
            let expires_at = at.checked_add(claim.lease)?;
            let mut task = load(tx, claim.project, claim.task)?;
            if !matches!(task.state, TaskState::Open | TaskState::Active)
                || (task.state == TaskState::Active
                    && task.expires_at.is_none_or(|expiry| expiry > at))
            {
                return Err(Error::Conflict);
            }
            task.generation = task
                .generation
                .checked_add(1)
                .ok_or(Error::Invalid("task generation"))?;
            task.state = TaskState::Active;
            task.owner = Some(claim.actor);
            task.expires_at = Some(expires_at);
            task.updated_at = at;
            save(tx, &task, Some(&claim.token))?;
            append(
                tx,
                &event(
                    task.project,
                    task.owner,
                    EventKind::TaskClaimed,
                    Some(task.id.to_string()),
                    task.generation.to_string(),
                    at,
                ),
                None,
            )?;
            Ok(TaskFence {
                task: task.id,
                actor: claim.actor,
                generation: task.generation,
                token: claim.token,
            })
        })
    }
    pub fn verify_task(
        &self,
        project: ProjectId,
        fence: &TaskFence,
        request: &Request<'_>,
    ) -> Result<Task> {
        request.retry(|| {
            // A short read transaction gives one snapshot for record and token.
            let tx = self.db.unchecked_transaction()?;
            let task = fenced(&tx, project, fence, request.clock.wall_now()?, true)?;
            tx.commit()?;
            Ok(task)
        })
    }
    pub fn renew_task(
        &self,
        project: ProjectId,
        fence: &TaskFence,
        lease: DurationMicros,
        request: &Request<'_>,
    ) -> Result<()> {
        self.write(request, |tx| {
            let at = request.clock.wall_now()?;
            let expires = crate::claims::lease_expiry(at, lease)?;
            let mut task = fenced(tx, project, fence, at, true)?;
            task.expires_at = Some(expires);
            task.updated_at = at;
            save(tx, &task, Some(&fence.token))?;
            append(
                tx,
                &event(
                    project,
                    Some(fence.actor),
                    EventKind::TaskRenewed,
                    Some(fence.task.to_string()),
                    "renewed".into(),
                    at,
                ),
                None,
            )?;
            Ok(())
        })
    }
    pub fn update_task(
        &self,
        project: ProjectId,
        fence: &TaskFence,
        objective: &str,
        note: Option<&str>,
        request: &Request<'_>,
    ) -> Result<()> {
        text(objective, "objective")?;
        if let Some(note) = note {
            text(note, "note")?;
        }
        self.write(request, |tx| {
            let at = request.clock.wall_now()?;
            let mut task = fenced(tx, project, fence, at, true)?;
            task.objective = objective.into();
            task.note = note.map(Into::into);
            task.updated_at = at;
            save(tx, &task, Some(&fence.token))?;
            append(
                tx,
                &event(
                    project,
                    task.owner,
                    EventKind::TaskUpdated,
                    Some(task.id.to_string()),
                    "updated".into(),
                    at,
                ),
                None,
            )?;
            Ok(())
        })
    }
    pub fn release_task(
        &self,
        project: ProjectId,
        fence: &TaskFence,
        request: &Request<'_>,
    ) -> Result<()> {
        self.close_task(project, fence, TaskState::Open, request)
    }
    pub fn finish_task(
        &self,
        project: ProjectId,
        fence: &TaskFence,
        state: TaskState,
        request: &Request<'_>,
    ) -> Result<()> {
        if !matches!(state, TaskState::Done | TaskState::Cancelled) {
            return Err(Error::Invalid("finish state"));
        }
        self.close_task(project, fence, state, request)
    }
    fn close_task(
        &self,
        project: ProjectId,
        fence: &TaskFence,
        state: TaskState,
        request: &Request<'_>,
    ) -> Result<()> {
        self.write(request, |tx| {
            let at = request.clock.wall_now()?;
            let mut task = fenced(tx, project, fence, at, false)?;
            task.state = state;
            task.owner = None;
            task.expires_at = None;
            task.updated_at = at;
            save(tx, &task, None)?;
            let kind = if state == TaskState::Open {
                EventKind::TaskReleased
            } else {
                EventKind::TaskFinished
            };
            append(
                tx,
                &event(
                    project,
                    Some(fence.actor),
                    kind,
                    Some(task.id.to_string()),
                    state.sql().into(),
                    at,
                ),
                None,
            )?;
            Ok(())
        })
    }
}
