use super::*;
use provenant_core::{HostId, HumanAnswerId, HumanRequestId, OperationId};
use provenant_records::inbox::*;

#[test]
fn overview_read_only_does_not_create_or_migrate() {
    let f = Fixture::new();
    assert!(matches!(
        Store::open_existing(&f.root, &f.request()),
        Err(Error::NotFound)
    ));
    assert!(!f.root.as_path().exists());
    std::fs::create_dir(f.root.as_path()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(f.root.as_path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        Store::open_existing(&f.root, &f.request()),
        Err(Error::NotFound)
    ));
    assert!(!f.root.as_path().join("fabric").exists());
    let writer = f.open();
    writer.db.pragma_update(None, "user_version", 8).unwrap();
    assert!(matches!(
        Store::open_existing(&f.root, &f.request()),
        Err(Error::SchemaVersion(8))
    ));
    let version: i64 = writer
        .db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 8);
}

#[test]
fn overview_reads_committed_wal_and_limits_rows() {
    let f = Fixture::new();
    let writer = f.open();
    for n in 0..2 {
        let id = HumanRequestId::from_bytes([n + 1; 16]).to_string();
        let project = project().to_string();
        let record = serde_json::json!({
            "schema_version": 1, "id": id, "project_id": project,
            "project_key": "demo", "request": { "title": format!("Title {n}"), "body": "secret" }
        })
        .to_string();
        writer
            .db
            .execute(
                "INSERT INTO human_requests VALUES(?1,?2,1,'chair','open',?3)",
                params![project, id, record.as_bytes()],
            )
            .unwrap();
    }
    let reader = Store::open_existing(&f.root, &f.request()).unwrap();
    let page = reader
        .pending_human_requests(1, 1_048_576, &f.request())
        .unwrap();
    assert!(page.truncated);
    assert_eq!(page.items[0].title, "Title 0");
    assert_eq!(page.items[0].project_key.as_deref(), Some("demo"));
    assert_eq!(
        reader
            .pending_human_requests(100, 1_048_576, &f.request())
            .unwrap()
            .items
            .len(),
        2
    );
}

#[test]
fn overview_bounds_aggregate_request_bytes_before_decoding() {
    let f = Fixture::new();
    let writer = f.open();
    let project = project().to_string();
    let mut lengths = Vec::new();
    for n in 0..3 {
        let id = HumanRequestId::from_bytes([n + 1; 16]).to_string();
        let record = serde_json::json!({
            "schema_version": 1, "id": id, "project_id": project,
            "project_key": "demo", "request": { "title": format!("Title {n}"), "body": "x".repeat(200) }
        }).to_string();
        lengths.push(record.len());
        writer
            .db
            .execute(
                "INSERT INTO human_requests VALUES(?1,?2,1,'chair','open',?3)",
                params![project, id, record.as_bytes()],
            )
            .unwrap();
    }
    let reader = Store::open_existing(&f.root, &f.request()).unwrap();
    let batch = reader
        .pending_human_requests(3, lengths[0] + lengths[1] - 1, &f.request())
        .unwrap();
    assert_eq!(batch.items.len(), 1);
    assert_eq!(batch.bytes_read, lengths[0]);
    assert!(batch.truncated);

    // Invalid JSON exposes any attempt to decode a row beyond the budget.
    writer
        .db
        .execute(
            "UPDATE human_requests SET record=?1 WHERE id=(SELECT min(id) FROM human_requests)",
            params![vec![b'x'; 4096]],
        )
        .unwrap();
    let batch = reader.pending_human_requests(3, 100, &f.request()).unwrap();
    assert!(batch.items.is_empty());
    assert_eq!(batch.bytes_read, 0);
    assert!(batch.truncated);
}

