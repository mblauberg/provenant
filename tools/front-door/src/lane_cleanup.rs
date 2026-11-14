//! Removal of the refs finished lanes leave behind (#3039).
//!
//! The candidates are the refs the shared ledger records as created by
//! Provenant: local branches, and remote branches at the endpoint a delivery
//! bound. A ref is removed only once the shared landed check proves it landed.
//! Unlanded refs are reported and retained regardless of age. Everything else is
//! retained: adopted or unobserved refs, a branch any worktree has checked
//! out, the integration ref itself and a ref that moved since it was checked.
//! Every deletion is leased on the tip just checked. Each artefact reports one
//! outcome; a failure is reported and never fails the caller.
use crate::gc_support::host_git_with_stderr;
use provenant_confinement::repo::GitRefName;
use provenant_confinement::worktrees::{
    self, Artefact, ArtefactOutcome, ArtefactState, IntegrationEdge, Landing, LandingTarget,
    RefDeletion, RefLedger, RefRecord, RetirementIntent, RetirementPort, Unproven, WorktreeContext,
};
use provenant_core::{CancelToken, GitOid};
use provenant_os::exec::Termination;
use provenant_records::custody::{
    LandedEvidence, LandingPredicate, LandingSource, RefEndpoint, RefOrigin, RemovalEvidence,
    RemovalOutcome, RemovalRecord, RemovalUncertainty, RetirementDecision, RetirementRecord,
    RetirementStep, RetirementStepState,
};
use std::time::Duration;

const REMOTE_READ: Duration = Duration::from_secs(30);
const REMOTE_WRITE: Duration = Duration::from_secs(60);

/// The policy one pass applies.
pub(crate) struct Sweep {
    pub(crate) dry_run: bool,
}

impl Sweep {
    pub(crate) fn new(dry_run: bool) -> Self {
        Self { dry_run }
    }
}

/// One typed landing decision, with the exact evidence or uncertainty used.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LandingDecision {
    Landed(LandedEvidence),
    Keep {
        reason: &'static str,
        cause: RemovalUncertainty,
    },
}

fn landing_evidence(target: &LandingTarget) -> Option<LandedEvidence> {
    Some(LandedEvidence {
        predicate: LandingPredicate::Landed,
        source: LandingSource::IntegrationTarget {
            reference: target.reference.as_ref()?.as_str().to_owned(),
            commit: target.tip,
        },
    })
}

fn ref_removal(
    project: provenant_core::ProjectId,
    reference: &provenant_records::custody::RefOwnership,
    tip: GitOid,
    outcome: RemovalOutcome,
) -> Result<RemovalRecord, worktrees::WorktreeError> {
    let subject = reference
        .custody_subject()
        .map_err(|_| worktrees::WorktreeError::InvalidRequest)?;
    Ok(RemovalRecord {
        project,
        subject,
        tip,
        outcome,
    })
}

fn kept_decision(reason: &'static str, cause: RemovalUncertainty) -> LandingDecision {
    LandingDecision::Keep { reason, cause }
}

fn landed_on_any(
    context: &WorktreeContext<'_>,
    tip: GitOid,
    targets: &[LandingTarget],
    chain: &[IntegrationEdge],
    cancel: &CancelToken,
) -> LandingDecision {
    if targets.is_empty() {
        return kept_decision("target_unavailable", RemovalUncertainty::UnprovenLanding);
    }
    let mut observed_unproven = false;
    let mut missing_commit = false;
    for target in targets {
        match worktrees::landing_in_repo(context, tip, target, chain, cancel) {
            Ok(Landing::Landed(_)) => {
                if let Some(evidence) = landing_evidence(target) {
                    return LandingDecision::Landed(evidence);
                }
            }
            Ok(Landing::Unproven(Unproven::MissingCommit { .. })) => {
                observed_unproven = true;
                missing_commit = true;
            }
            Ok(Landing::Unproven(_)) => observed_unproven = true,
            Err(_) => {}
        }
    }
    kept_decision(
        if observed_unproven {
            "not_landed"
        } else {
            "target_unavailable"
        },
        if missing_commit {
            RemovalUncertainty::MissingCommit
        } else {
            RemovalUncertainty::UnprovenLanding
        },
    )
}

fn warn(outcome: &ArtefactOutcome) {
    if outcome.outcome == ArtefactState::Failed {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "warning: could not remove {}: {}; it is retained",
            outcome.name,
            outcome.reason.as_deref().unwrap_or("unknown error")
        );
    }
}

/// Reuse the durable owner for this exact RefLedger generation, or write its
/// pre-removal intent before the first mutation.
fn retirement_for_ref(
    port: &dyn RetirementPort,
    project: provenant_core::ProjectId,
    ownership: &provenant_records::custody::RefOwnership,
    tip: GitOid,
    related_subjects: Vec<provenant_records::custody::Subject>,
    evidence: LandedEvidence,
) -> Result<Option<RetirementRecord>, worktrees::WorktreeError> {
    let subject = ownership
        .custody_subject()
        .map_err(|_| worktrees::WorktreeError::InvalidRequest)?;
    let Some(record) = port.begin_retirement(RetirementIntent {
        project,
        related_subjects,
        reference: ownership.clone(),
        expected_tip: tip,
        evidence,
    })?
    else {
        return Ok(None);
    };
    if record.project != project
        || record.subject != subject
        || record.expected_tip != tip
        || !record
            .reference
            .matches_ownership(ownership)
            .map_err(|_| worktrees::WorktreeError::InvalidRequest)?
    {
        return Err(worktrees::WorktreeError::IdentityChanged);
    }
    Ok(Some(record))
}

struct RefRetirementState {
    expected_tip: GitOid,
    record: Option<RetirementRecord>,
}

/// Use a durable retirement's pinned tip when resuming this exact ownership
/// generation; otherwise the ledger's creation observation remains the fence.
fn ref_retirement_state(
    port: &dyn RetirementPort,
    project: provenant_core::ProjectId,
    ownership: &provenant_records::custody::RefOwnership,
    ledger_tip: GitOid,
) -> Result<RefRetirementState, worktrees::WorktreeError> {
    let subject = ownership
        .custody_subject()
        .map_err(|_| worktrees::WorktreeError::InvalidRequest)?;
    let record = port.retirement_for_subject(project, &subject)?;
    if let Some(record) = &record {
        let same_generation = record
            .reference
            .matches_ownership(ownership)
            .map_err(|_| worktrees::WorktreeError::InvalidRequest)?;
        if record.project != project || record.subject != subject || !same_generation {
            return Err(worktrees::WorktreeError::IdentityChanged);
        }
    }
    Ok(RefRetirementState {
        expected_tip: record
            .as_ref()
            .map_or(ledger_tip, |record| record.expected_tip),
        record,
    })
}

