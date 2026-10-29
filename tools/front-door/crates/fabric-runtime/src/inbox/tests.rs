use super::*;
use provenant_core::{HostId, MonoTime, PathResolver, ProjectId, RootInputs, TestClock, WallTime};
use std::{
    io,
    path::{Path, PathBuf},
};

struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
        if path.exists() {
            path.canonicalize()
        } else {
            Ok(self
                .resolve_existing_ancestor(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?
                .join(path.file_name().ok_or(io::ErrorKind::InvalidInput)?))
        }
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: StateRoot,
    identity: Identity,
    clock: TestClock,
    cancel: CancelToken,
    registrations: std::sync::Mutex<std::collections::HashMap<String, AnswerTarget>>,
}
impl Fixture {
    fn new() -> Self {
        let temp = crate::test_support::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let mut roots = RootInputs::new(base.join("home"), base.join("repo"));
        roots.state_dir = Some(base.join("state"));
        let host = HostId::from_bytes([1; 16]);
        let identity = Identity {
            project: ProjectId::from_bytes([2; 16]),
            host,
            inbox: crate::identity::Address::new(host, "chair").unwrap(),
            client: "test".into(),
            cwd: provenant_records::AbsolutePath::new(&base).unwrap(),
            common_dir: provenant_records::AbsolutePath::new(&base).unwrap(),
            federation: None,
        };
        Self {
            _temp: temp,
            root: roots.resolve(&Resolver).unwrap().state,
            identity,
            clock: TestClock::new(
                WallTime::from_micros(2_000_000_000_000_000).unwrap(),
                MonoTime::from_micros(0),
            ),
            cancel: CancelToken::new(),
            registrations: Default::default(),
        }
    }
    fn service(&self) -> InboxService<'_> {
        InboxService::open(&self.root, &self.identity, &self.clock, &self.cancel).unwrap()
    }
    fn call(&self, input: InboxInput) -> Result<InboxResult, InboxError> {
        self.service().execute(input, &self.cancel)
    }
}
fn config() -> crate::config::FabricConfig {
    serde_json::from_value(serde_json::json!({"agent_label":"chair","client_label":"test","home_hosts":{},"peers":{},"limits":{"body_bytes":65536,"input_frame_bytes":1048576,"json_depth":64,"page_rows":100,"output_lines":500,"output_bytes":65536,"result_bytes":1048576,"lease_seconds":3600,"peer_seconds":30,"graph_timeout_seconds":14400,"bundle_bytes":67108864}})).unwrap()
}
fn op(text: &str) -> Option<OperationId> {
    Some(OperationId::parse(text).unwrap())
}

fn register(f: &Fixture, session: &str) {
    f.registrations.lock().unwrap().insert(
        session.into(),
        AnswerTarget {
            extra: Default::default(),
            host: f.identity.host,
            project: f.identity.project,
            session: session.into(),
            actor: f.identity.inbox.actor,
        },
    );
}

fn offer(
    f: &Fixture,
    session: &str,
    limit: u64,
    bytes: usize,
) -> Result<provenant_fabric_store::Page<AnswerOffer>, InboxError> {
    offer_until_ack(
        &f.root,
        &f.identity,
        &config(),
        false,
        &f.clock,
        session,
        limit,
        bytes,
        &f.cancel,
        || {
            f.registrations
                .lock()
                .unwrap()
                .get(session)
                .cloned()
                .ok_or(InboxError::Conflict)
        },
    )
}

#[test]
fn offer_until_ack_repeats_after_restart_stops_after_ack_and_preserves_overflow() {
    let f = Fixture::new();
    register(&f, "session-a");
    let mut pending = Vec::new();
    for n in 0..3 {
        let created = receipt(
            f.call(ask(RequestKind::Decision, &format!("ask-{n}")))
                .unwrap(),
        );
        pending.push(receipt(
            f.call(answer(created.request_id, &format!("answer-{n}")))
                .unwrap(),
        ));
    }
    let first = offer(&f, "session-a", 1, MAX_OFFER_BYTES).unwrap();
    assert!(first.truncated);
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].delivery.state, AnswerDeliveryState::Offered);
    assert_eq!(first.items[0].delivery.attempts, 1);
    let again = offer(&f, "session-a", 1, MAX_OFFER_BYTES).unwrap();
    assert_eq!(again.items[0].answer.id, first.items[0].answer.id);
    assert_eq!(again.items[0].delivery.attempts, 2);
    assert_eq!(again.items[0].answer, first.items[0].answer);
    for saved in &pending {
        let InboxResult::Item { item, .. } = f.call(show(saved.request_id)).unwrap() else {
            panic!("item")
        };
        if item.answer.as_ref().unwrap().id != first.items[0].answer.id {
            assert_eq!(
                item.delivery.as_ref().unwrap().state,
                AnswerDeliveryState::Queued
            );
            assert_eq!(item.delivery.as_ref().unwrap().attempts, 0);
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let batch = offer(&f, "session-a", 1, MAX_OFFER_BYTES).unwrap();
        let Some(item) = batch.items.first() else {
            break;
        };
        assert!(seen.insert(item.answer.id));
        f.call(InboxInput::Acknowledge {
            extra: Default::default(),
            id: item.answer.request_id,
            answer_id: item.answer.id,
            session: "session-a".into(),
            operation_id: None,
        })
        .unwrap();
    }
    assert_eq!(seen.len(), 3);
    assert!(
        !offer(&f, "session-a", 1, MAX_OFFER_BYTES)
            .unwrap()
            .truncated
    );
}

#[test]
fn offer_until_ack_rejects_unregistered_and_wrong_targets_without_mutation() {
    let mut f = Fixture::new();
    let created = receipt(f.call(ask(RequestKind::Decision, "ask")).unwrap());
    f.call(answer(created.request_id, "answer")).unwrap();
    assert_eq!(
        offer(&f, "session-a", 50, MAX_OFFER_BYTES)
            .unwrap_err()
            .code(),
        "conflict"
    );
    register(&f, "session-a");
    assert_eq!(
        offer(&f, "session-b", 50, MAX_OFFER_BYTES)
            .unwrap_err()
            .code(),
        "conflict"
    );
    register(&f, "session-b");
    assert!(
        offer(&f, "session-b", 50, MAX_OFFER_BYTES)
            .unwrap()
            .items
            .is_empty()
    );
    let original = f.identity.clone();
    f.identity.host = HostId::from_bytes([9; 16]);
    assert_eq!(
        offer(&f, "session-a", 50, MAX_OFFER_BYTES)
            .unwrap_err()
            .code(),
        "conflict"
    );
    f.identity = original.clone();
    f.identity.inbox = crate::identity::Address::new(f.identity.host, "other").unwrap();
    assert_eq!(
        offer(&f, "session-a", 50, MAX_OFFER_BYTES)
            .unwrap_err()
            .code(),
        "conflict"
    );
    f.identity = original.clone();
    f.identity.project = ProjectId::from_bytes([9; 16]);
    assert_eq!(
        offer(&f, "session-a", 50, MAX_OFFER_BYTES)
            .unwrap_err()
            .code(),
        "conflict"
    );
    f.identity = original;
    let InboxResult::Item { item, .. } = f.call(show(created.request_id)).unwrap() else {
        panic!("item")
    };
    assert_eq!(item.delivery.unwrap().state, AnswerDeliveryState::Queued);
    assert_eq!(
        offer(&f, "session-a", 50, MAX_OFFER_BYTES)
            .unwrap()
            .items
            .len(),
        1
    );
}

