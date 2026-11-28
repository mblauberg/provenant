//! Unit tests for the status-wait engine, kept beside it so the production
//! file stays within its line ceiling.
#[test]
fn pr_journal_display_fields_degrade_without_unavailable() {
    for value in [
        None,
        Some(serde_json::Value::Null),
        Some(serde_json::json!([])),
    ] {
        let mut status = serde_json::json!({"number":7,"state":"OPEN","head":"abc",
                "checks_total":1,"checks_failed":0,"checks_pending":0,"review":"APPROVED"});
        if let Some(value) = value {
            status["failing_checks"] = value;
        }
        let reading = event_reading(7, false, serde_json::json!({"status":status}));
        assert_eq!(reading.settle, super::Settle::Succeeded);
        assert!(!reading.line.contains("unavailable"));
    }
    for field in [
        "draft",
        "checks_passed",
        "checks_total",
        "failing_checks",
        "pending_checks",
        "merge",
    ] {
        let mut status = serde_json::json!({"number":7,"state":"OPEN","head":"abc"});
        status[field] = serde_json::json!({"invalid":true});
        let reading = event_reading(7, false, serde_json::json!({"status":status}));
        assert!(!reading.line.contains("unavailable"), "{field}");
        assert!(reading.line.contains("warning"), "{field}");
        assert_eq!(reading.settle, super::Settle::Pending, "{field}");
    }
    // These fields affect the verdict. Malformed values fail closed as an
    // unavailable, still-pending observation instead of defaulting to a
    // successful or terminal classification.
    for field in ["checks_failed", "checks_pending", "review", "error"] {
        let mut status = serde_json::json!({"number":7,"state":"OPEN","head":"abc"});
        status[field] = serde_json::json!({"invalid":true});
        let reading = event_reading(7, false, serde_json::json!({"status":status}));
        assert_eq!(reading.settle, super::Settle::Pending, "{field}");
        assert!(reading.line.contains("unavailable"), "{field}");
    }
    for field in ["number", "state", "head"] {
        let mut status = serde_json::json!({"number":7,"state":"OPEN","head":"abc"});
        status.as_object_mut().unwrap().remove(field);
        assert!(
            event_reading(7, false, serde_json::json!({"status":status}))
                .line
                .contains("unavailable")
        );
    }
}

use super::*;
#[allow(unused_imports)] // Also compiled by the harness=false worktree driver.
use provenant_core::Clock as _;

#[test]
fn graph_wait_worker_spawn_failure_preserves_the_cause() {
    let error = graph_wait_spawn_error(std::io::Error::other("thread limit"));
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert!(
        error
            .to_string()
            .contains("cannot spawn graph wait worker: thread limit")
    );
}

/// Render an event detail with a pinned-no-head subject, the same shape a
/// direct reading has before the forge reports a head.
fn event_reading(number: u64, complete: bool, detail: Value) -> Reading {
    let mut subject = None;
    let mut pinned_head_seen = false;
    reading_pr_event(
        &mut subject,
        &mut pinned_head_seen,
        &PrSample {
            number,
            repository: &Ok("owner/repo".into()),
            policy: SubjectPolicy::Pinned,
            policy_source: &super::test_policy_source(),
            now: WallTime::UNIX_EPOCH,
        },
        complete,
        detail,
    )
}

fn oid(text: &str) -> GitOid {
    GitOid::parse(text).expect("fixture OID")
}

fn pr_line(head: GitOid, passed: bool) -> crate::pr::status::Line {
    let (review, passed_count, failed) = if passed {
        ("approved", 1, 0)
    } else {
        ("changes_requested", 0, 1)
    };
    serde_json::from_value(json!({
        "number": 7, "state": "OPEN", "head": head.to_string(), "draft": false,
        "merge": "clean", "review": review, "error": null,
        "checks_passed": passed_count, "checks_total": 1,
        "checks_failed": failed, "checks_pending": 0
    }))
    .expect("fixture PR line")
}

#[test]
fn a_selection_with_peer_or_relayed_attempts_is_not_locally_represented() {
    let row = |id: &str| json!({"kind": "attempt", "attempt_id": id});
    let here = "aa".to_owned();
    assert!(rows_local(
        &[row("aa/1"), json!({"kind": "graph"})],
        Some(&here)
    ));
    assert!(!rows_local(&[row("aa/1"), row("bb/2")], Some(&here)));
    assert!(!rows_local::<String>(&[row("aa/1")], None));
}

#[test]
fn completed_peer_source_remains_in_final_observation() {
    let host = provenant_core::HostId::parse(&"a".repeat(32)).expect("fixture host");
    let completed_peer = Reading {
        settle: Settle::Succeeded,
        line: "peer settled".into(),
        data: json!({"rows":[{"kind":"attempt","attempt_id":format!("{}/2", "b".repeat(32))}]}),
    };
    let mut history = ObservationHistory::default();
    history.record(&completed_peer, Some(&host));

    let final_observation = history.render(
        None,
        &LocalCadence::compiled(Duration::from_millis(500)),
        Some("event source unavailable: fixture parent missing"),
    );
    assert!(completed_peer.done());
    assert!(final_observation.contains("peer/relay or unrepresented rows reread every 500 ms"));
}

#[test]
fn local_observation_reports_the_fallback_source_and_reason() {
    let observation = LocalObservation::paced(
        LocalCadence::compiled(Duration::from_millis(40)),
        "fixture has no native event backend",
    );
    let report = observation.report(false);
    assert!(
        report.contains(provenant_fabric_runtime::coordination::LOCAL_OBSERVATION_INTERVAL_KEY)
    );
    assert!(report.contains("fixture has no native event backend"));
}

