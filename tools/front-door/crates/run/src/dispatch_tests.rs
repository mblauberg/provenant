//! Dispatch, admission and holder behaviour at Run's owner boundary. Fake
//! providers and resources stand in for adapters and the host.
use super::tests::{call, clock, context, exit, input, route};
use super::*;
use provenant_core::{AttemptId, CancelToken, TaskId, WallTime};
use provenant_records::CanonicalV1;
use provenant_supervisor::{
    custody::ExitObservation, exec::Termination, fs::Directory, sys::resources::MemorySample,
};
use std::cell::Cell;
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

struct Host {
    available: u64,
}
impl Resources for Host {
    fn memory(&self) -> std::io::Result<MemorySample> {
        Ok(MemorySample {
            available_bytes: self.available,
            total_bytes: 100,
        })
    }
    fn disk(&self, _: &Directory) -> std::io::Result<u64> {
        Ok(u64::MAX)
    }
}
struct Fake {
    refuse: bool,
    observed: Option<RouteRecord>,
    decoded: Cell<usize>,
}
impl Providers for Fake {
    fn prepare(&self, _: &TaskRequest, _: &str) -> std::result::Result<PreparedLaunch, String> {
        if self.refuse {
            return Err("fixture adapter is disabled".into());
        }
        unreachable!("these tests never hand off to a guardian")
    }
    fn decode(&self, applied: &RouteRecord, stdout: &[u8], _: &ExitObservation) -> Decoded {
        assert_eq!(applied, &route());
        self.decoded.set(self.decoded.get() + 1);
        Decoded {
            outcome: Outcome::Succeeded,
            result: String::from_utf8_lossy(stdout).into_owned(),
            complete: true,
            observed_route: self.observed.clone(),
            usage: UsageRecord::default(),
            reason: None,
        }
    }
}
fn two_tasks(concurrency: u64) -> PreparedRun {
    let mut run = input();
    let mut task = run.request.tasks[0].clone();
    task.task_id = TaskId::from_bytes([7; 16]);
    run.request.tasks.push(task.clone());
    run.request.concurrency = concurrency;
    run.attempts.push(NewAttempt {
        task_id: task.task_id,
        attempt_id: AttemptId::from_bytes([8; 16]),
        prompt: "exact prompt".into(),
    });
    run
}
fn seal_exit(store: &RunStore, exit: &ExitObservation) -> TestResult {
    let mut value = serde_json::to_value(exit)?;
    let object = value.as_object_mut().ok_or("exit fixture")?;
    object.remove("publication");
    object.insert("schema_version".into(), 1.into());
    let dir = store.attempt_dir(exit.scope.run, exit.scope.attempt, false)?;
    artifacts::seal(
        &dir,
        "process-exit.json",
        &serde_json::to_vec(&value)?,
        RECORD_LIMIT,
    )?;
    Ok(())
}

#[test]
fn admission_holds_run_concurrency_spacing_floors_and_deadline() -> TestResult {
    let temp = tempfile::tempdir_in("/tmp")?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    let now = WallTime::from_micros(100)?;

    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
        ..AdmissionPolicy::default()
    });
    let serial = two_tasks(1);
    store.create(&serial, &call)?;
    let [first, second] = [serial.attempts[0].attempt_id, serial.attempts[1].attempt_id];
    for id in [first, second] {
        store.mark_starting(id, &route(), &call)?;
    }
    let running = store
        .claim(first, now, &call)?
        .ok_or("first not admitted")?;
    assert_eq!(running.state, AttemptState::Running);
    assert_eq!(running.started_at, Some(now));
    assert!(
        store.claim(second, now, &call)?.is_none(),
        "run concurrency 1"
    );

    store.set_admission_policy(AdmissionPolicy::default());
    let mut spaced = two_tasks(2);
    spaced.run_id = provenant_core::RunId::from_bytes([10; 16]);
    spaced.operation_id = provenant_core::OperationId::parse("spaced")?;
    spaced.attempts[0].attempt_id = AttemptId::from_bytes([11; 16]);
    spaced.attempts[1].attempt_id = AttemptId::from_bytes([12; 16]);
    store.create(&spaced, &call)?;
    let id = spaced.attempts[0].attempt_id;
    store.mark_starting(id, &route(), &call)?;
    assert!(store.claim(id, now, &call)?.is_none(), "20 s host spacing");

    let attempt = store.attempt_dir(spaced.run_id, id, false)?;
    store.set_admission_policy(AdmissionPolicy {
        admission_seconds: 0,
        ..AdmissionPolicy::default()
    });
    let mut expired = input();
    expired.run_id = provenant_core::RunId::from_bytes([13; 16]);
    expired.operation_id = provenant_core::OperationId::parse("expired")?;
    expired.attempts[0].attempt_id = AttemptId::from_bytes([14; 16]);
    store.create(&expired, &call)?;
    let id = expired.attempts[0].attempt_id;
    store.mark_starting(id, &route(), &call)?;
    let starved = Host { available: 1 };
    assert!(matches!(
        store.admit(id, &attempt, &starved, &clock, &cancel),
        Err(RunError::Refused)
    ));
    Ok(())
}

