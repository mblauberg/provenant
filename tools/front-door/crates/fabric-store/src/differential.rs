use crate::pending::IdSource;
use crate::tests::TimeCell;
use crate::*;
use serde_json::{json, Value};
use std::{cell::Cell, sync::Arc};
#[derive(Clone)]
struct FixtureClock(Arc<TimeCell>);
impl Clock for FixtureClock {
    fn now_ms(&self) -> i64 {
        self.0.get()
    }
}
#[derive(Default)]
struct FixtureIds(Cell<i64>);
impl pending::IdSource for FixtureIds {
    fn uuid(&self) -> Result<String> {
        self.0.set(self.0.get() + 1);
        Ok(format!("fixture-{}", self.0.get()))
    }
}
fn a() -> Identity {
    Identity {
        project: "p".into(),
        agent_id: "alice".into(),
        provider: "codex".into(),
    }
}
fn b() -> Identity {
    Identity {
        agent_id: "bob".into(),
        ..a()
    }
}
fn capture<T: Serialize>(out: &mut Vec<Value>, result: Result<T>) {
    #[derive(Serialize)]
    struct OkWire<T> {
        ok: T,
    }
    #[derive(Serialize)]
    struct ErrorWire {
        error: String,
    }
    out.push(match result {
        Ok(value) => {
            let wire = serde_json::to_string(&OkWire { ok: &value }).unwrap();
            json!({"ok":value, "wire":wire})
        }
        Err(error) => {
            let error = error.to_string();
            let wire = serde_json::to_string(&ErrorWire {
                error: error.clone(),
            })
            .unwrap();
            json!({"error":error, "wire":wire})
        }
    });
}
fn snapshot(store: &Store, out: Vec<Value>) -> Value {
    fn query(db: &Connection, sql: &str) -> Value {
        let mut stmt = db.prepare(sql).unwrap();
        let names: Vec<_> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = stmt.query([]).unwrap();
        let mut result = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            let mut record = serde_json::Map::new();
            for (index, name) in names.iter().enumerate() {
                use rusqlite::types::ValueRef;
                let value = match row.get_ref(index).unwrap() {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(i) => json!(i),
                    ValueRef::Real(f) => json!(f),
                    ValueRef::Text(t) => json!(std::str::from_utf8(t).unwrap()),
                    other => panic!("unexpected fixture value {other:?}"),
                };
                record.insert(name.clone(), value);
            }
            result.push(Value::Object(record));
        }
        json!(result)
    }
    let schema = query(
        &store.db,
        "SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name",
    );
    let tables = query(
        &store.db,
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
    );
    let mut rows = serde_json::Map::new();
    for table in tables.as_array().unwrap() {
        let name = table["name"].as_str().unwrap();
        rows.insert(
            name.into(),
            query(
                &store.db,
                &format!("SELECT * FROM \"{name}\" ORDER BY rowid"),
            ),
        );
    }
    let timeout: i64 = store
        .db
        .pragma_query_value(None, "busy_timeout", |r| r.get(0))
        .unwrap();
    let wal: String = store
        .db
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    let fk: i64 = store
        .db
        .pragma_query_value(None, "foreign_keys", |r| r.get(0))
        .unwrap();
    json!({"out":out,"schema":schema,"rows":rows,"pragmas":{"busy_timeout":timeout,"journal_mode":wal,"foreign_keys":fk}})
}
fn compare(scenario: &str, result: Value) {
    let Some(expected) = crate::oracle::node(scenario) else {
        return;
    };
    if result != expected {
        let path = std::env::temp_dir().join(format!("fabric-store-{scenario}-actual.json"));
        std::fs::write(&path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
        let expected_path =
            std::env::temp_dir().join(format!("fabric-store-{scenario}-expected.json"));
        std::fs::write(
            &expected_path,
            serde_json::to_vec_pretty(&expected).unwrap(),
        )
        .unwrap();
        panic!(
            "{scenario}: differential mismatch; actual {}, expected {}",
            path.display(),
            expected_path.display()
        );
    }
}
#[test]
fn landing_database_and_failure_differential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let clock = FixtureClock(Arc::new(TimeCell::new(1000)));
    let store = Store::open(&path, 5000, clock.clone()).unwrap();
    let other = Store::open(&path, 5000, clock.clone()).unwrap();
    let a = a();
    let b = b();
    let sha = "a".repeat(40);
    let mut out = Vec::new();
    capture(&mut out, store.acquire_landing(&a, "a", &sha, 1, 1000));
    capture(&mut out, other.acquire_landing(&b, "b", &sha, 60, 1000));
    capture(
        &mut out,
        store.verify_landing(&a, "a", 1, &"b".repeat(40), 1000),
    );
    capture(&mut out, store.renew_landing(&a, "a", 1, 1, 1000));
    capture(&mut out, other.acquire_landing(&b, "b", &sha, 60, 2000));
    capture(&mut out, store.verify_landing(&a, "a", 1, &sha, 2000));
    capture(&mut out, store.try_release_landing(&a, "a", 1, Some(2000)));
    clock.0.set(2000);
    // Inner outcomes are appended before the outer push outcome, matching the source.
    let failed = other.with_landing_push(&b, "b", 2, &sha, || -> Result<()> {
        store.note(&a, "write during push")?;
        capture(&mut out, store.try_release_landing(&b, "b", 2, Some(2000)));
        capture(
            &mut out,
            store.with_landing_push(&b, "b", 2, &sha, || Ok("forbidden")),
        );
        Err(invalid("push failed"))
    });
    capture(&mut out, failed);
    capture(&mut out, other.landing_lease("p", 2000));
    capture(
        &mut out,
        other.with_landing_push(&b, "b", 2, &sha, || Ok("pushed")),
    );
    capture(&mut out, other.landing_lease("p", 2000));
    capture(&mut out, store.acquire_landing(&a, "c", &sha, 1, 2000));
    capture(&mut out, store.try_release_landing(&a, "c", 3, Some(3000)));
    capture(
        &mut out,
        store.acquire_landing(&a, "bad/name", &sha, 1, 2000),
    );
    capture(
        &mut out,
        store.acquire_landing(&a, "bad", &sha.to_uppercase(), 1, 2000),
    );
    capture(&mut out, store.acquire_landing(&a, "bad", &sha, 0, 2000));
    capture(
        &mut out,
        store.acquire_landing(&a, "\u{feff}", &sha, 1, 2000),
    );
    store.db.execute_batch("CREATE TRIGGER fail_log BEFORE INSERT ON activity BEGIN SELECT RAISE(ABORT, 'log blocked'); END").unwrap();
    capture(&mut out, store.renew_landing(&a, "c", 3, 60, 2000));
    store.db.execute_batch("DROP TRIGGER fail_log").unwrap();
    let raw = Connection::open(&path).unwrap();
    capture(&mut out,store.with_landing_push(&a,"c",3,&sha,|| { raw.execute_batch("CREATE TRIGGER fail_release BEFORE UPDATE OF released_at ON landing_leases BEGIN SELECT RAISE(ABORT, 'release blocked'); END")?;Ok("pushed") }));
    raw.execute_batch("DROP TRIGGER fail_release").unwrap();
    compare("leases", snapshot(&store, out));
}
#[test]
fn session_database_differential() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        &dir.path().join("db"),
        5000,
        FixtureClock(Arc::new(TimeCell::new(1000))),
    )
    .unwrap();
    let a = a();
    let b = b();
    let mut out = Vec::new();
    capture(
        &mut out,
        store.claim_session_turn_with_id(
            &a,
            "Review",
            None,
            SessionTurnKind::Start,
            123,
            "fixture-1",
        ),
    );
    capture(
        &mut out,
        store.claim_session_turn_with_id(
            &b,
            "Review",
            None,
            SessionTurnKind::Start,
            124,
            "fixture-2",
        ),
    );
    let turn = SessionLaunch {
        run_id: "r".into(),
        task_id: "t".into(),
        attempt: 1,
    };
    capture(
        &mut out,
        store.bind_session_turn(&a, "Review", "wrong", &turn),
    );
    capture(
        &mut out,
        store.bind_session_turn(&a, "Review", "fixture-1", &turn),
    );
    capture(
        &mut out,
        store.release_session_launcher(&a, "Review", "fixture-1"),
    );
    capture(
        &mut out,
        store.settle_session_turn(&a, "Review", "wrong", "failed", None),
    );
    capture(&mut out, store.forget_session(&a, "Review"));
    let pointer = SessionPointer {
        adapter: Some("codex".into()),
        provider_session_id: Some("native".into()),
        run_id: "r".into(),
        task_id: "t".into(),
        attempt: 1,
        result_path: Some("/result".into()),
    };
    capture(
        &mut out,
        store.settle_session_turn(&a, "Review", "fixture-1", "ok", Some(&pointer)),
    );
    capture(
        &mut out,
        store.claim_session_turn_with_id(
            &b,
            "Review",
            Some("old-claim"),
            SessionTurnKind::Resume,
            125,
            "fixture-3",
        ),
    );
    capture(
        &mut out,
        store.claim_session_turn_with_id(
            &b,
            "Review",
            Some("fixture-1"),
            SessionTurnKind::Resume,
            125,
            "fixture-4",
        ),
    );
    capture(
        &mut out,
        store.settle_session_turn(&b, "Review", "fixture-4", "failed", None),
    );
    capture(&mut out, store.session("p", "Review"));
    capture(&mut out, store.session("p", "review"));
    capture(&mut out, store.sessions("p"));
    capture(&mut out, store.forget_session(&a, "Review"));
    capture(
        &mut out,
        store.claim_session_turn_with_id(
            &a,
            "NeverLaunched",
            None,
            SessionTurnKind::Fresh,
            126,
            "fixture-5",
        ),
    );
    capture(
        &mut out,
        store.settle_session_turn(&a, "NeverLaunched", "fixture-5", "failed", None),
    );
    capture(&mut out, store.sessions("p"));
    capture(
        &mut out,
        store.claim_session_turn_with_id(
            &a,
            "Missing",
            Some("stale"),
            SessionTurnKind::Start,
            127,
            "fixture-6",
        ),
    );
    compare("sessions", snapshot(&store, out));
}
#[test]
fn delivery_task_and_observation_database_differential() {
    let dir = tempfile::tempdir().unwrap();
    let clock = FixtureClock(Arc::new(TimeCell::new(1000)));
    let store = Store::open(&dir.path().join("db"), 5000, clock.clone()).unwrap();
    let a = a();
    let b = b();
    let ids = FixtureIds::default();
    let mut out = Vec::new();
    store.announce(&a).unwrap();
    store.announce(&b).unwrap();
    capture(&mut out, store.create_team(&a, "all", &[]));
    capture(&mut out, store.create_team(&a, "bob", &[]));
    capture(
        &mut out,
        store.create_team(&a, "team", &["bob".into(), "alice".into(), "bob".into()]),
    );
    capture(
        &mut out,
        store.create_task_with_id(
            &a,
            "work",
            "task",
            None,
            &["z".into(), "a".into(), "z".into()],
        ),
    );
    capture(
        &mut out,
        store.send_with_id(
            &a,
            "missing",
            "refused",
            SendOptions::default(),
            RecipientContext::default(),
            &ids.uuid().unwrap(),
        ),
    );
    capture(
        &mut out,
        store.send_with_id(
            &a,
            "bob",
            "missing parent",
            SendOptions {
                reply_to: Some("missing"),
                ..Default::default()
            },
            RecipientContext::default(),
            &ids.uuid().unwrap(),
        ),
    );
    capture(
        &mut out,
        store.send_with_id(
            &a,
            "bob",
            "missing task",
            SendOptions {
                task_id: Some("missing"),
                ..Default::default()
            },
            RecipientContext::default(),
            &ids.uuid().unwrap(),
        ),
    );
    capture(
        &mut out,
        store.send_with_id(
            &a,
            "team",
            "hello\nworld",
            SendOptions {
                task_id: Some("task"),
                ..Default::default()
            },
            RecipientContext::default(),
            &ids.uuid().unwrap(),
        ),
    );
    capture(
        &mut out,
        store.send_with_id(
            &a,
            "all",
            "reply",
            SendOptions {
                reply_to: Some("fixture-4"),
                ..Default::default()
            },
            RecipientContext::default(),
            &ids.uuid().unwrap(),
        ),
    );
    capture(
        &mut out,
        store.inbox(
            &b,
            InboxOptions {
                peek: true,
                ids: Some(&[]),
                ..Default::default()
            },
            &ids,
        ),
    );
    capture(
        &mut out,
        store.inbox(
            &b,
            InboxOptions {
                peek: true,
                ..Default::default()
            },
            &ids,
        ),
    );
    capture(&mut out, store.inbox_digest(&b, None));
    capture(
        &mut out,
        store.inbox(
            &b,
            InboxOptions {
                claim_ttl_ms: 50,
                busy_timeout_ms: 1,
                ..Default::default()
            },
            &ids,
        ),
    );
    capture(&mut out, store.inbox(&b, InboxOptions::default(), &ids));
    capture(&mut out, store.acknowledge(&b, "fixture-4", "wrong"));
    clock.0.set(1050);
    capture(&mut out, store.acknowledge(&b, "fixture-4", "fixture-6"));
    capture(
        &mut out,
        store.inbox(
            &b,
            InboxOptions {
                ids: Some(&["fixture-4".into()]),
                ..Default::default()
            },
            &ids,
        ),
    );
    capture(&mut out, store.acknowledge(&b, "fixture-4", "fixture-6"));
    capture(&mut out, store.acknowledge(&b, "fixture-4", "fixture-8"));
    capture(&mut out, store.acknowledge(&b, "fixture-4", "fixture-8"));
    capture(
        &mut out,
        store.inbox(
            &b,
            InboxOptions {
                peek: true,
                ..Default::default()
            },
            &ids,
        ),
    );
    capture(&mut out, store.claim_task(&a, "task"));
    capture(&mut out, store.claim_task(&a, "task"));
    capture(&mut out, store.claim_task(&b, "task"));
    capture(&mut out, store.update_task(&a, "task", "claimed", None));
    capture(&mut out, store.update_task(&a, "task", "done", Some("")));
    capture(&mut out, store.claim_task(&a, "task"));
    capture(&mut out, store.tasks("p", None));
    capture(&mut out, store.tasks("p", Some("open")));
    capture(&mut out, store.claim_task(&a, "missing"));
    capture(&mut out, store.update_task(&a, "missing", "done", None));
    capture(&mut out, store.activity_after("p", 2, 3));
    capture(&mut out, store.activity("p", 2));
    let lane = |task: &str, attempt: i64| LaneAttempt {
        run_id: "r".into(),
        task_id: task.into(),
        attempt: attempt.into(),
    };
    capture(&mut out, store.mark_lanes_seen(&b, &[lane("t", 2)]));
    capture(&mut out, store.mark_lanes_seen(&b, &[lane("t", 1)]));
    capture(
        &mut out,
        store.unseen_lanes(
            &b,
            &[
                lane("t", 1),
                lane("t", 3),
                lane("zero", 0),
                LaneAttempt {
                    run_id: "".into(),
                    task_id: "t".into(),
                    attempt: 1.into(),
                },
            ],
        ),
    );
    capture(
        &mut out,
        store.send_with_id(
            &a,
            "bob",
            "already observed",
            SendOptions {
                kind: Some("run_terminal"),
                output_path: Some("r:t:2"),
                ..Default::default()
            },
            RecipientContext::default(),
            &ids.uuid().unwrap(),
        ),
    );
    capture(
        &mut out,
        store.send_with_id(
            &a,
            "bob",
            "new attempt",
            SendOptions {
                kind: Some("run_terminal"),
                output_path: Some("r:t:3"),
                ..Default::default()
            },
            RecipientContext::default(),
            &ids.uuid().unwrap(),
        ),
    );
    capture(
        &mut out,
        store.acknowledge_terminal(&b, "r", "t", 3, Some("/legacy"), 1),
    );
    capture(
        &mut out,
        store.inbox(
            &b,
            InboxOptions {
                peek: true,
                ..Default::default()
            },
            &ids,
        ),
    );
    capture(
        &mut out,
        store.inbox(
            &b,
            InboxOptions {
                busy_timeout_ms: 0,
                ..Default::default()
            },
            &ids,
        ),
    );
    capture(
        &mut out,
        store.inbox(
            &b,
            InboxOptions {
                limit: 0,
                ..Default::default()
            },
            &ids,
        ),
    );
    capture(
        &mut out,
        store.inbox(
            &b,
            InboxOptions {
                claim_ttl_ms: 0,
                ..Default::default()
            },
            &ids,
        ),
    );
    capture(
        &mut out,
        store.inbox(
            &b,
            InboxOptions {
                task_id: Some(""),
                ..Default::default()
            },
            &ids,
        ),
    );
    capture(&mut out, store.activity_after("p", -1, 200));
    capture(&mut out, store.activity("p", 0));
    compare("messages", snapshot(&store, out));
}
#[test]
fn sqlite_contention_rolls_back_and_restores_timeout_differential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path, 5000, FixtureClock(Arc::new(TimeCell::new(1000)))).unwrap();
    let raw = Connection::open(&path).unwrap();
    let a = a();
    let b = b();
    let ids = FixtureIds::default();
    let mut out = Vec::new();
    store.announce(&a).unwrap();
    store.announce(&b).unwrap();
    store
        .send_with_id(
            &a,
            "bob",
            "claim me",
            SendOptions::default(),
            RecipientContext::default(),
            &ids.uuid().unwrap(),
        )
        .unwrap();
    raw.execute_batch("BEGIN IMMEDIATE").unwrap();
    let claim = store.inbox(
        &b,
        InboxOptions {
            busy_timeout_ms: 1,
            ..Default::default()
        },
        &ids,
    );
    assert!(claim.as_ref().is_err_and(is_sqlite_contention));
    capture(&mut out, claim);
    capture(
        &mut out,
        store.acknowledge_terminal(&b, "r", "t", 1, Some("/r"), 1),
    );
    raw.execute_batch("ROLLBACK").unwrap();
    capture(&mut out, store.inbox(&b, InboxOptions::default(), &ids));
    compare("contention", snapshot(&store, out));
}
#[test]
fn legacy_nullable_column_migration_differential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch("CREATE TABLE messages(message_id TEXT PRIMARY KEY, project TEXT NOT NULL, sender_id TEXT NOT NULL,
    body TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'note', conversation_id TEXT NOT NULL,
    reply_to TEXT REFERENCES messages(message_id), created_at INTEGER NOT NULL);
    INSERT INTO messages VALUES ('old', 'p', 'alice', 'retained', 'note', 'old', NULL, 1000)").unwrap();
    drop(raw);
    let store = Store::open(&path, 5000, FixtureClock(Arc::new(TimeCell::new(1000)))).unwrap();
    let mut out = Vec::new();
    capture(
        &mut out,
        store.inbox(
            &a(),
            InboxOptions {
                peek: true,
                ..Default::default()
            },
            &FixtureIds::default(),
        ),
    );
    compare("legacy", snapshot(&store, out));
}
#[test]
fn diagnostic_policy_differential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path, 5000, FixtureClock(Arc::new(TimeCell::new(1000)))).unwrap();
    store.announce(&a()).unwrap();
    store.note(&a(), "seed").unwrap();
    let mut out = Vec::new();
    let private = Connection::open(&path).unwrap();
    capture(
        &mut out,
        Ok(inspect_snapshot(
            &private,
            "<fixture-db>",
            "p",
            DiagnosticMode::Doctor,
        )),
    );
    store
        .db
        .execute_batch("INSERT INTO teams VALUES ('p','alice',1000)")
        .unwrap();
    capture(
        &mut out,
        Ok(inspect_snapshot(
            &private,
            "<fixture-db>",
            "p",
            DiagnosticMode::Doctor,
        )),
    );
    compare("diagnostics", snapshot(&store, out));
}