#[test]
fn repeated_short_waits_eventually_reach_one_paced_fallback_tick() {
    let interval = Duration::from_millis(40);
    let mut observation = LocalObservation::paced(
        LocalCadence::compiled(interval),
        "fixture event source unavailable",
    );
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_micros(0).expect("valid fixture time"),
        provenant_core::MonoTime::from_micros(0),
    );
    for _ in 0..3 {
        assert_eq!(
            observation
                .wait(Duration::from_millis(10), &CancelToken::new(), &clock)
                .expect("fixture clock can advance"),
            ObservationWake::NativeTimeout
        );
    }
    assert_eq!(
        observation
            .wait(Duration::from_millis(10), &CancelToken::new(), &clock)
            .expect("fixture clock can advance"),
        ObservationWake::PacedFallback
    );
    assert_eq!(clock.mono_now().as_micros(), 40_000);
    assert_eq!(
        observation
            .wait(Duration::from_millis(10), &CancelToken::new(), &clock)
            .expect("next cadence remains paced"),
        ObservationWake::NativeTimeout
    );
    assert_eq!(clock.mono_now().as_micros(), 50_000);
}

#[test]
fn registration_does_not_create_an_event_log_and_empty_timeout_is_not_a_read() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let log = directory.path().join("graph.events.jsonl");
    let mut observation = LocalObservation::subscribe(
        std::slice::from_ref(&log),
        LocalCadence::compiled(Duration::from_secs(1)),
    );
    assert!(!log.exists());
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_micros(0).expect("valid fixture time"),
        provenant_core::MonoTime::from_micros(0),
    );
    let wake = observation
        .wait(Duration::ZERO, &CancelToken::new(), &clock)
        .expect("zero deadline does not touch the clock");
    assert_eq!(wake, ObservationWake::NativeTimeout);
}

#[test]
fn a_short_fallback_deadline_is_not_mistaken_for_a_paced_tick() {
    let mut observation = LocalObservation::paced(
        LocalCadence::compiled(Duration::from_secs(1)),
        "fixture has no native event backend",
    );
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_micros(0).expect("valid fixture time"),
        provenant_core::MonoTime::from_micros(0),
    );
    let wake = observation
        .wait(Duration::ZERO, &CancelToken::new(), &clock)
        .expect("zero deadline does not touch the clock");
    assert_eq!(wake, ObservationWake::NativeTimeout);
}

#[test]
fn graph_events_read_only_local_sources_and_remote_deadlines_add_remote_sources() {
    let pending = [0, 1, 2];
    let remote = [1, 2].into_iter().collect();
    let as_set = |indices: Vec<usize>| {
        indices
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    };
    let event_reads = graph_reads_for_wake(&pending, &remote, GraphWake::Event);
    assert_eq!(as_set(event_reads.clone()), [0].into_iter().collect());
    let native_timeout = graph_reads_for_wake(&pending, &remote, GraphWake::NativeTimeout);
    assert!(native_timeout.is_empty());
    assert_eq!(
        graph_reads_for_wake(&pending, &remote, GraphWake::StallDeadline),
        [0]
    );
    let mut due_event_reads = event_reads;
    include_due_remote_reads(&mut due_event_reads, &pending, &remote, true);
    assert_eq!(as_set(due_event_reads), pending.into_iter().collect());
    let mut due_timeout_reads = native_timeout;
    include_due_remote_reads(&mut due_timeout_reads, &pending, &remote, true);
    assert_eq!(as_set(due_timeout_reads), [1, 2].into_iter().collect());
    assert_eq!(
        as_set(graph_reads_for_wake(
            &pending,
            &remote,
            GraphWake::PacedFallback
        )),
        pending.into_iter().collect()
    );
}

#[test]
fn a_flood_of_local_events_cannot_starve_a_due_remote_poll() {
    let pending = [0, 1];
    let remote = [1].into_iter().collect();
    let origin = Instant::now();
    let interval = Duration::from_millis(100);
    let next_remote_poll = origin + interval;

    for elapsed in (0..100).step_by(10) {
        let now = origin + Duration::from_millis(elapsed);
        let mut reads = graph_reads_for_wake(&pending, &remote, GraphWake::Event);
        include_due_remote_reads(&mut reads, &pending, &remote, now >= next_remote_poll);
        assert_eq!(reads, [0]);
    }
    let due = origin + interval;
    let mut due_reads = graph_reads_for_wake(&pending, &remote, GraphWake::Event);
    include_due_remote_reads(&mut due_reads, &pending, &remote, due >= next_remote_poll);
    assert_eq!(due_reads, pending);
    let next_remote_poll = due + interval;
    let mut next_reads = graph_reads_for_wake(&pending, &remote, GraphWake::Event);
    include_due_remote_reads(
        &mut next_reads,
        &pending,
        &remote,
        due + Duration::from_millis(99) >= next_remote_poll,
    );
    assert_eq!(next_reads, [0]);
    let mut second_due_reads = graph_reads_for_wake(&pending, &remote, GraphWake::Event);
    include_due_remote_reads(
        &mut second_due_reads,
        &pending,
        &remote,
        due + interval >= next_remote_poll,
    );
    assert_eq!(second_due_reads, pending);
}

#[test]
fn failed_reread_retains_the_last_snapshot_as_stale() {
    for failure_delivered in [false, true] {
        let rows = json!({"rows":[{"kind":"graph","state":"pending"}]});
        let mut latest = vec![Some(Reading {
            settle: Settle::Pending,
            line: "g pending".into(),
            data: rows.clone(),
        })];
        let (sender, receiver) = mpsc::channel();
        if failure_delivered {
            sender
                .send(Update {
                    target: 0,
                    reading: Reading::unavailable("g", "relay timed out"),
                })
                .unwrap();
        }
        drop(sender);
        let mut settled = Vec::new();
        assert!(!collect(
            &receiver,
            &mut latest,
            &mut settled,
            Instant::now(),
            WaitMode::All,
            &mut |_, _| {}
        ));
        let last = latest[0].as_ref().unwrap();
        assert_eq!(last.data["rows"], rows["rows"]);
        assert_eq!(last.data["stale"], true);
        assert_eq!(last.settle, Settle::Pending);
        assert!(settled.is_empty());
    }
}

