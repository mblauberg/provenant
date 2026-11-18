use super::*;
use provenant_core::{MonoTime, TestClock};
use std::sync::{Arc, Barrier};

fn project() -> ProjectId {
    ProjectId::from_bytes([1; 16])
}
fn host(node: &str) -> Host {
    Host {
        project: project(),
        lineage: ObservationLineage {
            attempt: Some(AttemptId::from_bytes([2; 16])),
            parent_attempt: None,
            work: Some(WorkKey::new(project(), "g", node, 1).unwrap()),
            extra: Default::default(),
        },
        adapter: Some("host-adapter".into()),
        model: Some("host-model".into()),
        commit: Some("host-commit".into()),
        commit_source: "host_retained_attempt",
    }
}
fn clock() -> TestClock {
    TestClock::new(
        WallTime::from_micros(100).unwrap(),
        MonoTime::from_micros(0),
    )
}
fn board(path: &Path) -> Board {
    Board::open(path, project(), default_read_cap(), default_byte_cap()).unwrap()
}
fn add(scope: Scope, text: &str) -> Input {
    Input::Add {
        text: text.into(),
        tags: vec!["any/free-form tag".into()],
        scope,
        resolves: None,
    }
}
fn read(scope: Scope, reader: &str, after: Option<i64>, limit: Option<u64>) -> Input {
    Input::Read {
        scope,
        reader: reader.into(),
        after,
        limit,
    }
}
fn execute(board: &Board, input: Input) -> serde_json::Value {
    board
        .execute(&host("root"), input, &clock(), &CancelToken::new())
        .unwrap()
}
fn page(board: &Board, input: Input) -> Page {
    serde_json::from_value(execute(board, input)).unwrap()
}

// Spec: docs/specs/lane-contracts.md#shared-notes
#[test]
fn concurrent_append_preserves_every_note_and_corrections() {
    let dir = tempfile::tempdir().unwrap();
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
        for n in 0..8 {
            let barrier = Arc::clone(&barrier);
            let path = dir.path();
            scope.spawn(move || {
                let board = board(path);
                barrier.wait();
                execute(&board, add(Scope::Project, &format!("interface {n}")));
            });
        }
    });
    let board = board(dir.path());
    let before = page(&board, read(Scope::Project, "r", Some(0), None));
    assert_eq!(before.events.len(), 8);
    let ids: std::collections::BTreeSet<_> = before.events.iter().map(|e| &e.id).collect();
    assert_eq!(ids.len(), 8);
    let bytes: Vec<_> = (1..=8)
        .map(|seq| {
            board
                .directory
                .read_stable(OsStr::new(&filename(seq)), 65536)
                .unwrap()
        })
        .collect();
    execute(
        &board,
        Input::Add {
            text: "correction".into(),
            tags: vec![],
            scope: Scope::Project,
            resolves: Some(1),
        },
    );
    for (i, old) in bytes.iter().enumerate() {
        assert_eq!(
            &board
                .directory
                .read_stable(OsStr::new(&filename(i as i64 + 1)), 65536)
                .unwrap(),
            old
        );
    }
    let after = page(&board, read(Scope::Project, "r", Some(8), None));
    assert_eq!(after.events[0].detail["resolves"], 1);
    let observation = after.events[0]
        .observation(ObservationCursor {
            project: project(),
            seq: 123,
        })
        .unwrap();
    assert_eq!(observation.id, after.events[0].id);
    assert_eq!(observation.cursor.seq, 123);
    assert_eq!(observation.kind, ObservationKind::Note);
}

