use super::*;

fn work(f: &Fixture, who: u8) -> ClaimRequest {
    ClaimRequest {
        extra: Default::default(),
        id: provenant_core::ClaimId::from_bytes([9; 16]),
        project: project(),
        actor: actor(who),
        token: [who; 16],
        lease: DurationMicros::from_seconds(1).unwrap(),
        scope: WorkScope::Paths {
            extra: Default::default(),
            paths: vec![f.temp.path().to_path_buf()],
        },
    }
}
#[test]
fn canonical_overlap_expiry_release_and_generation() {
    let mut f = Fixture::new();
    let store = f.open();
    let first = store.acquire_claim(&work(&f, 3), &f.request()).unwrap();
    assert!(matches!(
        store.acquire_claim(&work(&f, 4), &f.request()),
        Err(Error::Overlap { .. })
    ));
    f.clock
        .advance(DurationMicros::from_micros(999_999))
        .unwrap();
    store.verify_claim(project(), &first, &f.request()).unwrap();
    f.clock.advance(DurationMicros::from_micros(1)).unwrap();
    assert!(matches!(
        store.verify_claim(project(), &first, &f.request()),
        Err(Error::StaleFence)
    ));
    let second = store.acquire_claim(&work(&f, 4), &f.request()).unwrap();
    assert_eq!(second.generation, first.generation + 1);
    assert!(matches!(
        store.release_claim(project(), &first, &f.request()),
        Err(Error::StaleFence)
    ));
    store
        .release_claim(project(), &second, &f.request())
        .unwrap();
    let third = store.acquire_claim(&work(&f, 4), &f.request()).unwrap();
    assert_eq!(third.generation, second.generation + 1);
}

#[test]
fn work_claim_unknown_fields_survive_load_update_and_save() {
    let mut f = Fixture::new();
    let store = f.open();
    let input = work(&f, 3);
    let fence = store.acquire_claim(&input, &f.request()).unwrap();
    add_future_member(&store, "claims");
    assert_future_member(&store.verify_claim(project(), &fence, &f.request()).unwrap());
    store
        .renew_claim(
            project(),
            &fence,
            DurationMicros::from_seconds(2).unwrap(),
            &f.request(),
        )
        .unwrap();
    drop(store);
    let store = f.open();
    let saved = store.verify_claim(project(), &fence, &f.request()).unwrap();
    assert_eq!(
        saved.expires_at,
        Some(
            f.now()
                .checked_add(DurationMicros::from_seconds(2).unwrap())
                .unwrap()
        )
    );
    assert_future_member(&saved);
    // A takeover reconstructs the stored record and must also retain extras.
    f.clock
        .advance(DurationMicros::from_seconds(2).unwrap())
        .unwrap();
    let next = store.acquire_claim(&work(&f, 4), &f.request()).unwrap();
    assert_future_member(&store.verify_claim(project(), &next, &f.request()).unwrap());
}

