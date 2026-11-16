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
fn advisory_memory_floor_does_not_refuse_writer_or_reader() -> TestResult {
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
                extra: Default::default(),
                child_wait: None,
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
// Spec: docs/specs/fabric.md#session-ready-steering
fn admission_admits_only_its_own_steer_marker_and_fails_closed_otherwise() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let foreign = provenant_core::ActorId::from_bytes([0xee; 16]);
    let ids = writers(&store, [120, 121, 122, 123, 124, 125], &call)?;
    let marker = |id: AttemptId, case: usize| -> Result<Vec<u8>> {
        let scope = RunStore::scope(&store.record(id, &call)?);
        let request_id = format!("{}.{}.cancel", scope.attempt, scope.generation);
        let wrong_scope = provenant_records::custody::Scope {
            attempt: scope.attempt,
            generation: scope.generation + 1,
        };
        let steered = provenant_supervisor::machine::StopReason::Steered;
        let explicit = provenant_supervisor::machine::StopReason::ExplicitCancellation;
        let fields = match case {
            0 => return Ok(b"{not json".to_vec()),
            1 => (scope, foreign, request_id.as_str(), explicit),
            2 => (wrong_scope, context.actor_id, request_id.as_str(), steered),
            3 => (scope, context.actor_id, "other.cancel", steered),
            _ => (scope, context.actor_id, request_id.as_str(), steered),
        };
        Ok(crate::db::bytes(&serde_json::json!({
            "schema_version": 1,
            "scope": fields.0,
            "requester": fields.1,
            "request_id": fields.2,
            "reason": fields.3,
        }))?)
    };
    for (case, id) in ids.into_iter().enumerate() {
        let attempt = store.attempt_dir(id, false)?;
        if case < 5 {
            artifacts::seal(&attempt, "cancel.json", &marker(id, case)?, RECORD_LIMIT)?;
        }
        let scope = RunStore::scope(&store.record(id, &call)?);
        let expected = match case {
            0..=3 => crate::store::CancelMarker::Stop,
            4 => crate::store::CancelMarker::Steered {
                requester: context.actor_id,
            },
            _ => crate::store::CancelMarker::Absent,
        };
        assert_eq!(
            crate::store::CancelMarker::read(&attempt, scope)?,
            expected,
            "case {case}"
        );
        assert_eq!(
            crate::store::cancel_requested(&attempt, scope),
            case < 4,
            "case {case}"
        );
        let admitted = store.admit(id, &attempt, &HostFacts::new(100), &clock, &cancel);
        assert_eq!(
            matches!(admitted, Err(RunError::Cancelled)),
            case < 4,
            "case {case}: {admitted:?}"
        );
    }
    Ok(())
}
