//! Authenticated, bounded coordination over the Fabric store.
#![forbid(unsafe_code)]

pub mod facade;
pub mod input;
pub use crate::home::CoordinationService;

use input::bounded_input;
pub use input::{InputError, ValidateInput};

use provenant_core::{
    ActorId, CancelToken, ClaimId, Clock, DurationMicros, MessageId, ProjectId, StateRoot,
    WaitBudget, WaitTransport,
};
use provenant_os::fs::FsError;
use provenant_records::{ArtifactRef, RecordHeader};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use provenant_core::{Pause, ThreadPause};
use provenant_fabric_store::{ClaimFence, ClaimRequest, Request, Store};
pub use provenant_fabric_store::{
    Delivery, Event, EventKind, EventRecord, Message, MessageKind, MessageRef, Page,
    WorkClaim, WorkScope,
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
    #[error("clock operation failed")]
    Time(#[from] provenant_core::TimeError),
}

impl CoordinationError {
    pub fn code(&self) -> &str {
        if let Self::Peer(crate::hosts::PeerError::Refused { error }) = self {
            return error.code();
        }
        facade::FacadeError::from(self).static_code()
    }

    /// Project safe facts before a transport chooses its envelope.
    pub fn diagnostic(&self) -> provenant_records::diagnostic::Diagnostic {
        if let Self::Store(provenant_fabric_store::Error::Invalid(field)) = self {
            return provenant_records::diagnostic::Diagnostic::new(self.code())
                .detail(format!("invalid field: {field}"))
                .next("Check the field and retry.");
        }
        facade::FacadeError::from(self).diagnostic()
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
pub struct ClaimProof {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
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

bounded_input! {
    pub enum MailInput as WireMailInput {
        Send {
            recipients: Vec<ActorId>,
            kind: MessageKind,
            body: String,
            artifacts: Vec<ArtifactRef>,
            reply_to: Option<MessageRef>,
        },
        Read {
            after: Option<i64>,
            limit: u64,
            /// Read this exact chair session's mailbox instead of the agent's own.
            session: Option<String>,
        },
        MarkRead {
            through: i64,
            session: Option<String>,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum MailResult {
    Sent {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        id: MessageId,
    },
    Read {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        page: Page<Delivery>,
        cursor: i64,
    },
    MarkedRead {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        cursor: i64,
    },
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
#[serde(tag = "result", rename_all = "snake_case")]
pub enum EventsResult {
    Read {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        page: Page<Event>,
        cursor: i64,
        timed_out: bool,
        #[serde(default)]
        unknown_hosts: Vec<provenant_core::HostId>,
        #[serde(default)]
        run_events_pending: bool,
    },
    Noted {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        seq: i64,
    },
}

impl EventsResult {
    pub fn empty_timeout(after: Option<i64>) -> Self {
        Self::Read {
            extra: Default::default(),
            page: Page {
                extra: Default::default(),
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
        extra: Default::default(),
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
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ClaimResult {
    Acquired {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        proof: ClaimProof,
        claim: WorkClaim,
    },
    Renewed {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        claim: WorkClaim,
    },
    Verified {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        claim: WorkClaim,
    },
    Released {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        id: ClaimId,
    },
    Listed {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        page: Page<WorkClaim>,
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
}

pub struct LocalCoordination<'a> {
    store: Store,
    project: ProjectId,
    actor: ActorId,
    clock: &'a dyn Clock,
    backoff: &'a dyn Pause,
}

impl<'a> LocalCoordination<'a> {
    pub fn open(
        root: &StateRoot,
        project: ProjectId,
        actor: ActorId,
        clock: &'a dyn Clock,
        backoff: &'a dyn Pause,
        cancel: &CancelToken,
    ) -> Result<Self, CoordinationError> {
        let request = Request { clock, cancel };
        let store = Store::open(root, &request)?;
        Ok(Self::new(store, project, actor, clock, backoff))
    }

    pub fn new(
        store: Store,
        project: ProjectId,
        actor: ActorId,
        clock: &'a dyn Clock,
        backoff: &'a dyn Pause,
    ) -> Self {
        Self {
            store,
            project,
            actor,
            clock,
            backoff,
        }
    }

    /// A chair session's control mail has its own mailbox and cursor, so no
    /// other session that shares this agent label can read or consume it.
    fn mailbox(&self, session: Option<&str>) -> ActorId {
        match session {
            Some(session) => crate::identity::chair_session_mailbox(self.project, session),
            None => self.actor,
        }
    }

    fn request<'b>(&'b self, cancel: &'b CancelToken) -> Request<'b> {
        Request {
            clock: self.clock,
            cancel,
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

    fn claim_fence(&self, proof: &ClaimProof) -> ClaimFence {
        ClaimFence {
            extra: Default::default(),
            id: proof.id,
            actor: self.actor,
            generation: proof.generation,
            token: proof.token,
        }
    }

    fn claim_proof(&self, fence: ClaimFence) -> ClaimProof {
        ClaimProof {
            extra: Default::default(),
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
        let request = self.request(cancel);
        match input {
            MailInput::Send {
                recipients,
                kind,
                body,
                artifacts,
                reply_to,
                ..
            } => {
                let id = MessageId::from_bytes(Self::new_id_bytes()?);
                let message = Message {
                    extra: Default::default(),
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
                    created_at: self.clock.wall_now()?,
                };
                self.store.send(&message, &request)?;
                Ok(MailResult::Sent {
                    extra: Default::default(),
                    id,
                })
            }
            MailInput::Read {
                after,
                limit,
                session,
                ..
            } => {
                let mailbox = self.mailbox(session.as_deref());
                let prior_cursor = self.store.mail_cursor(self.project, mailbox, &request)?;
                let page = self
                    .store
                    .read_mail(self.project, mailbox, after, limit, &request)?;
                let page = bounded_page(page.items, page.truncated)?;
                let cursor = page
                    .items
                    .last()
                    .map(|delivery| delivery.seq)
                    .unwrap_or_else(|| after.unwrap_or(prior_cursor));
                Ok(MailResult::Read {
                    extra: Default::default(),
                    page,
                    cursor,
                })
            }
            MailInput::MarkRead {
                through, session, ..
            } => {
                let mailbox = self.mailbox(session.as_deref());
                self.store
                    .mark_mail_read(self.project, mailbox, through, &request)?;
                let cursor = self.store.mail_cursor(self.project, mailbox, &request)?;
                Ok(MailResult::MarkedRead {
                    extra: Default::default(),
                    cursor,
                })
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
                    extra: Default::default(),
                    page: Page {
                        extra: Default::default(),
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
        let request = self.request(cancel);
        match input {
            EventsInput::Read {
                after,
                limit,
                wait_seconds,
                ..
            } => {
                let read_budget = match wait_seconds {
                    Some(ms) => self.observation_budget(budget, ms)?,
                    None => budget,
                };
                let read_request = self.request(cancel);
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
                    extra: Default::default(),
                    page,
                    cursor,
                    timed_out,
                    unknown_hosts: Vec::new(),
                    run_events_pending: false,
                })
            }
            EventsInput::Note {
                note, dedup_key, ..
            } => {
                let record = EventRecord {
                    extra: Default::default(),
                    header: RecordHeader::V1,
                    project: self.project,
                    actor: Some(self.actor),
                    kind: EventKind::Note,
                    entity: None,
                    detail: note,
                    at: self.clock.wall_now()?,
                };
                let seq = self.store.note(&record, dedup_key.as_deref(), &request)?;
                Ok(EventsResult::Noted {
                    extra: Default::default(),
                    seq,
                })
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
        let request = self.request(cancel);
        match input {
            ClaimInput::Acquire {
                scope,
                lease_seconds,
                ..
            } => {
                let fence = self.store.acquire_claim(
                    &ClaimRequest {
                        extra: Default::default(),
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
                    extra: Default::default(),
                    proof: self.claim_proof(fence),
                    claim,
                })
            }
            ClaimInput::Renew {
                proof,
                lease_seconds,
                ..
            } => {
                let fence = self.claim_fence(&proof);
                self.store.renew_claim(
                    self.project,
                    &fence,
                    Self::lease_duration(lease_seconds),
                    &request,
                )?;
                let claim = self.store.verify_claim(self.project, &fence, &request)?;
                Ok(ClaimResult::Renewed {
                    extra: Default::default(),
                    claim,
                })
            }
            ClaimInput::Verify { proof, .. } => {
                let claim =
                    self.store
                        .verify_claim(self.project, &self.claim_fence(&proof), &request)?;
                Ok(ClaimResult::Verified {
                    extra: Default::default(),
                    claim,
                })
            }
            ClaimInput::Release { proof, .. } => {
                let id = proof.id;
                self.store
                    .release_claim(self.project, &self.claim_fence(&proof), &request)?;
                Ok(ClaimResult::Released {
                    extra: Default::default(),
                    id,
                })
            }
            ClaimInput::List { after, limit, .. } => {
                let page = self.store.claims(self.project, after, limit, &request)?;
                let page = bounded_page(page.items, page.truncated)?;
                Ok(ClaimResult::Listed {
                    extra: Default::default(),
                    page,
                })
            }
        }
    }
}

impl LocalCoordination<'_> {
    /// Retryable, bounded mutations atomically commit their receipt and effects.
    pub fn mutate(
        &self,
        input: Mutation,
        id: &provenant_core::OperationId,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<MutationResult, CoordinationError> {
        self.ensure_active(budget, cancel)?;
        let request = self.request(cancel);
        let frozen = input.clone();
        // The frozen input identifies the result domain. Untagged result shapes
        // overlap, so replay must decode the domain's concrete result type.
        macro_rules! record {
            ($variant:ident, $action:expr) => {
                self.store
                    .coordination_operation(self.project, self.actor, id, &frozen, &request, || {
                        $action
                    })
                    .map(MutationResult::$variant)
            };
        }
        match input {
            Mutation::Mail(MailInput::Read { .. })
            | Mutation::Events(EventsInput::Read { .. })
            | Mutation::Claim(ClaimInput::Verify { .. } | ClaimInput::List { .. }) => {
                Err(CoordinationError::InvalidInput("read is an observation"))
            }
            Mutation::Mail(value) => record!(Mail, self.mail(value, budget, cancel)),
            Mutation::Events(value) => record!(Events, self.events(value, budget, cancel)),
            Mutation::Claim(value) => record!(Claim, self.claim(value, budget, cancel)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "domain", content = "input", rename_all = "snake_case")]
pub enum Mutation {
    Mail(MailInput),
    Events(EventsInput),
    Claim(ClaimInput),
}
// Untagged wrappers add no object layer; their inner result retains extensions.
// Decode through the frozen input domain because result shapes overlap.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum MutationResult {
    Mail(MailResult),
    Events(EventsResult),
    Claim(ClaimResult),
}

/// A fresh operation id, for callers that did not supply one.
pub fn generated_operation_id(prefix: &str) -> Result<String, CoordinationError> {
    let bytes = provenant_os::fs::entropy_128().map_err(CoordinationError::Entropy)?;
    Ok(format!("{prefix}-{}", provenant_core::hex(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{MonoTime, PathResolver, RootInputs, StateRoot, TestClock, WallTime};
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
        backoff: ThreadPause,
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
                backoff: ThreadPause,
                project: ProjectId::from_bytes([1; 16]),
                actor: ActorId::from_bytes([2; 16]),
            }
        }
        fn service(&self) -> LocalCoordination<'_> {
            let cancel = CancelToken::new();
            let request = Request {
                clock: &self.clock,
                cancel: &cancel,
            };
            let store = Store::open(&self.root, &request).expect("isolated store");
            LocalCoordination::new(store, self.project, self.actor, &self.clock, &self.backoff)
        }
        fn budget(&self) -> WaitBudget {
            WaitBudget::mcp(self.clock.mono_now(), 5).expect("test MCP budget")
        }
    }

    fn assert_upgrade(error: &CoordinationError, expected_record: &str, expected_version: u64) {
        let facade = facade::FacadeError::from(error);
        assert_eq!(error.code(), "upgrade_required");
        let facade::FacadeError::UpgradeRequired {
            reader_release,
            writer_release,
            record,
            version,
        } = facade
        else {
            panic!("expected upgrade_required");
        };
        assert_eq!(reader_release, env!("CARGO_PKG_VERSION"));
        assert_eq!(writer_release, "unknown");
        assert_eq!(record, expected_record);
        assert_eq!(version, expected_version);
    }

    #[test]
    fn newer_stored_message_reports_upgrade_required() {
        let f = Fixture::new();
        let service = f.service();
        let cancel = CancelToken::new();
        service
            .mail(
                MailInput::Send {
                    extra: Default::default(),
                    recipients: vec![f.actor],
                    kind: MessageKind::Request,
                    body: "future message".into(),
                    artifacts: Vec::new(),
                    reply_to: None,
                },
                f.budget(),
                &cancel,
            )
            .unwrap();
        let db =
            rusqlite::Connection::open(f.root.as_path().join("fabric/fabric.sqlite3")).unwrap();
        db.execute("UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.header.schema_version',2) AS BLOB)", []).unwrap();
        let error = service
            .mail(
                MailInput::Read {
                    extra: Default::default(),
                    after: Some(0),
                    limit: 10,
                    session: None,
                },
                f.budget(),
                &cancel,
            )
            .unwrap_err();
        assert_upgrade(&error, "record schema", 2);
    }

    #[test]
    fn newer_database_schema_reports_upgrade_required() {
        let f = Fixture::new();
        drop(f.service());
        let version = provenant_fabric_store::CURRENT_SCHEMA_VERSION + 1;
        let db =
            rusqlite::Connection::open(f.root.as_path().join("fabric/fabric.sqlite3")).unwrap();
        db.pragma_update(None, "user_version", i64::try_from(version).unwrap())
            .unwrap();
        drop(db);
        let error = LocalCoordination::open(
            &f.root,
            f.project,
            f.actor,
            &f.clock,
            &f.backoff,
            &CancelToken::new(),
        )
        .err()
        .unwrap();
        assert_upgrade(&error, "database schema", version as u64);
    }

    #[test]
    fn coordination_time_errors_are_state_corrupt() {
        assert_eq!(
            CoordinationError::Time(provenant_core::TimeError::WallRange).code(),
            "state_corrupt"
        );
    }

    #[test]
    fn claim_replay_keeps_its_domain_and_result_extensions() {
        let fixture = Fixture::new();
        let service = fixture.service();
        let cancel = CancelToken::new();
        let acquire_id = provenant_core::OperationId::parse("acquire").unwrap();
        let MutationResult::Claim(ClaimResult::Acquired { proof, .. }) = service
            .mutate(
                Mutation::Claim(ClaimInput::Acquire {
                    extra: Default::default(),
                    scope: WorkScope::Issue {
                        extra: Default::default(),
                        key: "PR-42".into(),
                    },
                    lease_seconds: 60,
                }),
                &acquire_id,
                fixture.budget(),
                &cancel,
            )
            .unwrap()
        else {
            panic!("claim acquisition");
        };
        let claim_id = proof.id;
        let release = Mutation::Claim(ClaimInput::Release {
            extra: Default::default(),
            proof,
        });
        let release_id = provenant_core::OperationId::parse("release").unwrap();
        let first = service
            .mutate(release.clone(), &release_id, fixture.budget(), &cancel)
            .unwrap();
        assert!(matches!(
            first,
            MutationResult::Claim(ClaimResult::Released { .. })
        ));
        let database =
            rusqlite::Connection::open(fixture.root.as_path().join("fabric/fabric.sqlite3"))
                .unwrap();
        database.execute(
            "UPDATE coordination_operations SET result=CAST(json_set(CAST(result AS TEXT),'$.future',json_object('revision',2)) AS BLOB) WHERE id=?1",
            [release_id.to_string()],
        ).unwrap();
        let MutationResult::Claim(ClaimResult::Released { id, extra }) = service
            .mutate(release, &release_id, fixture.budget(), &cancel)
            .unwrap()
        else {
            panic!("replay must retain the claim domain");
        };
        assert_eq!(id, claim_id);
        assert_eq!(extra["future"], serde_json::json!({"revision":2}));
        let (input, result) = service
            .store
            .coordination_receipt_with_input::<Mutation, serde_json::Value>(
                fixture.project,
                fixture.actor,
                &release_id,
                &service.request(&cancel),
            )
            .unwrap()
            .unwrap();
        let crate::hosts::CoordinationResult::Claim {
            result: ClaimResult::Released { extra, .. },
            ..
        } = crate::hosts::CoordinationResult::from_stored(&input, result).unwrap()
        else {
            panic!("recovered receipt must retain the claim domain");
        };
        assert_eq!(extra["future"], serde_json::json!({"revision": 2}));
    }

    #[test]
    fn mail_and_events_use_authenticated_scope_and_cursor() {
        let fixture = Fixture::new();
        let service = fixture.service();
        let cancel = CancelToken::new();
        let MailResult::Sent { id, .. } = service
            .mail(
                MailInput::Send {
                    extra: Default::default(),
                    recipients: vec![fixture.actor],
                    kind: MessageKind::Request,
                    body: "please inspect".into(),
                    artifacts: Vec::new(),
                    reply_to: None,
                },
                fixture.budget(),
                &cancel,
            )
            .expect("send mail")
        else {
            panic!("send result")
        };
        let MailResult::Read { page, cursor, .. } = service
            .mail(
                MailInput::Read {
                    extra: Default::default(),
                    after: Some(0),
                    limit: 10,
                    session: None,
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

        let EventsResult::Noted { seq, .. } = service
            .events(
                EventsInput::Note {
                    extra: Default::default(),
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
                    extra: Default::default(),
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
                    extra: Default::default(),
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
                    extra: Default::default(),
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
    fn work_claim_fences_are_service_scoped() {
        let fixture = Fixture::new();
        let service = fixture.service();
        let cancel = CancelToken::new();
        let ClaimResult::Acquired { proof, claim, .. } = service
            .claim(
                ClaimInput::Acquire {
                    extra: Default::default(),
                    scope: WorkScope::Issue {
                        extra: Default::default(),
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
                    extra: Default::default(),
                    proof: proof.clone()
                },
                fixture.budget(),
                &cancel
            ),
            Ok(ClaimResult::Verified { .. })
        ));
        assert!(matches!(
            service.claim(
                ClaimInput::Release {
                    extra: Default::default(),
                    proof
                },
                fixture.budget(),
                &cancel
            ),
            Ok(ClaimResult::Released { .. })
        ));
    }

    struct SimulatedClock(std::sync::Mutex<u64>);
    impl Clock for SimulatedClock {
        fn mono_now(&self) -> MonoTime {
            MonoTime::from_micros(*self.0.lock().unwrap())
        }
        fn wall_now(&self) -> Result<WallTime, provenant_core::TimeError> {
            WallTime::from_micros(2_000_000_000_000_000)
        }
        fn wait_until(&self, deadline: MonoTime) -> Result<(), provenant_core::TimeError> {
            let mut value = self.0.lock().unwrap();
            *value = (*value).max(deadline.as_micros());
            Ok(())
        }
    }
    impl Pause for SimulatedClock {
        fn wait(&self, duration: DurationMicros, _: &CancelToken) {
            *self.0.lock().unwrap() += duration.as_micros();
        }
    }
    #[test]
    fn expired_event_waits_are_data_using_the_injected_clock() {
        let fixture = Fixture::new();
        let clock = SimulatedClock(std::sync::Mutex::new(0));
        let cancel = CancelToken::new();
        let budget = || WaitBudget::mcp(clock.mono_now(), 5).expect("budget");
        let service = LocalCoordination::open(
            &fixture.root,
            fixture.project,
            fixture.actor,
            &clock,
            &clock,
            &cancel,
        )
        .expect("service");
        let EventsResult::Read { cursor, .. } = service
            .events(
                EventsInput::Read {
                    extra: Default::default(),
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
                    extra: Default::default(),
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
        assert_eq!(*clock.0.lock().unwrap(), 1_000_000);
        let expired = budget();
        *clock.0.lock().unwrap() = 7_000_000;
        assert!(matches!(
            service.events(
                EventsInput::Read {
                    extra: Default::default(),
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
    }

    #[test]
    fn calls_honour_cancellation() {
        let fixture = Fixture::new();
        let service = fixture.service();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            service.events(
                EventsInput::Read {
                    extra: Default::default(),
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