#[test]
fn a_target_settling_after_the_last_sample_is_reported_settled_at_expiry() {
    // Expiry is modelled without a clock: the deadline has passed and the
    // workers are gone, leaving only the last paced sample, which saw the
    // graph pending. Its local store has settled since.
    let host = provenant_core::HostId::parse(&"a".repeat(32)).expect("fixture host");
    let graph = |state: &str, outcome: &str| {
        classify_rows(
            "fix",
            &[json!({"kind":"graph","state":state,"outcome":outcome})],
        )
    };
    let pull = Source::Pr {
        number: 7,
        repository: Ok("owner/repo".into()),
        subject: None,
        pinned_head_seen: false,
        policy: SubjectPolicy::Pinned,
        policy_source: super::test_policy_source(),
        complete: false,
    };
    for (with_pull, expected) in [(false, Outcome::Ok), (true, Outcome::Timeout)] {
        let mut sources = vec![Source::Graph("fix".parse().expect("target"))];
        if with_pull {
            sources.push(pull.clone());
        }
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Update {
                target: 0,
                reading: graph("pending", ""),
            })
            .expect("queue the last sample");
        drop(sender);
        let mut latest = vec![None; sources.len()];
        if with_pull {
            latest[1] = Some(reading(Settle::Pending));
        }
        let mut settled = Vec::new();
        let done = collect(
            &receiver,
            &mut latest,
            &mut settled,
            Instant::now(),
            WaitMode::All,
            &mut |_, _| {},
        );
        assert!(!done, "the blocking phase ends unsatisfied");
        let mut reads = Vec::new();
        let done = final_snapshot(
            &sources,
            &mut latest,
            &mut settled,
            WaitMode::All,
            Some(&host),
            |target| {
                reads.push(target.to_string());
                Some(graph("complete", "succeeded"))
            },
            &mut |_, _| {},
        );
        assert_eq!(reads, ["fix"], "only the local store is read after expiry");
        assert_eq!(
            latest[0].as_ref().map(|r| r.settle),
            Some(Settle::Succeeded)
        );
        assert_eq!(
            latest[0].as_ref().map(|r| r.data["stale"].is_null()),
            Some(true)
        );
        assert_eq!(outcome(&latest, &settled, done, WaitMode::All), expected);
    }
}

#[test]
fn bare_wait_reports_an_unreadable_graph_as_unsuccessful() -> Result<(), Box<dyn std::error::Error>>
{
    if crate::fabric::peer_tests::host_test(
        "status_wait::tests::bare_wait_reports_an_unreadable_graph_as_unsuccessful",
    )? {
        return Ok(());
    }

    use provenant_core::Clock;
    use provenant_fabric_runtime::graph::FileJournal;
    let rig = crate::worktree_tests::repository()?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let context = provenant_cli::fabric::Context::open(roots.clone(), &rig.home, &rig.project)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    drop(FileJournal::open(
        &roots.state,
        context.identity.project,
        "broken",
        provenant_core::WaitBudget::cli(clock.mono_now(), 30)?,
        &cancel,
        &clock,
    )?);
    std::fs::write(
        roots
            .state
            .as_path()
            .join("fabric/graphs")
            .join(context.identity.project.to_string())
            .join("graph-broken.json"),
        b"invalid checkpoint",
    )?;
    let edge = provenant_cli::run::Edge {
        roots: &roots,
        home: &rig.home,
        assets: &rig.project,
        cwd: &rig.project,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let discovered = crate::fabric::unsettled_graphs(&edge, deadline, &cancel)?;
    let graph = discovered
        .into_iter()
        .find(|graph| graph.target.name == "broken")
        .ok_or("broken graph was hidden")?;
    let reading = read(
        &Source::UnreadableGraph(graph.target, graph.error.ok_or("decode failure")?),
        &edge,
        &mut None,
        deadline,
        Duration::from_secs(1),
        &cancel,
        &provenant_confinement::worktrees::SystemWorkerObserver,
    );
    assert_eq!(reading.settle, Settle::Unsuccessful);
    assert!(reading.done());
    Ok(())
}

// File-stamp gates were removed with local polling; native event wake
// coverage lives beside the OS port, with paced fallback exercised by reads.

// Local-stamp ownership checks were removed with file polling. Native graph
// event subscription is covered by the OS port's multi-log invariant.

#[test]
fn preflight_expiry_and_cancellation_use_wait_exit_codes() {
    let cancel = CancelToken::new();
    let globals = Globals::default();
    let error = provenant_fabric_runtime::coordination::facade::FacadeError::Timeout;
    assert_eq!(
        preflight_error(&error, Instant::now(), &cancel, &globals),
        124
    );
    cancel.cancel();
    assert_eq!(
        preflight_error(
            &error,
            Instant::now() + Duration::from_secs(30),
            &cancel,
            &globals
        ),
        130
    );
}

#[test]
fn graph_wait_readers_share_the_callers_deadline_and_cancellation()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::coordination::facade::FacadeError;
    let rig = crate::worktree_tests::repository()?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let edge = provenant_cli::run::Edge {
        roots: &roots,
        home: &rig.home,
        assets: &rig.project,
        cwd: &rig.project,
    };
    let target = "missing".parse()?;
    for cancelled in [false, true] {
        let cancel = CancelToken::new();
        let deadline = if cancelled {
            cancel.cancel();
            Instant::now() + Duration::from_secs(30)
        } else {
            Instant::now()
        };
        for error in [
            crate::fabric::check_targets(
                std::slice::from_ref(&target),
                &edge,
                deadline,
                &cancel,
                |_, _| {},
            )
            .unwrap_err(),
            crate::fabric::unsettled_graphs(&edge, deadline, &cancel).unwrap_err(),
            crate::fabric::unified::read_status_rows(&target, &edge, &mut None, deadline, &cancel)
                .unwrap_err(),
        ] {
            assert!(matches!(
                (cancelled, error.downcast_ref::<FacadeError>()),
                (true, Some(FacadeError::Cancelled)) | (false, Some(FacadeError::Timeout))
            ));
        }
    }
    Ok(())
}

