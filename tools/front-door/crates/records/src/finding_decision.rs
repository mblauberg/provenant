//! The decision on one review finding, bound to the change it reviewed.
use provenant_core::GitOid;
use serde::{Deserialize, Serialize};

pub const FINDING_ID_BYTES: usize = 200;
pub const DECISION_REASON_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingDisposition {
    /// Repair the finding as reported.
    Act,
    /// Leave the finding unrepaired.
    Decline,
    /// Replace the reported repair with a larger fix, named in the reason.
    Replace,
}

/// A review covers the change between `reviewed_base` and `reviewed_head`,
/// not a head commit alone: a later head carries the review over only when
/// its delta from its own base is unchanged (#3132). For a stacked branch
/// the base is the head of the branch below.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "FindingDecisionWire")]
pub struct FindingDecision {
    finding: String,
    disposition: FindingDisposition,
    reason: String,
    reviewed_head: GitOid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reviewed_base: Option<GitOid>,
    #[serde(flatten)]
    extra: crate::ExtraFields,
}

#[derive(Deserialize)]
struct FindingDecisionWire {
    finding: String,
    disposition: FindingDisposition,
    reason: String,
    reviewed_head: GitOid,
    #[serde(default)]
    reviewed_base: Option<GitOid>,
    #[serde(flatten)]
    extra: crate::ExtraFields,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FindingDecisionError {
    #[error("finding identifier must be 1..=200 bytes")]
    Finding,
    #[error("decision reason must be 1..=4096 bytes")]
    Reason,
}

impl FindingDecision {
    pub fn new(
        finding: impl Into<String>,
        disposition: FindingDisposition,
        reason: impl Into<String>,
        reviewed_head: GitOid,
        reviewed_base: Option<GitOid>,
    ) -> Result<Self, FindingDecisionError> {
        FindingDecisionWire {
            finding: finding.into(),
            disposition,
            reason: reason.into(),
            reviewed_head,
            reviewed_base,
            extra: Default::default(),
        }
        .try_into()
    }

    pub fn finding(&self) -> &str {
        &self.finding
    }

    pub fn disposition(&self) -> FindingDisposition {
        self.disposition
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn reviewed_head(&self) -> GitOid {
        self.reviewed_head
    }

    pub fn reviewed_base(&self) -> Option<GitOid> {
        self.reviewed_base
    }
}

impl TryFrom<FindingDecisionWire> for FindingDecision {
    type Error = FindingDecisionError;

    fn try_from(wire: FindingDecisionWire) -> Result<Self, Self::Error> {
        if wire.finding.trim().is_empty() || wire.finding.len() > FINDING_ID_BYTES {
            return Err(FindingDecisionError::Finding);
        }
        if wire.reason.trim().is_empty() || wire.reason.len() > DECISION_REASON_BYTES {
            return Err(FindingDecisionError::Reason);
        }
        Ok(Self {
            finding: wire.finding,
            disposition: wire.disposition,
            reason: wire.reason,
            reviewed_head: wire.reviewed_head,
            reviewed_base: wire.reviewed_base,
            extra: wire.extra,
        })
    }
}
