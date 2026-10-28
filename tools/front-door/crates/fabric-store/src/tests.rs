use super::*;
use provenant_core::{
    CancelToken, Clock, DurationMicros, MessageId, MonoTime, PathResolver, RootInputs, StateRoot,
    TaskId, TestClock, TimeError, WaitBudget,
};
use provenant_records::{ArtifactRef, PathWire, RecordHeader};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
};
struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
        if path.exists() {
            path.canonicalize()
        } else {
            Ok(self
                .resolve_existing_ancestor(path.parent().ok_or(std::io::ErrorKind::InvalidInput)?)?
                .join(path.file_name().ok_or(std::io::ErrorKind::InvalidInput)?))
        }
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    root: StateRoot,
    clock: TestClock,
    cancel: CancelToken,
}
impl Fixture {
    fn new() -> Self {
        let temp = crate::test_support::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        Self {
            temp,
            root: inputs.resolve(&Resolver).unwrap().state,
            clock: TestClock::new(
                WallTime::from_micros(2_000_000_000_000_000).unwrap(),
                MonoTime::from_micros(0),
            ),
            cancel: CancelToken::new(),
        }
    }
    fn request(&self) -> Request<'_> {
        Request {
            clock: &self.clock,
            budget: WaitBudget::cli(self.clock.mono_now(), 1).unwrap(),
            cancel: &self.cancel,
            backoff: &ThreadWait,
        }
    }
    fn open(&self) -> Store {
        Store::open(&self.root, &self.request()).unwrap()
    }
    fn path(&self) -> PathBuf {
        self.root.as_path().join("fabric/fabric.sqlite3")
    }
    fn now(&self) -> WallTime {
        self.clock.wall_now().unwrap()
    }
}
fn project() -> ProjectId {
    ProjectId::from_bytes([1; 16])
}
fn actor(n: u8) -> ActorId {
    ActorId::from_bytes([n; 16])
}
fn message(f: &Fixture, n: u8) -> Message {
    Message {
        header: RecordHeader::V1,
        project: project(),
        id: MessageId::from_bytes([n; 16]),
        sender: actor(2),
        recipients: vec![actor(3), actor(4)],
        kind: MessageKind::Request,
        body: "Review this".into(),
        artifacts: vec![ArtifactRef {
            path: PathWire::from_relative(
                &provenant_core::RelativePath::parse("result.txt").unwrap(),
            )
            .unwrap(),
            digest: None,
        }],
        reply_to: None,
        task: None,
        conversation: None,
        created_at: f.now(),
    }
}
fn task(f: &Fixture) -> Task {
    Task {
        header: RecordHeader::V1,
        project: project(),
        id: TaskId::from_bytes([6; 16]),
        objective: "Build F1".into(),
        note: None,
        state: TaskState::Open,
        owner: None,
        generation: 0,
        expires_at: None,
        created_at: f.now(),
        updated_at: f.now(),
    }
}
fn claim(f: &Fixture, who: u8) -> TaskClaim {
    TaskClaim {
        project: project(),
        task: task(f).id,
        actor: actor(who),
        token: [who; 16],
        lease: DurationMicros::from_seconds(1).unwrap(),
    }
}
#[test]
fn immutable_mail_and_recipient_scoped_monotonic_durable_cursors() {
    let f = Fixture::new();
    let store = f.open();
    let req = f.request();
    let m = message(&f, 5);
    store.send(&m, &req).unwrap();
    store.send(&m, &req).unwrap();
    let rows = store
        .read_mail(project(), actor(3), Some(0), 100, &req)
        .unwrap()
        .items;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].message, m);
    assert_eq!(
        store
            .read_mail(project(), actor(3), Some(0), 100, &req)
            .unwrap()
            .items,
        rows
    );
    let seq = rows[0].seq;
    assert!(
        store
            .mark_mail_read(project(), actor(3), seq + 100, &req)
            .is_err()
    );
    store
        .mark_mail_read(project(), actor(3), seq, &req)
        .unwrap();
    store.mark_mail_read(project(), actor(3), 0, &req).unwrap();
    assert!(
        store
            .read_mail(project(), actor(3), None, 100, &req)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        store
            .read_mail(project(), actor(4), None, 100, &req)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        store
            .read_mail(project(), actor(3), Some(0), 100, &req)
            .unwrap()
            .items
            .len(),
        1
    );
    let mut changed = m;
    changed.body = "different".into();
    assert!(matches!(store.send(&changed, &req), Err(Error::Conflict)));
    drop(store);
    let store = f.open();
    assert!(
        store
            .read_mail(project(), actor(3), None, 100, &req)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        store
            .events(project(), 0, 100, &req)
            .unwrap()
            .items
            .iter()
            .filter(|e| e.record.kind == EventKind::MailSent)
            .count(),
        1
    );
}
#[test]
fn task_fences_survive_expiry_and_release_and_cannot_be_bypassed() {
    let mut f = Fixture::new();
    let store = f.open();
    let t = task(&f);
    store.create_task(&t, &f.request()).unwrap();
    let first = store.claim_task(&claim(&f, 3), &f.request()).unwrap();
    assert!(matches!(
        store.claim_task(&claim(&f, 4), &f.request()),
        Err(Error::Conflict)
    ));
    let mut wrong = first.clone();
    wrong.token = [9; 16];
    assert!(matches!(
        store.finish_task(project(), &wrong, TaskState::Done, &f.request()),
        Err(Error::StaleFence)
    ));
    f.clock
        .advance(DurationMicros::from_seconds(1).unwrap())
        .unwrap();
    let next = store.claim_task(&claim(&f, 4), &f.request()).unwrap();
    assert_eq!(next.generation, first.generation + 1);
    assert!(matches!(
        store.release_task(project(), &first, &f.request()),
        Err(Error::StaleFence)
    ));
    store.release_task(project(), &next, &f.request()).unwrap();
    let third = store.claim_task(&claim(&f, 4), &f.request()).unwrap();
    assert_eq!(third.generation, next.generation + 1);
    store
        .update_task(
            project(),
            &third,
            "New objective",
            Some("blocker"),
            &f.request(),
        )
        .unwrap();
    store
        .finish_task(project(), &third, TaskState::Cancelled, &f.request())
        .unwrap();
    assert!(matches!(
        store.claim_task(&claim(&f, 3), &f.request()),
        Err(Error::Conflict)
    ));
}
#[test]
fn barrier_race_between_two_connections_has_one_task_owner() {
    let f = Fixture::new();
    f.open().create_task(&task(&f), &f.request()).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    std::thread::scope(|s| {
        let launch = |who| {
            let barrier = barrier.clone();
            let f = &f;
            s.spawn(move || {
                let store = f.open();
                barrier.wait();
                store.claim_task(&claim(f, who), &f.request())
            })
        };
        let a = launch(3);
        let b = launch(4);
        let results = [a.join().unwrap(), b.join().unwrap()];
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(Error::Conflict)))
                .count(),
            1
        );
    });
}
#[test]
fn sqlite_full_rolls_back_messages_deliveries_and_events() {
    let f = Fixture::new();
    let store = f.open();
    store
        .db
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let pages: i64 = store
        .db
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    store
        .db
        .pragma_update(None, "max_page_count", pages)
        .unwrap();
    let mut m = message(&f, 5);
    m.body = "x".repeat(MAX_BODY_BYTES);
    assert!(
        matches!(store.send(&m,&f.request()),Err(Error::Sqlite(rusqlite::Error::SqliteFailure(code,_))) if code.code==rusqlite::ErrorCode::DiskFull)
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        store
            .events(project(), 0, 100, &f.request())
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        store
            .read_mail(project(), actor(3), Some(0), 100, &f.request())
            .unwrap()
            .items
            .is_empty()
    );
}
#[test]
fn future_schema_refuses_without_journal_mutation() {
    let f = Fixture::new();
    drop(f.open());
    let db = rusqlite::Connection::open(f.path()).unwrap();
    db.pragma_update(None, "user_version", 99).unwrap();
    db.pragma_update(None, "journal_mode", "DELETE").unwrap();
    drop(db);
    assert!(matches!(
        Store::open(&f.root, &f.request()),
        Err(Error::SchemaVersion(99))
    ));
    let db = rusqlite::Connection::open(f.path()).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
}

