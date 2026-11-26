//! The behavioural oracle for the one settlement decider.
//!
//! Each row fixes the sealed evidence and asserts the whole fact at once:
//! outcome, cause, reason, sealed stop cause, partial signals and progress.
//! Git evidence is only a launch pin and a terminal head; pushes, remote
//! refs and a settlement base are not fields and cannot change the fact.

use provenant_core::GitOid;
use provenant_records::CanonicalV1;
use provenant_records::declaration::{DECLARATION_SCHEMA, DeclarationEvidence, SealedDeclaration};
use provenant_records::handoff::{Handoff, HandoffError, HandoffStatus};
use provenant_records::run::{Outcome, PartialSignal, SettlementReason, TerminalCause};
use provenant_records::settlement::{
    CustodyFacts, GitEvidence, HostVerdict, IdentityEvidence, IdentityPolicyEffect, LaneRole,
    PrimaryCheckout, Progress, SETTLEMENT_EVIDENCE_SCHEMA, SettlementEvidence,
    classify::budget_completed, classify::settle,
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

fn base(lane: LaneRole, outcome: Outcome, cause: Option<TerminalCause>) -> SettlementEvidence {
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
    }
}

fn git_at(pinned: Option<&str>, head: Option<&str>) -> Option<GitEvidence> {
    Some(GitEvidence {
        pinned: pinned.map(|text| GitOid::parse(text).unwrap()),
        head: head.map(|text| GitOid::parse(text).unwrap()),
        uncommitted: None,
        primary: PrimaryCheckout::Clean,
    })
}

