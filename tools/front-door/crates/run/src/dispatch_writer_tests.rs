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
