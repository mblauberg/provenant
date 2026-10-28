//! Local human inbox service, shared by CLI and MCP. No provider or app dependency.
use crate::{
    coordination::{
        CoordinationError,
        input::{InputError, ValidateInput},
    },
    identity::Identity,
};
use provenant_core::{
    CancelToken, Clock, HumanAnswerId, HumanRequestId, MessageId, OperationId, StateRoot,
    WaitBudget, WaitTransport,
};
use provenant_fabric_store::{Request, Store, ThreadWait};
use provenant_records::{CanonicalV1, RecordHeader, inbox::*};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum InboxInput {
    Ask {
        session: String,
        request: Box<RequestDraft>,
        operation_id: Option<OperationId>,
    },
    List {
        id: Option<HumanRequestId>,
        session: Option<String>,
        #[serde(default)]
        state: InboxFilter,
        #[serde(default = "default_limit")]
        limit: u64,
        cursor: Option<InboxCursor>,
    },
    Answer {
        id: HumanRequestId,
        action: AnswerAction,
        option_id: Option<String>,
        text: Option<String>,
        expect_revision: u64,
        channel: InteractionChannel,
        operation_id: Option<OperationId>,
    },
    Close {
        id: HumanRequestId,
        state: RequestState,
        reason: String,
        expect_revision: u64,
        operation_id: Option<OperationId>,
    },
    Acknowledge {
        id: HumanRequestId,
        answer_id: HumanAnswerId,
        session: String,
        operation_id: Option<OperationId>,
    },
}
fn default_limit() -> u64 {
    50
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxFilter {
    #[default]
    Open,
    All,
}

/// Filters and the current request snapshot are bound into the cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InboxCursor {
    pub after: HumanRequestId,
    pub scope: provenant_core::Digest,
    pub snapshot: u64,
}
impl TryFrom<String> for InboxCursor {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let mut parts = value.split('.');
        let after = parts
            .next()
            .and_then(|v| HumanRequestId::parse(v).ok())
            .ok_or("invalid inbox cursor")?;
        let scope = parts
            .next()
            .and_then(|v| provenant_core::Digest::parse(v).ok())
            .ok_or("invalid inbox cursor")?;
        let snapshot = parts
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or("invalid inbox cursor")?;
        if parts.next().is_some() {
            return Err("invalid inbox cursor");
        }
        Ok(Self {
            after,
            scope,
            snapshot,
        })
    }
}
impl From<InboxCursor> for String {
    fn from(value: InboxCursor) -> Self {
        format!("{}.{}.{}", value.after, value.scope, value.snapshot)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxReceipt {
    pub operation_id: OperationId,
    pub request_id: HumanRequestId,
    pub revision: u64,
    pub state: RequestState,
    pub answer_id: Option<HumanAnswerId>,
    pub delivery: Option<AnswerDelivery>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum InboxResult {
    Saved {
        #[serde(flatten)]
        receipt: InboxReceipt,
    },
    Item {
        item: Box<InboxItem>,
    },
    Listed {
        items: Vec<HumanRequest>,
        next_cursor: Option<InboxCursor>,
        truncated: bool,
        coverage: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error("inbox operation {operation_id}: {source}")]
    Mutation {
        operation_id: OperationId,
        #[source]
        source: Box<InboxError>,
    },
    #[error("invalid inbox field: {0}")]
    Invalid(&'static str),
    #[error("operation ID conflicts with an earlier inbox operation")]
    OperationConflict,
    #[error("request revision or delivery target conflicts with current state")]
    Conflict,
    #[error("inbox cursor expired or filters changed; restart without --cursor")]
    CursorExpired,
    #[error("inbox operation is not available inside a worker lane")]
    LaneMutation,
    #[error("human inbox federation is not implemented; use the owning host directly")]
    UnsupportedOwner,
    #[error("human inbox owner is unavailable in this build")]
    OwnerUnavailable,
    #[error(
        "project has reached the 10000-request retention limit; no history or pending delivery was pruned"
    )]
    RequestLimit,
    #[error(transparent)]
    Store(provenant_fabric_store::Error),
    #[error(transparent)]
    Coordination(#[from] CoordinationError),
}

/// Shared edge entry: a remote Fabric home never falls back to a local mirror.
#[allow(clippy::too_many_arguments)]
pub fn execute(
    root: &StateRoot,
    identity: &Identity,
    config: &crate::config::FabricConfig,
    in_lane: bool,
    clock: &dyn Clock,
    input: InboxInput,
    budget: WaitBudget,
    cancel: &CancelToken,
    work_lookup: &WorkLookup<'_>,
) -> Result<InboxResult, InboxError> {
    validate_request(identity, config, in_lane, &input)?;
    InboxService::open(root, identity, clock, budget, cancel)?
        .with_work_lookup(work_lookup)
        .execute(input, budget, cancel)
}

/// Context-only validation runs before either transport opens a coordination store.
pub fn validate_request(
    identity: &Identity,
    config: &crate::config::FabricConfig,
    in_lane: bool,
    input: &InboxInput,
) -> Result<(), InboxError> {
    input.validate()?;
    validate_edge(identity, config, in_lane, input.is_mutation())?;
    if let InboxInput::List { limit, .. } = input
        && *limit > config.limits.page_rows
    {
        return Err(InboxError::Invalid("limit"));
    }
    let body_limit = usize::try_from(config.limits.body_bytes).unwrap_or(usize::MAX);
    match input {
        InboxInput::Ask { request, .. } if request.body.len() > body_limit => {
            return Err(InboxError::Invalid("body"));
        }
        InboxInput::Answer {
            text: Some(text), ..
        } if text.len() > body_limit => return Err(InboxError::Invalid("text")),
        InboxInput::Close { reason, .. } if reason.len() > body_limit => {
            return Err(InboxError::Invalid("reason"));
        }
        _ => {}
    }
    Ok(())
}

fn validate_edge(
    identity: &Identity,
    config: &crate::config::FabricConfig,
    in_lane: bool,
    mutation: bool,
) -> Result<(), InboxError> {
    if in_lane && mutation {
        return Err(InboxError::LaneMutation);
    }
    if identity
        .federation
        .as_ref()
        .and_then(|project| config.home_hosts.get(project.key.as_str()))
        .is_some_and(|host| *host != identity.host)
    {
        return Err(InboxError::UnsupportedOwner);
    }
    Ok(())
}

impl From<provenant_fabric_store::Error> for InboxError {
    fn from(error: provenant_fabric_store::Error) -> Self {
        match error {
            provenant_fabric_store::Error::Conflict => Self::Conflict,
            provenant_fabric_store::Error::Invalid("deadline") => Self::Invalid("deadline"),
            provenant_fabric_store::Error::Invalid(
                "operation ID reused with a different request",
            ) => Self::OperationConflict,
            provenant_fabric_store::Error::Invalid("human request retention limit") => {
                Self::RequestLimit
            }
            other => Self::Store(other),
        }
    }
}
impl InboxError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Mutation { source, .. } => source.code(),
            Self::Invalid(_) => "invalid_input",
            Self::OperationConflict => "operation_conflict",
            Self::Conflict => "conflict",
            Self::CursorExpired => "cursor_expired",
            Self::LaneMutation => "lane_coordination_refused",
            Self::UnsupportedOwner => "unsupported_owner",
            Self::RequestLimit => "request_limit",
            Self::Store(provenant_fabric_store::Error::NotFound) => "not_found",
            Self::Store(provenant_fabric_store::Error::Busy) => "busy",
            Self::Store(provenant_fabric_store::Error::Cancelled) => "cancelled",
            _ => "inbox_unavailable",
        }
    }
}
impl InboxInput {
    pub fn is_mutation(&self) -> bool {
        !matches!(self, Self::List { .. })
    }
    fn operation_mut(&mut self) -> Option<&mut Option<OperationId>> {
        match self {
            Self::Ask { operation_id, .. }
            | Self::Answer { operation_id, .. }
            | Self::Close { operation_id, .. }
            | Self::Acknowledge { operation_id, .. } => Some(operation_id),
            _ => None,
        }
    }
    /// Structural bounds are shared with MCP's schema. Relationships are checked
    /// by the service, where the authoritative request is available.
    fn validate_fields(&self) -> Result<(), InboxError> {
        match self {
            Self::Ask {
                session, request, ..
            } => {
                session_id(session)?;
                if let Some(work) = &request.work
                    && work.repository.as_ref().is_some_and(|repo| {
                        repo.to_path_buf()
                            .map_or(true, |path| path.as_os_str().len() > 4097)
                    })
                {
                    return Err(InboxError::Invalid("work.repository"));
                }
                if request
                    .supersedes
                    .as_ref()
                    .is_some_and(|replacement| replacement.expect_revision == 0)
                {
                    return Err(InboxError::Invalid("supersedes.expect_revision"));
                }
                bounded(&request.title, 256, "title")?;
                bounded(&request.body, REQUEST_BODY_BYTES, "body")?;
                if request.options.len() > MAX_OPTIONS {
                    return Err(InboxError::Invalid("options"));
                }
                for option in &request.options {
                    OperationId::parse(&option.id)
                        .map_err(|_| InboxError::Invalid("options.id"))?;
                    bounded(&option.label, 256, "options.label")?;
                    bounded(&option.description, 1024, "options.description")?;
                }
                if let Some(id) = &request.recommended_option_id {
                    OperationId::parse(id)
                        .map_err(|_| InboxError::Invalid("recommended_option_id"))?;
                }
                if let Some(text) = &request.proceeding_with {
                    bounded(text, 1024, "proceeding_with")?;
                }
            }
            Self::List { session, limit, .. } => {
                if let Some(session) = session {
                    session_id(session)?;
                }
                if !(1..=100).contains(limit) {
                    return Err(InboxError::Invalid("limit"));
                }
            }
            Self::Answer {
                option_id,
                text,
                expect_revision,
                ..
            } => {
                if *expect_revision == 0 {
                    return Err(InboxError::Invalid("expect_revision"));
                }
                if let Some(text) = text {
                    bounded(text, ANSWER_TEXT_BYTES, "text")?;
                }
                if let Some(option) = option_id {
                    OperationId::parse(option).map_err(|_| InboxError::Invalid("option_id"))?;
                }
            }
            Self::Close {
                state,
                reason,
                expect_revision,
                ..
            } => {
                if !matches!(
                    state,
                    RequestState::Withdrawn | RequestState::Superseded | RequestState::Expired
                ) || *expect_revision == 0
                {
                    return Err(InboxError::Invalid("closure state or revision"));
                }
                bounded(reason, 1024, "reason")?;
            }
            Self::Acknowledge { session, .. } => session_id(session)?,
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), InboxError> {
        self.validate_fields()?;
        match self {
            Self::Ask { request, .. } => {
                let ids = request
                    .options
                    .iter()
                    .map(|o| &o.id)
                    .collect::<std::collections::BTreeSet<_>>();
                if ids.len() != request.options.len() {
                    return Err(InboxError::Invalid("options.id"));
                }
                if request
                    .recommended_option_id
                    .as_ref()
                    .is_some_and(|id| !ids.contains(id))
                {
                    return Err(InboxError::Invalid("recommended_option_id"));
                }
                if (request.kind == RequestKind::Assumption) != request.proceeding_with.is_some() {
                    return Err(InboxError::Invalid("proceeding_with"));
                }
                if request.blocking && request.kind != RequestKind::Decision {
                    return Err(InboxError::Invalid("blocking"));
                }
                if request.title.trim().is_empty()
                    || request.body.trim().is_empty()
                    || request
                        .proceeding_with
                        .as_ref()
                        .is_some_and(|s| s.trim().is_empty())
                {
                    return Err(InboxError::Invalid("blank request content"));
                }
            }
            Self::List {
                id,
                session,
                cursor,
                ..
            } if id.is_some() && (session.is_some() || cursor.is_some()) => {
                return Err(InboxError::Invalid("list filters"));
            }
            Self::Answer {
                action,
                option_id,
                text,
                ..
            } => {
                if text.as_ref().is_some_and(|text| text.trim().is_empty()) {
                    return Err(InboxError::Invalid("blank answer"));
                }
                match action {
                    AnswerAction::Choose if option_id.is_some() && text.is_none() => {}
                    AnswerAction::Reply | AnswerAction::Override
                        if text.is_some() && option_id.is_none() => {}
                    AnswerAction::Confirm if text.is_none() && option_id.is_none() => {}
                    _ => return Err(InboxError::Invalid("answer action/content")),
                }
            }
            Self::Close { reason, .. } if reason.trim().is_empty() => {
                return Err(InboxError::Invalid("reason"));
            }
            _ => {}
        }
        Ok(())
    }
}
impl ValidateInput for InboxInput {
    fn validate_input(&self, _: WaitTransport) -> Result<(), InputError> {
        self.validate_fields().map_err(|error| InputError {
            path: match error {
                InboxError::Invalid(field) => field,
                _ => "inbox",
            }
            .into(),
        })
    }
}
fn bounded(text: &str, max: usize, field: &'static str) -> Result<(), InboxError> {
    if text.is_empty() || text.len() > max || text.contains('\0') {
        return Err(InboxError::Invalid(field));
    }
    Ok(())
}
fn session_id(id: &str) -> Result<(), InboxError> {
    if id.is_empty()
        || id.len() > 128
        || id.contains("..")
        || !id
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    {
        return Err(InboxError::Invalid("session"));
    }
    Ok(())
}

/// Resolve the registered chair at the adapter edge before opening the store.
/// The resolver must consult authoritative session discovery and fail closed.
#[allow(clippy::too_many_arguments)]
pub fn offer_until_ack(
    root: &StateRoot,
    identity: &Identity,
    config: &crate::config::FabricConfig,
    in_lane: bool,
    clock: &dyn Clock,
    session: &str,
    limit: u64,
    max_bytes: usize,
    budget: WaitBudget,
    cancel: &CancelToken,
    resolve: impl FnOnce() -> Result<AnswerTarget, InboxError>,
) -> Result<provenant_fabric_store::Page<AnswerOffer>, InboxError> {
    session_id(session)?;
    validate_edge(identity, config, in_lane, true)?;
    let target = resolve()?;
    if target.host != identity.host
        || target.project != identity.project
        || target.actor != identity.inbox.actor
        || target.session != session
    {
        return Err(InboxError::Conflict);
    }
    let service = InboxService::open(root, identity, clock, budget, cancel)?;
    let request = Request {
        clock,
        budget,
        cancel,
        backoff: &ThreadWait,
    };
    Ok(service.store.offer_human_answers(
        &target,
        limit,
        max_bytes,
        clock.wall_now().map_err(CoordinationError::from)?,
        &request,
    )?)
}

/// Supplied by the host composition edge; callers cannot supply coverage.
pub type WorkLookup<'a> =
    dyn Fn(&RequestWork, &Request<'_>) -> Result<WorkCoverage, InboxError> + 'a;

pub struct InboxService<'a> {
    store: Store,
    work_lookup: Option<&'a WorkLookup<'a>>,
    identity: &'a Identity,
    clock: &'a dyn Clock,
}
impl<'a> InboxService<'a> {
    pub fn open(
        root: &StateRoot,
        identity: &'a Identity,
        clock: &'a dyn Clock,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Self, InboxError> {
        let request = Request {
            clock,
            budget,
            cancel,
            backoff: &ThreadWait,
        };
        Ok(Self {
            store: Store::open(root, &request)?,
            work_lookup: None,
            identity,
            clock,
        })
    }

