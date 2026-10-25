//! Dispatch, admission, holder and control behaviour at Run's owner boundary.
//! Fake providers and resources stand in for adapters and the host.
use super::tests::{call, clock, context, exit, input, route};
use super::*;
use provenant_core::{AttemptId, CancelToken, TaskId, WallTime};
use provenant_records::CanonicalV1;
use provenant_supervisor::{
    custody::ExitObservation, exec::Termination, fs::Directory, sys::resources::MemorySample,
};
use std::cell::{Cell, RefCell};
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn nested_lane_depth_stops_at_the_configured_maximum() {
    assert!(matches!(next_lane_depth(0, 2), Ok(1)));
    assert!(matches!(next_lane_depth(1, 2), Ok(2)));
    assert!(matches!(
        next_lane_depth(2, 2),
        Err(RunError::DepthExceeded {
            depth: 3,
            max_depth: 2
        })
    ));
}

#[test]
fn host_checks_child_metadata_against_its_parent_record() -> TestResult {
    let parent = AttemptId::from_bytes([9; 16]);
    for (depth, proposed_parent, valid) in [
        (Some(2), Some(parent), true),
        (Some(1), Some(parent), false),
        (None, Some(parent), false),
        (Some(2), Some(AttemptId::from_bytes([8; 16])), false),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut nested = input();
        nested.request.lane_depth = depth;
        nested.request.parent_attempt = proposed_parent;
        let record = store.create(&nested, &call)?;
        assert_eq!(record.parent, None);
        assert_eq!(record.parent_attempt, proposed_parent);
        assert_eq!(store.run(record.run_id, &call)?, record);
        assert_eq!(store.queued_runs(&call)?, vec![record.run_id]);
        assert_eq!(
            store
                .validate_child(
                    record.run_id,
                    parent,
                    &input().request.tasks[0],
                    1,
                    2,
                    &call
                )
                .is_ok(),
            valid
        );
        if !valid {
            store.refuse_queued(record.run_id, "host refused child metadata", &call)?;
            assert!(store.queued_runs(&call)?.is_empty());
            assert!(matches!(
                store.record(nested.attempts[0].attempt_id, &call)?.state,
                AttemptState::Terminal {
                    outcome: Outcome::Rejected,
                    ..
                }
            ));
        }
    }
    Ok(())
}