#[test]
fn offer_until_ack_byte_budget_counts_serialised_envelope_and_leaves_answer_queued() {
    let f = Fixture::new();
    register(&f, "session-a");
    let created = receipt(f.call(ask(RequestKind::Question, "ask")).unwrap());
    f.call(InboxInput::Answer {
        extra: Default::default(),
        id: created.request_id,
        action: AnswerAction::Reply,
        option_id: None,
        text: Some("\"\\\n".repeat(1000)),
        expect_revision: 1,
        channel: InteractionChannel::Chat,
        operation_id: op("answer"),
    })
    .unwrap();
    let tiny = offer(&f, "session-a", 50, 128).unwrap();
    assert!(tiny.items.is_empty());
    assert!(tiny.truncated);
    let InboxResult::Item { item, .. } = f.call(show(created.request_id)).unwrap() else {
        panic!("item")
    };
    assert_eq!(item.delivery.unwrap().state, AnswerDeliveryState::Queued);
    let batch = offer(&f, "session-a", 50, MAX_OFFER_BYTES).unwrap();
    assert_eq!(batch.items.len(), 1);
    assert!(serde_json::to_vec(&batch).unwrap().len() <= MAX_OFFER_BYTES);
    assert!(!batch.truncated);
    for (rows, bytes) in [
        (0, MAX_OFFER_BYTES),
        (101, MAX_OFFER_BYTES),
        (50, 63),
        (50, MAX_OFFER_BYTES + 1),
    ] {
        assert!(offer(&f, "session-a", rows, bytes).is_err());
    }
}

#[test]
fn oversized_answer_is_refused_and_small_budget_skips_large_answers() {
    let f = Fixture::new();
    register(&f, "session-a");
    let large = receipt(f.call(ask(RequestKind::Question, "large")).unwrap());
    let make_answer = |text: String, operation: &str| InboxInput::Answer {
        extra: Default::default(),
        id: large.request_id,
        action: AnswerAction::Reply,
        option_id: None,
        text: Some(text),
        expect_revision: 1,
        channel: InteractionChannel::Chat,
        operation_id: op(operation),
    };
    assert_eq!(
        f.call(make_answer("\u{1}".repeat(4000), "too-large"))
            .unwrap_err()
            .code(),
        "invalid_input"
    );
    f.call(make_answer("x".repeat(3000), "large-answer"))
        .unwrap();
    let small = receipt(f.call(ask(RequestKind::Decision, "small")).unwrap());
    let saved = receipt(f.call(answer(small.request_id, "small-answer")).unwrap());
    let batch = offer(&f, "session-a", 50, 2048).unwrap();
    assert!(batch.truncated);
    assert_eq!(batch.items.len(), 1);
    assert_eq!(batch.items[0].answer.id, saved.answer_id.unwrap());
    assert!(serde_json::to_vec(&batch).unwrap().len() <= 2048);
    let full = offer(&f, "session-a", 50, MAX_OFFER_BYTES).unwrap();
    assert_eq!(full.items.len(), 2);
}