#[test]
fn final_line_states_every_settle_and_the_next_command() {
    use Settle::*;
    let sources = vec![
        Source::Graph("fixed".parse().expect("target")),
        Source::Graph("broken".parse().expect("target")),
        Source::Graph("input".parse().expect("target")),
        Source::Graph("slow".parse().expect("target")),
    ];
    let mixed = vec![
        Some(reading(Succeeded)),
        Some(reading(Unsuccessful)),
        Some(reading(NeedsInput)),
        Some(reading(Pending)),
    ];
    assert_eq!(
        final_human(&sources, &mixed, false, &[]),
        "next: fixed settled ok, broken failed, input needs input, slow still running at timeout; provenant wait slow\n"
    );
    let settled = vec![
        Some(reading(Succeeded)),
        Some(reading(Unsuccessful)),
        Some(reading(NeedsInput)),
        Some(reading(Succeeded)),
    ];
    assert_eq!(
        final_human(&sources, &settled, true, &[]),
        "next: fixed settled ok, broken failed, input needs input, slow settled ok; provenant status fixed broken input slow\n"
    );
    assert_eq!(
        final_human(&[], &[], true, &[]),
        "next: nothing to wait for; provenant status\n"
    );
    // A superseded target keeps its states; the next command names the newer handle.
    assert_eq!(
        final_human(&sources, &settled, true, &["slow-2".to_owned()]),
        "next: fixed settled ok, broken failed, input needs input, slow settled ok; provenant wait slow-2\n"
    );
}

/// The human last line is the edge rendering the typed step; this pins the
/// two views to the same `why` and `argv`.
fn final_human(
    sources: &[Source],
    latest: &[Option<Reading>],
    done: bool,
    superseding: &[String],
) -> String {
    provenant_cli::next_steps_text(
        &[final_step(sources, latest, done, superseding)],
        &provenant_workflow::session::CommandPrefix::default(),
    )
}

fn reading(settle: Settle) -> Reading {
    Reading {
        settle,
        line: format!("{settle:?}"),
        data: Value::Null,
    }
}

fn wait_on(updates: &[(usize, Settle)], count: usize, mode: WaitMode) -> (Outcome, Vec<usize>) {
    let (sender, receiver) = mpsc::channel();
    for (target, settle) in updates {
        assert!(
            sender
                .send(Update {
                    target: *target,
                    reading: reading(*settle),
                })
                .is_ok()
        );
    }
    let mut latest = vec![None; count];
    let mut settled = Vec::new();
    let done = collect(
        &receiver,
        &mut latest,
        &mut settled,
        Instant::now() + Duration::from_millis(20),
        mode,
        &mut |_, _| {},
    );
    (outcome(&latest, &settled, done, mode), settled)
}

#[test]
fn any_returns_at_the_first_settlement_and_takes_its_verdict() {
    use Settle::*;
    let (outcome, settled) = wait_on(
        &[
            (0, Pending),
            (1, Pending),
            (1, Unsuccessful),
            (0, Succeeded),
        ],
        2,
        WaitMode::Any,
    );
    assert_eq!(settled, [1], "the later settlement was never read");
    assert_eq!(outcome, Outcome::Failed);
    assert_eq!(
        wait_on(&[(0, Pending), (1, NeedsInput)], 2, WaitMode::Any).0,
        Outcome::Pending
    );
}

#[test]
fn all_waits_for_every_target_and_a_timeout_is_124() {
    use Settle::*;
    let (outcome, _) = wait_on(&[(0, Succeeded), (1, Pending)], 2, WaitMode::All);
    assert_eq!(provenant_cli::exit_status(&outcome), 124);
    let (outcome, _) = wait_on(
        &[(0, Succeeded), (1, Pending), (1, Succeeded)],
        2,
        WaitMode::All,
    );
    assert_eq!(provenant_cli::exit_status(&outcome), 0);
    let (outcome, _) = wait_on(&[(0, NeedsInput), (1, Unsuccessful)], 2, WaitMode::All);
    assert_eq!(provenant_cli::exit_status(&outcome), 1);
    let (outcome, settled) = wait_on(
        &[(1, Unsuccessful), (0, Pending), (0, Succeeded)],
        2,
        WaitMode::All,
    );
    assert_eq!(
        settled,
        [1, 0],
        "all mode reads the remaining target after failure"
    );
    assert_eq!(outcome, Outcome::Failed);
    assert_eq!(
        provenant_cli::exit_status(&wait_on(&[], 1, WaitMode::Any).0),
        124
    );
}

#[test]
fn fail_fast_waits_for_successes_but_returns_on_first_failure() {
    use Settle::*;
    let (outcome, settled) = wait_on(
        &[
            (0, Pending),
            (1, Pending),
            (1, Unsuccessful),
            (0, Succeeded),
        ],
        2,
        WaitMode::FailFast,
    );
    assert_eq!(settled, [1]);
    assert_eq!(outcome, Outcome::Failed);
    let (outcome, settled) = wait_on(
        &[(0, Pending), (1, Pending), (1, Succeeded), (0, Succeeded)],
        2,
        WaitMode::FailFast,
    );
    assert_eq!(settled, [1, 0]);
    assert_eq!(outcome, Outcome::Ok);
}

#[test]
fn a_failed_child_is_never_reported_as_a_successful_wait() {
    use serde_json::json;
    // The relay's terminal child rows are the wait's targets. A `failed`
    // or otherwise unsuccessful settled node must make the wait fail, not
    // silently return 0 with no result.
    let child =
        |node: &str, state: &str| json!({"node":node,"state":state,"settled":state != "running"});
    assert_eq!(
        super::child_settle(&child("a", "succeeded")),
        Settle::Succeeded
    );
    for state in ["failed", "blocked", "skipped"] {
        assert_eq!(
            super::child_settle(&child("a", state)),
            Settle::Unsuccessful,
            "state={state}"
        );
    }
    assert_eq!(super::child_settle(&child("a", "running")), Settle::Pending);
}

