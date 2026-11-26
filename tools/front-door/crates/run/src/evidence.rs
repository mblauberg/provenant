//! Host-owned settlement evidence: the launch pin, terminal head and the one
//! sealed record that is the single input to settlement.

use std::path::Path;

use provenant_core::GitOid;
use provenant_os::fs::Directory;
use provenant_records::{
    CanonicalV1, FileDigest,
    settlement::SettlementEvidence,
    warning::{SETTLEMENT_EVIDENCE_REDRIVE_DIFFERS, Warning, warn},
};

use crate::Result;

/// The sealed evidence record, beside the attempt's other host-owned facts.
const SETTLEMENT_EVIDENCE: &str = "settlement-evidence.json";

/// The launch-time Git base, sealed once by the writer at launch.
pub(crate) fn launch_pin(attempt: &Directory) -> Result<Option<GitOid>> {
    crate::writer::launch_pin(attempt)
}

/// The terminal Git head, sealed by the writer when custody closed.
pub(crate) fn terminal_head(attempt: &Directory) -> Result<Option<GitOid>> {
    crate::writer::terminal_head(attempt)
}

/// The recorded remote-tracking tip of the bound branch. Diagnostic only; the
/// classifier never reads it.
pub(crate) fn pushed_head(attempt: &Directory, worktree: &Path) -> Result<Option<GitOid>> {
    crate::writer::pushed_head(attempt, worktree)
}

/// Seal `evidence` as the attempt's one settlement input, returning the
/// effective evidence and the digest of its sealed bytes. A copy sealed by an
/// earlier drive wins: a restart that re-reads HEAD or re-prepares the writer
/// cannot move the pin once this record exists.
pub(crate) fn seal_evidence(
    attempt: &Directory,
    evidence: &SettlementEvidence,
) -> Result<(SettlementEvidence, FileDigest)> {
    if let Some((sealed, digest)) = read_evidence(attempt)? {
        if &sealed != evidence {
            warn(Warning::new(
                SETTLEMENT_EVIDENCE_REDRIVE_DIFFERS,
                "settlement evidence differs on re-drive; sealed copy kept",
            ));
        }
        return Ok((sealed, digest));
    }
    let bytes = crate::db::bytes(evidence)?;
    crate::artifacts::seal(attempt, SETTLEMENT_EVIDENCE, &bytes, crate::RECORD_LIMIT)?;
    Ok((evidence.clone(), CanonicalV1::file_digest(&bytes)))
}

/// Read the sealed settlement evidence and the digest of its exact bytes.
pub(crate) fn read_evidence(
    attempt: &Directory,
) -> Result<Option<(SettlementEvidence, FileDigest)>> {
    let Some(raw) =
        crate::artifacts::read_sealed(attempt, SETTLEMENT_EVIDENCE, crate::RECORD_LIMIT)?
    else {
        return Ok(None);
    };
    let evidence: SettlementEvidence = crate::db::decode(&raw)?;
    Ok(Some((evidence, CanonicalV1::file_digest(&raw))))
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod evidence_tests;