#[test]
fn javascript_date_range_and_signed_year_differential() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        &dir.path().join("db"),
        5000,
        FixtureClock(Arc::new(TimeCell::new(1000))),
    )
    .unwrap();
    store.note(&a(), "date fixture").unwrap();
    let mut out = Vec::new();
    for at in [
        -8_640_000_000_000_001_i64,
        -8_640_000_000_000_000,
        -62_167_219_200_000,
        -1,
        0,
        951_782_400_000,
        253_402_300_800_000,
        8_640_000_000_000_000,
        8_640_000_000_000_001,
    ] {
        store.db.execute("UPDATE activity SET at=?", [at]).unwrap();
        capture(&mut out, store.activity("p", 50));
    }
    compare("dates", snapshot(&store, out));
}

#[test]
fn review_routing_announce_failures_and_push_takeover_differential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path, 5000, FixtureClock(Arc::new(TimeCell::new(1000)))).unwrap();
    let other = Store::open(&path, 5000, FixtureClock(Arc::new(TimeCell::new(1000)))).unwrap();
    let ids = FixtureIds::default();
    let mut out = Vec::new();
    store.announce(&a()).unwrap();
    capture(
        &mut out,
        store.announce(&Identity {
            provider: "claude".into(),
            ..a()
        }),
    );
    capture(
        &mut out,
        store.create_team(&a(), "future", &["alice".into()]),
    );
    capture(
        &mut out,
        store.announce(&Identity {
            agent_id: "future".into(),
            ..a()
        }),
    );
    capture(
        &mut out,
        store.announce(&Identity {
            agent_id: "all".into(),
            ..a()
        }),
    );
    for seat in ["chair", "leader", "parent-seat"] {
        store
            .announce(&Identity {
                agent_id: seat.into(),
                ..a()
            })
            .unwrap();
    }
    for alias in ["chair", "parent", "root", "/root"] {
        capture(
            &mut out,
            store.send(
                &a(),
                alias,
                "question",
                SendOptions::default(),
                RecipientContext {
                    chair: Some("leader"),
                    parent: Some("parent-seat"),
                },
                &ids,
            ),
        );
    }
    capture(
        &mut out,
        store.send(
            &a(),
            "parent",
            "fallback",
            SendOptions::default(),
            RecipientContext {
                chair: Some("missing"),
                parent: Some("missing"),
            },
            &ids,
        ),
    );
    let sha = "a".repeat(40);
    capture(&mut out, store.acquire_landing(&a(), "s", &sha, 1, 1000));
    store
        .db
        .execute_batch("UPDATE landing_leases SET pushing_until=151000")
        .unwrap();
    capture(&mut out, other.acquire_landing(&b(), "s", &sha, 60, 2001));
    capture(
        &mut out,
        store.try_release_landing(&a(), "s", 1, Some(2001)),
    );
    capture(&mut out, other.acquire_landing(&b(), "s", &sha, 60, 151000));
    compare("review", snapshot(&store, out));
}

