use super::*;
use provenant_core::{HostId, HumanAnswerId, HumanRequestId, OperationId};
use provenant_records::inbox::*;

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
            work: None,
            deadline: None,
            supersedes: None,
            kind: RequestKind::Question,
            title: "Question".into(),
            body: "What next?".into(),
            blocking: false,
            options: vec![],
            recommended_option_id: None,
            proceeding_with: None,
        },
        work_coverage: None,
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

#[test]
fn request_correlation_expiry_filter_precedes_page_limit() {
    let f = Fixture::new();
    let store = f.open();
    let call = f.request();
    let mut value: HumanRequest = serde_json::from_value(serde_json::json!({
        "schema_version":1,"id":HumanRequestId::from_bytes([1;16]),"revision":1,
        "operation_id":"fixture","created_at":f.now(),"updated_at":f.now(),
        "project_id":project(),"project_key":null,"owner_host":HostId::from_bytes([8;16]),
        "chair_session_id":"chair-a","chair_actor":actor(2),
        "request":{"kind":"question","title":"Next","body":"What next?"},
        "state":"open","answer_id":null,"closure_reason":null
    }))
    .unwrap();
    for n in 1..=5 {
        value.id = HumanRequestId::from_bytes([n; 16]);
        value.request.deadline = if n == 1 || n == 4 {
            Some(f.now())
        } else {
            None
        };
        store.create_human_request(&value, &call).unwrap();
    }
    let mut after = None;
    for n in [2, 3, 5] {
        let page = store
            .human_requests(project(), None, false, after, 1, &call)
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, HumanRequestId::from_bytes([n; 16]));
        assert_eq!(page.items[0].state, RequestState::Open);
        assert_eq!(page.truncated, n != 5);
        after = Some(page.items[0].id);
    }
    let all = store
        .human_requests(project(), None, true, None, 10, &call)
        .unwrap();
    assert_eq!(all.items.len(), 5);
    assert_eq!(all.items[0].state, RequestState::Expired);
    assert_eq!(all.items[3].state, RequestState::Expired);
}
