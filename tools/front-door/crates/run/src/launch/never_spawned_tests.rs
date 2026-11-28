//! A launch refused inside custody before any guardian is a local refusal:
//! it keeps its cause, withdraws its reservation and keeps the never-spawned
//! exit as evidence. It is never `LAUNCH_UNKNOWN`.
use super::*;
use crate::dispatch_tests::Ready;
use crate::tests::{call, clock, context, input};
use provenant_core::CancelToken;

/// Prepares a plan with an enforced deadline of this many seconds; no
/// guardian ever runs it.
struct Deadline(u64);
impl Providers for Deadline {
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, Box<PrepareRefusal>> {
        let mut plan = Ready.prepare(task, prompt, paths, resume)?;
        plan.limits = crate::limits::deadline_only(self.0);
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
    // A deadline past a day is an invalid capture plan, and a cancel inside
    // custody stops the launch: both settle before provider custody, so on
    // every host. Ready carries no enforced deadline: its plan has no wall
    // bound and is valid, and its fixture command is refused by the guardian
    // handoff, which only a host with provider custody reaches.
    let mut cases = vec![
        (
            &Deadline(86_401) as &dyn Providers,
            false,
            "capture plan wall_ms is 86401000",
        ),
        (&Deadline(60), true, hand_off::CANCELLED_BEFORE_LAUNCH),
    ];
    if cfg!(target_os = "macos") {
        cases.push((
            &Ready as &dyn Providers,
            false,
            "the provider never spawned",
        ));
    }
    for (providers, cancel_inside, expected) in cases {
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

// Spec: docs/specs/run-launch.md#admission-and-custody
// The never-spawned exit is kept as evidence before custody is withdrawn. An
// exit that cannot be recorded keeps protected custody: the attempt is not
// terminalised without it and its reservation stays for reconcile.
#[test]
fn a_never_spawned_launch_keeps_custody_while_its_exit_is_unrecorded()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempt_id;
    store.create(&run, &call)?;
    hand_off::TEST_OBSTRUCT_EXIT.with(|fault| fault.set(true));
    let started = store.start(id, &Deadline(86_401), &|_, _| Ok(Vec::new()), &call)?;
    assert!(!hand_off::TEST_OBSTRUCT_EXIT.with(std::cell::Cell::get));
    let [Started::Blocked { error, .. }] = started.as_slice() else {
        return Err(format!("an unrecorded exit did not keep custody: {started:?}").into());
    };
    assert!(
        error.contains("capture plan wall_ms is 86401000"),
        "{error}"
    );
    assert!(error.contains("never-spawned exit unrecorded"), "{error}");
    let settle = Request {
        clock: &clock,
        cancel: &CancelToken::new(),
    };
    let record = store.record(id, &settle)?;
    assert!(
        matches!(record.state, AttemptState::Starting { .. }),
        "terminalised without its exit: {:?}",
        record.state
    );
    let attempt = store.attempt_dir(id, false)?;
    assert!(custody::reservation_present(
        &attempt,
        RunStore::scope(&record)
    )?);
    assert!(
        attempt.current_path()?.join("process-exit.json").is_dir(),
        "the obstructed exit was never written"
    );
    // Reconcile owns the retained custody: it records the exit, then
    // withdraws. While the exit still cannot be written it keeps custody.
    let observer = provenant_supervisor::census::SystemProcessObserver;
    let blocked = store.reconcile_with_observer(id, &Deadline(86_401), &settle, &observer)?;
    assert!(
        matches!(
            record_state(&store, id, &settle)?,
            AttemptState::Starting { .. }
        ),
        "reconcile terminalised without the exit: {blocked:?}"
    );
    std::fs::remove_dir(attempt.current_path()?.join("process-exit.json"))?;
    assert_eq!(
        store.reconcile_with_observer(id, &Deadline(86_401), &settle, &observer)?,
        vec![(id, Reconciled::Withdrawn)]
    );
    let record = store.record(id, &settle)?;
    let AttemptState::Terminal { outcome, cause, .. } = &record.state else {
        return Err(format!("reconcile did not settle: {:?}", record.state).into());
    };
    assert_eq!(*outcome, Outcome::Rejected);
    assert_eq!(*cause, Some(CauseKind::LaunchHandoffRefused));
    let exit: serde_json::Value = serde_json::from_slice(
        &artifacts::read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?
            .ok_or("reconcile withdrew without the never-spawned exit")?,
    )?;
    assert_eq!(exit["never_spawned"], true);
    // A repeated reconcile finds nothing left to settle.
    assert!(
        store
            .reconcile_with_observer(id, &Deadline(86_401), &settle, &observer)?
            .is_empty()
    );
    Ok(())
}

fn record_state(
    store: &RunStore,
    id: AttemptId,
    call: &Request<'_>,
) -> std::result::Result<AttemptState, Box<dyn std::error::Error>> {
    Ok(store.record(id, call)?.state)
}