#[test]
fn landing_wait_change_timeout_abort_expiry_and_generation_differential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let clock = FixtureClock(Arc::new(TimeCell::new(1000)));
    let store = Store::open(&path, 5000, clock.clone()).unwrap();
    let raw = Connection::open(&path).unwrap();
    let mut out = Vec::new();
    capture(
        &mut out,
        store.wait_landing(
            "p",
            0.003,
            1,
            || false,
            |d| clock.0.set(clock.0.get() + d.as_millis() as i64),
        ),
    );
    capture(
        &mut out,
        store.wait_landing("p", 1.0, 1, || true, |_| panic!("aborted")),
    );
    clock.0.set(1000);
    store
        .acquire_landing(&a(), "s", &"a".repeat(40), 60, 1000)
        .unwrap();
    let mut polls = 0;
    capture(
        &mut out,
        store.wait_landing(
            "p",
            1.0,
            1,
            || false,
            |d| {
                clock.0.set(clock.0.get() + d.as_millis() as i64);
                polls += 1;
                raw.execute_batch(if polls == 1 {
                    "UPDATE landing_leases SET pushing_until=151000"
                } else {
                    "UPDATE landing_leases SET released_at=1002"
                })
                .unwrap();
            },
        ),
    );
    store
        .acquire_landing(&a(), "s", &"a".repeat(40), 1, clock.0.get())
        .unwrap();
    capture(
        &mut out,
        store.wait_landing(
            "p",
            2.0,
            500,
            || false,
            |d| clock.0.set(clock.0.get() + d.as_millis() as i64),
        ),
    );
    store
        .acquire_landing(&a(), "s", &"a".repeat(40), 60, clock.0.get())
        .unwrap();
    capture(
        &mut out,
        store.wait_landing(
            "p",
            1.0,
            1,
            || false,
            |d| {
                clock.0.set(clock.0.get() + d.as_millis() as i64);
                raw.execute_batch("UPDATE landing_leases SET generation=generation+1")
                    .unwrap();
            },
        ),
    );
    compare("waits", snapshot(&store, out));
}