fn reference_step(
    record: &RetirementRecord,
) -> Result<&RetirementStepState, worktrees::WorktreeError> {
    record
        .outcomes
        .iter()
        .find(|outcome| outcome.step == RetirementStep::Reference)
        .map(|outcome| &outcome.state)
        .ok_or(worktrees::WorktreeError::RegistrationUnconfirmed)
}

fn update_reference_step(
    port: &dyn RetirementPort,
    record: &RetirementRecord,
    state: RetirementStepState,
) -> Result<RetirementRecord, worktrees::WorktreeError> {
    let current = reference_step(record)?.clone();
    if current == state || matches!(&current, RetirementStepState::Completed { .. }) {
        return Ok(record.clone());
    }
    let record = if matches!(
        &current,
        RetirementStepState::Refused { .. } | RetirementStepState::Unknown { .. }
    ) {
        port.advance_retirement(
            record,
            RetirementStep::Reference,
            RetirementStepState::Pending,
        )?
    } else {
        record.clone()
    };
    let current = reference_step(&record)?;
    if current == &state || matches!(current, RetirementStepState::Completed { .. }) {
        return Ok(record);
    }
    port.advance_retirement(&record, RetirementStep::Reference, state)
}

fn removal_is_complete(record: &RetirementRecord) -> Option<RemovalRecord> {
    let removal = record.removal();
    matches!(&removal.outcome, RemovalOutcome::Removed { .. }).then_some(removal)
}