// Spec: docs/specs/lane-contracts.md#shared-notes
#[test]
fn subtree_isolation_includes_broadcasts_without_sibling_leaks() {
    let dir = tempfile::tempdir().unwrap();
    let board = board(dir.path());
    let subtree = |g: &str, n: &str| Scope::Subtree {
        graph: g.into(),
        node: n.into(),
    };
    for scope in [
        Scope::Project,
        Scope::Graph { graph: "g".into() },
        subtree("g", "root"),
        subtree("g", "root--child"),
        subtree("g", "root--sibling"),
        subtree("g", "rootish"),
        subtree("other", "root--child"),
    ] {
        execute(&board, add(scope, "fact"));
    }
    let result = page(
        &board,
        read(subtree("g", "root--child"), "r", Some(0), None),
    );
    let seqs: Vec<_> = result.events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
    let other = Board::open(
        dir.path(),
        ProjectId::from_bytes([9; 16]),
        default_read_cap(),
        default_byte_cap(),
    )
    .unwrap();
    let other_host = Host::chair(ProjectId::from_bytes([9; 16]), None);
    let isolated = other
        .execute(
            &other_host,
            read(Scope::Project, "r", None, None),
            &clock(),
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!(isolated["events"].as_array().unwrap().len(), 0);
}

// Spec: docs/specs/lane-contracts.md#shared-notes
#[test]
fn delta_ack_replay_cap_and_cursor_recovery_never_lose_source() {
    let dir = tempfile::tempdir().unwrap();
    let board = board(dir.path());
    for _ in 0..3 {
        execute(&board, add(Scope::Project, "fact"));
    }
    let first = page(&board, read(Scope::Project, "r", None, Some(1)));
    assert!(first.truncated);
    assert_eq!(first.read_cap.source, LimitSource::Launch);
    assert_eq!(first.next, 1);
    // No ACK means the same page is redelivered after reconnect.
    let retry = page(
        &super::tests::board(dir.path()),
        read(Scope::Project, "r", None, Some(1)),
    );
    assert_eq!(retry.events[0].id, first.events[0].id);
    execute(
        &board,
        Input::Ack {
            reader: "r".into(),
            scope: Scope::Project,
            seq: first.next,
        },
    );
    let next = page(&board, read(Scope::Project, "r", None, None));
    assert_eq!(
        next.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(
        page(&board, read(Scope::Project, "independent", None, None))
            .events
            .len(),
        3
    );
    assert_eq!(
        page(&board, read(Scope::Project, "r", Some(0), None))
            .events
            .len(),
        3
    );
    execute(
        &board,
        Input::Ack {
            reader: "r".into(),
            scope: Scope::Project,
            seq: next.next,
        },
    );
    assert!(
        page(&board, read(Scope::Project, "r", None, None))
            .events
            .is_empty()
    );
    let cursor = board
        .cursor_name(host("root").lineage.attempt, "r", &Scope::Project)
        .unwrap();
    for bad in [b"broken".as_slice(), b"999", b"-1"] {
        board
            .directory
            .atomic_write(OsStr::new(&cursor), ".test-", bad, PublishMode::Replace)
            .unwrap();
        let recovered = page(&board, read(Scope::Project, "r", None, None));
        assert!(recovered.cursor_recovered);
        assert_eq!(recovered.events.len(), 3);
    }
    assert_eq!(board.sequences().unwrap(), vec![1, 2, 3]);
}

// Spec: docs/specs/lane-contracts.md#shared-notes
#[test]
fn relay_rejects_forged_provenance_and_stamps_host_identity() {
    use crate::relay::{HostRelay, ToolCall};
    let dir = tempfile::tempdir().unwrap();
    for field in [
        "provenance",
        "lane",
        "adapter",
        "model",
        "commit",
        "time",
        "verified",
        "lineage",
        "sender",
    ] {
        let mut value = serde_json::to_value(add(Scope::Project, "claim")).unwrap();
        value[field] = serde_json::json!("forged");
        assert!(ToolCall::parse("notes", value).is_err(), "{field}");
    }
    let state = dir.path().to_path_buf();
    let socket = dir.path().join("relay/notes.sock");
    let relay = HostRelay::start(
        socket.clone(),
        provenant_records::Limits::default(),
        move |call, cancel, _| {
            let ToolCall::Notes(input) = call else {
                panic!("wrong tool")
            };
            let clock = clock();
            board(&state)
                .execute(&host("root"), input, &clock, cancel)
                .map_err(|e| crate::relay::ServiceError::Cli {
                    code: "notes".into(),
                    message: e.to_string(),
                    data: serde_json::Value::Null,
                })
        },
    )
    .unwrap();
    let result = ToolCall::Notes(add(Scope::Project, "verified=true is only text"))
        .relay(&socket, &CancelToken::new())
        .unwrap();
    assert_eq!(
        result["detail"]["provenance"]["lane"],
        serde_json::json!(host("root").lineage.attempt)
    );
    assert_eq!(result["detail"]["provenance"]["adapter"], "host-adapter");
    assert_eq!(result["detail"]["provenance"]["model"], "host-model");
    assert_eq!(result["detail"]["provenance"]["commit"], "host-commit");
    assert_eq!(result["detail"]["provenance"]["verified"], false);
    drop(relay);
}

// Spec: docs/specs/lane-contracts.md#shared-notes
#[test]
fn two_live_lane_relays_exchange_graph_notes_mid_run() {
    use crate::relay::{HostRelay, ToolCall};
    let dir = tempfile::tempdir().unwrap();
    let graph = Scope::Graph { graph: "g".into() };
    let mut relays = Vec::new();
    let mut sockets = Vec::new();
    for n in 0..2 {
        let state = dir.path().to_path_buf();
        let socket = dir.path().join(format!("lane-{n}/relay.sock"));
        let mut identity = host(&format!("lane-{n}"));
        identity.lineage.attempt = Some(AttemptId::from_bytes([n + 2; 16]));
        relays.push(
            HostRelay::start(
                socket.clone(),
                provenant_records::Limits::default(),
                move |call, cancel, _| {
                    let ToolCall::Notes(input) = call else {
                        panic!("wrong tool")
                    };
                    board(&state)
                        .execute(&identity, input, &clock(), cancel)
                        .map_err(|e| crate::relay::ServiceError::Cli {
                            code: "notes".into(),
                            message: e.to_string(),
                            data: serde_json::Value::Null,
                        })
                },
            )
            .unwrap(),
        );
        sockets.push(socket);
    }
    std::thread::scope(|threads| {
        for socket in &sockets {
            let graph = graph.clone();
            threads.spawn(move || {
                ToolCall::Notes(add(graph, "interface available"))
                    .relay(socket, &CancelToken::new())
                    .unwrap()
            });
        }
    });
    for (n, socket) in sockets.iter().enumerate() {
        let result = ToolCall::Notes(read(graph.clone(), &format!("lane-{n}"), None, None))
            .relay(socket, &CancelToken::new())
            .unwrap();
        let page: Page = serde_json::from_value(result).unwrap();
        assert_eq!(page.events.len(), 2);
        assert!(
            page.events
                .iter()
                .any(|note| note.lineage.attempt == Some(AttemptId::from_bytes([3 - n as u8; 16])))
        );
        assert!(
            page.events
                .iter()
                .all(|note| note.detail["provenance"]["verified"] == false)
        );
    }
    drop(relays);
}

// Spec: docs/specs/lane-contracts.md#shared-notes
#[test]
fn reader_alias_cannot_advance_another_lanes_or_chairs_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let board = board(dir.path());
    execute(&board, add(Scope::Project, "unread"));
    let first = host("first");
    let mut second = host("second");
    second.lineage.attempt = Some(AttemptId::from_bytes([8; 16]));
    let ack = Input::Ack {
        reader: "shared-alias".into(),
        scope: Scope::Project,
        seq: 1,
    };
    board
        .execute(&first, ack, &clock(), &CancelToken::new())
        .unwrap();
    for identity in [&second, &Host::chair(project(), None)] {
        let value = board
            .execute(
                identity,
                read(Scope::Project, "shared-alias", None, None),
                &clock(),
                &CancelToken::new(),
            )
            .unwrap();
        let page: Page = serde_json::from_value(value).unwrap();
        assert_eq!(page.events.len(), 1);
    }
    let value = board
        .execute(
            &first,
            read(Scope::Project, "shared-alias", None, None),
            &clock(),
            &CancelToken::new(),
        )
        .unwrap();
    let page: Page = serde_json::from_value(value).unwrap();
    assert!(page.events.is_empty());
}

// Spec: docs/specs/lane-contracts.md#shared-notes
#[test]
fn every_accepted_note_fits_replay_and_oversize_rejection_has_no_effect() {
    let dir = tempfile::tempdir().unwrap();
    let limit = provenant_records::COMMAND_RESULT_MAX_BYTES as u64;
    let board = Board::open(
        dir.path(),
        project(),
        default_read_cap(),
        Sourced {
            value: limit * 2,
            source: LimitSource::Host,
        },
    )
    .unwrap();
    let text = "x".repeat(limit as usize / 2);
    execute(&board, add(Scope::Project, &text));
    execute(&board, add(Scope::Project, &text));
    let oversized = board.execute(
        &host("root"),
        add(Scope::Project, &"x".repeat(limit as usize)),
        &clock(),
        &CancelToken::new(),
    );
    assert!(matches!(oversized, Err(Error::ResponseCapacity { .. })));
    assert_eq!(board.sequences().unwrap(), vec![1, 2]);
    let first = page(&board, read(Scope::Project, "r", Some(0), None));
    assert!(first.truncated);
    assert_eq!(first.events.len(), 1);
    assert!(first.response_bytes().unwrap() as u64 <= limit);
    let second = page(&board, read(Scope::Project, "r", Some(first.next), None));
    assert_eq!(second.events.len(), 1);
    assert_eq!(second.events[0].seq, 2);
    assert!(second.response_bytes().unwrap() as u64 <= limit);
}