#[test]
fn cross_thread_sqlite_contention_differential() {
    use std::sync::mpsc;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path, 5000, FixtureClock(Arc::new(TimeCell::new(1000)))).unwrap();
    let ids = FixtureIds::default();
    let mut out = Vec::new();
    store.announce(&a()).unwrap();
    store.announce(&b()).unwrap();
    store
        .send(
            &a(),
            "bob",
            "claim me",
            SendOptions::default(),
            RecipientContext::default(),
            &ids,
        )
        .unwrap();
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            let raw = Connection::open(&path).unwrap();
            raw.execute_batch("BEGIN IMMEDIATE").unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            raw.execute_batch("ROLLBACK").unwrap();
        });
        locked_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        capture(
            &mut out,
            store.inbox(
                &b(),
                InboxOptions {
                    busy_timeout_ms: 1,
                    ..Default::default()
                },
                &ids,
            ),
        );
        capture(
            &mut out,
            store.acknowledge_terminal(&b(), "r", "t", 1, Some("/r"), 1),
        );
        release_tx.send(()).unwrap();
        writer.join().unwrap();
    });
    capture(&mut out, store.inbox(&b(), InboxOptions::default(), &ids));
    compare("thread-contention", snapshot(&store, out));
}

#[test]
fn work_claim_fencing_paths_errors_and_rollback_differential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path, 5000, FixtureClock(Arc::new(TimeCell::new(1000)))).unwrap();
    let other = Store::open(&path, 5000, FixtureClock(Arc::new(TimeCell::new(1000)))).unwrap();
    let ids = FixtureIds::default();
    let mut out = Vec::new();
    store.announce(&a()).unwrap();
    store.announce(&b()).unwrap();
    store
        .announce(&Identity {
            project: "z".into(),
            ..a()
        })
        .unwrap();
    capture(&mut out, store.agents("p"));
    capture(&mut out, store.projects());
    capture(
        &mut out,
        store.acquire_work(
            &a(),
            "s",
            WorkScope {
                issue: Some(" #869 "),
                paths: &["Runtime\\Fabric/".into(), "runtime/fabric".into()],
            },
            1,
            1000,
            &ids,
        ),
    );
    capture(&mut out, store.verify_work(&a(), "s", "fixture-1", 1, 1000));
    capture(
        &mut out,
        other.acquire_work(
            &b(),
            "s",
            WorkScope {
                issue: Some("869"),
                ..Default::default()
            },
            60,
            1000,
            &ids,
        ),
    );
    capture(
        &mut out,
        other.acquire_work(
            &b(),
            "s",
            WorkScope {
                paths: &["RUNTIME/FABRIC/SRC".into()],
                ..Default::default()
            },
            60,
            1000,
            &ids,
        ),
    );
    capture(
        &mut out,
        store.renew_work(&a(), "wrong", "fixture-1", 1, 60, 1000),
    );
    capture(
        &mut out,
        store.renew_work(&a(), "s", "fixture-1", 1, 1, 1000),
    );
    capture(
        &mut out,
        other.acquire_work(
            &b(),
            "s",
            WorkScope {
                paths: &["runtime/fabric/src".into()],
                ..Default::default()
            },
            60,
            2000,
            &ids,
        ),
    );
    capture(
        &mut out,
        store.renew_work(&a(), "s", "fixture-1", 1, 60, 2000),
    );
    capture(&mut out, store.verify_work(&a(), "s", "fixture-1", 1, 2000));
    capture(
        &mut out,
        store.release_work(&a(), "s", "fixture-1", 1, 2000),
    );
    capture(&mut out, store.work_claims("p", 2000));
    capture(
        &mut out,
        other.release_work(&b(), "s", "fixture-4", 2, 2000),
    );
    for path in ["", "/", "a//b", "a/../b", "./a", "a//"] {
        capture(
            &mut out,
            store.acquire_work(
                &a(),
                "s",
                WorkScope {
                    paths: &[path.into()],
                    ..Default::default()
                },
                60,
                2000,
                &ids,
            ),
        );
    }
    capture(
        &mut out,
        store.acquire_work(
            &a(),
            "s",
            WorkScope {
                issue: Some("#"),
                ..Default::default()
            },
            60,
            2000,
            &ids,
        ),
    );
    capture(
        &mut out,
        store.acquire_work(
            &a(),
            "s",
            WorkScope {
                paths: &vec!["a".into(); 101],
                ..Default::default()
            },
            60,
            2000,
            &ids,
        ),
    );
    capture(
        &mut out,
        store.acquire_work(
            &a(),
            "bad/session",
            WorkScope {
                issue: Some("x"),
                ..Default::default()
            },
            60,
            2000,
            &ids,
        ),
    );
    capture(
        &mut out,
        store.acquire_work(
            &a(),
            "s",
            WorkScope {
                issue: Some("x"),
                ..Default::default()
            },
            0,
            2000,
            &ids,
        ),
    );
    store.db.execute_batch("CREATE TRIGGER fail_log BEFORE INSERT ON activity BEGIN SELECT RAISE(ABORT,'log blocked'); END").unwrap();
    capture(
        &mut out,
        store.acquire_work(
            &a(),
            "s",
            WorkScope {
                paths: &["different".into()],
                ..Default::default()
            },
            60,
            2000,
            &ids,
        ),
    );
    store.db.execute_batch("DROP TRIGGER fail_log").unwrap();
    capture(&mut out, store.work_claims("p", 2000));
    compare("work", snapshot(&store, out));
    store.close().unwrap();
    other.close().unwrap();
}

