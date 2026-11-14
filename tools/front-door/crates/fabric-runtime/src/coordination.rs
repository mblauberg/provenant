//! Authenticated, bounded coordination over the Fabric store.
#![forbid(unsafe_code)]

pub mod facade;
pub mod input;
pub use crate::home::CoordinationService;

use input::bounded_input;
pub use input::{InputError, ValidateInput};

use provenant_core::{
    ActorId, CancelToken, Clock, DurationMicros, MessageId, ProjectId, StateRoot, WaitBudget,
    WaitTransport,
};
use provenant_os::fs::FsError;
use provenant_records::{ArtifactRef, RecordHeader};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use provenant_core::{Pause, ThreadPause};
pub use provenant_fabric_store::{
    Delivery, Event, EventKind, EventRecord, Message, MessageKind, MessageRef, Page,
};
use provenant_fabric_store::{HostStore, Request, Store};
pub use provenant_records::custody::{
    CustodyChange, CustodyPage, CustodyQuery, CustodyRecord, CustodyState, Holder, Origin, Subject,
    SubjectKind,
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

pub const LOCAL_OBSERVATION_INTERVAL_KEY: &str = "domains.wait.local_interval_ms";

/// Source cadence for local event observation when no native event source is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationSettings {
    pub local_interval: DurationMicros,
    pub source: provenant_config::Origin,
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
        Acquire { subject: Subject, holder: Holder },
        #[serde(rename = "claim_renew")]
        Renew { subject: Subject },
        #[serde(rename = "claim_verify")]
        Verify { subject: Subject },
        #[serde(rename = "claim_release")]
        Release { subject: Subject },
        #[serde(rename = "claim_list")]
        List { after: Option<u64>, limit: u64 },
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ClaimResult {
    Acquired {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        change: Box<CustodyChange>,
    },
    Renewed {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        record: Box<CustodyRecord>,
    },
    Verified {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        record: Box<CustodyRecord>,
    },
    Released {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        record: Box<CustodyRecord>,
    },
    Listed {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        page: CustodyPage,
    },
}

/// Narrow seam consumed by the relay host and implemented by the runtime composition.
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
    observation: ObservationSettings,
    clock: &'a dyn Clock,
    backoff: &'a dyn Pause,
}

impl<'a> LocalCoordination<'a> {
    pub fn open(
        root: &StateRoot,
        project: ProjectId,
        actor: ActorId,
        observation: ObservationSettings,
        clock: &'a dyn Clock,
        backoff: &'a dyn Pause,
        cancel: &CancelToken,
    ) -> Result<Self, CoordinationError> {
        let request = Request { clock, cancel };
        let store = Store::open(root, &request)?;
        Ok(Self::new(
            store,
            project,
            actor,
            observation,
            clock,
            backoff,
        ))
    }

