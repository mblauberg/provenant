//! The one pure decider over a sealed [`SettlementEvidence`] record.
//!
//! Nothing outside this module writes a launched attempt's outcome, cause,
//! settlement reason, stop or partial signals. Every note string and every
//! ordering decision is carried over from the holder's former terminal ladder
//! and the store's former classification; this is that ladder over one record.

use provenant_core::bounded_line;

use crate::declaration::DeclarationEvidence;
use crate::handoff::{HandoffResult, HandoffStatus};
use crate::run::{
    Outcome, PartialSignal, SettlementNodeOutcome, SettlementReason, TerminalCause,
    TerminalEvidence,
};
use crate::settlement::{
    IdentityPolicyEffect, LaneRole, OutcomeFact, PrimaryCheckout, Progress, SettlementEvidence,
};
use crate::stop::{Stop, StopCause};

const EMPTY_RESULT_REASON: &str = "provider result is empty or incomplete";
const NO_HANDOFF_REASON: &str = "no handoff: the lane ended without reporting results";
const UNCANCELLED_REASON: &str = "cancellation delivery before provider exit is unknown";
const IDENTITY_INCOMPLETE_REASON: &str =
    "provider identity evidence was incomplete; continuation refused by policy";
const IDENTITY_EOF_REASON: &str =
    "provider ended with an incomplete final frame; continuation refused";

fn append(reason: &mut Option<String>, note: impl Into<String>) {
    let note = note.into();
    *reason = Some(match reason.take() {
        Some(previous) => format!("{previous}; {note}"),
        None => note,
    });
}

fn bounded(note: &str) -> String {
    bounded_line(note, 240)
}

/// The old reason-derivation ladder, now private to the one decider.
fn derived_reason(
    outcome: Outcome,
    evidence: &TerminalEvidence,
    partial: &[PartialSignal],
    cause: Option<TerminalCause>,
    handoff: &HandoffResult,
    launched: bool,
) -> SettlementReason {
    if outcome == Outcome::Lost {
        return SettlementReason::Lost;
    }
    if outcome == Outcome::Unsettled {
        return SettlementReason::Unsettled;
    }
    if outcome == Outcome::Unclassified {
        return SettlementReason::Unclassified;
    }
    if matches!(evidence, TerminalEvidence::Lost { .. }) {
        return SettlementReason::Lost;
    }
    if matches!(evidence, TerminalEvidence::Unsettled { .. }) {
        return SettlementReason::Unsettled;
    }
    if outcome == Outcome::Cancelled && cause == Some(TerminalCause::Steered) {
        return SettlementReason::Steered;
    }
    if outcome == Outcome::Cancelled {
        return SettlementReason::Cancelled;
    }
    if !launched || matches!(evidence, TerminalEvidence::PreLaunchRefusal) {
        return SettlementReason::PreLaunchRefusal;
    }
    if handoff.is_err() {
        return SettlementReason::HandoffMalformed;
    }
    match cause {
        Some(TerminalCause::EmptyWriterResult) => return SettlementReason::EmptyResult,
        Some(TerminalCause::Blocked | TerminalCause::StoppedNoWork) => {
            return SettlementReason::DeclaredStop;
        }
        Some(
            TerminalCause::ProviderEarlyStop
            | TerminalCause::ProviderStalled
            | TerminalCause::ProviderStalledRetry,
        ) => return SettlementReason::ProviderStop,
        _ => {}
    }
    if partial.contains(&PartialSignal::StrayWork) {
        return SettlementReason::StrayWork;
    }
    if handoff
        .as_ref()
        .ok()
        .flatten()
        .is_some_and(crate::handoff::Handoff::unfinished)
    {
        return SettlementReason::DeclaredStop;
    }
    if partial.contains(&PartialSignal::DeclaredStop) {
        return SettlementReason::DeclaredStop;
    }
    if partial.contains(&PartialSignal::HandoffMissing) {
        return SettlementReason::HandoffMissing;
    }
    if partial.contains(&PartialSignal::ProviderStop) {
        return SettlementReason::ProviderStop;
    }
    if partial.contains(&PartialSignal::UncommittedWork) || outcome == Outcome::Uncommitted {
        return SettlementReason::UncommittedWork;
    }
    if cause == Some(TerminalCause::TokenBudget)
        && matches!(
            outcome,
            Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Failed | Outcome::TimedOut
        )
        && handoff
            .as_ref()
            .ok()
            .flatten()
            .is_some_and(|accepted| !accepted.unfinished() && accepted.validate().is_ok())
    {
        return SettlementReason::CompletedBeforeBudget;
    }
    if matches!(outcome, Outcome::Succeeded | Outcome::ReviewRequired) {
        return SettlementReason::Completed;
    }
    SettlementReason::Failed
}