#[test]
fn event_failure_rolls_back_task_and_mail_mutations() {
    let f = Fixture::new();
    let store = f.open();
    store.db.execute_batch("CREATE TRIGGER fail_events BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'injected event failure'); END").unwrap();
    assert!(store.create_task(&task(&f), &f.request()).is_err());
    assert!(store.send(&message(&f, 5), &f.request()).is_err());
    for table in ["tasks", "messages", "deliveries", "events"] {
        assert_eq!(
            store
                .db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    store.db.execute_batch("DROP TRIGGER fail_events").unwrap();
    store.create_task(&task(&f), &f.request()).unwrap();
    let fence = store.claim_task(&claim(&f, 3), &f.request()).unwrap();
    store.db.execute_batch("CREATE TRIGGER fail_events BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'injected event failure'); END").unwrap();
    assert!(
        store
            .finish_task(project(), &fence, TaskState::Done, &f.request())
            .is_err()
    );
    assert_eq!(
        store
            .task(project(), fence.task, &f.request())
            .unwrap()
            .state,
        TaskState::Active
    );
}
#[test]
fn event_replay_deduplication_and_durable_cursor() {
    let f = Fixture::new();
    let store = f.open();
    let note = event(
        project(),
        Some(actor(3)),
        EventKind::Note,
        None,
        "a note".into(),
        f.now(),
    );
    let seq = store.note(&note, Some("note-1"), &f.request()).unwrap();
    assert_eq!(
        seq,
        store.note(&note, Some("note-1"), &f.request()).unwrap()
    );
    let mut conflict = note.clone();
    conflict.detail = "different".into();
    assert!(matches!(
        store.note(&conflict, Some("note-1"), &f.request()),
        Err(Error::Conflict)
    ));
    assert!(
        store
            .mark_events_read(project(), actor(3), seq + 1, &f.request())
            .is_err()
    );
    store
        .mark_events_read(project(), actor(3), seq, &f.request())
        .unwrap();
    store
        .mark_events_read(project(), actor(3), 0, &f.request())
        .unwrap();
    drop(store);
    let store = f.open();
    assert_eq!(
        store
            .event_cursor(project(), actor(3), &f.request())
            .unwrap(),
        seq
    );
    assert_eq!(
        store
            .event_cursor(project(), actor(4), &f.request())
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .events(project(), 0, 1, &f.request())
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(
        store
            .events(ProjectId::from_bytes([9; 16]), 0, 1, &f.request())
            .unwrap()
            .items
            .is_empty()
    );
}
struct BusyHandshake {
    entered: Arc<Barrier>,
    released: Arc<Barrier>,
    first: std::sync::atomic::AtomicBool,
}
impl RetryWait for BusyHandshake {
    fn wait(&self, _: DurationMicros, _: &CancelToken) {
        if !self.first.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.entered.wait();
            self.released.wait();
        }
    }
}
#[test]
fn busy_retries_after_reserved_lock_release_without_sleep_or_open_transaction() {
    let f = Fixture::new();
    let holder = f.open();
    let ready = Arc::new(Barrier::new(2));
    let start = Arc::new(Barrier::new(2));
    let entered = Arc::new(Barrier::new(2));
    let released = Arc::new(Barrier::new(2));
    std::thread::scope(|s| {
        let f = &f;
        let ready2 = ready.clone();
        let start2 = start.clone();
        let entered2 = entered.clone();
        let released2 = released.clone();
        let worker = s.spawn(move || {
            let store = f.open();
            ready2.wait();
            start2.wait();
            let wait = BusyHandshake {
                entered: entered2,
                released: released2,
                first: std::sync::atomic::AtomicBool::new(false),
            };
            let mut request = f.request();
            request.backoff = &wait;
            store.send(&message(f, 5), &request)
        });
        ready.wait();
        holder.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        start.wait();
        entered.wait();
        holder.db.execute_batch("ROLLBACK").unwrap();
        released.wait();
        worker.join().unwrap().unwrap();
    });
    assert_eq!(
        holder
            .read_mail(project(), actor(3), Some(0), 100, &f.request())
            .unwrap()
            .items
            .len(),
        1
    );
}
struct AdvancingClock(std::cell::RefCell<TestClock>);
impl Clock for AdvancingClock {
    fn wall_now(&self) -> std::result::Result<WallTime, TimeError> {
        self.0.borrow().wall_now()
    }
    fn mono_now(&self) -> MonoTime {
        self.0.borrow().mono_now()
    }
    fn wait_until(&self, deadline: MonoTime) -> std::result::Result<(), TimeError> {
        self.0.borrow().wait_until(deadline)
    }
}
impl RetryWait for AdvancingClock {
    fn wait(&self, _: DurationMicros, _: &CancelToken) {
        self.0
            .borrow_mut()
            .advance(DurationMicros::from_seconds(1).unwrap())
            .unwrap();
    }
}
#[test]
fn busy_and_cancellation_are_bounded_and_leave_no_mutation() {
    let f = Fixture::new();
    let store = f.open();
    let holder = f.open();
    holder.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let clock = AdvancingClock(std::cell::RefCell::new(f.clock.clone()));
    let mut request = f.request();
    request.clock = &clock;
    request.backoff = &clock;
    assert!(matches!(
        store.send(&message(&f, 5), &request),
        Err(Error::Busy)
    ));
    holder.db.execute_batch("ROLLBACK").unwrap();
    f.cancel.cancel();
    assert!(matches!(
        store.send(&message(&f, 5), &f.request()),
        Err(Error::Cancelled)
    ));
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn cold_open_race_serialises_schema_setup() {
    let f = Fixture::new();
    let barrier = Arc::new(Barrier::new(2));
    std::thread::scope(|scope| {
        let start = || {
            let barrier = barrier.clone();
            let f = &f;
            scope.spawn(move || {
                barrier.wait();
                // The other opener can hold the write lock for seconds under
                // CI load. This test checks serialisation, not latency.
                let request = Request {
                    budget: WaitBudget::cli(f.clock.mono_now(), 30).unwrap(),
                    ..f.request()
                };
                let store = Store::open(&f.root, &request).unwrap();
                assert_eq!(
                    store
                        .db
                        .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                        .unwrap(),
                    crate::db::MIGRATIONS.len() as i64
                );
            })
        };
        let first = start();
        let second = start();
        first.join().unwrap();
        second.join().unwrap();
    });
}
#[cfg(unix)]
#[test]
fn state_symlinks_and_non_private_components_are_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for name in ["fabric.sqlite3", "fabric.sqlite3-wal", "fabric.sqlite3-shm"] {
        let f = Fixture::new();
        drop(f.open());
        let path = f.root.as_path().join("fabric").join(name);
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        let target = f.temp.path().join("target");
        std::fs::write(&target, b"untouched").unwrap();
        symlink(&target, &path).unwrap();
        assert!(matches!(
            Store::open(&f.root, &f.request()),
            Err(Error::UnsafePath(_))
        ));
        assert_eq!(std::fs::read(target).unwrap(), b"untouched");
    }
    let f = Fixture::new();
    drop(f.open());
    let path = f.root.as_path().join("fabric");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        Store::open(&f.root, &f.request()),
        Err(Error::UnsafePath(_))
    ));
}
#[test]
fn input_bounds_and_malformed_persisted_records_fail_without_panics() {
    let f = Fixture::new();
    let store = f.open();
    let mut m = message(&f, 5);
    m.body = "x".repeat(MAX_BODY_BYTES + 1);
    assert!(matches!(
        store.send(&m, &f.request()),
        Err(Error::Invalid(_))
    ));
    m = message(&f, 5);
    m.recipients.push(actor(3));
    assert!(store.send(&m, &f.request()).is_err());
    assert!(
        decode::<ArtifactRef>(br#"{"path":{"encoding":"utf8","path":"../escape"},"digest":null}"#)
            .is_err()
    );
    assert!(
        store
            .read_mail(project(), actor(3), Some(-1), 100, &f.request())
            .is_err()
    );
    assert!(store.events(project(), 0, 101, &f.request()).is_err());
    store.send(&message(&f, 5), &f.request()).unwrap();
    store
        .db
        .execute("UPDATE messages SET record=?", [b"{malformed".as_slice()])
        .unwrap();
    assert!(matches!(
        store.read_mail(project(), actor(3), Some(0), 100, &f.request()),
        Err(Error::Decode(_))
    ));
}

#[test]
fn delivery_sequences_are_local_to_each_project_and_never_wrap() {
    let f = Fixture::new();
    let store = f.open();
    let mut first = message(&f, 5);
    first.recipients = vec![actor(3)];
    store.send(&first, &f.request()).unwrap();
    let mut second = first.clone();
    second.project = ProjectId::from_bytes([9; 16]);
    store.send(&second, &f.request()).unwrap();
    assert_eq!(
        store
            .read_mail(first.project, actor(3), Some(0), 1, &f.request())
            .unwrap()
            .items[0]
            .seq,
        1
    );
    assert_eq!(
        store
            .read_mail(second.project, actor(3), Some(0), 1, &f.request())
            .unwrap()
            .items[0]
            .seq,
        1
    );
    store
        .db
        .execute(
            "UPDATE delivery_sequences SET seq=? WHERE project=?",
            params![i64::MAX, project().to_string()],
        )
        .unwrap();
    first.id = MessageId::from_bytes([7; 16]);
    assert!(matches!(
        store.send(&first, &f.request()),
        Err(Error::Invalid("delivery sequence exhausted"))
    ));
    assert_eq!(
        store
            .read_mail(project(), actor(3), Some(0), 100, &f.request())
            .unwrap()
            .items
            .len(),
        1
    );
}

#[test]
fn caller_timestamp_cannot_take_over_a_live_lease() {
    let f = Fixture::new();
    let store = f.open();
    let mut future_task = task(&f);
    future_task.created_at = f
        .now()
        .checked_add(DurationMicros::from_seconds(60).unwrap())
        .unwrap();
    future_task.updated_at = future_task.created_at;
    store.create_task(&future_task, &f.request()).unwrap();
    store.claim_task(&claim(&f, 3), &f.request()).unwrap();
    // The caller can future-date stored input, but cannot supply decision time.
    let mut future = claim(&f, 4);
    future.lease = DurationMicros::from_seconds(3600).unwrap();
    assert!(matches!(
        store.claim_task(&future, &f.request()),
        Err(Error::Conflict)
    ));
    let active = store.task(project(), task(&f).id, &f.request()).unwrap();
    assert_eq!(active.updated_at, f.now());
    assert_eq!(
        active.expires_at,
        Some(
            f.now()
                .checked_add(DurationMicros::from_seconds(1).unwrap())
                .unwrap()
        )
    );
}

#[test]
fn nonempty_unversioned_database_is_refused_before_mutation() {
    let f = Fixture::new();
    drop(f.open());
    let db = rusqlite::Connection::open(f.path()).unwrap();
    db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA user_version=0")
        .unwrap();
    drop(db);
    assert!(matches!(
        Store::open(&f.root, &f.request()),
        Err(Error::UnversionedDatabase)
    ));
    let db = rusqlite::Connection::open(f.path()).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
}

#[test]
fn mail_refuses_a_record_with_another_message_id() {
    let f = Fixture::new();
    let store = f.open();
    store.send(&message(&f, 5), &f.request()).unwrap();
    let forged = encode(&message(&f, 9)).unwrap();
    store
        .db
        .execute("UPDATE messages SET record=?", [forged])
        .unwrap();
    assert!(matches!(
        store.read_mail(project(), actor(3), Some(0), 100, &f.request()),
        Err(Error::Invalid("stored message identity"))
    ));
}

#[test]
fn byte_bounded_pages_report_truncation_and_resume_without_loss() {
    let f = Fixture::new();
    let store = f.open();
    let mut m = message(&f, 0);
    m.body = "x".repeat(MAX_BODY_BYTES);
    for n in 1..=20 {
        m.id = MessageId::from_bytes([n; 16]);
        store.send(&m, &f.request()).unwrap();
        let note = event(
            project(),
            None,
            EventKind::Note,
            None,
            m.body.clone(),
            f.now(),
        );
        store.note(&note, None, &f.request()).unwrap();
    }
    let mail = store
        .read_mail(project(), actor(3), Some(0), 100, &f.request())
        .unwrap();
    assert!(mail.truncated);
    let next = store
        .read_mail(
            project(),
            actor(3),
            mail.items.last().map(|r| r.seq),
            100,
            &f.request(),
        )
        .unwrap();
    assert!(!next.truncated);
    assert_eq!(mail.items.len() + next.items.len(), 20);
    let events = store.events(project(), 0, 100, &f.request()).unwrap();
    assert!(events.truncated);
    let next = store
        .events(
            project(),
            events.items.last().unwrap().seq,
            100,
            &f.request(),
        )
        .unwrap();
    assert!(!next.truncated);
    assert_eq!(events.items.len() + next.items.len(), 40);
}

#[test]
fn task_pages_report_byte_truncation_and_resume_without_loss() {
    let f = Fixture::new();
    let store = f.open();
    for n in 1..=20 {
        let mut record = task(&f);
        record.id = TaskId::from_bytes([n; 16]);
        record.objective = "x".repeat(MAX_BODY_BYTES);
        store.create_task(&record, &f.request()).unwrap();
    }

    let first = store
        .tasks(project(), None, None, 100, &f.request())
        .unwrap();
    let first = serde_json::to_value(first).unwrap();
    assert_eq!(
        first.get("truncated").and_then(serde_json::Value::as_bool),
        Some(true)
    );
    let first_items: Vec<Task> = serde_json::from_value(first["items"].clone()).unwrap();
    let next = store
        .tasks(
            project(),
            None,
            first_items.last().map(|record| record.id),
            100,
            &f.request(),
        )
        .unwrap();
    let next = serde_json::to_value(next).unwrap();
    assert_eq!(
        next.get("truncated").and_then(serde_json::Value::as_bool),
        Some(false)
    );
    let next_items: Vec<Task> = serde_json::from_value(next["items"].clone()).unwrap();
    assert_eq!(first_items.len() + next_items.len(), 20);
}

#[test]
fn store_clock_controls_fence_checks_mutation_times_and_acknowledgements() {
    let mut f = Fixture::new();
    let store = f.open();
    let mut t = task(&f);
    t.updated_at = f
        .now()
        .checked_add(DurationMicros::from_seconds(3600).unwrap())
        .unwrap();
    store.create_task(&t, &f.request()).unwrap();
    let fence = store.claim_task(&claim(&f, 3), &f.request()).unwrap();
    f.clock
        .advance(DurationMicros::from_micros(500_000))
        .unwrap();
    store
        .update_task(project(), &fence, "updated", None, &f.request())
        .unwrap();
    assert_eq!(
        store
            .task(project(), t.id, &f.request())
            .unwrap()
            .updated_at,
        f.now()
    );
    store.send(&message(&f, 5), &f.request()).unwrap();
    store
        .mark_mail_read(project(), actor(3), 1, &f.request())
        .unwrap();
    let events = store.events(project(), 0, 100, &f.request()).unwrap();
    assert_eq!(events.items.last().unwrap().record.at, f.now());
    f.clock
        .advance(DurationMicros::from_micros(500_000))
        .unwrap();
    assert!(matches!(
        store.update_task(project(), &fence, "too late", None, &f.request()),
        Err(Error::StaleFence)
    ));
    store.release_task(project(), &fence, &f.request()).unwrap();
    assert!(matches!(
        store.finish_task(project(), &fence, TaskState::Done, &f.request()),
        Err(Error::StaleFence)
    ));
}

// The TaskResult shape a coordination receipt stores, without the runtime crate.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum TaskReceipt {
    Created { task: Task },
}
const PEER_TASK: &str = "/result/data/result/task";
// A committed PeerResponse is plain serde_json in field order, not canonical.
fn peer_reply(task: &str) -> Vec<u8> {
    format!(
        r#"{{"version":1,"host":"{0}","project":"{0}","result":{{"result":"coordination","data":{{"domain":"task","result":{{"result":"created","task":{task}}}}}}}}}"#,
        "07".repeat(16)
    )
    .into_bytes()
}
// Every column that holds a serialised Task, Message or Event, as text.
fn stored_records(db: &rusqlite::Connection) -> Vec<String> {
    let mut result = Vec::new();
    for query in [
        "SELECT record FROM messages ORDER BY id",
        "SELECT record FROM tasks ORDER BY id",
        "SELECT record FROM events ORDER BY seq",
        "SELECT result FROM coordination_operations ORDER BY id",
        "SELECT response FROM peer_operations ORDER BY id",
    ] {
        let mut stmt = db.prepare(query).unwrap();
        result.extend(
            stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))
                .unwrap()
                .map(|r| String::from_utf8(r.unwrap()).unwrap()),
        );
    }
    result
}
// Versions 1 to 6 carry the imports table that migration 7 drops.
const IMPORTS_TABLE: &str = "CREATE TABLE imports(source TEXT PRIMARY KEY, \
     project TEXT NOT NULL, legacy_project TEXT NOT NULL, imported_at INTEGER NOT NULL, \
     tasks INTEGER NOT NULL, messages INTEGER NOT NULL, \
     skipped_rows BLOB NOT NULL DEFAULT X'7B7D') STRICT;";
