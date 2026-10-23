//! Authenticated, bounded coordination over the Fabric store.
#![forbid(unsafe_code)]

pub mod facade;

use provenant_core::{
    ActorId, CancelToken, ClaimId, Clock, DurationMicros, GitOid, MessageId, ProjectId, StateRoot,
    TaskId, WaitBudget, WaitTransport,
};
use provenant_records::{ArtifactRef, RecordHeader};
use provenant_supervisor::fs::{FsError, LocalFs, SafeFs};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use thiserror::Error;

use provenant_fabric_store::{ClaimFence, ClaimRequest, LandingRequest, Request, Store, TaskClaim};
pub use provenant_fabric_store::{
    Delivery, Event, EventKind, EventRecord, Landing, Message, MessageKind, MessageRef, Page,
    RetryWait, Task, TaskRef, TaskState, ThreadWait, WorkClaim, WorkScope,
};

#[derive(Debug, Error)]
pub enum CoordinationError {
    #[error("invalid coordination input: {0}")]
    InvalidInput(&'static str),
    #[error("coordination request was cancelled")]
    Cancelled,
    #[error("coordination request exceeded its wait budget")]
    Timeout,
    #[error("coordination store remained busy until its wait budget expired")]
    Busy,
    #[error("safe entropy generation failed")]
    Entropy(#[source] FsError),
    #[error("safe entropy returned an invalid encoding")]
    EntropyEncoding,
    #[error("coordination response could not be encoded")]
    ResponseEncoding(#[source] serde_json::Error),
    #[error("one coordination result exceeds its response byte budget")]
    ResponseTooLarge,
    #[error("Fabric store operation failed")]
    Store(#[source] provenant_fabric_store::Error),
    #[error("clock operation failed")]
    Time(#[from] provenant_core::TimeError),
}

impl CoordinationError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) | Self::Time(_) => "invalid_input",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Busy => "busy",
            Self::Entropy(_) | Self::EntropyEncoding => "io",
            Self::ResponseEncoding(_) | Self::ResponseTooLarge => "state_corrupt",
            Self::Store(error) => match error {
                provenant_fabric_store::Error::NotFound => "not_found",
                provenant_fabric_store::Error::Conflict
                | provenant_fabric_store::Error::Overlap { .. }
                | provenant_fabric_store::Error::PushUnresolved { .. }
                | provenant_fabric_store::Error::PushTransition { .. } => "conflict",
                provenant_fabric_store::Error::StaleFence => "stale_fence",
                provenant_fabric_store::Error::Busy => "busy",
                provenant_fabric_store::Error::Cancelled => "cancelled",
                provenant_fabric_store::Error::Invalid(_) => "invalid_input",
                provenant_fabric_store::Error::Decode(_)
                | provenant_fabric_store::Error::SchemaVersion(_)
                | provenant_fabric_store::Error::UnversionedDatabase
                | provenant_fabric_store::Error::Encode(_) => "state_corrupt",
                _ => "io",
            },
        }
    }
}

impl From<provenant_fabric_store::Error> for CoordinationError {
    fn from(error: provenant_fabric_store::Error) -> Self {
        match error {
            provenant_fabric_store::Error::Busy => Self::Busy,
            provenant_fabric_store::Error::Cancelled => Self::Cancelled,
            other => Self::Store(other),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProof {
    pub task: TaskId,
    pub generation: u64,
    pub token: [u8; 16],
}
impl std::fmt::Debug for TaskProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskProof")
            .field("task", &self.task)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimProof {
    pub id: ClaimId,
    pub generation: u64,
    pub token: [u8; 16],
}
impl std::fmt::Debug for ClaimProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimProof")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandingProof {
    pub reference: String,
    pub id: ClaimId,
    pub generation: u64,
    pub token: [u8; 16],
}
impl std::fmt::Debug for LandingProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LandingProof")
            .field("reference", &self.reference)
            .field("id", &self.id)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum MailInput {
    Send {
        recipients: Vec<ActorId>,
        kind: MessageKind,
        body: String,
        artifacts: Vec<ArtifactRef>,
        reply_to: Option<MessageRef>,
        task: Option<TaskRef>,
    },
    Read {
        after: Option<i64>,
        limit: u64,
    },
    MarkRead {
        through: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum MailResult {
    Sent { id: MessageId },
    Read { page: Page<Delivery>, cursor: i64 },
    MarkedRead { cursor: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskInput {
    Create {
        objective: String,
        note: Option<String>,
    },
    List {
        state: Option<TaskState>,
        after: Option<TaskId>,
        limit: u64,
    },
    Claim {
        id: TaskId,
        lease_ms: u64,
    },
    Update {
        proof: TaskProof,
        objective: String,
        note: Option<String>,
    },
    Finish {
        proof: TaskProof,
        state: TaskState,
    },
    Release {
        proof: TaskProof,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskResult {
    Created { task: Task },
    Listed { page: Page<Task> },
    Claimed { proof: TaskProof },
    Updated { task: Task },
    Finished { task: Task },
    Released { task: Task },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventsInput {
    Read {
        after: Option<i64>,
        limit: u64,
        wait_ms: Option<u64>,
    },
    Note {
        note: String,
        dedup_key: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventsResult {
    Read {
        page: Page<Event>,
        cursor: i64,
        timed_out: bool,
    },
    Noted {
        seq: i64,
    },
}

const MAX_COORDINATION_RESULT_BYTES: usize = 768 * 1024;

fn bounded_page<T: Serialize>(
    items: Vec<T>,
    mut truncated: bool,
) -> Result<Page<T>, CoordinationError> {
    let mut bounded = Vec::with_capacity(items.len());
    let mut encoded_bytes = 0_usize;
    for item in items {
        let item_bytes = serde_json::to_vec(&item).map_err(CoordinationError::ResponseEncoding)?;
        let Some(next_bytes) = encoded_bytes.checked_add(item_bytes.len()) else {
            return Err(CoordinationError::ResponseTooLarge);
        };
        if next_bytes > MAX_COORDINATION_RESULT_BYTES {
            truncated = true;
            break;
        }
        encoded_bytes = next_bytes;
        bounded.push(item);
    }
    Ok(Page {
        items: bounded,
        truncated,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClaimInput {
    Acquire { scope: WorkScope, lease_ms: u64 },
    Renew { proof: ClaimProof, lease_ms: u64 },
    Verify { proof: ClaimProof },
    Release { proof: ClaimProof },
    List { after: Option<ClaimId>, limit: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClaimResult {
    Acquired { proof: ClaimProof, claim: WorkClaim },
    Renewed { claim: WorkClaim },
    Verified { claim: WorkClaim },
    Released { id: ClaimId },
    Listed { page: Page<WorkClaim> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum LandingInput {
    Acquire {
        reference: String,
        expected: GitOid,
        candidate: GitOid,
        lease_ms: u64,
    },
    Renew {
        proof: LandingProof,
        lease_ms: u64,
    },
    Verify {
        proof: LandingProof,
    },
    Release {
        proof: LandingProof,
    },
    Wait {
        reference: String,
        generation: u64,
        wait_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum LandingResult {
    Acquired {
        proof: LandingProof,
        landing: Landing,
    },
    Renewed {
        landing: Landing,
    },
    Verified {
        landing: Landing,
    },
    Released {
        id: ClaimId,
        reference: String,
    },
    Waited {
        landing: Option<Landing>,
    },
}

/// Narrow seam consumed by MCP and implemented by the runtime composition.
/// The service is worker-local because each `Store` owns one SQLite connection.
pub trait CoordinationApi {
    fn mail(
        &self,
        input: MailInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<MailResult, CoordinationError>;
    fn task(
        &self,
        input: TaskInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<TaskResult, CoordinationError>;
    fn events(
        &self,
        input: EventsInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<EventsResult, CoordinationError>;
    fn claim(
        &self,
        input: ClaimInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<ClaimResult, CoordinationError>;
    fn landing(
        &self,
        input: LandingInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<LandingResult, CoordinationError>;
}

pub struct CoordinationService<'a> {
    store: Store,
    project: ProjectId,
    actor: ActorId,
    clock: &'a dyn Clock,
    backoff: &'a dyn RetryWait,
}

impl<'a> CoordinationService<'a> {
    pub fn open(
        root: &StateRoot,
        project: ProjectId,
        actor: ActorId,
        clock: &'a dyn Clock,
        backoff: &'a dyn RetryWait,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Self, CoordinationError> {
        let request = Request {
            clock,
            budget,
            cancel,
            backoff,
        };
        let store = Store::open(root, &request)?;
        Ok(Self::new(store, project, actor, clock, backoff))
    }

    pub fn new(
        store: Store,
        project: ProjectId,
        actor: ActorId,
        clock: &'a dyn Clock,
        backoff: &'a dyn RetryWait,
    ) -> Self {
        Self {
            store,
            project,
            actor,
            clock,
            backoff,
        }
    }

    fn request<'b>(&'b self, budget: WaitBudget, cancel: &'b CancelToken) -> Request<'b> {
        Request {
            clock: self.clock,
            budget,
            cancel,
            backoff: self.backoff,
        }
    }

    fn ensure_active(
        &self,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<(), CoordinationError> {
        if cancel.is_cancelled() {
            return Err(CoordinationError::Cancelled);
        }
        if budget.remaining(self.clock.mono_now()).as_micros() == 0 {
            return Err(CoordinationError::Timeout);
        }
        Ok(())
    }

    fn lease(milliseconds: u64) -> Result<DurationMicros, CoordinationError> {
        let micros = milliseconds
            .checked_mul(1_000)
            .ok_or(CoordinationError::InvalidInput("lease_ms"))?;
        if !(1_000_000..=3_600_000_000).contains(&micros) {
            return Err(CoordinationError::InvalidInput("lease_ms"));
        }
        Ok(DurationMicros::from_micros(micros))
    }

    fn page_limit(limit: u64) -> Result<(), CoordinationError> {
        if !(1..=provenant_fabric_store::MAX_PAGE_ROWS).contains(&limit) {
            return Err(CoordinationError::InvalidInput("limit"));
        }
        Ok(())
    }

    fn wait_budget(&self, duration_micros: u64) -> Result<WaitBudget, CoordinationError> {
        Ok(WaitBudget::new(
            self.clock.mono_now(),
            DurationMicros::from_micros(duration_micros),
            WaitTransport::Mcp,
        )?)
    }

    fn new_id_bytes() -> Result<[u8; 16], CoordinationError> {
        let first = entropy_half()?;
        let second = entropy_half()?;
        let mut bytes = [0_u8; 16];
        bytes[..8].copy_from_slice(&first);
        bytes[8..].copy_from_slice(&second);
        Ok(bytes)
    }

    fn task_fence(&self, proof: &TaskProof) -> provenant_fabric_store::TaskFence {
        provenant_fabric_store::TaskFence {
            task: proof.task,
            actor: self.actor,
            generation: proof.generation,
            token: proof.token,
        }
    }

    fn claim_fence(&self, proof: &ClaimProof) -> ClaimFence {
        ClaimFence {
            id: proof.id,
            actor: self.actor,
            generation: proof.generation,
            token: proof.token,
        }
    }

    fn landing_fence(&self, proof: &LandingProof) -> ClaimFence {
        ClaimFence {
            id: proof.id,
            actor: self.actor,
            generation: proof.generation,
            token: proof.token,
        }
    }

    fn claim_proof(&self, fence: ClaimFence) -> ClaimProof {
        ClaimProof {
            id: fence.id,
            generation: fence.generation,
            token: fence.token,
        }
    }

    fn landing_proof(&self, reference: String, fence: ClaimFence) -> LandingProof {
        LandingProof {
            reference,
            id: fence.id,
            generation: fence.generation,
            token: fence.token,
        }
    }
}

impl CoordinationApi for CoordinationService<'_> {
    fn mail(
        &self,
        input: MailInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<MailResult, CoordinationError> {
        self.ensure_active(budget, cancel)?;
        let request = self.request(budget, cancel);
        match input {
            MailInput::Send {
                recipients,
                kind,
                body,
                artifacts,
                reply_to,
                task,
            } => {
                let id = MessageId::from_bytes(Self::new_id_bytes()?);
                let message = Message {
                    header: RecordHeader::V1,
                    project: self.project,
                    id,
                    sender: self.actor,
                    recipients,
                    conversation: None,
                    legacy_output_path: None,
                    kind,
                    body,
                    artifacts,
                    reply_to,
                    task,
                    created_at: self.clock.wall_now()?,
                    legacy: None,
                };
                self.store.send(&message, &request)?;
                Ok(MailResult::Sent { id })
            }
            MailInput::Read { after, limit } => {
                Self::page_limit(limit)?;
                let prior_cursor = self.store.mail_cursor(self.project, self.actor, &request)?;
                let page =
                    self.store
                        .read_mail(self.project, self.actor, after, limit, &request)?;
                let page = bounded_page(page.items, page.truncated)?;
                let cursor = page
                    .items
                    .last()
                    .map(|delivery| delivery.seq)
                    .unwrap_or_else(|| after.unwrap_or(prior_cursor));
                Ok(MailResult::Read { page, cursor })
            }
            MailInput::MarkRead { through } => {
                self.store
                    .mark_mail_read(self.project, self.actor, through, &request)?;
                let cursor = self.store.mail_cursor(self.project, self.actor, &request)?;
                Ok(MailResult::MarkedRead { cursor })
            }
        }
    }

    fn task(
        &self,
        input: TaskInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<TaskResult, CoordinationError> {
        self.ensure_active(budget, cancel)?;
        let request = self.request(budget, cancel);
        match input {
            TaskInput::Create { objective, note } => {
                let task = Task {
                    header: RecordHeader::V1,
                    project: self.project,
                    id: TaskId::from_bytes(Self::new_id_bytes()?),
                    objective,
                    note,
                    state: TaskState::Open,
                    owner: None,
                    generation: 0,
                    expires_at: None,
                    created_at: self.clock.wall_now()?,
                    updated_at: self.clock.wall_now()?,
                    legacy: None,
                };
                self.store.create_task(&task, &request)?;
                Ok(TaskResult::Created { task })
            }
            TaskInput::List {
                state,
                after,
                limit,
            } => {
                Self::page_limit(limit)?;
                let tasks = self
                    .store
                    .tasks(self.project, state, after, limit, &request)?;
                Ok(TaskResult::Listed {
                    page: bounded_page(tasks, false)?,
                })
            }
            TaskInput::Claim { id, lease_ms } => {
                let fence = self.store.claim_task(
                    &TaskClaim {
                        project: self.project,
                        task: id,
                        actor: self.actor,
                        token: Self::new_id_bytes()?,
                        lease: Self::lease(lease_ms)?,
                    },
                    &request,
                )?;
                Ok(TaskResult::Claimed {
                    proof: TaskProof {
                        task: fence.task,
                        generation: fence.generation,
                        token: fence.token,
                    },
                })
            }
            TaskInput::Update {
                proof,
                objective,
                note,
            } => {
                let fence = self.task_fence(&proof);
                self.store.update_task(
                    self.project,
                    &fence,
                    &objective,
                    note.as_deref(),
                    &request,
                )?;
                let task = self.store.task(self.project, proof.task, &request)?;
                Ok(TaskResult::Updated { task })
            }
            TaskInput::Finish { proof, state } => {
                let fence = self.task_fence(&proof);
                self.store
                    .finish_task(self.project, &fence, state, &request)?;
                let task = self.store.task(self.project, proof.task, &request)?;
                Ok(TaskResult::Finished { task })
            }
            TaskInput::Release { proof } => {
                let fence = self.task_fence(&proof);
                self.store.release_task(self.project, &fence, &request)?;
                let task = self.store.task(self.project, proof.task, &request)?;
                Ok(TaskResult::Released { task })
            }
        }
    }

    fn events(
        &self,
        input: EventsInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<EventsResult, CoordinationError> {
        self.ensure_active(budget, cancel)?;
        let request = self.request(budget, cancel);
        match input {
            EventsInput::Read {
                after,
                limit,
                wait_ms,
            } => {
                Self::page_limit(limit)?;
                if wait_ms.is_some_and(|millis| !(1_000..=55_000).contains(&millis)) {
                    return Err(CoordinationError::InvalidInput("wait_ms"));
                }
                // Long-poll reads must be bounded from their first store call,
                // not only from the later retry loop. Otherwise a busy store
                // can consume the full tool budget before `wait_ms` applies.
                let (read_request, wait_cap) = if let Some(wait_ms) = wait_ms {
                    let input_us = wait_ms
                        .checked_mul(1_000)
                        .ok_or(CoordinationError::InvalidInput("wait_ms"))?;
                    let remaining = budget.remaining(self.clock.mono_now()).as_micros();
                    let cap_us = input_us.min(remaining);
                    if cap_us < 1_000_000 {
                        return Err(CoordinationError::Timeout);
                    }
                    (
                        self.request(self.wait_budget(cap_us)?, cancel),
                        Some(cap_us),
                    )
                } else {
                    (request, None)
                };
                let started = Instant::now();
                let prior_cursor =
                    self.store
                        .event_cursor(self.project, self.actor, &read_request)?;
                let start = after.unwrap_or(prior_cursor);
                let mut page = self
                    .store
                    .events(self.project, start, limit, &read_request)?;
                let mut timed_out = false;
                if page.items.is_empty() && wait_cap.is_some() {
                    let cap_us = wait_cap.ok_or(CoordinationError::InvalidInput("wait_ms"))?;
                    let cap = Duration::from_micros(cap_us);
                    loop {
                        if cancel.is_cancelled() {
                            return Err(CoordinationError::Cancelled);
                        }
                        let left = cap.saturating_sub(started.elapsed());
                        if left.is_zero() {
                            timed_out = true;
                            break;
                        }
                        self.backoff.wait(
                            DurationMicros::from_micros(
                                u64::try_from(left.min(Duration::from_millis(50)).as_micros())
                                    .unwrap_or(50_000),
                            ),
                            cancel,
                        );
                        page = self
                            .store
                            .events(self.project, start, limit, &read_request)?;
                        if !page.items.is_empty() {
                            break;
                        }
                    }
                }
                let page = bounded_page(page.items, page.truncated)?;
                let cursor = page
                    .items
                    .last()
                    .map(|event| event.seq)
                    .unwrap_or_else(|| after.unwrap_or(prior_cursor));
                if after.is_none() && cursor > prior_cursor {
                    self.store
                        .mark_events_read(self.project, self.actor, cursor, &read_request)?;
                }
                Ok(EventsResult::Read {
                    page,
                    cursor,
                    timed_out,
                })
            }
            EventsInput::Note { note, dedup_key } => {
                let record = EventRecord {
                    header: RecordHeader::V1,
                    project: self.project,
                    actor: Some(self.actor),
                    kind: EventKind::Note,
                    entity: None,
                    detail: note,
                    at: self.clock.wall_now()?,
                };
                let seq = self.store.note(&record, dedup_key.as_deref(), &request)?;
                Ok(EventsResult::Noted { seq })
            }
        }
    }

    fn claim(
        &self,
        input: ClaimInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<ClaimResult, CoordinationError> {
        self.ensure_active(budget, cancel)?;
        let request = self.request(budget, cancel);
        match input {
            ClaimInput::Acquire { scope, lease_ms } => {
                let fence = self.store.acquire_claim(
                    &ClaimRequest {
                        id: ClaimId::from_bytes(Self::new_id_bytes()?),
                        project: self.project,
                        actor: self.actor,
                        token: Self::new_id_bytes()?,
                        lease: Self::lease(lease_ms)?,
                        scope,
                    },
                    &request,
                )?;
                let claim = self.store.verify_claim(self.project, &fence, &request)?;
                Ok(ClaimResult::Acquired {
                    proof: self.claim_proof(fence),
                    claim,
                })
            }
            ClaimInput::Renew { proof, lease_ms } => {
                let fence = self.claim_fence(&proof);
                self.store
                    .renew_claim(self.project, &fence, Self::lease(lease_ms)?, &request)?;
                let claim = self.store.verify_claim(self.project, &fence, &request)?;
                Ok(ClaimResult::Renewed { claim })
            }
            ClaimInput::Verify { proof } => {
                let claim =
                    self.store
                        .verify_claim(self.project, &self.claim_fence(&proof), &request)?;
                Ok(ClaimResult::Verified { claim })
            }
            ClaimInput::Release { proof } => {
                let id = proof.id;
                self.store
                    .release_claim(self.project, &self.claim_fence(&proof), &request)?;
                Ok(ClaimResult::Released { id })
            }
            ClaimInput::List { after, limit } => {
                Self::page_limit(limit)?;
                let page = self.store.claims(self.project, after, limit, &request)?;
                let page = bounded_page(page.items, page.truncated)?;
                Ok(ClaimResult::Listed { page })
            }
        }
    }

    fn landing(
        &self,
        input: LandingInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<LandingResult, CoordinationError> {
        self.ensure_active(budget, cancel)?;
        let request = self.request(budget, cancel);
        match input {
            LandingInput::Acquire {
                reference,
                expected,
                candidate,
                lease_ms,
            } => {
                let guard = self
                    .store
                    .lock_landing(self.project, &reference, &request)?;
                let fence = self.store.acquire_landing(
                    &guard,
                    &LandingRequest {
                        id: ClaimId::from_bytes(Self::new_id_bytes()?),
                        project: self.project,
                        actor: self.actor,
                        token: Self::new_id_bytes()?,
                        reference: reference.clone(),
                        expected,
                        candidate,
                        lease: Self::lease(lease_ms)?,
                    },
                    &request,
                )?;
                let landing = self.store.verify_landing(&guard, &fence, &request)?;
                Ok(LandingResult::Acquired {
                    proof: self.landing_proof(reference, fence),
                    landing,
                })
            }
            LandingInput::Renew { proof, lease_ms } => {
                let guard = self
                    .store
                    .lock_landing(self.project, &proof.reference, &request)?;
                let fence = self.landing_fence(&proof);
                self.store
                    .renew_landing(&guard, &fence, Self::lease(lease_ms)?, &request)?;
                let landing = self.store.verify_landing(&guard, &fence, &request)?;
                Ok(LandingResult::Renewed { landing })
            }
            LandingInput::Verify { proof } => {
                let guard = self
                    .store
                    .lock_landing(self.project, &proof.reference, &request)?;
                let landing =
                    self.store
                        .verify_landing(&guard, &self.landing_fence(&proof), &request)?;
                Ok(LandingResult::Verified { landing })
            }
            LandingInput::Release { proof } => {
                let guard = self
                    .store
                    .lock_landing(self.project, &proof.reference, &request)?;
                let id = proof.id;
                self.store
                    .release_landing(&guard, &self.landing_fence(&proof), &request)?;
                Ok(LandingResult::Released {
                    id,
                    reference: proof.reference,
                })
            }
            LandingInput::Wait {
                reference,
                generation,
                wait_ms,
            } => {
                if !(1_000..=55_000).contains(&wait_ms) {
                    return Err(CoordinationError::InvalidInput("wait_ms"));
                }
                let input_us = wait_ms
                    .checked_mul(1_000)
                    .ok_or(CoordinationError::InvalidInput("wait_ms"))?;
                let remaining = budget.remaining(self.clock.mono_now()).as_micros();
                let cap_us = input_us.min(remaining);
                if cap_us < 1_000_000 {
                    return Err(CoordinationError::Timeout);
                }
                let poll_request = self.request(self.wait_budget(cap_us)?, cancel);
                let cap = Duration::from_micros(cap_us);
                let started = Instant::now();
                loop {
                    if cancel.is_cancelled() {
                        return Err(CoordinationError::Cancelled);
                    }
                    if started.elapsed() >= cap {
                        return Err(CoordinationError::Timeout);
                    }
                    let current = self
                        .store
                        .landing(self.project, &reference, &poll_request)?;
                    let changed = current.as_ref().is_none_or(|landing| {
                        landing.generation != generation || landing.holder.is_none()
                    });
                    if changed {
                        return Ok(LandingResult::Waited { landing: current });
                    }
                    let left = cap.saturating_sub(started.elapsed());
                    self.backoff.wait(
                        DurationMicros::from_micros(
                            u64::try_from(left.min(Duration::from_millis(50)).as_micros())
                                .unwrap_or(50_000),
                        ),
                        cancel,
                    );
                }
            }
        }
    }
}

fn entropy_half() -> Result<[u8; 8], CoordinationError> {
    let name = LocalFs
        .temporary_name()
        .map_err(CoordinationError::Entropy)?;
    let name = name
        .into_string()
        .map_err(|_| CoordinationError::EntropyEncoding)?;
    let encoded = name
        .strip_prefix(".provenant-staging.")
        .ok_or(CoordinationError::EntropyEncoding)?;
    let bytes = encoded.as_bytes();
    if bytes.len() != 24 {
        return Err(CoordinationError::EntropyEncoding);
    }
    let mut output = [0_u8; 8];
    for (index, byte) in output.iter_mut().enumerate() {
        let start = index
            .checked_mul(2)
            .ok_or(CoordinationError::EntropyEncoding)?;
        let end = start
            .checked_add(2)
            .ok_or(CoordinationError::EntropyEncoding)?;
        let pair = bytes
            .get(start..end)
            .ok_or(CoordinationError::EntropyEncoding)?;
        let pair = std::str::from_utf8(pair).map_err(|_| CoordinationError::EntropyEncoding)?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| CoordinationError::EntropyEncoding)?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{MonoTime, PathResolver, RootInputs, StateRoot, TestClock, WallTime};
    use provenant_fabric_store::ThreadWait;
    use std::path::{Path, PathBuf};

    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
            if path.exists() {
                path.canonicalize()
            } else {
                Ok(self
                    .resolve_existing_ancestor(
                        path.parent().ok_or(std::io::ErrorKind::InvalidInput)?,
                    )?
                    .join(path.file_name().ok_or(std::io::ErrorKind::InvalidInput)?))
            }
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        root: StateRoot,
        clock: TestClock,
        backoff: ThreadWait,
        project: ProjectId,
        actor: ActorId,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir_in(std::fs::canonicalize("/tmp").expect("tmp path"))
                .expect("temp root");
            let base = temp.path().canonicalize().expect("canonical temp root");
            let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
            inputs.state_dir = Some(base.join("state"));
            let root = inputs
                .resolve(&Resolver)
                .expect("isolated state root")
                .state;
            Self {
                _temp: temp,
                root,
                clock: TestClock::new(
                    WallTime::from_micros(2_000_000_000_000_000).expect("wall time"),
                    MonoTime::from_micros(0),
                ),
                backoff: ThreadWait,
                project: ProjectId::from_bytes([1; 16]),
                actor: ActorId::from_bytes([2; 16]),
            }
        }
        fn service(&self) -> CoordinationService<'_> {
            let budget = WaitBudget::cli(self.clock.mono_now(), 5).expect("test budget");
            let cancel = CancelToken::new();
            let request = Request {
                clock: &self.clock,
                budget,
                cancel: &cancel,
                backoff: &self.backoff,
            };
            let store = Store::open(&self.root, &request).expect("isolated store");
            CoordinationService::new(store, self.project, self.actor, &self.clock, &self.backoff)
        }
        fn budget(&self) -> WaitBudget {
            WaitBudget::mcp(self.clock.mono_now(), 5).expect("test MCP budget")
        }
    }

    #[test]
    fn mail_and_events_use_authenticated_scope_and_cursor() {
        let fixture = Fixture::new();
        let service = fixture.service();
        let cancel = CancelToken::new();
        let MailResult::Sent { id } = service
            .mail(
                MailInput::Send {
                    recipients: vec![fixture.actor],
                    kind: MessageKind::Request,
                    body: "please inspect".into(),
                    artifacts: Vec::new(),
                    reply_to: None,
                    task: None,
                },
                fixture.budget(),
                &cancel,
            )
            .expect("send mail")
        else {
            panic!("send result")
        };
        let MailResult::Read { page, cursor } = service
            .mail(
                MailInput::Read {
                    after: Some(0),
                    limit: 10,
                },
                fixture.budget(),
                &cancel,
            )
            .expect("read mail")
        else {
            panic!("read result")
        };
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].message.id, id);
        assert_eq!(page.items[0].message.project, fixture.project);
        assert_eq!(page.items[0].message.sender, fixture.actor);
        assert!(cursor > 0);

        let EventsResult::Noted { seq } = service
            .events(
                EventsInput::Note {
                    note: "handoff ready".into(),
                    dedup_key: None,
                },
                fixture.budget(),
                &cancel,
            )
            .expect("append event")
        else {
            panic!("note result")
        };
        let EventsResult::Read {
            page: event_page,
            cursor: event_cursor,
            timed_out: false,
        } = service
            .events(
                EventsInput::Read {
                    after: None,
                    limit: 10,
                    wait_ms: None,
                },
                fixture.budget(),
                &cancel,
            )
            .expect("read events")
        else {
            panic!("events result")
        };
        assert_eq!(event_page.items.last().map(|event| event.seq), Some(seq));
        assert_eq!(event_cursor, seq);
        let EventsResult::Read {
            page: timeout_page,
            timed_out: true,
            ..
        } = service
            .events(
                EventsInput::Read {
                    after: None,
                    limit: 10,
                    wait_ms: Some(1_000),
                },
                fixture.budget(),
                &cancel,
            )
            .expect("bounded event wait")
        else {
            panic!("event wait result")
        };
        assert!(timeout_page.items.is_empty());
    }

    #[test]
    fn task_proofs_are_bound_to_authenticated_actor() {
        let fixture = Fixture::new();
        let service = fixture.service();
        let cancel = CancelToken::new();
        let TaskResult::Created { task } = service
            .task(
                TaskInput::Create {
                    objective: "review change".into(),
                    note: None,
                },
                fixture.budget(),
                &cancel,
            )
            .expect("create task")
        else {
            panic!("create result")
        };
        let TaskResult::Claimed { proof } = service
            .task(
                TaskInput::Claim {
                    id: task.id,
                    lease_ms: 5_000,
                },
                fixture.budget(),
                &cancel,
            )
            .expect("claim task")
        else {
            panic!("claim result")
        };
        let TaskResult::Updated { task: updated } = service
            .task(
                TaskInput::Update {
                    proof: proof.clone(),
                    objective: "review and report".into(),
                    note: Some("bounded".into()),
                },
                fixture.budget(),
                &cancel,
            )
            .expect("update task")
        else {
            panic!("update result")
        };
        assert_eq!(updated.objective, "review and report");
        let other = CoordinationService::new(
            Store::open(
                &fixture.root,
                &Request {
                    clock: &fixture.clock,
                    budget: fixture.budget(),
                    cancel: &cancel,
                    backoff: &fixture.backoff,
                },
            )
            .expect("second connection"),
            fixture.project,
            ActorId::from_bytes([9; 16]),
            &fixture.clock,
            &fixture.backoff,
        );
        assert!(matches!(
            other.task(TaskInput::Release { proof }, fixture.budget(), &cancel),
            Err(CoordinationError::Store(
                provenant_fabric_store::Error::StaleFence
            ))
        ));
    }

    #[test]
    fn work_claim_and_landing_fences_are_service_scoped() {
        let fixture = Fixture::new();
        let service = fixture.service();
        let cancel = CancelToken::new();
        let ClaimResult::Acquired { proof, claim } = service
            .claim(
                ClaimInput::Acquire {
                    scope: WorkScope::Issue {
                        key: "PR-42".into(),
                    },
                    lease_ms: 5_000,
                },
                fixture.budget(),
                &cancel,
            )
            .expect("acquire work claim")
        else {
            panic!("claim result")
        };
        assert_eq!(claim.holder, Some(fixture.actor));
        assert!(matches!(
            service.claim(
                ClaimInput::Verify {
                    proof: proof.clone()
                },
                fixture.budget(),
                &cancel
            ),
            Ok(ClaimResult::Verified { .. })
        ));
        assert!(matches!(
            service.claim(ClaimInput::Release { proof }, fixture.budget(), &cancel),
            Ok(ClaimResult::Released { .. })
        ));

        let LandingResult::Acquired { proof, landing } = service
            .landing(
                LandingInput::Acquire {
                    reference: "refs/heads/release".into(),
                    expected: GitOid::parse("0123456789012345678901234567890123456789")
                        .expect("expected OID"),
                    candidate: GitOid::parse("abcdefabcdefabcdefabcdefabcdefabcdefabcd")
                        .expect("candidate OID"),
                    lease_ms: 5_000,
                },
                fixture.budget(),
                &cancel,
            )
            .expect("acquire landing lease")
        else {
            panic!("landing acquire result")
        };
        assert_eq!(landing.holder, Some(fixture.actor));
        assert!(matches!(
            service.landing(
                LandingInput::Wait {
                    reference: proof.reference.clone(),
                    generation: 0,
                    wait_ms: 1_000,
                },
                fixture.budget(),
                &cancel
            ),
            Ok(LandingResult::Waited { landing: Some(_) })
        ));
        assert!(matches!(
            service.landing(LandingInput::Release { proof }, fixture.budget(), &cancel),
            Ok(LandingResult::Released { .. })
        ));
    }

    #[test]
    fn input_enums_reject_unknown_fields_and_calls_honor_cancellation() {
        assert!(
            serde_json::from_str::<MailInput>(
                r#"{"op":"mark_read","through":1,"project":"forged"}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<TaskInput>(
            r#"{"op":"claim","id":"00000000000000000000000000000001","lease_ms":1000,"actor":"forged"}"#
        )
        .is_err());
        let fixture = Fixture::new();
        let service = fixture.service();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            service.events(
                EventsInput::Read {
                    after: None,
                    limit: 10,
                    wait_ms: None,
                },
                fixture.budget(),
                &cancel
            ),
            Err(CoordinationError::Cancelled)
        ));
    }
}
