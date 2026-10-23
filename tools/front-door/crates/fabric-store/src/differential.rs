use crate::*;
use serde_json::{json, Value};
use std::{cell::Cell, rc::Rc};
#[derive(Clone)]
struct FixtureClock(Rc<Cell<i64>>);
impl Clock for FixtureClock {
    fn now_ms(&self) -> i64 {
        self.0.get()
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
    out.push(match result {
        Ok(value) => json!({"ok":value}),
        Err(error) => json!({"error":error.to_string()}),
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
        assert_eq!(
            result,
            expected,
            "{scenario}: actual {}, expected {}",
            path.display(),
            expected_path.display()
        );
    }
}
#[test]
fn landing_database_and_failure_differential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let clock = FixtureClock(Rc::new(Cell::new(1000)));
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
        FixtureClock(Rc::new(Cell::new(1000))),
    )
    .unwrap();
    let a = a();
    let b = b();
    let mut out = Vec::new();
    capture(
        &mut out,
        store.claim_session_turn(&a, "Review", None, SessionTurnKind::Start, 123, "fixture-1"),
    );
    capture(
        &mut out,
        store.claim_session_turn(&b, "Review", None, SessionTurnKind::Start, 124, "fixture-2"),
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
        store.claim_session_turn(
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
        store.claim_session_turn(
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
        store.claim_session_turn(
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
        store.claim_session_turn(
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
