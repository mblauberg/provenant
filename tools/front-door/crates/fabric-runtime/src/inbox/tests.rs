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
        }
    }
    fn budget(&self) -> WaitBudget {
        WaitBudget::cli(self.clock.mono_now(), 5).unwrap()
    }
    fn service(&self) -> InboxService<'_> {
        InboxService::open(
            &self.root,
            &self.identity,
            &self.clock,
            self.budget(),
            &self.cancel,
        )
        .unwrap()
    }
    fn call(&self, input: InboxInput) -> Result<InboxResult, InboxError> {
        self.service().execute(input, self.budget(), &self.cancel)
    }
}
fn op(text: &str) -> Option<OperationId> {
    Some(OperationId::parse(text).unwrap())
}
fn ask(kind: RequestKind, id: &str) -> InboxInput {
    InboxInput::Ask {
        session: "session-a".into(),
        request: RequestDraft {
            kind,
            title: "Select repair".into(),
            body: "Which repair should proceed?".into(),
            blocking: kind == RequestKind::Decision,
            options: vec![RequestOption {
                id: "repair".into(),
                label: "Repair".into(),
                description: "Repair the failing check".into(),
            }],
            recommended_option_id: Some("repair".into()),
            proceeding_with: (kind == RequestKind::Assumption).then(|| "Repair the check".into()),
        },
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
    let InboxResult::Item { item } = f.call(show(created.request_id)).unwrap() else {
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
        let InboxResult::Item { item } = f.call(show(created.request_id)).unwrap() else {
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
        budget: f.budget(),
        cancel: &f.cancel,
        backoff: &ThreadWait,
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
    let InboxResult::Item { item } = f.call(show(created.request_id)).unwrap() else {
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
            service.execute(answer(created.request_id, "first"), f.budget(), &f.cancel)
        });
        let second = scope.spawn(|| {
            let service = f.service();
            barrier.wait();
            service.execute(answer(created.request_id, "second"), f.budget(), &f.cancel)
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
        budget: f.budget(),
        cancel: &f.cancel,
        backoff: &ThreadWait,
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
    let InboxResult::Item { item } = f.call(show(created.request_id)).unwrap() else {
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
        id: created.request_id,
        state: RequestState::Superseded,
        reason: "Replaced by a new scoped request".into(),
        expect_revision: 2,
        operation_id: op("close"),
    };
    let closed = receipt(f.call(input.clone()).unwrap());
    assert_eq!(closed.state, RequestState::Superseded);
    assert_eq!(receipt(f.call(input).unwrap()), closed);
    let InboxResult::Item { item } = f.call(show(created.request_id)).unwrap() else {
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
        id: None,
        session: None,
        state: InboxFilter::All,
        limit: 101,
        cursor: None,
    };
    assert_eq!(f.call(input).unwrap_err().code(), "invalid_input");
}

#[test]
fn inbox_remote_owner_and_worker_mutations_are_refused_without_opening_store() {
    let mut f = Fixture::new();
    let mut config:crate::config::FabricConfig=serde_json::from_value(serde_json::json!({"agent_label":"chair","client_label":"test","home_hosts":{},"peers":{},"limits":{"body_bytes":65536,"input_frame_bytes":1048576,"json_depth":64,"page_rows":100,"output_lines":500,"output_bytes":65536,"result_bytes":1048576,"lease_seconds":3600,"peer_seconds":30,"graph_timeout_seconds":14400,"bundle_bytes":67108864}})).unwrap();
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
        false,
        &f.clock,
        ask(RequestKind::Decision, "create"),
        f.budget(),
        &f.cancel,
    );
    assert_eq!(result.unwrap_err().code(), "unsupported_owner");
    assert!(!f.root.as_path().join("fabric").exists());
    let result = execute(
        &f.root,
        &f.identity,
        &config,
        true,
        &f.clock,
        ask(RequestKind::Decision, "create"),
        f.budget(),
        &f.cancel,
    );
    assert_eq!(result.unwrap_err().code(), "lane_coordination_refused");
    assert!(!f.root.as_path().join("fabric").exists());
}
