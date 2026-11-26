//! The behavioural oracle for the one settlement decider.
//!
//! Each row fixes the sealed evidence and asserts the whole fact at once:
//! outcome, cause, reason, sealed stop cause, partial signals and progress.
//! Git evidence is only a launch pin and a terminal head; pushes, remote
//! refs and a settlement base are not fields and cannot change the fact.

use provenant_core::GitOid;
use provenant_records::CanonicalV1;
use provenant_records::declaration::{
    DECLARATION_SCHEMA, DeclarationCounts, DeclarationEvidence, SealedDeclaration,
};
use provenant_records::handoff::{Handoff, HandoffError, HandoffStatus};
use provenant_records::provenance::{Layer, Source, Sourced};
use provenant_records::run::{Outcome, SettlementSignal, SettlementClass, CauseKind};
use provenant_records::settlement::{
    CustodyFacts, GitEvidence, HandoffSource, HostVerdict, IdentityEvidence, IdentityPolicyEffect,
    LaneRole, PrimaryCheckout, HeadProgress, SETTLEMENT_EVIDENCE_SCHEMA, SettlementEvidence,
    classify::budget_completed,
    classify::settle,
    policy::{NoWork, RouteSettlement, SettlementPolicy, SignalAction},
};
use provenant_records::stop::StopCause;

const BASE: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "2222222222222222222222222222222222222222";

fn handoff(status: HandoffStatus, note: &str) -> Handoff {
    Handoff {
        schema_version: 2,
        status,
        note: note.into(),
        remaining: vec!["Owner decision".into()],
        decisions: vec![],
        verify: vec![],
        spec_concerns: vec![],
        write_scope: None,
        blocked_on: None,
        extra: Default::default(),
    }
}

fn accepted(handoff: Handoff) -> DeclarationEvidence {
    DeclarationEvidence::Accepted(SealedDeclaration {
        schema_version: DECLARATION_SCHEMA,
        sequence: 1,
        request_id: "request".into(),
        digest: CanonicalV1::file_digest(b"payload"),
        handoff,
    })
}

fn base(lane: LaneRole, outcome: Outcome, cause: Option<CauseKind>) -> SettlementEvidence {
    SettlementEvidence {
        schema_version: SETTLEMENT_EVIDENCE_SCHEMA,
        lane,
        host: HostVerdict {
            outcome,
            cause,
            complete: true,
            result_empty: false,
            reason: None,
            terminal_note: None,
            signals: vec![],
            denial: None,
            stopped_handoff: None,
        },
        handoff: DeclarationEvidence::Missing,
        declarations: DeclarationCounts::default(),
        git: None,
        custody: CustodyFacts {
            launched: true,
            cancelled: outcome == Outcome::Cancelled,
            steered: false,
            cancellation_delivered: Some(true),
            import_cancelled: false,
            process_exit: true,
            stall: None,
            identity: IdentityEvidence {
                conflict: false,
                incomplete: IdentityPolicyEffect::Complete,
                eof_fragment_unsettles: false,
            },
        },
        policy: SettlementPolicy::shipped(),
    }
}

fn git_at(pinned: Option<&str>, head: Option<&str>) -> Option<GitEvidence> {
    Some(GitEvidence {
        input_head: pinned.map(|text| GitOid::parse(text).unwrap()),
        writer_head: head.map(|text| GitOid::parse(text).unwrap()),
        uncommitted: None,
        uncommitted_paths: Vec::new(),
        pushed_head: None,
        primary: PrimaryCheckout::Clean,
    })
}

struct Row {
    name: &'static str,
    evidence: SettlementEvidence,
    outcome: Outcome,
    cause: Option<CauseKind>,
    reason: SettlementClass,
    stop: Option<StopCause>,
    signals: Vec<SettlementSignal>,
    progress: Option<HeadProgress>,
}

