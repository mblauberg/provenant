//! Host-verified adoption of an existing pull request into a delivery item.
use super::{DeliveryCiAdmission, DeliveryCiAdmissionWait, DeliveryPublication, NodeProgress};
use serde::{Deserialize, Serialize};

/// Untrusted resolver facts attached to a queue-only publication seed.
///
/// The host must revalidate these facts against the authenticated queue item,
/// live PR and retained worktree before it returns an adoption receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryAdoptionIntent {
    pub operation_id: String,
    pub project: String,
    pub tracker: String,
    pub item: String,
    pub repository: String,
    pub issue_identity: String,
    pub issue_number: u64,
    pub pull_request: u64,
    pub pull_request_url: String,
    pub branch: String,
    pub worktree: String,
    pub source_directory_identity: provenant_records::run::SourceIdentity,
    pub custody_host: String,
    pub custody_attempt: String,
    pub custody_operation: String,
    pub head: String,
    pub base_repository: String,
    pub base_branch: String,
    pub base_head: String,
}

impl DeliveryAdoptionIntent {
    pub(super) fn valid(&self) -> bool {
        let sha256 = |value: &str| {
            value.strip_prefix("sha256:").is_some_and(|digest| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        };
        let nonempty = |value: &str| !value.trim().is_empty() && value.trim() == value;
        self.operation_id == self.operation_id.to_ascii_lowercase()
            && sha256(&self.operation_id)
            && [
                &self.project,
                &self.tracker,
                &self.item,
                &self.repository,
                &self.issue_identity,
                &self.pull_request_url,
                &self.branch,
                &self.worktree,
                &self.custody_host,
                &self.custody_attempt,
                &self.custody_operation,
                &self.base_repository,
                &self.base_branch,
            ]
            .into_iter()
            .all(|value| nonempty(value) && !value.chars().any(char::is_control))
            && self.issue_number > 0
            && self.pull_request > 0
            && self.pull_request_url.starts_with("https://")
            && self.worktree.starts_with('/')
            && self.head.parse::<provenant_core::GitOid>().is_ok()
            && self.base_head.parse::<provenant_core::GitOid>().is_ok()
            && self.repository.eq_ignore_ascii_case(&self.base_repository)
    }

    pub(super) fn receipt_matches(&self, receipt: &DeliveryPublication) -> bool {
        receipt.branch == self.branch
            && receipt.head == self.head
            && receipt.pull_request == self.pull_request_url
    }
}

/// Durable host receipt and CI-class lease for an adopted PR node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryAdoptionProgress {
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_admission: Option<DeliveryCiAdmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_admission_wait: Option<DeliveryCiAdmissionWait>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<DeliveryPublication>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DeliveryAdoptionProgress {
    pub(super) fn matches_intent(&self, intent: &DeliveryAdoptionIntent) -> bool {
        self.operation_id == intent.operation_id
            && self
                .receipt
                .as_ref()
                .is_none_or(|receipt| intent.receipt_matches(receipt))
    }
}

/// The host request to revalidate and durably adopt one queue item's PR.
pub struct DeliveryAdoptionRequest<'a> {
    pub graph_id: &'a str,
    pub node_id: &'a str,
    pub intent: &'a DeliveryAdoptionIntent,
}

impl NodeProgress {
    /// Read the typed, host-verified adoption progress from node progress.
    pub fn delivery_adoption(&self) -> Option<DeliveryAdoptionProgress> {
        self.delivery_adoption.clone()
    }

    pub(super) fn has_delivery_adoption(&self) -> bool {
        self.delivery_adoption.is_some()
    }

    /// Persist adoption progress without conflating it with a newly published
    /// writer receipt.
    pub fn set_delivery_adoption(&mut self, progress: &DeliveryAdoptionProgress) {
        self.delivery_adoption = Some(progress.clone());
    }
}