#[test]
fn host_refuses_child_authority_beyond_its_parent_task() -> TestResult {
    let parent = AttemptId::from_bytes([9; 16]);
    let writer = |worktree: &str| WriterRequest {
        project: "/project".into(),
        worktree: worktree.into(),
    };
    let deliver = |branch: &str, open_pr| WriterDeliveryRequest {
        remote: "origin".into(),
        branch: branch.into(),
        open_pr,
    };
    let read_only = input().request.tasks[0].clone();
    let mut writing = read_only.clone();
    writing.writer = Some(writer("/project/.worktrees/parent"));
    let mut delivering = writing.clone();
    delivering.deliver = Some(deliver("feature", true));
    // The host holds no sealed list of worktrees a lane may delegate, so no
    // lane can propose a writer or delivering child, even its own worktree.
    for (parent_task, child_writer, child_delivery, case) in [
        (
            &read_only,
            None,
            None,
            "read-only child of read-only parent",
        ),
        (&writing, None, None, "read-only child of writer parent"),
        (
            &read_only,
            Some("/project/.worktrees/child"),
            None,
            "writer child of read-only parent",
        ),
        (
            &writing,
            Some("/project/.worktrees/parent"),
            None,
            "writer child in its parent's worktree",
        ),
        (
            &writing,
            Some("/project/.worktrees/other"),
            None,
            "writer child in a sibling worktree",
        ),
        (
            &delivering,
            Some("/project/.worktrees/parent"),
            Some(deliver("feature", false)),
            "delivery child within parent grant",
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut nested = input();
        nested.request.lane_depth = Some(2);
        nested.request.parent_attempt = Some(parent);
        nested.request.tasks[0].writer = child_writer.map(writer);
        nested.request.tasks[0].deliver = child_delivery;
        let proposes_writer =
            nested.request.tasks[0].writer.is_some() || nested.request.tasks[0].deliver.is_some();
        let record = store.create(&nested, &call)?;
        let result = store.validate_child(record.run_id, parent, parent_task, 1, 2, &call);
        if proposes_writer {
            assert!(
                matches!(
                    result,
                    Err(RunError::Validation("a lane cannot propose a writer child"))
                ),
                "{case}: {result:?}"
            );
        } else {
            assert!(result.is_ok(), "{case}: {result:?}");
        }
    }
    Ok(())
}

#[test]
fn host_refuses_child_access_beyond_its_parent_task() -> TestResult {
    let parent = AttemptId::from_bytes([9; 16]);
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    for name in ["reads/sub", "writes/sub", "outside", "home/.config"] {
        std::fs::create_dir_all(base.join(name))?;
    }
    std::os::unix::fs::symlink(base.join("outside"), base.join("reads/alias"))?;
    let access = |network, reads: &[&str], writes: &[&str]| LaneAccess {
        network,
        read_roots: reads.iter().map(|path| base.join(path)).collect(),
        write_roots: writes.iter().map(|path| base.join(path)).collect(),
    };
    let task = |writer: bool, access: LaneAccess| {
        let mut task = input().request.tasks[0].clone();
        task.writer = writer.then(|| WriterRequest {
            project: "/project".into(),
            worktree: "/project/.worktrees/sibling".into(),
        });
        task.access = access;
        task
    };
    let read_only = task(false, LaneAccess::default());
    let granted = task(
        false,
        access(Some(NetworkAccess::Local), &["reads"], &["writes"]),
    );
    let quiet_writer = task(true, access(Some(NetworkAccess::Provider), &[], &[]));
    for (parent_task, child, valid, case) in [
        (
            &read_only,
            task(false, LaneAccess::default()),
            true,
            "default child",
        ),
        (
            &read_only,
            task(false, access(None, &[], &["writes"])),
            false,
            "write root under a read-only parent",
        ),
        (
            &read_only,
            task(false, access(None, &["home/.config"], &[])),
            false,
            "extra read root under a read-only parent",
        ),
        (
            &read_only,
            task(false, access(Some(NetworkAccess::Open), &[], &[])),
            false,
            "network under a read-only parent",
        ),
        (
            &granted,
            task(
                false,
                access(
                    Some(NetworkAccess::Local),
                    &["reads/sub", "writes"],
                    &["writes/sub"],
                ),
            ),
            true,
            "child within parent roots",
        ),
        (
            &granted,
            task(false, access(Some(NetworkAccess::Open), &[], &[])),
            false,
            "network beyond parent network",
        ),
        (
            &granted,
            task(false, access(None, &[], &["reads/sub"])),
            false,
            "write root inside a parent read root",
        ),
        (
            &granted,
            task(false, access(None, &["reads/alias"], &[])),
            false,
            "symlinked read root escaping the parent root",
        ),
        (
            &quiet_writer,
            task(false, LaneAccess::default()),
            true,
            "read-only default child of a quiet writer",
        ),
        (
            &quiet_writer,
            task(false, access(Some(NetworkAccess::Local), &[], &[])),
            false,
            "network beyond a writer parent's network",
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut nested = input();
        nested.request.lane_depth = Some(2);
        nested.request.parent_attempt = Some(parent);
        nested.request.tasks[0] = child;
        let record = store.create(&nested, &call)?;
        let result = store.validate_child(record.run_id, parent, parent_task, 1, 2, &call);
        assert_eq!(result.is_ok(), valid, "{case}: {result:?}");
    }
    Ok(())
}

#[test]
fn host_launches_only_the_validated_child_request() -> TestResult {
    let parent = AttemptId::from_bytes([9; 16]);
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let parent_task = input().request.tasks[0].clone();
    let mut nested = input();
    nested.request.lane_depth = Some(2);
    nested.request.parent_attempt = Some(parent);
    let record = store.create(&nested, &call)?;
    let validated = store.validate_child(record.run_id, parent, &parent_task, 1, 2, &call)?;

    // The lane swaps its sealed request and the recorded digest together.
    let mut swapped = validated.clone();
    swapped.tasks[0].access.network = Some(NetworkAccess::Open);
    store.run_dir(record.run_id, false)?.atomic_write(
        std::ffi::OsStr::new("request.json"),
        ".swap-",
        &crate::db::bytes(&swapped)?,
        provenant_supervisor::fs::PublishMode::Replace,
    )?;
    let mut run = store.run(record.run_id, &call)?;
    run.request_digest = swapped.digest()?;
    store.db.execute(
        "UPDATE runs SET record=?1 WHERE id=?2",
        rusqlite::params![crate::db::bytes(&run)?, run.run_id.to_string()],
    )?;
    assert_eq!(store.request(record.run_id, &call)?, swapped);
    let fake = Fake::default();
    assert!(matches!(
        store.start_child(record.run_id, &validated, &fake, &|_| unreachable!(), &call),
        Err(RunError::Validation(
            "child request changed after validation"
        ))
    ));
    assert_eq!(fake.prompt.borrow().as_str(), "");

    // A child cannot continue an attempt, whose retained state the host copies.
    let refuse = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start_child(record.run_id, &swapped, &refuse, &|_| unreachable!(), &call)?;
    let continued = store.resume(
        &Resume {
            prior: nested.attempts[0].attempt_id,
            expected_generation: 0,
            operation_id: provenant_core::OperationId::parse("continue")
                .map_err(|_| RunError::Validation("operation"))?,
            run_id: provenant_core::RunId::from_bytes([20; 16]),
            attempt_id: AttemptId::from_bytes([21; 16]),
            prompt: "answer".into(),
            created_at: WallTime::from_micros(100).map_err(|_| RunError::Validation("time"))?,
        },
        &call,
    )?;
    assert!(matches!(
        store.validate_child(continued.run_id, parent, &parent_task, 1, 2, &call),
        Err(RunError::Validation(
            "a child run cannot continue an attempt"
        ))
    ));
    Ok(())
}

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
#[derive(Default)]
struct Fake {
    refuse: bool,
    observed: Option<RouteRecord>,
    decoded: Cell<usize>,
    resumed: Cell<Option<AttemptId>>,
    prompt: RefCell<String>,
}
impl Providers for Fake {
    fn prepare(
        &self,
        _: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String> {
        assert!(paths.temporary.ends_with("tmp"));
        assert!(paths.temporary.is_dir());
        self.prompt.replace(prompt.into());
        self.resumed.set(resume.map(|r| r.attempt_id));
        if self.refuse {
            return Err("fixture adapter is disabled".into());
        }
        unreachable!("these tests never hand off to a guardian")
    }
    fn decode(
        &self,
        applied: &RouteRecord,
        stdout: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        assert_eq!(applied, &route());
        self.decoded.set(self.decoded.get() + 1);
        Ok(Decoded {
            outcome: Outcome::Succeeded,
            result: String::from_utf8_lossy(stdout).into_owned(),
            complete: true,
            observed_route: self.observed.clone(),
            usage: UsageRecord::default(),
            session: None,
            reason: None,
        })
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
pub(super) fn seal_exit(store: &RunStore, exit: &ExitObservation) -> TestResult {
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
fn skipped_fallback_candidates_are_recorded_on_the_attempt() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    let skipped = vec![SkippedRoute {
        route: "claude".into(),
        kind: SkipKind::AuthRequired,
        detail: "account work: sign in again".into(),
    }];
    store.record_skipped_routes(id, &skipped, &call)?;
    let started = store.mark_starting(id, &route(), &[], None, &call)?;
    assert_eq!(started.skipped_routes, skipped);
    assert_eq!(store.record(id, &call)?.skipped_routes, skipped);
    Ok(())
}

#[test]
fn admission_holds_run_concurrency_spacing_floors_and_deadline() -> TestResult {
    let temp = tempfile::tempdir()?;
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
        store.mark_starting(id, &route(), &[], None, &call)?;
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
    store.mark_starting(id, &route(), &[], None, &call)?;
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
    store.mark_starting(id, &route(), &[], None, &call)?;
    let starved = Host { available: 1 };
    assert!(matches!(
        store.admit(id, &attempt, &starved, &clock, &cancel),
        Err(RunError::Refused)
    ));
    Ok(())
}

#[test]
fn child_stores_share_the_host_ledger_and_recovery_frees_a_lease() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let now = WallTime::from_micros(100)?;
    let base = std::fs::canonicalize(temp.path())?;
    let host = base.join("host");
    let mut stores = Vec::new();
    for name in ["first", "second"] {
        std::fs::create_dir(base.join(name))?;
        let mut store =
            RunStore::open(&context(&base.join(name))?, &call)?.with_writer_registry(&host);
        store.set_admission_policy(AdmissionPolicy {
            startup_spacing_ms: 0,
            ..AdmissionPolicy::default()
        });
        stores.push(store);
    }
    // Each store fills half the project's host-wide limit with its own run.
    let half = PROJECT_ACTIVE / 2;
    let batches = [batch(1, half), batch(2, half)];
    for (store, run) in stores.iter().zip(&batches) {
        store.create(run, &call)?;
        for attempt in &run.attempts {
            store.mark_starting(attempt.attempt_id, &route(), &[], None, &call)?;
            store
                .claim(attempt.attempt_id, now, &call)?
                .ok_or("within the host limit")?;
        }
    }
    // Counted per store, the second store would admit a ninth attempt.
    let extra = batch(3, 1);
    let waiting = extra.attempts[0].attempt_id;
    stores[1].create(&extra, &call)?;
    stores[1].mark_starting(waiting, &route(), &[], None, &call)?;
    assert!(
        stores[1].claim(waiting, now, &call)?.is_none(),
        "both child stores count against one project limit"
    );
    // The first attempt's owner dies; recovery commits its terminal from the
    // sealed exit, as reconcile does, and that frees its host lease.
    let dead = batches[0].attempts[0].attempt_id;
    let dir = stores[0].attempt_dir(batches[0].run_id, dead, false)?;
    artifacts::seal(&dir, "stdout.log", b"partial", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope.run = batches[0].run_id;
    observed.scope.attempt = dead;
    observed.scope.generation = 1;
    observed.termination = Some(Termination::Signal(9));
    seal_exit(&stores[0], &observed)?;
    let fake = Fake {
        refuse: false,
        observed: Some(route()),
        ..Fake::default()
    };
    stores[0].finish(dead, &dir, &observed, &fake, &call)?;
    let admitted = stores[1]
        .claim(waiting, now, &call)?
        .ok_or("recovered lease not freed")?;
    assert_eq!(admitted.state, AttemptState::Running);
    Ok(())
}
/// A run of `tasks` attempts at full concurrency, with IDs unique to `seed`.
fn batch(seed: u8, tasks: u64) -> PreparedRun {
    let mut run = input();
    let template = run.request.tasks[0].clone();
    run.run_id = provenant_core::RunId::from_bytes([seed; 16]);
    run.operation_id = provenant_core::OperationId::parse(&format!("batch-{seed}")).unwrap();
    run.request.concurrency = tasks.min(8);
    run.request.tasks.clear();
    run.attempts.clear();
    for index in 0..tasks as u8 {
        let mut task = template.clone();
        task.task_id = TaskId::from_bytes([seed, index, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
        run.attempts.push(NewAttempt {
            task_id: task.task_id,
            attempt_id: AttemptId::from_bytes([
                seed, index, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            ]),
            prompt: "exact prompt".into(),
        });
        run.request.tasks.push(task);
    }
    run
}

#[test]
fn prepare_refusal_is_terminal_without_custody() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let fake = Fake {
        refuse: true,
        observed: None,
        ..Fake::default()
    };
    let started = store.start(run.run_id, &fake, &|_| unreachable!(), &call)?;
    assert!(
        matches!(&started[..], [Started::Refused { reason, .. }] if reason.contains("disabled"))
    );
    let prompt = fake.prompt.borrow();
    let path = store.context.state_root.as_path().join(format!(
        "runs/{}/attempts/{}",
        run.run_id, run.attempts[0].attempt_id
    ));
    assert!(prompt.contains(&path.join("result.md").display().to_string()));
    assert!(prompt.contains(&path.join("process-exit.json").display().to_string()));
    assert!(prompt.contains("only durable outputs"));
    assert!(prompt.contains("deleted"));
    assert!(prompt.ends_with("exact prompt"));
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
    let temp = tempfile::tempdir()?;
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
        ..Fake::default()
    };
    for (index, code) in [(0, 0), (1, 1)] {
        let id = batch.attempts[index].attempt_id;
        store.mark_starting(id, &route(), &[], None, &call)?;
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
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(id, &route(), &[], None, &call)?;
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
        ..Fake::default()
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

#[test]
fn private_temporary_directories_are_removed_after_terminal_and_withdrawal() -> TestResult {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = two_tasks(2);
    store.create(&run, &call)?;
    let outside = temp.path().join("outside.txt");
    std::fs::write(&outside, "keep")?;

    let withdrawn = run.attempts[0].attempt_id;
    let withdrawn_tmp = store.temporary_path(withdrawn, &call)?;
    std::fs::write(withdrawn_tmp.join("provider.tmp"), "discard")?;
    symlink(&outside, withdrawn_tmp.join("outside-link"))?;
    store.withdraw(
        withdrawn,
        AttemptState::Queued,
        Outcome::Cancelled,
        "cancelled before start",
        WallTime::from_micros(150)?,
        &call,
    )?;
    assert!(!withdrawn_tmp.exists());

    let terminal = run.attempts[1].attempt_id;
    let terminal_tmp = store.temporary_path(terminal, &call)?;
    std::fs::write(terminal_tmp.join("provider.tmp"), "discard")?;
    symlink(&outside, terminal_tmp.join("outside-link"))?;
    let mut observed = exit();
    observed.scope.attempt = terminal;
    observed.termination = Some(Termination::Exit(1));
    seal_exit(&store, &observed)?;
    let publication = TerminalPublication {
        outcome: Outcome::Failed,
        finished_at: WallTime::from_micros(160)?,
        started_at: Some(WallTime::from_micros(120)?),
        provider_ms: Some(1),
        reason: Some("fixture failure".into()),
        result: "",
        complete: true,
        redacted: false,
        resolved_route: Some(route()),
        applied_route: Some(route()),
        observed_route: Some(route()),
        usage: UsageRecord::default(),
        session: None,
    };
    store.publish_terminal(terminal, 1, &publication, &observed, &call)?;
    assert!(!terminal_tmp.exists());
    assert_eq!(std::fs::read_to_string(outside)?, "keep");
    Ok(())
}

fn terminal(store: &RunStore, id: AttemptId, call: &Request<'_>) -> Result<Outcome> {
    match store.record(id, call)?.state {
        AttemptState::Terminal { outcome, .. } => Ok(outcome),
        _ => Err(RunError::Nonterminal),
    }
}

#[test]
fn cancel_ends_queued_work_and_stops_admission_before_spawn() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let [queued, starting] = [batch.attempts[0].attempt_id, batch.attempts[1].attempt_id];
    let op = provenant_core::OperationId::parse("stop")?;
    store.bind_cancel(&op, &queued.to_string(), &call)?;
    store.bind_cancel(&op, &queued.to_string(), &call)?;
    assert!(matches!(
        store.bind_cancel(&op, &starting.to_string(), &call),
        Err(RunError::OperationConflict)
    ));

    assert_eq!(
        store.cancel(queued, "stop", &call)?,
        CancelReceipt::Cancelled
    );
    assert_eq!(terminal(&store, queued, &call)?, Outcome::Cancelled);
    assert!(store.attempt(queued, &call)?.started_at.is_none());
    assert_eq!(
        store.cancel(queued, "stop", &call)?,
        CancelReceipt::AlreadyTerminal
    );

    store.mark_starting(starting, &route(), &[], None, &call)?;
    // Before the handoff publishes its launch summary there is nothing to
    // stop; cancel retries until its budget ends, then says so.
    let system = provenant_supervisor::clock::SystemClock::new();
    let spent = Request {
        clock: &system,
        budget: provenant_core::WaitBudget::cli(provenant_core::Clock::mono_now(&system), 1)?,
        cancel: &cancel,
    };
    // A held custody lock is a live handoff: cancel leaves it untouched.
    let scope = provenant_supervisor::custody::Scope {
        run: batch.run_id,
        attempt: starting,
        generation: 1,
    };
    let held = provenant_supervisor::fixture::acquire_writer(
        store.attempt_dir(batch.run_id, starting, false)?,
        scope,
        call.budget,
        &cancel,
        &clock,
    )?;
    assert!(matches!(
        store.cancel(starting, "stop", &spent)?,
        CancelReceipt::Unobservable { .. }
    ));
    drop(held);
    // Supervisor's stop writes cancel.json; the waiting guardian sees it.
    let dir = store.attempt_dir(batch.run_id, starting, false)?;
    artifacts::seal(&dir, "cancel.json", b"{}", RECORD_LIMIT)?;
    let host = Host { available: 100 };
    assert!(matches!(
        store.admit(starting, &dir, &host, &clock, &cancel),
        Err(RunError::Cancelled)
    ));
    let mut observed = exit();
    observed.scope.attempt = starting;
    observed.scope.generation = 1;
    observed.never_spawned = true;
    observed.termination = None;
    observed.terminal_observed = false;
    observed.settlement_evidence = provenant_supervisor::custody::SettlementEvidence::NeverSpawned;
    seal_exit(&store, &observed)?;
    store.finish(starting, &dir, &observed, &Fake::default(), &call)?;
    assert_eq!(terminal(&store, starting, &call)?, Outcome::Cancelled);
    assert_eq!(store.run(batch.run_id, &call)?.closure, Closure::Closed);
    Ok(())
}

#[test]
fn resume_continues_a_terminal_attempt_once_with_its_route() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut run = input();
    let outer_parent = AttemptId::from_bytes([19; 16]);
    run.request.lane_depth = Some(1);
    run.request.parent_attempt = Some(outer_parent);
    run.request.tasks[0].account = Some("fixture-account".into());
    store.create(&run, &call)?;
    let prior = run.attempts[0].attempt_id;
    let resume = |operation: &str, expected_generation, byte| -> Result<Resume> {
        Ok(Resume {
            prior,
            expected_generation,
            operation_id: provenant_core::OperationId::parse(operation)
                .map_err(|_| RunError::Validation("operation"))?,
            run_id: provenant_core::RunId::from_bytes([byte; 16]),
            attempt_id: AttemptId::from_bytes([byte + 1; 16]),
            prompt: "answer".into(),
            created_at: WallTime::from_micros(100).map_err(|_| RunError::Validation("time"))?,
        })
    };
    assert!(matches!(
        store.resume(&resume("r1", 0, 20)?, &call),
        Err(RunError::Nonterminal)
    ));
    let fake = Fake {
        refuse: true,
        ..Fake::default()
    };
    store.start(run.run_id, &fake, &|_| unreachable!(), &call)?;
    assert_eq!(fake.resumed.get(), None);

    let continued = store.resume(&resume("r1", 0, 20)?, &call)?;
    assert_eq!(continued.parent, Some(prior));
    assert_eq!(
        store.request(continued.run_id, &call)?.tasks[0]
            .account
            .as_deref(),
        Some("fixture-account")
    );
    assert_eq!(store.resume(&resume("r1", 0, 20)?, &call)?, continued);
    let mut changed = resume("r1", 0, 20)?;
    changed.prompt = "other".into();
    assert!(matches!(
        store.resume(&changed, &call),
        Err(RunError::OperationConflict)
    ));
    for expected in [0, 1] {
        assert!(matches!(
            store.resume(&resume("r2", expected, 30)?, &call),
            Err(RunError::Conflict)
        ));
    }
    let next = AttemptId::from_bytes([21; 16]);
    let record = store.record(next, &call)?;
    assert_eq!(record.retry_of, Some(prior));
    assert_eq!(record.attempt_number, 2);
    assert_eq!(
        record.requested_route,
        store.record(prior, &call)?.requested_route
    );
    store.start(continued.run_id, &fake, &|_| unreachable!(), &call)?;
    assert_eq!(fake.resumed.get(), Some(prior));
    Ok(())
}

#[test]
fn cancel_settles_a_prepared_attempt_nobody_holds() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let batch = two_tasks(1);
    store.create(&batch, &call)?;
    let orphan = batch.attempts[0].attempt_id;
    store.mark_starting(orphan, &route(), &[], None, &call)?;
    let scope = provenant_supervisor::custody::Scope {
        run: batch.run_id,
        attempt: orphan,
        generation: 1,
    };
    drop(provenant_supervisor::fixture::acquire_writer(
        store.attempt_dir(batch.run_id, orphan, false)?,
        scope,
        call.budget,
        &cancel,
        &clock,
    )?);
    let system = provenant_supervisor::clock::SystemClock::new();
    let spent = Request {
        clock: &system,
        budget: provenant_core::WaitBudget::cli(provenant_core::Clock::mono_now(&system), 1)?,
        cancel: &cancel,
    };
    assert_eq!(
        store.cancel(orphan, "stop", &spent)?,
        CancelReceipt::Cancelled
    );
    assert_eq!(terminal(&store, orphan, &call)?, Outcome::Rejected);
    assert_eq!(
        store.cancel(orphan, "stop", &spent)?,
        CancelReceipt::AlreadyTerminal
    );
    Ok(())
}

#[test]
fn reconcile_finishes_a_durable_exit_and_withdraws_a_launch_that_never_spawned() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
        ..AdmissionPolicy::default()
    });
    let batch = two_tasks(2);
    store.create(&batch, &call)?;
    let [exited, orphan] = [batch.attempts[0].attempt_id, batch.attempts[1].attempt_id];

    // The guardian sealed an exit but died before its terminal commit.
    store.mark_starting(exited, &route(), &[], None, &call)?;
    store
        .claim(exited, WallTime::from_micros(100)?, &call)?
        .ok_or("not admitted")?;
    let dir = store.attempt_dir(batch.run_id, exited, false)?;
    artifacts::seal(&dir, "stdout.log", b"answer", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope.attempt = exited;
    observed.scope.generation = 1;
    let summary = serde_json::json!({"schema_version": 1, "scope": observed.scope, "requester": provenant_core::ActorId::from_bytes([2; 16])});
    artifacts::seal(
        &dir,
        "launch.json",
        &serde_json::to_vec(&summary)?,
        RECORD_LIMIT,
    )?;
    seal_exit(&store, &observed)?;

    // A handoff whose reservation never reached a guardian.
    store.mark_starting(orphan, &route(), &[], None, &call)?;
    let scope = provenant_supervisor::custody::Scope {
        run: batch.run_id,
        attempt: orphan,
        generation: 1,
    };
    drop(provenant_supervisor::fixture::acquire_writer(
        store.attempt_dir(batch.run_id, orphan, false)?,
        scope,
        call.budget,
        &cancel,
        &clock,
    )?);

    let fake = Fake {
        observed: Some(route()),
        ..Fake::default()
    };
    let found = store.reconcile(batch.run_id, &fake, &call)?;
    assert_eq!(
        found,
        vec![
            (exited, Reconciled::Finished),
            (orphan, Reconciled::Withdrawn)
        ]
    );
    assert_eq!(terminal(&store, exited, &call)?, Outcome::Succeeded);
    assert!(artifacts::read_sealed(&dir, "release.json", RECORD_LIMIT)?.is_some());
    assert_eq!(terminal(&store, orphan, &call)?, Outcome::Rejected);
    assert!(store.reconcile(batch.run_id, &fake, &call)?.is_empty());
    Ok(())
}
#[test]
fn reconcile_settles_a_writer_attempt_whose_worktree_was_removed() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = input();
    run.request.tasks[0].writer = Some(WriterRequest {
        project: temp.path().join("checkout"),
        worktree: temp.path().join("removed-lane"),
    });
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(id, &route(), &[], None, &call)?;
    store
        .claim(id, WallTime::from_micros(100)?, &call)?
        .ok_or("not admitted")?;
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let scope = provenant_supervisor::custody::Scope {
        run: run.run_id,
        attempt: id,
        generation: 1,
    };
    // The guardian reserved the writer and sealed an exit; its worktree is gone.
    let custody = context
        .state_root
        .as_path()
        .join("worktrees")
        .join(context.project_id.to_string())
        .join("lane");
    std::fs::create_dir_all(&custody)?;
    drop(provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&custody)?,
        scope,
        call.budget,
        &cancel,
        &clock,
    )?);
    let meta = attempt.metadata()?;
    let reservation = custody.join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let fields = value.as_object_mut().ok_or("reservation")?;
    fields.insert("launch_state".into(), "never_spawned".into());
    fields.insert("unresolved_cleanup".into(), false.into());
    fields.insert("census_incomplete".into(), false.into());
    fields.insert("attempt_device".into(), meta.device.into());
    fields.insert("attempt_inode".into(), meta.inode.into());
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    artifacts::seal(&attempt, "stdout.log", b"answer", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = scope;
    let summary = serde_json::json!({"schema_version": 1, "scope": scope, "requester": provenant_core::ActorId::from_bytes([2; 16])});
    artifacts::seal(
        &attempt,
        "launch.json",
        &serde_json::to_vec(&summary)?,
        RECORD_LIMIT,
    )?;
    seal_exit(&store, &observed)?;

    let fake = Fake {
        observed: Some(route()),
        ..Fake::default()
    };
    let found = store.reconcile(run.run_id, &fake, &call)?;
    assert_eq!(found, vec![(id, Reconciled::Finished)]);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Lost);
    assert!(store.writer_fences()?.is_empty());
    assert!(store.reconcile(run.run_id, &fake, &call)?.is_empty());
    Ok(())
}
#[test]
fn reconcile_withdraws_a_start_that_died_before_reserving() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    let id = run.attempts[0].attempt_id;
    store.create(&run, &call)?;
    store.mark_starting(id, &route(), &[], None, &call)?;
    let found = store.reconcile(run.run_id, &Fake::default(), &call)?;
    assert_eq!(found, vec![(id, Reconciled::Withdrawn)]);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Rejected);
    Ok(())
}

