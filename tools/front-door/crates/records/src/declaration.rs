//! Declaration journal wire types and errors.
use serde::{Deserialize, Serialize};

pub const DEFAULT_DECLARATION_JOURNAL_BYTES: u64 = 1 << 20;
/// Ceiling of the configurable bound: a journal can never grow past this even
/// when an operator raises `domains.run.handoff_journal_bytes`.
pub const MAX_DECLARATION_JOURNAL_BYTES: u64 = 16 << 20;
pub const DECLARATION_SCHEMA: u64 = 1;

/// The effective journal bound and the setting that supplied it. The key and
/// source travel with a `Full` refusal so the lane is told what to change,
/// and the bound travels to the receipt so a near-full call can warn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationLimit {
    pub journal_bytes: u64,
    pub key: String,
    pub source: String,
}

impl DeclarationLimit {
    pub fn shipped() -> Self {
        Self {
            journal_bytes: DEFAULT_DECLARATION_JOURNAL_BYTES,
            key: "domains.run.handoff_journal_bytes".into(),
            source: "compiled defaults".into(),
        }
    }

    /// Below one tenth of the bound, a receipt carries a warning-worthy
    /// remainder. A zero bound can never be near full.
    pub fn near_full(&self, remaining: u64) -> bool {
        self.journal_bytes > 0 && remaining < self.journal_bytes / 10
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclarationInput {
    pub request_id: String,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedDeclaration {
    pub schema_version: u64,
    pub sequence: u64,
    pub request_id: String,
    pub digest: crate::FileDigest,
    pub handoff: crate::handoff::Handoff,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeclarationLine {
    Accepted(SealedDeclaration),
    Rejected {
        sequence: u64,
        request_id: String,
        digest: crate::FileDigest,
        error: crate::handoff::HandoffError,
    },
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclarationReceipt {
    pub sequence: u64,
    pub request_id: String,
    pub digest: crate::FileDigest,
    /// Bytes left in the journal after this line was sealed. Additive on the
    /// wire: an older receipt without it decodes as zero.
    #[serde(default)]
    pub remaining_bytes: u64,
}

/// How many declaration lines an attempt collected, by disposition. Counts are
/// replayed from the whole journal, so a rejected line followed by an accepted
/// one reports both even though only the accepted line settles the lane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclarationCounts {
    #[serde(default)]
    pub accepted: u32,
    #[serde(default)]
    pub rejected: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeclarationEvidence {
    Missing,
    // Boxed so the closed set stays small: the sealed record is wire-identical,
    // and a rejected or missing declaration does not pay for a full handoff.
    Accepted(Box<SealedDeclaration>),
    Rejected { error: crate::handoff::HandoffError },
}

#[derive(Debug, thiserror::Error)]
pub enum DeclarationError {
    #[error("{0}")]
    Invalid(crate::handoff::HandoffError),
    #[error("request id reused with different content")]
    Conflict,
    #[error("attempt is settling; declarations are closed")]
    Closed,
    /// `setting_source` names the configuration that supplied the bound; it
    /// is not an error source and must not be named `source` to thiserror.
    #[error("declaration journal is full: {key} from {setting_source}")]
    Full { key: String, setting_source: String },
    #[error("not inside a lane: PROVENANT_PARENT_ATTEMPT_ID is unset")]
    NotInLane,
    #[error("declaration journal: {0}")]
    Io(String),
}