#[test]
fn concurrent_offer_and_ack_never_resurrects_delivery() {
    let f = Fixture::new();
    register(&f, "session-a");
    let created = receipt(f.call(ask(RequestKind::Decision, "race-ask")).unwrap());
    let saved = receipt(f.call(answer(created.request_id, "race-answer")).unwrap());
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            offer(&f, "session-a", 50, MAX_OFFER_BYTES).unwrap()
        });
        let second = scope.spawn(|| {
            barrier.wait();
            f.call(InboxInput::Acknowledge {
                extra: Default::default(),
                id: created.request_id,
                answer_id: saved.answer_id.unwrap(),
                session: "session-a".into(),
                operation_id: op("race-ack"),
            })
            .unwrap()
        });
        assert!(first.join().unwrap().items.len() <= 1);
        second.join().unwrap();
    });
    assert!(
        offer(&f, "session-a", 50, MAX_OFFER_BYTES)
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn offer_until_ack_rebind_never_transfers_old_answers() {
    let mut f = Fixture::new();
    register(&f, "session-a");
    let created = receipt(f.call(ask(RequestKind::Decision, "ask")).unwrap());
    f.call(answer(created.request_id, "answer")).unwrap();
    let original = f.identity.inbox.clone();
    f.identity.inbox = crate::identity::Address::new(f.identity.host, "new-chair").unwrap();
    register(&f, "session-a");
    assert!(
        offer(&f, "session-a", 50, MAX_OFFER_BYTES)
            .unwrap()
            .items
            .is_empty()
    );
    f.identity.inbox = original;
    assert_eq!(
        offer(&f, "session-a", 50, MAX_OFFER_BYTES)
            .unwrap_err()
            .code(),
        "conflict"
    );
    let InboxResult::Item { item, .. } = f.call(show(created.request_id)).unwrap() else {
        panic!("item")
    };
    assert_eq!(item.delivery.unwrap().state, AnswerDeliveryState::Queued);
}

#[test]
fn delivery_capabilities_publish_only_tested_fabric_fallback_for_each_provider() {
    let capabilities = delivery_capabilities();
    assert_eq!(
        capabilities.iter().map(|c| c.provider).collect::<Vec<_>>(),
        provenant_records::ADAPTER_SPECS
            .iter()
            .map(|spec| spec.id)
            .collect::<Vec<_>>()
    );
    for capability in capabilities {
        assert_eq!(capability.transport, AnswerTransport::FabricMail);
        assert!(!capability.live_injection);
        assert!(!capability.local_offer_reader);
        let mut f = Fixture::new();
        f.identity.client = capability.provider.spec().name.into();
        register(&f, "session-a");
        let created = receipt(f.call(ask(RequestKind::Decision, "ask")).unwrap());
        let saved = receipt(f.call(answer(created.request_id, "answer")).unwrap());
        let batch = offer(&f, "session-a", 50, MAX_OFFER_BYTES).unwrap();
        assert_eq!(batch.items[0].delivery.transport, capability.transport);
        assert_eq!(batch.items[0].answer.id, saved.answer_id.unwrap());
        let request = Request {
            clock: &f.clock,
            cancel: &f.cancel,
        };
        let mail = f
            .service()
            .store
            .read_mail(
                f.identity.project,
                f.identity.inbox.actor,
                Some(0),
                50,
                &request,
            )
            .unwrap();
        assert_eq!(mail.items.len(), 1);
        assert_eq!(mail.items[0].message.id, batch.items[0].delivery.message_id);
        assert_eq!(
            mail.items[0].message.recipients,
            vec![f.identity.inbox.actor]
        );
    }
}
fn ask(kind: RequestKind, id: &str) -> InboxInput {
    InboxInput::Ask {
        extra: Default::default(),
        session: "session-a".into(),
        request: Box::new(RequestDraft {
            extra: Default::default(),
            work: None,
            deadline: None,
            supersedes: None,
            kind,
            title: "Select repair".into(),
            body: "Which repair should proceed?".into(),
            blocking: kind == RequestKind::Decision,
            options: vec![RequestOption {
                extra: Default::default(),
                id: "repair".into(),
                label: "Repair".into(),
                description: "Repair the failing check".into(),
            }],
            recommended_option_id: Some("repair".into()),
            proceeding_with: (kind == RequestKind::Assumption).then(|| "Repair the check".into()),
        }),
        operation_id: op(id),
    }
}
fn receipt(result: InboxResult) -> InboxReceipt {
    match result {
        InboxResult::Saved { receipt } => receipt,
        _ => panic!("expected receipt"),
    }
}
fn answer(id: HumanRequestId, operation: &str) -> InboxInput {
    InboxInput::Answer {
        extra: Default::default(),
        id,
        action: AnswerAction::Choose,
        option_id: Some("repair".into()),
        text: None,
        expect_revision: 1,
        channel: InteractionChannel::Cli,
        operation_id: op(operation),
    }
}
fn show(id: HumanRequestId) -> InboxInput {
    InboxInput::List {
        extra: Default::default(),
        id: Some(id),
        session: None,
        state: InboxFilter::All,
        limit: 50,
        cursor: None,
    }
}

#[test]
fn inbox_request_and_answer_replay_survive_restart() {
    let f = Fixture::new();
    let input = ask(RequestKind::Decision, "create");
    let created = receipt(f.call(input.clone()).unwrap());
    assert_eq!(created.revision, 1);
    assert_eq!(receipt(f.call(input).unwrap()), created);
    let resolved = receipt(f.call(answer(created.request_id, "resolve")).unwrap());
    assert_eq!(resolved.revision, 2);
    assert_eq!(resolved.state, RequestState::Answered);
    assert_eq!(
        resolved.delivery.as_ref().unwrap().state,
        AnswerDeliveryState::Queued
    );
    assert_eq!(
        receipt(f.call(answer(created.request_id, "resolve")).unwrap()),
        resolved
    );
    let InboxResult::Item { item, .. } = f.call(show(created.request_id)).unwrap() else {
        panic!("item")
    };
    assert_eq!(item.history.len(), 2);
    assert_eq!(item.answer.as_ref().unwrap().request_revision, 1);
    assert_eq!(
        item.answer.as_ref().unwrap().id,
        resolved.answer_id.unwrap()
    );
    assert!(item.request.created_at.to_string().ends_with('Z'));
}

#[test]
fn inbox_operation_ids_bind_content_and_actor_before_current_revision() {
    let mut f = Fixture::new();
    let created = receipt(f.call(ask(RequestKind::Decision, "create")).unwrap());
    let input = answer(created.request_id, "answer");
    let resolved = receipt(f.call(input.clone()).unwrap());
    let mut changed = input.clone();
    if let InboxInput::Answer { channel, .. } = &mut changed {
        *channel = InteractionChannel::Console;
    }
    assert_eq!(f.call(changed).unwrap_err().code(), "operation_conflict");
    assert_eq!(receipt(f.call(input.clone()).unwrap()), resolved);
    f.identity.inbox = crate::identity::Address::new(f.identity.host, "other-chair").unwrap();
    assert_eq!(f.call(input).unwrap_err().code(), "operation_conflict");
}

#[test]
fn inbox_all_kinds_preserve_text_and_reject_stale_or_invalid_answers() {
    let f = Fixture::new();
    for (kind, action, state) in [
        (
            RequestKind::Question,
            AnswerAction::Reply,
            RequestState::Answered,
        ),
        (
            RequestKind::Assumption,
            AnswerAction::Override,
            RequestState::Overridden,
        ),
        (
            RequestKind::Assumption,
            AnswerAction::Confirm,
            RequestState::Confirmed,
        ),
    ] {
        let created = receipt(f.call(ask(kind, &format!("create-{action:?}"))).unwrap());
        let mut input = InboxInput::Answer {
            extra: Default::default(),
            id: created.request_id,
            action,
            option_id: None,
            text: (action != AnswerAction::Confirm).then(|| "  exact answer\n".into()),
            expect_revision: 1,
            channel: InteractionChannel::PhoneRemote,
            operation_id: op(&format!("answer-{action:?}")),
        };
        let resolved = receipt(f.call(input.clone()).unwrap());
        assert_eq!(resolved.state, state);
        if let InboxInput::Answer { operation_id, .. } = &mut input {
            *operation_id = op(&format!("stale-{action:?}"));
        }
        assert_eq!(f.call(input).unwrap_err().code(), "conflict");
        let InboxResult::Item { item, .. } = f.call(show(created.request_id)).unwrap() else {
            panic!("item")
        };
        assert_eq!(
            item.answer.as_ref().unwrap().text,
            (action != AnswerAction::Confirm).then(|| "  exact answer\n".into())
        );
        assert_eq!(
            item.answer.as_ref().unwrap().channel,
            InteractionChannel::PhoneRemote
        );
    }
    let created = receipt(
        f.call(ask(RequestKind::Assumption, "invalid-choice"))
            .unwrap(),
    );
    assert_eq!(
        f.call(answer(created.request_id, "wrong-kind"))
            .unwrap_err()
            .code(),
        "invalid_input"
    );
    let created = receipt(
        f.call(ask(RequestKind::Decision, "invalid-option"))
            .unwrap(),
    );
    let mut input = answer(created.request_id, "wrong-option");
    if let InboxInput::Answer { option_id, .. } = &mut input {
        *option_id = Some("absent".into());
    }
    assert_eq!(f.call(input).unwrap_err().code(), "invalid_input");
}

#[test]
fn inbox_acknowledgement_requires_the_exact_host_project_session_and_actor() {
    let mut f = Fixture::new();
    let created = receipt(f.call(ask(RequestKind::Decision, "create")).unwrap());
    let resolved = receipt(f.call(answer(created.request_id, "answer")).unwrap());
    let mut input = InboxInput::Acknowledge {
        extra: Default::default(),
        id: created.request_id,
        answer_id: resolved.answer_id.unwrap(),
        session: "wrong-session".into(),
        operation_id: op("ack"),
    };
    assert_eq!(f.call(input.clone()).unwrap_err().code(), "conflict");
    if let InboxInput::Acknowledge { session, .. } = &mut input {
        *session = "session-a".into();
    }
    let original = f.identity.clone();
    f.identity.inbox = crate::identity::Address::new(f.identity.host, "other").unwrap();
    assert_eq!(f.call(input.clone()).unwrap_err().code(), "conflict");
    f.identity = original.clone();
    f.identity.host = HostId::from_bytes([9; 16]);
    assert_eq!(f.call(input.clone()).unwrap_err().code(), "conflict");
    f.identity = original.clone();
    f.identity.project = ProjectId::from_bytes([9; 16]);
    assert_eq!(f.call(input.clone()).unwrap_err().code(), "not_found");
    f.identity = original;
    let ack = receipt(f.call(input.clone()).unwrap());
    assert_eq!(
        ack.delivery.as_ref().unwrap().state,
        AnswerDeliveryState::Acknowledged
    );
    assert_eq!(receipt(f.call(input).unwrap()), ack);
    // A new operation ID acknowledging the same consumed answer is harmless.
    let again = receipt(
        f.call(InboxInput::Acknowledge {
            extra: Default::default(),
            id: created.request_id,
            answer_id: resolved.answer_id.unwrap(),
            session: "session-a".into(),
            operation_id: op("ack-again"),
        })
        .unwrap(),
    );
    assert_eq!(again.delivery, ack.delivery);
}

#[test]
fn inbox_fabric_mail_is_correlated_and_read_cursor_does_not_acknowledge() {
    let f = Fixture::new();
    let created = receipt(f.call(ask(RequestKind::Decision, "create")).unwrap());
    let resolved = receipt(f.call(answer(created.request_id, "answer")).unwrap());
    let service = f.service();
    let request = Request {
        clock: &f.clock,
        cancel: &f.cancel,
    };
    let mail = service
        .store
        .read_mail(
            f.identity.project,
            f.identity.inbox.actor,
            Some(0),
            50,
            &request,
        )
        .unwrap();
    assert_eq!(mail.items.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&mail.items[0].message.body).unwrap();
    assert_eq!(body["target_session"], "session-a");
    assert_eq!(
        body["answer"]["id"],
        resolved.answer_id.unwrap().to_string()
    );
    assert_eq!(body["request_id"], created.request_id.to_string());
    service
        .store
        .mark_mail_read(
            f.identity.project,
            f.identity.inbox.actor,
            mail.items[0].seq,
            &request,
        )
        .unwrap();
    let InboxResult::Item { item, .. } = f.call(show(created.request_id)).unwrap() else {
        panic!("item")
    };
    assert_eq!(item.delivery.unwrap().state, AnswerDeliveryState::Queued);
}

#[test]
fn inbox_concurrent_answers_have_one_winner_and_one_mail() {
    let f = Fixture::new();
    let created = receipt(f.call(ask(RequestKind::Decision, "create")).unwrap());
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let service = f.service();
            barrier.wait();
            service.execute(answer(created.request_id, "first"), &f.cancel)
        });
        let second = scope.spawn(|| {
            let service = f.service();
            barrier.wait();
            service.execute(answer(created.request_id, "second"), &f.cancel)
        });
        vec![first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .find_map(|result| result.as_ref().err())
            .unwrap()
            .code(),
        "conflict"
    );
    let service = f.service();
    let request = Request {
        clock: &f.clock,
        cancel: &f.cancel,
    };
    assert_eq!(
        service
            .store
            .read_mail(
                f.identity.project,
                f.identity.inbox.actor,
                Some(0),
                100,
                &request
            )
            .unwrap()
            .items
            .len(),
        1
    );
    let InboxResult::Item { item, .. } = f.call(show(created.request_id)).unwrap() else {
        panic!("item")
    };
    assert_eq!(item.history.len(), 2);
}

