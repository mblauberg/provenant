//! Launch refusals survive settlement with a specific cause. Host-authored
//! explanations and fixed notes reach the reason; a provider's own message does not.
use super::tests::{call, clock, context, input};
use super::*;
use provenant_core::CancelToken;
use provenant_records::wire::LaneRow;
use provenant_supervisor::custody::ExitObservation;

/// A secret-shaped value in a provider's refusal message.
const PROVIDER_TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

#[test]
fn exact_model_refusal_keeps_safe_cause_without_skipped_routes()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    struct Refused {
        typed: bool,
        disk: bool,
        source_overlap: bool,
    }
    impl Providers for Refused {
        fn check_policy(&self, _: &TaskRequest) -> std::result::Result<(), Box<PrepareRefusal>> {
            if self.disk {
                return Err(Box::new(PrepareRefusal {
                    boundary_class: provenant_records::diagnostic::BoundaryClass::None,
                    policy_code: None,
                    resource: None,
                    terminal_cause: TerminalCause::DiskFloor,
                    terminal_note: Some("99 bytes available; gc.critical_free is 100 bytes (source: host configuration)".into()),
                    message: "HOST_DISK_ERROR_SENTINEL".into(),
                host_message: None,
                    skipped_routes: Vec::new(),
                    fallback_outcome: None,
                }));
            }
            Ok(())
        }
        fn prepare(
            &self,
            _: &TaskRequest,
            _: &str,
            _: &LaunchPaths,
            _: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, Box<PrepareRefusal>> {
            let mut refusal = PrepareRefusal::boxed(
                TerminalCause::RouteResolutionRefused,
                format!("PROVIDER_SECRET_SENTINEL {PROVIDER_TOKEN}"),
            );
            if self.source_overlap {
                refusal.boundary_class = provenant_records::diagnostic::BoundaryClass::Authority;
                refusal.terminal_note =
                    Some("product assets overlap the live graph source checkout".into());
            }
            if self.typed {
                refusal.terminal_note =
                    Some("route resolver classification: unsupported_effort".into());
            }
            Err(refusal)
        }
        fn decode(
            &self,
            _: &RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &std::path::Path,
        ) -> std::result::Result<Decoded, String> {
            unreachable!()
        }
    }
    for (model, typed, disk, source_overlap) in [
        ("codex/gpt-6.1-sol", true, false, false),
        ("agy/gemini-3.8-flash", false, false, false),
        ("codex/gpt-6.1-sol", false, false, true),
        ("codex/gpt-6.1-sol", false, true, false),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let mut run = input();
        run.request.task.route = RequestedRoute::Model {
            required_family: None,
            extra: Default::default(),
            provider: model.split_once('/').ok_or("adapter")?.0.into(),
            model: model.into(),
            effort: None,
        };
        if disk {
            let project = temp.path().join("project");
            let worktree = temp.path().join("worktree");
            std::fs::create_dir(&project)?;
            std::fs::create_dir(&worktree)?;
            run.request.task.writer = Some(WriterRequest {
                extra: Default::default(),
                custody_project: Some(crate::tests::writer_project()),
                project,
                worktree,
            });
        }
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let started = store.start(
            run.attempt_id,
            &Refused {
                typed,
                disk,
                source_overlap,
            },
            &|_, _| unreachable!(),
            &call,
        )?;
        assert!(matches!(started.as_slice(), [Started::Refused { attempt, .. }] if *attempt == id));
        let record = store.record(id, &call)?;
        assert!(record.skipped_routes.is_empty());
        assert_eq!(record.provider_launched, Some(false));
        let (state, reason) = LaneRow::state_and_reason(&record.state);
        assert_eq!(state, "rejected");
        let cause = if disk {
            TerminalCause::DiskFloor
        } else {
            TerminalCause::RouteResolutionRefused
        };
        let AttemptState::Terminal { terminal_cause, .. } = &record.state else {
            return Err("not terminal".into());
        };
        assert_eq!(*terminal_cause, Some(cause));
        let (_, label) = LaneRow::state_and_label(&record.state);
        let label = label.ok_or("missing human label")?;
        assert_eq!(label, cause.reason());
        assert!(!label.contains("PROVIDER_SECRET_SENTINEL"), "{label}");
        assert!(!label.contains("unsupported_effort"), "{label}");
        assert!(!label.contains("99 bytes available"), "{label}");
        let reason = reason.ok_or("missing structured reason")?;
        assert!(reason.starts_with(cause.detail()), "{reason}");
        if disk {
            assert!(reason.contains("gc.critical_free"), "{reason}");
            assert!(reason.contains("99 bytes available"), "{reason}");
            assert!(reason.contains("100 bytes"), "{reason}");
            assert!(reason.contains("host configuration"), "{reason}");
            assert!(!reason.contains("access grants"), "{reason}");
            assert!(!reason.contains("HOST_DISK_ERROR_SENTINEL"), "{reason}");
        } else if source_overlap {
            assert!(
                reason.contains("product assets overlap the live graph source checkout"),
                "{reason}"
            );
        } else if typed {
            assert!(reason.contains("unsupported_effort"), "{reason}");
        } else {
            // Without a host-authored explanation or fixed note, only the safe cause remains.
            assert!(reason.starts_with(cause.detail()), "{reason}");
            assert!(!reason.contains("PROVIDER_SECRET_SENTINEL"), "{reason}");
            assert!(!reason.contains(PROVIDER_TOKEN), "{reason}");
        }
        // Whatever the reason carries, a secret-shaped value never reaches it.
        assert!(!reason.contains(PROVIDER_TOKEN), "{reason}");
        assert!(!label.contains(PROVIDER_TOKEN), "{label}");
    }
    Ok(())
}