#[test]
fn read_only_exceptions_are_recorded_and_temporary_paths_are_private_per_attempt() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let prepared = two_tasks(2);
    store.create(&prepared, &call)?;
    let first = prepared.attempts[0].attempt_id;
    let second = prepared.attempts[1].attempt_id;
    let first_tmp = store.temporary_path(first, &call)?;
    let second_tmp = store.temporary_path(second, &call)?;
    assert_ne!(first_tmp, second_tmp);
    assert!(
        first_tmp.starts_with(
            context
                .state_root
                .as_path()
                .join("runs")
                .join(prepared.run_id.to_string())
                .join("attempts")
                .join(first.to_string())
        )
    );
    assert_eq!(
        std::fs::metadata(&first_tmp)?.permissions().mode() & 0o777,
        0o700
    );
    let observed = provenant_core::GitOid::Sha1([7; 20]);
    store.mark_starting(
        first,
        &route(),
        &[".agent-run/locks".into()],
        Some(observed),
        &call,
    )?;
    drop(store);
    let store = RunStore::open(&context, &call)?;
    assert_eq!(
        store.record(first, &call)?.read_only_writable,
        [".agent-run/locks"]
    );
    assert!(store.record(second, &call)?.read_only_writable.is_empty());
    // The host-observed head of a read-only lane persists; unobserved stays absent.
    assert_eq!(store.record(first, &call)?.head, Some(observed));
    assert_eq!(store.record(second, &call)?.head, None);
    // Old v1 records omit the added field and retain an empty default.
    let mut record = serde_json::to_value(store.record(first, &call)?)?;
    record
        .as_object_mut()
        .ok_or("record object")?
        .remove("read_only_writable");
    assert!(
        serde_json::from_value::<AttemptRecord>(record)?
            .read_only_writable
            .is_empty()
    );
    Ok(())
}
