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
    for n in 0..3 {
        let id = HumanRequestId::from_bytes([n + 1; 16]).to_string();
        let project = project().to_string();
        let mut record = serde_json::json!({
            "schema_version": 1, "id": id, "project_id": project,
            "project_key": "demo", "request": { "title": format!("Title {n}"), "body": "secret" }
        });
        if n == 1 {
            record["chair_session_id"] = "chair".into();
        } else if n == 2 {
            record["chair_session_id"] = "different-from-routing".into();
        }
        let record = record.to_string();
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
    assert_eq!(page.items[0].chair_session_id.as_deref(), Some("chair"));
    let all = reader
        .pending_human_requests(100, 1_048_576, &f.request())
        .unwrap();
    assert_eq!(all.items.len(), 3);
    assert_eq!(all.items[1].chair_session_id.as_deref(), Some("chair"));
    assert_eq!(all.items[2].chair_session_id.as_deref(), Some("chair"));
}

#[test]
fn overview_rejects_non_text_session_in_record() {
    let f = Fixture::new();
    let store = f.open();
    let id = HumanRequestId::from_bytes([1; 16]).to_string();
    let project = project().to_string();
    for value in [
        serde_json::json!(42),
        serde_json::json!({}),
        serde_json::json!(false),
        serde_json::Value::Null,
    ] {
        let record = serde_json::json!({
            "schema_version": 1, "id": id, "project_id": project,
            "chair_session_id": value, "request": { "title": "Question" }
        })
        .to_string();
        store
            .db
            .execute(
                "INSERT OR REPLACE INTO human_requests VALUES(?1,?2,1,'chair','open',?3)",
                params![project, id, record.as_bytes()],
            )
            .unwrap();
        assert!(matches!(
            store.pending_human_requests(1, 1_048_576, &f.request()),
            Err(Error::Invalid(_))
        ));
    }
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
