//! The one pure decider over a sealed [`SettlementEvidence`] record.
//!
//! Nothing outside this module writes a launched attempt's outcome, cause,
//! settlement reason, stop or partial signals. Every note string and every
//! ordering decision is carried over from the holder's former terminal ladder
//! and the store's former classification; this is that ladder over one record.

use provenant_core::bounded_line;

use crate::declaration::DeclarationEvidence;
use crate::handoff::{HandoffResult, HandoffStatus};
use crate::provenance::Sourced;
use crate::run::{
    CauseKind, DependantRelease, Outcome, SettlementClass, SettlementSignal, TerminalBasis,
};
use crate::settlement::policy::{AppliedPolicy, NoWork, SettlementPolicy, SignalAction};
use crate::settlement::{
    HandoffSource, HeadProgress, IdentityPolicyEffect, LaneRole, OutcomeFact, PrimaryCheckout,
    SettlementEvidence,
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
    evidence: &TerminalBasis,
    partial: &[SettlementSignal],
    cause: Option<CauseKind>,
    handoff: &HandoffResult,
    launched: bool,
) -> SettlementClass {
    if outcome == Outcome::Lost {
        return SettlementClass::Lost;
    }
    if outcome == Outcome::Unsettled {
        return SettlementClass::Unsettled;
    }
    if outcome == Outcome::Unclassified {
        return SettlementClass::Unclassified;
    }
    if matches!(evidence, TerminalBasis::Lost { .. }) {
        return SettlementClass::Lost;
    }
    if matches!(evidence, TerminalBasis::Unsettled { .. }) {
        return SettlementClass::Unsettled;
    }
    if outcome == Outcome::Cancelled && cause == Some(CauseKind::Steered) {
        return SettlementClass::Steered;
    }
    if outcome == Outcome::Cancelled {
        return SettlementClass::Cancelled;
    }
    if !launched || matches!(evidence, TerminalBasis::PreLaunchRefusal) {
        return SettlementClass::PreLaunchRefusal;
    }
    if handoff.is_err() {
        return SettlementClass::HandoffMalformed;
    }
    match cause {
        Some(CauseKind::EmptyWriterResult) => return SettlementClass::EmptyResult,
        Some(CauseKind::Blocked | CauseKind::StoppedNoWork) => {
            return SettlementClass::DeclaredStop;
        }
        Some(
            CauseKind::ProviderEarlyStop
            | CauseKind::ProviderStalled
            | CauseKind::ProviderStalledRetry,
        ) => return SettlementClass::ProviderStop,
        _ => {}
    }
    if partial.contains(&SettlementSignal::StrayWork) {
        return SettlementClass::StrayWork;
    }
    if handoff
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .is_some_and(crate::handoff::Handoff::unfinished)
    {
        return SettlementClass::DeclaredStop;
    }
    if partial.contains(&SettlementSignal::DeclaredStop) {
        return SettlementClass::DeclaredStop;
    }
    if partial.contains(&SettlementSignal::HandoffMissing) {
        return SettlementClass::HandoffMissing;
    }
    if partial.contains(&SettlementSignal::ProviderStop) {
        return SettlementClass::ProviderStop;
    }
    if partial.contains(&SettlementSignal::UncommittedWork) || outcome == Outcome::Uncommitted {
        return SettlementClass::UncommittedWork;
    }
    if cause == Some(CauseKind::TokenBudget)
        && matches!(
            outcome,
            Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Failed | Outcome::TimedOut
        )
        && handoff
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .is_some_and(|accepted| !accepted.unfinished() && accepted.validate().is_ok())
    {
        return SettlementClass::CompletedBeforeBudget;
    }
    if matches!(outcome, Outcome::Succeeded | Outcome::ReviewRequired) {
        return SettlementClass::Completed;
    }
    SettlementClass::Failed
}