#[test]
fn a_child_wait_takes_its_verdict_from_the_selected_mode() {
    let child =
        |node: &str, state: &str| json!({"node":node,"state":state,"settled":state != "running"});
    let ok = child("a", "succeeded");
    let failed = child("b", "failed");
    let running = child("c", "running");
    let children = [ok.clone(), failed.clone(), running.clone()];

    // Under `all`, any failed child fails the wait.
    assert_eq!(
        super::child_wait_outcome(&children, WaitMode::All, &[0, 1, 2]),
        Outcome::Failed
    );
    // Under `any`, only the first settled child decides the verdict, even
    // when a later child in the same snapshot failed.
    assert_eq!(
        super::child_wait_outcome(&children, WaitMode::Any, &[0]),
        Outcome::Ok
    );
    assert_eq!(
        super::child_wait_outcome(&children, WaitMode::Any, &[1]),
        Outcome::Failed
    );
    // Under `fail-fast`, the first failed child decides the verdict.
    assert_eq!(
        super::child_wait_outcome(&children, WaitMode::FailFast, &[1]),
        Outcome::Failed
    );
    let all_ok = [child("a", "succeeded"), child("b", "succeeded")];
    assert_eq!(
        super::child_wait_outcome(&all_ok, WaitMode::FailFast, &[0, 1]),
        Outcome::Ok
    );
    // A child still running when the wait ends early is not a failure.
    assert_eq!(
        super::child_wait_outcome(&[running], WaitMode::All, &[]),
        Outcome::Ok
    );
}

#[test]
fn selected_mode_is_named_in_human_and_terminal_json_output() {
    for (mode, label) in [
        (WaitMode::All, "all"),
        (WaitMode::Any, "any"),
        (WaitMode::FailFast, "fail-fast"),
    ] {
        assert_eq!(mode_heading(mode), format!("wait mode: {label}\n"));
        let data = terminal_data(mode, Vec::new(), false, None);
        assert_eq!(data["mode"], label);
        assert_eq!(data["timed_out"], false);
        assert!(data["local_observation"].is_null());
    }
}

#[test]
fn wait_readings_bound_the_whole_event_line_not_just_the_target() {
    use provenant_cli::limits::SUMMARY_ROW_BYTES;
    use provenant_core::ProjectId;
    use provenant_records::WorkKey;

    let long_reason = format!("worktree_missing: {}", "x".repeat(SUMMARY_ROW_BYTES * 2));
    let key = json!(WorkKey::new(ProjectId::from_bytes([9; 16]), "g", "node", 1).unwrap());
    let reading = classify_rows(
        "g",
        &[
            json!({"kind":"graph","state":"failed","reason":long_reason}),
            json!({"kind":"node","work_key":key,"state":"failed","reason":long_reason}),
        ],
    );
    assert!(
        reading.line.len() <= SUMMARY_ROW_BYTES,
        "wait target line is unbounded: {}",
        reading.line
    );
    assert!(
        reading.line.ends_with("..."),
        "no clip marker on the wait target line: {}",
        reading.line
    );
    let digests = reading.data["digests"].as_array().expect("digests");
    assert_eq!(digests.len(), 1);
    for digest in digests {
        let line = digest["line"].as_str().expect("digest line");
        assert!(
            line.len() <= SUMMARY_ROW_BYTES,
            "wait digest line is unbounded: {line}"
        );
        assert!(
            line.ends_with("..."),
            "no clip marker on a digest line: {line}"
        );
    }
}

#[test]
fn a_target_settles_by_its_own_row() {
    let graph =
        |state: &str, outcome: &str| json!({"kind":"graph","state":state,"outcome":outcome});
    let node = |state: &str| json!({"kind":"node","state":state});
    let attempt =
        |state: &str, outcome: Value| json!({"kind":"attempt","state":state,"outcome":outcome});
    let settle = |rows: &[Value]| classify_rows("fix", rows).settle;
    assert_eq!(
        settle(&[graph("complete", "succeeded"), node("succeeded")]),
        Settle::Succeeded
    );
    assert_eq!(
        settle(&[graph("complete", "partial")]),
        Settle::Unsuccessful
    );
    assert_eq!(
        settle(&[graph("paused", ""), node("pending")]),
        Settle::NeedsInput
    );
    // A settled older attempt never decides a node that is running again.
    assert_eq!(
        settle(&[node("running"), attempt("terminal", json!("failed"))]),
        Settle::Pending
    );
    assert_eq!(
        settle(&[attempt("terminal", json!("succeeded"))]),
        Settle::Succeeded
    );
    assert_eq!(settle(&[attempt("running", Value::Null)]), Settle::Pending);
}

#[test]
// Spec: docs/specs/cli.md#waits-are-readers
fn a_silent_attempt_ends_the_wait_without_claiming_settlement() {
    let rows = [
        json!({"kind":"graph","state":"running"}),
        json!({
            "kind":"attempt","state":"running","presence":"live","stalled":true,
            "last_event_at":"2026-11-26T04:58:00Z"
        }),
    ];
    let reading = classify_rows("quiet", &rows);
    assert_eq!(reading.settle, Settle::Stalled);
    assert!(reading.done(), "a stall must end the wait");
    assert!(
        reading.line.contains("2026-11-26T04:58:00Z"),
        "stalled line names its last-event time: {}",
        reading.line
    );
    assert_eq!(
        outcome(&[Some(reading)], &[0], true, WaitMode::All),
        Outcome::Stalled
    );
    // A settled graph still decides: a stale stall cannot win.
    let settled = classify_rows(
        "quiet",
        &[
            json!({"kind":"graph","state":"complete","outcome":"succeeded"}),
            json!({"kind":"attempt","state":"running","stalled":true}),
        ],
    );
    assert_eq!(settled.settle, Settle::Succeeded);
}

#[test]
fn pr_journal_obeys_the_selected_completion_policy() {
    let status = json!({
        "number": 7, "state": "OPEN", "head": "head", "draft": false,
        "merge": "clean", "review": "unknown", "error": null,
        "checks_passed": 0, "checks_total": 2,
        "checks_failed": 1, "checks_pending": 1
    });
    let event = json!({"status": status});
    assert_eq!(
        event_reading(7, false, event.clone()).settle,
        Settle::Unsuccessful
    );
    assert_eq!(event_reading(7, true, event).settle, Settle::Pending);
}

