//! Serializable evidence and outcomes shared by landing lanes.
use super::policy::LandPolicy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

macro_rules! stored_enum {
    (
        $(#[$attribute:meta])*
        $visibility:vis enum $name:ident {
            $($(#[$variant_attribute:meta])* $variant:ident $({ $($fields:tt)* })? => $sample:expr),+ $(,)?
        }
    ) => {
        $(#[$attribute])*
        $visibility enum $name {
            $($(#[$variant_attribute])* $variant $({ $($fields)* })?),+
        }

        impl $name {
            #[cfg(test)]
            pub(crate) fn all() -> Vec<Self> {
                vec![$($sample),+]
            }
        }
    };
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EligiblePr {
    pub number: u64,
    pub head: String,
    pub approval: String,
    pub approval_evidence: Vec<ApprovalEvidence>,
    pub base_branch: String,
    /// Set when the chair landed the pull request without a covering review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<Accepted>,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}
impl EligiblePr {
    /// Keep every observed approval; source priority and arrival order do not
    /// overwrite evidence already attached to the pinned head.
    pub fn extend_approval_evidence(
        &mut self,
        evidence: impl IntoIterator<Item = ApprovalEvidence>,
    ) {
        for proof in evidence {
            if !self.approval_evidence.contains(&proof) {
                self.approval_evidence.push(proof);
            }
        }
    }
}
/// The source and execution/reviewer that approved the pinned head.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalEvidence {
    pub source: provenant_cli::domains::ApprovalSource,
    pub reference: String,
    pub reviewer: String,
    pub head: String,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}
/// A delta landed on the chair's word, with who gave it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub reason: String,
    pub actor: String,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateStep {
    pub pr: EligiblePr,
    pub commit: String,
    pub tree: String,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub base: String,
    pub steps: Vec<CandidateStep>,
    pub commit: String,
    pub tree: String,
    pub ref_name: String,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}
pub use provenant_records::landing::{CheckEvidence, GateDefinition, GateResult};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateEvidence {
    pub commit: String,
    pub tree: String,
    pub definition_digest: String,
    pub checks: Vec<CheckEvidence>,
    pub issuer: Option<String>,
    pub coverage: Vec<String>,
    pub result: GateResult,
    pub pool: Option<String>,
    pub duration_seconds: Option<u64>,
    /// The commit whose passing checks were reused for this identical tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused_from_commit: Option<String>,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}
stored_enum! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum PublishMode {
        FastForward => Self::FastForward,
        RestOrdered => Self::RestOrdered,
        Command => Self::Command,
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishDecision {
    pub mode: PublishMode,
    pub reason: String,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishResult {
    pub landed: Vec<String>,
    pub alarms: Vec<Alarm>,
}
stored_enum! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Outcome {
        Landed => Self::Landed,
        Red => Self::Red,
        Conflicted => Self::Conflicted,
        Excluded => Self::Excluded,
        PendingUnknown => Self::PendingUnknown,
        /// Still waiting on CI or another lander; a resubmission resumes it.
        Pending => Self::Pending,
    }
}
/// The head and base a landing request pinned for one pull request. A
/// member whose live head or base differs is excluded, never landed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub head: Option<String>,
    pub base: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    pub code: String,
    pub message: String,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}
stored_enum! {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "status", rename_all = "snake_case")]
    pub enum CostEstimate {
        Known {
            micros: u64,
            #[serde(flatten, default)]
            extra: provenant_records::ExtraFields,
        } => Self::Known { micros: 0, extra: Default::default() },
        Unknown {
            reason: String,
            #[serde(flatten, default)]
            extra: provenant_records::ExtraFields,
        } => Self::Unknown { reason: String::new(), extra: Default::default() },
    }
}
/// Durable evidence recorded before attempting a publication.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingAttempt {
    pub repository: String,
    pub target: String,
    pub candidate: Candidate,
    pub policy: LandPolicy,
    pub gate_definition: GateDefinition,
    pub gate_evidence: GateEvidence,
    pub publish: PublishDecision,
    pub cost: CostEstimate,
    #[serde(default)]
    pub settled: Option<SettledAttempt>,
    /// Members for which dispatch was prepared, recorded before submission.
    #[serde(default)]
    pub dispatched: Option<Vec<u64>>,
    #[serde(default)]
    pub started_at: Option<u64>,
    /// Fields written by a newer minor release survive pending-record rewrites.
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}

/// A definite publication outcome. This phase never authorises publication.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettledAttempt {
    pub record: LandingReceipt,
    pub outcomes: BTreeMap<u64, SettledOutcome>,
    pub landed_commit: Option<String>,
    pub refusal: Option<String>,
    pub receipt_pending: bool,
    pub tree_pending: bool,
    pub follow_pending: bool,
    pub local_follow_pending: bool,
    pub cleanup_pending: bool,
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}

stored_enum! {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "outcome", rename_all = "snake_case")]
    pub enum SettledOutcome {
        Landed {
            commit: String,
            #[serde(flatten, default)]
            extra: provenant_records::ExtraFields,
        } => Self::Landed { commit: String::new(), extra: Default::default() },
        Refused {
            #[serde(flatten, default)]
            extra: provenant_records::ExtraFields,
        } => Self::Refused {
            extra: Default::default(),
        },
        NotLanded {
            #[serde(flatten, default)]
            extra: provenant_records::ExtraFields,
        } => Self::NotLanded {
            extra: Default::default(),
        },
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LandingReceipt {
    pub policy: LandPolicy,
    pub base: String,
    pub candidate: String,
    pub tree: String,
    pub candidate_ref: String,
    pub candidate_steps: Vec<CandidateStep>,
    pub pull_requests: Vec<EligiblePr>,
    pub gate_definition: GateDefinition,
    pub gate_evidence: GateEvidence,
    pub publish: PublishDecision,
    pub outcomes: BTreeMap<u64, Outcome>,
    pub split: Vec<Vec<u64>>,
    pub approved_to_landed: BTreeMap<String, String>,
    pub cost: CostEstimate,
    pub alarms: Vec<Alarm>,
    pub shadow: bool,
    /// Fields written by a newer minor release survive receipt rewrites.
    #[serde(flatten, default)]
    pub extra: provenant_records::ExtraFields,
}

impl LandingReceipt {
    pub fn new(
        candidate: &Candidate,
        policy: LandPolicy,
        gate_definition: GateDefinition,
        gate_evidence: GateEvidence,
        publish: PublishDecision,
        cost: CostEstimate,
        shadow: bool,
    ) -> Self {
        Self {
            policy,
            base: candidate.base.clone(),
            candidate: candidate.commit.clone(),
            tree: candidate.tree.clone(),
            candidate_ref: candidate.ref_name.clone(),
            candidate_steps: candidate.steps.clone(),
            pull_requests: candidate.steps.iter().map(|step| step.pr.clone()).collect(),
            gate_definition,
            gate_evidence,
            publish,
            outcomes: BTreeMap::new(),
            split: Vec::new(),
            approved_to_landed: BTreeMap::new(),
            cost,
            alarms: Vec::new(),
            shadow,
            extra: Default::default(),
        }
    }
}
