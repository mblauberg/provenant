//! Settlement projection: the graph-facing outcome of a settled attempt.
//!
//! The one pure classifier owns the outcome; this module only reads the sealed
//! record. Facts the graph owns — a result warning, a failed or not-attempted
//! delivery, and the configured deadline — stay with the owner and are applied
//! before this projection is called.

use crate::graph::AttemptOutcome;
use crate::run::{
    AttemptRecord, AttemptState, Outcome, PartialSignal, PreparationFailure, CauseKind,
};

/// The settlement part of a terminal attempt's graph outcome.
///
/// `None` means there is no terminal settlement to project (a record still
/// queued, starting, running or settling). The caller keeps its own graph
/// overrides and applies this projection to everything else.
pub fn graph_outcome(record: &AttemptRecord) -> Option<AttemptOutcome> {
    let AttemptState::Terminal {
        outcome,
        terminal_cause,
        ..
    } = &record.state
    else {
        return None;
    };
    let outcome = *outcome;
    let cause = *terminal_cause;
    let launched = record.provider_launched.unwrap_or(true);

    // A refusal happened before custody, so it describes route policy, not the
    // cause (for example an unregistered account).
    if !launched
        && outcome == Outcome::Rejected
        && record
            .preparation_failure
            .is_none_or(|failure| failure == PreparationFailure::AdapterUnavailable)
        && let Some(cause) = cause
        && cause != CauseKind::CommandUnavailable
    {
        return Some(AttemptOutcome::PreparationRefused(cause));
    }
    if let Some(failure) = record.preparation_failure {
        return Some(match failure {
            PreparationFailure::AuthRequired => AttemptOutcome::AuthRequired,
            PreparationFailure::UsageLimited => AttemptOutcome::UsageLimited,
            PreparationFailure::AdapterUnavailable => AttemptOutcome::AdapterUnavailable,
            PreparationFailure::GitSetup => AttemptOutcome::PreparationRefused(
                cause.unwrap_or(CauseKind::PreparationRefused),
            ),
        });
    }

    Some(match outcome {
        Outcome::Succeeded => AttemptOutcome::Succeeded,
        Outcome::AuthRequired => AttemptOutcome::AuthRequired,
        Outcome::RateLimited | Outcome::QuotaLimited => AttemptOutcome::UsageLimited,
        Outcome::TimedOut if cause == Some(CauseKind::TokenBudget) => {
            AttemptOutcome::OtherFailure
        }
        Outcome::TimedOut => AttemptOutcome::OtherFailure,
        Outcome::ModelUnavailable => AttemptOutcome::AdapterUnavailable,
        Outcome::Unclassified => AttemptOutcome::Unclassified,
        Outcome::Rejected if !launched => cause.or(Some(CauseKind::Unrecorded)).map_or(
            AttemptOutcome::OtherFailure,
            AttemptOutcome::PreparationRefused,
        ),
        Outcome::ReviewRequired if cause == Some(CauseKind::StoppedNoWork) => {
            AttemptOutcome::StoppedNoWork
        }
        // A declared stop is one fact whether or not the writer left
        // uncommitted edits; the graph restarts it on the retained worktree.
        Outcome::ReviewRequired | Outcome::Uncommitted
            if record
                .partial_signals
                .contains(&PartialSignal::DeclaredStop)
                && record.partial_signals.iter().all(|signal| {
                    matches!(
                        signal,
                        PartialSignal::DeclaredStop | PartialSignal::UncommittedWork
                    )
                }) =>
        {
            AttemptOutcome::DeclaredStop
        }
        Outcome::Failed if cause == Some(CauseKind::ProviderStalledRetry) => {
            AttemptOutcome::ProviderStalledRetry
        }
        Outcome::Failed => AttemptOutcome::TaskFailed,
        Outcome::ReviewRequired => AttemptOutcome::ReviewRequired,
        // Uncommitted edits without a declared stop need the chair, so the node
        // fails without fallback.
        Outcome::Uncommitted => AttemptOutcome::OtherFailure,
        _ => AttemptOutcome::OtherFailure,
    })
}