#[test]
fn inbox_close_preserves_the_answer_and_history_and_replays() {
    let f = Fixture::new();
    let created = receipt(f.call(ask(RequestKind::Decision, "create")).unwrap());
    let resolved = receipt(f.call(answer(created.request_id, "answer")).unwrap());
    let input = InboxInput::Close {
        extra: Default::default(),
        id: created.request_id,
        state: RequestState::Superseded,
        reason: "Replaced by a new scoped request".into(),
        expect_revision: 2,
        operation_id: op("close"),
    };
    let closed = receipt(f.call(input.clone()).unwrap());
    assert_eq!(closed.state, RequestState::Superseded);
    assert_eq!(receipt(f.call(input).unwrap()), closed);
    let InboxResult::Item { item, .. } = f.call(show(created.request_id)).unwrap() else {
        panic!("item")
    };
    assert_eq!(item.history.len(), 3);
    assert_eq!(item.answer.unwrap().id, resolved.answer_id.unwrap());
    assert_eq!(item.delivery.unwrap().state, AnswerDeliveryState::Queued);
}

#[test]
fn inbox_paging_binds_filters_snapshot_and_limits() {
    let f = Fixture::new();
    for n in 0..3 {
        f.call(ask(RequestKind::Question, &format!("create-{n}")))
            .unwrap();
    }
    let list = |session: Option<String>, cursor| InboxInput::List {
        extra: Default::default(),
        id: None,
        session,
        state: InboxFilter::Open,
        limit: 1,
        cursor,
    };
    let InboxResult::Listed {
        items,
        next_cursor,
        truncated,
        ..
    } = f.call(list(None, None)).unwrap()
    else {
        panic!("list")
    };
    assert_eq!(items.len(), 1);
    assert!(truncated);
    let cursor = next_cursor.unwrap();
    let InboxResult::Listed { items: next, .. } = f.call(list(None, Some(cursor.clone()))).unwrap()
    else {
        panic!("list")
    };
    assert_ne!(items[0].id, next[0].id);
    assert_eq!(
        f.call(list(Some("other-session".into()), Some(cursor.clone())))
            .unwrap_err()
            .code(),
        "cursor_expired"
    );
    f.call(ask(RequestKind::Question, "change")).unwrap();
    assert_eq!(
        f.call(list(None, Some(cursor))).unwrap_err().code(),
        "cursor_expired"
    );
    let input = InboxInput::List {
        extra: Default::default(),
        id: None,
        session: None,
        state: InboxFilter::All,
        limit: 101,
        cursor: None,
    };
    assert_eq!(f.call(input).unwrap_err().code(), "invalid_input");
}