/// Settle one launched attempt from a sealed evidence record. Pure: no I/O, no
/// clock and no reads of remote refs, pushes or prose.
pub fn settle(evidence: &SettlementEvidence) -> OutcomeFact {
    let host = &evidence.host;
    let custody = &evidence.custody;
    let writer = !matches!(evidence.lane, LaneRole::Reader);

    let mut outcome = host.outcome;
    let mut cause = host.cause;
    let mut reason = host.reason.clone();
    let mut terminal_note = host.terminal_note.clone();
    let mut complete = host.complete;
    let mut signals = host.signals.clone();

    // 1. Edits left on the worktree are not a success. The census note is the
    // only change to the reason here; slice 5a no longer mutates the host state.
    if writer && outcome == Outcome::Succeeded {
        if let Some(git) = &evidence.git
            && let Some(note) = &git.uncommitted
        {
            outcome = Outcome::Uncommitted;
            append(&mut reason, note.clone());
        }
    }

    // 2. A move in the primary checkout is evidence, not an early decision.
    if let Some(git) = &evidence.git {
        match &git.primary {
            PrimaryCheckout::Review { note } => {
                outcome = Outcome::ReviewRequired;
                signals.push(PartialSignal::StrayWork);
                append(&mut reason, note.clone());
            }
            PrimaryCheckout::Warn { note } | PrimaryCheckout::Unavailable { note } => {
                append(&mut reason, note.clone());
            }
            PrimaryCheckout::NotChecked | PrimaryCheckout::Clean => {}
        }
    }

    // 3. An empty final result: a writer's work is its commit, a reader's
    // answer is its result.
    if host.result_empty && matches!(outcome, Outcome::Succeeded | Outcome::Uncommitted) {
        if writer {
            cause = Some(TerminalCause::EmptyWriterResult);
            if outcome == Outcome::Succeeded {
                outcome = Outcome::ReviewRequired;
            }
            append(&mut reason, TerminalCause::EmptyWriterResult.detail());
        } else {
            outcome = Outcome::Failed;
            reason = Some(EMPTY_RESULT_REASON.into());
        }
    }

    // 4. Delivery keys off the outcome before a declared stop.
    let delivery_eligible = outcome == Outcome::Succeeded;

    // 5. A host-synthesised stop outranks a declaration; otherwise the last
    // accepted journal line is the declaration, the rejected lines its error.
    let stopped = host.stopped_handoff.clone();
    let declaration: HandoffResult = match &stopped {
        Some(handoff) => Ok(Some(handoff.clone())),
        None => match &evidence.handoff {
            DeclarationEvidence::Accepted(sealed) => Ok(Some(sealed.handoff.clone())),
            DeclarationEvidence::Rejected { error } => Err(error.clone()),
            DeclarationEvidence::Missing => Ok(None),
        },
    };
    let unfinished = declaration
        .as_ref()
        .ok()
        .flatten()
        .is_some_and(crate::handoff::Handoff::unfinished);

    if let Err(error) = &declaration {
        signals.push(PartialSignal::HandoffMalformed);
        append(&mut reason, error.to_string());
        if matches!(outcome, Outcome::Succeeded | Outcome::Uncommitted) {
            outcome = Outcome::ReviewRequired;
        }
    }
    if cause == Some(TerminalCause::EmptyWriterResult) {
        signals.push(PartialSignal::HandoffMissing);
    }
    if matches!(
        outcome,
        Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
    ) && evidence.lane == LaneRole::ModelWriter
        && stopped.is_none()
        && matches!(&evidence.handoff, DeclarationEvidence::Missing)
    {
        signals.push(PartialSignal::HandoffMissing);
        if outcome == Outcome::Succeeded {
            outcome = Outcome::ReviewRequired;
            reason = Some(NO_HANDOFF_REASON.into());
        }
    }

    if matches!(
        outcome,
        Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
    ) {
        let mut notes = Vec::new();
        if let Some(handoff) = declaration
            .as_ref()
            .ok()
            .flatten()
            .filter(|handoff| handoff.unfinished())
        {
            signals.push(PartialSignal::DeclaredStop);
            if outcome == Outcome::Succeeded {
                outcome = Outcome::ReviewRequired;
            }
            notes.push(format!("lane stopped early: {}", bounded(&handoff.note)));
        }
        if outcome == Outcome::Uncommitted {
            signals.push(PartialSignal::UncommittedWork);
        }
        signals.sort();
        signals.dedup();
        if !signals.is_empty() {
            let labels: Vec<_> = signals.iter().map(|signal| signal.as_str()).collect();
            notes.insert(0, format!("partial finish ({})", labels.join(", ")));
            append(&mut reason, notes.join(": "));
        }
    } else if unfinished {
        signals.push(PartialSignal::DeclaredStop);
        signals.sort();
        signals.dedup();
    } else {
        signals.clear();
    }

    // 6. A blocked writer settles with its own cause and its bounded note.
    if writer
        && (cause.is_none() || cause == Some(TerminalCause::Blocked))
        && matches!(
            outcome,
            Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
        )
        && let Some(blocked) = declaration
            .as_ref()
            .ok()
            .flatten()
            .filter(|handoff| handoff.status == HandoffStatus::Blocked)
    {
        cause = Some(TerminalCause::Blocked);
        terminal_note = Some(bounded(&blocked.note));
    }

    // 7. No-work is a launch-pinned Git fact: the head is known and equal to
    // the pin, and no other partial signal explains the stop. Pushes, remote
    // refs and an unknown head never produce it.
    let declared_stopped_early = declaration
        .as_ref()
        .ok()
        .flatten()
        .is_some_and(|handoff| handoff.status == HandoffStatus::StoppedEarly);
    if writer
        && outcome == Outcome::ReviewRequired
        && declared_stopped_early
        && cause.is_none()
        && evidence
            .git
            .as_ref()
            .is_some_and(|git| git.progress() == Progress::Unchanged)
        && !signals.contains(&PartialSignal::StrayWork)
        && !signals.contains(&PartialSignal::UncommittedWork)
        && !signals.contains(&PartialSignal::HandoffMalformed)
    {
        cause = Some(TerminalCause::StoppedNoWork);
        let previous = reason.take().unwrap_or_default();
        reason = Some(format!(
            "{}; {}",
            TerminalCause::StoppedNoWork.detail(),
            previous
        ));
    }

    // 8. A denial is a diagnostic, not part of the provider's answer.
    if let Some(denial) = &host.denial {
        append(&mut reason, denial.clone());
        if outcome == Outcome::Succeeded {
            outcome = Outcome::ReviewRequired;
        }
    }

    // 9. Cancellation custody decides between cancelled and unsettled.
    if !custody.cancelled && outcome == Outcome::Cancelled {
        outcome = Outcome::Unsettled;
        reason = Some(UNCANCELLED_REASON.into());
    } else if custody.cancelled {
        outcome = Outcome::Cancelled;
        if custody.steered && cause != Some(TerminalCause::IdentityMismatch) {
            cause = Some(TerminalCause::Steered);
            reason = Some(TerminalCause::Steered.detail().into());
        }
    }

    // 10. Provider identity is a host fact; a policy effect stays configurable.
    let identity = &custody.identity;
    if identity.conflict {
        outcome = Outcome::Failed;
        complete = false;
        cause = Some(TerminalCause::IdentityMismatch);
        reason = Some(TerminalCause::IdentityMismatch.detail().into());
    } else {
        if matches!(
            outcome,
            Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
        ) {
            match &identity.incomplete {
                IdentityPolicyEffect::Warn { message } => {
                    append(&mut reason, message.clone());
                    signals.push(PartialSignal::IdentityUnobserved);
                }
                IdentityPolicyEffect::Unsettle => {
                    outcome = Outcome::Unsettled;
                    complete = false;
                    if !matches!(cause, Some(TerminalCause::ProviderCaptureDefect { .. })) {
                        cause = None;
                        reason = Some(IDENTITY_INCOMPLETE_REASON.into());
                    }
                }
                IdentityPolicyEffect::Complete | IdentityPolicyEffect::Proceed => {}
            }
        }
        if identity.eof_fragment_unsettles && cause != Some(TerminalCause::IdentityMismatch) {
            outcome = Outcome::Unsettled;
            complete = false;
            if !matches!(cause, Some(TerminalCause::ProviderCaptureDefect { .. })) {
                cause = None;
                reason = Some(IDENTITY_EOF_REASON.into());
            }
        }
    }

    // 11. A sealed stall policy names the typed cause; provider text never does.
    if let Some(stall) = custody.stall {
        outcome = Outcome::Failed;
        cause = Some(stall);
        reason = Some(stall.detail().into());
    }

    // 12. Uncertain custody does not keep a writer's other typed causes.
    if matches!(
        outcome,
        Outcome::Cancelled | Outcome::Lost | Outcome::Unsettled
    ) && !matches!(
        cause,
        Some(
            TerminalCause::Steered
                | TerminalCause::IdentityMismatch
                | TerminalCause::ProviderCaptureDefect { .. }
                | TerminalCause::ProviderStalled
                | TerminalCause::ProviderStalledRetry
        )
    ) {
        cause = None;
    }

    // 13. Terminal evidence follows the custody ladder, not the prose.
    let terminal_evidence = if custody.import_cancelled {
        TerminalEvidence::ImportCancellation
    } else if outcome == Outcome::Unsettled {
        TerminalEvidence::Unsettled {
            reason: reason
                .clone()
                .unwrap_or_else(|| Outcome::Unsettled.diagnostic_reason().to_owned()),
        }
    } else if outcome == Outcome::Cancelled
        && cause != Some(TerminalCause::Steered)
        && custody.cancellation_delivered == Some(true)
    {
        TerminalEvidence::EffectiveCancellation
    } else if custody.process_exit {
        TerminalEvidence::ProcessExit
    } else {
        TerminalEvidence::PreLaunchRefusal
    };

    // 14. One reason, then one stop, source and graph result from that reason.
    let settlement_reason = derived_reason(
        outcome,
        &terminal_evidence,
        &signals,
        cause,
        &declaration,
        custody.launched,
    );
    let outcome = settlement_reason.rule().terminal_outcome.apply(outcome);
    let stop = match Stop::sealed(cause, outcome, settlement_reason, declaration.clone()) {
        Ok(stop) => stop,
        Err(_) => Stop::sealed(cause, outcome, settlement_reason, Ok(None)).unwrap_or(None),
    };
    let stop_cause = stop.as_ref().map(Stop::cause);
    let source = stop_cause.map_or(settlement_reason.rule().source, StopCause::source);
    let node: SettlementNodeOutcome = settlement_reason.node_outcome(stop_cause);

    // 15. Progress is the sealed head against the sealed launch pin only.
    let progress = evidence.git.as_ref().map(|git| git.progress());
    signals.sort();
    signals.dedup();

    OutcomeFact {
        outcome,
        cause,
        settlement_reason,
        stop,
        source,
        node,
        terminal_evidence,
        signals,
        progress,
        handoff: declaration,
        reason,
        terminal_note,
        complete,
        delivery_eligible,
    }
}

/// The budget success gate: a done, accepted declaration with a clean outcome.
/// This is the `CompletedBeforeBudget` arm of the old derive, which was the
/// only reason whose declared terminal effect was `Succeeded`.
pub fn budget_completed(outcome: Outcome, handoff: &DeclarationEvidence) -> bool {
    matches!(
        outcome,
        Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Failed | Outcome::TimedOut
    ) && matches!(handoff, DeclarationEvidence::Accepted(sealed)
        if sealed.handoff.status == HandoffStatus::Done && sealed.handoff.validate().is_ok())
}