fn recovered_ref_outcome(
    port: &dyn RetirementPort,
    state: RefRetirementState,
    tip: GitOid,
    evidence: Option<LandedEvidence>,
    kind: Artefact,
    name: &str,
    remote: Option<&str>,
    ledger: &RefLedger,
    record: &RefRecord,
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> ArtefactOutcome {
    let out = |state| {
        let outcome = ArtefactOutcome::new(kind, name, state);
        if let Some(remote) = remote {
            outcome.with_remote(remote)
        } else {
            outcome
        }
    };
    if state.expected_tip != tip {
        return out(ArtefactState::Retained)
            .with_reason("retirement_identity_changed")
            .with_head(tip);
    }
    let retirement = match state.record {
        Some(retirement) => retirement,
        None => {
            if let Err(error) = ledger.forget(record, context.clock, cancel) {
                return out(ArtefactState::Failed).with_reason(error.to_string());
            }
            return out(ArtefactState::Absent);
        }
    };
    if retirement.expected_tip != tip {
        return out(ArtefactState::Retained)
            .with_reason("retirement_identity_changed")
            .with_removal(retirement.removal());
    }
    let retirement = if matches!(
        reference_step(&retirement),
        Ok(RetirementStepState::Completed { .. })
    ) {
        retirement
    } else if let Some(evidence) = evidence {
        match update_reference_step(
            port,
            &retirement,
            RetirementStepState::Completed {
                evidence: RemovalEvidence::Landing { evidence },
            },
        ) {
            Ok(retirement) => retirement,
            Err(error) => return out(ArtefactState::Failed).with_reason(error.to_string()),
        }
    } else {
        retirement
    };
    if let Some(removal) = removal_is_complete(&retirement) {
        if let Err(error) = ledger.forget(record, context.clock, cancel) {
            return out(ArtefactState::Failed)
                .with_reason(error.to_string())
                .with_head(tip)
                .with_removal(removal);
        }
        return out(ArtefactState::Removed)
            .with_reason("retirement_resumed")
            .with_head(tip)
            .with_removal(removal);
    }
    out(ArtefactState::Retained)
        .with_reason("retirement_incomplete")
        .with_head(tip)
        .with_removal(retirement.removal())
}

/// Remove one created local branch when its live tip matches the durable
/// retirement, or the ledger's creation observation when no intent exists.
pub(crate) fn sweep_local(
    context: &WorktreeContext<'_>,
    ledger: &RefLedger,
    record: &RefRecord,
    targets: &[LandingTarget],
    sweep: &Sweep,
    retirement: &dyn RetirementPort,
    cancel: &CancelToken,
) -> ArtefactOutcome {
    let outcome = local_outcome(context, ledger, record, targets, sweep, retirement, cancel);
    warn(&outcome);
    outcome
}

fn local_outcome(
    context: &WorktreeContext<'_>,
    ledger: &RefLedger,
    record: &RefRecord,
    targets: &[LandingTarget],
    sweep: &Sweep,
    retirement: &dyn RetirementPort,
    cancel: &CancelToken,
) -> ArtefactOutcome {
    let name = record.reference.clone();
    let out = |state| ArtefactOutcome::new(Artefact::LocalBranch, name.clone(), state);
    if record.origin != RefOrigin::Created || record.endpoint != RefEndpoint::Local {
        return out(ArtefactState::Retained).with_reason("not_created");
    }
    let Ok(reference) = GitRefName::new(&record.reference) else {
        return out(ArtefactState::Retained).with_reason("invalid_ref");
    };
    if targets
        .iter()
        .any(|target| target.reference.as_ref() == Some(&reference))
    {
        return out(ArtefactState::Retained).with_reason("integration_ref");
    }
    let ownership = record.ownership();
    let retirement_state = match ref_retirement_state(
        retirement,
        context.binding.project_id(),
        &ownership,
        record.tip,
    ) {
        Ok(state) => state,
        Err(error) => return out(ArtefactState::Failed).with_reason(error.to_string()),
    };
    let tip = match worktrees::ref_tip(context, &reference, cancel) {
        Ok(Some(tip)) => tip,
        Ok(None) => {
            let expected_tip = retirement_state.expected_tip;
            let evidence = match landed_on_any(context, expected_tip, targets, &[], cancel) {
                LandingDecision::Landed(evidence) => Some(evidence),
                LandingDecision::Keep { .. } => None,
            };
            let recovered = recovered_ref_outcome(
                retirement,
                retirement_state,
                expected_tip,
                evidence,
                Artefact::LocalBranch,
                &name,
                None,
                ledger,
                record,
                context,
                cancel,
            );
            return recovered;
        }
        Err(error) => return out(ArtefactState::Failed).with_reason(error.to_string()),
    };
    if tip != retirement_state.expected_tip {
        let mut outcome = out(ArtefactState::Retained)
            .with_reason("changed")
            .with_head(tip);
        if let Some(retirement) = retirement_state.record {
            outcome = outcome.with_removal(retirement.removal());
        }
        return outcome;
    }
    match worktrees::branch_checked_out(context, &reference, cancel) {
        Ok(false) => {}
        Ok(true) => return out(ArtefactState::Retained).with_reason("checked_out"),
        Err(error) => return out(ArtefactState::Failed).with_reason(error.to_string()),
    }
    let decision = landed_on_any(context, tip, targets, &[], cancel);
    if let LandingDecision::Keep { reason, cause } = decision {
        let removal = match ref_removal(
            context.binding.project_id(),
            &ownership,
            tip,
            RemovalOutcome::Kept { cause },
        ) {
            Ok(removal) => removal,
            Err(error) => return out(ArtefactState::Failed).with_reason(error.to_string()),
        };
        return out(ArtefactState::Retained)
            .with_reason(reason)
            .with_head(tip)
            .with_removal(removal);
    }
    let LandingDecision::Landed(evidence) = decision else {
        return out(ArtefactState::Failed).with_reason("landing decision was incomplete");
    };
    if sweep.dry_run {
        return out(ArtefactState::WouldRemove)
            .with_reason("landed")
            .with_head(tip);
    }
    let retirement_record = match retirement_for_ref(
        retirement,
        context.binding.project_id(),
        &ownership,
        tip,
        Vec::new(),
        evidence.clone(),
    ) {
        Ok(Some(retirement_record)) => retirement_record,
        Ok(None) => return out(ArtefactState::Retained).with_reason("custody_unowned"),
        Err(error) => return out(ArtefactState::Failed).with_reason(error.to_string()),
    };
    if matches!(
        reference_step(&retirement_record),
        Ok(RetirementStepState::Completed { .. })
    ) {
        return out(ArtefactState::Retained)
            .with_reason("retirement_completed_ref_present")
            .with_head(tip)
            .with_removal(retirement_record.removal());
    }
    if !matches!(
        &retirement_record.decision,
        RetirementDecision::Discard { .. }
    ) {
        return out(ArtefactState::Retained)
            .with_reason("retirement_preserved")
            .with_head(tip)
            .with_removal(retirement_record.removal());
    }
    let retirement_record =
        match update_reference_step(retirement, &retirement_record, RetirementStepState::Pending) {
            Ok(record) => record,
            Err(error) => return out(ArtefactState::Failed).with_reason(error.to_string()),
        };
    match worktrees::delete_ref_leased(context, &reference, tip, cancel) {
        Ok(RefDeletion::Removed) => match update_reference_step(
            retirement,
            &retirement_record,
            RetirementStepState::Completed {
                evidence: RemovalEvidence::Landing { evidence },
            },
        ) {
            Ok(retirement_record) => {
                if let Err(error) = ledger.forget(record, context.clock, cancel) {
                    return out(ArtefactState::Failed)
                        .with_reason(error.to_string())
                        .with_head(tip)
                        .with_removal(retirement_record.removal());
                }
                let removal = retirement_record.removal();
                if matches!(&removal.outcome, RemovalOutcome::Removed { .. }) {
                    out(ArtefactState::Removed)
                        .with_reason("landed")
                        .with_head(tip)
                        .with_removal(removal)
                } else {
                    out(ArtefactState::Retained)
                        .with_reason("retirement_incomplete")
                        .with_head(tip)
                        .with_removal(removal)
                }
            }
            Err(error) => out(ArtefactState::Failed)
                .with_reason(error.to_string())
                .with_head(tip),
        },
        Ok(RefDeletion::Absent) => recovered_ref_outcome(
            retirement,
            RefRetirementState {
                expected_tip: tip,
                record: Some(retirement_record),
            },
            tip,
            Some(evidence),
            Artefact::LocalBranch,
            &name,
            None,
            ledger,
            record,
            context,
            cancel,
        ),
        Ok(RefDeletion::Changed) => match update_reference_step(
            retirement,
            &retirement_record,
            RetirementStepState::Refused {
                reason: "reference_tip_changed".into(),
            },
        ) {
            Ok(record) => out(ArtefactState::Retained)
                .with_reason("changed")
                .with_head(tip)
                .with_removal(record.removal()),
            Err(error) => out(ArtefactState::Failed).with_reason(error.to_string()),
        },
        Ok(RefDeletion::CheckedOut) => match update_reference_step(
            retirement,
            &retirement_record,
            RetirementStepState::Refused {
                reason: "reference_checked_out".into(),
            },
        ) {
            Ok(record) => out(ArtefactState::Retained)
                .with_reason("checked_out")
                .with_head(tip)
                .with_removal(record.removal()),
            Err(error) => out(ArtefactState::Failed).with_reason(error.to_string()),
        },
        Err(error) => match update_reference_step(
            retirement,
            &retirement_record,
            RetirementStepState::Unknown {
                reason: error.to_string(),
            },
        ) {
            Ok(record) => out(ArtefactState::Failed)
                .with_reason(error.to_string())
                .with_head(tip)
                .with_removal(record.removal()),
            Err(update_error) => out(ArtefactState::Failed)
                .with_reason(format!("{error}; retirement update failed: {update_error}")),
        },
    }
}

/// Remove one remote branch Provenant created, and its tracking ref, when it
/// has landed on `target` (through ancestry or `chain`).
pub(crate) fn sweep_remote(
    context: &WorktreeContext<'_>,
    ledger: &RefLedger,
    record: &RefRecord,
    target: Option<&LandingTarget>,
    chain: &[IntegrationEdge],
    sweep: &Sweep,
    retirement: &dyn RetirementPort,
    cancel: &CancelToken,
) -> Vec<ArtefactOutcome> {
    let outcomes = remote_outcomes(
        context, ledger, record, target, chain, sweep, retirement, cancel,
    );
    outcomes.iter().for_each(warn);
    outcomes
}

fn remote_outcomes(
    context: &WorktreeContext<'_>,
    ledger: &RefLedger,
    record: &RefRecord,
    target: Option<&LandingTarget>,
    chain: &[IntegrationEdge],
    sweep: &Sweep,
    retirement: &dyn RetirementPort,
    cancel: &CancelToken,
) -> Vec<ArtefactOutcome> {
    let RefEndpoint::Remote { remote, push_url } = &record.endpoint else {
        return Vec::new();
    };
    let name = record.reference.clone();
    let out = |state| {
        ArtefactOutcome::new(Artefact::RemoteBranch, name.clone(), state).with_remote(remote)
    };
    if record.origin != RefOrigin::Created {
        return vec![out(ArtefactState::Retained).with_reason("not_created")];
    }
    let Ok(reference) = GitRefName::new(&record.reference) else {
        return vec![out(ArtefactState::Retained).with_reason("invalid_ref")];
    };
    if target.is_some_and(|target| target.reference.as_ref() == Some(&reference)) {
        return vec![out(ArtefactState::Retained).with_reason("integration_ref")];
    }
    let tracking = GitRefName::remote_tracking(format!("{remote}/{}", reference.short_name())).ok();
    let ownership = record.ownership();
    let retirement_state = match ref_retirement_state(
        retirement,
        context.binding.project_id(),
        &ownership,
        record.tip,
    ) {
        Ok(state) => state,
        Err(error) => return vec![out(ArtefactState::Failed).with_reason(error.to_string())],
    };
    let tip = match remote_tip(context, push_url, &reference) {
        Ok(tip) => tip,
        Err(error) => return vec![out(ArtefactState::Failed).with_reason(error)],
    };
    let Some(tip) = tip else {
        let expected_tip = retirement_state.expected_tip;
        let evidence = target
            .filter(|target| {
                worktrees::landing_in_repo(context, expected_tip, target, chain, cancel)
                    .is_ok_and(|landing| landing.is_landed())
            })
            .and_then(landing_evidence);
        let recovered = recovered_ref_outcome(
            retirement,
            retirement_state,
            expected_tip,
            evidence,
            Artefact::RemoteBranch,
            &name,
            Some(remote),
            ledger,
            record,
            context,
            cancel,
        );
        let mut outcomes = vec![recovered];
        if let Some(tracking) = &tracking {
            outcomes.extend(stale_tracking(context, remote, tracking, cancel));
        }
        return outcomes;
    };
    if tip != retirement_state.expected_tip {
        let mut outcome = out(ArtefactState::Retained)
            .with_reason("changed")
            .with_head(tip);
        if let Some(retirement) = retirement_state.record {
            outcome = outcome.with_removal(retirement.removal());
        }
        return vec![outcome];
    }
    let decision = landed_on_any(
        context,
        tip,
        target.map(std::slice::from_ref).unwrap_or(&[]),
        chain,
        cancel,
    );
    if let LandingDecision::Keep { reason, cause } = decision {
        let removal = match ref_removal(
            context.binding.project_id(),
            &ownership,
            tip,
            RemovalOutcome::Kept { cause },
        ) {
            Ok(removal) => removal,
            Err(error) => return vec![out(ArtefactState::Failed).with_reason(error.to_string())],
        };
        return vec![
            out(ArtefactState::Retained)
                .with_reason(reason)
                .with_head(tip)
                .with_removal(removal),
        ];
    }
    let LandingDecision::Landed(evidence) = decision else {
        return vec![out(ArtefactState::Failed).with_reason("landing decision was incomplete")];
    };
    if sweep.dry_run {
        return vec![
            out(ArtefactState::WouldRemove)
                .with_reason("landed")
                .with_head(tip),
        ];
    }
    let retirement_record = match retirement_for_ref(
        retirement,
        context.binding.project_id(),
        &ownership,
        tip,
        Vec::new(),
        evidence.clone(),
    ) {
        Ok(Some(retirement_record)) => retirement_record,
        Ok(None) => return vec![out(ArtefactState::Retained).with_reason("custody_unowned")],
        Err(error) => return vec![out(ArtefactState::Failed).with_reason(error.to_string())],
    };
    if matches!(
        reference_step(&retirement_record),
        Ok(RetirementStepState::Completed { .. })
    ) {
        return vec![
            out(ArtefactState::Retained)
                .with_reason("retirement_completed_ref_present")
                .with_head(tip)
                .with_removal(retirement_record.removal()),
        ];
    }
    if !matches!(
        &retirement_record.decision,
        RetirementDecision::Discard { .. }
    ) {
        return vec![
            out(ArtefactState::Retained)
                .with_reason("retirement_preserved")
                .with_head(tip)
                .with_removal(retirement_record.removal()),
        ];
    }
    let retirement_record =
        match update_reference_step(retirement, &retirement_record, RetirementStepState::Pending) {
            Ok(record) => record,
            Err(error) => return vec![out(ArtefactState::Failed).with_reason(error.to_string())],
        };
    let mut outcomes = Vec::new();
    match delete_remote(context, push_url, &reference, tip) {
        Ok(()) => match update_reference_step(
            retirement,
            &retirement_record,
            RetirementStepState::Completed {
                evidence: RemovalEvidence::Landing { evidence },
            },
        ) {
            Ok(retirement_record) => {
                let removal = retirement_record.removal();
                if matches!(&removal.outcome, RemovalOutcome::Removed { .. }) {
                    if let Err(error) = ledger.forget(record, context.clock, cancel) {
                        outcomes.push(
                            out(ArtefactState::Failed)
                                .with_reason(error.to_string())
                                .with_head(tip)
                                .with_removal(removal),
                        );
                    } else {
                        outcomes.push(
                            out(ArtefactState::Removed)
                                .with_reason("landed")
                                .with_head(tip)
                                .with_removal(removal),
                        );
                    }
                } else {
                    outcomes.push(
                        out(ArtefactState::Retained)
                            .with_reason("retirement_incomplete")
                            .with_head(tip)
                            .with_removal(removal),
                    );
                }
                if let Some(tracking) = &tracking {
                    outcomes.extend(stale_tracking(context, remote, tracking, cancel));
                }
            }
            Err(error) => outcomes.push(
                out(ArtefactState::Failed)
                    .with_reason(error.to_string())
                    .with_head(tip),
            ),
        },
        Err(error) => match update_reference_step(
            retirement,
            &retirement_record,
            RetirementStepState::Unknown {
                reason: error.clone(),
            },
        ) {
            Ok(record) => outcomes.push(
                out(ArtefactState::Failed)
                    .with_reason(error)
                    .with_head(tip)
                    .with_removal(record.removal()),
            ),
            Err(update_error) => outcomes.push(
                out(ArtefactState::Failed)
                    .with_reason(format!("{error}; retirement update failed: {update_error}"))
                    .with_head(tip),
            ),
        },
    }
    outcomes
}

/// Keep a tracking ref unless a future declared ownership generation exists.
fn stale_tracking(
    context: &WorktreeContext<'_>,
    remote: &str,
    tracking: &GitRefName,
    cancel: &CancelToken,
) -> Option<ArtefactOutcome> {
    let out = |state| {
        ArtefactOutcome::new(Artefact::TrackingRef, tracking.as_str(), state).with_remote(remote)
    };
    match worktrees::ref_tip(context, tracking, cancel) {
        Ok(Some(held)) => Some(
            out(ArtefactState::Retained)
                .with_reason("ownership_unobserved")
                .with_head(held),
        ),
        Ok(None) => None,
        Err(error) => Some(out(ArtefactState::Failed).with_reason(error.to_string())),
    }
}

/// The remote's advertised tip for one full ref, or `None` when absent.
fn remote_tip(
    context: &WorktreeContext<'_>,
    push_url: &str,
    reference: &GitRefName,
) -> Result<Option<GitOid>, String> {
    let listed = host_git_with_stderr(
        context.repo.root_path(),
        context.executor,
        &["ls-remote", "--", push_url, reference.as_str()],
        REMOTE_READ,
        false,
    )
    .map_err(|error| format!("cannot read the remote branch: {error}"))?;
    if listed.termination != Termination::Exit(0) {
        return Err("cannot read the remote branch".into());
    }
    let text = std::str::from_utf8(&listed.stdout.bytes)
        .map_err(|_| "remote listing is not text".to_owned())?;
    let mut found = None;
    for line in text.lines() {
        let Some((oid, name)) = line.split_once('\t') else {
            return Err("remote listing is malformed".into());
        };
        if name != reference.as_str() {
            continue;
        }
        if found.is_some() {
            return Err("remote listing is ambiguous".into());
        }
        found = Some(GitOid::parse(oid).map_err(|_| "remote listing is malformed".to_owned())?);
    }
    Ok(found)
}

/// Delete a remote branch only while it is still at `expected`.
fn delete_remote(
    context: &WorktreeContext<'_>,
    push_url: &str,
    reference: &GitRefName,
    expected: GitOid,
) -> Result<(), String> {
    let lease = format!("--force-with-lease={}:{expected}", reference.as_str());
    let refspec = format!(":{}", reference.as_str());
    let pushed = host_git_with_stderr(
        context.repo.root_path(),
        context.executor,
        &[
            "push",
            "--porcelain",
            "--no-verify",
            &lease,
            "--",
            push_url,
            &refspec,
        ],
        REMOTE_WRITE,
        true,
    )
    .map_err(|error| format!("leased remote deletion did not complete: {error}"))?;
    if pushed.termination == Termination::Exit(0) {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&pushed.stderr.bytes);
    let detail = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("rejected")
        .trim()
        .chars()
        .take(200)
        .collect::<String>();
    Err(format!("remote refused the leased deletion: {detail}"))
}

/// Find and fetch the default branch the remote advertises, into the
/// tracking ref named for the bound remote.
fn remote_default(
    context: &WorktreeContext<'_>,
    remote: &str,
    push_url: &str,
    cancel: &CancelToken,
) -> Result<LandingTarget, String> {
    let run = |args: &[&str], timeout: Duration| {
        host_git_with_stderr(
            context.repo.root_path(),
            context.executor,
            args,
            timeout,
            false,
        )
        .map_err(|error| error.to_string())
    };
    let advertised = run(
        &["ls-remote", "--symref", "--", push_url, "HEAD"],
        REMOTE_READ,
    )?;
    if advertised.termination != Termination::Exit(0) {
        return Err("cannot observe the remote's default branch".into());
    }
    let text = std::str::from_utf8(&advertised.stdout.bytes)
        .map_err(|_| "invalid remote default branch".to_owned())?;
    let branch = text
        .lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        })
        .ok_or_else(|| "the remote has no default branch".to_owned())?;
    let reference =
        GitRefName::branch(branch).map_err(|_| "invalid remote default branch".to_owned())?;
    let tracking = GitRefName::remote_tracking(format!("{remote}/{branch}"))
        .map_err(|_| "invalid remote name".to_owned())?;
    let refspec = format!("+{}:{}", reference.as_str(), tracking.as_str());
    let fetched = run(
        &[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--refmap=",
            "--",
            push_url,
            &refspec,
        ],
        REMOTE_WRITE,
    )?;
    if fetched.termination != Termination::Exit(0) {
        return Err("cannot fetch the remote's default branch".into());
    }
    let tip = worktrees::ref_tip(context, &tracking, cancel)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "fetched default branch is missing".to_owned())?;
    Ok(LandingTarget {
        reference: Some(reference),
        tip,
    })
}

