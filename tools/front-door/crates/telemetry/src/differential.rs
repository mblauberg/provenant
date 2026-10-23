//! Independent Python oracle on isolated fixtures. No installed command or live ledger is used.
use crate::{costs, delivery, ingest, report, Numeric, Usage};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
fn oracle_available(availability: std::result::Result<(), String>, required: bool) -> bool {
    match availability {
        Ok(()) => true,
        Err(reason) => {
            assert!(!required, "required differential interpreter unavailable: {reason}");
            eprintln!("SKIP: {reason}");
            false
        }
    }
}
fn python_version(program: &std::ffi::OsStr) -> std::result::Result<(), String> {
    let out = Command::new(program).args(["-c", "import sys; print(sys.version_info.major, sys.version_info.minor)"]).output().map_err(|e| format!("Python 3.11+ interpreter unavailable: {e}"))?;
    let version = String::from_utf8_lossy(&out.stdout);
    let parts: Vec<_> = version.split_whitespace().filter_map(|s| s.parse::<u32>().ok()).collect();
    if out.status.success() && parts.len() == 2 && (parts[0],parts[1]) >= (3,11) {
        Ok(())
    } else {
        Err(format!("Python 3.11+ required; interpreter reported {version:?}"))
    }
}
fn python(source: &str, input: &Value) -> Option<Value> {
    let program = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let required = std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() == Ok("1");
    if !oracle_available(python_version(&program), required) { return None; }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let mut child = Command::new(program).arg("-c").arg(source).arg(root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("checked Python oracle could not start");
    child.stdin.take().unwrap().write_all(serde_json::to_string(input).unwrap().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    Some(crate::decode_json(&out.stdout).unwrap())
}
#[test]
fn required_oracles_refuse_missing_or_old_interpreters() {
    let missing = std::ffi::OsStr::new("/no-such-provenant-python");
    assert!(python_version(missing).is_err());
    assert!(!oracle_available(python_version(missing), false));
    assert!(std::panic::catch_unwind(|| oracle_available(python_version(missing), true)).is_err());
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("python39");
        std::fs::write(&program, "#!/bin/sh\nprintf '3 9\\n'\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(python_version(program.as_os_str()).is_err());
        assert!(std::panic::catch_unwind(|| oracle_available(python_version(program.as_os_str()), true)).is_err());
    }
}
fn equal(a: &Value, b: &Value) {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => assert_eq!(a.to_string(), b.to_string()),
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
t=load('telemetry')
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
            model_raw: Some("Original Model Alias".into()),
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
    let source=format!("{LOAD}\nreport=load('telemetry_report')\nprint(json.dumps({{'groups':costs.aggregate(t,input['rows'],('adapter','model'),input['prices']), 'outcomes':report.outcomes(input['rows'],t,costs,input['prices'],{{}})}}))");
    let Some(expected) = python(&source, &input) else {
        return;
    };
    let actual = json!({"groups":costs::aggregate(&rows,&["adapter","model"],&prices).unwrap(),"outcomes":report::outcomes(&rows,&prices,false).unwrap()});
    equal(&actual, &expected);
}
fn retained_event(event: &crate::Event) -> Value {
    let mut value = serde_json::to_value(event).unwrap();
    value["host"] = json!("fixture-host");
    let mut parts = vec![json!("event"), json!(event.kind)];
    if event.kind == "session.compaction" {
        if event.session.as_deref().is_some_and(|s| !s.is_empty()) && event.ts.as_deref().is_some_and(|s| !s.is_empty()) {
            parts.extend([json!(event.session),json!(event.ts)]);
        } else { parts.push(json!(event.source_ref)); }
    } else {
        parts.extend([json!(event.source_ref),json!(event.ts)]);
        for field in ["session","run_id","lane","task_id","pr","provider","account","model","harness_rev"] {
            parts.push(value[field].clone());
        }
        parts.push(event.attrs.clone());
    }
    value["event_id"] = json!(crate::pending::identity(&parts));
    value
}
#[test]
fn native_parser_and_hashes_match_python() {
    let mut cases = vec![
        json!({"parser":"codex","row":{"type":"session_meta","payload":{"id":"session-é","cwd":"repo","creator_account_id":"fixture"}}}),
        json!({"parser":"codex","row":{"type":"turn_context","payload":{"model":"observed","effort":"high"}}}),
        json!({"parser":"codex","row":{"type":"event_msg","timestamp":"2026-01-01T00:00:00Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":8,"reasoning_output_tokens":3,"secret":991}},"rate_limits":{"primary":{"used_percent":0,"window_minutes":300,"resets_at":1735689600791625390i64 as f64 / 1e9},"secondary":{"used_percent":0.0,"window_minutes":300.0,"resets_at":"later"}}}}}),
        json!({"parser":"codex","row":{"type":"event_msg","timestamp":"2026-01-01T00:01:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":120,"output_tokens":10}}}}}),
        json!({"parser":"codex","row":{"type":"event_msg","timestamp":"2026-01-01T00:02:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":140,"output_tokens":12}}}}}),
        json!({"parser":"codex","row":{"type":"compacted","timestamp":"2026-01-01T00:03:00Z","payload":{"secret":"unread"}}}),
        json!({"parser":"claude","row":{"type":"assistant","sessionId":"claude-é","account":"fixture","message":{"id":"message","model":"observed","content":"secret","usage":{"input_tokens":100,"cache_read_input_tokens":2,"cache_creation_input_tokens":0,"output_tokens":12,"output_tokens_details":{"thinking_tokens":5}}}}}),
        json!({"parser":"claude","row":{"type":"assistant","message":{"id":"clamped","usage":{"output_tokens":1,"output_tokens_details":{"thinking_tokens":5}}}}}),
        json!({"parser":"claude","row":{"type":"system","subtype":"compact_boundary","timestamp":"2026-01-01T00:04:00Z","sessionId":"claude-é","compactMetadata":{"trigger":"auto","preTokens":1000,"postTokens":100,"secret":"drop"}}}),
    ];
    let context = json!({"attributes":[{"key":"provenant.run_id","value":{"stringValue":"run"}},{"key":"provenant.lane","value":{"stringValue":"lane"}},{"key":"provenant.harness_rev","value":{"stringValue":"rev"}}]});
    let api = json!({"timeUnixNano":"1735689600791625390","attributes":[{"key":"event.name","value":{"stringValue":"api_request"}},{"key":"session.id","value":{"stringValue":"otel-session"}},{"key":"input_tokens","value":{"intValue":"9007199254740993"}},{"key":"duration_ms","value":{"doubleValue":12.0}},{"key":"success","value":{"boolValue":true}},{"key":"prompt","value":{"stringValue":"secret"}}]});
    cases.push(json!({"parser":"otel","row":{"signal":"logs","body":{"resourceLogs":[{"resource":context,"scopeLogs":[{"logRecords":[{"attributes":[]},api.clone(),api,{"timeUnixNano":"bad","attributes":[{"key":"event.name","value":{"stringValue":"user_prompt"}},{"key":"prompt_length","value":{"intValue":"23"}},{"key":"prompt","value":{"stringValue":"secret"}}]}]}]}]},"received_at":"2026-01-01T00:00:00Z"}}));
    cases.push(json!({"parser":"otel","row":{"signal":"metrics","body":{"resourceMetrics":[{"resource":context,"scopeMetrics":[{"metrics":[{"name":"claude_code.active_time.total","sum":{"aggregationTemporality":2,"dataPoints":[{"asInt":"3","timeUnixNano":"1735689600791625390"}]}},{"name":"gauge","gauge":{"dataPoints":[{"asDouble":3.5}]}}]}]}]},"received_at":"fallback"}}));
    let input = json!({"cases":cases,"usage_template":Usage::default()});
    let source = format!("{LOAD}\n{NATIVE_ORACLE}");
    let Some(expected) = python(&source, &input) else { return; };
    let mut sink = ingest::tests::Capture::default();
    let mut state = ingest::ParserState::default();
    for (offset, case) in cases.iter().enumerate() {
        let row = &case["row"];
        match case["parser"].as_str().unwrap() {
            "codex" => ingest::codex_record(&mut sink,row,&mut state,Path::new("fixture"),offset as u64,None).unwrap(),
            "claude" => ingest::claude_record(&mut sink,row,Path::new("fixture"),offset as u64,None).unwrap(),
            "otel" => ingest::otel_record(&mut sink,row,Path::new("fixture"),offset as u64).unwrap(),
            _ => unreachable!(),
        }
    }
    let observations: Vec<_> = sink.observations.iter().map(|row| {
        let mut value = serde_json::to_value(row).unwrap();
        value["host"] = json!("fixture-host");
        value["event_id"] = json!(crate::pending::identity(&[value.clone()]));
        value
    }).collect();
    let events: Vec<_> = sink.events.iter().map(retained_event).collect();
    equal(&json!({"usage":sink.usage,"observations":observations,"events":events}), &expected);
}
const NATIVE_ORACLE: &str = r#"
parser=load('telemetry_ingest'); delivery=load('telemetry_delivery')
t.host_id=lambda:'fixture-host'
t.delivery_owner=lambda:delivery
captured={'usage':[],'observations':[],'events':[]}
def usage(db, source, event_id, **values):
 values={k:v for k,v in values.items() if k not in ('origin_path','owner')}
 captured['usage'].append(dict(input['usage_template'],source=source,event_id=event_id,**values))
t.put_usage=usage
class DB:
 def execute(self,query,args):
  table=query.split('INTO ',1)[1].split('(',1)[0]
  columns=query.split('(',1)[1].split(')',1)[0].split(',')
  value=dict(zip(columns,args))
  if table=='events':
   value['attrs']=json.loads(value['attrs']);value['refresh']='DO UPDATE' in query
  captured[table].append(value)
db=DB();state={}
for offset,case in enumerate(input['cases']):
 getattr(parser,case['parser']+'_record')(t,db,case['row'],state,Path('fixture'),offset)
print(json.dumps(captured))
"#;
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
    equal(&serde_json::to_value(roundtrip.input_tokens).unwrap(), &json!(9007199254740993u64));
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
