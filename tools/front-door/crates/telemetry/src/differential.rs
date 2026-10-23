//! Independent Python oracle on isolated fixtures. No installed command or live ledger is used.
use crate::{costs, delivery, ingest, report, Numeric, Usage};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
fn python(source: &str, input: &Value) -> Option<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let mut child = match Command::new("python3")
        .arg("-c")
        .arg(source)
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("SKIP: Python oracle interpreter absent");
            return None;
        }
        Err(e) => panic!("Python oracle could not run: {e}"),
    };
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(input).unwrap().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(serde_json::from_slice(&out.stdout).unwrap())
}
fn equal(a: &Value, b: &Value) {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => assert!(
            (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() <= 1e-12,
            "{a} != {b}"
        ),
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(b) {
                equal(a, b);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
            for (k, a) in a {
                equal(a, &b[k]);
            }
        }
        _ => assert_eq!(a, b),
    }
}
const LOAD: &str = r#"
import importlib.util,json,sys,types
from pathlib import Path
root=Path(sys.argv[1])
def load(name):
 s=importlib.util.spec_from_file_location(name,root/'scripts'/f'{name}.py'); m=importlib.util.module_from_spec(s);s.loader.exec_module(m);return m
input=json.load(sys.stdin)
costs=load('telemetry_costs')
def number(v):return v if isinstance(v,(int,float)) and not isinstance(v,bool) and v>=0 else None
t=types.SimpleNamespace(number=number)
"#;
#[test]
fn costs_and_outcomes_match_python() {
    let mut rows = Vec::new();
    for (attempt, status, tokens) in [(1.0, "failed", None), (2.0, "ok", Some(Numeric::from(100)))]
    {
        rows.push(Usage {
            adapter: Some("adapter".into()),
            provider: Some("provider".into()),
            model: Some("model".into()),
            source: "fabric".into(),
            event_id: attempt.to_string(),
            host: "host".into(),
            repository: Some("repo".into()),
            run_id: Some("run".into()),
            task_id: Some("task".into()),
            attempt: Some(attempt.into()),
            status: Some(status.into()),
            error_class: if status == "failed" {
                Some("provider_error".into())
            } else {
                None
            },
            duration_ms: Some((attempt * 1000.0).into()),
            provider_ms: Some(1000.into()),
            input_tokens: tokens,
            cache_read_tokens: Some(10.into()),
            cache_write_tokens: Some(0.into()),
            output_tokens: Some(20.into()),
            reasoning_tokens: None,
            ..Usage::default()
        });
    }
    let prices = costs::Prices {
        models: BTreeMap::from([(
            "adapter/model".into(),
            costs::Price {
                billing: Some("subscription".into()),
                usd_per_million: BTreeMap::from([
                    ("input".into(), json!(2)),
                    ("cached_input".into(), json!(0.5)),
                    ("output".into(), json!(4)),
                ]),
                ..costs::Price::default()
            },
        )]),
        ..costs::Prices::default()
    };
    let input = json!({"rows":rows,"prices":prices.models});
    let source=format!("{LOAD}\nreport=load('telemetry_report')\nfor r in input['rows']:r['model_raw']=r['model']\nprint(json.dumps({{'groups':costs.aggregate(t,input['rows'],('adapter','model'),input['prices']), 'outcomes':report.outcomes(input['rows'],t,costs,input['prices'],{{}})}}))");
    let Some(expected) = python(&source, &input) else {
        return;
    };
    let actual = json!({"groups":costs::aggregate(&rows,&["adapter","model"],&prices).unwrap(),"outcomes":report::outcomes(&rows,&prices,false).unwrap()});
    equal(&actual, &expected);
}
#[test]
fn native_parser_and_hashes_match_python() {
    let rows = json!([
        {"type":"session_meta","payload":{"id":"session-é","cwd":"repo","creator_account_id":"fixture"}},
        {"type":"turn_context","payload":{"model":"observed","effort":"high"}},
        {"type":"event_msg","timestamp":"2026-01-01T00:00:00Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":8,"reasoning_output_tokens":3,"secret":991}}}},
        {"type":"event_msg","timestamp":"2026-01-01T00:01:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":120,"output_tokens":10}}}},
        {"type":"event_msg","timestamp":"2026-01-01T00:02:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":140,"output_tokens":12}}}}
    ]);
    let source=format!("{LOAD}\nimport hashlib\nparser=load('telemetry_ingest')\nusage=[]\nt.delivery_owner=lambda:types.SimpleNamespace(compact=lambda *args:None)\nt.identity=lambda *values:hashlib.sha256(json.dumps(values,sort_keys=True).encode()).hexdigest()\nt.put_usage=lambda db,source,event_id,**values:usage.append(dict(source=source,event_id=event_id,**{{k:v for k,v in values.items() if k not in ('origin_path','owner')}}))\nt.observation=lambda *args,**kwargs:None\nstate={{}}\nfor offset,row in enumerate(input):parser.codex_record(t,None,row,state,Path('fixture'),offset)\nprint(json.dumps(usage))");
    let Some(expected) = python(&source, &rows) else {
        return;
    };
    let mut sink = ingest::tests::Capture::default();
    let mut state = ingest::ParserState::default();
    for (offset, row) in rows.as_array().unwrap().iter().enumerate() {
        ingest::codex_record(
            &mut sink,
            row,
            &mut state,
            Path::new("fixture"),
            offset as u64,
            None,
        )
        .unwrap();
    }
    for (actual, expected) in sink.usage.iter().zip(expected.as_array().unwrap()) {
        let actual = serde_json::to_value(actual).unwrap();
        for (k, expected) in expected.as_object().unwrap() {
            equal(&actual[k], expected);
        }
    }
    assert_eq!(sink.usage.len(), expected.as_array().unwrap().len());
}
#[test]
fn sidecar_repairs_match_python_including_unicode_hashes() {
    let input = json!({"rows":[{"cycle":1,"failing_before":["é","a","a"],"at":"2026-01-01"},{"cycle":2,"failing_before":[],"at":"2026-01-02"}],"receipt":{"repair_cycles":2,"delivery_acceptance":{"basis":"user"}}});
    let source=format!("{LOAD}\nimport tempfile\nd=load('telemetry_delivery');events=[];updates=[]\nt.put_event=lambda db,kind,stamp,ref,**kw:events.append({{'kind':kind,'ts':stamp,'source_ref':ref,'attrs':kw['attrs']}})\nclass DB:\n def execute(self,q,args):updates.append([args[1],args[2],json.loads(args[0])])\nwith tempfile.TemporaryDirectory() as tmp:\n path=Path(tmp)/'repairs.jsonl';(path.parent/'RUN.json').write_text(json.dumps(input['receipt']));state={{}}\n for i,row in enumerate(input['rows']):d.record(t,DB(),row,state,path,i)\n for row in events:row['source_ref']=row['source_ref'].split('#')[-1]\n for row in updates:row[1]=row[1].split('#')[-1]\n print(json.dumps({{'events':events,'updates':updates}}))");
    let Some(expected) = python(&source, &input) else {
        return;
    };
    let mut sink = ingest::tests::Capture::default();
    let mut state = delivery::SidecarState::default();
    for (i, row) in input["rows"].as_array().unwrap().iter().enumerate() {
        delivery::record(
            &mut sink,
            "repairs.jsonl",
            row,
            &mut state,
            &input["receipt"],
            i.to_string(),
        )
        .unwrap();
    }
    let events: Vec<_> = sink
        .events
        .iter()
        .map(|e| json!({"kind":e.kind,"ts":e.ts,"source_ref":e.source_ref,"attrs":e.attrs}))
        .collect();
    equal(&json!({"events":events,"updates":sink.updates}), &expected);
}

