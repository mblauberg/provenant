use super::*;
use provenant_core::OperationId;

#[test]
fn receipt_failure_rolls_back_effects_and_events_before_a_safe_retry() {
    let f = Fixture::new();
    let store = f.open();
    let request = f.request();
    let id = OperationId::parse("atomic-1").unwrap();
    let message = message(&f, 9);
    store.db.execute_batch("CREATE TRIGGER reject_receipt BEFORE INSERT ON coordination_operations BEGIN SELECT RAISE(ABORT,'receipt refused'); END;").unwrap();
    let action = || {
        store.send(&message, &request)?;
        Ok::<_, Error>(message.id)
    };
    assert!(
        store
            .coordination_operation(project(), actor(2), &id, &"send", &request, action)
            .is_err()
    );
    assert!(
        store
            .read_mail(project(), actor(3), Some(0), 100, &request)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        store
            .events(project(), 0, 100, &request)
            .unwrap()
            .items
            .is_empty()
    );
    store
        .db
        .execute_batch("DROP TRIGGER reject_receipt")
        .unwrap();
    assert_eq!(
        store
            .coordination_operation(project(), actor(2), &id, &"send", &request, action)
            .unwrap(),
        message.id
    );
    assert_eq!(
        store
            .read_mail(project(), actor(3), Some(0), 100, &request)
            .unwrap()
            .items
            .len(),
        1
    );
}

#[test]
fn concurrent_duplicate_operations_apply_once_and_share_the_committed_receipt() {
    let f = Fixture::new();
    f.open();
    let barrier = Arc::new(Barrier::new(4));
    let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let message = message(&f, 8);
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..4 {
            let root = f.root.clone();
            let barrier = barrier.clone();
            let calls = calls.clone();
            let message = message.clone();
            workers.push(scope.spawn(move || {
                let clock = TestClock::new(message.created_at, MonoTime::from_micros(0));
                let cancel = CancelToken::new();
                let request = Request {
                    clock: &clock,
                    budget: WaitBudget::cli(clock.mono_now(), 5).unwrap(),
                    cancel: &cancel,
                    backoff: &ThreadWait,
                };
                let store = Store::open(&root, &request).unwrap();
                barrier.wait();
                store
                    .coordination_operation(
                        project(),
                        actor(2),
                        &OperationId::parse("parallel-1").unwrap(),
                        &"send",
                        &request,
                        || {
                            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            store.send(&message, &request)?;
                            Ok::<_, Error>(message.id)
                        },
                    )
                    .unwrap()
            }));
        }
        for worker in workers {
            assert_eq!(worker.join().unwrap(), message.id);
        }
    });
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
