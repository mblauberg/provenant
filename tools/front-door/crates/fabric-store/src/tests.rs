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
        let temp = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
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
                let store = f.open();
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

#[test]
fn ordered_migrations_upgrade_existing_rows_and_do_not_reapply() {
    let f = Fixture::new();
    let (m, t) = (message(&f, 5), task(&f));
    let store = f.open();
    store.send(&m, &f.request()).unwrap();
    store.create_task(&t, &f.request()).unwrap();
    drop(store);
    // Rebuild the version-6 shape: the imports table and the null legacy members.
    let db = rusqlite::Connection::open(f.path()).unwrap();
    db.execute_batch(
        "CREATE TABLE imports(source TEXT PRIMARY KEY, project TEXT NOT NULL, \
         legacy_project TEXT NOT NULL, imported_at INTEGER NOT NULL, tasks INTEGER NOT NULL, \
         messages INTEGER NOT NULL, skipped_rows BLOB NOT NULL DEFAULT X'7B7D') STRICT; \
         UPDATE messages SET record=CAST(json_insert(CAST(record AS TEXT),'$.legacy',json('null'),\
         '$.legacy_output_path',json('null')) AS BLOB); \
         UPDATE tasks SET record=CAST(json_insert(CAST(record AS TEXT),'$.legacy',json('null')) AS BLOB); \
         PRAGMA user_version=6;",
    )
    .unwrap();
    drop(db);
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
    // Replaying the same records succeeds only if the stored bytes match a fresh encode.
    store.send(&m, &f.request()).unwrap();
    assert_eq!(store.task(t.project, t.id, &f.request()).unwrap(), t);
    drop(store);
    drop(f.open());
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

mod operations;