#[test]
fn queued_proposal_refusal_does_not_invent_a_launch_preparation_cause()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    store.refuse_queued(run.attempt_id, "child proposal is invalid", &call)?;
    let record = store.record(run.attempt_id, &call)?;
    let AttemptState::Terminal {
        terminal_cause,
        outcome,
        ..
    } = &record.state
    else {
        return Err("not terminal".into());
    };
    assert_eq!(*outcome, Outcome::Rejected);
    assert_eq!(*terminal_cause, Some(TerminalCause::Unrecorded));
    let (_, reason) = LaneRow::state_and_reason(&record.state);
    assert_eq!(reason.as_deref(), Some(TerminalCause::Unrecorded.detail()));
    Ok(())
}

#[test]
fn git_setup_preparation_failure_projects_its_reason_and_remedy_to_status()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    const REASON: &str = "launch preparation failed: Git setup: no Git remote is declared and none can be inferred; remotes seen: [\"origin\", \"pr\"]; did you mean origin?";
    let state = AttemptState::Terminal {
        extra: Default::default(),
        terminal_cause: Some(TerminalCause::PreparationRefused),
        terminal_note: Some(TerminalCause::preparation_note(REASON)),
        stop: None,
        outcome: Outcome::Rejected,
        exit_code: None,
        finished_at: provenant_core::WallTime::from_micros(1).unwrap(),
        reason: Some(REASON.into()),
    };
    // `provenant status` and `result.md` both read this projection.
    let (_, status_reason) = LaneRow::state_and_reason(&state);
    let status_reason = status_reason.ok_or("missing status reason")?;
    assert!(
        status_reason.starts_with(TerminalCause::PreparationRefused.detail()),
        "{status_reason}"
    );
    assert!(
        status_reason.contains("no Git remote is declared"),
        "{status_reason}"
    );
    assert!(
        status_reason.contains("declare `ports.git.remote` or set the branch's upstream"),
        "{status_reason}"
    );
    assert!(
        !status_reason.contains("host refused the launch handoff"),
        "{status_reason}"
    );
    assert!(
        !status_reason.contains("provenant reconcile"),
        "{status_reason}"
    );
    let (_, label) = LaneRow::state_and_label(&state);
    assert_eq!(
        label.as_deref(),
        Some(TerminalCause::PreparationRefused.reason())
    );
    // The typed failure and stop keep their own names; a real host handoff
    // refusal keeps its reconcile remedy.
    assert_eq!(
        serde_json::to_value(provenant_records::run::PreparationFailure::GitSetup)?,
        serde_json::json!("git_setup")
    );
    assert_eq!(
        serde_json::to_value(TerminalCause::PreparationRefused)?,
        serde_json::json!("preparation_refused")
    );
    assert_eq!(
        provenant_records::StopCause::from(TerminalCause::PreparationRefused),
        provenant_records::StopCause::PreparationRefused
    );
    assert!(
        !TerminalCause::PreparationRefused
            .detail()
            .contains("reconcile")
    );
    assert!(
        TerminalCause::HandoffRefused
            .detail()
            .contains("provenant reconcile")
    );
    Ok(())
}

