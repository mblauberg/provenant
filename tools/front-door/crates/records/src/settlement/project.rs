//! Settlement projection: the graph-facing outcome of a settled attempt.
//!
//! The one pure classifier owns the outcome; this module only reads the sealed
//! record. Facts the graph owns — a result warning, a failed or not-attempted
//! delivery, and the configured deadline — stay with the owner and are applied
//! before this projection is called.

use crate::graph::GraphAttemptOutcome;
use crate::run::{
    AttemptRecord, AttemptState, Outcome, SettlementSignal, PreparationFailure, CauseKind,
};

/// The settlement part of a terminal attempt's graph outcome.
///
/// `None` means there is no terminal settlement to project (a record still
/// queued, starting, running or settling). The caller keeps its own graph
/// overrides and applies this projection to everything else.
pub fn graph_outcome(record: &AttemptRecord) -> Option<GraphAttemptOutcome> {
    let AttemptState::Terminal {
        outcome,
        cause,
        ..
    } = &record.state
    else {
        return None;
    };
    let outcome = *outcome;
    let cause = *cause;
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
        return Some(GraphAttemptOutcome::PreparationRefused(cause));
    }
    if let Some(failure) = record.preparation_failure {
        return Some(match failure {
            PreparationFailure::AuthRequired => GraphAttemptOutcome::AuthRequired,
            PreparationFailure::UsageLimited => GraphAttemptOutcome::UsageLimited,
            PreparationFailure::AdapterUnavailable => GraphAttemptOutcome::AdapterUnavailable,
            PreparationFailure::GitSetup => GraphAttemptOutcome::PreparationRefused(
                cause.unwrap_or(CauseKind::PreparationRefused),
            ),
        });
    }

    Some(match outcome {
        Outcome::Succeeded => GraphAttemptOutcome::Succeeded,
        Outcome::AuthRequired => GraphAttemptOutcome::AuthRequired,
        Outcome::RateLimited | Outcome::QuotaLimited => GraphAttemptOutcome::UsageLimited,
        Outcome::TimedOut if cause == Some(CauseKind::TokenBudget) => {
            GraphAttemptOutcome::OtherFailure
        }
        Outcome::TimedOut => GraphAttemptOutcome::OtherFailure,
        Outcome::ModelUnavailable => GraphAttemptOutcome::AdapterUnavailable,
        Outcome::Unclassified => GraphAttemptOutcome::Unclassified,
        Outcome::Rejected if !launched => cause.or(Some(CauseKind::Unrecorded)).map_or(
            GraphAttemptOutcome::OtherFailure,
            GraphAttemptOutcome::PreparationRefused,
        ),
        Outcome::ReviewRequired if cause == Some(CauseKind::StoppedNoWork) => {
            GraphAttemptOutcome::StoppedNoWork
        }
        // A declared stop is one fact whether or not the writer left
        // uncommitted edits; the graph restarts it on the retained worktree.
        Outcome::ReviewRequired | Outcome::Uncommitted
            if record
                .signals
                .contains(&SettlementSignal::DeclaredStop)
                && record.signals.iter().all(|signal| {
                    matches!(
                        signal,
                        SettlementSignal::DeclaredStop | SettlementSignal::UncommittedWork
                    )
                }) =>
        {
            GraphAttemptOutcome::DeclaredStop
        }
        Outcome::Failed if cause == Some(CauseKind::ProviderStalledRetry) => {
            GraphAttemptOutcome::ProviderStalledRetry
        }
        Outcome::Failed => GraphAttemptOutcome::TaskFailed,
        Outcome::ReviewRequired => GraphAttemptOutcome::ReviewRequired,
        // Uncommitted edits without a declared stop need the chair, so the node
        // fails without fallback.
        Outcome::Uncommitted => GraphAttemptOutcome::OtherFailure,
        _ => GraphAttemptOutcome::OtherFailure,
    })
}