fn settled_pr_reading(head: &str, passed: bool, workflows: Option<bool>) -> Reading {
    let mut data = json!({
        "number": 7, "state": "OPEN", "head": head, "draft": false,
        "merge": "clean", "review": "unknown", "error": null,
        "checks_passed": u32::from(passed), "checks_total": 1,
        "checks_failed": u32::from(!passed), "checks_pending": 0
    });
    if let Some(active) = workflows {
        data["active_workflows"] = json!(active);
    }
    Reading {
        settle: if passed {
            Settle::Succeeded
        } else {
            Settle::Unsuccessful
        },
        line: "#7 settled".into(),
        data,
    }
}

#[test]
fn a_new_head_stays_pending_until_actions_runs_exist_or_the_grace_ends() {
    let mut grace = ActionsGrace::new(Duration::from_secs(3600));
    let held = grace.apply(settled_pr_reading("a", true, None), "#7", false);
    assert_eq!(held.settle, Settle::Pending);
    assert!(held.line.starts_with("#7 pending"), "{}", held.line);
    assert!(held.line.contains("head=a"), "{}", held.line);

    // Runs for the head end the hold.
    for active in [true, false] {
        let pinned_head_seen =
            grace.apply(settled_pr_reading("a", true, Some(active)), "#7", false);
        assert_eq!(pinned_head_seen.settle, Settle::Succeeded);
    }

    // A default wait still settles on a failure at once; a complete one holds.
    let failed = settled_pr_reading("a", false, None);
    assert_eq!(
        grace.apply(failed.clone(), "#7", false).settle,
        Settle::Unsuccessful
    );
    assert_eq!(grace.apply(failed, "#7", true).settle, Settle::Pending);

    // An unreadable reading carries no line to hold.
    let unreadable = Reading::unavailable("#7", "boom");
    assert_eq!(grace.apply(unreadable.clone(), "#7", true), unreadable);
}

#[test]
fn the_grace_period_is_bounded_and_restarts_for_a_new_head() {
    let mut ended = ActionsGrace::new(Duration::ZERO);
    assert_eq!(
        ended
            .apply(settled_pr_reading("a", true, None), "#7", false)
            .settle,
        Settle::Succeeded,
        "a zero grace period never holds"
    );

    let mut grace = ActionsGrace::new(Duration::from_secs(3600));
    assert_eq!(
        grace
            .apply(settled_pr_reading("a", true, None), "#7", false)
            .settle,
        Settle::Pending
    );
    assert!(!grace.is_due());
    let left = grace.due_in().expect("a held reading has a deadline");
    assert!(left > Duration::from_secs(3000), "{left:?}");
    // Age the head: the period has run out, so the reading settles.
    let (_, first_read) = grace.head.as_mut().unwrap();
    *first_read = Instant::now()
        .checked_sub(Duration::from_secs(7200))
        .expect("monotonic clock far enough from its origin");
    assert_eq!(
        grace
            .apply(settled_pr_reading("a", true, None), "#7", false)
            .settle,
        Settle::Succeeded
    );
    assert_eq!(grace.due_in(), None, "a settled reading is not held");
    // A later push is a new head with its own period.
    assert_eq!(
        grace
            .apply(settled_pr_reading("b", true, None), "#7", false)
            .settle,
        Settle::Pending
    );
}

/// One observation of PR #7 in owner/repo at the epoch.
fn test_sample(policy: SubjectPolicy) -> PrSample<'static> {
    static FIXTURE_REPOSITORY: std::sync::LazyLock<Result<String, String>> =
        std::sync::LazyLock::new(|| Ok("owner/repo".into()));
    static FIXTURE_POLICY_SOURCE: std::sync::LazyLock<provenant_records::Source> =
        std::sync::LazyLock::new(super::test_policy_source);
    PrSample {
        number: 7,
        repository: &FIXTURE_REPOSITORY,
        policy,
        policy_source: &FIXTURE_POLICY_SOURCE,
        now: WallTime::UNIX_EPOCH,
    }
}

fn pinned_subject(head: GitOid, policy: SubjectPolicy) -> Box<PinnedSubject> {
    Box::new(PinnedSubject {
        extra: Default::default(),
        base: None,
        head,
        origin: SubjectOrigin::ForgePr {
            repository: "owner/repo".into(),
            number: 7,
        },
        pinned_at: WallTime::from_micros(0).expect("fixture time"),
        policy,
        repository: Some("owner/repo".into()),
        base_ref: None,
        policy_source: Some(super::test_policy_source()),
        followed_from: Vec::new(),
    })
}

#[test]
fn a_pinned_wait_ignores_the_verdict_of_another_head() {
    let pinned = oid(&"a".repeat(40));
    let other = oid(&"b".repeat(40));
    let mut subject = Some(pinned_subject(pinned, SubjectPolicy::Pinned));
    let mut pinned_head_seen = false;
    let mut line = pr_line(other, true);
    assert_eq!(line.verdict(), Some(true), "the other head passes");
    let moved = apply_subject_sample(
        &mut subject,
        &mut pinned_head_seen,
        &mut None,
        &mut line,
        &test_sample(SubjectPolicy::Pinned),
    );
    assert!(!moved, "an unseen head is not a move");
    assert_eq!(line.verdict(), None, "its verdict is ignored");
    assert_eq!(pinned_head(&subject, &line), pinned.to_string());
    assert_eq!(line.head, other.to_string());
}

