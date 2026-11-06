//! Launch refusals must survive settlement without exposing provider/error text.
use super::tests::{call, clock, context, input};
use super::*;
use provenant_core::CancelToken;
use provenant_records::wire::LaneRow;
use provenant_supervisor::custody::ExitObservation;

#[test]
fn exact_model_refusal_keeps_safe_cause_without_skipped_routes()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    struct Refused {
        typed: bool,
        disk: bool,
    }
    impl Providers for Refused {
        fn check_policy(&self, _: &TaskRequest) -> std::result::Result<(), PrepareRefusal> {
            if self.disk {
                return Err(PrepareRefusal {
                    terminal_cause: Some(TerminalCause::DiskFloor),
                    terminal_note: Some("99 bytes available; gc.free_floor is 100 bytes (source: host configuration)".into()),
                    message: "HOST_DISK_ERROR_SENTINEL".into(),
                    skipped_routes: Vec::new(),
                    fallback_outcome: None,
                });
            }
            Ok(())
        }
        fn prepare(
            &self,
            _: &TaskRequest,
            _: &str,
            _: &LaunchPaths,
            _: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, PrepareRefusal> {
            let mut refusal = PrepareRefusal::from("PROVIDER_SECRET_SENTINEL");
            refusal.terminal_cause = None;
            if self.typed {
                refusal.terminal_cause = Some(TerminalCause::RouteResolutionRefused);
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
    for (model, typed, disk) in [
        ("codex/gpt-6.1-sol", true, false),
        ("agy/gemini-3.8-flash", false, false),
        ("codex/gpt-6.1-sol", false, true),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut run = input();
        run.request.tasks[0].route = RequestedRoute::Model {
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
            run.request.tasks[0].writer = Some(WriterRequest {
                extra: Default::default(),
                project,
                worktree,
            });
        }
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        let started = store.start(
            run.run_id,
            &Refused { typed, disk },
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
        } else if typed {
            TerminalCause::RouteResolutionRefused
        } else {
            TerminalCause::PreparationRefused
        };
        let AttemptState::Terminal { extra, .. } = &record.state else {
            return Err("not terminal".into());
        };
        assert_eq!(
            extra.get("terminal_cause"),
            Some(&serde_json::to_value(cause)?)
        );
        let (_, label) = LaneRow::state_and_label(&record.state);
        let label = label.ok_or("missing human label")?;
        assert_eq!(label, cause.reason());
        assert!(label.len() <= 60, "{label}");
        assert!(!label.contains("PROVIDER_SECRET_SENTINEL"), "{label}");
        assert!(!label.contains("unsupported_effort"), "{label}");
        assert!(!label.contains("99 bytes available"), "{label}");
        let reason = reason.ok_or("missing structured reason")?;
        assert!(reason.starts_with(cause.detail()), "{reason}");
        assert!(!reason.contains("PROVIDER_SECRET_SENTINEL"), "{reason}");
        if disk {
            assert!(reason.contains("gc.free_floor"), "{reason}");
            assert!(reason.contains("99 bytes available"), "{reason}");
            assert!(reason.contains("100 bytes"), "{reason}");
            assert!(reason.contains("host configuration"), "{reason}");
            assert!(reason.contains("provenant gc --apply"), "{reason}");
            assert!(!reason.contains("access grants"), "{reason}");
            assert!(!reason.contains("HOST_DISK_ERROR_SENTINEL"), "{reason}");
        } else if typed {
            assert!(reason.contains("unsupported_effort"), "{reason}");
            assert!(reason.contains("provenant route resolve"), "{reason}");
        } else {
            assert!(reason.contains("before retrying"), "{reason}");
        }
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
    store.refuse_queued(run.run_id, "child proposal is invalid", &call)?;
    let record = store.record(run.attempts[0].attempt_id, &call)?;
    let AttemptState::Terminal { extra, outcome, .. } = &record.state else {
        return Err("not terminal".into());
    };
    assert_eq!(*outcome, Outcome::Rejected);
    assert!(!extra.contains_key("terminal_cause"));
    let (_, reason) = LaneRow::state_and_reason(&record.state);
    assert_eq!(
        reason.as_deref(),
        Some(Outcome::Rejected.diagnostic_reason())
    );
    assert!(!reason.unwrap().contains("launch preparation"));
    Ok(())
}
