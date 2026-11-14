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
fn an_unreadable_host_clock_or_boot_identity_refuses_the_claim() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let [lane_id] = writers(&store, [91], &call)?;
    let attempt = store.attempt_dir(lane_id, false)?;
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
        let attempt = store.attempt_dir(id, false)?;
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
        let attempt = store.attempt_dir(id, false)?;
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
fn waiting_parent_yields_host_and_project_leases_and_both_writers_finish() -> TestResult {
    child_wait_lease_cycle(false)
}