fn downgrade_to_six(store: &Store, rows: &str) {
    store
        .db
        .execute_batch(&format!("{IMPORTS_TABLE} {rows} PRAGMA user_version=6;"))
        .unwrap();
}
// Seed each record kind that carried the removed members: a message, a task,
// its coordination receipt and a committed peer reply carrying that task.
fn seed_records(f: &Fixture, store: &Store) -> (Message, Task) {
    let request = f.request();
    let mut m = message(f, 5);
    m.body = "Review \"this\"\tnow é".into();
    let mut t = task(f);
    t.objective = "Ship \"F1\"\n é".into();
    store.send(&m, &request).unwrap();
    let id = provenant_core::OperationId::parse("create-1").unwrap();
    let receipt = store
        .coordination_operation(project(), actor(2), &id, &"create", &request, || {
            store.create_task(&t, &request)?;
            Ok::<_, Error>(TaskReceipt::Created { task: t.clone() })
        })
        .unwrap();
    assert_eq!(receipt, TaskReceipt::Created { task: t.clone() });
    let peer = provenant_core::OperationId::parse("peer-1").unwrap();
    let digest = provenant_core::Digest::from_bytes([7; 32]);
    store
        .prepare_peer(project(), actor(2), &peer, digest, b"{}", &request)
        .unwrap();
    store
        .peer_phase(project(), actor(2), &peer, PeerPhaseChange::Sent, &request)
        .unwrap();
    let response = peer_reply(&serde_json::to_string(&t).unwrap());
    store
        .peer_phase(
            project(),
            actor(2),
            &peer,
            PeerPhaseChange::Committed { response },
            &request,
        )
        .unwrap();
    (m, t)
}