#[test]
fn numeric_boundaries_and_python_float_hash_spelling() {
    let values = json!([
        9007199254740993u64,
        1e-5,
        1e-7,
        1e16,
        1e20,
        -0.0,
        "é\u{007f}😀"
    ]);
    let source="import sys,json,hashlib\nv=json.load(sys.stdin)\nprint(json.dumps(hashlib.sha256(json.dumps(v,sort_keys=True).encode()).hexdigest()))";
    if let Some(expected) = python(source, &values) {
        assert_eq!(
            crate::pending::identity(values.as_array().unwrap()),
            expected.as_str().unwrap()
        );
    }
    let huge = Numeric::from(9007199254740993u64);
    let row = Usage {
        input_tokens: Some(huge),
        cache_read_tokens: Some(0.into()),
        cache_write_tokens: Some(0.into()),
        ..Usage::default()
    };
    let group = costs::unit(&row, None, BTreeMap::new(), &costs::Prices::default()).unwrap();
    let merged = costs::combine([group.clone(), group], &[]).unwrap();
    assert_eq!(
        serde_json::to_value(&merged[0]).unwrap()["input_tokens"],
        json!(18014398509481986u64)
    );
    let roundtrip: Usage = serde_json::from_value(serde_json::to_value(row).unwrap()).unwrap();
    assert_eq!(roundtrip.input_tokens, Some(huge));
    let lower = Usage {
        attempt: Some(Numeric::Float(9007199254740992.0)),
        status: Some("failed".into()),
        ..Usage::default()
    };
    let higher = Usage {
        attempt: Some(9007199254740993u64.into()),
        status: Some("ok".into()),
        ..Usage::default()
    };
    assert_eq!(
        report::outcomes(&[lower, higher], &costs::Prices::default(), true).unwrap()[0].succeeded,
        1
    );
}
#[test]
fn peer_integral_floats_remain_accepted() {
    let group = costs::unit(
        &Usage::default(),
        None,
        BTreeMap::from([("host".into(), json!("local"))]),
        &costs::Prices::default(),
    )
    .unwrap();
    let mut group = serde_json::to_value(group).unwrap();
    for field in [
        "rows",
        "missing_token_rows",
        "cache_share_rows",
        "missing_cost_rows",
    ] {
        group[field] = json!(group[field].as_u64().unwrap() as f64);
    }
    for field in costs::TOKEN_FIELDS {
        group["missing_token_fields"][field] = json!(1.0);
    }
    let summary = json!({"schema":crate::hosts::SUMMARY_SCHEMA,"host":"local","session_usage":[group],"excluded_rows":{},"receipt_usage_rows_excluded":0,"pending_source_rebuilds":[]});
    assert!(crate::hosts::validate(&summary, "local").is_ok());
    let mut large = summary.clone();
    large["session_usage"][0]["rows"] = json!(1e20);
    assert!(crate::hosts::validate(&large, "local").is_ok());
}