#[test]
fn a_pinned_wait_settles_pr_head_moved_when_the_pr_moves() {
    let first = oid(&"a".repeat(40));
    let second = oid(&"b".repeat(40));
    let mut subject = Some(pinned_subject(first, SubjectPolicy::Pinned));
    let mut pinned_head_seen = false;
    let mut first_sample = pr_line(first, true);
    assert!(!apply_subject_sample(
        &mut subject,
        &mut pinned_head_seen,
        &mut None,
        &mut first_sample,
        &test_sample(SubjectPolicy::Pinned),
    ));
    assert!(pinned_head_seen, "the pinned head was observed");

    let mut moved_line = pr_line(second, true);
    let (moved, warnings) = warning::capture(|| {
        apply_subject_sample(
            &mut subject,
            &mut pinned_head_seen,
            &mut None,
            &mut moved_line,
            &test_sample(SubjectPolicy::Pinned),
        )
    });
    assert!(moved, "a pinned_head_seen head that moves settles the wait");
    assert_eq!(
        moved_line.cause.as_deref(),
        Some(crate::pr::status::PR_HEAD_MOVED)
    );
    assert!(
        moved_line
            .next
            .as_deref()
            .is_some_and(|next| next.contains("domains.wait.pr_head_policy=follow_latest")),
        "{:?}",
        moved_line.next
    );
    assert!(
        warnings.is_empty(),
        "a pinned wait does not follow: {warnings:?}"
    );
    let reading = pr_reading("#7", &subject, &None, moved_line, false, moved);
    assert_eq!(reading.settle, Settle::Unsuccessful);
    assert!(
        reading.line.contains("cause=pr_head_moved"),
        "{}",
        reading.line
    );
}

#[test]
fn a_follow_latest_wait_repins_and_warns() {
    let first = oid(&"a".repeat(40));
    let second = oid(&"b".repeat(40));
    let mut subject = Some(pinned_subject(first, SubjectPolicy::FollowLatest));
    let mut pinned_head_seen = true;
    let mut head_check_seen = None;
    let mut line = pr_line(second, true);
    let (moved, warnings) = warning::capture(|| {
        apply_subject_sample(
            &mut subject,
            &mut pinned_head_seen,
            &mut head_check_seen,
            &mut line,
            &test_sample(SubjectPolicy::FollowLatest),
        )
    });
    assert!(!moved, "follow_latest never settles pr_head_moved");
    let subject = subject.expect("subject stays pinned");
    assert_eq!(subject.head, second, "the new head is pinned");
    assert_eq!(
        subject.followed_from,
        vec![first],
        "the earlier pin is recorded in order"
    );
    assert_eq!(
        head_check_seen,
        Some(HeadCheck::Moved {
            pinned: first,
            current: second
        }),
        "the reading records the move it followed"
    );
    let reading = pr_reading(
        "#7",
        &Some(subject.clone()),
        &head_check_seen,
        line.clone(),
        false,
        false,
    );
    assert_eq!(
        reading.data["head_check"],
        json!({"state": "moved", "pinned": first.to_string(), "current": second.to_string()}),
        "the wait reading publishes the head check"
    );
    assert!(
        !pinned_head_seen,
        "the new head must be observed before it can move"
    );
    assert_eq!(line.cause, None);
    assert!(
        warnings.iter().any(|warning| {
            warning.code.as_ref() == warning::PR_HEAD_FOLLOWED.as_str()
                && warning.message.contains(&first.to_string())
                && warning.message.contains(&second.to_string())
        }),
        "both heads are named in the warning: {warnings:?}"
    );
}

#[test]
fn a_wait_pins_the_forge_head_at_admission() {
    let now = WallTime::from_micros(0).expect("fixture time");
    let forge = "c".repeat(40);
    let subject = pr_subject(
        7,
        &Ok("owner/repo".into()),
        SubjectPolicy::Pinned,
        PrHeadSource::Auto,
        &super::test_policy_source(),
        now,
        None,
        Some((forge.clone(), "feature".into())),
        None,
    )
    .expect("forge head pins a subject");
    assert_eq!(subject.head, oid(&forge));
    assert_eq!(
        subject.origin,
        SubjectOrigin::ForgePr {
            repository: "owner/repo".into(),
            number: 7
        }
    );
    assert_eq!(subject.policy, SubjectPolicy::Pinned);
    assert_eq!(subject.repository.as_deref(), Some("owner/repo"));
    assert!(subject.policy_source.is_some());

    let explicit = "d".repeat(40);
    let launch_source = provenant_records::Source::at(provenant_records::Layer::Launch, "--head");
    let forced = pr_subject(
        7,
        &Ok("owner/repo".into()),
        SubjectPolicy::FollowLatest,
        PrHeadSource::Auto,
        &launch_source,
        now,
        Some(&explicit),
        Some((forge.clone(), "feature".into())),
        None,
    )
    .expect("--head pins a subject");
    assert_eq!(forced.head, oid(&explicit));
    assert_eq!(forced.origin, SubjectOrigin::Explicit);
    assert_eq!(forced.policy, SubjectPolicy::Pinned, "--head always pins");
    assert_eq!(forced.policy_source, Some(launch_source));

    let local = "e".repeat(40);
    let local_subject = pr_subject(
        7,
        &Ok("owner/repo".into()),
        SubjectPolicy::Pinned,
        PrHeadSource::Auto,
        &super::test_policy_source(),
        now,
        None,
        Some((forge, "feature".into())),
        Some(local.clone()),
    )
    .expect("local tip pins a subject");
    assert_eq!(local_subject.head, oid(&local));
    assert_eq!(
        local_subject.origin,
        SubjectOrigin::LocalRef {
            name: "feature".into()
        }
    );
}

#[test]
fn a_wait_without_a_forge_head_pins_its_first_sample() {
    let head = oid(&"f".repeat(40));
    let mut subject = None;
    let mut pinned_head_seen = false;
    let mut line = pr_line(head, true);
    let moved = apply_subject_sample(
        &mut subject,
        &mut pinned_head_seen,
        &mut None,
        &mut line,
        &test_sample(SubjectPolicy::FollowLatest),
    );
    assert!(!moved);
    let subject = subject.expect("the first sample pins the subject");
    assert_eq!(subject.head, head);
    assert_eq!(subject.policy, SubjectPolicy::FollowLatest);
    assert_eq!(
        subject.origin,
        SubjectOrigin::ForgePr {
            repository: "owner/repo".into(),
            number: 7
        }
    );
    assert!(pinned_head_seen);
    let reading = pr_reading("#7", &Some(subject.clone()), &None, line, false, moved);
    assert_eq!(reading.data["subject"], json!(subject));
    assert_eq!(reading.settle, Settle::Succeeded);
}

