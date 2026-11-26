//! Evidence and facts for settlement of a lane attempt.
pub mod classify;
pub mod project;

use serde::{Deserialize, Serialize};

use crate::declaration::DeclarationEvidence;
use crate::handoff::Handoff;
use crate::run::{
    CauseSource, Outcome, PartialSignal, SettlementNodeOutcome, SettlementReason, TerminalCause,
    TerminalEvidence,
};
use crate::stop::Stop;

pub const SETTLEMENT_EVIDENCE_SCHEMA: u64 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SettlementEvidence {
    pub schema_version: u64,
    pub lane: LaneRole,
    pub host: HostVerdict,
    pub handoff: DeclarationEvidence,
    pub git: Option<GitEvidence>,
    pub custody: CustodyFacts,
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
pub struct HostVerdict {
    pub outcome: Outcome,
    pub cause: Option<TerminalCause>,
    pub complete: bool,
    pub result_empty: bool,
    pub reason: Option<String>,
    pub terminal_note: Option<String>,
    pub signals: Vec<PartialSignal>,
    pub denial: Option<String>,
    pub stopped_handoff: Option<Handoff>,
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
    pub stall: Option<TerminalCause>,
    pub identity: IdentityEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct IdentityEvidence {
    pub conflict: bool,
    pub incomplete: IdentityPolicyEffect,
    pub eof_fragment_unsettles: bool,
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
    pub pinned: Option<crate::GitOid>,
    pub head: Option<crate::GitOid>,
    pub uncommitted: Option<String>,
    pub primary: PrimaryCheckout,
}
impl GitEvidence {
    pub fn progress(&self) -> Progress {
        match (&self.pinned, &self.head) {
            (Some(pinned), Some(head)) if pinned == head => Progress::Unchanged,
            (Some(_), Some(_)) => Progress::Moved,
            _ => Progress::Unknown,
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
pub enum Progress {
    Moved,
    Unchanged,
    Unknown,
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutcomeFact {
    pub outcome: Outcome,
    pub cause: Option<TerminalCause>,
    pub settlement_reason: SettlementReason,
    pub stop: Option<Stop>,
    pub source: CauseSource,
    pub node: SettlementNodeOutcome,
    pub terminal_evidence: TerminalEvidence,
    pub signals: Vec<PartialSignal>,
    pub progress: Option<Progress>,
    pub handoff: crate::handoff::HandoffResult,
    pub reason: Option<String>,
    pub terminal_note: Option<String>,
    pub complete: bool,
    pub delivery_eligible: bool,
}

#[cfg(any(test, feature = "fixture"))]
impl SettlementEvidence {
    /// A launched, complete fixture with no declaration or Git evidence.
    pub fn fixture(lane: LaneRole, outcome: Outcome, cause: Option<TerminalCause>) -> Self {
        Self {
            schema_version: SETTLEMENT_EVIDENCE_SCHEMA,
            lane,
            host: HostVerdict {
                outcome,
                cause,
                complete: true,
                result_empty: false,
                reason: matches!(outcome, Outcome::Unsettled | Outcome::Lost)
                    .then(|| "fixture".into()),
                terminal_note: None,
                signals: vec![],
                denial: None,
                stopped_handoff: None,
            },
            handoff: DeclarationEvidence::Missing,
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
                },
            },
        }
    }
}
