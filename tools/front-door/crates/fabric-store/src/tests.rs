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

// A coordination receipt as stored, without the runtime crate.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum SentReceipt {
    Sent { message: Message },
}
const PEER_MESSAGE: &str = "/result/data/result/message";
// A committed PeerResponse is plain serde_json in field order, not canonical.
fn peer_reply(message: &str) -> Vec<u8> {
    format!(
        r#"{{"version":1,"host":"{0}","project":"{0}","result":{{"result":"coordination","data":{{"domain":"mail","result":{{"result":"sent","message":{message}}}}}}}}}"#,
        "07".repeat(16)
    )
    .into_bytes()
}
// Every column that holds a serialised Message or Event, as text.
fn stored_records(db: &rusqlite::Connection) -> Vec<String> {
    let mut result = Vec::new();
    for query in [
        "SELECT record FROM messages ORDER BY id",
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
// Fixtures rewinding a current database must remove later tables in
// foreign-key order and restore removed ones before the older schema can be
// upgraded again.
const REMOVE_HUMAN_TABLES: &str = "DROP TABLE run_chairs; DROP TABLE human_answer_outbox; \
    DROP TABLE human_answers; DROP TABLE human_request_history; DROP TABLE human_requests;";
/// The task board table as every release before migration 015 created it.
pub(crate) const RESTORE_TASKS_TABLE: &str = "CREATE TABLE tasks (
 project TEXT NOT NULL, id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('open','active','done','cancelled')),
 generation INTEGER NOT NULL CHECK(generation >= 0),
 owner TEXT, token BLOB, expires INTEGER, record BLOB NOT NULL,
 PRIMARY KEY(project, id),
 CHECK((state = 'active' AND owner IS NOT NULL AND length(token) = 16 AND expires IS NOT NULL)
    OR (state <> 'active' AND owner IS NULL AND token IS NULL AND expires IS NULL))
) STRICT;";
fn downgrade_to_six(store: &Store, rows: &str) {
    store
        .db
        .execute_batch(&format!(
            "{REMOVE_HUMAN_TABLES} {RESTORE_TASKS_TABLE} CREATE TABLE imports(source TEXT PRIMARY KEY, project TEXT NOT NULL, legacy_project TEXT NOT NULL, imported_at INTEGER NOT NULL, tasks INTEGER NOT NULL, messages INTEGER NOT NULL, skipped_rows BLOB NOT NULL DEFAULT X'7B7D') STRICT; {rows} PRAGMA user_version=6;"
        ))
        .unwrap();
}
// Seed a message, its coordination receipt and a committed peer reply
// carrying that message.
fn seed_records(f: &Fixture, store: &Store) -> Message {
    let request = f.request();
    let mut m = message(f, 5);
    m.body = "Review \"this\"\tnow é".into();
    let id = provenant_core::OperationId::parse("send-1").unwrap();
    let receipt = store
        .coordination_operation(project(), actor(2), &id, &"send", &request, || {
            store.send(&m, &request)?;
            Ok::<_, Error>(SentReceipt::Sent { message: m.clone() })
        })
        .unwrap();
    assert_eq!(receipt, SentReceipt::Sent { message: m.clone() });
    let peer = provenant_core::OperationId::parse("peer-1").unwrap();
    let digest = provenant_core::Digest::from_bytes([7; 32]);
    store
        .prepare_peer(project(), actor(2), &peer, digest, b"{}", &request)
        .unwrap();
    store
        .peer_phase(project(), actor(2), &peer, PeerPhaseChange::Sent, &request)
        .unwrap();
    let response = peer_reply(&serde_json::to_string(&m).unwrap());
    store
        .peer_phase(
            project(),
            actor(2),
            &peer,
            PeerPhaseChange::Committed { response },
            &request,
        )
        .unwrap();
    m
}

// Rewrite the seeded message as a version 6 binary writes it, with the removed
// members null. Canonical records sort `legacy` and `legacy_output_path` into
// place.
fn write_null_members(store: &Store) {
    let bytes: Vec<u8> = store
        .db
        .query_row("SELECT record FROM messages", [], |r| r.get(0))
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let object = value.as_object_mut().unwrap();
    for name in ["legacy", "legacy_output_path"] {
        object.insert(name.into(), serde_json::Value::Null);
    }
    let old = encode(&value).unwrap();
    store
        .db
        .execute("UPDATE messages SET record=?1", [old])
        .unwrap();
}