// Rewrite each seeded row as a version 6 binary writes it, with the removed
// members null. Canonical records sort `legacy` and `legacy_output_path` into
// place; the peer reply's Task has `legacy` as its last field.
fn write_null_members(store: &Store, t: &Task) {
    let rows = [
        (
            "messages",
            "record",
            "",
            &["legacy", "legacy_output_path"][..],
        ),
        ("tasks", "record", "", &["legacy"][..]),
        (
            "coordination_operations",
            "result",
            "/task",
            &["legacy"][..],
        ),
    ];
    for (table, column, pointer, names) in rows {
        let bytes: Vec<u8> = store
            .db
            .query_row(&format!("SELECT {column} FROM {table}"), [], |r| r.get(0))
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let object = value.pointer_mut(pointer).unwrap().as_object_mut().unwrap();
        for name in names {
            object.insert((*name).into(), serde_json::Value::Null);
        }
        let old = encode(&value).unwrap();
        assert!(
            String::from_utf8(old.clone())
                .unwrap()
                .contains(r#""legacy":null,"#)
        );
        store
            .db
            .execute(&format!("UPDATE {table} SET {column}=?1"), [old])
            .unwrap();
    }
    let task_json = serde_json::to_string(t).unwrap();
    let old_reply = peer_reply(&format!(
        "{},\"legacy\":null}}",
        task_json.strip_suffix('}').unwrap()
    ));
    store
        .db
        .execute("UPDATE peer_operations SET response=?1", [old_reply])
        .unwrap();
}

#[test]
fn ordered_migrations_upgrade_existing_rows_and_do_not_reapply() {
    let f = Fixture::new();
    let store = f.open();
    let (m, t) = seed_records(&f, &store);
    let before = stored_records(&store.db);
    write_null_members(&store, &t);
    downgrade_to_six(&store, "");
    assert_ne!(stored_records(&store.db), before);
    drop(store);

    let store = f.open();
    assert_eq!(
        store
            .db
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        crate::db::MIGRATIONS.len() as i64
    );
    let imports: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='imports'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(imports, 0);
    // Each replay succeeds only if the stored bytes match a fresh encode, or
    // for receipts, only if the stored reply still decodes.
    let request = f.request();
    store.send(&m, &request).unwrap();
    store.create_task(&t, &request).unwrap();
    let id = provenant_core::OperationId::parse("create-1").unwrap();
    let replayed = store
        .coordination_operation(project(), actor(2), &id, &"create", &request, || {
            Err::<TaskReceipt, Error>(Error::Invalid("receipt replay reran its effect"))
        })
        .unwrap();
    assert_eq!(replayed, TaskReceipt::Created { task: t.clone() });
    assert_eq!(
        store
            .coordination_receipt::<TaskReceipt>(project(), actor(2), &id, &request)
            .unwrap(),
        Some(TaskReceipt::Created { task: t.clone() })
    );
    let peer = provenant_core::OperationId::parse("peer-1").unwrap();
    let digest = provenant_core::Digest::from_bytes([7; 32]);
    let operation = store
        .prepare_peer(project(), actor(2), &peer, digest, b"{}", &request)
        .unwrap();
    assert_eq!(operation.phase, PeerPhase::Committed);
    let reply: serde_json::Value = provenant_records::decode(
        operation.response.as_deref().unwrap(),
        provenant_records::Limits::CONFIG,
    )
    .unwrap();
    let task: Task = serde_json::from_value(reply.pointer(PEER_TASK).unwrap().clone()).unwrap();
    assert_eq!(task, t);
    assert_eq!(stored_records(&store.db), before);
    drop(store);
    drop(f.open());
}

#[test]
fn retired_members_written_after_migration_are_refused() {
    let f = Fixture::new();
    let store = f.open();
    let (_, t) = seed_records(&f, &store);
    write_null_members(&store, &t);
    let request = f.request();
    let id = provenant_core::OperationId::parse("create-1").unwrap();
    assert!(matches!(
        store.coordination_receipt::<TaskReceipt>(project(), actor(2), &id, &request),
        Err(Error::Decode(_))
    ));
    assert!(matches!(
        store.task(project(), t.id, &request),
        Err(Error::Decode(_))
    ));
    assert!(matches!(
        store.read_mail(project(), actor(3), Some(0), 100, &request),
        Err(Error::Decode(_))
    ));
}

#[test]
fn empty_legacy_import_upgrades_and_leaves_no_trace() {
    let f = Fixture::new();
    let store = f.open();
    let (_, t) = seed_records(&f, &store);
    let events = store.events(project(), 0, 100, &f.request()).unwrap().items;
    // An import that brought nothing over left its receipt and one event.
    downgrade_to_six(
        &store,
        &format!(
            "INSERT INTO imports VALUES('/old/fabric.db','{}','lp',0,0,0,X'7B7D'); \
             INSERT INTO events(project,dedup,record) SELECT project,NULL,\
             CAST(json_set(CAST(record AS TEXT),'$.kind','imported') AS BLOB) \
             FROM events WHERE seq=1;",
            project()
        ),
    );
    drop(store);
    let store = f.open();
    let imports: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='imports'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(imports, 0);
    assert_eq!(
        store.events(project(), 0, 100, &f.request()).unwrap().items,
        events
    );
    assert_eq!(store.task(project(), t.id, &f.request()).unwrap(), t);
}

// Pins the cursor edge migration 7 leaves behind. The `imported` event it
// deletes was the project's highest seq, so that cursor value is refused until
// the next event takes a higher seq. Accepted rather than fixed (#1590).
#[test]
fn cursor_at_a_deleted_import_event_waits_for_the_next_event() {
    let f = Fixture::new();
    let store = f.open();
    let note = event(
        project(),
        Some(actor(3)),
        EventKind::Note,
        None,
        "a note".into(),
        f.now(),
    );
    let first = store.note(&note, None, &f.request()).unwrap();
    downgrade_to_six(
        &store,
        &format!(
            "INSERT INTO imports VALUES('/old/fabric.db','{}','lp',0,0,0,X'7B7D'); \
             INSERT INTO events(project,dedup,record) SELECT project,NULL,\
             CAST(json_set(CAST(record AS TEXT),'$.kind','imported') AS BLOB) \
             FROM events WHERE seq=1;",
            project()
        ),
    );
    drop(store);
    let store = f.open();
    let deleted = first + 1;
    assert!(matches!(
        store.mark_events_read(project(), actor(3), deleted, &f.request()),
        Err(Error::Invalid("event cursor"))
    ));
    store
        .mark_events_read(project(), actor(3), first, &f.request())
        .unwrap();
    let mut next = note.clone();
    next.detail = "another note".into();
    assert_eq!(store.note(&next, None, &f.request()).unwrap(), deleted + 1);
    store
        .mark_events_read(project(), actor(3), deleted, &f.request())
        .unwrap();
    assert_eq!(
        store
            .event_cursor(project(), actor(3), &f.request())
            .unwrap(),
        deleted
    );
}

#[test]
fn empty_committed_peer_response_upgrades_unchanged() {
    let f = Fixture::new();
    let store = f.open();
    seed_records(&f, &store);
    downgrade_to_six(&store, "UPDATE peer_operations SET response=X'';");
    drop(store);
    let store = f.open();
    let response: Vec<u8> = store
        .db
        .query_row("SELECT response FROM peer_operations", [], |r| r.get(0))
        .unwrap();
    assert!(response.is_empty());
}

#[test]
fn legacy_import_data_refuses_the_upgrade_and_keeps_every_row() {
    let reference = r#"json('{"id":"i","project":"p","source":"s"}')"#;
    let cases = [
        (
            "imports",
            "INSERT INTO imports VALUES('/old/fabric.db','p','lp',0,1,0,X'7B7D');".to_owned(),
        ),
        (
            "imports",
            "INSERT INTO imports VALUES('/old/fabric.db','p','lp',0,0,1,X'7B7D');".to_owned(),
        ),
        (
            "events",
            format!(
                "INSERT INTO imports VALUES('/old/fabric.db','{}','lp',0,0,0,X'7B7D'); \
                 UPDATE events SET record=CAST(json_set(CAST(record AS TEXT),'$.kind','imported') AS BLOB) \
                 WHERE seq=1;",
                ProjectId::from_bytes([9; 16])
            ),
        ),
        (
            "events",
            "UPDATE events SET record=CAST(json_set(CAST(record AS TEXT),'$.kind','imported') AS BLOB) \
             WHERE seq=1;"
                .to_owned(),
        ),
        (
            "messages",
            format!(
                "UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.legacy',{reference}) AS BLOB);"
            ),
        ),
        (
            "messages",
            "UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.legacy_output_path','out.md') AS BLOB);"
                .to_owned(),
        ),
        (
            "messages",
            format!(
                "UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.conversation',\
                 json_object('reference',{reference},'scope','legacy')) AS BLOB);"
            ),
        ),
        (
            "messages",
            format!(
                "UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.reply_to',\
                 json_object('reference',{reference},'scope','legacy')) AS BLOB);"
            ),
        ),
        (
            "messages",
            format!(
                "UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.task',\
                 json_object('reference',{reference},'scope','legacy')) AS BLOB);"
            ),
        ),
        (
            "tasks",
            format!(
                "UPDATE tasks SET record=CAST(json_set(CAST(record AS TEXT),'$.legacy',{reference}) AS BLOB);"
            ),
        ),
        (
            "coordination_operations",
            format!(
                "UPDATE coordination_operations SET result=CAST(json_set(CAST(result AS TEXT),'$.task.legacy',{reference}) AS BLOB);"
            ),
        ),
        (
            "peer_operations",
            format!(
                "UPDATE peer_operations SET response=CAST(json_set(CAST(response AS TEXT),'$.result.data.result.task.legacy',{reference}) AS BLOB);"
            ),
        ),
    ];
    for (table, rows) in cases {
        let f = Fixture::new();
        let store = f.open();
        seed_records(&f, &store);
        downgrade_to_six(&store, &rows);
        let before = stored_records(&store.db);
        drop(store);
        let refused = Store::open(&f.root, &f.request());
        let error = refused.err().unwrap();
        assert!(
            matches!(error, Error::LegacyImportData { table: found, rows: 1 } if found == table),
            "{table}: {error:?}"
        );
        let text = error.to_string();
        assert!(
            text.contains(&format!("1 row(s) of legacy import data in table {table}"))
                && text.contains("docs/runbooks/fabric-hosts.md"),
            "{text}"
        );
        let db = rusqlite::Connection::open(f.path()).unwrap();
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 6, "{table}");
        assert_eq!(stored_records(&db), before, "{table}");
    }
}

#[test]
fn owner_can_close_its_expired_active_task_but_cannot_update_it() {
    for state in [TaskState::Open, TaskState::Done, TaskState::Cancelled] {
        let mut f = Fixture::new();
        let store = f.open();
        store.create_task(&task(&f), &f.request()).unwrap();
        let fence = store.claim_task(&claim(&f, 3), &f.request()).unwrap();
        f.clock
            .advance(DurationMicros::from_seconds(1).unwrap())
            .unwrap();
        assert!(matches!(
            store.update_task(project(), &fence, "changed", None, &f.request()),
            Err(Error::StaleFence)
        ));
        if state == TaskState::Open {
            store.release_task(project(), &fence, &f.request()).unwrap();
        } else {
            store
                .finish_task(project(), &fence, state, &f.request())
                .unwrap();
        }
        assert_eq!(
            store
                .task(project(), fence.task, &f.request())
                .unwrap()
                .state,
            state
        );
    }
}

mod f2;

mod human_inbox;
mod operations;