#[test]
fn entropy_defaults_and_failure_order_differential() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        &dir.path().join("db"),
        5000,
        FixtureClock(Arc::new(TimeCell::new(1000))),
    )
    .unwrap();
    let ids = FixtureIds::default();
    let mut out = Vec::new();
    store.announce(&a()).unwrap();
    store.announce(&b()).unwrap();
    capture(
        &mut out,
        store.send(
            &a(),
            "bob",
            "empty task",
            SendOptions {
                task_id: Some(""),
                ..Default::default()
            },
            RecipientContext::default(),
            &ids,
        ),
    );
    capture(
        &mut out,
        store.send(
            &a(),
            "bob",
            "empty output",
            SendOptions {
                output_path: Some(""),
                ..Default::default()
            },
            RecipientContext::default(),
            &ids,
        ),
    );
    capture(
        &mut out,
        store.send(
            &a(),
            "missing",
            "failed recipient",
            SendOptions::default(),
            RecipientContext::default(),
            &ids,
        ),
    );
    capture(
        &mut out,
        store.claim_session_turn(&a(), "name", None, SessionTurnKind::Start, 123, &ids),
    );
    capture(
        &mut out,
        store.claim_session_turn(&a(), "name", None, SessionTurnKind::Start, 124, &ids),
    );
    capture(
        &mut out,
        store.create_task(&a(), "generated", CreateTaskOptions::default(), &ids),
    );
    capture(
        &mut out,
        store.create_task(
            &a(),
            "empty",
            CreateTaskOptions {
                task_id: Some(""),
                ..Default::default()
            },
            &ids,
        ),
    );
    capture(
        &mut out,
        store.send(
            &a(),
            "bob",
            "generated",
            SendOptions::default(),
            RecipientContext::default(),
            &ids,
        ),
    );
    capture(&mut out, store.inbox(&b(), InboxOptions::default(), &ids));
    compare("ids", snapshot(&store, out));
}

