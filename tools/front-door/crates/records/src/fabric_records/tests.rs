use super::*;
use serde_json::json;
use std::path::Path;
use std::process::Command;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../")
            .join(name),
    )
    .unwrap()
}
#[test]
fn retained_attempt_roundtrips_without_losing_unowned_fields() {
    let value: Value =
        serde_json::from_slice(&fixture("tests/fixtures/fabric-v1/attempt.json")).unwrap();
    let row: AttemptRow = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(row).unwrap(), value);
}
#[test]
fn optional_fields_preserve_absence_null_and_unknowns() {
    for field in [None, Some(Value::Null), Some(json!("ok"))] {
        let mut value = json!({"run_id":"run","task_id":"task","attempt":1,"future":{"x":true}});
        if let Some(status) = field {
            value["status"] = status;
        }
        let row: AttemptRow = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(row).unwrap(), value);
    }
}
#[test]
fn j2_cooldown_and_route_health_fixtures_roundtrip() {
    let cooldowns = fixture("schemas/fixtures/cooldowns.valid.json");
    let health = fixture("schemas/fixtures/route-health.valid.json");
    assert_eq!(
        serde_json::to_value(Cooldowns::decode(&cooldowns).unwrap()).unwrap(),
        serde_json::from_slice::<Value>(&cooldowns).unwrap()
    );
    assert_eq!(
        serde_json::to_value(RouteHealthDocument::decode(&health).unwrap()).unwrap(),
        serde_json::from_slice::<Value>(&health).unwrap()
    );
}
#[test]
fn scope_keeps_missing_and_explicit_unknown_separate() {
    for scope in [
        json!({}),
        json!({"account":null,"host":null}),
        json!({"account":"pro","host":"peer"}),
    ] {
        let s: Scope = serde_json::from_value(scope.clone()).unwrap();
        assert_eq!(serde_json::to_value(s).unwrap(), scope);
    }
}
#[test]
fn index_deduplication_does_not_hide_later_attempts() {
    let value = json!({"run_id":"run","task_id":"task","attempt":1,"dir":"/run","status":"ok","line":"route","result_sha256":null,"ended_at":"2026-10-23T00:00:00Z"});
    let first: IndexEntry = serde_json::from_value(value.clone()).unwrap();
    let mut next = first.clone();
    assert_eq!(first.key(), next.key());
    next.attempt += 1;
    assert_ne!(first.key(), next.key());
    assert_eq!(serde_json::to_value(first).unwrap(), value);
}
#[test]
fn codecs_leave_schema_policy_with_the_store_owner() {
    assert_eq!(
        Cooldowns::decode(br#"{"schema":"fabric.cooldowns.v2","cooldowns":{}}"#)
            .unwrap()
            .schema,
        "fabric.cooldowns.v2"
    );
    assert_eq!(
        RouteHealthDocument::decode(br#"{"schema":"fabric.route-health.v2","routes":{}}"#)
            .unwrap()
            .schema,
        "fabric.route-health.v2"
    );
}
#[test]
fn cooldown_keys_are_collision_free_and_scope_specific() {
    let a = Scope {
        account: Field::Value("a,b".into()),
        host: Field::Value("host".into()),
    };
    let b = Scope {
        account: Field::Value("a".into()),
        host: Field::Value("b,host".into()),
    };
    assert_ne!(
        cooldown_key("adapter", "model", &a).unwrap(),
        cooldown_key("adapter", "model", &b).unwrap()
    );
    assert_eq!(
        cooldown_key("adapter", "model", &Scope::default()).unwrap(),
        r#"["adapter",null,null,"model"]"#
    );
}
#[test]
fn differential_python_keys_and_terminal_statuses() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../skills/orchestrate/scripts/fabric_records.py");
    let output=match Command::new("python3").args(["-c",r#"
import json,sys
sys.path.insert(0,sys.argv[1]);import fabric_records as m
scopes=[{}, {'account':None,'host':None},{'account':'a,b','host':'peer'},{'account':'é😀\x7f','host':'"\\\n'}]
print(json.dumps({'keys':[m.cooldown_key('adäpter','model/é',s) for s in scopes],'statuses':sorted(m.TERMINAL_STATUSES)}))
"#]).arg(path.parent().unwrap()).output() {
        Ok(output)=>output,
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{eprintln!("skipped: python3 is absent");return;},
        Err(error)=>panic!("cannot run Python oracle: {error}"),
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let scopes = [
        json!({}),
        json!({"account":null,"host":null}),
        json!({"account":"a,b","host":"peer"}),
        json!({"account":"é😀\u{007f}","host":"\"\\\n"}),
    ];
    let keys: Vec<_> = scopes
        .into_iter()
        .map(|v| cooldown_key("adäpter", "model/é", &serde_json::from_value(v).unwrap()).unwrap())
        .collect();
    assert_eq!(json!(keys), result["keys"]);
    let mut statuses = TERMINAL_STATUSES.to_vec();
    statuses.sort();
    assert_eq!(json!(statuses), result["statuses"]);
}
