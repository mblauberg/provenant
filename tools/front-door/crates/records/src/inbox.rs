//! Provider-neutral human requests. These records confer no workflow authority.
use provenant_core::{
    ActorId, AttemptId, GitOid, HostId, HumanAnswerId, HumanRequestId, OperationId, ProjectId,
    RunId, TaskId, WallTime,
};
use serde::{Deserialize, Serialize};

pub const REQUEST_BODY_BYTES: usize = 8192;
pub const ANSWER_TEXT_BYTES: usize = 4096;
pub const REQUEST_REPOSITORY_BYTES: usize = 4096;
pub const MAX_OPTIONS: usize = 16;
pub const MAX_OFFER_BYTES: usize = 8192;

/// Exact registered destination. An actor label alone never selects a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerTarget {
    pub host: HostId,
    pub project: ProjectId,
    pub session: String,
    pub actor: ActorId,
}

/// A local service offer is not proof of provider injection or consumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerOffer {
    pub answer: HumanAnswer,
    pub delivery: AnswerDelivery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DeliveryCapability {
    pub provider: crate::Harness,
    pub transport: AnswerTransport,
    pub live_injection: bool,
    pub local_offer_reader: bool,
}

/// Implemented local fallback only. Provider hook/input adapters are separate work.
pub fn delivery_capabilities() -> Vec<DeliveryCapability> {
    crate::ADAPTER_SPECS
        .iter()
        .map(|spec| DeliveryCapability {
            provider: spec.id,
            transport: AnswerTransport::FabricMail,
            live_injection: false,
            local_offer_reader: false,
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Decision,
    Assumption,
    Question,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Open,
    Answered,
    Confirmed,
    Overridden,
    Withdrawn,
    Superseded,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerAction {
    Choose,
    Reply,
    Confirm,
    Override,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionChannel {
    Chat,
    Console,
    PhoneRemote,
    Cli,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestOption {
    pub id: String,
    pub label: String,
    pub description: String,
}

/// Claimed links. Repository is a canonical Git common directory, not a checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestWork {
    pub run_id: RunId,
    pub task_id: Option<TaskId>,
    pub attempt_id: Option<AttemptId>,
    pub repository: Option<crate::AbsolutePath>,
    pub head: Option<GitOid>,
}

/// Coverage of the creation-time host lookup, never inferred from caller claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkCoverage {
    Unindexed,
    Verified,
    Partial,
    HeadUnavailable,
    RepositoryUnavailable,
    Mismatch,
    Limit,
}

/// Explicit replacement of a viewed request revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestReplacement {
    pub id: HumanRequestId,
    pub expect_revision: u64,
}

/// Caller-authored content only. The service binds project, host and actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestDraft {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<RequestReplacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<WallTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work: Option<RequestWork>,
    pub kind: RequestKind,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default)]
    pub options: Vec<RequestOption>,
    pub recommended_option_id: Option<String>,
    pub proceeding_with: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanRequest {
    pub schema_version: u64,
    pub id: HumanRequestId,
    pub revision: u64,
    pub operation_id: OperationId,
    /// RFC 3339 UTC, including a trailing Z.
    pub created_at: WallTime,
    pub updated_at: WallTime,
    pub project_id: ProjectId,
    pub project_key: Option<String>,
    pub owner_host: HostId,
    pub chair_session_id: String,
    pub chair_actor: ActorId,
    pub request: RequestDraft,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_coverage: Option<WorkCoverage>,
    pub state: RequestState,
    pub answer_id: Option<HumanAnswerId>,
    pub closure_reason: Option<String>,
}

impl HumanRequest {
    /// Expiry is a read-time projection of unresolved work, never an answer.
    pub fn state_at(&self, at: WallTime) -> RequestState {
        if self.state == RequestState::Open && self.request.deadline.is_some_and(|time| time <= at)
        {
            RequestState::Expired
        } else {
            self.state
        }
    }

    /// `request.work` is always a claim. Only this accessor exposes a verified
    /// creation-preparation snapshot; other coverage must never become work links.
    pub fn verified_work(&self) -> Option<&RequestWork> {
        (self.work_coverage == Some(WorkCoverage::Verified))
            .then_some(self.request.work.as_ref())
            .flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanAnswer {
    pub schema_version: u64,
    pub id: HumanAnswerId,
    pub request_id: HumanRequestId,
    pub request_revision: u64,
    pub action: AnswerAction,
    pub option_id: Option<String>,
    /// Exact submitted text, including whitespace.
    pub text: Option<String>,
    pub actor: ActorId,
    pub channel: InteractionChannel,
    pub answered_at: WallTime,
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerDeliveryState {
    Queued,
    Offered,
    Acknowledged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerTransport {
    FabricMail,
    ClaudeHook,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerDelivery {
    pub schema_version: u64,
    pub answer_id: HumanAnswerId,
    pub target_host: HostId,
    pub target_session: String,
    pub target_actor: ActorId,
    /// Fabric mail is not live provider injection.
    pub transport: AnswerTransport,
    pub message_id: provenant_core::MessageId,
    pub state: AnswerDeliveryState,
    pub attempts: u64,
    pub last_attempt_at: Option<WallTime>,
    pub acknowledged_at: Option<WallTime>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxItem {
    pub request: HumanRequest,
    pub answer: Option<HumanAnswer>,
    pub delivery: Option<AnswerDelivery>,
    /// Immutable request revisions, oldest first. No silent editing after answer.
    pub history: Vec<HumanRequest>,
}