struct Row {
    name: &'static str,
    evidence: SettlementEvidence,
    outcome: Outcome,
    cause: Option<TerminalCause>,
    reason: SettlementReason,
    stop: Option<StopCause>,
    signals: Vec<PartialSignal>,
    progress: Option<Progress>,
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
        pinned: Some(GitOid::parse(BASE).unwrap()),
        head: Some(GitOid::parse(HEAD).unwrap()),
        uncommitted: None,
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
        pinned: Some(GitOid::parse(BASE).unwrap()),
        head: Some(GitOid::parse(HEAD).unwrap()),
        uncommitted: Some("M src/lib.rs".into()),
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
        Some(TerminalCause::ProviderCaptureDefect { line: 42 }),
    );

    let mut budget = base(
        LaneRole::ModelWriter,
        Outcome::Failed,
        Some(TerminalCause::TokenBudget),
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
            cause: Some(TerminalCause::Blocked),
            reason: SettlementReason::DeclaredStop,
            stop: Some(StopCause::Blocked),
            signals: vec![PartialSignal::DeclaredStop],
            progress: Some(Progress::Moved),
        },
        Row {
            name: "pushed then done",
            evidence: pushed_done,
            outcome: Outcome::Succeeded,
            cause: None,
            reason: SettlementReason::Completed,
            stop: None,
            signals: vec![],
            progress: Some(Progress::Moved),
        },
        Row {
            name: "committed, not pushed, stopped early",
            evidence: committed_stopped,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementReason::DeclaredStop,
            stop: Some(StopCause::StoppedEarly),
            signals: vec![PartialSignal::DeclaredStop],
            progress: Some(Progress::Moved),
        },
        Row {
            name: "self-rebased force-push",
            evidence: force_pushed,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementReason::DeclaredStop,
            stop: Some(StopCause::StoppedEarly),
            signals: vec![PartialSignal::DeclaredStop],
            progress: Some(Progress::Moved),
        },
        Row {
            name: "restart after commit with the sealed pin",
            evidence: restarted,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementReason::DeclaredStop,
            stop: Some(StopCause::StoppedEarly),
            signals: vec![PartialSignal::DeclaredStop],
            progress: Some(Progress::Moved),
        },
        Row {
            name: "stopped early with an unchanged head",
            evidence: unchanged,
            outcome: Outcome::ReviewRequired,
            cause: Some(TerminalCause::StoppedNoWork),
            reason: SettlementReason::DeclaredStop,
            stop: Some(StopCause::StoppedEarly),
            signals: vec![PartialSignal::DeclaredStop],
            progress: Some(Progress::Unchanged),
        },
        Row {
            name: "stopped early with an unknown head",
            evidence: unknown,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementReason::DeclaredStop,
            stop: Some(StopCause::StoppedEarly),
            signals: vec![PartialSignal::DeclaredStop],
            progress: Some(Progress::Unknown),
        },
        Row {
            name: "stray primary checkout",
            evidence: stray,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementReason::StrayWork,
            stop: None,
            signals: vec![PartialSignal::StrayWork],
            progress: Some(Progress::Moved),
        },
        Row {
            name: "rejected declaration only",
            evidence: rejected,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementReason::HandoffMalformed,
            stop: Some(StopCause::HandoffMalformed),
            signals: vec![PartialSignal::HandoffMalformed],
            progress: None,
        },
        Row {
            name: "model writer declared nothing",
            evidence: missing,
            outcome: Outcome::ReviewRequired,
            cause: None,
            reason: SettlementReason::HandoffMissing,
            stop: Some(StopCause::HandoffMissing),
            signals: vec![PartialSignal::HandoffMissing],
            progress: None,
        },
        Row {
            name: "adapter writer declares no handoff",
            evidence: adapter,
            outcome: Outcome::Succeeded,
            cause: None,
            reason: SettlementReason::Completed,
            stop: None,
            signals: vec![],
            progress: None,
        },
        Row {
            name: "reader has no progress",
            evidence: reader,
            outcome: Outcome::Succeeded,
            cause: None,
            reason: SettlementReason::Completed,
            stop: None,
            signals: vec![],
            progress: None,
        },
        Row {
            name: "empty writer result",
            evidence: empty,
            outcome: Outcome::ReviewRequired,
            cause: Some(TerminalCause::EmptyWriterResult),
            reason: SettlementReason::EmptyResult,
            stop: Some(StopCause::EmptyResult),
            signals: vec![PartialSignal::HandoffMissing],
            progress: None,
        },
        Row {
            name: "uncommitted edits",
            evidence: uncommitted,
            outcome: Outcome::Uncommitted,
            cause: None,
            reason: SettlementReason::UncommittedWork,
            stop: Some(StopCause::Uncommitted),
            signals: vec![PartialSignal::UncommittedWork],
            progress: Some(Progress::Moved),
        },
        Row {
            name: "cancelled and steered",
            evidence: steered,
            outcome: Outcome::Cancelled,
            cause: Some(TerminalCause::Steered),
            reason: SettlementReason::Steered,
            stop: Some(StopCause::Steered),
            signals: vec![],
            progress: None,
        },
        Row {
            name: "cancelled without delivered cancellation",
            evidence: unknown_cancel,
            outcome: Outcome::Unsettled,
            cause: None,
            reason: SettlementReason::Unsettled,
            stop: Some(StopCause::Unsettled),
            signals: vec![],
            progress: None,
        },
        Row {
            name: "identity mismatch",
            evidence: identity,
            outcome: Outcome::Failed,
            cause: Some(TerminalCause::IdentityMismatch),
            reason: SettlementReason::Failed,
            stop: Some(StopCause::IdentityMismatch),
            signals: vec![],
            progress: None,
        },
        Row {
            name: "capture defect",
            evidence: capture,
            outcome: Outcome::Failed,
            cause: Some(TerminalCause::ProviderCaptureDefect { line: 42 }),
            reason: SettlementReason::Failed,
            stop: Some(StopCause::Unsettled),
            signals: vec![],
            progress: None,
        },
        Row {
            name: "budget completion",
            evidence: budget,
            outcome: Outcome::Succeeded,
            cause: Some(TerminalCause::TokenBudget),
            reason: SettlementReason::CompletedBeforeBudget,
            stop: Some(StopCause::TokenBudget),
            signals: vec![],
            progress: None,
        },
        Row {
            name: "pre-launch refusal",
            evidence: prelaunch,
            outcome: Outcome::Failed,
            cause: None,
            reason: SettlementReason::PreLaunchRefusal,
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
            fact.settlement_reason, row.reason,
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
        assert_eq!(fact.progress, row.progress, "{}: progress", row.name);
        // No-work needs a known, unchanged head. Unknown and Moved never yield it.
        if row.progress != Some(Progress::Unchanged) {
            assert_ne!(
                fact.cause,
                Some(TerminalCause::StoppedNoWork),
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
    assert_eq!(settle(&evidence).cause, Some(TerminalCause::StoppedNoWork));

    // A push, a moved remote ref or a settlement base is not a field of the
    // record. Only the sealed head changes when it is re-read after a restart.
    evidence.git.as_mut().unwrap().head = Some(GitOid::parse(HEAD).unwrap());
    let fact = settle(&evidence);
    assert_eq!(fact.progress, Some(Progress::Moved));
    assert_ne!(fact.cause, Some(TerminalCause::StoppedNoWork));

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