#[test]
fn path_prefix_boundaries_symlinks_worktrees_and_issue_scopes() {
    let f = Fixture::new();
    let store = f.open();
    let a = f.temp.path().join("a");
    let ab = f.temp.path().join("ab");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&ab).unwrap();
    std::fs::create_dir(a.join("child")).unwrap();
    let mut input = work(&f, 3);
    input.scope = WorkScope::Worktree {
        extra: Default::default(),
        path: a.clone(),
    };
    let fence = store.acquire_claim(&input, &f.request()).unwrap();
    input.id = provenant_core::ClaimId::from_bytes([11; 16]);
    input.scope = WorkScope::Paths {
        extra: Default::default(),
        paths: vec![ab],
    };
    store.acquire_claim(&input, &f.request()).unwrap();
    input.scope = WorkScope::Paths {
        extra: Default::default(),
        paths: vec![a.join("child")],
    };
    assert!(matches!(
        store.acquire_claim(&input, &f.request()),
        Err(Error::Overlap { .. })
    ));
    let link = f.temp.path().join("alias");
    std::os::unix::fs::symlink(&a, &link).unwrap();
    input.scope = WorkScope::Paths {
        extra: Default::default(),
        paths: vec![link],
    };
    assert!(matches!(
        store.acquire_claim(&input, &f.request()),
        Err(Error::Overlap { .. })
    ));
    input.scope = WorkScope::Paths {
        extra: Default::default(),
        paths: vec![f.temp.path().into()],
    };
    assert!(matches!(
        store.acquire_claim(&input, &f.request()),
        Err(Error::Overlap { .. })
    ));
    let mut wrong = fence.clone();
    wrong.token = [0; 16];
    assert!(matches!(
        store.release_claim(project(), &wrong, &f.request()),
        Err(Error::StaleFence)
    ));
    store
        .renew_claim(
            project(),
            &fence,
            DurationMicros::from_seconds(2).unwrap(),
            &f.request(),
        )
        .unwrap();
    store
        .release_claim(project(), &fence, &f.request())
        .unwrap();
    input.id = provenant_core::ClaimId::from_bytes([12; 16]);
    input.scope = WorkScope::Issue {
        extra: Default::default(),
        key: "issue-42".into(),
    };
    let issue = store.acquire_claim(&input, &f.request()).unwrap();
    input.id = provenant_core::ClaimId::from_bytes([13; 16]);
    input.actor = actor(4);
    assert!(
        matches!(store.acquire_claim(&input,&f.request()),Err(Error::Overlap {claim}) if claim==issue.id)
    );
    input.project = ProjectId::from_bytes([2; 16]);
    store.acquire_claim(&input, &f.request()).unwrap();
    let listed = store.claims(project(), None, 100, &f.request()).unwrap();
    assert_eq!(listed.items.len(), 2);
    assert_eq!(
        store
            .claims(
                project(),
                listed.items.last().map(|c| c.id),
                100,
                &f.request()
            )
            .unwrap()
            .items
            .len(),
        0
    );
    assert!(!format!("{fence:?}").contains("token"));
}

#[test]
fn two_connection_claim_and_exact_expiry_takeover_races_have_one_winner() {
    let mut f = Fixture::new();
    drop(f.open());
    for takeover in [false, true] {
        if takeover {
            f.clock
                .advance(DurationMicros::from_seconds(1).unwrap())
                .unwrap();
        }
        let barrier = Arc::new(Barrier::new(2));
        std::thread::scope(|s| {
            let launch = |who| {
                let f = &f;
                let barrier = barrier.clone();
                s.spawn(move || {
                    let store = f.open();
                    barrier.wait();
                    store.acquire_claim(&work(f, who), &f.request())
                })
            };
            let a = launch(3);
            let b = launch(4);
            let results = [a.join().unwrap(), b.join().unwrap()];
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
            assert_eq!(
                results
                    .iter()
                    .filter(|r| matches!(r, Err(Error::Overlap { .. })))
                    .count(),
                1
            );
            assert_eq!(
                results.into_iter().find_map(Result::ok).unwrap().generation,
                if takeover { 2 } else { 1 }
            );
        });
    }
}

#[test]
fn expired_claim_refuses_renewal_but_allows_release() {
    let mut f = Fixture::new();
    let store = f.open();
    let work = store.acquire_claim(&work(&f, 3), &f.request()).unwrap();
    f.clock
        .advance(DurationMicros::from_seconds(1).unwrap())
        .unwrap();
    assert!(matches!(
        store.renew_claim(
            project(),
            &work,
            DurationMicros::from_seconds(1).unwrap(),
            &f.request()
        ),
        Err(Error::StaleFence)
    ));
    store.release_claim(project(), &work, &f.request()).unwrap();
}