fn rows() -> Vec<Row> {
    let mut pushed_blocked = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    pushed_blocked.git = git_at(Some(BASE), Some(HEAD));
    pushed_blocked.handoff = accepted(handoff(HandoffStatus::Blocked, "Needs an owner decision."));

    let mut pushed_done = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    pushed_done.git = git_at(Some(BASE), Some(HEAD));
    pushed_done.handoff = accepted(handoff(HandoffStatus::Done, "Delivered and pushed"));

    let mut committed_stopped = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    committed_stopped.git = git_at(Some(BASE), Some(HEAD));
    committed_stopped.handoff =
        accepted(handoff(HandoffStatus::StoppedEarly, "More work remains."));

    let mut force_pushed = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    force_pushed.host.reason = Some("self-rebased before the stop".into());
    force_pushed.git = git_at(Some(BASE), Some(HEAD));
    force_pushed.handoff = accepted(handoff(HandoffStatus::StoppedEarly, "More work remains."));

    let mut restarted = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    restarted.git = git_at(Some(BASE), Some(HEAD));
    restarted.handoff = accepted(handoff(
        HandoffStatus::StoppedEarly,
        "Sealed pin survives restart.",
    ));

    let mut unchanged = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    unchanged.git = git_at(Some(BASE), Some(BASE));
    unchanged.handoff = accepted(handoff(HandoffStatus::StoppedEarly, "Nothing left to do."));

    let mut unknown = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    unknown.git = git_at(None, Some(HEAD));
    unknown.handoff = accepted(handoff(
        HandoffStatus::StoppedEarly,
        "Head was not observed.",
    ));

    let mut stray = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    stray.git = Some(GitEvidence {
        input_head: Some(GitOid::parse(BASE).unwrap()),
        writer_head: Some(GitOid::parse(HEAD).unwrap()),
        uncommitted: None,
        uncommitted_paths: Vec::new(),
        pushed_head: None,
        primary: PrimaryCheckout::Review {
            note: "work made in the primary checkout".into(),
        },
    });
    stray.handoff = accepted(handoff(HandoffStatus::Done, "Done"));

    let mut rejected = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    rejected.handoff = DeclarationEvidence::Rejected {
        error: HandoffError::new("status", "expected status done, stopped_early or blocked"),
    };

    let missing = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    let adapter = base(LaneRole::AdapterWriter, Outcome::Succeeded, None);
    let reader = base(LaneRole::Reader, Outcome::Succeeded, None);

    let mut empty = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    empty.host.result_empty = true;

    let mut uncommitted = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    uncommitted.git = Some(GitEvidence {
        input_head: Some(GitOid::parse(BASE).unwrap()),
        writer_head: Some(GitOid::parse(HEAD).unwrap()),
        uncommitted: Some("M src/lib.rs".into()),
        uncommitted_paths: vec!["src/lib.rs".into()],
        pushed_head: None,
        primary: PrimaryCheckout::Clean,
    });
    uncommitted.handoff = accepted(handoff(HandoffStatus::Done, "Done"));

    let mut steered = base(LaneRole::ModelWriter, Outcome::Cancelled, None);
    steered.custody.steered = true;

    let mut unknown_cancel = base(LaneRole::ModelWriter, Outcome::Cancelled, None);
    unknown_cancel.custody.cancelled = false;

    let mut identity = base(LaneRole::ModelWriter, Outcome::Failed, None);
    identity.custody.identity.conflict = true;

    let capture = base(
        LaneRole::ModelWriter,
        Outcome::Failed,
        Some(CauseKind::ProviderCaptureDefect { line: 42 }),
    );

    let mut budget = base(
        LaneRole::ModelWriter,
        Outcome::Failed,
        Some(CauseKind::TokenBudget),
    );
    budget.handoff = accepted(handoff(HandoffStatus::Done, "Delivered and pushed"));

    let mut prelaunch = base(LaneRole::ModelWriter, Outcome::Failed, None);
    prelaunch.custody.launched = false;
    prelaunch.custody.process_exit = false;

    vec![
        Row {
            name: "pushed then blocked",
            evidence: pushed_blocked,
            outcome: Outcome::ReviewRequired,
            cause: Some(CauseKind::Blocked),
            reason: SettlementClass::DeclaredStop,
            stop: Some(StopCause::Blocked),
            signals: vec![SettlementSignal::DeclaredStop],
            progress: Some(HeadProgress::Moved),
        },
        Row {
            name: "pushed then done",
            evidence: pushed_done,
            outcome: Outcome::Succeeded,
            cause: None,
            reason: SettlementClass::Completed,
            stop: None,
            signals: vec![],
            progress: Some(HeadProgress::Moved),
        },
        Row {
            name: "committed, not pushed, stopped early",
            evidence: committed_stopped,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementClass::DeclaredStop,
            stop: Some(StopCause::StoppedEarly),
            signals: vec![SettlementSignal::DeclaredStop],
            progress: Some(HeadProgress::Moved),
        },
        Row {
            name: "self-rebased force-push",
            evidence: force_pushed,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementClass::DeclaredStop,
            stop: Some(StopCause::StoppedEarly),
            signals: vec![SettlementSignal::DeclaredStop],
            progress: Some(HeadProgress::Moved),
        },
        Row {
            name: "restart after commit with the sealed pin",
            evidence: restarted,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementClass::DeclaredStop,
            stop: Some(StopCause::StoppedEarly),
            signals: vec![SettlementSignal::DeclaredStop],
            progress: Some(HeadProgress::Moved),
        },
        Row {
            name: "stopped early with an unchanged head",
            evidence: unchanged,
            outcome: Outcome::ReviewRequired,
            cause: Some(CauseKind::StoppedNoWork),
            reason: SettlementClass::DeclaredStop,
            stop: Some(StopCause::StoppedEarly),
            signals: vec![SettlementSignal::DeclaredStop],
            progress: Some(HeadProgress::Unchanged),
        },
        Row {
            name: "stopped early with an unknown head",
            evidence: unknown,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementClass::DeclaredStop,
            stop: Some(StopCause::StoppedEarly),
            signals: vec![SettlementSignal::DeclaredStop],
            progress: Some(HeadProgress::Unknown),
        },
        Row {
            name: "stray primary checkout",
            evidence: stray,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementClass::StrayWork,
            stop: None,
            signals: vec![SettlementSignal::StrayWork],
            progress: Some(HeadProgress::Moved),
        },
        Row {
            name: "rejected declaration only",
            evidence: rejected,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementClass::HandoffMalformed,
            stop: Some(StopCause::HandoffMalformed),
            signals: vec![SettlementSignal::HandoffMalformed],
            progress: None,
        },
        Row {
            name: "model writer declared nothing",
            evidence: missing,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementClass::HandoffMissing,
            stop: Some(StopCause::HandoffMissing),
            signals: vec![SettlementSignal::HandoffMissing],
            progress: None,
        },
        Row {
            name: "adapter writer declares no handoff",
            evidence: adapter,
            outcome: Outcome::Succeeded,
            cause: None,
            reason: SettlementClass::Completed,
            stop: None,
            signals: vec![],
            progress: None,
        },
        Row {
            name: "reader has no progress",
            evidence: reader,
            outcome: Outcome::Succeeded,
            cause: None,
            reason: SettlementClass::Completed,
            stop: None,
            signals: vec![],
            progress: None,
        },
        Row {
            name: "empty writer result",
            evidence: empty,
            outcome: Outcome::ReviewRequired,
            cause: Some(CauseKind::EmptyWriterResult),
            reason: SettlementClass::EmptyResult,
            stop: Some(StopCause::EmptyResult),
            signals: vec![SettlementSignal::HandoffMissing],
            progress: None,
        },
        Row {
            name: "uncommitted edits",
            evidence: uncommitted,
            outcome: Outcome::Uncommitted,
            cause: None,
            reason: SettlementClass::UncommittedWork,
            stop: Some(StopCause::Uncommitted),
            signals: vec![SettlementSignal::UncommittedWork],
            progress: Some(HeadProgress::Moved),
        },
        Row {
            name: "cancelled and steered",
            evidence: steered,
            outcome: Outcome::Cancelled,
            cause: Some(CauseKind::Steered),
            reason: SettlementClass::Steered,
            stop: Some(StopCause::Steered),
            signals: vec![],
            progress: None,
        },
        Row {
            name: "cancelled without delivered cancellation",
            evidence: unknown_cancel,
            outcome: Outcome::Unsettled,
            cause: None,
            reason: SettlementClass::Unsettled,
            stop: Some(StopCause::Unsettled),
            signals: vec![],
            progress: None,
        },
        Row {
            name: "identity mismatch",
            evidence: identity,
            outcome: Outcome::Failed,
            cause: Some(CauseKind::IdentityMismatch),
            reason: SettlementClass::Failed,
            stop: Some(StopCause::IdentityMismatch),
            signals: vec![],
            progress: None,
        },
        Row {
            name: "capture defect",
            evidence: capture,
            outcome: Outcome::Failed,
            cause: Some(CauseKind::ProviderCaptureDefect { line: 42 }),
            reason: SettlementClass::Failed,
            stop: Some(StopCause::Unsettled),
            signals: vec![],
            progress: None,
        },
        Row {
            name: "budget completion",
            evidence: budget,
            outcome: Outcome::Succeeded,
            cause: Some(CauseKind::TokenBudget),
            reason: SettlementClass::CompletedBeforeBudget,
            stop: Some(StopCause::TokenBudget),
            signals: vec![],
            progress: None,
        },
        Row {
            name: "pre-launch refusal",
            evidence: prelaunch,
            outcome: Outcome::Failed,
            cause: None,
            reason: SettlementClass::PreLaunchRefusal,
            stop: None,
            signals: vec![],
            progress: None,
        },
    ]
}

