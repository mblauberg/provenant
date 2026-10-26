//! Authenticated, bounded coordination over the Fabric store.
#![forbid(unsafe_code)]

pub mod facade;
pub mod input;
pub use crate::home::CoordinationService;

use input::bounded_input;
pub use input::{InputError, ValidateInput};

use provenant_core::{
    ActorId, CancelToken, ClaimId, Clock, DurationMicros, GitOid, MessageId, ProjectId, StateRoot,
    TaskId, WaitBudget, WaitTransport,
};
use provenant_os::fs::FsError;
use provenant_records::{ArtifactRef, RecordHeader};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use provenant_fabric_store::{ClaimFence, ClaimRequest, LandingRequest, Request, Store, TaskClaim};
pub use provenant_fabric_store::{
    Delivery, Event, EventKind, EventRecord, Landing, Message, MessageKind, MessageRef, Page,
    RetryWait, Task, TaskRef, TaskState, ThreadWait, WorkClaim, WorkScope,
};

#[derive(Debug, Error)]
pub enum CoordinationError {
    #[error("coordination mutations are not allowed inside a lane")]
    LaneMutation,
    #[error(transparent)]
    Peer(#[from] crate::hosts::PeerError),
    #[error("invalid coordination input: {0}")]
    InvalidInput(&'static str),
    #[error(transparent)]
    Input(#[from] InputError),
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
    #[error("Fabric store operation failed")]
    Store(#[source] provenant_fabric_store::Error),
    /// The store refused an upgrade; its message names the data and remedy.
    #[error(transparent)]
    UpgradeRefused(provenant_fabric_store::Error),
    #[error("clock operation failed")]
    Time(#[from] provenant_core::TimeError),
}

impl CoordinationError {
    pub fn code(&self) -> &'static str {
        facade::FacadeError::from(self).code()
    }
}

impl From<provenant_fabric_store::Error> for CoordinationError {
    fn from(error: provenant_fabric_store::Error) -> Self {
        match error {
            provenant_fabric_store::Error::Busy => Self::Busy,
            provenant_fabric_store::Error::Cancelled => Self::Cancelled,
            error @ provenant_fabric_store::Error::LegacyImportData { .. } => {
                Self::UpgradeRefused(error)
            }
            other => Self::Store(other),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProof {
    pub task: TaskId,
    pub generation: u64,
    #[serde(with = "input::hex_token")]
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
    #[serde(with = "input::hex_token")]
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
    #[serde(with = "input::hex_token")]
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

bounded_input! {
    pub enum MailInput as WireMailInput {
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
        MarkRead { through: i64 },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum MailResult {
    Sent { id: MessageId },
    Read { page: Page<Delivery>, cursor: i64 },
    MarkedRead { cursor: i64 },
}

bounded_input! {
    pub enum TaskInput as WireTaskInput {
        Create { objective: String, note: Option<String> },
        List {
            state: Option<TaskState>,
            after: Option<TaskId>,
            limit: u64,
        },
        Claim { id: TaskId, lease_seconds: u64 },
        Update {
            proof: TaskProof,
            objective: String,
            note: Option<String>,
        },
        Finish { proof: TaskProof, state: TaskState },
        Release { proof: TaskProof },
    }
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

bounded_input! {
    pub enum EventsInput as WireEventsInput {
        Read {
            after: Option<i64>,
            limit: u64,
            wait_seconds: Option<u64>,
        },
        Note { note: String, dedup_key: Option<String> },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventsResult {
    Read {
        page: Page<Event>,
        cursor: i64,
        timed_out: bool,
        #[serde(default)]
        unknown_hosts: Vec<provenant_core::HostId>,
        #[serde(default)]
        run_events_pending: bool,
    },
    Noted {
        seq: i64,
    },
}

impl EventsResult {
    pub fn empty_timeout(after: Option<i64>) -> Self {
        Self::Read {
            page: Page {
                items: vec![],
                truncated: false,
            },
            cursor: after.unwrap_or(0),
            timed_out: true,
            unknown_hosts: vec![],
            run_events_pending: false,
        }
    }
}

const MAX_COORDINATION_RESULT_BYTES: usize = 768 * 1024;

fn bounded_page<T: Serialize>(
    items: Vec<T>,
    truncated: bool,
) -> Result<Page<T>, CoordinationError> {
    let mut page = Page {
        items: Vec::with_capacity(items.len()),
        truncated,
    };
    let mut remaining = MAX_COORDINATION_RESULT_BYTES;
    for item in items {
        // Measure each row once, rather than re-encoding the growing page.
        let size = serde_json::to_vec(&item)
            .map_err(CoordinationError::ResponseEncoding)?
            .len();
        if size > remaining {
            page.truncated = true;
            break;
        }
        remaining -= size;
        page.items.push(item);
    }
    Ok(page)
}

// Claims and landings share the `fabric_lease` tool, so each op names its kind
// on every wire.
bounded_input! {
    pub enum ClaimInput as WireClaimInput {
        #[serde(rename = "claim_acquire")]
        Acquire { scope: WorkScope, lease_seconds: u64 },
        #[serde(rename = "claim_renew")]
        Renew { proof: ClaimProof, lease_seconds: u64 },
        #[serde(rename = "claim_verify")]
        Verify { proof: ClaimProof },
        #[serde(rename = "claim_release")]
        Release { proof: ClaimProof },
        #[serde(rename = "claim_list")]
        List { after: Option<ClaimId>, limit: u64 },
    }
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

bounded_input! {
    pub enum LandingInput as WireLandingInput {
        #[serde(rename = "landing_acquire")]
        Acquire {
            reference: String,
            expected: GitOid,
            candidate: GitOid,
            lease_seconds: u64,
        },
        #[serde(rename = "landing_renew")]
        Renew { proof: LandingProof, lease_seconds: u64 },
        #[serde(rename = "landing_verify")]
        Verify { proof: LandingProof },
        #[serde(rename = "landing_release")]
        Release { proof: LandingProof },
        #[serde(rename = "landing_wait")]
        Wait { reference: String, generation: u64, wait_seconds: u64 },
    }
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
        timed_out: bool,
    },
}

/// Narrow seam consumed by MCP and implemented by the runtime composition.
/// The service is worker-local because each `Store` owns one SQLite connection.
pub trait CoordinationApi {
    fn mutate(
        &self,
        input: Mutation,
        operation_id: Option<provenant_core::OperationId>,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<MutationResult, CoordinationError>;
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

pub struct LocalCoordination<'a> {
    store: Store,
    project: ProjectId,
    actor: ActorId,
    clock: &'a dyn Clock,
    backoff: &'a dyn RetryWait,
}

impl<'a> LocalCoordination<'a> {
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

    fn lease_duration(seconds: u64) -> DurationMicros {
        DurationMicros::from_micros(seconds * 1_000_000)
    }

    fn observation_budget(
        &self,
        budget: WaitBudget,
        wait_seconds: u64,
    ) -> Result<WaitBudget, CoordinationError> {
        let requested = WaitBudget::new(
            self.clock.mono_now(),
            DurationMicros::from_micros(wait_seconds * 1_000_000),
            WaitTransport::Cli,
        )?;
        Ok(
            if requested.deadline().as_mono_time() < budget.deadline().as_mono_time() {
                requested
            } else {
                budget
            },
        )
    }

    pub(crate) fn pause_observation(&self, budget: WaitBudget, cancel: &CancelToken) {
        self.backoff.wait(
            DurationMicros::from_micros(
                500_000.min(budget.remaining(self.clock.mono_now()).as_micros()),
            ),
            cancel,
        );
    }
    pub(crate) fn pause(&self, budget: WaitBudget, cancel: &CancelToken) {
        let left = budget.remaining(self.clock.mono_now()).as_micros();
        self.backoff
            .wait(DurationMicros::from_micros(left.min(50_000)), cancel);
    }

    fn new_id_bytes() -> Result<[u8; 16], CoordinationError> {
        provenant_os::fs::entropy_128().map_err(CoordinationError::Entropy)
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

impl CoordinationApi for LocalCoordination<'_> {
    fn mutate(
        &self,
        input: Mutation,
        operation_id: Option<provenant_core::OperationId>,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<MutationResult, CoordinationError> {
        let operation_id = operation_id.unwrap_or(
            provenant_core::OperationId::parse(&provenant_core::hex(
                &provenant_os::fs::entropy_128().map_err(CoordinationError::Entropy)?,
            ))
            .map_err(|_| CoordinationError::EntropyEncoding)?,
        );
        LocalCoordination::mutate(self, input, &operation_id, budget, cancel)
    }

    fn mail(
        &self,
        input: MailInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<MailResult, CoordinationError> {
        input.validate_input(WaitTransport::Cli)?;
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
                    kind,
                    body,
                    artifacts,
                    reply_to,
                    task,
                    created_at: self.clock.wall_now()?,
                };
                self.store.send(&message, &request)?;
                Ok(MailResult::Sent { id })
            }
            MailInput::Read { after, limit } => {
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
        input.validate_input(WaitTransport::Cli)?;
        self.ensure_active(budget, cancel)?;
        let request = self.request(budget, cancel);
        match input {
            TaskInput::Create { objective, note } => {
                let now = self.clock.wall_now()?;
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
                    created_at: now,
                    updated_at: now,
                };
                self.store.create_task(&task, &request)?;
                Ok(TaskResult::Created { task })
            }
            TaskInput::List {
                state,
                after,
                limit,
            } => {
                let page = self
                    .store
                    .tasks(self.project, state, after, limit, &request)?;
                Ok(TaskResult::Listed {
                    page: bounded_page(page.items, page.truncated)?,
                })
            }
            TaskInput::Claim { id, lease_seconds } => {
                let fence = self.store.claim_task(
                    &TaskClaim {
                        project: self.project,
                        task: id,
                        actor: self.actor,
                        token: Self::new_id_bytes()?,
                        lease: Self::lease_duration(lease_seconds),
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
        input.validate_input(WaitTransport::Cli)?;
        if let Err(error) = self.ensure_active(budget, cancel) {
            return match (&error, &input) {
                (
                    CoordinationError::Timeout,
                    EventsInput::Read {
                        after,
                        wait_seconds: Some(_),
                        ..
                    },
                ) => Ok(EventsResult::Read {
                    page: Page {
                        items: Vec::new(),
                        truncated: false,
                    },
                    cursor: after.unwrap_or(0),
                    timed_out: true,
                    unknown_hosts: Vec::new(),
                    run_events_pending: false,
                }),
                _ => Err(error),
            };
        }
        let request = self.request(budget, cancel);
        match input {
            EventsInput::Read {
                after,
                limit,
                wait_seconds,
            } => {
                let read_budget = match wait_seconds {
                    Some(ms) => self.observation_budget(budget, ms)?,
                    None => budget,
                };
                let read_request = self.request(read_budget, cancel);
                // fabric.md CLI and MCP surfaces: events use caller-held cursors.
                let start = after.unwrap_or(0);
                let mut page = self
                    .store
                    .events(self.project, start, limit, &read_request)?;
                let mut timed_out = false;
                while page.items.is_empty() && wait_seconds.is_some() {
                    if cancel.is_cancelled() {
                        return Err(CoordinationError::Cancelled);
                    }
                    self.pause(read_budget, cancel);
                    if read_budget.remaining(self.clock.mono_now()).as_micros() == 0 {
                        timed_out = true;
                        break;
                    }
                    page = self
                        .store
                        .events(self.project, start, limit, &read_request)?;
                }
                let page = bounded_page(page.items, page.truncated)?;
                let cursor = page.items.last().map(|event| event.seq).unwrap_or(start);
                Ok(EventsResult::Read {
                    page,
                    cursor,
                    timed_out,
                    unknown_hosts: Vec::new(),
                    run_events_pending: false,
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
        input.validate_input(WaitTransport::Cli)?;
        self.ensure_active(budget, cancel)?;
        let request = self.request(budget, cancel);
        match input {
            ClaimInput::Acquire {
                scope,
                lease_seconds,
            } => {
                let fence = self.store.acquire_claim(
                    &ClaimRequest {
                        id: ClaimId::from_bytes(Self::new_id_bytes()?),
                        project: self.project,
                        actor: self.actor,
                        token: Self::new_id_bytes()?,
                        lease: Self::lease_duration(lease_seconds),
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
            ClaimInput::Renew {
                proof,
                lease_seconds,
            } => {
                let fence = self.claim_fence(&proof);
                self.store.renew_claim(
                    self.project,
                    &fence,
                    Self::lease_duration(lease_seconds),
                    &request,
                )?;
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
        input.validate_input(WaitTransport::Cli)?;
        if let Err(error) = self.ensure_active(budget, cancel) {
            return match (&error, &input) {
                (CoordinationError::Timeout, LandingInput::Wait { .. }) => {
                    Ok(LandingResult::Waited {
                        landing: None,
                        timed_out: true,
                    })
                }
                _ => Err(error),
            };
        }
        let request = self.request(budget, cancel);
        match input {
            LandingInput::Wait {
                reference,
                generation,
                wait_seconds,
            } => {
                let poll_budget = self.observation_budget(budget, wait_seconds)?;
                let poll_request = self.request(poll_budget, cancel);
                let mut current = None;
                loop {
                    if cancel.is_cancelled() {
                        return Err(CoordinationError::Cancelled);
                    }
                    if poll_budget.remaining(self.clock.mono_now()).as_micros() == 0 {
                        return Ok(LandingResult::Waited {
                            landing: current,
                            timed_out: true,
                        });
                    }
                    current = self
                        .store
                        .landing(self.project, &reference, &poll_request)?;
                    let now = self.clock.wall_now()?;
                    let changed = current.as_ref().is_none_or(|landing| {
                        landing.generation != generation
                            || landing.holder.is_none()
                            || landing
                                .expires_at
                                .is_some_and(|expires_at| expires_at <= now)
                    });
                    if changed {
                        return Ok(LandingResult::Waited {
                            landing: current,
                            timed_out: false,
                        });
                    }
                    self.pause(poll_budget, cancel);
                }
            }
            input => {
                let reference = landing_reference(&input)?;
                let guard = self.store.lock_landing(self.project, reference, &request)?;
                self.landing_locked(input, &guard, &request)
            }
        }
    }
}

impl LocalCoordination<'_> {
    fn landing_locked(
        &self,
        input: LandingInput,
        guard: &provenant_fabric_store::LandingGuard,
        request: &Request<'_>,
    ) -> Result<LandingResult, CoordinationError> {
        match input {
            LandingInput::Acquire {
                reference,
                expected,
                candidate,
                lease_seconds,
            } => {
                let fence = self.store.acquire_landing(
                    guard,
                    &LandingRequest {
                        id: ClaimId::from_bytes(Self::new_id_bytes()?),
                        project: self.project,
                        actor: self.actor,
                        token: Self::new_id_bytes()?,
                        reference: reference.clone(),
                        expected,
                        candidate,
                        lease: Self::lease_duration(lease_seconds),
                    },
                    request,
                )?;
                let landing = self.store.verify_landing(guard, &fence, request)?;
                Ok(LandingResult::Acquired {
                    proof: self.landing_proof(reference, fence),
                    landing,
                })
            }
            LandingInput::Renew {
                proof,
                lease_seconds,
            } => {
                let fence = self.landing_fence(&proof);
                self.store.renew_landing(
                    guard,
                    &fence,
                    Self::lease_duration(lease_seconds),
                    request,
                )?;
                let landing = self.store.verify_landing(guard, &fence, request)?;
                Ok(LandingResult::Renewed { landing })
            }
            LandingInput::Verify { proof } => {
                let landing =
                    self.store
                        .verify_landing(guard, &self.landing_fence(&proof), request)?;
                Ok(LandingResult::Verified { landing })
            }
            LandingInput::Release { proof } => {
                let id = proof.id;
                self.store
                    .release_landing(guard, &self.landing_fence(&proof), request)?;
                Ok(LandingResult::Released {
                    id,
                    reference: proof.reference,
                })
            }
            LandingInput::Wait { .. } => {
                Err(CoordinationError::InvalidInput("wait is an observation"))
            }
        }
    }

    /// Retryable, bounded mutations atomically commit their receipt and effects.
    /// The landing OS lock is acquired before the SQLite transaction.
    pub fn mutate(
        &self,
        input: Mutation,
        id: &provenant_core::OperationId,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<MutationResult, CoordinationError> {
        self.ensure_active(budget, cancel)?;
        let request = self.request(budget, cancel);
        let guard = if let Mutation::Landing(value) = &input {
            Some(
                self.store
                    .lock_landing(self.project, landing_reference(value)?, &request)?,
            )
        } else {
            None
        };
        let frozen = input.clone();
        self.store
            .coordination_operation(
                self.project,
                self.actor,
                id,
                &frozen,
                &request,
                || match input {
                    Mutation::Mail(value) => match value {
                        MailInput::Read { .. } => {
                            Err(CoordinationError::InvalidInput("read is an observation"))
                        }
                        value => self.mail(value, budget, cancel).map(MutationResult::Mail),
                    },
                    Mutation::Task(value) => match value {
                        TaskInput::List { .. } => {
                            Err(CoordinationError::InvalidInput("list is an observation"))
                        }
                        value => self.task(value, budget, cancel).map(MutationResult::Task),
                    },
                    Mutation::Events(value) => match value {
                        EventsInput::Read { .. } => {
                            Err(CoordinationError::InvalidInput("read is an observation"))
                        }
                        value => self
                            .events(value, budget, cancel)
                            .map(MutationResult::Events),
                    },
                    Mutation::Claim(value) => match value {
                        ClaimInput::Verify { .. } | ClaimInput::List { .. } => {
                            Err(CoordinationError::InvalidInput("read is an observation"))
                        }
                        value => self.claim(value, budget, cancel).map(MutationResult::Claim),
                    },
                    Mutation::Landing(value) => {
                        value.validate_input(WaitTransport::Cli)?;
                        match (&value, guard.as_ref()) {
                            (LandingInput::Verify { .. } | LandingInput::Wait { .. }, _) => {
                                Err(CoordinationError::InvalidInput("read is an observation"))
                            }
                            (_, Some(guard)) => self
                                .landing_locked(value, guard, &request)
                                .map(MutationResult::Landing),
                            _ => Err(CoordinationError::InvalidInput("landing lock required")),
                        }
                    }
                },
            )
    }
}

fn landing_reference(input: &LandingInput) -> Result<&str, CoordinationError> {
    match input {
        LandingInput::Acquire { reference, .. } => Ok(reference),
        LandingInput::Renew { proof, .. }
        | LandingInput::Release { proof }
        | LandingInput::Verify { proof } => Ok(&proof.reference),
        LandingInput::Wait { .. } => Err(CoordinationError::InvalidInput("wait is an observation")),
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "domain", content = "input", rename_all = "snake_case")]
pub enum Mutation {
    Mail(MailInput),
    Task(TaskInput),
    Events(EventsInput),
    Claim(ClaimInput),
    Landing(LandingInput),
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MutationResult {
    Mail(MailResult),
    Task(TaskResult),
    Events(EventsResult),
    Claim(ClaimResult),
    Landing(LandingResult),
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
            let temp = crate::test_support::tempdir().expect("temp root");
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
        fn service(&self) -> LocalCoordination<'_> {
            let budget = WaitBudget::cli(self.clock.mono_now(), 5).expect("test budget");
            let cancel = CancelToken::new();
            let request = Request {
                clock: &self.clock,
                budget,
                cancel: &cancel,
                backoff: &self.backoff,
            };
            let store = Store::open(&self.root, &request).expect("isolated store");
            LocalCoordination::new(store, self.project, self.actor, &self.clock, &self.backoff)
        }
        fn budget(&self) -> WaitBudget {
            WaitBudget::mcp(self.clock.mono_now(), 5).expect("test MCP budget")
        }
    }

    struct TickingClock(std::cell::Cell<i64>);
    impl Clock for TickingClock {
        fn wall_now(&self) -> Result<WallTime, provenant_core::TimeError> {
            let now = self.0.get();
            self.0.set(now + 1);
            WallTime::from_micros(now)
        }
        fn mono_now(&self) -> MonoTime {
            MonoTime::from_micros(0)
        }
        fn wait_until(&self, _deadline: MonoTime) -> Result<(), provenant_core::TimeError> {
            unreachable!("fixed-time test clock never waits")
        }
    }

    #[test]
    fn coordination_time_errors_are_state_corrupt() {
        assert_eq!(
            CoordinationError::Time(provenant_core::TimeError::WallRange).code(),
            "state_corrupt"
        );
    }

    #[test]
    fn refused_upgrade_has_its_own_code_and_names_the_data() {
        let error = CoordinationError::from(provenant_fabric_store::Error::LegacyImportData {
            table: "tasks",
            rows: 2,
        });
        assert_eq!(error.code(), "upgrade_refused");
        let text = error.to_string();
        assert!(
            text.contains("2 row(s) of legacy import data in table tasks"),
            "{text}"
        );
        assert_eq!(facade::FacadeError::from(&error).code(), "upgrade_refused");
    }

    #[test]
    fn task_create_uses_one_timestamp() {
        let temporary = crate::test_support::tempdir().expect("temp root");
        let base = temporary
            .path()
            .canonicalize()
            .expect("canonical temp root");
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let root = inputs
            .resolve(&Resolver)
            .expect("isolated state root")
            .state;
        let clock = TickingClock(std::cell::Cell::new(2_000_000_000_000_000));
        let backoff = ThreadWait;
        let budget = WaitBudget::cli(clock.mono_now(), 5).expect("test budget");
        let cancel = CancelToken::new();
        let service = LocalCoordination::open(
            &root,
            ProjectId::from_bytes([31; 16]),
            ActorId::from_bytes([32; 16]),
            &clock,
            &backoff,
            budget,
            &cancel,
        )
        .expect("open service");

        let TaskResult::Created { task } = service
            .task(
                TaskInput::Create {
                    objective: "single timestamp".into(),
                    note: None,
                },
                budget,
                &cancel,
            )
            .expect("create task")
        else {
            panic!("create result")
        };

        assert_eq!(task.created_at, task.updated_at);
    }

    struct AdvancingClock {
        wall: std::cell::Cell<i64>,
        mono: std::cell::Cell<u64>,
    }
    impl AdvancingClock {
        fn new() -> Self {
            Self {
                wall: std::cell::Cell::new(2_000_000_000_000_000),
                mono: std::cell::Cell::new(0),
            }
        }
        fn budget(&self) -> WaitBudget {
            WaitBudget::cli(MonoTime::from_micros(self.mono.get()), 10).expect("wait budget")
        }
    }
    impl Clock for AdvancingClock {
        fn wall_now(&self) -> Result<WallTime, provenant_core::TimeError> {
            WallTime::from_micros(self.wall.get())
        }
        fn mono_now(&self) -> MonoTime {
            MonoTime::from_micros(self.mono.get())
        }
        fn wait_until(&self, deadline: MonoTime) -> Result<(), provenant_core::TimeError> {
            let now = self.mono_now();
            if deadline > now {
                let duration = DurationMicros::from_micros(deadline.as_micros() - now.as_micros());
                let wall = WallTime::from_micros(self.wall.get())?.checked_add(duration)?;
                self.wall.set(wall.as_micros());
                self.mono.set(deadline.as_micros());
            }
            Ok(())
        }
    }
    impl RetryWait for AdvancingClock {
        fn wait(&self, duration: DurationMicros, _: &CancelToken) {
            self.wall.set(self.wall.get() + duration.as_micros() as i64);
            self.mono.set(self.mono.get() + duration.as_micros());
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
            ..
        } = service
            .events(
                EventsInput::Read {
                    after: None,
                    limit: 10,
                    wait_seconds: None,
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
        // A lost/suppressed response must be replayable without a mark.
        let EventsResult::Read {
            page: replay,
            cursor,
            ..
        } = service
            .events(
                EventsInput::Read {
                    after: None,
                    limit: 10,
                    wait_seconds: None,
                },
                fixture.budget(),
                &cancel,
            )
            .expect("replay events")
        else {
            panic!("events read")
        };
        assert_eq!(replay, event_page);
        assert_eq!(cursor, event_cursor);
        let EventsResult::Read { page, .. } = service
            .events(
                EventsInput::Read {
                    after: Some(cursor),
                    limit: 10,
                    wait_seconds: None,
                },
                fixture.budget(),
                &cancel,
            )
            .expect("caller cursor")
        else {
            panic!("events read")
        };
        assert!(page.items.is_empty());
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
                    lease_seconds: 5,
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
        let other = LocalCoordination::new(
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
                    lease_seconds: 5,
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
                    lease_seconds: 5,
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
                    wait_seconds: 1,
                },
                fixture.budget(),
                &cancel
            ),
            Ok(LandingResult::Waited {
                landing: Some(_),
                timed_out: false
            })
        ));
        assert!(matches!(
            service.landing(LandingInput::Release { proof }, fixture.budget(), &cancel),
            Ok(LandingResult::Released { .. })
        ));
    }

    #[test]
    fn landing_wait_returns_on_expired_holder() {
        let fixture = Fixture::new();
        let clock = AdvancingClock::new();
        let cancel = CancelToken::new();
        let service = LocalCoordination::open(
            &fixture.root,
            fixture.project,
            fixture.actor,
            &clock,
            &clock,
            clock.budget(),
            &cancel,
        )
        .expect("open service");
        let LandingResult::Acquired { landing, .. } = service
            .landing(
                LandingInput::Acquire {
                    reference: "refs/heads/expired-wait".into(),
                    expected: GitOid::parse(&"a".repeat(40)).expect("expected oid"),
                    candidate: GitOid::parse(&"b".repeat(40)).expect("candidate oid"),
                    lease_seconds: 1,
                },
                clock.budget(),
                &cancel,
            )
            .expect("acquire lease")
        else {
            panic!("landing acquire result")
        };
        clock
            .wall
            .set(landing.expires_at.expect("lease expiry").as_micros());

        assert!(matches!(
            service.landing(
                LandingInput::Wait {
                    reference: landing.reference,
                    generation: landing.generation,
                    wait_seconds: 5,
                },
                clock.budget(),
                &cancel,
            ),
            Ok(LandingResult::Waited {
                landing: Some(_),
                timed_out: false,
            })
        ));
    }

    struct SimulatedClock(std::cell::Cell<u64>);
    impl Clock for SimulatedClock {
        fn mono_now(&self) -> MonoTime {
            MonoTime::from_micros(self.0.get())
        }
        fn wall_now(&self) -> Result<WallTime, provenant_core::TimeError> {
            WallTime::from_micros(2_000_000_000_000_000)
        }
        fn wait_until(&self, deadline: MonoTime) -> Result<(), provenant_core::TimeError> {
            self.0.set(self.0.get().max(deadline.as_micros()));
            Ok(())
        }
    }
    impl RetryWait for SimulatedClock {
        fn wait(&self, duration: DurationMicros, _: &CancelToken) {
            self.0.set(self.0.get() + duration.as_micros());
        }
    }
    #[test]
    fn expired_event_and_landing_waits_are_data_using_the_injected_clock() {
        let fixture = Fixture::new();
        let clock = SimulatedClock(std::cell::Cell::new(0));
        let cancel = CancelToken::new();
        let budget = || WaitBudget::mcp(clock.mono_now(), 5).expect("budget");
        let service = LocalCoordination::open(
            &fixture.root,
            fixture.project,
            fixture.actor,
            &clock,
            &clock,
            budget(),
            &cancel,
        )
        .expect("service");
        let acquired = service
            .landing(
                LandingInput::Acquire {
                    reference: "refs/heads/main".into(),
                    expected: GitOid::parse(&"a".repeat(40)).expect("oid"),
                    candidate: GitOid::parse(&"b".repeat(40)).expect("oid"),
                    lease_seconds: 5,
                },
                budget(),
                &cancel,
            )
            .expect("acquire");
        let LandingResult::Acquired { landing, .. } = acquired else {
            panic!("landing");
        };
        let result = service
            .landing(
                LandingInput::Wait {
                    reference: "refs/heads/main".into(),
                    generation: landing.generation,
                    wait_seconds: 1,
                },
                budget(),
                &cancel,
            )
            .expect("expired wait is data");
        let value = serde_json::to_value(result).expect("result");
        assert_eq!(value["timed_out"], true);
        assert_eq!(clock.0.get(), 1_000_000);
        let EventsResult::Read { cursor, .. } = service
            .events(
                EventsInput::Read {
                    after: None,
                    limit: 100,
                    wait_seconds: None,
                },
                budget(),
                &cancel,
            )
            .expect("cursor")
        else {
            panic!("events");
        };
        assert!(matches!(
            service.events(
                EventsInput::Read {
                    after: Some(cursor),
                    limit: 100,
                    wait_seconds: Some(1),
                },
                budget(),
                &cancel
            ),
            Ok(EventsResult::Read {
                timed_out: true,
                ..
            })
        ));
        assert_eq!(clock.0.get(), 2_000_000);
        let expired = budget();
        clock.0.set(7_000_000);
        assert!(matches!(
            service.events(
                EventsInput::Read {
                    after: Some(cursor),
                    limit: 100,
                    wait_seconds: Some(1),
                },
                expired,
                &cancel
            ),
            Ok(EventsResult::Read {
                timed_out: true,
                ..
            })
        ));
        assert!(matches!(
            service.landing(
                LandingInput::Wait {
                    reference: "refs/heads/main".into(),
                    generation: landing.generation,
                    wait_seconds: 1,
                },
                expired,
                &cancel
            ),
            Ok(LandingResult::Waited {
                timed_out: true,
                ..
            })
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
            r#"{"op":"claim","id":"00000000000000000000000000000001","lease_seconds":1,"actor":"forged"}"#
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
                    wait_seconds: None,
                },
                fixture.budget(),
                &cancel
            ),
            Err(CoordinationError::Cancelled)
        ));
    }
}