    pub fn new(
        store: Store,
        project: ProjectId,
        actor: ActorId,
        observation: ObservationSettings,
        clock: &'a dyn Clock,
        backoff: &'a dyn Pause,
    ) -> Self {
        Self {
            store,
            project,
            actor,
            observation,
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

    fn wait_fallback_interval(
        &self,
        duration: DurationMicros,
        cancel: &CancelToken,
    ) -> Result<(), CoordinationError> {
        let mut remaining = duration.as_micros();
        while remaining > 0 && !cancel.is_cancelled() {
            let slice = remaining.min(250_000);
            let before = self.clock.mono_now();
            self.backoff
                .wait(DurationMicros::from_micros(slice), cancel);
            if cancel.is_cancelled() {
                break;
            }
            let after = self.clock.mono_now();
            if after > before {
                remaining = remaining.saturating_sub(after.as_micros() - before.as_micros());
            } else {
                let deadline = before.checked_add(DurationMicros::from_micros(slice))?;
                self.clock.wait_until(deadline)?;
                remaining = remaining.saturating_sub(slice);
            }
        }
        Ok(())
    }

    fn observation_extra(
        &self,
        backend: Option<provenant_os::changes::watch::Backend>,
        fallback_reason: Option<&str>,
    ) -> provenant_records::ExtraFields {
        let mut extra = provenant_records::ExtraFields::new();
        let value = match (backend, fallback_reason) {
            (Some(backend), _) => serde_json::json!({
                "mode": "event_source",
                "backend": match backend {
                    provenant_os::changes::watch::Backend::Inotify => "inotify",
                    provenant_os::changes::watch::Backend::Kqueue => "kqueue",
                    provenant_os::changes::watch::Backend::Unsupported => "unsupported",
                },
            }),
            (None, Some(reason)) => serde_json::json!({
                "mode": "paced_reread",
                "source_key": LOCAL_OBSERVATION_INTERVAL_KEY,
                "source_origin": &self.observation.source,
                "interval_ms": self.observation.local_interval.as_micros() / 1000,
                "reason": reason,
            }),
            (None, None) => return extra,
        };
        extra.insert("observation".into(), value);
        extra
    }

    fn new_id_bytes() -> Result<[u8; 16], CoordinationError> {
        provenant_os::fs::entropy_128().map_err(CoordinationError::Entropy)
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
            let nonblocking_read = budget.is_nonblocking()
                && matches!(
                    (&error, &input),
                    (
                        CoordinationError::Timeout,
                        EventsInput::Read {
                            wait_seconds: Some(0),
                            ..
                        }
                    )
                );
            if !nonblocking_read {
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
                // Register before the first read so a commit during the query stays queued.
                let waiting = wait_seconds.is_some_and(|seconds| seconds > 0);
                let mut fallback_reason = None;
                let mut watcher = if waiting {
                    let paths = self.store.event_observation_paths();
                    match provenant_os::changes::watch::Watcher::subscribe(&paths) {
                        Ok(watcher) => Some(watcher),
                        Err(error) => {
                            fallback_reason = Some(error.to_string());
                            None
                        }
                    }
                } else {
                    None
                };
                let mut backend = watcher.as_ref().map(|watcher| watcher.backend());
                // fabric.md CLI surface: events use caller-held cursors.
                let start = after.unwrap_or(0);
                let mut page = self
                    .store
                    .events(self.project, start, limit, &read_request)?;
                let mut timed_out = false;
                while page.items.is_empty() && wait_seconds.is_some() {
                    if cancel.is_cancelled() {
                        return Err(CoordinationError::Cancelled);
                    }
                    let remaining = read_budget.remaining(self.clock.mono_now()).as_micros();
                    if remaining == 0 {
                        if waiting {
                            // The change signal is a hint. Re-read once at the deadline
                            // so a skipped/coalesced file event cannot hide a commit.
                            page = self
                                .store
                                .events(self.project, start, limit, &read_request)?;
                        }
                        timed_out = page.items.is_empty();
                        break;
                    }
                    if watcher.is_some() {
                        let slice = remaining.min(250_000);
                        let deadline = self
                            .clock
                            .mono_now()
                            .checked_add(DurationMicros::from_micros(slice))?;
                        let outcome = match watcher.as_mut() {
                            Some(active) => active.wait(std::time::Duration::from_micros(slice)),
                            None => continue,
                        };
                        match outcome {
                            Ok(provenant_os::changes::watch::WaitOutcome::Changed) => {
                                if cancel.is_cancelled() {
                                    return Err(CoordinationError::Cancelled);
                                }
                                if read_budget.remaining(self.clock.mono_now()).as_micros() == 0 {
                                    page = self.store.events(
                                        self.project,
                                        start,
                                        limit,
                                        &read_request,
                                    )?;
                                    timed_out = page.items.is_empty();
                                    break;
                                }
                                page =
                                    self.store
                                        .events(self.project, start, limit, &read_request)?;
                            }
                            Ok(provenant_os::changes::watch::WaitOutcome::TimedOut) => {
                                // Native timeout checks cancellation/deadline only; it
                                // never triggers a periodic database reread.
                                self.clock.wait_until(deadline)?;
                                if cancel.is_cancelled() {
                                    return Err(CoordinationError::Cancelled);
                                }
                            }
                            Ok(provenant_os::changes::watch::WaitOutcome::Woken) => {
                                if cancel.is_cancelled() {
                                    return Err(CoordinationError::Cancelled);
                                }
                            }
                            Err(error) => {
                                fallback_reason = Some(error.to_string());
                                watcher = None;
                                backend = None;
                            }
                        }
                    } else {
                        let interval = self.observation.local_interval.as_micros().min(remaining);
                        self.wait_fallback_interval(DurationMicros::from_micros(interval), cancel)?;
                        if cancel.is_cancelled() {
                            return Err(CoordinationError::Cancelled);
                        }
                        if read_budget.remaining(self.clock.mono_now()).as_micros() == 0 {
                            continue;
                        }
                        page = self
                            .store
                            .events(self.project, start, limit, &read_request)?;
                    }
                }
                let extra = if waiting {
                    self.observation_extra(backend, fallback_reason.as_deref())
                } else {
                    Default::default()
                };
                let page = bounded_page(page.items, page.truncated)?;
                let cursor = page.items.last().map(|event| event.seq).unwrap_or(start);
                Ok(EventsResult::Read {
                    extra,
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
        let request = self.request(cancel);
        let ledger = self.store.host_custody(&request)?;
        self.claim_at(&ledger, input, budget, cancel)
    }
}

impl LocalCoordination<'_> {
    fn claim_at(
        &self,
        ledger: &HostStore,
        input: ClaimInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<ClaimResult, CoordinationError> {
        input.validate_input(WaitTransport::Cli)?;
        self.ensure_active(budget, cancel)?;
        let call = self.request(cancel);
        if let ClaimInput::Renew { subject, .. } | ClaimInput::Release { subject, .. } = &input
            && let Some(record) = ledger.custody_item(self.project, subject, &call)?
            && record.state.kind() == provenant_records::custody::StateKind::Active
        {
            crate::custody::warning(format_args!(
                "{subject} is held by {:?}; advisory action by {}",
                record.holder, self.actor
            ));
        }
        Ok(match input {
            ClaimInput::Acquire {
                subject, holder, ..
            } => {
                if holder.actor() != Some(self.actor) {
                    return Err(CoordinationError::InvalidInput("custody holder actor"));
                }
                ClaimResult::Acquired {
                    extra: Default::default(),
                    change: Box::new(ledger.acquire_custody(
                        self.project,
                        &subject,
                        &holder,
                        Origin::Provenant,
                        &call,
                    )?),
                }
            }
            ClaimInput::Renew { subject, .. } => ClaimResult::Renewed {
                extra: Default::default(),
                record: Box::new(ledger.touch_custody(
                    self.project,
                    &subject,
                    self.actor,
                    &call,
                )?),
            },
            ClaimInput::Verify { subject, .. } => ClaimResult::Verified {
                extra: Default::default(),
                record: Box::new(
                    ledger
                        .custody_item(self.project, &subject, &call)?
                        .ok_or(provenant_fabric_store::Error::NotFound)?,
                ),
            },
            ClaimInput::Release { subject, .. } => ClaimResult::Released {
                extra: Default::default(),
                record: Box::new(ledger.change_custody_state(
                    self.project,
                    &subject,
                    CustodyState::Released,
                    Some(self.actor),
                    &call,
                )?),
            },
            ClaimInput::List { after, limit, .. } => ClaimResult::Listed {
                extra: Default::default(),
                page: ledger.custody(
                    &CustodyQuery {
                        project: Some(self.project),
                        after: after.unwrap_or(0),
                        limit,
                        ..Default::default()
                    },
                    &call,
                )?,
            },
        })
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
            Mutation::Claim(value) => {
                let ledger = self.store.host_custody(&request)?;
                ledger
                    .coordination_operation(self.project, self.actor, id, &frozen, &request, || {
                        self.claim_at(&ledger, value, budget, cancel)
                    })
                    .map(MutationResult::Claim)
            }
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
        fn observation_settings(&self) -> ObservationSettings {
            crate::test_support::observation_settings()
        }
        fn service(&self) -> LocalCoordination<'_> {
            let cancel = CancelToken::new();
            let request = Request {
                clock: &self.clock,
                cancel: &cancel,
            };
            let store = Store::open(&self.root, &request).expect("isolated store");
            LocalCoordination::new(
                store,
                self.project,
                self.actor,
                self.observation_settings(),
                &self.clock,
                &self.backoff,
            )
        }
        fn budget(&self) -> WaitBudget {
            WaitBudget::relay(self.clock.mono_now(), 5).expect("test relay budget")
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
    fn fallback_observation_preserves_the_declared_cadence_origin_and_reason() {
        let fixture = Fixture::new();
        let mut service = fixture.service();
        let interval_ms = 73;
        let user_path = provenant_records::PathWire::from_relative(
            &provenant_core::RelativePath::parse("config.toml").expect("fixture path"),
        )
        .expect("fixture wire path");
        for source in [
            fixture.observation_settings().source,
            provenant_config::Origin::User { path: user_path },
        ] {
            service.observation = ObservationSettings {
                local_interval: DurationMicros::from_micros(interval_ms * 1000),
                source: source.clone(),
            };
            let reason = "fixture event source unavailable";
            let extra = service.observation_extra(None, Some(reason));
            let observation = &extra["observation"];
            assert_eq!(observation["mode"], "paced_reread");
            assert_eq!(observation["source_key"], LOCAL_OBSERVATION_INTERVAL_KEY);
            assert_eq!(
                observation["source_origin"],
                serde_json::to_value(source).expect("origin")
            );
            assert_eq!(observation["interval_ms"], interval_ms);
            assert_eq!(observation["reason"], reason);
        }
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
            f.observation_settings(),
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
    fn custody_mutations_replay_from_the_host_ledger() {
        let fixture = Fixture::new();
        let service = fixture.service();
        let cancel = CancelToken::new();
        let subject = Subject::new(SubjectKind::Issue, "3078");
        let holder = Holder::Chair {
            chair: provenant_records::custody::Chair {
                extra: Default::default(),
                host: provenant_core::HostId::from_bytes([1; 16]),
                actor: fixture.actor,
                session: None,
                process: None,
            },
        };
        let acquire = Mutation::Claim(ClaimInput::Acquire {
            extra: Default::default(),
            subject: subject.clone(),
            holder,
        });
        let id = provenant_core::OperationId::parse("acquire").unwrap();
        let first = service
            .mutate(acquire.clone(), &id, fixture.budget(), &cancel)
            .unwrap();
        let replay = service
            .mutate(acquire, &id, fixture.budget(), &cancel)
            .unwrap();
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(replay).unwrap()
        );
        let release = Mutation::Claim(ClaimInput::Release {
            extra: Default::default(),
            subject,
        });
        let id = provenant_core::OperationId::parse("release").unwrap();
        let first = service
            .mutate(release.clone(), &id, fixture.budget(), &cancel)
            .unwrap();
        let replay = service
            .mutate(release, &id, fixture.budget(), &cancel)
            .unwrap();
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(replay).unwrap()
        );
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
    fn custody_holders_use_the_authenticated_actor() {
        let fixture = Fixture::new();
        let service = fixture.service();
        let cancel = CancelToken::new();
        let input = ClaimInput::Acquire {
            extra: Default::default(),
            subject: Subject::new(SubjectKind::Issue, "3078"),
            holder: Holder::Chair {
                chair: provenant_records::custody::Chair {
                    extra: Default::default(),
                    host: provenant_core::HostId::from_bytes([1; 16]),
                    actor: ActorId::from_bytes([99; 16]),
                    session: None,
                    process: None,
                },
            },
        };
        assert!(matches!(
            service.claim(input, fixture.budget(), &cancel),
            Err(CoordinationError::InvalidInput(_))
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
        let budget = || WaitBudget::relay(clock.mono_now(), 5).expect("budget");
        let service = LocalCoordination::open(
            &fixture.root,
            fixture.project,
            fixture.actor,
            fixture.observation_settings(),
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
        let EventsResult::Noted { seq, .. } = service
            .events(
                EventsInput::Note {
                    extra: Default::default(),
                    note: "zero wait observation".into(),
                    dedup_key: None,
                },
                budget(),
                &cancel,
            )
            .expect("append event")
        else {
            panic!("event note");
        };
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
        let EventsResult::Read {
            page,
            cursor: observed,
            timed_out: false,
            ..
        } = service
            .events(
                EventsInput::Read {
                    extra: Default::default(),
                    after: Some(cursor),
                    limit: 100,
                    wait_seconds: Some(0),
                },
                WaitBudget::nonblocking(clock.mono_now()),
                &cancel,
            )
            .expect("nonblocking observation")
        else {
            panic!("zero wait must observe once");
        };
        assert_eq!(page.items.last().map(|event| event.seq), Some(seq));
        assert_eq!(observed, seq);
        let before = clock.mono_now();
        assert!(matches!(
            service.events(
                EventsInput::Read {
                    extra: Default::default(),
                    after: Some(seq),
                    limit: 100,
                    wait_seconds: Some(0),
                },
                WaitBudget::nonblocking(before),
                &cancel
            ),
            Ok(EventsResult::Read {
                page: Page { items, .. },
                timed_out: true,
                ..
            }) if items.is_empty()
        ));
        assert_eq!(clock.mono_now(), before);
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