#[test]
fn legacy_terminal_rows_preserve_missing_null_coercion_and_binding_errors() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        &dir.path().join("db"),
        5000,
        FixtureClock(Arc::new(TimeCell::new(1000))),
    )
    .unwrap();
    let mut out = Vec::new();
    for text in [
        r#"{}"#,
        r#"{"run_id":null,"task_id":"","attempt_count":"2"}"#,
        r#"{"run_id":42,"task_id":0,"attempt":null,"attempt_count":true}"#,
        r#"{"run_id":["r",null,["x"]],"task_id":[],"attempt":[]}"#,
        r#"{"run_id":{},"task_id":"t","attempt":"2.5"}"#,
        r#"{"run_id":"r","task_id":"t","attempt":"0x10000000000000001"}"#,
        r#"{"run_id":"r","attempt":"invalid"}"#,
        r#"{"run_id":"r","attempt":"Infinity"}"#,
        r#"{"run_id":"r","attempt":"-Infinity"}"#,
        r#"{"run_id":{"toString":null},"attempt":1}"#,
        r#"{"run_id":"r","attempt":1,"run_dir":false}"#,
        r#"{"run_id":"r","attempt":null}"#,
        r#"{"run_id":"r","attempt":"","run_dir":null}"#,
        r#"{"run_id":1e21,"attempt":1}"#,
        r#"{"run_id":1e-7,"attempt":1}"#,
        r#"{"run_id":1e20,"attempt":1}"#,
        r#"{"run_id":true,"attempt":["3"]}"#,
        r#"{"run_id":1e-6,"attempt":1}"#,
    ] {
        let row = TerminalRow::from_json(text).unwrap();
        capture(&mut out, lane_task_id(&row));
        capture(&mut out, store.acknowledge_terminal_row(&b(), &row, 1));
    }
    let lanes = vec![
        LaneAttempt {
            run_id: "r".into(),
            task_id: "t".into(),
            attempt: 3.into(),
        },
        LaneAttempt {
            run_id: "r".into(),
            task_id: "t".into(),
            attempt: serde_json::Number::from_f64(2.5).unwrap(),
        },
        LaneAttempt {
            run_id: "zero".into(),
            task_id: "t".into(),
            attempt: 0.into(),
        },
    ];
    capture(&mut out, store.unseen_lanes(&b(), &lanes));
    capture(&mut out, store.mark_lanes_seen(&b(), &lanes));
    capture(&mut out, store.unseen_lanes(&b(), &lanes));
    store.announce(&a()).unwrap();
    store.announce(&b()).unwrap();
    capture(
        &mut out,
        store.send(
            &a(),
            "bob",
            "fraction seen",
            SendOptions {
                kind: Some("run_terminal"),
                output_path: Some("r:t:2.5"),
                ..Default::default()
            },
            RecipientContext::default(),
            &FixtureIds::default(),
        ),
    );
    compare("legacy-rows", snapshot(&store, out));
}