    pub fn with_work_lookup(mut self, lookup: &'a WorkLookup<'a>) -> Self {
        self.work_lookup = Some(lookup);
        self
    }

    pub fn execute(
        &self,
        mut input: InboxInput,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<InboxResult, InboxError> {
        input.validate()?;
        let request = Request {
            clock: self.clock,
            budget,
            cancel,
            backoff: &ThreadWait,
        };
        request.check()?;
        if let InboxInput::List {
            id,
            session,
            state,
            limit,
            cursor,
        } = input
        {
            return self.list(id, session, state, limit, cursor, &request);
        }
        let operation = input
            .operation_mut()
            .ok_or(InboxError::Invalid("operation_id"))?;
        let id = match operation {
            Some(id) => id.clone(),
            None => OperationId::parse(&provenant_core::hex(&entropy()?))
                .map_err(|_| InboxError::Invalid("operation_id"))?,
        };
        *operation = Some(id.clone());
        self.store
            .coordination_operation(
                self.identity.project,
                self.identity.inbox.actor,
                &id,
                &("human_inbox_v1", &input),
                &request,
                || self.mutate(&input, &id, &request),
            )
            .map_err(|source| InboxError::Mutation {
                operation_id: id,
                source: Box::new(source),
            })
    }

    fn list(
        &self,
        id: Option<HumanRequestId>,
        session: Option<String>,
        state: InboxFilter,
        limit: u64,
        cursor: Option<InboxCursor>,
        request: &Request<'_>,
    ) -> Result<InboxResult, InboxError> {
        if let Some(id) = id {
            return Ok(InboxResult::Item {
                item: Box::new(self.store.human_item(self.identity.project, id, request)?),
            });
        }
        let scope = CanonicalV1::record_digest(
            "human_inbox_cursor",
            1,
            &(
                self.identity.host,
                self.identity.project,
                &session,
                state,
                limit,
            ),
        )
        .map_err(provenant_fabric_store::Error::from)?;
        let snapshot = self
            .store
            .human_generation(self.identity.project, request)?;
        if cursor
            .as_ref()
            .is_some_and(|c| c.scope != scope || c.snapshot != snapshot)
        {
            return Err(InboxError::CursorExpired);
        }
        let page = self.store.human_requests(
            self.identity.project,
            session.as_deref(),
            state == InboxFilter::All,
            cursor.as_ref().map(|c| c.after),
            limit,
            request,
        )?;
        if snapshot
            != self
                .store
                .human_generation(self.identity.project, request)?
        {
            return Err(InboxError::CursorExpired);
        }
        let next_cursor = if page.truncated {
            page.items.last().map(|value| InboxCursor {
                after: value.id,
                scope,
                snapshot,
            })
        } else {
            None
        };
        Ok(InboxResult::Listed {
            items: page.items,
            next_cursor,
            truncated: page.truncated,
            coverage: "local_only; peer inboxes and unregistered sessions are unknown".into(),
        })
    }

    fn mutate(
        &self,
        input: &InboxInput,
        operation: &OperationId,
        request: &Request<'_>,
    ) -> Result<InboxResult, InboxError> {
        let at = self.clock.wall_now().map_err(CoordinationError::from)?;
        let timestamp = at;
        let (value, delivery) = match input {
            InboxInput::Ask {
                session,
                request: draft,
                ..
            } => {
                let work_coverage = draft
                    .work
                    .as_ref()
                    .map(|work| {
                        let coverage =
                            self.work_lookup.ok_or(InboxError::OwnerUnavailable)?(work, request)?;
                        if coverage == WorkCoverage::Mismatch {
                            return Err(InboxError::Invalid("work repository or head mismatch"));
                        }
                        Ok(coverage)
                    })
                    .transpose()?;
                let value = HumanRequest {
                    schema_version: 1,
                    id: HumanRequestId::from_bytes(entropy()?),
                    revision: 1,
                    operation_id: operation.clone(),
                    created_at: timestamp,
                    updated_at: timestamp,
                    project_id: self.identity.project,
                    project_key: self
                        .identity
                        .federation
                        .as_ref()
                        .map(|p| p.key.as_str().into()),
                    owner_host: self.identity.host,
                    chair_session_id: session.clone(),
                    chair_actor: self.identity.inbox.actor,
                    request: draft.as_ref().clone(),
                    work_coverage,
                    state: RequestState::Open,
                    answer_id: None,
                    closure_reason: None,
                };
                self.store.create_human_request(&value, request)?;
                (value, None)
            }
            InboxInput::Answer {
                id,
                action,
                option_id,
                text,
                expect_revision,
                channel,
                ..
            } => {
                let value = self
                    .store
                    .human_request(self.identity.project, *id, request)?;
                if value.owner_host != self.identity.host {
                    return Err(InboxError::UnsupportedOwner);
                }
                let assumption = value.request.kind == RequestKind::Assumption;
                if assumption != matches!(action, AnswerAction::Confirm | AnswerAction::Override) {
                    return Err(InboxError::Invalid("action for request kind"));
                }
                if option_id
                    .as_ref()
                    .is_some_and(|id| !value.request.options.iter().any(|option| option.id == *id))
                {
                    return Err(InboxError::Invalid("option_id"));
                }
                let answer = HumanAnswer {
                    schema_version: 1,
                    id: HumanAnswerId::from_bytes(entropy()?),
                    request_id: *id,
                    request_revision: *expect_revision,
                    action: *action,
                    option_id: option_id.clone(),
                    text: text.clone(),
                    actor: self.identity.inbox.actor,
                    channel: *channel,
                    answered_at: timestamp,
                    operation_id: operation.clone(),
                };
                let delivery = AnswerDelivery {
                    schema_version: 1,
                    answer_id: answer.id,
                    target_host: value.owner_host,
                    target_session: value.chair_session_id.clone(),
                    target_actor: value.chair_actor,
                    transport: AnswerTransport::FabricMail,
                    message_id: MessageId::from_bytes(entropy()?),
                    state: AnswerDeliveryState::Queued,
                    attempts: 0,
                    last_attempt_at: None,
                    acknowledged_at: None,
                    error: None,
                };
                // Reserve the largest possible attempt count in the wire envelope.
                let mut offered = delivery.clone();
                offered.state = AnswerDeliveryState::Offered;
                offered.attempts = u64::MAX;
                offered.last_attempt_at = Some(timestamp);
                let page = provenant_fabric_store::Page {
                    items: vec![AnswerOffer {
                        answer: answer.clone(),
                        delivery: offered,
                    }],
                    truncated: false,
                };
                if CanonicalV1::bytes(&page)
                    .map_err(provenant_fabric_store::Error::from)?
                    .as_bytes()
                    .len()
                    > MAX_OFFER_BYTES
                {
                    return Err(InboxError::Invalid("answer exceeds delivery byte limit"));
                }
                let body=serde_json::to_string(&serde_json::json!({"schema_version":1,"kind":"human_answer","request_id":id,"target_host":value.owner_host,"target_session":value.chair_session_id,"target_actor":value.chair_actor,"answer":answer,"authority":"This answer is scoped to its request; it is not a review receipt or new authority. Acknowledge this answer ID after consuming it once."})).map_err(CoordinationError::ResponseEncoding)?;
                let message = provenant_fabric_store::Message {
                    header: RecordHeader::V1,
                    project: self.identity.project,
                    id: delivery.message_id,
                    sender: self.identity.inbox.actor,
                    recipients: vec![value.chair_actor],
                    conversation: None,
                    kind: provenant_fabric_store::MessageKind::Response,
                    body,
                    artifacts: vec![],
                    reply_to: None,
                    task: None,
                    created_at: at,
                };
                (
                    self.store
                        .answer_human_request(&answer, &delivery, &message, request)?,
                    Some(delivery),
                )
            }
            InboxInput::Close {
                id,
                state,
                reason,
                expect_revision,
                ..
            } => (
                self.store.close_human_request(
                    provenant_fabric_store::HumanClosure {
                        project: self.identity.project,
                        id: *id,
                        actor: self.identity.inbox.actor,
                        revision: *expect_revision,
                        state: *state,
                        reason,
                        at: timestamp,
                    },
                    request,
                )?,
                None,
            ),
            InboxInput::Acknowledge {
                id,
                answer_id,
                session,
                ..
            } => {
                let delivery = self.store.acknowledge_human_answer(
                    provenant_fabric_store::HumanAcknowledgement {
                        project: self.identity.project,
                        id: *id,
                        answer: *answer_id,
                        host: self.identity.host,
                        actor: self.identity.inbox.actor,
                        session,
                        at: timestamp,
                    },
                    request,
                )?;
                (
                    self.store
                        .human_request(self.identity.project, *id, request)?,
                    Some(delivery),
                )
            }
            InboxInput::List { .. } => return Err(InboxError::Invalid("mutation")),
        };
        Ok(InboxResult::Saved {
            receipt: InboxReceipt {
                operation_id: operation.clone(),
                request_id: value.id,
                revision: value.revision,
                state: value.state_at(at),
                answer_id: value.answer_id,
                delivery,
            },
        })
    }
}

fn entropy() -> Result<[u8; 16], InboxError> {
    provenant_os::fs::entropy_128()
        .map_err(CoordinationError::Entropy)
        .map_err(Into::into)
}

#[cfg(test)]
mod tests;