#[test]
fn ordered_migrations_upgrade_existing_rows_and_do_not_reapply() {
    let f = Fixture::new();
    let store = f.open();
    let m = seed_records(&f, &store);
    let before = stored_records(&store.db);
    write_null_members(&store);
    downgrade_to_six(&store, "");
    let before_upgrade = stored_records(&store.db);
    assert_ne!(before_upgrade, before);
    drop(store);

    let store = f.open();
    let backup = f
        .root
        .as_path()
        .join(format!("fabric/fabric.sqlite3.bak-{}", f.now().as_micros()));
    let backup = provenant_sqlite::open_connection(&backup, true).unwrap();
    assert_eq!(
        backup
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        6
    );
    assert_eq!(stored_records(&backup), before_upgrade);
    assert_eq!(
        store
            .db
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        crate::db::MIGRATIONS.len() as i64
    );
    // Each replay succeeds only if the stored bytes match a fresh encode, or
    // for receipts, only if the stored reply still decodes.
    let request = f.request();
    store.send(&m, &request).unwrap();
    let id = provenant_core::OperationId::parse("send-1").unwrap();
    let replayed = store
        .coordination_operation(project(), actor(2), &id, &"send", &request, || {
            Err::<SentReceipt, Error>(Error::Invalid("receipt replay reran its effect"))
        })
        .unwrap();
    assert_eq!(replayed, SentReceipt::Sent { message: m.clone() });
    assert_eq!(
        store
            .coordination_receipt::<SentReceipt>(project(), actor(2), &id, &request)
            .unwrap(),
        Some(SentReceipt::Sent { message: m.clone() })
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
    let sent: Message =
        serde_json::from_value(reply.pointer(PEER_MESSAGE).unwrap().clone()).unwrap();
    assert_eq!(sent, m);
    assert_eq!(stored_records(&store.db), before);
    drop(store);
    drop(f.open());
}

#[test]
fn legacy_import_rows_are_deliberately_dropped_and_all_stores_read_cleanly() {
    let f = Fixture::new();
    let store = f.open();
    let modern_message = seed_records(&f, &store);
    for id in 81..=85 {
        store.send(&message(&f, id), &f.request()).unwrap();
    }
    let modern_events = store.events(project(), 0, 100, &f.request()).unwrap().items;
    downgrade_to_six(
        &store,
        &format!(
            "INSERT INTO imports VALUES('/old/fabric.db','{}','lp',0,1,5,X'7B7D'); \
             UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.legacy',json_object('id','old')) AS BLOB) WHERE id='{}'; \
             UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.conversation',json_object('scope','legacy','id','old')) AS BLOB) WHERE id='{}'; \
             UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.reply_to',json_object('scope','legacy','id','old')) AS BLOB) WHERE id='{}'; \
             UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.task',json_object('scope','legacy','id','old')) AS BLOB) WHERE id='{}'; \
             UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.legacy_output_path','old-output') AS BLOB) WHERE id='{}'; \
             INSERT INTO events(project,dedup,record) SELECT project,NULL,CAST(json_set(CAST(record AS TEXT),'$.kind','imported') AS BLOB) FROM events WHERE seq=1; \
             INSERT INTO coordination_operations SELECT project,'legacy-send',actor,request,CAST(json_set(CAST(result AS TEXT),'$.message.legacy',json_object('id','old')) AS BLOB) FROM coordination_operations WHERE id='send-1'; \
             INSERT INTO peer_operations SELECT project,'legacy-peer',actor,digest,phase,request,CAST(json_set(CAST(response AS TEXT),'$.result.data.result.message.legacy',json_object('id','old')) AS BLOB) FROM peer_operations WHERE id='peer-1'; \
             INSERT INTO peer_operations SELECT project,'empty-peer',actor,digest,phase,request,X'' FROM peer_operations WHERE id='peer-1';",
            project(),
            MessageId::from_bytes([81; 16]),
            MessageId::from_bytes([82; 16]),
            MessageId::from_bytes([83; 16]),
            MessageId::from_bytes([84; 16]),
            MessageId::from_bytes([85; 16]),
        ),
    );
    let deleted_cursor: i64 = store
        .db
        .query_row("SELECT max(seq) FROM events", [], |row| row.get(0))
        .unwrap();
    drop(store);
    let store = f.open();
    let request = f.request();
    assert!(
        !store
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='imports')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    );
    let empty_response: Vec<u8> = store
        .db
        .query_row(
            "SELECT response FROM peer_operations WHERE id='empty-peer'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(empty_response.is_empty());
    let mail = store
        .read_mail(project(), actor(3), Some(0), 100, &request)
        .unwrap();
    assert_eq!(mail.items.len(), 1);
    assert_eq!(mail.items[0].message, modern_message);
    assert_eq!(
        store.events(project(), 0, 100, &request).unwrap().items,
        modern_events
    );
    assert!(matches!(
        store.mark_events_read(project(), actor(3), deleted_cursor, &request),
        Err(Error::Invalid(_))
    ));
    let note = event(
        project(),
        Some(actor(3)),
        EventKind::Note,
        None,
        "after migration".into(),
        f.now(),
    );
    assert!(store.note(&note, None, &request).unwrap() > deleted_cursor);
    store
        .mark_events_read(project(), actor(3), deleted_cursor, &request)
        .unwrap();
    assert_eq!(
        store.event_cursor(project(), actor(3), &request).unwrap(),
        deleted_cursor
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
            .coordination_receipt::<SentReceipt>(
                project(),
                actor(2),
                &provenant_core::OperationId::parse("send-1").unwrap(),
                &request
            )
            .unwrap(),
        Some(SentReceipt::Sent {
            message: modern_message
        })
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
            .coordination_receipt::<SentReceipt>(
                project(),
                actor(2),
                &provenant_core::OperationId::parse("legacy-send").unwrap(),
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
fn task_board_migration_drops_tasks_and_strips_message_links() {
    let f = Fixture::new();
    let store = f.open();
    let request = f.request();
    let mut m = message(&f, 5);
    m.body = "Review \"this\"\tnow é".into();
    store.send(&m, &request).unwrap();
    let modern_events = store.events(project(), 0, 100, &request).unwrap().items;
    let mail_send = serde_json::json!({"domain":"mail","input":{"op":"send","body":"b"}});
    let send_id = provenant_core::OperationId::parse("mail-send").unwrap();
    store
        .coordination_operation(project(), actor(2), &send_id, &mail_send, &request, || {
            Ok::<_, Error>(serde_json::json!({"result":"sent"}))
        })
        .unwrap();
    let peer = provenant_core::OperationId::parse("task-peer").unwrap();
    store
        .prepare_peer(
            project(),
            actor(2),
            &peer,
            provenant_core::Digest::from_bytes([7; 32]),
            br#"{"input":{"domain":"task","input":{"op":"list"}}}"#,
            &request,
        )
        .unwrap();
    // Rewind to schema 14 with the rows a release with the task board wrote.
    store
        .db
        .execute_batch(&format!(
            "{RESTORE_TASKS_TABLE} \
             INSERT INTO tasks VALUES('{project}','t1','open',0,NULL,NULL,NULL,CAST('{{}}' AS BLOB)); \
             INSERT INTO events(project,dedup,record) SELECT project,NULL,CAST(json_set(CAST(record AS TEXT),'$.kind','task_created') AS BLOB) FROM events WHERE seq=1; \
             UPDATE messages SET record=CAST(json_set(CAST(record AS TEXT),'$.task',json_object('scope','local','id','t1')) AS BLOB); \
             UPDATE coordination_operations SET request=CAST(json_set(CAST(request AS TEXT),'$.input.task',json('null')) AS BLOB) WHERE id='mail-send'; \
             INSERT INTO coordination_operations VALUES('{project}','task-create','{actor}',CAST('{{\"domain\":\"task\",\"input\":{{\"op\":\"create\"}}}}' AS BLOB),CAST('{{}}' AS BLOB)); \
             PRAGMA user_version=14;",
            project = project(),
            actor = actor(2),
        ))
        .unwrap();
    drop(store);
    let store = f.open();
    let tables: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='tasks'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
    assert_eq!(
        store.events(project(), 0, 100, &request).unwrap().items,
        modern_events
    );
    let mail = store
        .read_mail(project(), actor(3), Some(0), 100, &request)
        .unwrap();
    assert_eq!(mail.items[0].message, m);
    store.send(&m, &request).unwrap();
    assert!(
        store
            .coordination_replay::<serde_json::Value>(
                project(),
                actor(2),
                &send_id,
                &mail_send,
                &request
            )
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .coordination_receipt::<serde_json::Value>(
                project(),
                actor(2),
                &provenant_core::OperationId::parse("task-create").unwrap(),
                &request
            )
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .lookup_peer(project(), actor(2), &peer, &request)
            .unwrap()
            .is_none()
    );
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
            "{RESTORE_TASKS_TABLE} DROP INDEX human_hook_queued; DROP INDEX human_hook_pending; DROP INDEX human_queued_delivery; DROP INDEX human_local_delivery; DROP TABLE run_chairs; PRAGMA user_version=8;"
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
