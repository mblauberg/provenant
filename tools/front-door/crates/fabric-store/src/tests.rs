use super::*;
use provenant_core::{
    CancelToken, Clock, DurationMicros, MessageId, MonoTime, PathResolver, RootInputs, StateRoot,
    TestClock, TimeError,
};
use provenant_records::{ArtifactRef, PathWire, RecordHeader};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
};

#[test]
fn bounded_row_collection_stops_before_over_budget_row() {
    let rows = [
        Ok((vec![0; MAX_PAGE_BYTES / 2], ())),
        Ok((vec![0; MAX_PAGE_BYTES / 2], ())),
        Ok((vec![0; 1], ())),
    ];
    let rows = bounded_rows(rows.into_iter(), |bytes, ()| Ok(bytes)).unwrap();
    assert_eq!(rows.items.len(), 2);
    assert!(rows.truncated);
    assert_eq!(
        rows.items.iter().map(Vec::len).sum::<usize>(),
        MAX_PAGE_BYTES
    );

    let rows = [Ok((vec![0; MAX_PAGE_BYTES + 1], ()))];
    let rows = bounded_rows(rows.into_iter(), |bytes, ()| Ok(bytes)).unwrap();
    assert!(rows.items.is_empty());
    assert!(rows.truncated);
}

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

#[test]
fn fabric_commits_signal_inbox_and_board() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let before = provenant_os::changes::read(fixture.root.as_path()).unwrap();
    store
        .send(&message(&fixture, 5), &fixture.request())
        .unwrap();
    let after = provenant_os::changes::read(fixture.root.as_path()).unwrap();
    assert!(after.inbox > before.inbox);
    let note = event(
        project(),
        Some(actor(3)),
        EventKind::Note,
        None,
        "board".into(),
        fixture.now(),
    );
    store.note(&note, None, &fixture.request()).unwrap();
    let board = provenant_os::changes::read(fixture.root.as_path()).unwrap();
    assert!(board.board > after.board);
}

#[test]
fn signal_failure_reports_a_committed_fabric_write() {
    let fixture = Fixture::new();
    let store = fixture.open();
    // Opening writes no commit, so no signal exists yet: create one with a bump.
    let inbox = provenant_os::changes::Scope::Inbox;
    provenant_os::changes::bump(fixture.root.as_path(), inbox).unwrap();
    let signal = fixture
        .root
        .as_path()
        .join(provenant_core::changes::FILE_NAME);
    std::fs::remove_file(&signal).unwrap();
    std::fs::create_dir(&signal).unwrap();
    // The signal is a hint: a forced failure must not fail the committed write.
    store
        .send(&message(&fixture, 5), &fixture.request())
        .unwrap();
    assert_eq!(
        store
            .read_mail(project(), actor(3), Some(0), 10, &fixture.request())
            .unwrap()
            .items
            .len(),
        1
    );
}
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