// Each stage runs with real SQLite allocation failure or a second connection's
// RESERVED lock. Successful writes after the fault prove the connection recovers.
#[derive(Debug, Clone, Copy)]
enum Mutation {
    Acquire,
    Renew,
    Release,
}
fn exercise_fault(stage: Mutation, full: bool) {
    let f = Fixture::new();
    let store = f.open();
    let work_fence = if matches!(stage, Mutation::Acquire) {
        None
    } else {
        Some(store.acquire_claim(&work(&f, 3), &f.request()).unwrap())
    };
    let action = |req: &Request<'_>| -> Result<()> {
        match stage {
            Mutation::Acquire => store.acquire_claim(&work(&f, 3), req).map(|_| ()),
            Mutation::Renew => store.renew_claim(
                project(),
                work_fence.as_ref().unwrap(),
                DurationMicros::from_seconds(2).unwrap(),
                req,
            ),
            Mutation::Release => store.release_claim(project(), work_fence.as_ref().unwrap(), req),
        }
    };
    let before = database_records(&store);
    let holder = f.open();
    let req = f.request();
    if full {
        store.db.execute_batch("CREATE TABLE fault_padding(bytes BLOB); CREATE TRIGGER fill_on_event BEFORE INSERT ON events BEGIN INSERT INTO fault_padding VALUES(zeroblob(1048576)); END;").unwrap();
        let pages: i64 = store
            .db
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .unwrap();
        store
            .db
            .pragma_update(None, "max_page_count", pages)
            .unwrap();
    } else {
        holder.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        store
            .db
            .busy_timeout(std::time::Duration::from_millis(10))
            .unwrap();
    }
    let result = action(&req);
    if full {
        assert!(
            matches!(result,Err(Error::Sqlite(rusqlite::Error::SqliteFailure(code,_))) if code.code==rusqlite::ErrorCode::DiskFull),
            "{stage:?}: {result:?}"
        );
        store
            .db
            .execute_batch("DROP TRIGGER fill_on_event; PRAGMA max_page_count=1073741823;")
            .unwrap();
    } else {
        assert!(matches!(result, Err(Error::Busy)), "{stage:?}: {result:?}");
        holder.db.execute_batch("ROLLBACK").unwrap();
    }
    assert_eq!(database_records(&store), before, "{stage:?}");
    action(&f.request()).unwrap();
}
fn database_records(store: &Store) -> Vec<Vec<u8>> {
    let mut result = Vec::new();
    for table in ["claims", "operations", "events"] {
        let mut stmt = store
            .db
            .prepare(&format!("SELECT record FROM {table} ORDER BY record"))
            .unwrap();
        result.extend(
            stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))
                .unwrap()
                .map(|r| r.unwrap()),
        );
    }
    for query in [
        "SELECT project,kind,scope_key,id,generation FROM claim_counters ORDER BY project,kind,scope_key",
        "SELECT project,claim,path FROM claim_paths ORDER BY project,claim,path",
    ] {
        let mut stmt = store.db.prepare(query).unwrap();
        let columns = stmt.column_count();
        result.extend(
            stmt.query_map([], |row| {
                let mut bytes = Vec::new();
                for column in 0..columns {
                    bytes.extend_from_slice(format!("{:?}", row.get_ref(column)?).as_bytes());
                }
                Ok(bytes)
            })
            .unwrap()
            .map(|r| r.unwrap()),
        );
    }
    result
}
#[test]
fn full_and_busy_roll_back_every_new_mutation_and_its_event() {
    for full in [false, true] {
        for stage in [Mutation::Acquire, Mutation::Renew, Mutation::Release] {
            exercise_fault(stage, full);
        }
    }
}

