use super::*;
use serde_json::json;

#[test]
fn trust_and_effort_refusals_match_python_at_exact_age_boundaries() {
    let catalog = json!({"adapters":{"claude":{"fixed_model_family":"anthropic"}},"families":{"anthropic":{"aliases":{"senior":["sonnet"]}}}});
    let valid = json!({"schema_version":1,"source":"codex debug models","observed_at":"2026-10-23T00:00:00Z","models":{"GPT-6":{"resolved_model":"gpt-6","supported_efforts":["high"],"extra":null}}});
    let mut cases = vec![
        json!(["codex", valid.to_string()]),
        json!(["agy", valid.to_string()]),
        json!(["codex", "null"]),
        json!(["codex", "{bad"]),
        json!(["codex", r#"{"models":{},"models":{}}"#]),
    ];
    for (key, values) in [
        (
            "schema_version",
            vec![json!(true), json!(1.0), json!(2), json!(null)],
        ),
        (
            "observed_at",
            vec![
                json!("2026-10-22T23:55:00Z"),
                json!("2026-10-22T23:54:59.999999Z"),
                json!("2026-10-23T00:01:00Z"),
                json!("2026-10-23T00:01:00.000001Z"),
                json!("2026-10-23T00:00:00"),
                json!("bad"),
                json!(null),
            ],
        ),
        ("source", vec![json!("unknown"), json!(null)]),
        (
            "models",
            vec![
                json!({}),
                json!([]),
                json!({"x":null}),
                json!({"gpt-6":{"resolved_model":"other","supported_efforts":["high"]}}),
                json!({"gpt-6":{"resolved_model":"gpt-6","supported_efforts":[]}}),
                json!({"gpt-6":{"resolved_model":"gpt-6","supported_efforts":[true]}}),
                json!({"gpt-6":{"resolved_model":"gpt-6","supported_efforts":["high"]},"GPT-6":{"resolved_model":"gpt-6","supported_efforts":["low"]}}),
            ],
        ),
    ] {
        for value in values {
            let mut item = valid.clone();
            item[key] = value;
            cases.push(json!(["codex", item.to_string()]));
        }
    }
    let claude = json!({"schema_version":1,"source":"claude subscription canary","observed_at":"2026-10-23T00:00:00+00:00","provenance":{"kind":"subscription_runtime_canary","auth_method":"claude.ai","subscription_type":"max"},"models":{"sonnet":{"resolved_model":"claude-sonnet-5-5","requested_effort":"high","effort_verified":false}}});
    cases.push(json!(["claude", claude.to_string()]));
    for (key, value) in [
        ("provenance", json!({})),
        (
            "models",
            json!({"sonnet":{"resolved_model":"claude-sonnet-5-5","requested_effort":"high","effort_verified":true}}),
        ),
        (
            "models",
            json!({"sonnet":{"resolved_model":"claude-sonnet-5-5","requested_effort":"high","effort_verified":false,"supported_efforts":null}}),
        ),
    ] {
        let mut c = claude.clone();
        c[key] = value;
        cases.push(json!(["claude", c.to_string()]));
    }
    let Some(expected): Option<Value> = crate::oracle::python(
        r#"
import importlib.util,json,pathlib,sys,tempfile
from datetime import datetime,timezone
sys.path.insert(0,str(pathlib.Path('scripts').resolve()))
import model_route_probe as m
class Clock(datetime):
 @classmethod
 def now(cls,tz=None):return cls(2026,10,23,tzinfo=timezone.utc)
m.datetime=Clock
d=json.load(sys.stdin); results=[]
with tempfile.TemporaryDirectory() as directory:
 path=pathlib.Path(directory)/'snapshot.json'
 for adapter,raw in d['cases']:
  path.write_text(raw); results.append(m.load_capabilities(str(path),adapter,d['catalog']))
print(json.dumps(results))
"#,
        &json!({"cases":cases,"catalog":catalog}),
    ) else {
        return;
    };
    let now = "2026-10-23T00:00:00Z".parse().unwrap();
    let actual: Vec<_> = cases
        .iter()
        .map(|c| {
            let (models, error) = load_capabilities(
                c[1].as_str().unwrap(),
                c[0].as_str().unwrap(),
                &catalog,
                now,
            )
            .unwrap();
            json!([models, error.map(|e| e.to_string()).unwrap_or_default()])
        })
        .collect();
    assert_eq!(json!(actual), expected);
}

#[test]
fn duplicate_nested_members_and_out_of_range_integers_cannot_enter_snapshots() {
    assert!(load_json(r#"{"a":{"x":1,"x":2}}"#).is_err());
    assert!(matches!(
        load_json("18446744073709551616"),
        Err(JsonError::NumberOutOfRange)
    ));
    let now = "2026-10-23T00:00:00Z".parse().unwrap();
    assert_eq!(
        load_capabilities_path(None, "codex", &json!({}), now).unwrap(),
        (OrderedObject::default(), None)
    );
    assert_eq!(
        load_capabilities_path(
            Some(Path::new("/nonexistent/provenant-test")),
            "codex",
            &json!({}),
            now
        )
        .unwrap()
        .1,
        Some(CapabilityFailure::DiscoveryFailed)
    );
}

#[test]
fn python_iso_dates_offsets_and_microseconds_match_the_snapshot_oracle() {
    let stamps = [
        "2026-10-23T00:00:00Z",
        "20261023x000000+00",
        "2026-W43-5T00:00:00+00:00",
        "2026W435T000000+0000",
        "2026-10-23\n00:00:00Z",
        "2026-10-23T00:00:00,000001Z",
        "2026-10-23T00:00:00.000000999Z",
        "2026-10-23T01:39:00+00:99",
        "2026-10-23T00:00:00",
        "bad",
        "2026-10-23T24:00:00Z",
        "0000-10-23T00:00:00Z",
        "2026-10-23T12.5Z",
    ];
    let Some(expected): Option<Vec<Option<String>>> = crate::oracle::python(
        r#"
from datetime import datetime,timezone
import json,sys
out=[]
for raw in json.load(sys.stdin):
 try:
  d=datetime.fromisoformat(raw.replace('Z','+00:00'))
  out.append(d.astimezone(timezone.utc).isoformat(timespec='microseconds') if d.tzinfo else None)
 except ValueError:out.append(None)
print(json.dumps(out))
"#,
        &json!(stamps),
    ) else {
        return;
    };
    for (raw, expected) in stamps.into_iter().zip(expected) {
        let actual = pending::timestamp(raw).map(|v| v.to_string());
        let expected = expected
            .and_then(|v| v.parse::<jiff::Timestamp>().ok())
            .map(|v| v.to_string());
        assert_eq!(actual, expected, "{raw}");
    }
}
