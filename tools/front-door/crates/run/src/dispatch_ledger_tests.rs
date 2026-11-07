use super::*;

#[test]
fn a_version_1_start_of_any_age_holds_each_class_one_full_spacing() -> TestResult {
    // Old (a spacing and more), recent, and in the wall future: the upgrade
    // reads no wall clock, so the recorded age changes nothing.
    for (case, ago_ms) in [
        ("old", 3_600_000),
        ("recent", 5_000),
        ("future", -3_600_000),
    ] {
        let temp = tempfile::tempdir()?;
        let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
        let call = stepped_call(&host, &cancel);
        let mut store = RunStore::open(&context(temp.path())?, &call)?;
        store.set_admission_policy(AdmissionPolicy {
            startup_spacing_ms: 20_000,
            read_only_spacing_ms: 2_000,
            ..AdmissionPolicy::default()
        });
        version_1_start(&store, &host, ago_ms)?;
        let writer = starting(&store, &lane(100, true), &call)?;
        let reader = starting(&store, &lane(101, false), &call)?;
        assert!(
            store.claim(writer, &host, &call)?.is_none(),
            "{case}: the writer is held from the upgrade"
        );
        assert!(
            store.claim(reader, &host, &call)?.is_none(),
            "{case}: the read-only class is held from the upgrade"
        );
        host.pass_ms(1_500);
        assert!(
            store.claim(reader, &host, &call)?.is_none(),
            "{case}: the read-only spacing has not run out"
        );
        host.pass_ms(1_000);
        assert!(
            store.claim(reader, &host, &call)?.is_some(),
            "{case}: the 2 s read-only spacing has run out"
        );
        host.pass_ms(17_000);
        assert!(
            store.claim(writer, &host, &call)?.is_none(),
            "{case}: the writer is held the full 20 s, not less"
        );
        host.pass_ms(1_000);
        assert!(
            store.claim(writer, &host, &call)?.is_some(),
            "{case}: held one spacing, and no longer"
        );
    }
    Ok(())
}
#[test]
fn a_version_1_ledger_with_no_start_holds_nothing() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    old_ledger(
        &store,
        &format!("{VERSION_1_LEDGER} PRAGMA user_version=1;"),
    )?;
    let writer = starting(&store, &lane(104, true), &call)?;
    let reader = starting(&store, &lane(105, false), &call)?;
    assert!(store.claim(writer, &host, &call)?.is_some());
    assert!(store.claim(reader, &host, &call)?.is_some());
    Ok(())
}
#[test]
fn a_version_1_upgrade_refuses_when_the_host_clock_cannot_be_read() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    version_1_start(&store, &host, 5_000)?;
    let writer = starting(&store, &lane(103, true), &call)?;
    *host.boot_readable.lock().unwrap() = false;
    assert!(matches!(
        store.claim(writer, &host, &call),
        Err(RunError::Admission(AdmissionError::Clock))
    ));
    let raw = rusqlite::Connection::open(store.writer_registry.join("admission.sqlite3"))?;
    let version: i64 = raw.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let rows: i64 = raw.query_row("SELECT COUNT(*) FROM spacing", [], |r| r.get(0))?;
    assert_eq!(
        (version, rows),
        (1, 1),
        "the refused upgrade dropped nothing"
    );
    *host.boot_readable.lock().unwrap() = true;
    assert!(store.claim(writer, &host, &call)?.is_none());
    Ok(())
}
#[test]
fn a_version_2_ledger_that_kept_wall_milliseconds_starts_every_class_free() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (host, cancel) = (Stepped::up(100_000), CancelToken::new());
    let call = stepped_call(&host, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        read_only_spacing_ms: 2_000,
        ..AdmissionPolicy::default()
    });
    old_ledger(
        &store,
        "CREATE TABLE leases (
          attempt TEXT PRIMARY KEY, project TEXT NOT NULL, store BLOB NOT NULL,
          class TEXT NOT NULL DEFAULT 'writer' CHECK(class IN ('writer','read_only'))
         ) STRICT;
         CREATE INDEX lease_projects ON leases(project);
         CREATE TABLE spacing (
          class TEXT PRIMARY KEY CHECK(class IN ('writer','read_only')),
          last_start INTEGER NOT NULL
         ) STRICT;
         INSERT INTO spacing(class,last_start) VALUES ('writer', 4000000000000);
         PRAGMA user_version=2;",
    )?;
    let [first, second] = writers(&store, [96, 97], &call)?;
    assert!(
        store.claim(first, &host, &call)?.is_some(),
        "wall milliseconds from before boot-keyed spacing hold no class"
    );
    assert!(store.claim(second, &host, &call)?.is_none());
    Ok(())
}
#[test]
fn legacy_tickets_without_a_class_are_treated_as_writers() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 20_000,
        ..AdmissionPolicy::default()
    });
    // A ledger from before lane classes, holding one lease and no start, so
    // the upgrade holds no class (the carried start has its own tests).
    old_ledger(
        &store,
        "CREATE TABLE leases (attempt TEXT PRIMARY KEY, project TEXT NOT NULL, store BLOB NOT NULL) STRICT;
         CREATE INDEX lease_projects ON leases(project);
         CREATE TABLE spacing (id INTEGER PRIMARY KEY CHECK(id=1), last_start INTEGER NOT NULL) STRICT;
         INSERT INTO leases(attempt,project,store) VALUES ('legacy','project',x'2f6e6f77686572652f');
         PRAGMA user_version=1;",
    )?;

    let old = starting(&store, &lane(80, true), &call)?;
    // A ticket recorded before classes has no class field; strip it.
    let raw: Vec<u8> = store.db.query_row(
        "SELECT record FROM admission WHERE attempt=?1",
        [old.to_string()],
        |r| r.get(0),
    )?;
    let mut ticket: serde_json::Value = db::decode(&raw)?;
    ticket
        .as_object_mut()
        .ok_or("ticket object")?
        .remove("lane_class");
    store.db.execute(
        "UPDATE admission SET record=?1 WHERE attempt=?2",
        rusqlite::params![db::bytes(&ticket)?, old.to_string()],
    )?;
    let writer = starting(&store, &lane(81, true), &call)?;
    let reader = starting(&store, &lane(82, false), &call)?;

    assert!(store.claim(old, &at(11_000), &call)?.is_some());
    assert!(
        store.claim(writer, &at(11_001), &call)?.is_none(),
        "a ticket with no class is a writer, spaced 20 s from the last writer start"
    );
    assert!(
        store.claim(reader, &at(11_001), &call)?.is_some(),
        "the writer start holds no read-only lane"
    );
    assert!(store.claim(writer, &at(31_000), &call)?.is_some());
    Ok(())
}
#[test]
fn reconcile_backfills_legacy_temporary_without_changing_terminal_records() -> TestResult {
    for path_kind in ["absent", "directory", "unreadable"] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        let temporary = store.temporary_path(id, &call)?;
        let before = store
            .withdraw(
                id,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "fixture",
                WallTime::from_micros(150)?,
                &call,
            )?
            .ok_or("not withdrawn")?;
        // Reproduce leaks left by an older binary without a cleanup hint.
        if path_kind == "directory" {
            // Older binaries also created tmp private to the store owner.
            std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
                .create(&temporary)?;
            std::fs::write(temporary.join("legacy.lock"), "discard")?;
        } else if path_kind == "unreadable" {
            let attempt = temporary.parent().ok_or("attempt directory")?;
            std::fs::rename(attempt, attempt.with_extension("moved"))?;
            std::os::unix::fs::symlink(attempt.with_extension("moved"), attempt)?;
        }
        store.db.execute_batch("DROP TABLE settlement_receipts; DROP INDEX reaction_pending; DROP TABLE reactions; DROP INDEX retained_temporary; ALTER TABLE attempts DROP COLUMN tmp_retained; PRAGMA user_version=3;")?;
        drop(store);

        let store = RunStore::open(&context, &call)?;
        if path_kind == "unreadable" {
            let attempt = temporary.parent().ok_or("attempt directory")?;
            std::fs::remove_file(attempt)?;
            std::fs::rename(attempt.with_extension("moved"), attempt)?;
        }
        // Records from before the receipt table carry no receipts.
        let mut legacy = before.clone();
        legacy.settlement_receipts = None;
        assert_eq!(store.record(id, &call)?, legacy);
        assert!(
            store
                .db
                .execute(
                    "UPDATE attempts SET record=record WHERE id=?1",
                    [id.to_string()]
                )
                .is_err()
        );
        for alias in ["rowid", "oid", "_rowid_"] {
            assert!(
                store
                    .db
                    .execute(
                        &format!("UPDATE attempts SET {alias}={alias}+100 WHERE id=?1"),
                        [id.to_string()]
                    )
                    .is_err()
            );
        }
        assert!(
            store
                .db
                .execute("DELETE FROM attempts WHERE id=?1", [id.to_string()])
                .is_err()
        );
        let expected = if path_kind == "absent" {
            vec![]
        } else {
            vec![run.run_id]
        };
        assert_eq!(store.open_runs(10, &call)?, expected);
        if path_kind == "directory" {
            assert_eq!(
                store.reconcile_with_observer(
                    run.run_id,
                    &Fake::default(),
                    &call,
                    &provenant_supervisor::census::SystemProcessObserver
                )?,
                vec![(id, Reconciled::TemporaryRemoved)]
            );
            assert!(!temporary.exists());
            assert!(store.open_runs(10, &call)?.is_empty());
        } else if path_kind == "unreadable" {
            // The migration retained the unreadable candidate. Now that its
            // anchor is repaired, absence clears the hint without a result.
            assert!(
                store
                    .reconcile_with_observer(
                        run.run_id,
                        &Fake::default(),
                        &call,
                        &provenant_supervisor::census::SystemProcessObserver
                    )?
                    .is_empty()
            );
        }
        drop(store);
        let store = RunStore::open(&context, &call)?;
        let mut legacy = before.clone();
        legacy.settlement_receipts = None;
        assert_eq!(store.record(id, &call)?, legacy);
        assert!(store.open_runs(10, &call)?.is_empty());
    }
    Ok(())
}
#[test]
fn raised_write_grant_limit_preserves_large_terminal_ledger() -> TestResult {
    struct Raised(Confined);
    impl Providers for Raised {
        fn write_grant_limit(&self) -> WriteGrantLimit {
            WriteGrantLimit {
                bytes: 2 * 1024 * 1024,
                origin: provenant_records::diagnostic::SettingSource::Configuration,
            }
        }
        fn prepare(
            &self,
            task: &TaskRequest,
            prompt: &str,
            paths: &LaunchPaths,
            resume: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, crate::PrepareRefusal> {
            self.0.prepare(task, prompt, paths, resume)
        }
        fn decode(
            &self,
            route: &RouteRecord,
            output: &[u8],
            exit: &ExitObservation,
            path: &std::path::Path,
        ) -> std::result::Result<Decoded, String> {
            self.0.decode(route, output, exit, path)
        }
    }
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let prepared = input();
    store.create(&prepared, &call)?;
    let id = prepared.attempts[0].attempt_id;
    let provider = Raised(Confined {
        writer: false,
        tool_writes: (0..400)
            .map(|i| PathGrant {
                path: format!("/fixture/tools/{i}/{}", "x".repeat(3000)).into(),
                scope: PathScope::Subtree,
            })
            .collect(),
    });
    let started = store.start(
        prepared.run_id,
        &provider,
        &|record, _| {
            let route = record.applied_route.as_ref().expect("applied route");
            assert_eq!(route.extra["write_grant_limit"]["value"], 2 * 1024 * 1024);
            assert_eq!(
                route.extra["write_grant_limit"]["origin"],
                "host configuration"
            );
            Err(RunError::Validation("fixture holder"))
        },
        &call,
    )?;
    assert!(matches!(
        started.as_slice(),
        [Started::Refused {
            kind: RefusalKind::Handoff,
            ..
        }]
    ));
    let record = store.record(id, &call)?;
    assert!(
        provenant_records::CanonicalV1::bytes(&record.write_grants)?
            .as_bytes()
            .len()
            > RECORD_LIMIT as usize
    );
    let attempt = store.attempt_dir(prepared.run_id, id, false)?;
    let terminal =
        crate::artifacts::read_sealed(&attempt, "terminal-pending.json", TERMINAL_RECORD_LIMIT)?
            .ok_or("terminal decision")?;
    assert!(terminal.len() > RECORD_LIMIT as usize);
    drop(store);
    let reopened = RunStore::open(&context, &call)?;
    assert_eq!(
        reopened.record(id, &call)?.write_grants,
        record.write_grants
    );
    Ok(())
}
#[test]
fn admission_default_spacing_does_not_hold_a_second_writer() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let [first, second] = writers(&store, [80, 81], &call)?;
    assert!(store.claim(first, &clock, &call)?.is_some());
    assert!(store.claim(second, &clock, &call)?.is_some());
    Ok(())
}