/// Settle one launched attempt from a sealed evidence record. Pure: no I/O, no
/// clock and no reads of remote refs, pushes or prose.
pub fn settle(evidence: &SettlementEvidence) -> OutcomeFact {
    let host = &evidence.host;
    let custody = &evidence.custody;
    let writer = !matches!(evidence.lane, LaneRole::Reader);

    let policy = &evidence.policy;
    let shipped = SettlementPolicy::shipped();
    let mut applied: Vec<AppliedPolicy> = Vec::new();

    let mut outcome = host.outcome;
    let mut cause = host.cause;
    let mut reason = host.reason.clone();
    let mut terminal_note = host.terminal_note.clone();
    let mut complete = host.complete;
    let mut signals = host.signals.clone();

    // A recorded signal is a fact. An action changes the outcome and whether
    // the signal reaches the class ladder; only a non-default action that
    // actually changed one of those is reported with its source.
    fn record_action(
        applied: &mut Vec<AppliedPolicy>,
        shipped: SignalAction,
        sourced: &Sourced<SignalAction>,
        signal: SettlementSignal,
        changed: bool,
    ) {
        if sourced.value != shipped && changed {
            applied.push(AppliedPolicy {
                signal,
                action: sourced.value,
                source: sourced.source.clone(),
            });
        }
    }
    fn classed(action: SignalAction) -> bool {
        !matches!(
            action,
            SignalAction::Fail | SignalAction::Warn | SignalAction::Off
        )
    }

    // 1. Edits left on the worktree are not a success. The census note is the
    // only change to the reason here; slice 5a no longer mutates the host state.
    if writer && outcome == Outcome::Succeeded {
        if let Some(git) = &evidence.git
            && let Some(note) = &git.uncommitted
        {
            let action = policy.uncommitted_work.value;
            let before = outcome;
            outcome = match action {
                SignalAction::Fail => Outcome::Failed,
                SignalAction::Review => Outcome::ReviewRequired,
                SignalAction::Warn | SignalAction::Off => outcome,
                SignalAction::Hold => Outcome::Uncommitted,
            };
            signals.push(SettlementSignal::UncommittedWork);
            if action != SignalAction::Off {
                append(&mut reason, note.clone());
            }
            record_action(
                &mut applied,
                shipped.uncommitted_work.value,
                &policy.uncommitted_work,
                SettlementSignal::UncommittedWork,
                outcome != before || !classed(action),
            );
        }
    }

    // 2. A move in the primary checkout is evidence, not an early decision.
    if let Some(git) = &evidence.git {
        match &git.primary {
            PrimaryCheckout::Review { note } => {
                let action = policy.stray_work.value;
                let before = outcome;
                outcome = match action {
                    SignalAction::Fail => Outcome::Failed,
                    SignalAction::Review => Outcome::ReviewRequired,
                    SignalAction::Warn | SignalAction::Off => outcome,
                    SignalAction::Hold => Outcome::Uncommitted,
                };
                signals.push(SettlementSignal::StrayWork);
                if action != SignalAction::Off {
                    append(&mut reason, note.clone());
                }
                record_action(
                    &mut applied,
                    shipped.stray_work.value,
                    &policy.stray_work,
                    SettlementSignal::StrayWork,
                    outcome != before || !classed(action),
                );
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
            let action = policy.empty_result.value;
            let before = outcome;
            cause = Some(CauseKind::EmptyWriterResult);
            outcome = match action {
                SignalAction::Fail => Outcome::Failed,
                SignalAction::Review if outcome == Outcome::Succeeded => Outcome::ReviewRequired,
                SignalAction::Review
                | SignalAction::Warn
                | SignalAction::Off
                | SignalAction::Hold => outcome,
            };
            if action != SignalAction::Off {
                append(&mut reason, CauseKind::EmptyWriterResult.detail());
            }
            record_action(
                &mut applied,
                shipped.empty_result.value,
                &policy.empty_result,
                SettlementSignal::HandoffMissing,
                outcome != before || !classed(action),
            );
        } else {
            // A reader's only product is its answer, so an empty one stays
            // failed under every value of the writer-facing knob.
            outcome = Outcome::Failed;
            reason = Some(EMPTY_RESULT_REASON.into());
        }
    }

    // 4. Delivery keys off the outcome before a declared stop.
    let delivery_eligible = outcome == Outcome::Succeeded;

    // 5. A host-synthesised stop outranks a declaration; otherwise the last
    // accepted journal line is the declaration, the rejected lines its error.
    let stopped = host.host_handoff.clone();
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
        .and_then(Option::as_ref)
        .is_some_and(crate::handoff::Handoff::unfinished);

    if let Err(error) = &declaration {
        let action = policy.handoff_malformed.value;
        let before = outcome;
        signals.push(SettlementSignal::HandoffMalformed);
        if action != SignalAction::Off {
            append(&mut reason, error.to_string());
        }
        outcome = match action {
            SignalAction::Fail => Outcome::Failed,
            SignalAction::Review
                if matches!(outcome, Outcome::Succeeded | Outcome::Uncommitted) =>
            {
                Outcome::ReviewRequired
            }
            SignalAction::Review | SignalAction::Warn | SignalAction::Off | SignalAction::Hold => {
                outcome
            }
        };
        record_action(
            &mut applied,
            shipped.handoff_malformed.value,
            &policy.handoff_malformed,
            SettlementSignal::HandoffMalformed,
            outcome != before || !classed(action),
        );
    }
    if cause == Some(CauseKind::EmptyWriterResult) {
        signals.push(SettlementSignal::HandoffMissing);
    }
    if matches!(
        outcome,
        Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
    ) && evidence.lane == LaneRole::ModelWriter
        && stopped.is_none()
        && matches!(&evidence.handoff, DeclarationEvidence::Missing)
    {
        let action = policy.handoff_missing.value;
        let before = outcome;
        signals.push(SettlementSignal::HandoffMissing);
        outcome = match action {
            SignalAction::Fail => Outcome::Failed,
            SignalAction::Review if outcome == Outcome::Succeeded => {
                reason = Some(NO_HANDOFF_REASON.into());
                Outcome::ReviewRequired
            }
            SignalAction::Review | SignalAction::Warn | SignalAction::Off | SignalAction::Hold => {
                outcome
            }
        };
        record_action(
            &mut applied,
            shipped.handoff_missing.value,
            &policy.handoff_missing,
            SettlementSignal::HandoffMissing,
            outcome != before || !classed(action),
        );
    }

    if matches!(
        outcome,
        Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
    ) {
        let mut notes = Vec::new();
        if let Some(handoff) = declaration
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .filter(|handoff| handoff.unfinished())
        {
            signals.push(SettlementSignal::DeclaredStop);
            if outcome == Outcome::Succeeded {
                outcome = Outcome::ReviewRequired;
            }
            notes.push(format!("lane stopped early: {}", bounded(&handoff.note)));
        }
        if outcome == Outcome::Uncommitted {
            signals.push(SettlementSignal::UncommittedWork);
        }
        signals.sort();
        signals.dedup();
        if !signals.is_empty() {
            let labels: Vec<_> = signals.iter().map(|signal| signal.as_str()).collect();
            notes.insert(0, format!("partial finish ({})", labels.join(", ")));
            append(&mut reason, notes.join(": "));
        }
    } else if unfinished {
        signals.push(SettlementSignal::DeclaredStop);
        signals.sort();
        signals.dedup();
    } else {
        signals.clear();
    }

    // 6. A blocked writer settles with its own cause and its bounded note.
    if writer
        && (cause.is_none() || cause == Some(CauseKind::Blocked))
        && matches!(
            outcome,
            Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
        )
        && let Some(blocked) = declaration
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .filter(|handoff| handoff.status == HandoffStatus::Blocked)
    {
        cause = Some(CauseKind::Blocked);
        terminal_note = Some(bounded(&blocked.note));
    }

    // 7. No-work is a launch-pinned Git fact: the head is known and equal to
    // the pin, and no other partial signal explains the stop. Pushes, remote
    // refs and an unknown head never produce it.
    let declared_stopped_early = declaration
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .is_some_and(|handoff| handoff.status == HandoffStatus::StoppedEarly);
    let no_work_applies = writer
        && outcome == Outcome::ReviewRequired
        && declared_stopped_early
        && cause.is_none()
        && evidence
            .git
            .as_ref()
            .is_some_and(|git| git.head_progress() == HeadProgress::Unchanged)
        && !signals.contains(&SettlementSignal::StrayWork)
        && !signals.contains(&SettlementSignal::UncommittedWork)
        && !signals.contains(&SettlementSignal::HandoffMalformed);
    if no_work_applies && policy.no_work.value == NoWork::Detect {
        cause = Some(CauseKind::StoppedNoWork);
        let previous = reason.take().unwrap_or_default();
        reason = Some(format!(
            "{}; {}",
            CauseKind::StoppedNoWork.detail(),
            previous
        ));
    }
    if no_work_applies && policy.no_work.value != shipped.no_work.value {
        applied.push(AppliedPolicy {
            signal: SettlementSignal::DeclaredStop,
            action: SignalAction::Off,
            source: policy.no_work.source.clone(),
        });
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
        if custody.steered && cause != Some(CauseKind::IdentityMismatch) {
            cause = Some(CauseKind::Steered);
            reason = Some(CauseKind::Steered.detail().into());
        }
    }

    // 10. Provider identity is a host fact; a policy effect stays configurable.
    let identity = &custody.identity;
    if identity.conflict {
        outcome = Outcome::Failed;
        complete = false;
        cause = Some(CauseKind::IdentityMismatch);
        reason = Some(CauseKind::IdentityMismatch.detail().into());
    } else {
        if matches!(
            outcome,
            Outcome::Succeeded | Outcome::ReviewRequired | Outcome::Uncommitted
        ) {
            match &identity.incomplete {
                IdentityPolicyEffect::Warn { message } => {
                    append(&mut reason, message.clone());
                    signals.push(SettlementSignal::IdentityUnobserved);
                }
                IdentityPolicyEffect::Unsettle => {
                    outcome = Outcome::Unsettled;
                    complete = false;
                    if !matches!(cause, Some(CauseKind::ProviderCaptureDefect { .. })) {
                        cause = None;
                        reason = Some(IDENTITY_INCOMPLETE_REASON.into());
                    }
                }
                IdentityPolicyEffect::Complete | IdentityPolicyEffect::Proceed => {}
            }
        }
        if identity.eof_fragment_unsettles && cause != Some(CauseKind::IdentityMismatch) {
            outcome = Outcome::Unsettled;
            complete = false;
            if !matches!(cause, Some(CauseKind::ProviderCaptureDefect { .. })) {
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
            CauseKind::Steered
                | CauseKind::IdentityMismatch
                | CauseKind::ProviderCaptureDefect { .. }
                | CauseKind::ProviderStalled
                | CauseKind::ProviderStalledRetry
        )
    ) {
        cause = None;
    }

    // 13. Terminal evidence follows the custody ladder, not the prose.
    let terminal_basis = if custody.import_cancelled {
        TerminalBasis::ImportCancellation
    } else if outcome == Outcome::Unsettled {
        TerminalBasis::Unsettled {
            reason: reason
                .clone()
                .unwrap_or_else(|| Outcome::Unsettled.diagnostic_reason().to_owned()),
        }
    } else if outcome == Outcome::Cancelled
        && cause != Some(CauseKind::Steered)
        && custody.cancellation_delivered == Some(true)
    {
        TerminalBasis::EffectiveCancellation
    } else if custody.process_exit {
        TerminalBasis::ProcessExit
    } else {
        TerminalBasis::PreLaunchRefusal
    };

    // 14. One reason, then one stop, source and graph result from that reason.
    // Every signal stays on the fact. A non-releasing action (fail, warn, off)
    // withholds its signal and cause from the class ladder, so the class is a
    // plain failure or a completed lane instead of the recorded partial state.
    let empty_result_missing = cause == Some(CauseKind::EmptyWriterResult);
    let class_signals: Vec<SettlementSignal> = signals
        .iter()
        .copied()
        .filter(|signal| match signal {
            SettlementSignal::UncommittedWork => classed(policy.uncommitted_work.value),
            SettlementSignal::StrayWork => classed(policy.stray_work.value),
            SettlementSignal::HandoffMalformed => classed(policy.handoff_malformed.value),
            SettlementSignal::HandoffMissing => {
                classed(policy.handoff_missing.value)
                    && (!empty_result_missing || classed(policy.empty_result.value))
            }
            _ => true,
        })
        .collect();
    let class_cause =
        if cause == Some(CauseKind::EmptyWriterResult) && !classed(policy.empty_result.value) {
            None
        } else {
            cause
        };
    let class_declaration: HandoffResult =
        if declaration.is_err() && !classed(policy.handoff_malformed.value) {
            Ok(None)
        } else {
            declaration.clone()
        };
    let settlement_class = derived_reason(
        outcome,
        &terminal_basis,
        &class_signals,
        class_cause,
        &class_declaration,
        custody.launched,
    );
    let outcome = settlement_class.rule().terminal_outcome.apply(outcome);
    let stop = match Stop::sealed(cause, outcome, settlement_class, declaration.clone()) {
        Ok(stop) => stop,
        Err(_) => Stop::sealed(cause, outcome, settlement_class, Ok(None)).unwrap_or(None),
    };
    let stop_cause = stop.as_ref().map(Stop::cause);
    let source = stop_cause.map_or(settlement_class.rule().source, StopCause::source);
    let release: DependantRelease = settlement_class.release(stop_cause);

    // 15. HeadProgress is the sealed head against the sealed launch pin only.
    let head_progress = evidence.git.as_ref().map(|git| git.head_progress());
    signals.sort();
    signals.dedup();

    // Provenance is a fact about where the settled handoff came from. A host
    // stop outranks a declaration; an accepted declaration is the lane's own.
    let handoff_source = if stopped.is_some() {
        Some(HandoffSource::Host)
    } else if matches!(&evidence.handoff, DeclarationEvidence::Accepted(_)) {
        Some(HandoffSource::Declared)
    } else {
        None
    };
    // Adapters owe no declaration, so their count is absent rather than zero.
    let declarations = (evidence.lane != LaneRole::AdapterWriter).then_some(evidence.declarations);

    OutcomeFact {
        outcome,
        cause,
        settlement_class,
        stop,
        cause_source: source,
        release,
        terminal_basis,
        signals,
        head_progress,
        handoff: declaration,
        handoff_source,
        declarations,
        applied,
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
