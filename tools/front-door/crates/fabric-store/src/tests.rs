use super::*;
use provenant_core::{
    CancelToken, Clock, DurationMicros, MessageId, MonoTime, PathResolver, RootInputs, StateRoot,
    TaskId, TestClock, TimeError,
};
use provenant_records::{ArtifactRef, PathWire, RecordHeader};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
};

#[test]
fn sqlite_diagnostic_reports_code_without_private_details() {
    let error = Error::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_READONLY),
        Some("private SQL value".into()),
    ));
    let message = error.to_string();
    assert!(message.contains("Error code 8: attempt to write a readonly database"));
    assert!(!message.contains("private SQL value"));
}

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
            cancel: &self.cancel,
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
        extra: Default::default(),
        header: RecordHeader::V1,
        project: project(),
        id: MessageId::from_bytes([n; 16]),
        sender: actor(2),
        recipients: vec![actor(3), actor(4)],
        kind: MessageKind::Request,
        body: "Review this".into(),
        artifacts: vec![ArtifactRef {
            extra: Default::default(),
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
        extra: Default::default(),
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

// Simulate a newer writer, then exercise the older reader's normal store API.
fn add_future_member(store: &Store, table: &str) {
    store.db.execute(
        &format!("UPDATE {table} SET record=CAST(json_set(CAST(record AS TEXT),'$.future_member',json_object('revision',2,'notes',json_array('new'))) AS BLOB)"),
        [],
    ).unwrap();
}

fn assert_future_member(value: &impl Serialize) {
    assert_eq!(
        serde_json::to_value(value).unwrap()["future_member"],
        serde_json::json!({"revision": 2, "notes": ["new"]})
    );
}

#[test]
fn task_unknown_fields_survive_load_update_and_save() {
    let f = Fixture::new();
    let store = f.open();
    store.create_task(&task(&f), &f.request()).unwrap();
    add_future_member(&store, "tasks");
    assert_future_member(&store.task(project(), task(&f).id, &f.request()).unwrap());
    let fence = store.claim_task(&claim(&f, 3), &f.request()).unwrap();
    store
        .update_task(
            project(),
            &fence,
            "updated objective",
            Some("updated note"),
            &f.request(),
        )
        .unwrap();
    drop(store);
    let store = f.open();
    let saved = store.task(project(), task(&f).id, &f.request()).unwrap();
    assert_eq!(saved.objective, "updated objective");
    assert_future_member(&saved);
}

#[test]
fn message_unknown_fields_survive_load_update_and_save() {
    let f = Fixture::new();
    let store = f.open();
    store.send(&message(&f, 5), &f.request()).unwrap();
    add_future_member(&store, "messages");
    let mut loaded = store
        .read_mail(project(), actor(3), Some(0), 100, &f.request())
        .unwrap()
        .items
        .remove(0)
        .message;
    assert_future_member(&loaded);
    // Mail is immutable: saving an edited copy requires a new message ID.
    loaded.id = MessageId::from_bytes([7; 16]);
    loaded.body = "updated body".into();
    store.send(&loaded, &f.request()).unwrap();
    drop(store);
    let store = f.open();
    let saved = store
        .read_mail(project(), actor(3), Some(0), 100, &f.request())
        .unwrap()
        .items
        .remove(1)
        .message;
    assert_eq!(saved.body, "updated body");
    assert_future_member(&saved);
}

#[test]
fn event_unknown_fields_survive_load_update_and_save() {
    let f = Fixture::new();
    let store = f.open();
    store
        .note(
            &event(
                project(),
                Some(actor(3)),
                EventKind::Note,
                None,
                "original note".into(),
                f.now(),
            ),
            None,
            &f.request(),
        )
        .unwrap();
    add_future_member(&store, "events");
    let mut loaded = store
        .events(project(), 0, 100, &f.request())
        .unwrap()
        .items
        .remove(0)
        .record;
    assert_future_member(&loaded);
    // Events append; an edited note is saved as a new event.
    loaded.detail = "updated note".into();
    store.note(&loaded, None, &f.request()).unwrap();
    drop(store);
    let store = f.open();
    let saved = store
        .events(project(), 0, 100, &f.request())
        .unwrap()
        .items
        .remove(1)
        .record;
    assert_eq!(saved.detail, "updated note");
    assert_future_member(&saved);
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
// The holder keeps its write lock across a real-time sleep so the sender waits.
#[allow(clippy::disallowed_methods)]
#[test]
fn a_busy_store_waits_for_the_writer_instead_of_failing() {
    let f = Fixture::new();
    let store = f.open();
    let held = Barrier::new(2);
    std::thread::scope(|s| {
        let releaser = s.spawn(|| {
            let holder = f.open();
            holder.db.execute_batch("BEGIN IMMEDIATE").unwrap();
            held.wait();
            std::thread::sleep(std::time::Duration::from_millis(200));
            holder.db.execute_batch("ROLLBACK").unwrap();
        });
        held.wait();
        store.send(&message(&f, 5), &f.request()).unwrap();
        releaser.join().unwrap();
    });
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
fn busy_and_cancellation_are_bounded_and_leave_no_mutation() {
    let f = Fixture::new();
    let store = f.open();
    let holder = f.open();
    holder.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    store
        .db
        .busy_timeout(std::time::Duration::from_millis(10))
        .unwrap();
    assert!(matches!(
        store.send(&message(&f, 5), &f.request()),
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
                let store = Store::open(&f.root, &f.request()).unwrap();
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
#[serde(tag = "result", rename_all = "snake_case")]
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
// Fixtures rewinding a current database must remove later tables in
// foreign-key order before the older schema can be upgraded again.
const REMOVE_VIEWER_TABLES: &str = "DROP TABLE viewer_requests; DROP TABLE viewer_summaries;";
const REMOVE_HUMAN_TABLES: &str = "DROP TABLE run_chairs; DROP TABLE human_answer_outbox; \
    DROP TABLE human_answers; DROP TABLE human_request_history; DROP TABLE human_requests;";
fn downgrade_to_six(store: &Store, rows: &str) {
    store
        .db
        .execute_batch(&format!(
            "{REMOVE_VIEWER_TABLES} {REMOVE_HUMAN_TABLES} {IMPORTS_TABLE} {rows} PRAGMA user_version=6;"
        ))
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
fn newer_stored_task_header_reports_unsupported_version() {
    let f = Fixture::new();
    let store = f.open();
    let (_, task) = seed_records(&f, &store);
    store.db.execute(
        "UPDATE tasks SET record=CAST(json_set(CAST(record AS TEXT),'$.header.schema_version',2) AS BLOB)",
        [],
    ).unwrap();
    assert!(matches!(
        store.task(project(), task.id, &f.request()),
        Err(Error::Decode(
            provenant_records::DecodeError::UnsupportedVersion { found: 2, .. }
        ))
    ));
}

#[test]
fn retired_members_written_after_migration_are_preserved_as_extensions() {
    let f = Fixture::new();
    let store = f.open();
    let (_, t) = seed_records(&f, &store);
    write_null_members(&store, &t);
    let request = f.request();
    let id = provenant_core::OperationId::parse("create-1").unwrap();
    let mut expected = t;
    expected
        .extra
        .insert("legacy".into(), serde_json::Value::Null);
    assert_eq!(
        store
            .coordination_receipt::<TaskReceipt>(project(), actor(2), &id, &request)
            .unwrap(),
        Some(TaskReceipt::Created {
            task: expected.clone()
        })
    );
    assert_eq!(
        store.task(project(), expected.id, &request).unwrap(),
        expected
    );
    let mail = store
        .read_mail(project(), actor(3), Some(0), 100, &request)
        .unwrap();
    for name in ["legacy", "legacy_output_path"] {
        assert_eq!(
            mail.items[0].message.extra.get(name),
            Some(&serde_json::Value::Null)
        );
    }
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
fn legacy_import_rows_are_deliberately_dropped_and_all_stores_read_cleanly() {
    let f = Fixture::new();
    let store = f.open();
    let (modern_message, modern_task) = seed_records(&f, &store);
    let mut imported_task = task(&f);
    imported_task.id = TaskId::from_bytes([80; 16]);
    store.create_task(&imported_task, &f.request()).unwrap();
    for id in 81..=84 {
        store.send(&message(&f, id), &f.request()).unwrap();
    }
    downgrade_to_six(
        &store,
        &format!(
            "INSERT INTO imports VALUES('/old/fabric.db','{}','lp',0,1,4,X'7B7D'); \
             UPDATE tasks SET record=CAST(json_set(CAST(record AS TEXT),'$.legacy',json_object('id','old')) AS BLOB) WHERE id='{}'; \
             UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.legacy',json_object('id','old')) AS BLOB) WHERE id='{}'; \
             UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.conversation',json_object('scope','legacy','id','old')) AS BLOB) WHERE id='{}'; \
             UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.reply_to',json_object('scope','legacy','id','old')) AS BLOB) WHERE id='{}'; \
             UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.task',json_object('scope','legacy','id','old')) AS BLOB) WHERE id='{}'; \
             INSERT INTO events(project,dedup,record) SELECT project,NULL,CAST(json_set(CAST(record AS TEXT),'$.kind','imported') AS BLOB) FROM events WHERE seq=1; \
             INSERT INTO coordination_operations SELECT project,'legacy-create',actor,request,CAST(json_set(CAST(result AS TEXT),'$.task.legacy',json_object('id','old')) AS BLOB) FROM coordination_operations WHERE id='create-1'; \
             INSERT INTO peer_operations SELECT project,'legacy-peer',actor,digest,phase,request,CAST(json_set(CAST(response AS TEXT),'$.result.data.result.task.legacy',json_object('id','old')) AS BLOB) FROM peer_operations WHERE id='peer-1';",
            project(),
            imported_task.id,
            MessageId::from_bytes([81; 16]),
            MessageId::from_bytes([82; 16]),
            MessageId::from_bytes([83; 16]),
            MessageId::from_bytes([84; 16]),
        ),
    );
    drop(store);
    let store = f.open();
    let request = f.request();
    assert_eq!(
        store
            .tasks(project(), None, None, 100, &request)
            .unwrap()
            .items,
        vec![modern_task.clone()]
    );
    assert!(matches!(
        store.task(project(), imported_task.id, &request),
        Err(Error::NotFound)
    ));
    let mail = store
        .read_mail(project(), actor(3), Some(0), 100, &request)
        .unwrap();
    assert_eq!(mail.items.len(), 1);
    assert_eq!(mail.items[0].message, modern_message);
    assert!(
        !store
            .events(project(), 0, 100, &request)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        store
            .claims(project(), None, 100, &request)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        store
            .landing(project(), "refs/heads/main", &request)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .coordination_receipt::<TaskReceipt>(
                project(),
                actor(2),
                &provenant_core::OperationId::parse("create-1").unwrap(),
                &request
            )
            .unwrap(),
        Some(TaskReceipt::Created { task: modern_task })
    );
    let operation = store
        .lookup_peer(
            project(),
            actor(2),
            &provenant_core::OperationId::parse("peer-1").unwrap(),
            &request,
        )
        .unwrap()
        .unwrap();
    let _: serde_json::Value =
        serde_json::from_slice(operation.response.as_deref().unwrap()).unwrap();
    assert!(
        store
            .coordination_receipt::<TaskReceipt>(
                project(),
                actor(2),
                &provenant_core::OperationId::parse("legacy-create").unwrap(),
                &request
            )
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .lookup_peer(
                project(),
                actor(2),
                &provenant_core::OperationId::parse("legacy-peer").unwrap(),
                &request
            )
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .db
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
            .optional()
            .unwrap()
            .is_none()
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

mod human_inbox;
mod operations;

#[test]
#[allow(clippy::disallowed_methods)] // Verify the upgraded deadline-aware index path.
fn run_chair_migration_preserves_mail_and_rejects_older_readers() {
    let f = Fixture::new();
    let store = f.open();
    store.send(&message(&f, 9), &f.request()).unwrap();
    store
        .db
        .execute_batch(&format!(
            "{REMOVE_VIEWER_TABLES} DROP INDEX human_hook_queued; DROP INDEX human_hook_pending; DROP INDEX human_queued_delivery; DROP INDEX human_local_delivery; DROP TABLE run_chairs; PRAGMA user_version=8;"
        ))
        .unwrap();
    drop(store);
    let store = f.open();
    let target = provenant_records::inbox::AnswerTarget {
        extra: Default::default(),
        host: provenant_core::HostId::from_bytes([8; 16]),
        project: project(),
        actor: actor(2),
        session: "chair".into(),
    };
    // The production read forces INDEXED BY human_queued_delivery: this fails
    // if the upgrade forgot the index or created one with an unusable predicate.
    assert!(
        store
            .read_queued_human_answers(
                &target,
                std::time::Instant::now() + std::time::Duration::from_secs(1)
            )
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        store
            .read_mail(project(), actor(3), Some(0), 100, &f.request())
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(matches!(
        provenant_sqlite::read_version(&store.db, 8),
        Err(provenant_sqlite::VersionError::Unsupported(version)) if version == crate::db::MIGRATIONS.len() as i64
    ));
    assert!(
        HostStore::open(&f.root, &f.request())
            .unwrap()
            .run_chair(
                project(),
                provenant_core::RunId::from_bytes([1; 16]),
                &f.request()
            )
            .unwrap()
            .is_none()
    );
}

#[test]
fn host_store_refuses_unsafe_database_companions() {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let f = Fixture::new();
        let store = f.open();
        let path = f
            .root
            .as_path()
            .join("fabric")
            .join(format!("host.sqlite3{suffix}"));
        std::os::unix::fs::symlink(f.path(), &path).unwrap();
        assert!(matches!(
            HostStore::open(&f.root, &f.request()),
            Err(Error::UnsafePath(_))
        ));
        // The host refusal must leave ordinary mail storage intact.
        store.send(&message(&f, 9), &f.request()).unwrap();
    }
}

#[test]
fn host_store_ignores_lane_writable_locks_and_shared_rows() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let shared = f.open();
    let host = HostStore::open(&f.root, &f.request()).unwrap();
    let run = provenant_core::RunId::from_bytes([1; 16]);
    let value = RunChair {
        extra: Default::default(),
        request_digest: provenant_core::Digest::from_bytes([2; 32]),
        chair: None,
    };
    host.seal_run_chair(project(), run, &value, &f.request())
        .unwrap();
    shared
        .db
        .execute(
            "INSERT INTO run_chairs VALUES(?1,?2,?3)",
            params![project().to_string(), run.to_string(), b"forged".as_slice()],
        )
        .unwrap();
    std::fs::set_permissions(
        f.root.as_path().join("fabric/locks"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(matches!(
        Store::open(&f.root, &f.request()),
        Err(Error::UnsafePath(_))
    ));
    drop(host);
    // A current-schema open must not need a migration write transaction.
    let writer = rusqlite::Connection::open(f.root.as_path().join("fabric/host.sqlite3")).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(
        HostStore::open(&f.root, &f.request())
            .unwrap()
            .run_chair(project(), run, &f.request())
            .unwrap(),
        Some(value)
    );
}