#[test]
fn inbox_worker_mutations_are_refused_without_opening_store() {
    let mut f = Fixture::new();
    let mut config = config();
    let key = crate::identity::CrossHostProjectKey::from_home(
        Path::new("/home/test"),
        Path::new("/home/test/repo/.git"),
    )
    .unwrap();
    config
        .home_hosts
        .insert(key.as_str().into(), HostId::from_bytes([9; 16]));
    f.identity.federation = Some(crate::identity::FederatedProject {
        project: f.identity.project,
        key,
    });
    let result = execute(
        &f.root,
        &f.identity,
        &config,
        true,
        &f.clock,
        ask(RequestKind::Decision, "create"),
        &f.cancel,
        &|_, _| panic!("lookup must not run"),
        &|_, _| panic!("control lookup must not run"),
    );
    assert_eq!(result.unwrap_err().code(), "lane_coordination_refused");
    assert!(!f.root.as_path().join("fabric").exists());
    let result = execute(
        &f.root,
        &f.identity,
        &config,
        false,
        &f.clock,
        ask(RequestKind::Decision, "create"),
        &f.cancel,
        &|_, _| panic!("lookup must not run"),
        &|_, _| panic!("control lookup must not run"),
    );
    assert!(result.is_ok());
}

#[test]
fn request_correlation_saves_exact_coverage_once_and_requires_host_lookup() {
    let f = Fixture::new();
    let mut input = ask(RequestKind::Question, "linked");
    let work = RequestWork {
        extra: Default::default(),
        run_id: provenant_core::RunId::from_bytes([3; 16]),
        task_id: Some(provenant_core::TaskId::from_bytes([4; 16])),
        attempt_id: None,
        repository: None,
        head: None,
    };
    if let InboxInput::Ask { request, .. } = &mut input {
        request.work = Some(work.clone());
    }
    assert_eq!(
        f.call(input.clone()).unwrap_err().code(),
        "inbox_unavailable"
    );
    let calls = std::cell::Cell::new(0);
    for (n, coverage) in [
        WorkCoverage::Unindexed,
        WorkCoverage::Verified,
        WorkCoverage::Partial,
        WorkCoverage::RepositoryUnavailable,
        WorkCoverage::HeadUnavailable,
        WorkCoverage::Limit,
        WorkCoverage::Mismatch,
    ]
    .into_iter()
    .enumerate()
    {
        let lookup = |selected: &RequestWork, _: &Request<'_>| {
            assert_eq!(selected, &work);
            calls.set(calls.get() + 1);
            Ok(coverage)
        };
        let service = f.service().with_work_lookup(&lookup);
        if let InboxInput::Ask { operation_id, .. } = &mut input {
            *operation_id = op(&format!("linked-{n}"));
        }
        let result = service.execute(input.clone(), &f.cancel);
        if coverage == WorkCoverage::Mismatch {
            assert_eq!(result.unwrap_err().code(), "work_mismatch");
            continue;
        }
        let saved = receipt(result.unwrap());
        let InboxResult::Item { item, .. } = f.call(show(saved.request_id)).unwrap() else {
            panic!("item")
        };
        assert_eq!(saved.work_coverage, Some(coverage));
        assert_eq!(item.request.schema_version, 2);
        assert_eq!(
            item.request.verified_work().is_some(),
            coverage == WorkCoverage::Verified
        );
        assert_eq!(item.request.request.work, Some(work.clone()));
        assert_eq!(item.request.work_coverage, Some(coverage));
        assert_eq!(item.history[0].work_coverage, Some(coverage));
        // Replay with no resolver available must retain the creation-time result.
        assert_eq!(receipt(f.call(input.clone()).unwrap()), saved);
    }
    assert_eq!(calls.get(), 7);
    assert_eq!(
        f.service()
            .store
            .pending_human_requests(
                100,
                1_048_576,
                &Request {
                    clock: &f.clock,
                    cancel: &f.cancel,
                }
            )
            .unwrap()
            .items
            .len(),
        6
    );
}

#[test]
fn request_correlation_v2_survives_answer_close_and_acknowledgement() {
    let f = Fixture::new();
    let work = RequestWork {
        extra: Default::default(),
        run_id: provenant_core::RunId::from_bytes([3; 16]),
        task_id: Some(provenant_core::TaskId::from_bytes([4; 16])),
        attempt_id: None,
        repository: None,
        head: None,
    };
    let mut input = ask(RequestKind::Decision, "linked-roundtrip");
    if let InboxInput::Ask { request, .. } = &mut input {
        request.work = Some(work.clone());
    }
    let created = receipt(
        f.service()
            .with_work_lookup(&|_, _| Ok(WorkCoverage::Verified))
            .execute(input, &f.cancel)
            .unwrap(),
    );
    let check = |history_len| {
        let InboxResult::Item { item, .. } = f.call(show(created.request_id)).unwrap() else {
            panic!("item")
        };
        assert_eq!(item.history.len(), history_len);
        for record in std::iter::once(&item.request).chain(&item.history) {
            assert_eq!(record.schema_version, 2);
            assert_eq!(record.request.work.as_ref(), Some(&work));
            assert_eq!(record.work_coverage, Some(WorkCoverage::Verified));
            assert_eq!(record.verified_work(), Some(&work));
        }
        item
    };
    check(1);
    let answered = receipt(f.call(answer(created.request_id, "linked-answer")).unwrap());
    check(2);
    f.call(InboxInput::Close {
        extra: Default::default(),
        id: created.request_id,
        state: RequestState::Superseded,
        reason: "Replaced by a new scoped request".into(),
        expect_revision: 2,
        operation_id: op("linked-close"),
    })
    .unwrap();
    check(3);
    f.call(InboxInput::Acknowledge {
        extra: Default::default(),
        id: created.request_id,
        answer_id: answered.answer_id.unwrap(),
        session: "session-a".into(),
        operation_id: op("linked-ack"),
    })
    .unwrap();
    let item = check(3);
    assert_eq!(item.request.state, RequestState::Superseded);
    assert_eq!(item.answer.unwrap().id, answered.answer_id.unwrap());
    assert_eq!(
        item.delivery.unwrap().state,
        AnswerDeliveryState::Acknowledged
    );
}

