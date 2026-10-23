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
        legacy_output_path: None,
        created_at: f.now(),
        legacy: None,
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
        legacy: None,
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
fn old_snapshot(f: &Fixture) -> (PathBuf, rusqlite::Connection) {
    let path = f.temp.path().join("old-snapshot.sqlite3");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/old-schema.sql"
    )))
    .unwrap();
    (path, db)
}
fn plan(f: &Fixture, path: PathBuf) -> ImportPlan {
    ImportPlan {
        snapshot: path,
        source: "cutover-1".into(),
        legacy_project: "legacy-project".into(),
        project: project(),
        actors: std::collections::BTreeMap::from([
            ("sender".into(), actor(2)),
            ("a".into(), actor(3)),
            ("b".into(), actor(4)),
        ]),
        cutover: f.now(),
    }
}
fn old_message(
    db: &rusqlite::Connection,
    id: &str,
    at: i64,
    kind: &str,
    read: Option<i64>,
    task: Option<&str>,
    reply: Option<&str>,
) {
    db.execute("INSERT INTO messages VALUES(?,'legacy-project','sender','request body',?,'outside',?,?,NULL,?)",params![id,kind,reply,task,at]).unwrap();
    for recipient in ["a", "b"] {
        db.execute(
            "INSERT INTO deliveries VALUES(?,'legacy-project',?,?)",
            params![id, recipient, read],
        )
        .unwrap();
    }
}
#[test]
fn bounded_import_preserves_subset_links_boundary_cursors_and_source_bytes() {
    let f = Fixture::new();
    let store = f.open();
    let (path, db) = old_snapshot(&f);
    let cutoff = f.now().as_micros() / 1000 - 14 * 86_400_000;
    for (id, state) in [
        ("open", "open"),
        ("blocked", "blocked"),
        ("closed", "done"),
        ("claimed", "claimed"),
    ] {
        db.execute(
            "INSERT INTO tasks VALUES('legacy-project',?,'old objective','someone',?,?,?)",
            params![id, state, cutoff, cutoff],
        )
        .unwrap();
    }
    old_message(
        &db,
        "boundary",
        cutoff,
        "request",
        None,
        Some("blocked"),
        None,
    );
    old_message(
        &db,
        "recent",
        cutoff + 1,
        "request",
        None,
        Some("closed"),
        Some("boundary"),
    );
    old_message(&db, "expired", cutoff - 1, "request", None, None, None);
    old_message(
        &db,
        "acked",
        cutoff + 1,
        "request",
        Some(cutoff + 2),
        None,
        None,
    );
    old_message(&db, "response", cutoff + 1, "response", None, None, None);
    db.execute(
        "UPDATE deliveries SET read_at=? WHERE message_id='recent' AND recipient_id='b'",
        [cutoff + 3],
    )
    .unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    let plan = plan(&f, path.clone());
    let receipt = store.import_snapshot(&plan, &f.request()).unwrap();
    assert_eq!(
        receipt,
        ImportReceipt {
            tasks: 2,
            messages: 2,
            skipped: false,
            skipped_rows: Default::default(),
        }
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_dir(f.temp.path()).unwrap().count(), 2);
    assert!(
        store
            .read_mail(project(), actor(3), None, 100, &f.request())
            .unwrap()
            .items
            .is_empty()
    );
    let mail = store
        .read_mail(project(), actor(3), Some(0), 100, &f.request())
        .unwrap()
        .items;
    assert_eq!(mail.len(), 2);
    assert_eq!(
        store
            .read_mail(project(), actor(4), Some(0), 100, &f.request())
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(matches!(mail[0].message.task, Some(TaskRef::Local { .. })));
    assert!(matches!(mail[1].message.task, Some(TaskRef::Legacy { .. })));
    assert!(matches!(
        mail[1].message.reply_to,
        Some(MessageRef::Local { .. })
    ));
    assert!(matches!(
        mail[0].message.conversation,
        Some(MessageRef::Legacy { .. })
    ));
    let tasks = store
        .tasks(project(), None, None, 100, &f.request())
        .unwrap();
    assert!(
        tasks
            .iter()
            .all(|t| t.state == TaskState::Open && t.owner.is_none())
    );
    assert!(tasks.iter().any(|t| t.note.is_some()));
    std::fs::remove_file(path).unwrap();
    assert!(store.import_snapshot(&plan, &f.request()).unwrap().skipped);
}
#[test]
fn failed_import_is_atomic_retryable_and_refuses_live_sidecars() {
    let f = Fixture::new();
    let store = f.open();
    let (path, db) = old_snapshot(&f);
    old_message(
        &db,
        "request",
        f.now().as_micros() / 1000,
        "request",
        None,
        None,
        None,
    );
    drop(db);
    let plan = plan(&f, path.clone());
    store.db.execute_batch("CREATE TRIGGER fail_cursor BEFORE INSERT ON mail_cursors BEGIN SELECT RAISE(ABORT,'injected cursor failure'); END").unwrap();
    assert!(store.import_snapshot(&plan, &f.request()).is_err());
    for table in [
        "imports",
        "messages",
        "deliveries",
        "mail_cursors",
        "events",
    ] {
        assert_eq!(
            store
                .db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    store.db.execute_batch("DROP TRIGGER fail_cursor").unwrap();
    assert!(!store.import_snapshot(&plan, &f.request()).unwrap().skipped);
    let db = rusqlite::Connection::open(path).unwrap();
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    old_message(
        &db,
        "live",
        f.now().as_micros() / 1000,
        "request",
        None,
        None,
        None,
    );
    let mut different = plan;
    different.source = "other-source".into();
    assert!(matches!(
        store.import_snapshot(&different, &f.request()),
        Err(Error::ImportNotQuiescent)
    ));
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
    for (old, state) in [
        ("open", TaskState::Open),
        ("blocked", TaskState::Open),
        ("claimed", TaskState::Active),
        ("active", TaskState::Active),
        ("done", TaskState::Done),
        ("cancelled", TaskState::Cancelled),
    ] {
        assert_eq!(TaskState::from_old(old).unwrap(), state);
    }
    assert!(TaskState::from_old("unknown").is_err());
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
fn import_keeps_old_open_work_but_excludes_future_tasks_and_requests() {
    let f = Fixture::new();
    let store = f.open();
    let (path, db) = old_snapshot(&f);
    let now = f.now().as_micros() / 1000;
    for (id, created) in [("old", now - 30 * 86_400_000), ("future", now + 1)] {
        db.execute(
            "INSERT INTO tasks VALUES('legacy-project',?,'objective',NULL,'open',?,?)",
            params![id, created, created],
        )
        .unwrap();
    }
    old_message(&db, "future", now + 1, "request", None, None, None);
    drop(db);
    assert_eq!(
        store
            .import_snapshot(&plan(&f, path), &f.request())
            .unwrap(),
        ImportReceipt {
            tasks: 1,
            messages: 0,
            skipped: false,
            skipped_rows: Default::default(),
        }
    );
    assert_eq!(
        store
            .tasks(project(), None, None, 100, &f.request())
            .unwrap()[0]
            .legacy
            .as_ref()
            .unwrap()
            .id,
        "old"
    );
}
#[test]
fn large_source_and_malformed_row_do_not_block_a_bounded_import() {
    let f = Fixture::new();
    let store = f.open();
    let (path, db) = old_snapshot(&f);
    db.execute(
        "INSERT INTO tasks VALUES('legacy-project','bad','objective',NULL,'open',1.5,0)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO tasks VALUES('legacy-project','good','objective',NULL,'open',1,0)",
        [],
    )
    .unwrap();
    drop(db);
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(67_108_865).unwrap();
    drop(file);
    let receipt = store
        .import_snapshot(&plan(&f, path), &f.request())
        .unwrap();
    assert_eq!(receipt.tasks, 1);
    assert_eq!(
        receipt.skipped_rows,
        std::collections::BTreeMap::from([(ImportSkipReason::InvalidValue, 1)])
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
fn cleanly_closed_wal_source_imports_without_changing_old_root() {
    let f = Fixture::new();
    let store = f.open();
    let (path, db) = old_snapshot(&f);
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    old_message(
        &db,
        "wal",
        f.now().as_micros() / 1000,
        "request",
        None,
        None,
        None,
    );
    drop(db);
    assert_eq!(&std::fs::read(&path).unwrap()[18..20], &[2, 2]);
    let before = std::fs::read(&path).unwrap();
    let names = || {
        std::fs::read_dir(f.temp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let entries = names();
    assert_eq!(
        store
            .import_snapshot(&plan(&f, path.clone()), &f.request())
            .unwrap()
            .messages,
        1
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(names(), entries);
}

#[test]
fn invalid_legacy_row_does_not_abort_valid_import() {
    let f = Fixture::new();
    let store = f.open();
    let (path, db) = old_snapshot(&f);
    old_message(
        &db,
        "bad",
        f.now().as_micros() / 1000,
        "request",
        None,
        None,
        None,
    );
    old_message(
        &db,
        "good",
        f.now().as_micros() / 1000,
        "request",
        None,
        None,
        None,
    );
    db.execute("UPDATE messages SET body=NULL WHERE message_id='bad'", [])
        .unwrap();
    drop(db);
    assert_eq!(
        store
            .import_snapshot(&plan(&f, path), &f.request())
            .unwrap()
            .messages,
        1
    );
}

#[test]
fn import_counts_each_row_validation_reason_and_replays_the_receipt() {
    for reason in [
        ImportSkipReason::NullValue,
        ImportSkipReason::InvalidValue,
        ImportSkipReason::EmptyText,
        ImportSkipReason::OversizeText,
        ImportSkipReason::Nul,
        ImportSkipReason::UnmappedActor,
        ImportSkipReason::DuplicateRecipient,
        ImportSkipReason::TooManyRecipients,
        ImportSkipReason::RecordSize,
    ] {
        let f = Fixture::new();
        let store = f.open();
        let (path, db) = old_snapshot(&f);
        old_message(
            &db,
            "bad",
            f.now().as_micros() / 1000,
            "request",
            None,
            None,
            None,
        );
        old_message(
            &db,
            "good",
            f.now().as_micros() / 1000,
            "request",
            None,
            None,
            Some("bad"),
        );
        let mut plan = plan(&f, path);
        match reason {
            ImportSkipReason::NullValue => {
                db.execute("UPDATE messages SET body=NULL WHERE message_id='bad'", [])
                    .unwrap();
            }
            ImportSkipReason::InvalidValue => {
                db.execute(
                    "UPDATE messages SET created_at=created_at-0.5 WHERE message_id='bad'",
                    [],
                )
                .unwrap();
            }
            ImportSkipReason::EmptyText => {
                db.execute("UPDATE messages SET body='' WHERE message_id='bad'", [])
                    .unwrap();
            }
            ImportSkipReason::OversizeText => {
                db.execute(
                    "UPDATE messages SET body=? WHERE message_id='bad'",
                    ["x".repeat(MAX_BODY_BYTES + 1)],
                )
                .unwrap();
            }
            ImportSkipReason::Nul => {
                db.execute(
                    "UPDATE messages SET body=? WHERE message_id='bad'",
                    ["a\0b"],
                )
                .unwrap();
            }
            ImportSkipReason::UnmappedActor => {
                db.execute(
                    "UPDATE messages SET sender_id='unknown' WHERE message_id='bad'",
                    [],
                )
                .unwrap();
            }
            ImportSkipReason::DuplicateRecipient => {
                plan.actors.insert("alias".into(), actor(3));
                db.execute(
                    "INSERT INTO deliveries VALUES('bad','legacy-project','alias',NULL)",
                    [],
                )
                .unwrap();
            }
            ImportSkipReason::TooManyRecipients => {
                for n in 0..99_u8 {
                    let label = format!("recipient-{n}");
                    plan.actors.insert(label.clone(), actor(n + 5));
                    db.execute(
                        "INSERT INTO deliveries VALUES('bad','legacy-project',?,NULL)",
                        [label],
                    )
                    .unwrap();
                }
            }
            ImportSkipReason::RecordSize => {
                db.execute(
                    "UPDATE messages SET body=? WHERE message_id='bad'",
                    ["\u{1}".repeat(MAX_BODY_BYTES)],
                )
                .unwrap();
            }
        }
        drop(db);
        let receipt = store.import_snapshot(&plan, &f.request()).unwrap();
        let retained = matches!(
            reason,
            ImportSkipReason::DuplicateRecipient | ImportSkipReason::TooManyRecipients
        );
        assert_eq!(receipt.messages, if retained { 2 } else { 1 }, "{reason:?}");
        assert_eq!(
            receipt.skipped_rows,
            std::collections::BTreeMap::from([(reason, 1)])
        );
        let mail = store
            .read_mail(project(), actor(3), Some(0), 100, &f.request())
            .unwrap();
        let good = mail
            .items
            .iter()
            .find(|row| row.message.legacy.as_ref().unwrap().id == "good")
            .unwrap();
        assert_eq!(
            matches!(good.message.reply_to, Some(MessageRef::Local { .. })),
            retained
        );
        if retained {
            let bad = mail
                .items
                .iter()
                .find(|row| row.message.legacy.as_ref().unwrap().id == "bad")
                .unwrap();
            assert!(bad.message.recipients.contains(&actor(3)));
            assert!(bad.message.recipients.contains(&actor(4)));
        }
        let replay = store.import_snapshot(&plan, &f.request()).unwrap();
        assert!(replay.skipped);
        assert_eq!(replay.skipped_rows, receipt.skipped_rows);
    }
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
    drop(f.open());
    let db = rusqlite::Connection::open(f.path()).unwrap();
    db.execute_batch("DROP TABLE operations; DROP TABLE claim_paths; DROP TABLE claims; ALTER TABLE imports DROP COLUMN skipped_rows; PRAGMA user_version=1; INSERT INTO imports VALUES('prior','project','legacy',0,1,2);").unwrap();
    drop(db);
    let store = f.open();
    assert_eq!(
        store
            .db
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        crate::db::MIGRATIONS.len() as i64
    );
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT skipped_rows FROM imports WHERE source='prior'",
                [],
                |r| r.get::<_, Vec<u8>>(0)
            )
            .unwrap(),
        b"{}"
    );
    drop(store);
    drop(f.open());
}

#[test]
fn import_filename_cannot_inject_sqlite_uri_options() {
    let f = Fixture::new();
    let store = f.open();
    let (path, db) = old_snapshot(&f);
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    old_message(
        &db,
        "encoded",
        f.now().as_micros() / 1000,
        "request",
        None,
        None,
        None,
    );
    drop(db);
    let encoded = f.temp.path().join("old ?mode=rwc#é%.sqlite3");
    std::fs::rename(path, &encoded).unwrap();
    let before = std::fs::read(&encoded).unwrap();
    assert_eq!(
        store
            .import_snapshot(&plan(&f, encoded.clone()), &f.request())
            .unwrap()
            .messages,
        1
    );
    assert_eq!(std::fs::read(encoded).unwrap(), before);
    assert_eq!(std::fs::read_dir(f.temp.path()).unwrap().count(), 2);
}

#[test]
fn invalid_individual_deliveries_preserve_valid_recipients() {
    for (recipient, reason) in [
        (None, ImportSkipReason::NullValue),
        (Some(String::new()), ImportSkipReason::EmptyText),
        (
            Some("x".repeat(MAX_BODY_BYTES + 1)),
            ImportSkipReason::OversizeText,
        ),
        (Some("a\0b".into()), ImportSkipReason::Nul),
        (Some("unknown".into()), ImportSkipReason::UnmappedActor),
    ] {
        let f = Fixture::new();
        let store = f.open();
        let (path, db) = old_snapshot(&f);
        old_message(
            &db,
            "message",
            f.now().as_micros() / 1000,
            "request",
            None,
            None,
            None,
        );
        db.execute(
            "INSERT INTO deliveries VALUES('message','legacy-project',?,NULL)",
            [recipient],
        )
        .unwrap();
        drop(db);
        let receipt = store
            .import_snapshot(&plan(&f, path), &f.request())
            .unwrap();
        assert_eq!(receipt.messages, 1);
        assert_eq!(
            receipt.skipped_rows,
            std::collections::BTreeMap::from([(reason, 1)])
        );
        let mail = store
            .read_mail(project(), actor(3), Some(0), 100, &f.request())
            .unwrap();
        assert_eq!(mail.items[0].message.recipients, vec![actor(3), actor(4)]);
        assert_eq!(
            store
                .read_mail(project(), actor(4), Some(0), 100, &f.request())
                .unwrap()
                .items
                .len(),
            1
        );
    }
}

#[test]
fn oversized_legacy_id_is_skipped_without_exhausting_selected_text_budget() {
    let f = Fixture::new();
    let store = f.open();
    let (path, db) = old_snapshot(&f);
    old_message(
        &db,
        &"x".repeat(8_388_609),
        f.now().as_micros() / 1000,
        "request",
        None,
        None,
        None,
    );
    old_message(
        &db,
        "good",
        f.now().as_micros() / 1000,
        "request",
        None,
        None,
        None,
    );
    drop(db);
    let receipt = store
        .import_snapshot(&plan(&f, path), &f.request())
        .unwrap();
    assert_eq!(receipt.messages, 1);
    assert_eq!(
        receipt.skipped_rows,
        std::collections::BTreeMap::from([(ImportSkipReason::OversizeText, 2)])
    );
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