#[test]
fn one_decider_settles_every_family_from_one_sealed_record() {
    for row in rows() {
        let fact = settle(&row.evidence);
        assert_eq!(fact.outcome, row.outcome, "{}: outcome", row.name);
        assert_eq!(fact.cause, row.cause, "{}: cause", row.name);
        assert_eq!(
            fact.settlement_class, row.reason,
            "{}: settlement reason",
            row.name
        );
        assert_eq!(
            fact.stop.as_ref().map(|stop| stop.cause()),
            row.stop,
            "{}: stop cause",
            row.name
        );
        assert_eq!(fact.signals, row.signals, "{}: signals", row.name);
        assert_eq!(fact.head_progress, row.progress, "{}: progress", row.name);
        // No-work needs a known, unchanged head. Unknown and Moved never yield it.
        if row.progress != Some(HeadProgress::Unchanged) {
            assert_ne!(
                fact.cause,
                Some(CauseKind::StoppedNoWork),
                "{}: no-work without an unchanged head",
                row.name
            );
        }
    }
}

#[test]
fn progress_is_pin_relative_and_never_reads_remote_state() {
    let mut evidence = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    evidence.git = git_at(Some(BASE), Some(BASE));
    evidence.handoff = accepted(handoff(HandoffStatus::StoppedEarly, "Nothing left to do."));
    assert_eq!(settle(&evidence).cause, Some(CauseKind::StoppedNoWork));

    // A push, a moved remote ref or a settlement base is not a field of the
    // record. Only the sealed head changes when it is re-read after a restart.
    evidence.git.as_mut().unwrap().head = Some(GitOid::parse(HEAD).unwrap());
    let fact = settle(&evidence);
    assert_eq!(fact.head_progress, Some(HeadProgress::Moved));
    assert_ne!(fact.cause, Some(CauseKind::StoppedNoWork));

    // Re-reading HEAD to the same value leaves the sealed fact byte-identical.
    let again = settle(&evidence);
    assert_eq!(fact, again);
}