#[test]
fn invalid_inputs_tokens_generations_and_cancellation_leave_no_mutation() {
    let f = Fixture::new();
    let store = f.open();
    for lease in [0, 999_999, 3_600_000_001, u64::MAX] {
        let mut input = work(&f, 3);
        input.lease = DurationMicros::from_micros(lease);
        assert!(matches!(
            store.acquire_claim(&input, &f.request()),
            Err(Error::Invalid(_))
        ));
    }
    for scope in [
        WorkScope::Paths {
            extra: Default::default(),
            paths: vec![],
        },
        WorkScope::Paths {
            extra: Default::default(),
            paths: vec!["relative".into()],
        },
        WorkScope::Issue {
            extra: Default::default(),
            key: String::new(),
        },
    ] {
        let mut input = work(&f, 3);
        input.scope = scope;
        assert!(matches!(
            store.acquire_claim(&input, &f.request()),
            Err(Error::Invalid(_))
        ));
    }
    let fence = store.acquire_claim(&work(&f, 3), &f.request()).unwrap();
    let before = database_records(&store);
    f.cancel.cancel();
    assert!(matches!(
        store.release_claim(project(), &fence, &f.request()),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        store.renew_claim(
            project(),
            &fence,
            DurationMicros::from_seconds(1).unwrap(),
            &f.request()
        ),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        store.acquire_claim(&work(&f, 4), &f.request()),
        Err(Error::Cancelled)
    ));
    assert_eq!(database_records(&store), before);
}

