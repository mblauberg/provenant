//! Declaration journal wire types and errors.
use serde::{Deserialize, Serialize};

pub const DECLARATION_JOURNAL_BYTES: u64 = 1 << 20;
pub const DECLARATION_SCHEMA: u64 = 1;

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
pub enum JournalLine {
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
    Accepted(SealedDeclaration),
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
    #[error("declaration journal is full")]
    Full,
    #[error("not inside a lane: PROVENANT_PARENT_ATTEMPT_ID is unset")]
    NotInLane,
    #[error("declaration journal: {0}")]
    Io(String),
}
