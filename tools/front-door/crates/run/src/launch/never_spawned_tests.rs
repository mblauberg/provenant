//! A launch refused inside custody before any guardian is a local refusal:
//! it keeps its cause, withdraws its reservation and keeps the never-spawned
//! exit as evidence. It is never `LAUNCH_UNKNOWN`.
use super::*;
use crate::dispatch_tests::Ready;
use crate::tests::{call, clock, context, input};
use provenant_core::CancelToken;

/// Prepares a plan with a valid capture plan; no guardian ever runs it.
struct Bounded;
impl Providers for Bounded {
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, Box<PrepareRefusal>> {
        let mut plan = Ready.prepare(task, prompt, paths, resume)?;
        plan.limits = crate::limits::deadline_only(60);
        Ok(plan)
    }
    fn decode(
        &self,
        _: &RouteRecord,
        _: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        unreachable!("no guardian runs a provider in these tests")
    }
}

// Spec: docs/specs/run-launch.md#admission-and-custody
#[test]
fn a_never_spawned_launch_is_a_local_refusal_with_its_cause()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Ready carries no enforced deadline, so its capture plan has wall_ms=0.
    for (providers, cancel_inside, expected) in [
        (&Ready as &dyn Providers, false, "capture plan wall_ms is 0"),
        (&Bounded, true, hand_off::CANCELLED_BEFORE_LAUNCH),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        hand_off::TEST_CANCEL_INSIDE_LAUNCH.with(|fault| fault.set(cancel_inside));
        let started = store.start(id, providers, &|_, _| Ok(Vec::new()), &call)?;
        assert_eq!(cancel.is_cancelled(), cancel_inside);
        let [Started::Refused { reason, .. }] = started.as_slice() else {
            return Err(format!("{expected}: launch did not refuse: {started:?}").into());
        };
        assert!(reason.contains(expected), "{reason}");
        let settle = Request {
            clock: &clock,
            cancel: &CancelToken::new(),
        };
        let record = store.record(id, &settle)?;
        let AttemptState::Terminal {
            outcome,
            reason: Some(terminal),
            cause,
            ..
        } = &record.state
        else {
            return Err(format!("{expected}: attempt did not settle: {:?}", record.state).into());
        };
        assert_eq!(*outcome, Outcome::Rejected);
        assert!(terminal.contains(expected), "{terminal}");
        assert_eq!(*cause, Some(CauseKind::LaunchHandoffRefused));
        assert_eq!(record.provider_launched, Some(false));
        // The reservation is withdrawn, not fenced; the exit stays as evidence.
        let attempt = store.attempt_dir(id, false)?;
        assert!(!custody::reservation_present(
            &attempt,
            RunStore::scope(&record)
        )?);
        let exit: serde_json::Value = serde_json::from_slice(
            &artifacts::read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?
                .ok_or("never-spawned exit missing")?,
        )?;
        assert_eq!(exit["never_spawned"], true);
    }
    Ok(())
}
