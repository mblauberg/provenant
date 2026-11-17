//! Settlement progress shared by the guardian and the run recovery driver.
use crate::census::ProcessIdentity;
use crate::custody::{self, CustodyError, Result};
use provenant_core::{CancelToken, Clock, GitOid, WaitBudget};
use provenant_os::fs::{Directory, LockMode, PublishMode};
use provenant_records::custody::Scope;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;

pub const FILE: &str = "settlement-progress.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Import,
    Publish,
    Record,
    Release,
    Complete,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Journal {
    pub schema_version: u64,
    pub scope: Scope,
    pub phase: Phase,
    pub attempts: u32,
    pub head: Option<GitOid>,
    pub base: Option<GitOid>,
    pub owner: Option<ProcessIdentity>,
    #[serde(default)]
    pub owner_since_ms: u64,
    #[serde(default)]
    pub retry_after_ms: u64,
    #[serde(default)]
    pub recovery_failures: u32,
    #[serde(default)]
    pub blocking_reason: Option<String>,
    #[serde(default)]
    pub abandonment: Option<Abandonment>,
    #[serde(default, flatten)]
    pub extra: provenant_records::ExtraFields,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Abandonment {
    pub reason: String,
    pub actor: provenant_core::ActorId,
    pub at: provenant_core::WallTime,
    pub phase: Phase,
    /// Intent survives a crash between the loss declaration and fence release.
    #[serde(default)]
    pub declare_lost: bool,
    #[serde(default, flatten)]
    pub extra: provenant_records::ExtraFields,
}

impl Journal {
    pub fn new(scope: Scope) -> Self {
        Self {
            schema_version: 1,
            scope,
            phase: Phase::Import,
            attempts: 0,
            head: None,
            base: None,
            owner: None,
            owner_since_ms: 0,
            retry_after_ms: 0,
            recovery_failures: 0,
            blocking_reason: None,
            abandonment: None,
            extra: Default::default(),
        }
    }
}

/// Preserve the holder's terminal failure under the driver's journal lock.
/// Keep all progress, extensions and retry policy owned by the run driver.
pub(crate) fn terminal_failed(attempt: &Directory, scope: Scope, reason: &str) -> Result<()> {
    let clock = provenant_os::clock::SystemClock::new();
    let budget = WaitBudget::cli(clock.mono_now(), 1).map_err(|_| CustodyError::Timeout)?;
    let _lock = attempt.lock(
        OsStr::new("settlement.lock"),
        LockMode::Exclusive,
        budget,
        &CancelToken::new(),
        &clock,
    )?;
    let mut journal = match custody::read_record::<Journal>(attempt, FILE, custody::RECORD_LIMIT) {
        Ok(journal) => journal,
        Err(CustodyError::Filesystem(error)) if error.is_not_found() => Journal::new(scope),
        Err(error) => return Err(error),
    };
    if journal.schema_version != 1 || journal.scope != scope {
        return Err(CustodyError::InvalidRecord);
    }
    // Control characters cannot make a status row span lines. Bound on a UTF-8 boundary.
    let mut reason: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut end = reason.len().min(4096);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason.truncate(end);
    journal.blocking_reason = Some(reason);
    custody::write_record(attempt, FILE, &journal, PublishMode::Replace)?.require_durable()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_failure_preserves_scoped_progress_and_unknown_fields()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        // Spec: docs/specs/supervisor.md#tick-driven-settlement
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let scope = Scope {
            attempt: provenant_core::AttemptId::from_bytes([1; 16]),
            generation: 1,
        };
        terminal_failed(&attempt, scope, "first failure")?;
        let mut journal = custody::read_record::<Journal>(&attempt, FILE, custody::RECORD_LIMIT)?;
        assert_eq!(journal.blocking_reason.as_deref(), Some("first failure"));
        journal.phase = Phase::Record;
        journal.attempts = 2;
        journal.retry_after_ms = 123;
        journal
            .extra
            .insert("future_journal".into(), serde_json::json!({"keep": true}));
        custody::write_record(&attempt, FILE, &journal, PublishMode::Replace)?.require_durable()?;
        terminal_failed(&attempt, scope, "terminal\nfailed")?;
        let saved = custody::read_record::<Journal>(&attempt, FILE, custody::RECORD_LIMIT)?;
        assert_eq!(saved.blocking_reason.as_deref(), Some("terminal failed"));
        assert_eq!(saved.phase, journal.phase);
        assert_eq!(saved.attempts, journal.attempts);
        assert_eq!(saved.retry_after_ms, journal.retry_after_ms);
        assert_eq!(saved.extra, journal.extra);
        assert!(
            terminal_failed(
                &attempt,
                Scope {
                    generation: 2,
                    ..scope
                },
                "wrong scope"
            )
            .is_err()
        );
        assert_eq!(
            custody::read_record::<Journal>(&attempt, FILE, custody::RECORD_LIMIT)?.blocking_reason,
            saved.blocking_reason
        );
        Ok(())
    }
}