fn capture_report(out: &mut Vec<Value>, mut report: DatabaseDiagnostic, base: &std::path::Path) {
    report.database = "<fixture-db>".into();
    report.error = report
        .error
        .map(|e| e.replace(base.to_string_lossy().as_ref(), "<fixture-db>"));
    capture(out, Ok(report));
}
#[test]
fn diagnostic_absence_stat_errors_invalid_db_and_wal_snapshot_differential() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path, 5000, FixtureClock(Arc::new(TimeCell::new(1000)))).unwrap();
    store.announce(&a()).unwrap();
    store.note(&a(), "in WAL").unwrap();
    let mut out = Vec::new();
    for (file, mode) in [
        (&path, DiagnosticMode::Status),
        (&path, DiagnosticMode::Doctor),
        (&dir.path().join("db.absent/deep"), DiagnosticMode::Status),
    ] {
        capture_report(
            &mut out,
            inspect_database_with_filesystem(
                file,
                "p",
                mode,
                &provenant_supervisor::fs::LocalFs,
                scratch.path(),
            ),
            &path,
        );
    }
    std::fs::write(dir.path().join("db.bad"), b"not sqlite").unwrap();
    for (file, mode) in [
        (dir.path().join("db.bad"), DiagnosticMode::Doctor),
        (dir.path().join("db.bad/deep"), DiagnosticMode::Status),
    ] {
        capture_report(
            &mut out,
            inspect_database_with_filesystem(
                &file,
                "p",
                mode,
                &provenant_supervisor::fs::LocalFs,
                scratch.path(),
            ),
            &path,
        );
    }
    capture(
        &mut out,
        Ok(std::fs::read_dir(scratch.path()).unwrap().count()),
    );
    compare("diagnostic-paths", snapshot(&store, out));
}

