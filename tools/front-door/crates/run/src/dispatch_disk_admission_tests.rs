use super::*;

#[test]
fn admission_configured_spacing_applies_to_both_classes() -> TestResult {
    for writer in [true, false] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        std::fs::write(
            store.writer_registry.join("config.toml"),
            "schema_version=1\n[domains.fabric.lanes]\nstart_spacing_ms=500\n",
        )?;
        let first = starting(&store, &lane(80, writer), &call)?;
        let second = starting(&store, &lane(81, writer), &call)?;
        assert!(store.claim(first, &at(0), &call)?.is_some());
        assert!(store.claim(second, &at(499), &call)?.is_none());
        assert!(store.claim(second, &at(500), &call)?.is_some());
    }
    Ok(())
}
#[test]
fn admission_wait_reason_names_the_reached_writer_cap() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    std::fs::write(
        store.writer_registry.join("config.toml"),
        "schema_version=1\n[domains.fabric.lanes]\nmax_writer_lanes=1\nmax_project_writer_lanes=1\n",
    )?;
    let [first, second] = writers(&store, [80, 81], &call)?;
    assert!(store.claim(first, &clock, &call)?.is_some());
    assert!(store.claim(second, &clock, &call)?.is_none());
    let waits = store.lane_waits_with_resources(
        &[store.record(second, &call)?],
        &call,
        &HostFacts::new(100),
    );
    assert_eq!(
        waits[0].waiting_for,
        "host capacity: host writer lanes: 1 of 1 active"
    );
    Ok(())
}
#[test]
fn prepared_write_grants_hold_admission_on_the_disk_floor() -> TestResult {
    use provenant_records::run::WriteSource;
    for (source, writable) in [
        (WriteSource::WriteRoot, true),
        (WriteSource::ToolWrite, true),
        (WriteSource::ReadOnlyException, true),
        (WriteSource::LaneCache, true),
        (WriteSource::StateWrite, false),
        (WriteSource::CredentialWrite, false),
        (WriteSource::PrivateState, false),
        (WriteSource::Scratch, false),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let registry = temp.path().canonicalize()?.join("registry");
        std::fs::DirBuilder::new().mode(0o700).create(&registry)?;
        std::fs::write(
            registry.join("config.toml"),
            "schema_version=1\n[gc]\nfree_floor='100B'\n",
        )?;
        let mut store = RunStore::open(&context, &call)?.with_writer_registry(&registry);
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..Default::default()
        });
        let run = lane(97, false);
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        let grants = admission::AppliedWriteGrants {
            grants: vec![WriteGrantRecord {
                extra: Default::default(),
                source,
                path: provenant_records::AbsolutePath::new(temp.path())?,
                scope: PathScope::Subtree,
            }],
            ..Default::default()
        };
        let record = store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &grants,
            None,
            &call,
        )?;
        let mut facts = HostFacts::new(100);
        facts.disk = 99;
        let waits = store.lane_waits_with_resources(&[record], &call, &facts);
        assert_eq!(
            waits[0].lane_class,
            if writable {
                LaneClass::Writer
            } else {
                LaneClass::ReadOnly
            }
        );
        let attempt = store.attempt_dir(id, false)?;
        let result = store.admit(id, &attempt, &facts, &clock, &cancel);
        if writable {
            assert!(
                matches!(
                    result,
                    Err(RunError::Admission(AdmissionError::Disk {
                        free: 99,
                        floor: 100,
                        ..
                    }))
                ),
                "{source:?}: {result:?}"
            );
        } else {
            assert!(
                matches!(result?.state, AttemptState::Running { .. }),
                "{source:?}"
            );
        }
    }
    Ok(())
}
#[test]
fn guardian_admission_resource_refusals_survive_durable_exit_and_retained_status() -> TestResult {
    use provenant_records::diagnostic::{
        LaneLimit, PressureLevel, PressureThreshold, SettingSource,
    };
    use provenant_supervisor::custody::{
        self, AdmissionError, CustodyError, Observation, ProcessExit,
    };
    for (error, setting, source, action) in [
        (
            CustodyError::Admission(AdmissionError::Disk {
                free: 1,
                floor: 2,
                origin: SettingSource::Configuration,
            }),
            "gc.free_floor",
            "host configuration",
            "free disk space",
        ),
        (
            CustodyError::Admission(AdmissionError::MemoryPressure {
                level: PressureLevel::Critical,
                threshold: PressureThreshold::Warning,
                origin: SettingSource::Configuration,
            }),
            "domains.fabric.lanes.memory_pressure_threshold",
            "host configuration",
            "reduce host memory pressure",
        ),
        (
            CustodyError::Admission(AdmissionError::LaneCap {
                active: 2,
                limit: 2,
                kind: LaneLimit::ProjectTotal,
                origin: SettingSource::ShippedDefault,
            }),
            "domains.fabric.lanes.max_project_lanes",
            "shipped default",
            "wait for an active lane",
        ),
        (
            CustodyError::Admission(AdmissionError::Processes { used: 2, limit: 3 }),
            "RLIMIT_NPROC",
            "operating system",
            "reduce host process use",
        ),
        (
            CustodyError::Io(std::io::Error::other("TOKEN_SENTINEL /private/host/path")),
            "",
            "",
            "inspect",
        ),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        store.create(&run, &call)?;
        let id = run.attempts[0].attempt_id;
        let record = store.mark_starting(
            &store.record(id, &call)?,
            &route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        // This is the same constructor Driver::run uses after holder.admit
        // refuses. Persist and reread it through normal custody observation.
        let process = ProcessExit::admission_refused(RunStore::scope(&record), &error);
        assert_eq!(process.launch_refusal.is_some(), !setting.is_empty());
        let dir = store.attempt_dir(id, false)?;
        artifacts::seal(
            &dir,
            "process-exit.json",
            &serde_json::to_vec(&process)?,
            RECORD_LIMIT,
        )?;
        let handle = custody::hold(dir, process.scope, store.context.actor_id);
        let Observation::Exited(observed) = custody::observe(
            &handle,
            provenant_core::WaitBudget::nonblocking(clock.mono_now()),
            &cancel,
            &clock,
        )?
        else {
            return Err("guardian refusal did not produce durable exit evidence".into());
        };
        assert_eq!(observed.launch_refusal, process.launch_refusal);
        let fake = Fake::default();
        store.finish(id, &store.attempt_dir(id, false)?, &observed, &fake, &call)?;
        let record = store.record(id, &call)?;
        assert!(matches!(
            record.state,
            AttemptState::Terminal {
                outcome: Outcome::Rejected,
                ..
            }
        ));
        let (_, reason) = provenant_records::wire::lane::LaneRow::state_and_reason(&record.state);
        let reason = reason.ok_or("missing retained refusal reason")?;
        assert!(reason.contains(setting), "{reason}");
        assert!(reason.contains(source), "{reason}");
        assert!(reason.contains(action), "{reason}");
        assert!(!reason.contains("TOKEN_SENTINEL"));
        assert!(!reason.contains("/private/host/path"));
        assert_eq!(fake.decoded.get(), 0);
    }
    Ok(())
}
