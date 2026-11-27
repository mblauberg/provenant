//! The sealed writer terminal record: the head and push evidence written once
//! when writer custody closes, and its readers.
use crate::{Result, RunError};
use provenant_core::GitOid;
use provenant_os::fs::Directory;

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct WriterTerminal {
    pub schema_version: u64,
    pub head: GitOid,
    /// The remote-tracking tip of the bound branch when custody closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushed_head: Option<GitOid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_work: Option<provenant_records::run::StopWorkSummary>,
    /// Settlement warnings recorded once, so a resume does not re-emit them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// The sealed writer terminal record, if custody has closed.
fn read_terminal(attempt: &Directory) -> Result<Option<WriterTerminal>> {
    let Some(raw) =
        crate::artifacts::read_sealed(attempt, "writer-terminal.json", crate::RECORD_LIMIT)?
    else {
        return Ok(None);
    };
    let terminal: WriterTerminal = crate::db::decode(&raw)?;
    if terminal.schema_version != 1 {
        return Err(RunError::Validation("writer terminal version mismatch"));
    }
    Ok(Some(terminal))
}

/// The terminal Git head, read from the sealed writer terminal record.
pub(crate) fn terminal_head(attempt: &Directory) -> Result<Option<GitOid>> {
    Ok(read_terminal(attempt)?.map(|terminal| terminal.head))
}

/// The remote-tracking tip of the bound branch at the cut, read from the
/// sealed writer terminal record. Diagnostic evidence only: the classifier
/// never reads it, so a push can never change a settlement fact.
pub(crate) fn pushed_head(attempt: &Directory) -> Result<Option<GitOid>> {
    Ok(read_terminal(attempt)?.and_then(|terminal| terminal.pushed_head))
}
