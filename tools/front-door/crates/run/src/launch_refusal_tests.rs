//! Launch refusals must survive settlement without exposing provider/error text.
use super::tests::{call, clock, context, input};
use super::*;
use provenant_core::CancelToken;
use provenant_records::wire::LaneRow;
use provenant_supervisor::custody::ExitObservation;

#[test]
fn exact_model_refusal_keeps_safe_cause_without_skipped_routes()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    struct Refused(bool);
    impl Providers for Refused {
        fn prepare(
            &self,
            _: &TaskRequest,
            _: &str,
            _: &LaunchPaths,
            _: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, PrepareRefusal> {
            let mut refusal = PrepareRefusal::from("PROVIDER_SECRET_SENTINEL");
            if self.0 {
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
    for (model, typed) in [("codex/gpt-6.1-sol", true), ("agy/gemini-3.8-flash", false)] {
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
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        let started = store.start(run.run_id, &Refused(typed), &|_, _| unreachable!(), &call)?;
        assert!(matches!(started.as_slice(), [Started::Refused { attempt, .. }] if *attempt == id));
        let record = store.record(id, &call)?;
        assert!(record.skipped_routes.is_empty());
        assert_eq!(record.provider_launched, Some(false));
        let (state, reason) = LaneRow::state_and_reason(&record.state);
        assert_eq!(state, "rejected");
        let cause = if typed {
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
        let reason = reason.ok_or("missing reason")?;
        assert!(reason.starts_with(cause.reason()), "{reason}");
        assert!(!reason.contains("PROVIDER_SECRET_SENTINEL"), "{reason}");
        if typed {
            assert!(reason.contains("unsupported_effort"), "{reason}");
            assert!(reason.contains("provenant route resolve"), "{reason}");
        } else {
            assert!(reason.contains("before retrying"), "{reason}");
        }
    }
    Ok(())
}