/// The launch withdrawal maps a typed Git-setup preparation refusal to its own
/// failure and note. Drive the production mapping rather than hand-building a
/// terminal record, so reverting it cannot leave this green (#3931).
#[test]
fn git_setup_handoff_maps_to_git_setup_failure_and_remote_remedy()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_records::run::PreparationFailure;

    const MESSAGE: &str = "Git setup: no Git remote is declared and none can be inferred; remotes seen: [\"origin\", \"pr\"]; did you mean origin?";
    let handoff = HandOff::TypedLocal {
        message: MESSAGE.into(),
        cause: TerminalCause::PreparationRefused,
    };
    let reason = format!("launch preparation failed: {MESSAGE}");
    let (failure, note) = super::handoff_preparation(&handoff, &reason);
    assert_eq!(failure, Some(PreparationFailure::GitSetup));
    let note = note.ok_or("missing preparation note")?;
    assert!(
        note.contains("declare `ports.git.remote` or set the branch's upstream"),
        "{note}"
    );
    // The status reason reads this same typed note and cause.
    let state = AttemptState::Terminal {
        extra: Default::default(),
        terminal_cause: Some(TerminalCause::PreparationRefused),
        terminal_note: Some(note),
        stop: None,
        outcome: Outcome::Rejected,
        exit_code: None,
        finished_at: provenant_core::WallTime::from_micros(1).unwrap(),
        reason: Some(reason),
    };
    let (_, status_reason) = LaneRow::state_and_reason(&state);
    let status_reason = status_reason.ok_or("missing status reason")?;
    assert!(
        status_reason.starts_with(TerminalCause::PreparationRefused.detail()),
        "{status_reason}"
    );
    assert!(
        status_reason.contains("no Git remote is declared"),
        "{status_reason}"
    );
    assert!(
        !status_reason.contains("provenant reconcile"),
        "{status_reason}"
    );

    // Every other local refusal stays an adapter failure and owns its note;
    // a boundary never establishes safe preparation for fallback.
    let local = HandOff::Local("Git setup: some other failure".into());
    let (failure, note) = super::handoff_preparation(
        &local,
        "launch preparation failed: Git setup: some other failure",
    );
    assert_eq!(failure, Some(PreparationFailure::AdapterUnavailable));
    assert!(note.is_none());
    let typed_other = HandOff::TypedLocal {
        message: "writer lineage".into(),
        cause: TerminalCause::HandoffRefused,
    };
    let (failure, _) =
        super::handoff_preparation(&typed_other, "launch preparation failed: writer lineage");
    assert_eq!(failure, Some(PreparationFailure::AdapterUnavailable));
    let boundary = HandOff::Boundary {
        message: "authority".into(),
        class: provenant_records::diagnostic::BoundaryClass::Authority,
        cause: None,
    };
    let (failure, note) =
        super::handoff_preparation(&boundary, "launch preparation failed: authority");
    assert_eq!(failure, None);
    assert!(note.is_none());
    Ok(())
}