#[test]
fn the_budget_gate_requires_a_done_accepted_declaration() {
    let done = accepted(handoff(HandoffStatus::Done, "Delivered and pushed"));
    assert!(budget_completed(Outcome::Failed, &done));
    assert!(budget_completed(Outcome::Succeeded, &done));
    assert!(budget_completed(Outcome::TimedOut, &done));
    assert!(!budget_completed(Outcome::Cancelled, &done));
    assert!(!budget_completed(
        Outcome::Failed,
        &DeclarationEvidence::Missing
    ));
    assert!(!budget_completed(
        Outcome::Failed,
        &DeclarationEvidence::Rejected {
            error: HandoffError::new("status", "expected status done, stopped_early or blocked"),
        }
    ));
    assert!(!budget_completed(
        Outcome::Failed,
        &accepted(handoff(HandoffStatus::StoppedEarly, "More work remains."))
    ));
}

#[test]
fn pushed_head_and_uncommitted_paths_are_not_settlement_evidence() {
    let mut evidence = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    evidence.git = git_at(Some(BASE), Some(HEAD));
    evidence.handoff = accepted(handoff(HandoffStatus::Done, "Delivered"));
    let fact = settle(&evidence);

    let mut varied = evidence.clone();
    let git = varied.git.as_mut().unwrap();
    git.pushed_head = Some(GitOid::parse(HEAD).unwrap());
    git.uncommitted_paths = vec!["src/lib.rs".into()];
    assert_eq!(
        settle(&varied),
        fact,
        "diagnostic Git evidence must not move a fact"
    );
}