/// The landing target GC fetched from the repository's own remote, as
/// `refs/remotes/<remote>/<branch>` and its tip.
pub(crate) fn fetched_target(fetched: &(String, GitOid)) -> Option<LandingTarget> {
    let (_, branch) = fetched.0.strip_prefix("refs/remotes/")?.split_once('/')?;
    Some(LandingTarget {
        reference: Some(GitRefName::branch(branch).ok()?),
        tip: fetched.1,
    })
}

/// One GC pass over the ledger: only proven landed, unchanged created refs go.
/// `upstream` is the default branch GC already fetched, when it did.
pub(crate) fn collect(
    context: &WorktreeContext<'_>,
    upstream: Option<LandingTarget>,
    sweep: &Sweep,
    retirement: &dyn RetirementPort,
    cancel: &CancelToken,
) -> Vec<ArtefactOutcome> {
    let ledger = match RefLedger::for_context(context) {
        Ok(ledger) => ledger,
        Err(_) => return Vec::new(),
    };
    let records = match ledger.list() {
        Ok(records) => records,
        Err(error) => {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: could not read the ref ledger: {error}; its refs are retained"
            );
            return Vec::new();
        }
    };
    let created = records
        .into_iter()
        .filter(|record| record.origin == RefOrigin::Created)
        .collect::<Vec<_>>();
    // One default-branch fetch per bound endpoint.
    let mut endpoints: Vec<(RefEndpoint, Option<LandingTarget>)> = Vec::new();
    for record in &created {
        if let RefEndpoint::Remote { remote, push_url } = &record.endpoint
            && !endpoints
                .iter()
                .any(|(endpoint, _)| endpoint == &record.endpoint)
        {
            let target = remote_default(context, remote, push_url, cancel).ok();
            endpoints.push((record.endpoint.clone(), target));
        }
    }
    let mut outcomes = Vec::new();
    // Remote refs first, so a branch whose remote copy just went is judged
    // against the same fetched targets.
    for record in &created {
        if record.endpoint == RefEndpoint::Local {
            continue;
        }
        let target = endpoints
            .iter()
            .find(|(endpoint, _)| endpoint == &record.endpoint)
            .and_then(|(_, target)| target.as_ref());
        outcomes.extend(sweep_remote(
            context,
            &ledger,
            record,
            target,
            &[],
            sweep,
            retirement,
            cancel,
        ));
    }
    let mut targets = upstream.into_iter().collect::<Vec<_>>();
    for (_, target) in &endpoints {
        if let Some(target) = target
            && !targets.contains(target)
        {
            targets.push(target.clone());
        }
    }
    for record in &created {
        if record.endpoint == RefEndpoint::Local {
            outcomes.push(sweep_local(
                context, &ledger, record, &targets, sweep, retirement, cancel,
            ));
        }
    }
    outcomes
}

