//! Serializable evidence and outcomes shared by landing lanes.
use super::policy::LandPolicy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EligiblePr {
    pub number: u64,
    pub head: String,
    pub approval: String,
    pub base_branch: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateStep {
    pub pr: EligiblePr,
    pub commit: String,
    pub tree: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub base: String,
    pub steps: Vec<CandidateStep>,
    pub commit: String,
    pub tree: String,
    pub ref_name: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateDefinition {
    pub kind: provenant_cli::domains::GateKind,
    pub digest: String,
    pub checks: Vec<String>,
    pub commands: Vec<String>,
    pub issuer: Option<String>,
    pub executor: Option<String>,
    pub coverage: Vec<String>,
    pub timeout_seconds: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateResult {
    Passed,
    Failed,
    Pending,
    Missing,
    Skipped,
    Neutral,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckEvidence {
    pub name: String,
    pub issuer: Option<String>,
    pub result: GateResult,
}
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
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishMode {
    FastForward,
    RestOrdered,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishDecision {
    pub mode: PublishMode,
    pub reason: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishResult {
    pub landed: Vec<String>,
    pub alarms: Vec<Alarm>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Landed,
    Red,
    Conflicted,
    Excluded,
    PendingUnknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CostEstimate {
    Known { micros: u64 },
    Unknown { reason: String },
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
    #[serde(default)]
    pub split: Vec<Vec<u64>>,
    pub approved_to_landed: BTreeMap<String, String>,
    pub cost: CostEstimate,
    pub alarms: Vec<Alarm>,
    pub shadow: bool,
}