#[test]
fn a_host_stop_records_a_host_handoff_source() {
    let mut evidence = base(LaneRole::ModelWriter, Outcome::ReviewRequired, None);
    evidence.host.stopped_handoff = Some(handoff(
        HandoffStatus::StoppedEarly,
        "the host stopped this attempt",
    ));
    assert_eq!(settle(&evidence).handoff_source, Some(HandoffSource::Host));
}

#[test]
fn rejected_then_accepted_reports_both_counts_and_a_declared_source() {
    let mut evidence = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    evidence.handoff = accepted(handoff(HandoffStatus::Done, "Delivered"));
    let counts = DeclarationCounts {
        accepted: 1,
        rejected: 1,
    };
    evidence.declarations = counts;
    let fact = settle(&evidence);
    assert_eq!(fact.handoff_source, Some(HandoffSource::Declared));
    assert_eq!(fact.declarations, Some(counts));
}

#[test]
fn an_adapter_writer_reports_no_declaration_counts() {
    let evidence = base(LaneRole::AdapterWriter, Outcome::Succeeded, None);
    assert_eq!(settle(&evidence).declarations, None);
}

// --- Slice 9: the sealed policy is applied at each ladder step. ---

fn sourced_action(value: SignalAction) -> Sourced<SignalAction> {
    Sourced {
        value,
        source: Source {
            layer: Layer::Project,
            origin: Some(".provenant/config.toml".into()),
        },
    }
}

fn sourced_no_work(value: NoWork) -> Sourced<NoWork> {
    Sourced {
        value,
        source: Source {
            layer: Layer::Route,
            origin: Some("model_policy lane.settlement".into()),
        },
    }
}

#[test]
fn handoff_missing_fail_settles_failed_with_its_source() {
    let mut evidence = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    evidence.policy.handoff_missing = sourced_action(SignalAction::Fail);
    let fact = settle(&evidence);
    assert_eq!(fact.outcome, Outcome::Failed);
    assert_eq!(fact.settlement_class, SettlementClass::Failed);
    assert!(fact.signals.contains(&SettlementSignal::HandoffMissing));
    assert_eq!(fact.applied.len(), 1);
    assert_eq!(fact.applied[0].signal, SettlementSignal::HandoffMissing);
    assert_eq!(fact.applied[0].action, SignalAction::Fail);
    assert_eq!(fact.applied[0].source.layer, Layer::Project);
}

#[test]
fn handoff_missing_off_records_the_signal_but_releases_dependants() {
    let mut evidence = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    evidence.policy.handoff_missing = sourced_action(SignalAction::Off);
    let fact = settle(&evidence);
    assert_eq!(fact.outcome, Outcome::Succeeded);
    assert_eq!(fact.settlement_class, SettlementClass::Completed);
    assert_eq!(
        fact.release,
        provenant_records::run::DependantRelease::Release
    );
    assert!(fact.signals.contains(&SettlementSignal::HandoffMissing));
    assert_eq!(fact.applied.len(), 1);
    assert_eq!(fact.applied[0].action, SignalAction::Off);
}