#[test]
fn local_delivery_target_query_is_indexed() {
    let f = Fixture::new();
    let store = f.open();
    let plan: Vec<String> = store.db.prepare(
        "EXPLAIN QUERY PLAN SELECT project FROM human_answer_outbox INDEXED BY human_local_delivery
         WHERE json_extract(CAST(record AS TEXT),'$.target_host')=?1
         AND json_extract(CAST(record AS TEXT),'$.target_session')=?2
         AND json_extract(CAST(record AS TEXT),'$.target_actor')=?3
         AND project=?4
         AND json_extract(CAST(record AS TEXT),'$.state') IN ('queued','offered')
         ORDER BY project,answer LIMIT 1"
    ).unwrap().query_map(params!["host", "session", "actor", "project"], |row| row.get(3))
      .unwrap().collect::<std::result::Result<_,_>>().unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("SEARCH") && line.contains("human_local_delivery")),
        "{plan:?}"
    );
    assert!(
        plan.iter()
            .all(|line| !line.contains("SCAN") && !line.contains("TEMP B-TREE")),
        "{plan:?}"
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // Exercise the real-time reader boundary.
fn human_answer_outbox_mail_and_history_roll_back_when_receipt_fails() {
    let f = Fixture::new();
    let store = f.open();
    let request = f.request();
    let id = HumanRequestId::from_bytes([5; 16]);
    let original = HumanRequest {
        schema_version: 1,
        id,
        revision: 1,
        operation_id: OperationId::parse("ask").unwrap(),
        created_at: f.now(),
        updated_at: f.now(),
        project_id: project(),
        project_key: None,
        owner_host: HostId::from_bytes([8; 16]),
        chair_session_id: "chair-a".into(),
        chair_actor: actor(2),
        request: RequestDraft {
            kind: RequestKind::Question,
            title: "Question".into(),
            body: "What next?".into(),
            blocking: false,
            options: vec![],
            recommended_option_id: None,
            proceeding_with: None,
        },
        state: RequestState::Open,
        answer_id: None,
        closure_reason: None,
    };
    store.create_human_request(&original, &request).unwrap();
    let operation = OperationId::parse("answer").unwrap();
    let answer = HumanAnswer {
        schema_version: 1,
        id: HumanAnswerId::from_bytes([6; 16]),
        request_id: id,
        request_revision: 1,
        action: AnswerAction::Reply,
        option_id: None,
        text: Some("Ship it".into()),
        actor: actor(3),
        channel: InteractionChannel::Cli,
        answered_at: f.now(),
        operation_id: operation.clone(),
    };
    let mut mail = message(&f, 7);
    mail.kind = MessageKind::Response;
    mail.recipients = vec![actor(2)];
    let delivery = AnswerDelivery {
        schema_version: 1,
        answer_id: answer.id,
        target_host: original.owner_host,
        target_session: original.chair_session_id.clone(),
        target_actor: original.chair_actor,
        transport: AnswerTransport::FabricMail,
        message_id: mail.id,
        state: AnswerDeliveryState::Queued,
        attempts: 1,
        last_attempt_at: Some(f.now()),
        acknowledged_at: None,
        error: None,
    };
    store.db.execute_batch("CREATE TRIGGER reject_human_receipt BEFORE INSERT ON coordination_operations BEGIN SELECT RAISE(ABORT,'receipt refused'); END;").unwrap();
    let action = || store.answer_human_request(&answer, &delivery, &mail, &request);
    let error = store
        .coordination_operation(project(), actor(3), &operation, &"answer", &request, action)
        .unwrap_err();
    assert!(
        matches!(error,Error::Sqlite(rusqlite::Error::SqliteFailure(code,_)) if code.code==rusqlite::ErrorCode::ConstraintViolation)
    );
    assert_eq!(
        store.human_item(project(), id, &request).unwrap(),
        InboxItem {
            request: original.clone(),
            answer: None,
            delivery: None,
            history: vec![original]
        }
    );
    for table in [
        "human_answers",
        "human_answer_outbox",
        "messages",
        "deliveries",
        "coordination_operations",
    ] {
        let rows: i64 = store
            .db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
    store
        .db
        .execute_batch("DROP TRIGGER reject_human_receipt")
        .unwrap();
    let resolved = store
        .coordination_operation(project(), actor(3), &operation, &"answer", &request, action)
        .unwrap();
    assert_eq!(resolved.answer_id, Some(answer.id));
    let target = AnswerTarget {
        host: delivery.target_host,
        project: project(),
        session: delivery.target_session.clone(),
        actor: delivery.target_actor,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    let reader = Store::open_read_only(&f.root, deadline).unwrap().unwrap();
    let observed = reader
        .read_human_answers(&target, 1, MAX_OFFER_BYTES, deadline)
        .unwrap();
    assert_eq!(observed.items[0].answer, answer);
    assert_eq!(observed.items[0].delivery, delivery);
    assert_eq!(
        store.human_item(project(), id, &request).unwrap().delivery,
        Some(delivery.clone())
    );
    assert!(
        reader
            .read_human_answers(
                &AnswerTarget {
                    actor: actor(99),
                    ..target.clone()
                },
                1,
                MAX_OFFER_BYTES,
                deadline
            )
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        reader
            .read_human_answers(&target, 1, 64, deadline)
            .unwrap()
            .truncated
    );
    assert!(
        reader
            .db
            .execute("DELETE FROM human_answer_outbox", [])
            .is_err()
    );
    drop(reader);
    store.db.execute_batch("CREATE TRIGGER reject_offer BEFORE UPDATE ON human_answer_outbox BEGIN SELECT RAISE(ABORT,'offer refused'); END;").unwrap();
    assert!(
        store
            .offer_human_answers(&target, 50, MAX_OFFER_BYTES, f.now(), &request)
            .is_err()
    );
    assert_eq!(
        store
            .human_item(project(), id, &request)
            .unwrap()
            .delivery
            .unwrap(),
        delivery
    );
    store.db.execute_batch("DROP TRIGGER reject_offer").unwrap();
    let offered = store
        .offer_human_answers(&target, 50, MAX_OFFER_BYTES, f.now(), &request)
        .unwrap();
    assert_eq!(offered.items.len(), 1);
    assert_eq!(
        offered.items[0].delivery.state,
        AnswerDeliveryState::Offered
    );
    let plan:Vec<String>=store.db.prepare("EXPLAIN QUERY PLAN SELECT substr(record,1,?6) FROM human_requests WHERE project=?1 AND (?2 IS NULL OR session=?2) AND (?3 OR state='open') AND (?4 IS NULL OR id>?4) ORDER BY id LIMIT ?5").unwrap().query_map(params![project().to_string(),"chair-a",false,id.to_string(),51,MAX_RECORD_READ_BYTES],|row|row.get(3)).unwrap().map(|row|row.unwrap()).collect();
    assert!(
        plan.iter()
            .any(|row| row.contains("SEARCH human_requests USING INDEX")),
        "{plan:?}"
    );
}

#[test]
fn human_inbox_migration_preserves_existing_mail_and_refuses_old_binary_version() {
    let f = Fixture::new();
    let store = f.open();
    store.send(&message(&f, 9), &f.request()).unwrap();
    // Reopen is idempotent and all pre-inbox records remain readable.
    drop(store);
    let store = f.open();
    assert_eq!(
        store
            .read_mail(project(), actor(3), Some(0), 100, &f.request())
            .unwrap()
            .items
            .len(),
        1
    );
    let version: i64 = store
        .db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, crate::db::MIGRATIONS.len() as i64);
    assert!(matches!(
        provenant_sqlite::read_version(&store.db, 7),
        Err(provenant_sqlite::VersionError::Unsupported(found)) if found == version
    ));
}

#[test]
#[allow(clippy::disallowed_methods)] // Exercise the real-time reader boundary.
fn local_delivery_open_allocates_nothing_and_observes_deadline() {
    let f = Fixture::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    assert!(Store::open_read_only(&f.root, deadline).unwrap().is_none());
    assert!(!f.root.as_path().exists());
    let writer = f.open();
    let past = std::time::Instant::now() - std::time::Duration::from_millis(1);
    assert!(matches!(
        Store::open_read_only(&f.root, past),
        Err(Error::Busy)
    ));
    writer.db.execute_batch("PRAGMA user_version=9").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    assert!(matches!(
        Store::open_read_only(&f.root, deadline),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        writer
            .db
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        9
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // Fresh processes exercise native SQLite VFS opens.
fn local_delivery_fresh_process() -> Result<()> {
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    if let Some(path) = std::env::var_os("PROVENANT_READER_ROOT") {
        let path = PathBuf::from(path);
        let mut inputs = RootInputs::new(path.join("home"), path.join("assets"));
        inputs.state_dir = Some(path);
        let roots = inputs.resolve(&Resolver).unwrap();
        let reader =
            Store::open_read_only(&roots.state, Instant::now() + Duration::from_millis(100))?
                .unwrap();
        assert_eq!(
            reader
                .db
                .query_row("SELECT value FROM probe", [], |row| row.get::<_, i64>(0))?,
            42
        );
        return Ok(());
    }
    let f = Fixture::new();
    let writer = f.open();
    writer
        .db
        .execute_batch("CREATE TABLE probe(value INTEGER); INSERT INTO probe VALUES(42)")?;
    let child = || {
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::human_inbox::local_delivery_fresh_process",
                "--nocapture",
            ])
            .env("PROVENANT_READER_ROOT", f.root.as_path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    };
    child(); // Uncheckpointed WAL remains visible from a different process.
    drop(writer);
    child(); // The ordinary idle state must also support local delivery.
    // Missing companions may defer delivery, but may never be recreated.
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", f.path().display()));
    }
    assert!(Store::open_read_only(&f.root, Instant::now() + Duration::from_millis(100)).is_err());
    assert!(!f.path().with_extension("sqlite3-wal").exists());
    assert!(!f.path().with_extension("sqlite3-shm").exists());
    Ok(())
}
