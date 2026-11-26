//! Evidence and facts for settlement of a lane attempt.
pub mod classify;
pub mod policy;
pub mod project;

use provenant_core::GitOid;
use serde::{Deserialize, Serialize};

use crate::declaration::{DeclarationCounts, DeclarationEvidence};
use crate::handoff::Handoff;
use crate::run::{
    CauseKind, CauseSource, DependantRelease, Outcome, SettlementClass, SettlementSignal,
    TerminalBasis,
};
use crate::settlement::policy::{AppliedPolicy, SettlementPolicy};
use crate::stop::Stop;

pub const SETTLEMENT_EVIDENCE_SCHEMA: u64 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SettlementEvidence {
    pub schema_version: u64,
    pub lane: LaneRole,
    pub host: HostFacts,
    pub handoff: DeclarationEvidence,
    /// Every declaration line, not just the one that settled the lane.
    #[serde(default)]
    pub declarations: DeclarationCounts,
    pub git: Option<GitEvidence>,
    pub custody: CustodyFacts,
    /// The effective policy, sealed so a re-drive reproduces the decision.
    #[serde(default)]
    pub policy: SettlementPolicy,
}

/// Where the settled handoff came from. The host synthesises one for budget
/// and context stops; otherwise the lane declared it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffSource {
    Declared,
    Host,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneRole {
    Reader,
    ModelWriter,
    AdapterWriter,
}

/// The holder's host phase result, before settlement classification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HostFacts {
    pub outcome: Outcome,
    pub cause: Option<CauseKind>,
    pub complete: bool,
    pub result_empty: bool,
    pub reason: Option<String>,
    pub terminal_note: Option<String>,
    pub signals: Vec<SettlementSignal>,
    pub denial: Option<String>,
    pub host_handoff: Option<Handoff>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CustodyFacts {
    pub launched: bool,
    pub cancelled: bool,
    pub steered: bool,
    pub cancellation_delivered: Option<bool>,
    pub import_cancelled: bool,
    pub process_exit: bool,
    pub stall: Option<CauseKind>,
    pub identity: IdentityEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct IdentityEvidence {
    pub conflict: bool,
    pub incomplete: IdentityPolicyEffect,
    pub eof_fragment_unsettles: bool,
    /// Whether the observed provider route matched the requested one. The
    /// host records `Observed` for an attempt that already failed, so a
    /// failure keeps its own cause. The classifier applies it in the identity
    /// step, so the class, the release and delivery eligibility follow the
    /// same decision.
    #[serde(default)]
    pub route: RouteIdentityFact,
}

/// The observed route against the resolved and applied routes, with the
/// identities a mismatch names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteIdentityFact {
    #[default]
    Observed,
    Unobserved,
    Mismatch {
        requested: String,
        observed: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IdentityPolicyEffect {
    Complete,
    Proceed,
    Warn { message: String },
    Unsettle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GitEvidence {
    pub input_head: Option<GitOid>,
    pub writer_head: Option<GitOid>,
    pub uncommitted: Option<String>,
    /// The bounded, sorted paths behind the census note. Diagnostic only:
    /// the classifier keeps deciding from `uncommitted`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncommitted_paths: Vec<String>,
    /// The remote-tracking tip of the bound branch at the cut. Diagnostic
    /// only: `settle` never reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushed_head: Option<GitOid>,
    pub primary: PrimaryCheckout,
}
impl GitEvidence {
    pub fn head_progress(&self) -> HeadProgress {
        match (&self.input_head, &self.writer_head) {
            (Some(pinned), Some(head)) if pinned == head => HeadProgress::Unchanged,
            (Some(_), Some(_)) => HeadProgress::Moved,
            _ => HeadProgress::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrimaryCheckout {
    NotChecked,
    Clean,
    Warn { note: String },
    Review { note: String },
    Unavailable { note: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadProgress {
    Moved,
    Unchanged,
    Unknown,
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutcomeFact {
    pub outcome: Outcome,
    pub cause: Option<CauseKind>,
    pub settlement_class: SettlementClass,
    pub stop: Option<Stop>,
    pub cause_source: CauseSource,
    pub release: DependantRelease,
    pub terminal_basis: TerminalBasis,
    pub signals: Vec<SettlementSignal>,
    pub head_progress: Option<HeadProgress>,
    pub handoff: crate::handoff::HandoffResult,
    pub handoff_source: Option<HandoffSource>,
    /// Declaration counts, absent for lanes that owe no handoff.
    pub declarations: Option<DeclarationCounts>,
    /// Non-default policy actions that changed the decision.
    pub applied: Vec<AppliedPolicy>,
    pub reason: Option<String>,
    pub terminal_note: Option<String>,
    pub complete: bool,
    pub delivery_eligible: bool,
}

#[cfg(any(test, feature = "fixture"))]
impl SettlementEvidence {
    /// A launched, complete fixture with no declaration or Git evidence.
    pub fn fixture(lane: LaneRole, outcome: Outcome, cause: Option<CauseKind>) -> Self {
        Self {
            schema_version: SETTLEMENT_EVIDENCE_SCHEMA,
            lane,
            host: HostFacts {
                outcome,
                cause,
                complete: true,
                result_empty: false,
                reason: matches!(outcome, Outcome::Unsettled | Outcome::Lost)
                    .then(|| "fixture".into()),
                terminal_note: None,
                signals: vec![],
                denial: None,
                host_handoff: None,
            },
            handoff: DeclarationEvidence::Missing,
            declarations: DeclarationCounts::default(),
            git: None,
            custody: CustodyFacts {
                launched: true,
                cancelled: outcome == Outcome::Cancelled,
                steered: false,
                cancellation_delivered: Some(true),
                import_cancelled: false,
                process_exit: true,
                stall: None,
                identity: IdentityEvidence {
                    conflict: false,
                    incomplete: IdentityPolicyEffect::Complete,
                    eof_fragment_unsettles: false,
                    route: RouteIdentityFact::Observed,
                },
            },
            policy: SettlementPolicy::shipped(),
        }
    }
}