#[test]
fn prepare_refusal_is_terminal_without_custody() -> TestResult {
    let temp = tempfile::tempdir_in("/tmp")?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let fake = Fake {
        refuse: true,
        observed: None,
        decoded: Cell::new(0),
    };
    let started = store.start(run.run_id, &fake, &|_| unreachable!(), &call)?;
    assert!(
        matches!(&started[..], [Started::Refused { reason, .. }] if reason.contains("disabled"))
    );
    let fact = store.attempt(run.attempts[0].attempt_id, &call)?;
    assert_eq!(
        fact.terminal_state,
        provenant_records::AttemptOutcome::Rejected
    );
    assert!(fact.started_at.is_none());
    assert_eq!(store.run(run.run_id, &call)?.closure, Closure::Closed);
    assert!(
        store
            .start(run.run_id, &fake, &|_| unreachable!(), &call)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn holder_commits_decoded_success_only_with_exit_zero_and_observed_identity() -> TestResult {
    let temp = tempfile::tempdir_in("/tmp")?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
        ..AdmissionPolicy::default()
    });
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let now = WallTime::from_micros(100)?;
    let fake = Fake {
        refuse: false,
        observed: Some(route()),
        decoded: Cell::new(0),
    };
    for (index, code) in [(0, 0), (1, 1)] {
        let id = batch.attempts[index].attempt_id;
        store.mark_starting(id, &route(), &call)?;
        store.claim(id, now, &call)?.ok_or("not admitted")?;
        let dir = store.attempt_dir(batch.run_id, id, false)?;
        artifacts::seal(&dir, "stdout.log", b"answer", CAPTURE_LIMIT)?;
        let mut observed = exit();
        observed.scope.attempt = id;
        observed.scope.generation = 1;
        observed.terminal_observed = false;
        observed.termination = Some(Termination::Exit(code));
        seal_exit(&store, &observed)?;
        let record = store.finish(id, &dir, &observed, &fake, &call)?;
        let AttemptState::Terminal { outcome, .. } = record.state else {
            return Err("not terminal".into());
        };
        assert_eq!(
            outcome,
            if code == 0 {
                Outcome::Succeeded
            } else {
                Outcome::Failed
            }
        );
        assert_eq!(store.finish(id, &dir, &observed, &fake, &call)?, record);
        provenant_supervisor::custody::publish_release(&dir, observed.scope)?;
        provenant_supervisor::custody::publish_release(&dir, observed.scope)?;
    }
    assert_eq!(fake.decoded.get(), 2);
    let fact = store.attempt(batch.attempts[0].attempt_id, &call)?;
    assert_eq!(
        fact.output_refs[0].digest,
        CanonicalV1::file_digest(b"answer")
    );
    assert_eq!(store.run(batch.run_id, &call)?.closure, Closure::Closed);
    Ok(())
}

#[test]
fn never_spawned_exit_is_a_refusal_with_its_reason() -> TestResult {
    let temp = tempfile::tempdir_in("/tmp")?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(id, &route(), &call)?;
    let mut observed = exit();
    observed.scope.generation = 1;
    observed.never_spawned = true;
    observed.termination = None;
    observed.terminal_observed = false;
    observed.warnings = vec!["admission refused".into()];
    observed.settlement_evidence = provenant_supervisor::custody::SettlementEvidence::NeverSpawned;
    seal_exit(&store, &observed)?;
    let dir = store.attempt_dir(run.run_id, id, false)?;
    let fake = Fake {
        refuse: false,
        observed: None,
        decoded: Cell::new(0),
    };
    store.finish(id, &dir, &observed, &fake, &call)?;
    let fact = store.attempt(id, &call)?;
    assert_eq!(
        fact.terminal_state,
        provenant_records::AttemptOutcome::Rejected
    );
    assert_eq!(fact.reason.as_deref(), Some("admission refused"));
    assert_eq!(fake.decoded.get(), 0);
    Ok(())
}