#[test]
fn a_forge_head_source_ignores_a_differing_local_tip() {
    let now = WallTime::from_micros(0).expect("fixture time");
    let forge = "c".repeat(40);
    let local = "e".repeat(40);
    let subject = pr_subject(
        7,
        &Ok("owner/repo".into()),
        SubjectPolicy::Pinned,
        PrHeadSource::Forge,
        &super::test_policy_source(),
        now,
        None,
        Some((forge.clone(), "feature".into())),
        Some(local.clone()),
    )
    .expect("the forge head pins a subject");
    assert_eq!(
        subject.head,
        oid(&forge),
        "the differing local tip is ignored"
    );
    assert_eq!(
        subject.origin,
        SubjectOrigin::ForgePr {
            repository: "owner/repo".into(),
            number: 7
        }
    );
}

#[test]
fn a_local_head_source_without_a_branch_warns_and_uses_the_forge() {
    let now = WallTime::from_micros(0).expect("fixture time");
    let forge = "c".repeat(40);
    let source = "domains.wait.projects.demo.pr_head_source from config.toml";
    let (subject, warnings) = warning::capture(|| {
        warn_local_head_unavailable(7, source);
        pr_subject(
            7,
            &Ok("owner/repo".into()),
            SubjectPolicy::Pinned,
            PrHeadSource::Local,
            &super::test_policy_source(),
            now,
            None,
            Some((forge.clone(), "feature".into())),
            None,
        )
    });
    let subject = subject.expect("the forge head pins a subject");
    assert_eq!(
        subject.head,
        oid(&forge),
        "no local tip falls back to the forge"
    );
    assert_eq!(
        subject.origin,
        SubjectOrigin::ForgePr {
            repository: "owner/repo".into(),
            number: 7
        }
    );
    assert!(
        warnings.iter().any(|warning| {
            warning.code.as_ref() == warning::PR_HEAD_LOCAL_UNAVAILABLE.as_str()
                && warning.message.contains(source)
        }),
        "the warning names the source of the local choice: {warnings:?}"
    );
}

#[test]
fn a_refused_wait_settles_head_unpinned() {
    let source = "domains.wait.projects.demo.pr_head_unpinned from config.toml";
    let reading = unpinned_refusal(7, source);
    assert_eq!(reading.settle, Settle::Unsuccessful);
    assert_eq!(
        reading.data["cause"],
        json!(crate::pr::status::HEAD_UNPINNED)
    );
    assert!(reading.line.contains("#7"));
    assert!(
        reading.data["next"]
            .as_str()
            .is_some_and(|next| next.contains(source)),
        "the next step names the supplying key: {}",
        reading.data["next"]
    );
}

#[test]
fn pr_timeout_reports_the_last_known_check_summary_and_next_step() {
    let sources = vec![Source::Pr {
        number: 7,
        repository: Ok("owner/repo".into()),
        subject: None,
        pinned_head_seen: false,
        policy: SubjectPolicy::Pinned,
        policy_source: super::test_policy_source(),
        complete: false,
    }];
    let pending = Reading {
        settle: Settle::Pending,
        line: "#7 pending".into(),
        data: json!({
            "number": 7, "state": "OPEN", "head": "abc", "draft": false,
            "merge": "clean", "review": "unknown", "error": null,
            "checks_passed": null, "checks_total": null,
            "checks_failed": null, "checks_pending": null
        }),
    };
    let settled = Reading {
        settle: Settle::Succeeded,
        line: "#7 ready".into(),
        data: json!({
            "number": 7, "state": "OPEN", "head": "abc", "draft": false,
            "merge": "clean", "review": "approved", "error": null,
            "checks_passed": 1, "checks_total": 1,
            "checks_failed": 0, "checks_pending": 0
        }),
    };
    let details = pr_timeout_details(&sources, &[Some(pending), Some(settled)]);
    assert_eq!(details.len(), 1, "only an unsettled PR gets a detail");
    assert!(details[0].contains("#7 OPEN abc"), "{details:?}");
    assert!(details[0].contains("checks=unavailable"), "{details:?}");
    assert!(
        details[0].contains("cause=checks_not_scheduled"),
        "{details:?}"
    );
    assert!(
        details[0].contains(crate::pr::status::NO_CHECKS_NEXT),
        "{details:?}"
    );
}

#[test]
fn a_timed_out_complete_wait_keeps_its_last_known_line() {
    // A failure with checks still running settles a default wait but keeps a
    // complete wait pending, so only the complete wait has a last known line.
    let reading = Reading {
        settle: Settle::Pending,
        line: "#7 failing".into(),
        data: json!({
            "number": 7, "state": "OPEN", "head": "abc", "draft": false,
            "merge": "clean", "review": "unknown", "error": null,
            "checks_passed": 0, "checks_total": 2,
            "checks_failed": 1, "checks_pending": 1,
            "failing_checks": ["lint"], "pending_checks": ["build"]
        }),
    };
    let source = |complete| {
        vec![Source::Pr {
            number: 7,
            repository: Ok("owner/repo".into()),
            subject: None,
            pinned_head_seen: false,
            policy: SubjectPolicy::Pinned,
            policy_source: super::test_policy_source(),
            complete,
        }]
    };
    let complete = pr_timeout_details(&source(true), &[Some(reading.clone())]);
    assert_eq!(complete.len(), 1, "{complete:?}");
    assert!(complete[0].contains("fail=1"), "{complete:?}");
    assert!(complete[0].contains("pending=1"), "{complete:?}");
    assert!(complete[0].contains("pending_names=build"), "{complete:?}");
    assert!(pr_timeout_details(&source(false), &[Some(reading)]).is_empty());
}