/// Settlement of an integrated lane: its own remote branch, when a delivery
/// created one at the bound endpoint, goes once the integration receipt
/// proves it landed. The local branch and worktree were settled by
/// `prune_owned`.
pub(crate) fn settle_integrated(
    context: &WorktreeContext<'_>,
    branch: &GitRefName,
    receipt: &provenant_run::WriterDeliveryReceipt,
    sweep: &Sweep,
    retirement: &dyn RetirementPort,
    cancel: &CancelToken,
) -> Vec<ArtefactOutcome> {
    let (Some(integration), Some(head)) = (receipt.integration.as_ref(), receipt.head) else {
        return Vec::new();
    };
    let Some(push_url) = integration.push_url.as_ref() else {
        return Vec::new();
    };
    let Ok(target) = GitRefName::new(&integration.target) else {
        return Vec::new();
    };
    let endpoint = RefEndpoint::Remote {
        remote: integration.remote.clone(),
        push_url: push_url.clone(),
    };
    let Ok(ledger) = RefLedger::for_context(context) else {
        return Vec::new();
    };
    let Ok(Some(record)) = ledger.load(&endpoint, branch) else {
        return Vec::new();
    };
    let root = LandingTarget {
        reference: Some(target.clone()),
        tip: integration.integrated,
    };
    let chain = [IntegrationEdge {
        source: branch.clone(),
        head,
        target,
        previous: integration.previous,
        integrated: integration.integrated,
    }];
    sweep_remote(
        context,
        &ledger,
        &record,
        Some(&root),
        &chain,
        sweep,
        retirement,
        cancel,
    )
}