#[test]
fn non_utf8_paths_and_exhausted_generations_return_typed_errors() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    let store = f.open();
    let path = f.temp.path().join(std::ffi::OsString::from_vec(vec![255]));
    let mut input = work(&f, 3);
    input.scope = WorkScope::Paths {
        extra: Default::default(),
        paths: vec![path],
    };
    assert!(matches!(
        store.acquire_claim(&input, &f.request()),
        Err(Error::PathEncoding)
    ));
    let input = work(&f, 3);
    let fence = store.acquire_claim(&input, &f.request()).unwrap();
    store
        .release_claim(project(), &fence, &f.request())
        .unwrap();
    store
        .db
        .execute(
            "UPDATE claim_counters SET generation=9223372036854775807",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.acquire_claim(&input, &f.request()),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn acquire_retries_return_live_fences_without_renewal_or_events() {
    let mut f = Fixture::new();
    let store = f.open();
    let work_input = work(&f, 3);
    let work = store.acquire_claim(&work_input, &f.request()).unwrap();
    let before = database_records(&store);
    assert_eq!(
        store.acquire_claim(&work_input, &f.request()).unwrap(),
        work
    );
    assert_eq!(database_records(&store), before);
    let mut changed = work_input.clone();
    changed.token = [8; 16];
    assert!(matches!(
        store.acquire_claim(&changed, &f.request()),
        Err(Error::Overlap { .. })
    ));
    changed.token = work_input.token;
    changed.actor = actor(8);
    assert!(matches!(
        store.acquire_claim(&changed, &f.request()),
        Err(Error::Overlap { .. })
    ));
    f.clock.advance(work_input.lease).unwrap();
    let next = store.acquire_claim(&work_input, &f.request()).unwrap();
    assert_eq!(next.generation, work.generation + 1);
}

#[test]
fn renewal_uses_one_wall_timestamp_for_expiry_and_record() {
    struct TickingClock(std::sync::Mutex<i64>);
    impl Clock for TickingClock {
        fn wall_now(&self) -> std::result::Result<WallTime, TimeError> {
            let mut value = self.0.lock().unwrap();
            let at = *value;
            *value += 1;
            WallTime::from_micros(at)
        }
        fn mono_now(&self) -> MonoTime {
            MonoTime::from_micros(0)
        }
        fn wait_until(&self, _deadline: MonoTime) -> std::result::Result<(), TimeError> {
            unreachable!("fixed-time test clock never waits")
        }
    }
    let f = Fixture::new();
    let store = f.open();
    let work_input = work(&f, 3);
    let work = store.acquire_claim(&work_input, &f.request()).unwrap();
    let clock = TickingClock(std::sync::Mutex::new(f.now().as_micros()));
    let mut req = f.request();
    req.clock = &clock;
    store
        .renew_claim(project(), &work, work_input.lease, &req)
        .unwrap();
    let row = store.verify_claim(project(), &work, &req).unwrap();
    assert_eq!(
        row.expires_at,
        Some(row.updated_at.checked_add(work_input.lease).unwrap())
    );
}

#[test]
fn acquire_prunes_only_bounded_released_unreferenced_rows_and_keeps_counters() {
    let f = Fixture::new();
    let store = f.open();
    let mut input = work(&f, 3);
    let mut released = Vec::new();
    // Seed a backlog without intervening acquires pruning it.
    for n in 0..105_u128 {
        input.id = provenant_core::ClaimId::from_bytes(n.to_be_bytes());
        input.scope = WorkScope::Issue {
            extra: Default::default(),
            key: format!("prune-{n}"),
        };
        released.push((
            input.clone(),
            store.acquire_claim(&input, &f.request()).unwrap(),
        ));
    }
    for (_, fence) in &released {
        store.release_claim(project(), fence, &f.request()).unwrap();
    }
    input.id = provenant_core::ClaimId::from_bytes([80; 16]);
    input.scope = WorkScope::Issue {
        extra: Default::default(),
        key: "live".into(),
    };
    let live = store.acquire_claim(&input, &f.request()).unwrap();
    let count: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM claims WHERE holder IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 5);
    store.verify_claim(project(), &live, &f.request()).unwrap();
    let (input, old) = &released[0];
    let next = store.acquire_claim(input, &f.request()).unwrap();
    assert_eq!(next.id, old.id);
    assert_eq!(next.generation, old.generation + 1);
    assert!(matches!(
        store.release_claim(project(), old, &f.request()),
        Err(Error::StaleFence)
    ));
    let count: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM claims WHERE holder IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn migration_four_preserves_all_claim_rows_and_pruning_retains_held_rows() {
    let mut f = Fixture::new();
    let store = f.open();
    let mut input = work(&f, 3);
    let work_fence = store.acquire_claim(&input, &f.request()).unwrap();
    let before = database_records(&store);
    downgrade_to_six(&store, "");
    store.db.execute_batch("DROP TABLE peer_operations; DROP TABLE run_event_cursors; DROP TABLE coordination_operations; DROP TABLE claim_counters; DROP INDEX claim_path_lookup; DROP INDEX released_claims; PRAGMA user_version=3;").unwrap();
    drop(store);
    let store = f.open();
    assert_eq!(database_records(&store), before);
    store
        .verify_claim(project(), &work_fence, &f.request())
        .unwrap();
    let counters: i64 = store
        .db
        .query_row("SELECT count(*) FROM claim_counters", [], |r| r.get(0))
        .unwrap();
    assert_eq!(counters, 1);
    // Expired but unreleased owners are retained too.
    f.clock.advance(input.lease).unwrap();
    input.id = provenant_core::ClaimId::from_bytes([20; 16]);
    input.scope = WorkScope::Issue {
        extra: Default::default(),
        key: "trigger-prune".into(),
    };
    let live = store.acquire_claim(&input, &f.request()).unwrap();
    store.verify_claim(project(), &live, &f.request()).unwrap();
    store
        .release_claim(project(), &work_fence, &f.request())
        .unwrap();
    // Re-acquire also prunes that released path, retaining its counter.
    let next = store.acquire_claim(&work(&f, 3), &f.request()).unwrap();
    assert_eq!(next.id, work_fence.id);
    assert_eq!(next.generation, work_fence.generation + 1);
    let paths: i64 = store
        .db
        .query_row("SELECT count(*) FROM claim_paths", [], |r| r.get(0))
        .unwrap();
    assert_eq!(paths, 1);
    assert!(
        store
            .db
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
            .optional()
            .unwrap()
            .is_none()
    );
}

#[cfg(target_os = "macos")]
#[test]
#[allow(clippy::print_stderr)] // Report whether the filesystem probe exercised case aliases.
fn case_aliases_on_case_insensitive_volume_follow_canonical_paths() {
    let f = Fixture::new();
    let upper = f.temp.path().join("Foo");
    let lower = f.temp.path().join("foo");
    std::fs::create_dir(&upper).unwrap();
    if !lower.exists() {
        eprintln!("case-sensitive volume: case alias probe not applicable");
        return;
    }
    let same = upper.canonicalize().unwrap() == lower.canonicalize().unwrap();
    eprintln!("case-insensitive volume: canonical paths equal = {same}");
    let store = f.open();
    let mut input = work(&f, 3);
    input.scope = WorkScope::Paths {
        extra: Default::default(),
        paths: vec![upper],
    };
    store.acquire_claim(&input, &f.request()).unwrap();
    input.id = provenant_core::ClaimId::from_bytes([14; 16]);
    input.actor = actor(4);
    input.scope = WorkScope::Paths {
        extra: Default::default(),
        paths: vec![lower],
    };
    let result = store.acquire_claim(&input, &f.request());
    if same {
        assert!(matches!(result, Err(Error::Overlap { .. })));
    } else {
        assert!(result.is_ok());
    } // Documented advisory limitation.
}

#[test]
fn overlap_queries_search_the_path_index_and_root_covers_every_path() {
    let f = Fixture::new();
    let store = f.open();
    for (sql, args) in [
        (
            crate::claims::DESCENDANT_OVERLAP,
            vec![
                project().to_string(),
                "/a/".into(),
                "/a0".into(),
                "0".into(),
            ],
        ),
        (
            crate::claims::ANCESTOR_OVERLAP,
            vec![project().to_string(), "/a".into(), "0".into()],
        ),
    ] {
        let mut statement = store
            .db
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        let plan = statement
            .query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(3))
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<_>>();
        assert!(
            plan.iter().any(|s| s.contains("claim_path_lookup")),
            "{plan:?}"
        );
    }
    let mut input = work(&f, 3);
    let fence = store.acquire_claim(&input, &f.request()).unwrap();
    input.actor = actor(4);
    input.id = provenant_core::ClaimId::from_bytes([21; 16]);
    input.scope = WorkScope::Paths {
        extra: Default::default(),
        paths: vec!["/".into()],
    };
    assert!(matches!(
        store.acquire_claim(&input, &f.request()),
        Err(Error::Overlap { .. })
    ));
    store
        .release_claim(project(), &fence, &f.request())
        .unwrap();
    let root = store.acquire_claim(&input, &f.request()).unwrap();
    assert!(matches!(
        store.acquire_claim(&work(&f, 3), &f.request()),
        Err(Error::Overlap { .. })
    ));
    store.release_claim(project(), &root, &f.request()).unwrap();
}