#[test]
fn handoff_malformed_off_keeps_the_error_but_completes_the_lane() {
    let mut evidence = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    evidence.handoff = DeclarationEvidence::Rejected {
        error: HandoffError::new("status", "expected status done, stopped_early or blocked"),
    };
    evidence.policy.handoff_malformed = sourced_action(SignalAction::Off);
    let fact = settle(&evidence);
    assert_eq!(fact.outcome, Outcome::Succeeded);
    assert_eq!(fact.settlement_class, SettlementClass::Completed);
    assert!(fact.signals.contains(&SettlementSignal::HandoffMalformed));
    assert!(fact.handoff.is_err());
    assert_eq!(fact.applied.len(), 1);
}

#[test]
fn empty_result_fail_overrides_the_default_review() {
    let mut evidence = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    evidence.host.result_empty = true;
    evidence.policy.empty_result = sourced_action(SignalAction::Fail);
    let fact = settle(&evidence);
    assert_eq!(fact.outcome, Outcome::Failed);
    assert_eq!(fact.settlement_class, SettlementClass::Failed);
    assert_eq!(fact.applied.len(), 1);
    assert_eq!(fact.applied[0].signal, SettlementSignal::HandoffMissing);
}

#[test]
fn uncommitted_work_off_does_not_downgrade_a_success() {
    let mut evidence = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    evidence.git = git_at(Some(BASE), Some(HEAD));
    evidence.git.as_mut().unwrap().uncommitted = Some("dirty worktree".into());
    evidence.handoff = accepted(handoff(HandoffStatus::Done, "Delivered"));
    evidence.policy.uncommitted_work = sourced_action(SignalAction::Off);
    let fact = settle(&evidence);
    assert_eq!(fact.outcome, Outcome::Succeeded);
    assert_eq!(fact.settlement_class, SettlementClass::Completed);
    assert!(fact.signals.contains(&SettlementSignal::UncommittedWork));
    assert_eq!(fact.applied.len(), 1);
}

#[test]
fn no_work_off_keeps_a_declared_stop_on_an_unchanged_head() {
    let mut evidence = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    evidence.git = git_at(Some(BASE), Some(BASE));
    evidence.handoff = accepted(handoff(HandoffStatus::StoppedEarly, "More work remains."));
    evidence.policy.no_work = sourced_no_work(NoWork::Off);
    let fact = settle(&evidence);
    assert_eq!(fact.cause, None);
    assert_eq!(fact.settlement_class, SettlementClass::DeclaredStop);
    assert_eq!(fact.applied.len(), 1);
    assert_eq!(fact.applied[0].source.layer, Layer::Route);
    assert_eq!(fact.applied[0].signal, SettlementSignal::DeclaredStop);
}

#[test]
fn a_route_value_is_sealed_with_the_policy() {
    let route = RouteSettlement {
        stray_work: Some(SignalAction::Off),
        ..Default::default()
    };
    let mut evidence = base(LaneRole::ModelWriter, Outcome::Succeeded, None);
    evidence.handoff = accepted(handoff(HandoffStatus::Done, "Delivered"));
    evidence.git = git_at(Some(BASE), Some(HEAD));
    evidence.git.as_mut().unwrap().primary = PrimaryCheckout::Review {
        note: "primary checkout moved".into(),
    };
    evidence.policy.stray_work = Sourced {
        value: route.stray_work.unwrap(),
        source: Source {
            layer: Layer::Route,
            origin: Some("model_policy lane.settlement".into()),
        },
    };
    let fact = settle(&evidence);
    assert_eq!(fact.outcome, Outcome::Succeeded);
    assert_eq!(fact.applied.len(), 1);
    assert_eq!(fact.applied[0].source.layer, Layer::Route);
}