#[test]
fn request_correlation_absent_links_preserve_old_operation_digest() {
    let input = ask(RequestKind::Question, "old-ask");
    let mut old = serde_json::to_value(&input).unwrap();
    old["request"].as_object_mut().unwrap().remove("work");
    let f = Fixture::new();
    let request = Request {
        clock: &f.clock,
        cancel: &f.cancel,
    };
    let saved = f.call(input.clone()).unwrap();
    let replay: InboxResult = f
        .service()
        .store
        .coordination_operation(
            f.identity.project,
            f.identity.inbox.actor,
            &OperationId::parse("old-ask").unwrap(),
            &("human_inbox_v1", old),
            &request,
            || -> Result<InboxResult, InboxError> { panic!("replay mutated") },
        )
        .unwrap();
    assert_eq!(saved, replay);
}

#[test]
fn request_correlation_lookup_never_holds_fabric_write_lock() {
    let f = Fixture::new();
    let mut input = ask(RequestKind::Question, "outside-transaction");
    if let InboxInput::Ask { request, .. } = &mut input {
        request.work = Some(RequestWork {
            extra: Default::default(),
            run_id: provenant_core::RunId::from_bytes([3; 16]),
            task_id: None,
            attempt_id: None,
            repository: None,
            head: None,
        });
    }
    let lookup = |_: &RequestWork, _: &Request<'_>| {
        let db =
            rusqlite::Connection::open(f.root.as_path().join("fabric/fabric.sqlite3")).unwrap();
        db.busy_timeout(std::time::Duration::ZERO).unwrap();
        db.execute_batch("BEGIN IMMEDIATE; ROLLBACK")
            .expect("lookup blocked unrelated Fabric writer");
        // A competing preparation wins after our preflight. The transaction
        // must return that receipt, rather than our later observation.
        f.service()
            .with_work_lookup(&|_, _| Ok(WorkCoverage::Unindexed))
            .execute(input.clone(), &f.cancel)
            .unwrap();
        Ok(WorkCoverage::Verified)
    };
    let saved = receipt(
        f.service()
            .with_work_lookup(&lookup)
            .execute(input.clone(), &f.cancel)
            .unwrap(),
    );
    assert_eq!(saved.work_coverage, Some(WorkCoverage::Unindexed));
}

#[test]
fn request_correlation_legacy_records_keep_v1_wire_shape_and_answer() {
    let f = Fixture::new();
    let saved = receipt(f.call(ask(RequestKind::Decision, "legacy")).unwrap());
    let InboxResult::Item { item, .. } = f.call(show(saved.request_id)).unwrap() else {
        panic!("item")
    };
    let mut old = serde_json::to_value(&item.request).unwrap();
    old.as_object_mut().unwrap().remove("work_coverage");
    old["request"].as_object_mut().unwrap().remove("work");
    let bytes = serde_json::to_vec(&old).unwrap();
    let db = rusqlite::Connection::open(f.root.as_path().join("fabric/fabric.sqlite3")).unwrap();
    for table in ["human_requests", "human_request_history"] {
        db.execute(&format!("UPDATE {table} SET record=?1"), [&bytes])
            .unwrap();
    }
    let InboxResult::Item { item, .. } = f.call(show(saved.request_id)).unwrap() else {
        panic!("item")
    };
    assert_eq!(serde_json::to_value(&item.request).unwrap(), old);
    let listed = f
        .call(serde_json::from_value(serde_json::json!({"op":"list"})).unwrap())
        .unwrap();
    let InboxResult::Listed { items, .. } = listed else {
        panic!("list")
    };
    assert_eq!(items, vec![item.request]);
    f.call(answer(saved.request_id, "legacy-answer")).unwrap();
    let InboxResult::Item { item, .. } = f.call(show(saved.request_id)).unwrap() else {
        panic!("item")
    };
    assert_eq!(item.request.schema_version, 1);
    assert!(
        serde_json::to_value(&item.request)
            .unwrap()
            .get("work_coverage")
            .is_none()
    );
}

#[test]
fn request_correlation_repository_bound_counts_encoded_wire_bytes() {
    use std::os::unix::ffi::OsStringExt;
    for (size, valid) in [(2048, true), (2049, false)] {
        let mut bytes = vec![b'x'; size];
        bytes[0] = 0xff;
        let path = PathBuf::from(std::ffi::OsString::from_vec([vec![b'/'], bytes].concat()));
        let mut input = ask(RequestKind::Question, "native-path");
        if let InboxInput::Ask { request, .. } = &mut input {
            request.work = Some(RequestWork {
                extra: Default::default(),
                run_id: provenant_core::RunId::from_bytes([3; 16]),
                task_id: None,
                attempt_id: None,
                repository: Some(provenant_records::AbsolutePath::new(&path).unwrap()),
                head: None,
            });
        }
        assert_eq!(input.validate().is_ok(), valid);
    }
}

fn extended_ask(operation: &str, fields: serde_json::Value) -> InboxInput {
    let mut value = serde_json::to_value(ask(RequestKind::Decision, operation)).unwrap();
    value["request"]
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}
fn replacement(id: HumanRequestId, revision: u64, operation: &str) -> InboxInput {
    extended_ask(
        operation,
        serde_json::json!({"supersedes":{"id":id,"expect_revision":revision}}),
    )
}
fn item(f: &Fixture, id: HumanRequestId) -> InboxItem {
    match f.call(show(id)).unwrap() {
        InboxResult::Item { item, .. } => *item,
        _ => panic!("item"),
    }
}

#[test]
fn replacement_work_mismatch_leaves_original_open_and_unindexed_is_saved() {
    let f = Fixture::new();
    let old = receipt(f.call(ask(RequestKind::Decision, "original-work")).unwrap());
    let original = item(&f, old.request_id);
    let mut input = replacement(old.request_id, 1, "replace-work");
    if let InboxInput::Ask { request, .. } = &mut input {
        request.work = Some(RequestWork {
            extra: Default::default(),
            run_id: provenant_core::RunId::from_bytes([3; 16]),
            task_id: None,
            attempt_id: None,
            repository: None,
            head: None,
        });
    }
    assert_eq!(
        f.service()
            .with_work_lookup(&|_, _| Ok(WorkCoverage::Mismatch))
            .execute(input.clone(), &f.cancel)
            .unwrap_err()
            .code(),
        "work_mismatch"
    );
    assert_eq!(item(&f, old.request_id), original);
    let saved = receipt(
        f.service()
            .with_work_lookup(&|_, _| Ok(WorkCoverage::Unindexed))
            .execute(input, &f.cancel)
            .unwrap(),
    );
    assert_eq!(saved.work_coverage, Some(WorkCoverage::Unindexed));
    assert_eq!(
        item(&f, old.request_id).request.state,
        RequestState::Superseded
    );
}