struct MutatingFs {
    source: std::path::PathBuf,
    copies: Cell<u64>,
    always: bool,
}
impl provenant_supervisor::fs::SafeFs for MutatingFs {
    fn root(&self, absolute: bool) -> std::io::Result<Box<dyn provenant_supervisor::fs::SafeDir>> {
        provenant_supervisor::fs::LocalFs.root(absolute)
    }
    fn open_anchor(
        &self,
        path: &std::path::Path,
    ) -> std::io::Result<Box<dyn provenant_supervisor::fs::SafeDir>> {
        provenant_supervisor::fs::LocalFs.open_anchor(path)
    }
    fn temporary_name(&self) -> std::io::Result<std::ffi::OsString> {
        provenant_supervisor::fs::LocalFs.temporary_name()
    }
    fn open_source(
        &self,
        path: &std::path::Path,
    ) -> std::io::Result<Box<dyn provenant_supervisor::fs::SafeFile>> {
        let file = provenant_supervisor::fs::LocalFs.open_source(path)?;
        if path == self.source {
            self.copies.set(self.copies.get() + 1);
            if self.always || self.copies.get() == 1 {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(path)?
                    .set_times(std::fs::FileTimes::new().set_modified(
                        std::time::UNIX_EPOCH + Duration::from_secs(100 + self.copies.get()),
                    ))?;
            }
        }
        Ok(file)
    }
}
#[test]
fn diagnostic_snapshot_retries_version_changes_and_has_a_three_attempt_bound() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path, 5000, FixtureClock(Arc::new(TimeCell::new(1000)))).unwrap();
    store.announce(&a()).unwrap();
    let mut out = Vec::new();
    for always in [false, true] {
        let fs = MutatingFs {
            source: path.clone(),
            copies: Cell::new(0),
            always,
        };
        capture_report(
            &mut out,
            inspect_database_with_filesystem(
                &path,
                "p",
                DiagnosticMode::Doctor,
                &fs,
                scratch.path(),
            ),
            &path,
        );
        capture(&mut out, Ok(fs.copies.get()));
    }
    capture(
        &mut out,
        Ok(std::fs::read_dir(scratch.path()).unwrap().count()),
    );
    compare("snapshot-races", snapshot(&store, out));
}

#[test]
fn persisted_work_paths_keep_dynamic_json_and_short_circuit_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path, 5000, FixtureClock(Arc::new(TimeCell::new(1000)))).unwrap();
    let ids = FixtureIds::default();
    let mut out = Vec::new();
    store
        .acquire_work(
            &a(),
            "s",
            WorkScope {
                issue: Some("x"),
                paths: &["valid".into()],
            },
            60,
            1000,
            &ids,
        )
        .unwrap();
    for value in [
        Value::Null,
        json!(false),
        json!("text"),
        json!({"x":1}),
        json!([null]),
        json!([false]),
        json!(["elsewhere", false]),
        json!(["match", false]),
    ] {
        store
            .db
            .execute(
                "UPDATE work_claims SET paths=? WHERE id=?",
                params![serde_json::to_string(&value).unwrap(), "fixture-1"],
            )
            .unwrap();
        capture(&mut out, store.work_claims("p", 1000));
        capture(
            &mut out,
            store.acquire_work(
                &b(),
                "s",
                WorkScope {
                    issue: Some("x"),
                    ..Default::default()
                },
                60,
                1000,
                &ids,
            ),
        );
        capture(
            &mut out,
            store.acquire_work(
                &b(),
                "s",
                WorkScope {
                    issue: Some("y"),
                    ..Default::default()
                },
                60,
                1000,
                &ids,
            ),
        );
        capture(
            &mut out,
            store.acquire_work(
                &b(),
                "s",
                WorkScope {
                    paths: &["match".into()],
                    ..Default::default()
                },
                60,
                1000,
                &ids,
            ),
        );
        store
            .db
            .execute("DELETE FROM work_claims WHERE id != 'fixture-1'", [])
            .unwrap();
    }
    compare("work-path-json", snapshot(&store, out));
}
