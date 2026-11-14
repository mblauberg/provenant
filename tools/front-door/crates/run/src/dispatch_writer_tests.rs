use super::*;

#[test]
fn write_grants_wire_bound_and_git_digest_are_preserved() -> TestResult {
    use std::os::unix::ffi::OsStringExt;

    let temp = tempfile::tempdir()?;
    let clock = clock();
    let cancel = CancelToken::new();
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let prepared = input();
    store.create(&prepared, &call)?;
    let id = prepared.attempt_id;
    let grant = WriteGrantRecord {
        extra: Default::default(),
        source: WriteSource::LaneCache,
        path: provenant_records::AbsolutePath::new(std::path::Path::new("/fixture/cache"))?,
        scope: PathScope::Subtree,
    };
    let mut applied = admission::AppliedWriteGrants {
        grants: vec![grant.clone(); 1025],
        git_grants_digest: Some(provenant_core::Digest::from_bytes([7; 32])),
    };
    let mut record = store.record(id, &call)?;
    record.write_grants = applied.grants.clone();
    reader::validate_attempt(&record)?;
    assert!(reader::valid_write_grants(&applied.grants));
    assert!(
        provenant_records::CanonicalV1::bytes(&applied.grants)?
            .as_bytes()
            .len()
            < provenant_records::run::MAX_WRITE_GRANT_BYTES as usize
    );
    // Native bytes double in the wire encoding, exceeding the cap even
    // though the raw path is smaller. Existing records remain readable.
    let mut path = b"/fixture/".to_vec();
    path.extend(vec![b'x'; 128 * 1024]);
    path.push(0xff);
    applied.grants = vec![WriteGrantRecord {
        path: provenant_records::AbsolutePath::new(std::path::Path::new(
            &std::ffi::OsString::from_vec(path),
        ))?,
        ..grant.clone()
    }];
    assert!(
        provenant_records::CanonicalV1::bytes(&applied.grants)?
            .as_bytes()
            .len()
            > provenant_records::run::MAX_WRITE_GRANT_BYTES as usize
    );
    store.mark_starting(&record, &route(), &[], &applied, None, &call)?;
    record.write_grants = applied.grants.clone();
    let reread: AttemptRecord = serde_json::from_slice(&serde_json::to_vec(&record)?)?;
    reader::validate_attempt(&reread)?;

    drop(store);
    let store = RunStore::open(&context, &call)?;
    assert_eq!(
        store.record(id, &call)?.git_grants_digest,
        applied.git_grants_digest
    );
    // AbsolutePath retains PathWire's contained byte encoding. It cannot
    // deserialise a relative escape into an absolute grant.
    let mut invalid = serde_json::to_value(store.record(id, &call)?)?;
    invalid["write_grants"][0]["path"]["relative_to_root"]["path"] = serde_json::json!("../escape");
    assert!(serde_json::from_value::<AttemptRecord>(invalid).is_err());
    Ok(())
}
#[test]
fn lane_waits_report_only_the_running_lanes_own_disk_pause() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let id = starting(&store, &lane(120, true), &call)?;
    let running = store.claim(id, &at(0), &call)?.ok_or("lane starts")?;
    let other_id = starting(&store, &lane(121, true), &call)?;
    let other = store
        .claim(other_id, &at(20_000), &call)?
        .ok_or("other starts")?;
    let marker = store
        .context
        .state_root
        .as_path()
        .join("attempts")
        .join(id.to_string())
        .join("process.json");
    let facts = super::tests::HostFacts::new(100);
    let guardian = facts.guardian_identity().ok_or("fixture guardian")?;
    std::fs::write(
        &marker,
        serde_json::to_vec(&serde_json::json!({
            "disk_paused": true, "guardian": guardian,
        }))?,
    )?;
    let records = [running, other];
    let waits = store.lane_waits_with_resources(&records, &call, &facts);
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0].attempt_id, id);
    assert_eq!(
        waits[0].waiting_for,
        "disk pressure paused builds below gc.free_floor; run `provenant gc --apply`"
    );
    let exit_path = marker.with_file_name("process-exit.json");
    std::fs::write(&exit_path, br#"{"unresolved_cleanup":true}"#)?;
    assert!(
        store
            .lane_waits_with_resources(&records, &call, &facts)
            .is_empty(),
        "an exited provider cannot remain disk-paused while its guardian settles cleanup"
    );
    std::fs::remove_file(exit_path)?;
    let mut dead_guardian = guardian;
    dead_guardian.native_start = dead_guardian.native_start.saturating_add(1);
    std::fs::write(
        &marker,
        serde_json::to_vec(&serde_json::json!({
            "disk_paused": true, "guardian": dead_guardian,
        }))?,
    )?;
    assert!(
        store
            .lane_waits_with_resources(&records, &call, &facts)
            .is_empty(),
        "a dead guardian's checkpoint must not report a current disk pause"
    );
    Ok(())
}
#[test]
fn lane_waits_report_a_running_lane_waiting_for_a_build_slot() -> TestResult {
    use provenant_os::build_slots::WAIT_MARKER;
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let id = starting(&store, &lane(120, true), &call)?;
    let running = store.claim(id, &at(0), &call)?.ok_or("lane starts")?;
    let other_id = starting(&store, &lane(121, true), &call)?;
    let other = store
        .claim(other_id, &at(20_000), &call)?
        .ok_or("other starts")?;
    let attempt = |id: AttemptId, _attempt: AttemptId| {
        store
            .context
            .state_root
            .as_path()
            .join("attempts")
            .join(id.to_string())
    };
    // The shim's marker lives in the lane's scratch, `tmp/candidate-N`.
    let tmp = attempt(id, running.attempt_id).join("tmp/candidate-0");
    std::fs::create_dir_all(&tmp)?;
    let marker = tmp.join(format!("{WAIT_MARKER}.100"));
    let records = [running.clone(), other];
    assert!(store.lane_waits(&records, &call).is_empty());
    std::fs::write(&marker, "slot 0")?;
    let waits = store.lane_waits(&records, &call);
    assert_eq!(waits.len(), 1, "only the lane that waits is reported");
    assert_eq!(waits[0].attempt_id, id);
    assert!(waits[0].waiting_for.starts_with("build slot:"));
    assert!(waits[0].waiting_for.contains("build slots"));
    std::fs::write(&marker, "lock 0")?;
    assert!(
        store.lane_waits(&records, &call)[0]
            .waiting_for
            .contains("lock")
    );
    // A marker with the wait's start and slot counts shows the queue position.
    std::fs::write(&marker, "slot 0 0 2 3")?;
    assert_eq!(
        store.lane_waits(&records, &call)[0].waiting_for,
        "build slot: waiting 1 of 1 (longest first); 2 of 3 slots held"
    );
    // Implausible counts fall back to the plain wait.
    for forged in [
        "slot 0 0 4 3",
        "slot 0 0 0 65",
        "slot 0 0 0 0",
        "slot 0 9 1 3",
        "slot 0 0 1",
    ] {
        std::fs::write(&marker, forged)?;
        let waits = store.lane_waits(&records, &call);
        assert!(
            waits.is_empty()
                || waits[0]
                    .waiting_for
                    .contains("one of the host's build slots"),
            "{forged}"
        );
    }
    // Text the lane invents is ignored, and so is a marker that is not a file.
    for forged in ["disk 0", "slot", "slot x", "slot 0 and more words"] {
        std::fs::write(&marker, forged)?;
        assert!(store.lane_waits(&records, &call).is_empty(), "{forged}");
    }
    // Waits queue oldest first: the lane that began waiting later is second.
    let other_tmp = attempt(other_id, records[1].attempt_id).join("tmp/candidate-0");
    std::fs::create_dir_all(&other_tmp)?;
    let other_marker = other_tmp.join(format!("{WAIT_MARKER}.200"));
    std::fs::write(&marker, "slot 0 0 3 3")?;
    std::fs::write(&other_marker, "slot 0 0 3 3")?;
    let waits = store.lane_waits(&records, &call);
    let text = |lane: AttemptId| {
        waits
            .iter()
            .find(|wait| wait.attempt_id == lane)
            .map(|wait| wait.waiting_for.clone())
    };
    // Equal starts break by attempt id, so each lane has a distinct place.
    let mut places = [text(id), text(other_id)];
    places.sort();
    assert_eq!(
        places,
        [
            Some("build slot: waiting 1 of 2 (longest first); 3 of 3 slots held".to_owned()),
            Some("build slot: waiting 2 of 2 (longest first); 3 of 3 slots held".to_owned())
        ]
    );
    std::fs::remove_file(&other_marker)?;
    std::fs::write(&marker, "disk 0")?;
    // One marker per command: a sibling's marker going away leaves the wait.
    let sibling = tmp.join(format!("{WAIT_MARKER}.101"));
    std::fs::write(&sibling, "slot 0")?;
    std::fs::write(&marker, "disk 0")?;
    assert_eq!(store.lane_waits(&records, &call).len(), 1);
    std::fs::remove_file(&sibling)?;
    assert!(store.lane_waits(&records, &call).is_empty());
    std::fs::remove_file(&marker)?;
    std::os::unix::fs::symlink(temp.path().join("elsewhere"), &marker)?;
    std::fs::write(temp.path().join("elsewhere"), "slot 0")?;
    assert!(store.lane_waits(&records, &call).is_empty());
    std::fs::remove_file(&marker)?;
    // A marker the build stopped refreshing goes stale, and an exited
    // provider cannot be waiting.
    std::fs::write(&marker, "slot 0")?;
    assert_eq!(store.lane_waits(&records, &call).len(), 1);
    std::fs::write(
        attempt(id, running.attempt_id).join("process-exit.json"),
        br#"{"unresolved_cleanup":true}"#,
    )?;
    assert!(store.lane_waits(&records, &call).is_empty());
    std::fs::remove_file(attempt(id, running.attempt_id).join("process-exit.json"))?;
    clock.wait_until(MonoTime::from_micros(400_000_000))?;
    assert!(store.lane_waits(&records, &call).is_empty());
    Ok(())
}
#[test]
fn disk_pause_wait_ignores_false_missing_symlinked_and_oversized_observations() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let id = starting(&store, &lane(120, true), &call)?;
    let running = store.claim(id, &at(0), &call)?.ok_or("lane starts")?;
    let records = [running];
    let marker = store
        .context
        .state_root
        .as_path()
        .join("attempts")
        .join(id.to_string())
        .join("process.json");
    assert!(store.lane_waits(&records, &call).is_empty());
    std::fs::write(&marker, br#"{"disk_paused":false}"#)?;
    assert!(store.lane_waits(&records, &call).is_empty());
    std::fs::remove_file(&marker)?;
    let elsewhere = temp.path().join("elsewhere.json");
    let body = r#"{"disk_paused":true}"#;
    std::fs::write(&elsewhere, body)?;
    std::os::unix::fs::symlink(&elsewhere, &marker)?;
    assert!(store.lane_waits(&records, &call).is_empty());
    std::fs::remove_file(&marker)?;
    let padding = " ".repeat(usize::try_from(RECORD_LIMIT)? + 1);
    std::fs::write(&marker, format!("{body}{padding}"))?;
    assert!(store.lane_waits(&records, &call).is_empty());
    Ok(())
}
#[test]
fn admission_uses_configured_gc_floor_and_reports_the_fix() -> TestResult {
    struct Disk(u64);
    impl Resources for Disk {
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            crate::tests::HostFacts::new(100).guardian_identity()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            Ok(MemorySample {
                total_bytes: 100,
                available_bytes: 100,
            })
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(self.0)
        }
    }
    for (writer, write_root, free) in [
        (true, false, 99),
        (true, false, 100),
        (false, false, 99),
        (false, false, 100),
        (false, true, 99),
        (false, true, 100),
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
        std::fs::write(
            context.state_root.as_path().join("config.toml"),
            "schema_version=1\n[gc]\nfree_floor='1B'\n",
        )?;
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..Default::default()
        });
        let mut run = lane(98, writer);
        if write_root {
            run.request.task.access.write_roots = vec![temp.path().canonicalize()?];
        }
        let has_writes = writer || write_root;
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(id, false)?;
        let record = store.record(id, &call)?;
        let waits = store.lane_waits_with_resources(&[record], &call, &Disk(free));
        if has_writes && free < 100 {
            assert_eq!(
                waits[0].waiting_for,
                "host capacity: disk space: 99 bytes available; gc.free_floor is 100 bytes (source: host configuration)"
            );
        } else {
            assert_eq!(
                waits[0].waiting_for,
                "launch preparation: preparing the lane"
            );
        }
        let result = store.admit(id, &attempt, &Disk(free), &clock, &cancel);
        if has_writes && free < 100 {
            // Status sees a healthy attempt volume, but the guardian sampled
            // a full cwd/cache volume through its sealed LaneResources.
            let record = store.record(id, &call)?;
            let waits = store.lane_waits_with_resources(&[record], &call, &Disk(u64::MAX));
            assert_eq!(
                waits[0].waiting_for,
                "host capacity: disk space: 99 bytes available; gc.free_floor is 100 bytes (source: host configuration)"
            );
        }
        if !has_writes || free == 100 {
            assert_eq!(
                result?.state,
                AttemptState::Running {
                    extra: Default::default()
                }
            );
        } else {
            let error = result.err().ok_or("admitted below floor")?;
            assert!(matches!(
                error,
                RunError::Admission(AdmissionError::Disk {
                    free: 99,
                    floor: 100,
                    origin: provenant_records::diagnostic::SettingSource::Configuration,
                })
            ));
            assert_eq!(error.code(), "DISK_FLOOR");
            // The message names the floor's key and the repair command.
            let message = error.to_string();
            assert!(message.contains("gc.free_floor"), "{message}");
            assert!(message.contains("source: host configuration"), "{message}");
            assert!(message.contains("provenant gc --apply"), "{message}");
        }
    }
    Ok(())
}
#[test]
fn admission_below_the_floor_reclaims_build_output_and_then_admits() -> TestResult {
    struct Disk {
        free: Cell<u64>,
        reclaims: Cell<u32>,
        helps: bool,
    }
    impl Resources for Disk {
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            crate::tests::HostFacts::new(100).guardian_identity()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            Ok(MemorySample {
                total_bytes: 100,
                available_bytes: 100,
            })
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(self.free.get())
        }
        fn reclaim_disk(&self, _: &Directory) -> bool {
            self.reclaims.set(self.reclaims.get() + 1);
            if self.helps {
                self.free.set(100);
            }
            self.helps
        }
    }
    for (writer, helps) in [(true, true), (true, false), (false, false)] {
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
        let run = lane(102, writer);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(id, false)?;
        let disk = Disk {
            free: Cell::new(40),
            reclaims: Cell::new(0),
            helps,
        };
        let result = store.admit(id, &attempt, &disk, &clock, &cancel);
        assert_eq!(
            disk.reclaims.get(),
            u32::from(writer),
            "only writers reclaim during admission"
        );
        if helps || !writer {
            assert_eq!(
                result?.state,
                AttemptState::Running {
                    extra: Default::default()
                }
            );
        } else {
            assert!(matches!(
                result,
                Err(RunError::Admission(AdmissionError::Disk {
                    free: 40,
                    floor: 100,
                    ..
                }))
            ));
        }
    }
    Ok(())
}
#[test]
fn admission_missing_configuration_uses_the_default_host_floor() -> TestResult {
    struct Disk(u64);
    impl Resources for Disk {
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            crate::tests::HostFacts::new(100).guardian_identity()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            HostFacts::new(100).memory()
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(self.0)
        }
    }
    for below in [true, false] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let registry = temp.path().canonicalize()?.join("registry");
        std::fs::DirBuilder::new().mode(0o700).create(&registry)?;
        let config = registry.join("config.toml");
        std::fs::write(&config, "schema_version=1\n[gc]\nfree_floor='1B'\n")?;
        let mut store = RunStore::open(&context, &call)?.with_writer_registry(&registry);
        store.set_admission_policy(AdmissionPolicy {
            admission_seconds: 1,
            ..Default::default()
        });
        let run = lane(101, true);
        let id = starting(&store, &run, &call)?;
        let attempt = store.attempt_dir(id, false)?;
        std::fs::remove_file(config)?;
        // A nested store's config must not replace the missing host config.
        std::fs::write(context.state_root.as_path().join("config.toml"), "[gc")?;
        let floor = provenant_config::GcSettings::default().free_floor_bytes(&registry)?;
        let free = floor - u64::from(below);
        if below {
            let record = store.record(id, &call)?;
            let waits = store.lane_waits_with_resources(&[record], &call, &Disk(free));
            assert_eq!(
                waits[0].waiting_for,
                format!(
                    "host capacity: disk space: {free} bytes available; gc.free_floor is {floor} bytes (source: shipped default)"
                )
            );
        }
        let result = store.admit(id, &attempt, &Disk(free), &clock, &cancel);
        if below {
            assert!(
                matches!(result, Err(RunError::Admission(AdmissionError::Disk { free: f, floor: p, origin: provenant_records::diagnostic::SettingSource::ShippedDefault })) if f == free && p == floor)
            );
        } else {
            assert_eq!(
                result?.state,
                AttemptState::Running {
                    extra: Default::default()
                }
            );
        }
    }
    Ok(())
}
#[test]
fn admission_rereads_gc_floor_after_submission_and_while_waiting() -> TestResult {
    struct Disk<'a> {
        config: &'a std::path::Path,
        samples: Cell<u64>,
    }
    impl Resources for Disk<'_> {
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            crate::tests::HostFacts::new(100).guardian_identity()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            Ok(MemorySample {
                total_bytes: 100,
                available_bytes: 100,
            })
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            self.samples.set(self.samples.get() + 1);
            std::fs::write(self.config, "schema_version=1\n[gc]\nfree_floor='100B'\n")?;
            Ok(100)
        }
    }
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let mut store = RunStore::open(&context, &call)?;
    let config = context.state_root.as_path().join("config.toml");
    std::fs::write(&config, "schema_version=1\n[gc]\nfree_floor='1B'\n")?;
    store.set_admission_policy(AdmissionPolicy {
        admission_seconds: 1,
        ..Default::default()
    });
    let run = lane(99, true);
    let id = starting(&store, &run, &call)?;
    let attempt = store.attempt_dir(id, false)?;
    std::fs::write(&config, "schema_version=1\n[gc]\nfree_floor='101B'\n")?;
    let disk = Disk {
        config: &config,
        samples: Cell::new(0),
    };
    assert_eq!(
        store.admit(id, &attempt, &disk, &clock, &cancel)?.state,
        AttemptState::Running {
            extra: Default::default()
        }
    );
    assert_eq!(
        disk.samples.get(),
        2,
        "the changed floor must hold admission for one observation"
    );
    Ok(())
}
#[test]
fn admission_waits_for_a_readable_current_gc_floor() -> TestResult {
    struct Disk<'a> {
        config: &'a std::path::Path,
        samples: Cell<u64>,
    }
    impl Resources for Disk<'_> {
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            crate::tests::HostFacts::new(100).guardian_identity()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            crate::tests::HostFacts::new(100).observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            Ok(MemorySample {
                total_bytes: 100,
                available_bytes: 100,
            })
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            self.samples.set(self.samples.get() + 1);
            std::fs::write(self.config, "schema_version=1\n[gc]\nfree_floor='100B'\n")?;
            Ok(100)
        }
    }
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let mut store = RunStore::open(&context, &call)?;
    let config = context.state_root.as_path().join("config.toml");
    std::fs::write(&config, "schema_version=1\n[gc]\nfree_floor='1B'\n")?;
    store.set_admission_policy(AdmissionPolicy {
        admission_seconds: 1,
        ..Default::default()
    });
    let run = lane(100, true);
    let id = starting(&store, &run, &call)?;
    let attempt = store.attempt_dir(id, false)?;
    std::fs::write(&config, "schema_version=1\n[gc]\nfree_floor='")?;
    let disk = Disk {
        config: &config,
        samples: Cell::new(0),
    };
    assert_eq!(
        store.admit(id, &attempt, &disk, &clock, &cancel)?.state,
        AttemptState::Running {
            extra: Default::default()
        }
    );
    assert_eq!(
        disk.samples.get(),
        2,
        "an unreadable current floor must queue until the next valid observation"
    );
    Ok(())
}
#[test]
fn cancel_and_reconcile_return_promptly_while_writer_lock_is_held() -> TestResult {
    use std::ffi::OsStr;
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let attempt = store.attempt_dir(id, false)?;
    let _writer = attempt.try_lock(
        OsStr::new("writer.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    assert_eq!(
        store.cancel(id, "bounded cancellation", &call)?,
        CancelReceipt::Requested
    );
    // A unit-test executable interprets "internal settle" as test filters.
    // Exercise the control's process boundary without starting hidden tests.
    let run_directory = store.attempt_dir(run.attempt_id, false)?;
    for lock_name in ["settlement-spawn.lock", "settlement-worker.lock"] {
        let held =
            run_directory.try_lock(OsStr::new(lock_name), provenant_os::fs::LockMode::Exclusive)?;
        store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
            panic!("a concurrent settlement claim must exclude another spawn");
        })?;
        drop(held);
    }
    let driver = crate::settlement::Guard::claim(
        &attempt,
        RunStore::scope(&store.record(id, &call)?),
        call.clock,
    )?
    .ok_or("settlement owner")?;
    store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
        panic!("a live settlement owner needs no detached worker");
    })?;
    drop(driver);
    let mut launched = false;
    let queued = store.queue_reconcile_with(run.attempt_id, &call, |argv, cwd| {
        assert_eq!(argv.len(), 4);
        assert_eq!(argv[0], std::env::current_exe().unwrap().into_os_string());
        assert_eq!(argv[1..3], ["internal", "settle"]);
        assert_eq!(cwd, std::env::current_dir().unwrap());
        let request: serde_json::Value = serde_json::from_str(argv[3].to_str().unwrap()).unwrap();
        assert_eq!(
            request["attempt"],
            serde_json::to_value(run.attempt_id).unwrap()
        );
        launched = true;
        Ok(())
    })?;
    assert!(launched);
    store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
        panic!("an already queued settlement must not spawn another worker");
    })?;
    let marker: serde_json::Value = serde_json::from_slice(
        &run_directory.read_stable(OsStr::new("settlement-queued.json"), RECORD_LIMIT)?,
    )?;
    let expiry: WallTime = serde_json::from_value(marker["expires"].clone())?;
    let remaining = expiry.as_micros() - clock.wall_now()?.as_micros();
    clock.wait_until(provenant_core::MonoTime::from_micros(
        clock.mono_now().as_micros() + u64::try_from(remaining)?,
    ))?;
    let mut requeued = false;
    store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
        requeued = true;
        Ok(())
    })?;
    assert!(requeued, "expiry permits a fresh worker at the boundary");
    // Invalid or obsolete markers must fail visibly instead of permitting a spawn.
    for marker in [
        serde_json::json!({"schema_version":1,"attempt":run.attempt_id,"expires_ms":0}),
        serde_json::json!({"schema_version":3,"attempt":run.attempt_id,"expires":"1970-01-01T00:00:00.000000Z"}),
        serde_json::json!({"schema_version":2,"attempt":AttemptId::from_bytes([99;16]),"expires":"1970-01-01T00:00:00.000000Z"}),
        serde_json::json!({"schema_version":2,"attempt":run.attempt_id,"expires":"broken"}),
    ] {
        run_directory
            .atomic_write(
                OsStr::new("settlement-queued.json"),
                ".settlement-test-",
                &serde_json::to_vec(&marker)?,
                provenant_os::fs::PublishMode::Replace,
            )?
            .require_durable()?;
        assert!(
            store
                .queue_reconcile_with(run.attempt_id, &call, |_, _| {
                    panic!("invalid queue marker must not spawn a worker");
                })
                .is_err()
        );
    }
    assert!(matches!(queued.as_slice(), [(found, Reconciled::Settling { .. })] if *found == id));
    assert!(
        attempt
            .read_stable(OsStr::new("cancel.json"), RECORD_LIMIT)?
            .len()
            > 10
    );
    Ok(())
}
