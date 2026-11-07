use super::*;

#[test]
fn read_only_lane_is_admitted_inside_a_writers_spacing_window() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    let writer = starting(&store, &lane(20, true), &call)?;
    let reader = starting(&store, &lane(21, false), &call)?;
    assert!(store.claim(writer, &at(10_000), &call)?.is_some());
    assert!(
        store.claim(reader, &at(11_000), &call)?.is_some(),
        "a writer that started 1 s ago does not space a read-only lane"
    );
    Ok(())
}
#[test]
fn writers_stay_20_seconds_apart_and_read_only_starts_do_not_reset_the_clock() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    let first = starting(&store, &lane(30, true), &call)?;
    let second = starting(&store, &lane(31, true), &call)?;
    let third = starting(&store, &lane(32, true), &call)?;
    let reader = starting(&store, &lane(33, false), &call)?;
    assert!(store.claim(first, &at(0), &call)?.is_some());
    assert!(
        store.claim(second, &at(19_999), &call)?.is_none(),
        "20 s between writers"
    );
    assert!(store.claim(reader, &at(20_000), &call)?.is_some());
    assert!(
        store.claim(second, &at(20_001), &call)?.is_some(),
        "measured from the last writer, not the read-only start at 20 s"
    );
    assert!(store.claim(third, &at(40_000), &call)?.is_none());
    assert!(store.claim(third, &at(40_001), &call)?.is_some());
    Ok(())
}
#[test]
fn read_only_lanes_are_spaced_2_seconds_apart() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    let first = starting(&store, &lane(40, false), &call)?;
    let second = starting(&store, &lane(41, false), &call)?;
    let third = starting(&store, &lane(42, false), &call)?;
    assert!(store.claim(first, &at(0), &call)?.is_some());
    assert!(
        store.claim(second, &at(1_999), &call)?.is_none(),
        "a burst of provider launches is still staggered"
    );
    assert!(store.claim(second, &at(2_000), &call)?.is_some());
    assert!(store.claim(third, &at(3_999), &call)?.is_none());
    assert!(store.claim(third, &at(4_000), &call)?.is_some());
    Ok(())
}
#[test]
fn the_store_accepts_run_concurrency_above_the_task_count_up_to_sixteen() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut run = lane(90, false);
    run.request.concurrency = 16;
    store.create(&run, &call)?;
    assert_eq!(store.request(run.run_id, &call)?.concurrency, 16);
    let mut wider = lane(91, false);
    wider.request.concurrency = 17;
    assert!(matches!(
        store.create(&wider, &call),
        Err(RunError::Validation(_))
    ));
    Ok(())
}
#[test]
fn a_nested_lane_reads_class_spacing_from_the_host_ledger_only_with_the_host_root() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let host = context(temp.path())?;
    // A nested lane's store is a child directory with no ledger of its own.
    let mut inputs =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
    inputs.state_dir = Some(host.state_root.as_path().join("children").join("lane"));
    let nested = ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        ..host.clone()
    };
    let mut writable =
        RunStore::open(&nested, &call)?.with_writer_registry(host.state_root.as_path());
    writable.set_admission_policy(AdmissionPolicy {
        read_only_spacing_ms: 600_000,
        ..AdmissionPolicy::default()
    });
    let (first, second) = (lane(110, false), lane(111, false));
    let first_id = starting(&writable, &first, &call)?;
    let second_id = starting(&writable, &second, &call)?;
    let attempt = writable.attempt_dir(first.run_id, first_id, false)?;
    writable.admit(first_id, &attempt, &HostFacts::new(100), &clock, &cancel)?;
    let waiting = writable.record(second_id, &call)?;
    let waits = |registry: Option<&std::path::Path>| -> Result<String> {
        let reader = RunStore::open_read_only(&nested, &call)?.ok_or(RunError::NotFound)?;
        let reader = match registry {
            Some(host) => reader.with_writer_registry(host),
            None => reader,
        };
        let waits = reader.lane_waits(std::slice::from_ref(&waiting), &call);
        Ok(waits
            .first()
            .map(|w| w.waiting_for.clone())
            .unwrap_or_default())
    };
    assert!(
        waits(Some(host.state_root.as_path()))?.starts_with("read-only startup spacing: "),
        "the host root shows the class's spacing wait"
    );
    assert!(
        waits(None)?.starts_with("launch preparation"),
        "the child directory has no ledger, so the wait goes unseen"
    );
    Ok(())
}
#[test]
fn a_wall_clock_step_back_then_forward_cannot_admit_two_writers_within_a_spacing() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    let [first, second] = writers(&store, [80, 81], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());

    // The wall clock steps back an hour and a claim sees it, then it is
    // corrected forward. Only 5 s of real time have passed since the start.
    host.step_wall_ms(-3_600_000);
    host.pass_ms(5_000);
    assert!(store.claim(second, &host, &call)?.is_none());
    host.step_wall_ms(3_600_000);
    assert!(
        store.claim(second, &host, &call)?.is_none(),
        "5 s after a writer started, whatever the wall clock read between"
    );

    // The hold is one spacing from the start, not from the claim that saw the
    // step: 20 s of real time after the start the writer is admitted, no later.
    host.pass_micros(14_999_999);
    assert!(store.claim(second, &host, &call)?.is_none());
    host.pass_micros(1);
    assert!(store.claim(second, &host, &call)?.is_some());
    Ok(())
}
#[test]
fn a_forward_wall_clock_step_alone_does_not_free_the_class() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    let [first, second] = writers(&store, [82, 83], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());
    host.step_wall_ms(86_400_000);
    assert!(
        store.claim(second, &host, &call)?.is_none(),
        "a day on the wall clock is no time on the host clock"
    );
    host.pass_micros(19_999_999);
    assert!(store.claim(second, &host, &call)?.is_none());
    host.pass_micros(1);
    assert!(store.claim(second, &host, &call)?.is_some());
    Ok(())
}
#[test]
fn read_only_spacing_holds_on_the_host_clock_and_never_holds_a_writer() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    let first = starting(&store, &lane(85, false), &call)?;
    let second = starting(&store, &lane(86, false), &call)?;
    let [writer] = writers(&store, [87], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());
    host.step_wall_ms(-60_000);
    host.pass_micros(1_999_999);
    assert!(store.claim(second, &host, &call)?.is_none());
    assert!(
        store.claim(writer, &host, &call)?.is_some(),
        "the read-only start holds no writer"
    );
    host.pass_micros(1);
    assert!(store.claim(second, &host, &call)?.is_some());
    Ok(())
}
#[test]
fn a_new_boot_frees_the_class_and_spacing_runs_on_the_new_boot() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(500_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    let [first, second, third] = writers(&store, [88, 89, 90], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());
    // The host restarted. Its clock is behind the recorded start, which is
    // another boot's reading and says nothing about this one.
    host.reboot("boot-b", 1_000);
    assert!(
        store.claim(second, &host, &call)?.is_some(),
        "a start from an earlier boot does not hold the class"
    );
    host.pass_ms(5_000);
    assert!(
        store.claim(third, &host, &call)?.is_none(),
        "the new boot's start is the one that spaces"
    );
    host.pass_ms(15_000);
    assert!(store.claim(third, &host, &call)?.is_some());
    Ok(())
}
#[test]
fn an_unreadable_host_clock_or_boot_identity_refuses_the_claim() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let [lane_id] = writers(&store, [91], &call)?;
    let attempt = store.attempt_dir(lane(91, true).run_id, lane_id, false)?;
    for (readable, name) in [
        (&host.host_readable, "monotonic clock"),
        (&host.boot_readable, "boot identity"),
    ] {
        *readable.lock().unwrap() = false;
        assert!(
            matches!(
                store.claim(lane_id, &host, &call),
                Err(RunError::Admission(AdmissionError::Clock))
            ),
            "an unreadable {name} refuses, with no wall-clock fallback"
        );
        let admitted = store.admit(lane_id, &attempt, &HostFacts::new(100), &host, &cancel);
        let refused = admitted.err().ok_or("admit must refuse")?;
        assert_eq!(refused.code(), "HOST_CLOCK", "{name}: {refused}");
        assert_eq!(
            store.record(lane_id, &call)?.state,
            AttemptState::Starting {
                extra: Default::default()
            },
            "nothing started"
        );
        *readable.lock().unwrap() = true;
    }
    assert!(
        store.claim(lane_id, &host, &call)?.is_some(),
        "no lease was taken while the clock was unreadable"
    );
    Ok(())
}
#[test]
fn lane_waits_count_spacing_on_the_host_clock_and_say_nothing_when_it_is_unreadable() -> TestResult
{
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    let [first, second] = writers(&store, [92, 93], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());
    let wait = |host: &Stepped| -> Result<String> {
        let records = [store.record(second, &call)?];
        // Isolate the host-clock contract from the runner's disk and pressure.
        let waits = store.lane_waits_with_resources(
            &records,
            &stepped_call(host, &cancel),
            &HostFacts::new(100),
        );
        Ok(waits
            .first()
            .map(|w| w.waiting_for.clone())
            .unwrap_or_default())
    };
    host.pass_ms(5_000);
    host.step_wall_ms(86_400_000);
    assert_eq!(
        wait(&host)?,
        "writer startup spacing: 15.0 s left of 20.0 s",
        "the wall clock stepped a day and is not what the wait counts"
    );
    *host.host_readable.lock().unwrap() = false;
    assert!(
        wait(&host)?.starts_with("launch preparation"),
        "no wait is stated when the host clock cannot be read"
    );
    Ok(())
}
#[test]
fn a_start_ahead_of_the_host_clock_in_the_same_boot_never_frees_the_class() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    let [first, second] = writers(&store, [94, 95], &call)?;
    assert!(store.claim(first, &host, &call)?.is_some());
    let ledger = crate::ledger::open(&store.writer_registry, &call)?;
    ledger.execute(
        "UPDATE spacing SET last_start=last_start+3600000000 WHERE class='writer'",
        [],
    )?;
    host.pass_ms(60_000);
    assert!(
        store.claim(second, &host, &call)?.is_none(),
        "a recorded start the clock has not reached holds, never frees"
    );
    Ok(())
}
#[test]
fn a_delayed_admission_cannot_start_inside_a_newer_start_or_rewind_it() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (Stepped::up(0), CancelToken::new());
    let call = stepped_call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    // Keep the writer slot limit from masking the startup-spacing behaviour.
    std::fs::write(
        store.writer_registry.join("config.toml"),
        "schema_version=1\n[domains.fabric.lanes]\nmax_concurrent_builds = 2\n",
    )?;
    store.set_admission_policy(AdmissionPolicy {
        admission_seconds: 1,
        startup_spacing_ms: 20_000,
        ..AdmissionPolicy::default()
    });
    let (delayed, rival) = (lane(70, true), lane(71, true));
    let delayed_id = starting(&store, &delayed, &call)?;
    let rival_id = starting(&store, &rival, &call)?;
    let attempt = store.attempt_dir(delayed.run_id, delayed_id, false)?;
    // `admit` samples the time, then the probe stalls while the rival's
    // writer starts one second later. The delayed lane must see that start.
    let probe = RivalStarts {
        store: &store,
        rival: rival_id,
        clock: &clock,
        call: &call,
        started: Cell::new(false),
    };
    let admission = store.admit(delayed_id, &attempt, &probe, &clock, &cancel);
    assert!(
        matches!(&admission, Err(RunError::Refused)),
        "admission: {admission:?}"
    );
    assert!(probe.started.get());
    assert_eq!(
        store.record(delayed_id, &call)?.state,
        AttemptState::Starting {
            extra: Default::default()
        }
    );
    // The rival's start, at 1 s, is still the recorded one.
    let since_rival = clock.host_monotonic_micros()? / 1000 - 1_000;
    assert!(since_rival >= 500);
    assert_eq!(
        crate::ledger::spacing_left(&store.writer_registry, LaneClass::Writer, 20_000, &clock),
        Some(20_000 - since_rival)
    );
    Ok(())
}
#[test]
fn memory_floor_warns_for_both_lane_classes() -> TestResult {
    // Each case has its own store and ledger, so only the floor can decide it.
    for (seed, writer, available) in [
        (50, true, 1),
        (51, false, 1),
        (52, true, 50),
        (53, false, 50),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        let run = lane(seed, writer);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let admitted = store.admit(id, &attempt, &HostFacts::new(available), &clock, &cancel);
        assert_eq!(
            admitted?.state,
            AttemptState::Running {
                extra: Default::default()
            },
            "writer: {writer}"
        );
    }
    Ok(())
}
#[test]
fn admission_warns_for_unconfigured_memory_floor_without_refusing() -> TestResult {
    // Both sides of the unconfigured floor admit; the floor is advisory.
    for available in [0, 1, 2, 100] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            memory_floor_basis_points: 100,
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        let run = lane(60, false);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(run.run_id, id, false)?;
        let admitted = store.admit(
            id,
            &attempt,
            &super::tests::HostFacts::new(available),
            &clock,
            &cancel,
        );
        let admitted = admitted?;
        assert!(matches!(admitted.state, AttemptState::Running { .. }));
        assert_eq!(admitted.started_at, Some(clock.wall_now()?));
    }
    Ok(())
}
#[test]
fn total_lane_caps_report_setting_and_source_for_read_only_lanes() -> TestResult {
    use provenant_records::diagnostic::{LaneLimit, SettingSource};
    for (config, expected) in [
        ("max_lanes = 3\nmax_project_lanes = 3", LaneLimit::HostTotal),
        (
            "max_lanes = 8\nmax_project_lanes = 3",
            LaneLimit::ProjectTotal,
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            read_only_spacing_ms: 0,
            admission_seconds: 1,
            ..AdmissionPolicy::default()
        });
        std::fs::write(
            store.writer_registry.join("config.toml"),
            format!("schema_version=1\n[domains.fabric.lanes]\n{config}\n"),
        )?;
        let facts = super::tests::HostFacts::new(100);
        for seed in 80..84 {
            let run = lane(seed, false);
            let id = starting(&store, &run, &call)?;
            let dir = store.attempt_dir(run.run_id, id, false)?;
            let result = store.admit(id, &dir, &facts, &clock, &cancel);
            if seed < 83 {
                assert!(matches!(result?.state, AttemptState::Running { .. }));
            } else {
                let Err(error @ RunError::Admission(AdmissionError::LaneCap { kind, origin, .. })) =
                    result
                else {
                    return Err("total cap did not refuse with its typed reason".into());
                };
                assert_eq!(kind, expected);
                assert_eq!(origin, SettingSource::Configuration);
                assert_eq!(error.code(), "LANE_CAP");
                let projection =
                    error.diagnostic(provenant_records::diagnostic::Destination::Local);
                assert!(projection.detail.unwrap().contains(kind.setting()));
                assert!(projection.next.unwrap().contains(kind.setting()));
            }
        }
    }
    Ok(())
}
#[test]
fn project_cap_allows_up_to_32_active_lanes() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    // Four runs of eight keep each run under its own batch concurrency.
    let mut index = 0_i64;
    for seed in 60..64 {
        let run = batch(seed, 8);
        store.create(&run, &call)?;
        for attempt in &run.attempts {
            let id = attempt.attempt_id;
            store.mark_starting(
                &store.record(id, &call)?,
                &route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            // Spaced past both classes' windows so only the cap can refuse.
            index += 1;
            assert!(
                store.claim(id, &at(index * 20_000), &call)?.is_some(),
                "lane {index} is within the 32-lane project cap"
            );
        }
    }
    let over = lane(70, false);
    let id = starting(&store, &over, &call)?;
    assert!(
        store.claim(id, &at(40 * 20_000), &call)?.is_none(),
        "the 33rd lane waits for the project cap"
    );
    Ok(())
}
#[test]
fn batch_concurrency_defaults_by_lane_class_and_an_explicit_value_wins() {
    let profile = |knobs: &[(&str, u64)]| HarnessProfile {
        name: "test".into(),
        knobs: knobs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
        ..HarnessProfile::default()
    };
    let tuned = profile(&[
        ("lanes.max_concurrency", 2),
        ("lanes.max_concurrency_read_only", 5),
    ]);
    assert_eq!(concurrency(None, &tuned, false, None), 2);
    assert_eq!(concurrency(None, &tuned, true, None), 5);
    let bare = HarnessProfile::default();
    let writer_default = concurrency(None, &bare, false, None);
    let reader_default = concurrency(None, &bare, true, None);
    assert!(reader_default >= writer_default);
    assert_eq!(concurrency(Some(3), &bare, false, None), 3);
    assert_eq!(concurrency(Some(3), &bare, true, None), 3);
    assert_eq!(concurrency(None, &tuned, true, Some(3)), 3, "bounded");
    assert_eq!(
        concurrency(None, &tuned, true, Some(7)),
        5,
        "a cap, not a floor"
    );
    assert_eq!(concurrency(Some(12), &tuned, true, Some(3)), 12, "explicit");
}
#[test]
fn lane_waits_name_the_class_and_what_each_unstarted_lane_waits_for() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    let first = starting(&store, &lane(100, true), &call)?;
    let second = starting(&store, &lane(101, true), &call)?;
    let reader = starting(&store, &lane(102, false), &call)?;
    let queued = lane(103, false);
    store.create(&queued, &call)?;
    let queued = queued.attempts[0].attempt_id;
    // The test clock reads 0 ms, so the writer start is happening now.
    assert!(store.claim(first, &at(0), &call)?.is_some());
    let records = [first, second, reader, queued]
        .map(|id| store.record(id, &call))
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    let waits = store.lane_waits(&records, &call);
    let wait = |id| waits.iter().find(|w| w.attempt_id == id);
    assert!(wait(first).is_none(), "a running lane waits for nothing");
    let writer = wait(second).ok_or("second lane waits")?;
    assert_eq!(writer.lane_class, LaneClass::Writer);
    assert_eq!(
        writer.waiting_for,
        "writer startup spacing: 20.0 s left of 20.0 s"
    );
    let reader = wait(reader).ok_or("read-only lane waits")?;
    assert_eq!(reader.lane_class, LaneClass::ReadOnly);
    assert!(
        reader.waiting_for.starts_with("launch preparation"),
        "no read-only start yet, so the writer's spacing is not its wait: {}",
        reader.waiting_for
    );
    let queued = wait(queued).ok_or("queued lane waits")?;
    assert_eq!(queued.lane_class, LaneClass::ReadOnly);
    assert!(queued.waiting_for.starts_with("launch"));

    // A serial run reports its own concurrency before any spacing.
    let serial = {
        let mut run = two_tasks(1);
        run.run_id = RunId::from_bytes([110; 16]);
        run.operation_id = provenant_core::OperationId::parse("serial-waits")?;
        run.attempts[0].attempt_id = AttemptId::from_bytes([111; 16]);
        run.attempts[1].attempt_id = AttemptId::from_bytes([112; 16]);
        run
    };
    store.create(&serial, &call)?;
    let [a, b] = [serial.attempts[0].attempt_id, serial.attempts[1].attempt_id];
    for id in [a, b] {
        store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
    }
    assert!(store.claim(a, &at(0), &call)?.is_some());
    let blocked = store.lane_waits(&[store.record(b, &call)?], &call);
    assert_eq!(
        blocked[0].waiting_for,
        "run concurrency: 1 of 1 lanes of this run are active"
    );
    Ok(())
}
#[test]
fn waiting_parent_yields_host_and_project_leases_and_both_writers_finish() -> TestResult {
    child_wait_lease_cycle(false)
}