#[test]
fn pruned_paths_and_counters_roll_back_if_acquire_event_fails() {
    let f = Fixture::new();
    let store = f.open();
    let old_input = work(&f, 3);
    let fence = store.acquire_claim(&old_input, &f.request()).unwrap();
    store
        .release_claim(project(), &fence, &f.request())
        .unwrap();
    let before = database_records(&store);
    let mut input = old_input.clone();
    input.id = provenant_core::ClaimId::from_bytes([22; 16]);
    input.scope = WorkScope::Issue {
        extra: Default::default(),
        key: "prune-trigger".into(),
    };
    store.db.execute_batch("CREATE TRIGGER refuse_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'event failure'); END;").unwrap();
    assert!(matches!(
        store.acquire_claim(&input, &f.request()),
        Err(Error::Sqlite(_))
    ));
    assert_eq!(database_records(&store), before);
    store.db.execute_batch("DROP TRIGGER refuse_event").unwrap();
    store.acquire_claim(&input, &f.request()).unwrap();
    let paths: i64 = store
        .db
        .query_row("SELECT count(*) FROM claim_paths", [], |r| r.get(0))
        .unwrap();
    assert_eq!(paths, 0);
    let mut collision = old_input.clone();
    collision.scope = WorkScope::Issue {
        extra: Default::default(),
        key: "cannot-reuse-pruned-id".into(),
    };
    assert!(matches!(
        store.acquire_claim(&collision, &f.request()),
        Err(Error::Conflict)
    ));
    let next = store.acquire_claim(&old_input, &f.request()).unwrap();
    assert_eq!(next.generation, fence.generation + 1);
}