#[test]
#[allow(clippy::disallowed_methods)] // Native read-only WAL open deadline.
fn live_read_only_observations_refresh_wal_and_refuse_mutations() {
    let f = Fixture::new();
    let writer = f.open();
    let reader = Store::open_read_only(
        &f.root,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
    .unwrap()
    .unwrap()
    .live_read_only()
    .unwrap();
    assert!(
        reader
            .read_mail(project(), actor(3), Some(0), 10, &f.request())
            .unwrap()
            .items
            .is_empty()
    );
    writer.send(&message(&f, 5), &f.request()).unwrap();
    assert_eq!(
        reader
            .read_mail(project(), actor(3), Some(0), 10, &f.request())
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(reader.send(&message(&f, 6), &f.request()).is_err());
    assert!(reader.db.execute_batch("DELETE FROM messages").is_err());
    assert!(f.open().live_read_only().is_err());
}
#[test]
fn persisted_wal_survives_last_writer_in_unwritable_directory() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let writer = f.open();
    writer.send(&message(&f, 5), &f.request()).unwrap();
    drop(writer);
    let dir = f.path().parent().unwrap().to_path_buf();
    for suffix in ["-wal", "-shm"] {
        assert!(PathBuf::from(format!("{}{suffix}", f.path().display())).exists());
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = provenant_sqlite::open_read_only_connection(&f.path()).and_then(|db| {
        db.query_row("SELECT count(*) FROM messages", [], |row| {
            row.get::<_, i64>(0)
        })
    });
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result.unwrap(), 1);
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
        conversation: None,
        created_at: f.now(),
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
fn event_failure_rolls_back_mail_mutations() {
    let f = Fixture::new();
    let store = f.open();
    store.db.execute_batch("CREATE TRIGGER fail_events BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'injected event failure'); END").unwrap();
    assert!(store.send(&message(&f, 5), &f.request()).is_err());
    for table in ["messages", "deliveries", "events"] {
        assert_eq!(
            store
                .db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
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
#[test]
fn a_busy_store_waits_for_the_writer_instead_of_failing() {
    let f = Fixture::new();
    let store = f.open();
    let (held_tx, held_rx) = std::sync::mpsc::sync_channel(0);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::scope(|s| {
        let fixture = &f;
        let holder = s.spawn(move || {
            let holder = fixture.open();
            holder.db.execute_batch("BEGIN IMMEDIATE").unwrap();
            held_tx.send(()).unwrap();
            let blocked = matches!(
                done_rx.recv_timeout(std::time::Duration::from_millis(100)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            );
            holder.db.execute_batch("ROLLBACK").unwrap();
            blocked
        });
        held_rx.recv().unwrap();
        let result = store.send(&message(&f, 5), &f.request());
        let _ = done_tx.send(());
        assert!(
            holder.join().unwrap(),
            "sender returned while the write lock was held"
        );
        result.unwrap();
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
fn concurrent_cold_opens_wait_for_one_initialiser() {
    // SQLite never calls the busy handler on the switch to WAL, so without
    // the initialisation lock a second cold open reports busy at once.
    for _ in 0..10 {
        let f = Fixture::new();
        let barrier = Barrier::new(8);
        std::thread::scope(|scope| {
            let opens: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        Store::open(&f.root, &f.request()).map(drop)
                    })
                })
                .collect();
            for open in opens {
                assert!(matches!(open.join().unwrap(), Ok(())));
            }
        });
    }
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
fn nonempty_unversioned_database_is_refused_before_mutation() {
    let f = Fixture::new();
    drop(f.open());
    let db = rusqlite::Connection::open(f.path()).unwrap();
    db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA user_version=0")
        .unwrap();
    drop(db);
    assert!(matches!(
        Store::open(&f.root, &f.request()),
        Err(Error::SchemaVersion(0))
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
fn store_clock_controls_mutation_times_and_acknowledgements() {
    let mut f = Fixture::new();
    let store = f.open();
    f.clock
        .advance(DurationMicros::from_micros(500_000))
        .unwrap();
    store.send(&message(&f, 5), &f.request()).unwrap();
    store
        .mark_mail_read(project(), actor(3), 1, &f.request())
        .unwrap();
    let events = store.events(project(), 0, 100, &f.request()).unwrap();
    assert_eq!(events.items.last().unwrap().record.at, f.now());
}

mod f2;

mod human_inbox;
mod operations;

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
fn custody_is_host_owned_and_does_not_read_lane_writable_rows() {
    use provenant_records::custody::*;
    let f = Fixture::new();
    let shared = f.open();
    let host = HostStore::open(&f.root, &f.request()).unwrap();
    let subject = Subject::new(SubjectKind::Graph, "graph");
    let holder = Holder::Attempt {
        attempt: provenant_core::AttemptId::from_bytes([1; 16]),
        chair: None,
    };
    let record = host
        .acquire_custody(
            project(),
            &subject,
            &holder,
            Origin::Provenant,
            &f.request(),
        )
        .unwrap()
        .record;
    shared.db.execute("INSERT INTO custody(project,kind,subject,state,holder,record) VALUES(?1,'graph','graph','active','forged',?2)", params![project().to_string(), b"forged".as_slice()]).unwrap();
    assert_eq!(
        host.custody_item(project(), &subject, &f.request())
            .unwrap(),
        Some(record)
    );
}