#[cfg(test)]
// Custom harness=false drivers include this module without running its unit tests.
#[allow(dead_code, unused_imports)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    struct FreeWorkers;

    impl worktrees::WorkerObserver for FreeWorkers {
        fn observe_roots(
            &self,
            roots: &[std::path::PathBuf],
        ) -> std::collections::BTreeMap<std::path::PathBuf, worktrees::Occupancy> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, worktrees::Occupancy::Available))
                .collect()
        }
    }

    struct CleanupFixture {
        _temp: tempfile::TempDir,
        home: std::path::PathBuf,
        project: std::path::PathBuf,
        remote: std::path::PathBuf,
        roots: provenant_core::Roots,
        base: GitOid,
        unlanded: GitOid,
        landed: GitOid,
    }

    impl CleanupFixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            use provenant_os::fixture::run_git;
            let temp = provenant_os::fixture::tempdir(|root| tempfile::tempdir_in(root))?;
            let home = temp.path().join("home");
            let project = temp.path().join("project");
            let remote = temp.path().join("remote.git");
            let state = temp.path().join("state");
            for path in [&home, &project, &remote] {
                std::fs::create_dir(path)?;
            }
            provenant_os::fixture::private_state_dir(&state)?;
            run_git(&remote, &["init", "--bare", "-q"])?;
            run_git(&project, &["init", "-q", "-b", "main"])?;
            run_git(&project, &["commit", "--allow-empty", "-qm", "base"])?;
            let head = || -> Result<GitOid, Box<dyn std::error::Error>> {
                Ok(GitOid::parse(
                    String::from_utf8(run_git(&project, &["rev-parse", "HEAD"])?)?.trim(),
                )?)
            };
            let base = head()?;
            run_git(&project, &["checkout", "-qb", "lane/old"])?;
            run_git(
                &project,
                &["commit", "--allow-empty", "-qm", "unlanded work"],
            )?;
            let unlanded = head()?;
            run_git(&project, &["checkout", "-q", "main"])?;
            run_git(
                &project,
                &["commit", "--allow-empty", "-qm", "human work landed"],
            )?;
            let landed = head()?;
            run_git(&project, &["branch", "lane/changed"])?;
            run_git(
                &project,
                &[
                    "remote",
                    "add",
                    "bound",
                    remote.to_str().ok_or("remote path")?,
                ],
            )?;
            run_git(
                &project,
                &["push", "bound", "main", "lane/old", "lane/changed"],
            )?;
            let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
            inputs.state_dir = Some(state);
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            Ok(Self {
                _temp: temp,
                home,
                project,
                remote,
                roots,
                base,
                unlanded,
                landed,
            })
        }

        fn with_context<T>(
            &self,
            action: impl FnOnce(
                &WorktreeContext<'_>,
                &CancelToken,
            ) -> Result<T, crate::worktrees::Error>,
        ) -> Result<T, crate::worktrees::Error> {
            crate::worktrees::with_context(
                &self.roots,
                &self.home,
                &self.project,
                Duration::from_secs(30),
                &FreeWorkers,
                action,
            )
        }

        fn with_retirement<T>(
            &self,
            context: &WorktreeContext<'_>,
            cancel: &CancelToken,
            record: &RefRecord,
            action: impl FnOnce(&dyn RetirementPort) -> Result<T, worktrees::WorktreeError>,
        ) -> Result<T, worktrees::WorktreeError> {
            use provenant_records::custody::{Chair, Holder, Origin};

            let project = context.binding.project_id();
            let ownership = record.ownership();
            let subject = ownership
                .custody_subject()
                .map_err(|error| worktrees::WorktreeError::Retirement(Box::new(error)))?;
            let actor = provenant_core::ActorId::from_bytes([7; 16]);
            let clock = provenant_os::clock::SystemClock::new();
            let request = provenant_fabric_store::Request {
                clock: &clock,
                cancel,
            };
            let store = provenant_fabric_store::HostStore::open(&self.roots.state, &request)
                .map_err(|error| worktrees::WorktreeError::Retirement(Box::new(error)))?;
            let holder = Holder::Chair {
                chair: Chair {
                    extra: Default::default(),
                    host: provenant_core::HostId::from_bytes([8; 16]),
                    actor,
                    session: Some("lane-cleanup-test".into()),
                    process: None,
                },
            };
            store
                .acquire_custody(project, &subject, &holder, Origin::Provenant, &request)
                .map_err(|error| worktrees::WorktreeError::Retirement(Box::new(error)))?;
            drop(store);
            let retirement = crate::custody::RetirementWriter::open(
                &self.roots,
                project,
                actor,
                &clock,
                cancel,
            )?;
            action(&retirement)
        }

        fn endpoint(&self) -> RefEndpoint {
            RefEndpoint::Remote {
                remote: "bound".into(),
                push_url: self.remote.to_string_lossy().into_owned(),
            }
        }
    }

    fn record_old_ref(
        context: &WorktreeContext<'_>,
        ledger: &RefLedger,
        endpoint: RefEndpoint,
        branch: &str,
        tip: GitOid,
        cancel: &CancelToken,
    ) -> Result<RefRecord, worktrees::WorktreeError> {
        ledger.record(
            worktrees::RefObservation {
                endpoint,
                reference: GitRefName::branch(branch)
                    .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                origin: RefOrigin::Created,
                tip,
                // Far beyond the former 14-day expiry threshold.
                recorded_at: provenant_core::WallTime::from_unix_seconds(1).unwrap(),
                attempt_id: None,
                operation_id: None,
            },
            context.clock,
            cancel,
        )
    }

    #[test]
    fn ancient_unlanded_local_and_remote_refs_survive_automatic_cleanup()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = CleanupFixture::new()?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let target = LandingTarget {
                reference: Some(GitRefName::branch("main").unwrap()),
                tip: fixture.landed,
            };
            for endpoint in [RefEndpoint::Local, fixture.endpoint()] {
                let record = record_old_ref(
                    context,
                    &ledger,
                    endpoint.clone(),
                    "lane/old",
                    fixture.unlanded,
                    cancel,
                )?;
                let outcomes = if endpoint == RefEndpoint::Local {
                    vec![
                        fixture.with_retirement(context, cancel, &record, |retirement| {
                            Ok(sweep_local(
                                context,
                                &ledger,
                                &record,
                                std::slice::from_ref(&target),
                                &Sweep::new(false),
                                retirement,
                                cancel,
                            ))
                        })?,
                    ]
                } else {
                    fixture.with_retirement(context, cancel, &record, |retirement| {
                        Ok(sweep_remote(
                            context,
                            &ledger,
                            &record,
                            Some(&target),
                            &[],
                            &Sweep::new(false),
                            retirement,
                            cancel,
                        ))
                    })?
                };
                let primary = outcomes
                    .iter()
                    .find(|outcome| {
                        outcome.artefact
                            == if endpoint == RefEndpoint::Local {
                                Artefact::LocalBranch
                            } else {
                                Artefact::RemoteBranch
                            }
                    })
                    .unwrap();
                assert_eq!(primary.outcome, ArtefactState::Retained);
                assert_eq!(primary.head, Some(fixture.unlanded));
                assert_eq!(
                    primary.removal.as_ref().map(|record| &record.outcome),
                    Some(&RemovalOutcome::Kept {
                        cause: RemovalUncertainty::UnprovenLanding,
                    })
                );
                assert_eq!(
                    ledger.load(&endpoint, &GitRefName::branch("lane/old").unwrap())?,
                    Some(record)
                );
            }
            assert_eq!(
                worktrees::ref_tip(context, &GitRefName::branch("lane/old").unwrap(), cancel)?,
                Some(fixture.unlanded)
            );
            assert_eq!(
                remote_tip(
                    context,
                    fixture.remote.to_str().unwrap(),
                    &GitRefName::branch("lane/old").unwrap()
                )
                .unwrap(),
                Some(fixture.unlanded)
            );
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn human_changed_local_and_remote_tips_survive_even_when_landed()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = CleanupFixture::new()?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let target = LandingTarget {
                reference: Some(GitRefName::branch("main").unwrap()),
                tip: fixture.landed,
            };
            for endpoint in [RefEndpoint::Local, fixture.endpoint()] {
                let record = record_old_ref(
                    context,
                    &ledger,
                    endpoint.clone(),
                    "lane/changed",
                    fixture.base,
                    cancel,
                )?;
                let outcomes = if endpoint == RefEndpoint::Local {
                    vec![
                        fixture.with_retirement(context, cancel, &record, |retirement| {
                            Ok(sweep_local(
                                context,
                                &ledger,
                                &record,
                                std::slice::from_ref(&target),
                                &Sweep::new(false),
                                retirement,
                                cancel,
                            ))
                        })?,
                    ]
                } else {
                    fixture.with_retirement(context, cancel, &record, |retirement| {
                        Ok(sweep_remote(
                            context,
                            &ledger,
                            &record,
                            Some(&target),
                            &[],
                            &Sweep::new(false),
                            retirement,
                            cancel,
                        ))
                    })?
                };
                let primary = outcomes
                    .iter()
                    .find(|outcome| {
                        outcome.artefact
                            == if endpoint == RefEndpoint::Local {
                                Artefact::LocalBranch
                            } else {
                                Artefact::RemoteBranch
                            }
                    })
                    .unwrap();
                assert_eq!(primary.outcome, ArtefactState::Retained);
                assert_eq!(primary.head, Some(fixture.landed));
                assert_eq!(
                    ledger.load(&endpoint, &GitRefName::branch("lane/changed").unwrap())?,
                    Some(record)
                );
            }
            assert_eq!(
                worktrees::ref_tip(
                    context,
                    &GitRefName::branch("lane/changed").unwrap(),
                    cancel
                )?,
                Some(fixture.landed)
            );
            assert_eq!(
                remote_tip(
                    context,
                    fixture.remote.to_str().unwrap(),
                    &GitRefName::branch("lane/changed").unwrap()
                )
                .unwrap(),
                Some(fixture.landed)
            );
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn removed_ref_projects_its_completed_durable_retirement()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = CleanupFixture::new()?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let record = record_old_ref(
                context,
                &ledger,
                RefEndpoint::Local,
                "lane/changed",
                fixture.base,
                cancel,
            )?;
            assert_ne!(record.tip, fixture.landed);
            let target = LandingTarget {
                reference: Some(GitRefName::branch("main").unwrap()),
                tip: fixture.landed,
            };
            fixture.with_retirement(context, cancel, &record, |retirement| {
                let intent = RetirementIntent {
                    project: context.binding.project_id(),
                    related_subjects: Vec::new(),
                    reference: record.ownership(),
                    expected_tip: fixture.landed,
                    evidence: RemovalEvidence::Landing {
                        evidence: LandedEvidence {
                            predicate: LandingPredicate::Subsumed,
                            source: LandingSource::RetainedRef {
                                reference: "refs/heads/main".into(),
                                commit: fixture.landed,
                            },
                        },
                    },
                };
                let pending = retirement
                    .begin_retirement(intent)?
                    .ok_or(worktrees::WorktreeError::RegistrationUnconfirmed)?;
                assert_eq!(pending.expected_tip, fixture.landed);
                assert!(matches!(
                    reference_step(&pending)?,
                    RetirementStepState::Pending
                ));
                let outcome = sweep_local(
                    context,
                    &ledger,
                    &record,
                    std::slice::from_ref(&target),
                    &Sweep::new(false),
                    retirement,
                    cancel,
                );
                assert_eq!(outcome.outcome, ArtefactState::Removed);
                let removal = outcome
                    .removal
                    .ok_or(worktrees::WorktreeError::RegistrationUnconfirmed)?;
                let retirement_id = match &removal.outcome {
                    RemovalOutcome::Removed { retirement_id, .. } => retirement_id,
                    RemovalOutcome::Kept { .. } => {
                        return Err(worktrees::WorktreeError::RegistrationUnconfirmed);
                    }
                };
                let subject = record
                    .ownership()
                    .custody_subject()
                    .map_err(|error| worktrees::WorktreeError::Retirement(Box::new(error)))?;
                let durable = retirement
                    .retirement_for_subject(context.binding.project_id(), &subject)?
                    .ok_or(worktrees::WorktreeError::RegistrationUnconfirmed)?;
                assert_eq!(&durable.id, retirement_id);
                assert_eq!(durable.id, pending.id);
                assert_eq!(durable.removal(), removal);
                assert!(durable.outcomes.iter().any(|outcome| {
                    outcome.step == RetirementStep::Reference
                        && matches!(&outcome.state, RetirementStepState::Completed { .. })
                }));
                if let RemovalOutcome::Removed {
                    evidence: RemovalEvidence::Landing { evidence },
                    ..
                } = &removal.outcome
                {
                    assert_eq!(evidence.predicate, LandingPredicate::Landed);
                    assert_eq!(
                        evidence.source,
                        LandingSource::IntegrationTarget {
                            reference: "refs/heads/main".into(),
                            commit: fixture.landed,
                        }
                    );
                }
                assert_eq!(
                    worktrees::ref_tip(
                        context,
                        &GitRefName::branch("lane/changed").unwrap(),
                        cancel
                    )?,
                    None
                );
                assert_eq!(
                    ledger.load(
                        &RefEndpoint::Local,
                        &GitRefName::branch("lane/changed").unwrap()
                    )?,
                    None
                );
                Ok(())
            })?;
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn refused_reference_step_reopens_before_retry() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = CleanupFixture::new()?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let record = record_old_ref(
                context,
                &ledger,
                RefEndpoint::Local,
                "lane/changed",
                fixture.landed,
                cancel,
            )?;
            fixture.with_retirement(context, cancel, &record, |retirement| {
                let port = retirement;
                let evidence = LandedEvidence {
                    predicate: LandingPredicate::Landed,
                    source: LandingSource::IntegrationTarget {
                        reference: "refs/heads/main".into(),
                        commit: fixture.landed,
                    },
                };
                let intent = RetirementIntent {
                    project: context.binding.project_id(),
                    related_subjects: Vec::new(),
                    reference: record.ownership(),
                    expected_tip: record.tip,
                    evidence: evidence.clone(),
                };
                let retirement = port
                    .begin_retirement(intent)?
                    .ok_or(worktrees::WorktreeError::RegistrationUnconfirmed)?;
                let refused = port.advance_retirement(
                    &retirement,
                    RetirementStep::Reference,
                    RetirementStepState::Refused {
                        reason: "checkout changed".into(),
                    },
                )?;
                let completed = update_reference_step(
                    port,
                    &refused,
                    RetirementStepState::Completed {
                        evidence: RemovalEvidence::Landing { evidence },
                    },
                )?;
                assert!(matches!(
                    reference_step(&completed)?,
                    RetirementStepState::Completed { .. }
                ));
                Ok(())
            })?;
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn unowned_stale_tracking_ref_is_kept_after_the_remote_tip_disappears()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = CleanupFixture::new()?;
        provenant_os::fixture::run_git(&fixture.project, &["fetch", "bound"])?;
        provenant_os::fixture::run_git(
            &fixture.remote,
            &["update-ref", "-d", "refs/heads/lane/old"],
        )?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let record = record_old_ref(
                context,
                &ledger,
                fixture.endpoint(),
                "lane/old",
                fixture.unlanded,
                cancel,
            )?;
            let target = LandingTarget {
                reference: Some(GitRefName::branch("main").unwrap()),
                tip: fixture.landed,
            };
            let tracking = GitRefName::remote_tracking("bound/lane/old").unwrap();
            let outcomes = fixture.with_retirement(context, cancel, &record, |retirement| {
                Ok(sweep_remote(
                    context,
                    &ledger,
                    &record,
                    Some(&target),
                    &[],
                    &Sweep::new(false),
                    retirement,
                    cancel,
                ))
            })?;
            let remote = outcomes
                .iter()
                .find(|outcome| outcome.artefact == Artefact::RemoteBranch)
                .unwrap();
            assert_eq!(remote.outcome, ArtefactState::Absent);
            let tracking_outcome = outcomes
                .iter()
                .find(|outcome| outcome.artefact == Artefact::TrackingRef)
                .unwrap();
            assert_eq!(tracking_outcome.outcome, ArtefactState::Retained);
            assert_eq!(tracking_outcome.head, Some(fixture.unlanded));
            assert!(tracking_outcome.removal.is_none());
            assert_eq!(
                worktrees::ref_tip(context, &tracking, cancel)?,
                Some(fixture.unlanded)
            );
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn unknown_landing_target_and_unreachable_remote_preserve_ancient_work()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = CleanupFixture::new()?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let record = record_old_ref(
                context,
                &ledger,
                fixture.endpoint(),
                "lane/old",
                fixture.unlanded,
                cancel,
            )?;
            let outcomes = fixture.with_retirement(context, cancel, &record, |retirement| {
                Ok(sweep_remote(
                    context,
                    &ledger,
                    &record,
                    None,
                    &[],
                    &Sweep::new(false),
                    retirement,
                    cancel,
                ))
            })?;
            let outcome = outcomes
                .iter()
                .find(|outcome| outcome.artefact == Artefact::RemoteBranch)
                .unwrap();
            assert_eq!(outcome.outcome, ArtefactState::Retained);
            assert_eq!(
                remote_tip(
                    context,
                    fixture.remote.to_str().unwrap(),
                    &GitRefName::branch("lane/old").unwrap()
                )
                .unwrap(),
                Some(fixture.unlanded)
            );
            Ok(())
        })?;
        let offline = fixture.remote.with_extension("offline");
        std::fs::rename(&fixture.remote, &offline)?;
        fixture.with_context(|context, cancel| {
            let ledger = RefLedger::for_context(context)?;
            let record = ledger
                .load(
                    &fixture.endpoint(),
                    &GitRefName::branch("lane/old").unwrap(),
                )?
                .unwrap();
            let outcomes = fixture.with_retirement(context, cancel, &record, |retirement| {
                Ok(sweep_remote(
                    context,
                    &ledger,
                    &record,
                    None,
                    &[],
                    &Sweep::new(false),
                    retirement,
                    cancel,
                ))
            })?;
            let outcome = outcomes
                .iter()
                .find(|outcome| outcome.artefact == Artefact::RemoteBranch)
                .unwrap();
            assert_eq!(outcome.outcome, ArtefactState::Failed);
            assert_eq!(
                ledger.load(
                    &fixture.endpoint(),
                    &GitRefName::branch("lane/old").unwrap()
                )?,
                Some(record)
            );
            assert_eq!(
                worktrees::ref_tip(context, &GitRefName::branch("lane/old").unwrap(), cancel)?,
                Some(fixture.unlanded)
            );
            Ok(())
        })?;
        std::fs::rename(&offline, &fixture.remote)?;
        let head =
            provenant_os::fixture::run_git(&fixture.remote, &["rev-parse", "refs/heads/lane/old"])?;
        assert_eq!(
            String::from_utf8(head)?.trim(),
            fixture.unlanded.to_string()
        );
        Ok(())
    }

    #[test]
    fn unproven_ref_decisions_keep_typed_causes() {
        let missing = kept_decision("not_landed", RemovalUncertainty::MissingCommit);
        assert!(matches!(
            missing,
            LandingDecision::Keep {
                cause: RemovalUncertainty::MissingCommit,
                ..
            }
        ));
        let unknown = kept_decision("target_unavailable", RemovalUncertainty::UnprovenLanding);
        assert!(matches!(
            unknown,
            LandingDecision::Keep {
                cause: RemovalUncertainty::UnprovenLanding,
                ..
            }
        ));
    }

    #[test]
    fn fetched_target_names_the_remote_default_branch() {
        let tip = GitOid::parse(&"cd".repeat(20)).unwrap();
        let target = fetched_target(&("refs/remotes/upstream/main".into(), tip)).unwrap();
        assert_eq!(target.reference, Some(GitRefName::branch("main").unwrap()));
        assert_eq!(target.tip, tip);
        assert!(fetched_target(&("refs/heads/main".into(), tip)).is_none());
    }
}