#[test]
fn request_correlation_supersession_preserves_answer_and_checks_owner_revision() {
    let mut f = Fixture::new();
    let old = receipt(f.call(ask(RequestKind::Decision, "original")).unwrap());
    let answered = receipt(f.call(answer(old.request_id, "answer")).unwrap());
    let original = item(&f, old.request_id);
    assert_eq!(
        f.call(replacement(old.request_id, 1, "stale"))
            .unwrap_err()
            .code(),
        "conflict"
    );
    let input = replacement(old.request_id, 2, "replace");
    let identity = f.identity.clone();
    for n in 0..3 {
        match n {
            0 => f.identity.host = HostId::from_bytes([9; 16]),
            1 => {
                f.identity.inbox = crate::identity::Address::new(f.identity.host, "other").unwrap()
            }
            _ => f.identity.project = ProjectId::from_bytes([9; 16]),
        }
        assert_eq!(
            f.call(input.clone()).unwrap_err().code(),
            if n == 2 { "not_found" } else { "conflict" }
        );
        f.identity = identity.clone();
    }
    let mut other_session = input.clone();
    if let InboxInput::Ask { session, .. } = &mut other_session {
        *session = "other".into();
    }
    assert_eq!(f.call(other_session).unwrap_err().code(), "conflict");
    let saved = receipt(f.call(input.clone()).unwrap());
    assert_eq!(receipt(f.call(input).unwrap()), saved);
    let closed = item(&f, old.request_id);
    assert_eq!(closed.request.state, RequestState::Superseded);
    assert_eq!(closed.request.revision, 3);
    assert_eq!(closed.answer, original.answer);
    assert_eq!(closed.delivery, original.delivery);
    assert_eq!(closed.request.answer_id, answered.answer_id);
    assert_eq!(closed.history.len(), 3);
    assert!(
        closed
            .request
            .closure_reason
            .unwrap()
            .contains(&saved.request_id.to_string())
    );
    assert_eq!(
        f.call(replacement(old.request_id, 3, "again"))
            .unwrap_err()
            .code(),
        "conflict"
    );
}

