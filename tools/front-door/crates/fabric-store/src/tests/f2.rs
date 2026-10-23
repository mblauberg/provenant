use super::*;

fn work(f: &Fixture, who: u8) -> ClaimRequest {
    ClaimRequest {
        id: provenant_core::ClaimId::from_bytes([9; 16]),
        project: project(),
        actor: actor(who),
        token: [who; 16],
        lease: DurationMicros::from_seconds(1).unwrap(),
        scope: WorkScope::Paths {
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

fn landing(who: u8) -> LandingRequest {
    LandingRequest {
        id: provenant_core::ClaimId::from_bytes([10; 16]),
        project: project(),
        actor: actor(who),
        token: [who; 16],
        reference: "refs/heads/main".into(),
        expected: provenant_core::GitOid::parse(&"1".repeat(40)).unwrap(),
        candidate: provenant_core::GitOid::parse(&"2".repeat(40)).unwrap(),
        lease: DurationMicros::from_seconds(1).unwrap(),
    }
}
#[test]
fn pushing_survives_expiry_and_unknown_until_reconciliation() {
    let mut f = Fixture::new();
    let store = f.open();
    let input = landing(3);
    let guard = store
        .lock_landing(project(), &input.reference, &f.request())
        .unwrap();
    let fence = store.acquire_landing(&guard, &input, &f.request()).unwrap();
    let id = provenant_core::OperationId::parse("push-1").unwrap();
    store
        .prepare_push(&guard, &fence, &id, &f.request())
        .unwrap();
    store
        .change_push(&guard, &fence, &id, PushAction::Begin, &f.request())
        .unwrap();
    f.clock
        .advance(DurationMicros::from_seconds(1).unwrap())
        .unwrap();
    assert!(matches!(
        store.acquire_landing(&guard, &landing(4), &f.request()),
        Err(Error::PushUnresolved { .. })
    ));
    assert!(matches!(
        store.release_landing(&guard, &fence, &f.request()),
        Err(Error::PushUnresolved { .. })
    ));
    store
        .change_push(&guard, &fence, &id, PushAction::Uncertain, &f.request())
        .unwrap();
    assert!(matches!(
        store.acquire_landing(&guard, &landing(4), &f.request()),
        Err(Error::PushUnresolved { .. })
    ));
    let result = store
        .change_push(
            &guard,
            &fence,
            &id,
            PushAction::Reconcile {
                remote: Some(input.candidate),
            },
            &f.request(),
        )
        .unwrap();
    assert_eq!(result.phase, PushPhase::Committed);
    store.release_landing(&guard, &fence, &f.request()).unwrap();
    assert_eq!(
        store
            .acquire_landing(&guard, &landing(4), &f.request())
            .unwrap()
            .generation,
        fence.generation + 1
    );
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
    input.scope = WorkScope::Worktree { path: a.clone() };
    let fence = store.acquire_claim(&input, &f.request()).unwrap();
    input.id = provenant_core::ClaimId::from_bytes([11; 16]);
    input.scope = WorkScope::Paths { paths: vec![ab] };
    store.acquire_claim(&input, &f.request()).unwrap();
    input.scope = WorkScope::Paths {
        paths: vec![a.join("child")],
    };
    assert!(matches!(
        store.acquire_claim(&input, &f.request()),
        Err(Error::Overlap { .. })
    ));
    let link = f.temp.path().join("alias");
    std::os::unix::fs::symlink(&a, &link).unwrap();
    input.scope = WorkScope::Paths { paths: vec![link] };
    assert!(matches!(
        store.acquire_claim(&input, &f.request()),
        Err(Error::Overlap { .. })
    ));
    input.scope = WorkScope::Paths {
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
        key: "issue-42".into(),
    };
    let issue = store.acquire_claim(&input, &f.request()).unwrap();
    input.id = provenant_core::ClaimId::from_bytes([13; 16]);
    assert!(
        matches!(store.acquire_claim(&input,&f.request()),Err(Error::Overlap {claim}) if claim==issue.id)
    );
    input.project = ProjectId::from_bytes([2; 16]);
    store.acquire_claim(&input, &f.request()).unwrap();
    let listed = store.claims(project(), None, 100, &f.request()).unwrap();
    assert_eq!(listed.items.len(), 3);
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
fn landing_os_lock_serialises_connections_and_ref_and_store_are_bound() {
    let f = Fixture::new();
    let store = f.open();
    let input = landing(3);
    let ready = Arc::new(Barrier::new(2));
    let proceed = Arc::new(Barrier::new(2));
    std::thread::scope(|s| {
        let ready2 = ready.clone();
        let proceed2 = proceed.clone();
        let f = &f;
        let input = &input;
        let contender = s.spawn(move || {
            let second = f.open();
            ready2.wait();
            proceed2.wait();
            let guard = second
                .lock_landing(project(), &input.reference, &f.request())
                .unwrap();
            second.acquire_landing(&guard, &landing(4), &f.request())
        });
        ready.wait();
        let guard = store
            .lock_landing(project(), &input.reference, &f.request())
            .unwrap();
        let fence = store.acquire_landing(&guard, input, &f.request()).unwrap();
        let mut req = f.request();
        req.budget = WaitBudget::cli(MonoTime::from_micros(0), 1).unwrap();
        let expired_clock = TestClock::new(f.now(), MonoTime::from_micros(1_000_000));
        req.clock = &expired_clock;
        assert!(
            store
                .lock_landing(project(), &input.reference, &req)
                .is_err()
        );
        let other = Fixture::new();
        let other_store = other.open();
        assert!(matches!(
            other_store.verify_landing(&guard, &fence, &other.request()),
            Err(Error::Invalid(_))
        ));
        let different = store
            .lock_landing(project(), "refs/heads/other", &f.request())
            .unwrap();
        assert!(matches!(
            store.verify_landing(&different, &fence, &f.request()),
            Err(Error::StaleFence)
        ));
        proceed.wait();
        drop(guard);
        assert!(matches!(contender.join().unwrap(), Err(Error::Conflict)));
    });
}

fn operation_id() -> provenant_core::OperationId {
    provenant_core::OperationId::parse("push-1").unwrap()
}
fn setup_phase(
    store: &Store,
    f: &Fixture,
    guard: &LandingGuard,
    fence: &ClaimFence,
    phase: PushPhase,
) {
    let id = operation_id();
    store.prepare_push(guard, fence, &id, &f.request()).unwrap();
    if phase == PushPhase::Prepared {
        return;
    }
    store
        .change_push(guard, fence, &id, PushAction::Begin, &f.request())
        .unwrap();
    let action = match phase {
        PushPhase::Prepared | PushPhase::Pushing => return,
        PushPhase::Unknown => PushAction::Uncertain,
        PushPhase::Committed => PushAction::Confirmed,
        PushPhase::Retryable => PushAction::Reconcile {
            remote: Some(landing(3).expected),
        },
        PushPhase::Conflict => PushAction::Reconcile {
            remote: Some(provenant_core::GitOid::parse(&"3".repeat(40)).unwrap()),
        },
    };
    store
        .change_push(guard, fence, &id, action, &f.request())
        .unwrap();
}
#[test]
fn push_transition_matrix_and_frozen_operation_identity() {
    use PushPhase::*;
    for from in [Prepared, Pushing, Unknown, Committed, Retryable, Conflict] {
        for action in [
            PushAction::Begin,
            PushAction::Uncertain,
            PushAction::Confirmed,
            PushAction::Reconcile { remote: None },
            PushAction::Reconcile {
                remote: Some(landing(3).candidate),
            },
            PushAction::Reconcile {
                remote: Some(landing(3).expected),
            },
            PushAction::Reconcile {
                remote: Some(provenant_core::GitOid::parse(&"3".repeat(40)).unwrap()),
            },
        ] {
            let f = Fixture::new();
            let store = f.open();
            let input = landing(3);
            let guard = store
                .lock_landing(project(), &input.reference, &f.request())
                .unwrap();
            let fence = store.acquire_landing(&guard, &input, &f.request()).unwrap();
            setup_phase(&store, &f, &guard, &fence, from);
            let before = store
                .push_operation(project(), &operation_id(), &f.request())
                .unwrap();
            // Replaying preparation preserves all later phase/result evidence.
            assert_eq!(
                store
                    .prepare_push(&guard, &fence, &operation_id(), &f.request())
                    .unwrap(),
                before
            );
            let expected = match (from, action) {
                (Prepared | Retryable, PushAction::Begin) => Some(Pushing),
                (Pushing, PushAction::Uncertain) => Some(Unknown),
                (Pushing, PushAction::Confirmed) => Some(Committed),
                (Pushing | Unknown, PushAction::Reconcile { remote }) => Some(match remote {
                    None => Unknown,
                    Some(oid) if oid == input.candidate => Committed,
                    Some(oid) if oid == input.expected => Retryable,
                    Some(_) => Conflict,
                }),
                _ => None,
            };
            let result = store.change_push(&guard, &fence, &operation_id(), action, &f.request());
            if let Some(expected) = expected {
                let operation = result.unwrap();
                assert_eq!(operation.phase, expected, "{from:?} {action:?}");
                let observed = match action {
                    PushAction::Confirmed => Some(input.candidate),
                    PushAction::Reconcile { remote } => remote,
                    PushAction::Begin | PushAction::Uncertain => None,
                };
                assert_eq!(operation.observed_remote, observed, "{from:?} {action:?}");
            } else {
                assert!(
                    matches!(result, Err(Error::PushTransition { .. })),
                    "{from:?} {action:?}"
                );
                assert_eq!(
                    store
                        .push_operation(project(), &operation_id(), &f.request())
                        .unwrap(),
                    before
                );
            }
        }
    }
    let mut f = Fixture::new();
    let store = f.open();
    let input = landing(3);
    let guard = store
        .lock_landing(project(), &input.reference, &f.request())
        .unwrap();
    let first = store.acquire_landing(&guard, &input, &f.request()).unwrap();
    store
        .prepare_push(&guard, &first, &operation_id(), &f.request())
        .unwrap();
    assert!(matches!(
        store.prepare_push(
            &guard,
            &first,
            &provenant_core::OperationId::parse("different").unwrap(),
            &f.request()
        ),
        Err(Error::Conflict)
    ));
    f.clock
        .advance(DurationMicros::from_seconds(1).unwrap())
        .unwrap();
    let second = store
        .acquire_landing(&guard, &landing(4), &f.request())
        .unwrap();
    assert!(matches!(
        store.prepare_push(&guard, &second, &operation_id(), &f.request()),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        store.release_landing(&guard, &first, &f.request()),
        Err(Error::StaleFence)
    ));
}

#[test]
fn expired_claim_renewal_is_refused_and_expired_landing_can_be_resolved_by_owner() {
    let mut f = Fixture::new();
    let store = f.open();
    let input = landing(3);
    let work = store.acquire_claim(&work(&f, 3), &f.request()).unwrap();
    let guard = store
        .lock_landing(project(), &input.reference, &f.request())
        .unwrap();
    let fence = store.acquire_landing(&guard, &input, &f.request()).unwrap();
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
    assert!(matches!(
        store.verify_landing(&guard, &fence, &f.request()),
        Err(Error::StaleFence)
    ));
    store
        .renew_landing(
            &guard,
            &fence,
            DurationMicros::from_seconds(1).unwrap(),
            &f.request(),
        )
        .unwrap();
    assert_eq!(
        store
            .verify_landing(&guard, &fence, &f.request())
            .unwrap()
            .expires_at,
        Some(
            f.now()
                .checked_add(DurationMicros::from_seconds(1).unwrap())
                .unwrap()
        )
    );
    store.release_claim(project(), &work, &f.request()).unwrap();
    store.release_landing(&guard, &fence, &f.request()).unwrap();
}

#[test]
fn task_renew_and_verify_require_the_live_current_fence() {
    let mut f = Fixture::new();
    let store = f.open();
    store.create_task(&task(&f), &f.request()).unwrap();
    let fence = store.claim_task(&claim(&f, 3), &f.request()).unwrap();
    f.clock
        .advance(DurationMicros::from_micros(999_999))
        .unwrap();
    store
        .renew_task(
            project(),
            &fence,
            DurationMicros::from_seconds(1).unwrap(),
            &f.request(),
        )
        .unwrap();
    f.clock.advance(DurationMicros::from_micros(1)).unwrap();
    store.verify_task(project(), &fence, &f.request()).unwrap();
    f.clock
        .advance(DurationMicros::from_micros(999_999))
        .unwrap();
    assert!(matches!(
        store.verify_task(project(), &fence, &f.request()),
        Err(Error::StaleFence)
    ));
    assert!(matches!(
        store.renew_task(
            project(),
            &fence,
            DurationMicros::from_seconds(1).unwrap(),
            &f.request()
        ),
        Err(Error::StaleFence)
    ));
}

// Each stage runs with real SQLite allocation failure or a second connection's
// RESERVED lock. Successful writes after the fault prove the connection recovers.
#[derive(Debug, Clone, Copy)]
enum Mutation {
    WorkAcquire,
    WorkRenew,
    WorkRelease,
    LandingAcquire,
    LandingRenew,
    LandingRelease,
    Prepare,
    Begin,
    Uncertain,
    Confirm,
    Reconcile,
    TaskRenew,
    TaskClose,
}
fn exercise_fault(stage: Mutation, full: bool) {
    let f = Fixture::new();
    let store = f.open();
    let input = landing(3);
    let guard = store
        .lock_landing(project(), &input.reference, &f.request())
        .unwrap();
    let work_fence = if matches!(stage, Mutation::WorkAcquire) {
        None
    } else {
        Some(store.acquire_claim(&work(&f, 3), &f.request()).unwrap())
    };
    let landing_fence = if matches!(stage, Mutation::LandingAcquire) {
        None
    } else {
        Some(store.acquire_landing(&guard, &input, &f.request()).unwrap())
    };
    if matches!(
        stage,
        Mutation::Begin | Mutation::Uncertain | Mutation::Confirm | Mutation::Reconcile
    ) {
        store
            .prepare_push(
                &guard,
                landing_fence.as_ref().unwrap(),
                &operation_id(),
                &f.request(),
            )
            .unwrap();
    }
    if matches!(
        stage,
        Mutation::Uncertain | Mutation::Confirm | Mutation::Reconcile
    ) {
        store
            .change_push(
                &guard,
                landing_fence.as_ref().unwrap(),
                &operation_id(),
                PushAction::Begin,
                &f.request(),
            )
            .unwrap();
    }
    store.create_task(&task(&f), &f.request()).unwrap();
    let task_fence = store.claim_task(&claim(&f, 3), &f.request()).unwrap();
    let action = |req: &Request<'_>| -> Result<()> {
        match stage {
            Mutation::WorkAcquire => store.acquire_claim(&work(&f, 3), req).map(|_| ()),
            Mutation::WorkRenew => store.renew_claim(
                project(),
                work_fence.as_ref().unwrap(),
                DurationMicros::from_seconds(2).unwrap(),
                req,
            ),
            Mutation::WorkRelease => {
                store.release_claim(project(), work_fence.as_ref().unwrap(), req)
            }
            Mutation::LandingAcquire => store.acquire_landing(&guard, &input, req).map(|_| ()),
            Mutation::LandingRenew => store.renew_landing(
                &guard,
                landing_fence.as_ref().unwrap(),
                DurationMicros::from_seconds(2).unwrap(),
                req,
            ),
            Mutation::LandingRelease => {
                store.release_landing(&guard, landing_fence.as_ref().unwrap(), req)
            }
            Mutation::Prepare => store
                .prepare_push(
                    &guard,
                    landing_fence.as_ref().unwrap(),
                    &operation_id(),
                    req,
                )
                .map(|_| ()),
            Mutation::Begin | Mutation::Uncertain | Mutation::Confirm | Mutation::Reconcile => {
                store
                    .change_push(
                        &guard,
                        landing_fence.as_ref().unwrap(),
                        &operation_id(),
                        match stage {
                            Mutation::Begin => PushAction::Begin,
                            Mutation::Uncertain => PushAction::Uncertain,
                            Mutation::Confirm => PushAction::Confirmed,
                            _ => PushAction::Reconcile {
                                remote: Some(input.candidate),
                            },
                        },
                        req,
                    )
                    .map(|_| ())
            }
            Mutation::TaskRenew => store.renew_task(
                project(),
                &task_fence,
                DurationMicros::from_seconds(2).unwrap(),
                req,
            ),
            Mutation::TaskClose => store.finish_task(project(), &task_fence, TaskState::Done, req),
        }
    };
    let before = database_records(&store);
    let holder = f.open();
    let clock = AdvancingClock(std::cell::RefCell::new(f.clock.clone()));
    let mut req = f.request();
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
        req.clock = &clock;
        req.backoff = &clock;
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
    for table in ["claims", "tasks", "operations", "events"] {
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
    result
}
#[test]
fn full_and_busy_roll_back_every_new_mutation_and_its_event() {
    for full in [false, true] {
        for stage in [
            Mutation::WorkAcquire,
            Mutation::WorkRenew,
            Mutation::WorkRelease,
            Mutation::LandingAcquire,
            Mutation::LandingRenew,
            Mutation::LandingRelease,
            Mutation::Prepare,
            Mutation::Begin,
            Mutation::Uncertain,
            Mutation::Confirm,
            Mutation::Reconcile,
            Mutation::TaskRenew,
            Mutation::TaskClose,
        ] {
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
        WorkScope::Paths { paths: vec![] },
        WorkScope::Paths {
            paths: vec!["relative".into()],
        },
        WorkScope::Issue { key: String::new() },
    ] {
        let mut input = work(&f, 3);
        input.scope = scope;
        assert!(matches!(
            store.acquire_claim(&input, &f.request()),
            Err(Error::Invalid(_))
        ));
    }
    for branch in [
        "main",
        "refs/heads/../main",
        "refs/heads/a.lock",
        "refs/heads/a:b",
        "refs/heads/a\n",
        "refs/heads/.a",
        "refs/heads/a@{b",
    ] {
        assert!(matches!(
            store.lock_landing(project(), branch, &f.request()),
            Err(Error::Invalid(_))
        ));
    }
    let input = landing(3);
    let guard = store
        .lock_landing(project(), &input.reference, &f.request())
        .unwrap();
    let fence = store.acquire_landing(&guard, &input, &f.request()).unwrap();
    let before = database_records(&store);
    for wrong in [
        ClaimFence {
            token: [0; 16],
            ..fence.clone()
        },
        ClaimFence {
            generation: 99,
            ..fence.clone()
        },
        ClaimFence {
            actor: actor(9),
            ..fence.clone()
        },
    ] {
        assert!(matches!(
            store.renew_landing(&guard, &wrong, input.lease, &f.request()),
            Err(Error::StaleFence)
        ));
        assert!(matches!(
            store.release_landing(&guard, &wrong, &f.request()),
            Err(Error::StaleFence)
        ));
        assert!(matches!(
            store.prepare_push(&guard, &wrong, &operation_id(), &f.request()),
            Err(Error::StaleFence)
        ));
    }
    f.cancel.cancel();
    assert!(matches!(
        store.release_landing(&guard, &fence, &f.request()),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        store.acquire_claim(&work(&f, 3), &f.request()),
        Err(Error::Cancelled)
    ));
    assert_eq!(database_records(&store), before);
}

#[test]
fn reopened_store_keeps_unknown_push_pinned_after_owner_lock_closes() {
    let mut f = Fixture::new();
    let store = f.open();
    let input = landing(3);
    let guard = store
        .lock_landing(project(), &input.reference, &f.request())
        .unwrap();
    let fence = store.acquire_landing(&guard, &input, &f.request()).unwrap();
    setup_phase(&store, &f, &guard, &fence, PushPhase::Unknown);
    drop(guard);
    drop(store);
    f.clock
        .advance(DurationMicros::from_seconds(1).unwrap())
        .unwrap();
    let store = f.open();
    let guard = store
        .lock_landing(project(), &input.reference, &f.request())
        .unwrap();
    assert!(matches!(
        store.acquire_landing(&guard, &landing(4), &f.request()),
        Err(Error::PushUnresolved { .. })
    ));
    let unknown = store
        .change_push(
            &guard,
            &fence,
            &operation_id(),
            PushAction::Reconcile { remote: None },
            &f.request(),
        )
        .unwrap();
    assert_eq!(unknown.phase, PushPhase::Unknown);
    assert!(matches!(
        store.release_landing(&guard, &fence, &f.request()),
        Err(Error::PushUnresolved { .. })
    ));
    store
        .change_push(
            &guard,
            &fence,
            &operation_id(),
            PushAction::Reconcile {
                remote: Some(input.expected),
            },
            &f.request(),
        )
        .unwrap();
    assert!(matches!(
        store.change_push(
            &guard,
            &fence,
            &operation_id(),
            PushAction::Begin,
            &f.request()
        ),
        Err(Error::StaleFence)
    ));
    store
        .renew_landing(&guard, &fence, input.lease, &f.request())
        .unwrap();
    store
        .change_push(
            &guard,
            &fence,
            &operation_id(),
            PushAction::Begin,
            &f.request(),
        )
        .unwrap();
    store
        .change_push(
            &guard,
            &fence,
            &operation_id(),
            PushAction::Confirmed,
            &f.request(),
        )
        .unwrap();
}

#[test]
fn expired_task_close_and_takeover_are_serialised_across_connections() {
    for state in [TaskState::Open, TaskState::Done, TaskState::Cancelled] {
        let mut f = Fixture::new();
        let store = f.open();
        store.create_task(&task(&f), &f.request()).unwrap();
        let fence = store.claim_task(&claim(&f, 3), &f.request()).unwrap();
        f.clock
            .advance(DurationMicros::from_seconds(1).unwrap())
            .unwrap();
        let barrier = Arc::new(Barrier::new(2));
        std::thread::scope(|s| {
            let f = &f;
            let b = barrier.clone();
            let takeover = s.spawn(move || {
                let store = f.open();
                b.wait();
                store.claim_task(&claim(f, 4), &f.request())
            });
            barrier.wait();
            let closed = if state == TaskState::Open {
                store.release_task(project(), &fence, &f.request())
            } else {
                store.finish_task(project(), &fence, state, &f.request())
            };
            let taken = takeover.join().unwrap();
            let actual = store.task(project(), fence.task, &f.request()).unwrap();
            match (closed, taken) {
                (Err(Error::StaleFence), Ok(next)) | (Ok(()), Ok(next))
                    if state == TaskState::Open || actual.state == TaskState::Active =>
                {
                    assert_eq!(actual.owner, Some(actor(4)));
                    assert_eq!(actual.generation, next.generation);
                    assert_eq!(next.generation, 2);
                }
                (Ok(()), Err(Error::Conflict)) => {
                    assert_eq!(actual.state, state);
                    assert_eq!(actual.owner, None);
                }
                other => panic!("invalid close/takeover outcomes: {other:?}"),
            }
        });
    }
}

#[test]
fn non_utf8_paths_and_exhausted_generations_return_typed_errors() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    let store = f.open();
    let path = f.temp.path().join(std::ffi::OsString::from_vec(vec![255]));
    let mut input = work(&f, 3);
    input.scope = WorkScope::Paths { paths: vec![path] };
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
        .execute("UPDATE claims SET generation=9223372036854775807", [])
        .unwrap();
    assert!(matches!(
        store.acquire_claim(&input, &f.request()),
        Err(Error::Invalid(_))
    ));
}