#[test]
fn request_correlation_supersession_is_atomic_with_receipt_and_concurrent_replacements() {
    let f = Fixture::new();
    let old = receipt(f.call(ask(RequestKind::Decision, "original")).unwrap());
    let db = rusqlite::Connection::open(f.root.as_path().join("fabric/fabric.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON coordination_operations WHEN NEW.id='replace' BEGIN SELECT RAISE(ABORT,'receipt failure'); END;").unwrap();
    let input = replacement(old.request_id, 1, "replace");
    assert!(
        matches!(f.call(input.clone()), Err(InboxError::Mutation { source, .. }) if matches!(*source, InboxError::Store(provenant_fabric_store::Error::Sqlite(_))))
    );
    assert_eq!(item(&f, old.request_id).history.len(), 1);
    assert_eq!(item(&f, old.request_id).request.state, RequestState::Open);
    let count: i64 = db
        .query_row("SELECT count(*) FROM human_requests", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    db.execute_batch("DROP TRIGGER fail_receipt").unwrap();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let service = f.service();
            barrier.wait();
            service.execute(input, &f.cancel)
        });
        let second = scope.spawn(|| {
            let service = f.service();
            barrier.wait();
            service.execute(replacement(old.request_id, 1, "second"), &f.cancel)
        });
        vec![first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .find_map(|r| r.as_ref().err())
            .unwrap()
            .code(),
        "conflict"
    );
    assert_eq!(
        item(&f, old.request_id).request.state,
        RequestState::Superseded
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM human_requests", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn request_correlation_deadline_expires_on_read_without_rewriting_answers_or_history() {
    use provenant_core::DurationMicros;
    let mut f = Fixture::new();
    let deadline = f
        .clock
        .wall_now()
        .unwrap()
        .checked_add(DurationMicros::from_micros(10))
        .unwrap();
    let input = extended_ask("deadline", serde_json::json!({"deadline":deadline}));
    let open = receipt(f.call(input.clone()).unwrap());
    assert_eq!(item(&f, open.request_id).request.schema_version, 2);
    let resolved = receipt(
        f.call(extended_ask(
            "answered-deadline",
            serde_json::json!({"deadline":deadline}),
        ))
        .unwrap(),
    );
    f.call(answer(resolved.request_id, "answer")).unwrap();
    let before = item(&f, resolved.request_id);
    assert_eq!(item(&f, open.request_id).request.state, RequestState::Open);
    f.clock.advance(DurationMicros::from_micros(10)).unwrap();
    let expired = item(&f, open.request_id);
    assert_eq!(expired.request.state, RequestState::Expired);
    assert_eq!(
        expired.request.closure_reason.as_deref(),
        Some("deadline elapsed")
    );
    assert_eq!(expired.history[0].closure_reason, None);
    assert_eq!(expired.request.revision, 1);
    assert_eq!(expired.history.len(), 1);
    assert_eq!(expired.history[0].state, RequestState::Open);
    assert_eq!(receipt(f.call(input).unwrap()), open);
    assert_eq!(
        f.call(answer(open.request_id, "late")).unwrap_err().code(),
        "conflict"
    );
    assert_eq!(item(&f, resolved.request_id), before);
    for all in [false, true] {
        let result = f
            .call(InboxInput::List {
                extra: Default::default(),
                id: None,
                session: None,
                state: if all {
                    InboxFilter::All
                } else {
                    InboxFilter::Open
                },
                limit: 1,
                cursor: None,
            })
            .unwrap();
        let InboxResult::Listed {
            items, truncated, ..
        } = result
        else {
            panic!("list")
        };
        if all {
            assert_eq!(items.len(), 1);
            assert!(truncated);
        } else {
            assert!(items.is_empty());
            assert!(!truncated);
        }
    }
    let replaceable = receipt(f.call(ask(RequestKind::Decision, "replaceable")).unwrap());
    let original = item(&f, replaceable.request_id);
    for fields in [
        serde_json::json!({"deadline":deadline}),
        serde_json::json!({"deadline":deadline,"supersedes":{"id":replaceable.request_id,"expect_revision":1}}),
    ] {
        assert_eq!(
            f.call(extended_ask("elapsed", fields)).unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(item(&f, replaceable.request_id), original);
    }
}

#[test]
#[allow(clippy::disallowed_methods)] // The bounded hook offer API uses real deadlines.
fn hook_offers_retry_idempotently_without_hiding_later_queued_answers() {
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    for n in 0..18 {
        let created = receipt(
            f.call(ask(RequestKind::Decision, &format!("once-ask-{n}")))
                .unwrap(),
        );
        f.call(answer(created.request_id, &format!("once-answer-{n}")))
            .unwrap();
    }
    let target = AnswerTarget {
        extra: Default::default(),
        host: f.identity.host,
        project: f.identity.project,
        actor: f.identity.inbox.actor,
        session: "session-a".into(),
    };
    let deadline = || Instant::now() + Duration::from_secs(2);
    let read = || {
        let store = Store::open_read_only(&f.root, deadline()).unwrap().unwrap();
        store
            .read_queued_human_answers(&target, deadline())
            .unwrap()
    };
    let first = read();
    assert_eq!(first.items.len(), 16);
    let claim = |page, until| {
        Store::offer_human_answers_once(&f.root, &target, page, f.clock.wall_now().unwrap(), until)
    };
    let barrier = std::sync::Barrier::new(2);
    let claimed = std::thread::scope(|scope| {
        let worker = || {
            barrier.wait();
            claim(first.clone(), deadline())
        };
        let a = scope.spawn(worker);
        let b = scope.spawn(worker);
        [a.join().unwrap(), b.join().unwrap()]
            .into_iter()
            .filter_map(Result::ok)
            .map(|page| page.items.len())
            .sum::<usize>()
    });
    assert_eq!(claimed, 16);
    assert!(claim(first, deadline()).unwrap().items.is_empty());
    let remaining = read();
    assert_eq!(remaining.items.len(), 2);
    assert!(matches!(
        claim(remaining.clone(), Instant::now()),
        Err(provenant_fabric_store::Error::Busy)
    ));
    assert_eq!(read(), remaining);
    assert_eq!(claim(remaining, deadline()).unwrap().items.len(), 2);
    // A committed offer that missed the host's deadline or stdout is still
    // eligible for the exact same session, without recording another attempt.
    let retries = Store::open_read_only(&f.root, deadline())
        .unwrap()
        .unwrap()
        .read_offered_human_answers(&target, deadline())
        .unwrap();
    assert_eq!(retries.items.len(), 16);
    let retried = claim(retries.clone(), deadline()).unwrap();
    assert_eq!(retried, retries);
    assert!(retried.items.iter().all(|item| item.delivery.attempts == 1));
}

fn chair_message(session: &str, text: &str, id: &str) -> InboxInput {
    InboxInput::ChairMessage {
        extra: Default::default(),
        session: session.into(),
        text: text.into(),
        channel: InteractionChannel::Console,
        operation_id: op(id),
    }
}
fn steer(text: &str, id: &str) -> InboxInput {
    InboxInput::Steer {
        extra: Default::default(),
        attempt: provenant_core::AttemptId::from_bytes([7; 16]),
        text: text.into(),
        operation_id: op(id),
    }
}
fn control_route(f: &Fixture, provider: Option<Harness>, launch: LaunchKind) -> ControlRoute {
    ControlRoute {
        provider,
        launch,
        recipient: (launch == LaunchKind::Chair).then_some(f.identity.inbox.actor),
        target: "target".into(),
    }
}
fn control(
    f: &Fixture,
    route: ControlRoute,
    input: InboxInput,
) -> Result<ControlReceipt, InboxError> {
    let lookup = move |_: &InboxInput, _: &Request<'_>| Ok(route.clone());
    match f
        .service()
        .with_control_lookup(&lookup)
        .execute(input, &f.cancel)?
    {
        InboxResult::Control { receipt } => Ok(receipt),
        other => panic!("{other:?}"),
    }
}
fn mailbox(f: &Fixture) -> Vec<provenant_fabric_store::Delivery> {
    let request = Request {
        clock: &f.clock,
        cancel: &f.cancel,
    };
    Store::open(&f.root, &request)
        .unwrap()
        .read_mail(
            f.identity.project,
            f.identity.inbox.actor,
            Some(0),
            50,
            &request,
        )
        .unwrap()
        .items
}

#[test]
fn chair_message_queues_mail_replays_and_refuses_a_conflicting_reuse() {
    let f = Fixture::new();
    let route = || control_route(&f, None, LaunchKind::Chair);
    let first = control(
        &f,
        route(),
        chair_message("session-a", "look at CI", "msg-1"),
    )
    .unwrap();
    assert_eq!(first.state, ControlState::Queued);
    assert_eq!(first.transport, Some(AnswerTransport::FabricMail));
    assert_eq!(first.control, ControlKind::ChairMessage);
    let replay = control(
        &f,
        route(),
        chair_message("session-a", "look at CI", "msg-1"),
    )
    .unwrap();
    assert_eq!(replay, first);
    let conflict = control(
        &f,
        route(),
        chair_message("session-a", "different", "msg-1"),
    );
    assert!(matches!(
        conflict,
        Err(InboxError::Mutation { ref source, .. }) if source.code() == "operation_conflict"
    ));
    let mail = mailbox(&f);
    assert_eq!(mail.len(), 1);
    assert!(mail[0].message.body.contains("look at CI"));
    assert!(mail[0].message.body.contains("not an approval"));
}

#[test]
fn unsupported_steer_refuses_before_any_side_effect() {
    let f = Fixture::new();
    for provider in provenant_records::ADAPTER_SPECS.iter().map(|spec| spec.id) {
        let route = control_route(&f, Some(provider), LaunchKind::Lane);
        let receipt = control(&f, route, steer("slow down", "steer-1")).unwrap();
        assert_eq!(receipt.state, ControlState::Unsupported, "{provider:?}");
        assert_eq!(receipt.transport, None);
        assert_eq!(receipt.message_id, None);
    }
    // Nothing was recorded, so the ID is still free for a different request.
    let route = control_route(&f, None, LaunchKind::Lane);
    let receipt = control(&f, route, steer("other text", "steer-1")).unwrap();
    assert_eq!(receipt.state, ControlState::Unsupported);
    assert!(mailbox(&f).is_empty());
}

#[test]
fn control_text_is_bounded_and_nonblank() {
    let f = Fixture::new();
    let route = || control_route(&f, None, LaunchKind::Chair);
    for text in [
        "".to_owned(),
        "  \n".to_owned(),
        "x".repeat(CONTROL_TEXT_BYTES + 1),
    ] {
        assert!(matches!(
            control(&f, route(), chair_message("session-a", &text, "msg-bad")),
            Err(InboxError::Invalid(_))
        ));
        assert!(matches!(
            control(&f, route(), steer(&text, "steer-bad")),
            Err(InboxError::Invalid(_))
        ));
    }
    let longest = "x".repeat(CONTROL_TEXT_BYTES);
    assert!(control(&f, route(), chair_message("session-a", &longest, "msg-max")).is_ok());
}

#[test]
fn control_is_refused_inside_a_lane_and_without_a_lookup() {
    let f = Fixture::new();
    let input = chair_message("session-a", "hello", "msg-lane");
    assert!(matches!(
        validate_request(&f.identity, &config(), true, &input),
        Err(InboxError::LaneMutation)
    ));
    assert!(matches!(
        f.call(input),
        Err(InboxError::Mutation { ref source, .. }) if source.code() == "inbox_unavailable"
    ));
}
