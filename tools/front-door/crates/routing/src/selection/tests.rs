use super::*;
fn context<'a>(
    policies: &'a BTreeMap<String, BTreeMap<String, String>>,
    aliases: &'a BTreeMap<String, usize>,
    efforts: &'a BTreeMap<String, usize>,
) -> SelectionContext<'a> {
    SelectionContext {
        task_class: "work",
        role: "worker",
        policy_table: policies,
        aliases,
        efforts,
    }
}
type PolicyTables = (
    BTreeMap<String, BTreeMap<String, String>>,
    BTreeMap<String, usize>,
    BTreeMap<String, usize>,
);
fn tables() -> PolicyTables {
    (
        BTreeMap::from([(
            "work".into(),
            BTreeMap::from([
                ("role".into(), "worker".into()),
                ("minimum_alias".into(), "senior".into()),
                ("minimum_effort".into(), "high".into()),
            ]),
        )]),
        BTreeMap::from([
            ("junior".into(), 0),
            ("senior".into(), 1),
            ("principal".into(), 2),
        ]),
        BTreeMap::from([("low".into(), 0), ("high".into(), 1), ("ultra".into(), 2)]),
    )
}
fn candidate(id: &str, family: &str, adapter: &str, status: &str) -> Value {
    json!({"candidate_id":id,"route":{"schema_version":1,"status":status,"adapter":adapter,"model_family":family,"resolved_model":format!("{family}-model"),"task_class":"work","route_source":"task-class","role":"worker","alias":"senior","effort":"high","requested_effort":"high"}})
}
#[test]
fn spreading_state_checks_exact_numeric_equality_and_preserves_counter_kind() {
    for (raw, status) in [
        (
            r#"{"schema_version":true,"assignments":{"f":1},"selection_count":1.0}"#,
            StateStatus::Loaded,
        ),
        (
            r#"{"schema_version":1,"assignments":{"f":1},"selection_count":true}"#,
            StateStatus::Loaded,
        ),
        (
            r#"{"schema_version":1,"assignments":{"f":true},"selection_count":1}"#,
            StateStatus::Invalid,
        ),
        (
            r#"{"schema_version":1,"assignments":{"f":1.0},"selection_count":1}"#,
            StateStatus::Invalid,
        ),
        (
            r#"{"schema_version":1,"assignments":{"f":9007199254740993},"selection_count":9007199254740992.0}"#,
            StateStatus::Invalid,
        ),
        (
            r#"{"schema_version":1,"assignments":{"":0},"selection_count":0}"#,
            StateStatus::Invalid,
        ),
        ("null", StateStatus::Invalid),
        ("{bad", StateStatus::Invalid),
    ] {
        assert_eq!(load_spread_state(Some(raw)).unwrap().1, status, "{raw}");
    }
    assert!(matches!(
        load_spread_state(Some(r#"{"ignored":18446744073709551616}"#)),
        Err(SelectionError::Json(
            provenant_records::DecodeError::NumberOutOfRange(_)
        ))
    ));
}
#[test]
fn selection_receipts_and_state_match_python_failures_and_soft_preferences() {
    let (policies, aliases, efforts) = tables();
    let ctx = context(&policies, &aliases, &efforts);
    let candidates = json!({"schema_version":1,"candidates":[candidate("a","anthropic","claude","ok"),candidate("o","openai","codex","ok"),candidate("g","google","agy","adapter_disabled")]});
    let prefs = vec![
        json!({}),
        json!({"task_classes":{"work":{"family_affinity":["openai","unknown"]}}}),
        json!({"roles":{"worker":{"adapter_affinity":["agy"],"model_affinity":["missing"]}}}),
        json!({"deprioritise":{"families":["anthropic"]}}),
        json!({"avoid":{"models":["openai-model"],"adapters":["codex"]},"spreading":{"policy":{"z":true,"a":null},"extra":1},"unknown":3}),
    ];
    let states = [
        None,
        Some(
            r#"{"schema_version":1,"assignments":{"anthropic":0,"openai":1},"selection_count":1.0}"#,
        ),
        Some(
            r#"{"schema_version":1,"assignments":{"anthropic":1,"openai":1},"selection_count":2}"#,
        ),
        Some("{bad"),
    ];
    let mut fixtures = vec![];
    for p in prefs {
        for s in states {
            fixtures.push(
                json!({"candidates":candidates.to_string(),"preferences":p.to_string(),"state":s}),
            );
        }
    }
    fixtures.extend([
        json!({"candidates":"null","preferences":"{}","state":null}),
        json!({"candidates":json!({"schema_version":1,"candidates":[candidate("g","google","agy","adapter_disabled")]}).to_string(),"preferences":"{}","state":null}),
        json!({"candidates":candidates.to_string(),"preferences":"{bad","state":null}),
        json!({"candidates":candidates.to_string(),"preferences":"[]","state":null}),
    ]);
    let Some(expected): Option<Value> = crate::oracle::python(
        r#"
import importlib.util,pathlib,json,sys,tempfile,argparse,contextlib,io
s=importlib.util.spec_from_file_location('prefs',pathlib.Path('scripts/model_route_preferences.py'));m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
d=json.load(sys.stdin);out=[]
for fixture in d['fixtures']:
 with tempfile.TemporaryDirectory() as tmp:
  p=pathlib.Path(tmp);c=p/'c.json';f=p/'p.json';st=p/'state.json'
  c.write_text(fixture['candidates']);f.write_text(fixture['preferences'])
  if fixture['state'] is not None:st.write_text(fixture['state'])
  args=argparse.Namespace(task_class='work',role='worker',candidates_file=str(c),preferences_file=str(f),spread_state_file=str(st))
  stream=io.StringIO()
  with contextlib.redirect_stdout(stream):code=m.select(args,d['policies'],d['aliases'],d['efforts'])
  receipt=json.loads(stream.getvalue());state=st.read_text() if receipt['status']=='ok' else None
  out.append([code,receipt,state])
print(json.dumps(out))
"#,
        &json!({"fixtures":fixtures,"policies":policies,"aliases":aliases,"efforts":efforts}),
    ) else {
        return;
    };
    let actual: Vec<_> = fixtures
        .iter()
        .map(|f| {
            let raw: OrderedValue =
                crate::pending::from_json(f["candidates"].as_str().unwrap()).unwrap();
            let (pref, status) =
                match crate::pending::from_json::<OrderedValue>(f["preferences"].as_str().unwrap())
                {
                    Ok(v @ OrderedValue::Object(_)) => (v, PreferenceStatus::Loaded),
                    Ok(_) => (OrderedValue::Null, PreferenceStatus::Invalid),
                    Err(_) => (OrderedValue::Null, PreferenceStatus::Corrupt),
                };
            let (state, state_status) = load_spread_state(f["state"].as_str()).unwrap();
            let output = select_documents(&ctx, &raw, &pref, status, state, state_status).unwrap();
            let encoded = output.record.spreading.as_ref().map(|s| {
                format!(
                    "{}\n",
                    crate::pending::json_dumps(
                        &serde_json::to_value(&s.state_after).unwrap().into(),
                        true,
                        Some(2)
                    )
                )
            });
            json!([output.code, output.record, encoded])
        })
        .collect();
    assert_eq!(json!(actual), expected);
}
#[test]
fn fair_spreading_is_order_independent_and_affinity_cannot_lower_policy() {
    let (policies, aliases, efforts) = tables();
    let ctx = context(&policies, &aliases, &efforts);
    let preferences: OrderedValue =
        json!({"task_classes":{"work":{"family_affinity":["openai"]}}}).into();
    let mut raw = vec![
        candidate("a", "anthropic", "claude", "ok"),
        candidate("o", "openai", "codex", "ok"),
    ];
    let mut state = SpreadState::default();
    let mut picks = vec![];
    for _ in 0..8 {
        raw.reverse();
        let result = select_documents(
            &ctx,
            &json!({"schema_version":1,"candidates":raw}).into(),
            &preferences,
            PreferenceStatus::Loaded,
            Some(state),
            StateStatus::Loaded,
        )
        .unwrap();
        picks.push(result.record.chosen_candidate_id.unwrap());
        state = result.record.spreading.unwrap().state_after;
    }
    assert_eq!(picks, ["o", "a", "o", "a", "o", "a", "o", "a"]);
    assert_eq!(state.selection_count, 8);
    assert_eq!(
        state.assignments,
        BTreeMap::from([("anthropic".into(), 4), ("openai".into(), 4)])
    );
}

struct MemoryStore(std::sync::Mutex<BTreeMap<PathBuf, String>>);
struct Transaction<'a>(std::sync::MutexGuard<'a, BTreeMap<PathBuf, String>>);
impl SelectionStore for MemoryStore {
    fn begin(&self, _: &Path) -> Result<Box<dyn SelectionTransaction + '_>, SelectionError> {
        Ok(Box::new(Transaction(self.0.lock().unwrap())))
    }
}
impl SelectionTransaction for Transaction<'_> {
    fn read(&mut self, path: &Path) -> Result<Option<String>, SelectionError> {
        Ok(self.0.get(path).cloned())
    }
    fn write_atomic(&mut self, path: &Path, bytes: &[u8]) -> Result<(), SelectionError> {
        self.0
            .insert(path.into(), String::from_utf8(bytes.to_vec()).unwrap());
        Ok(())
    }
}
#[test]
fn concurrent_selection_keeps_one_locked_transaction_until_publication() {
    let raw=json!({"schema_version":1,"candidates":[candidate("a","anthropic","claude","ok"),candidate("o","openai","codex","ok")]}).to_string();
    let store = MemoryStore(std::sync::Mutex::new(BTreeMap::from([
        (PathBuf::from("candidates"), raw),
        (PathBuf::from("preferences"), "{}".into()),
    ])));
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..16)
            .map(|_| {
                scope.spawn(|| {
                    let (policies, aliases, efforts) = tables();
                    select(
                        &store,
                        &SelectionPaths {
                            candidates: Path::new("candidates"),
                            preferences: Path::new("preferences"),
                            state: Path::new("state"),
                        },
                        &context(&policies, &aliases, &efforts),
                    )
                    .unwrap()
                    .code
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), 0);
        }
    });
    let state = load_spread_state(Some(&store.0.lock().unwrap()[Path::new("state")]))
        .unwrap()
        .0
        .unwrap();
    assert_eq!(state.selection_count, 16);
    assert_eq!(
        state.assignments,
        BTreeMap::from([("anthropic".into(), 8), ("openai".into(), 8)])
    );
}

#[test]
fn publication_failure_cannot_produce_a_success_receipt() {
    struct FailingStore;
    struct FailingTransaction;
    impl SelectionStore for FailingStore {
        fn begin(&self, _: &Path) -> Result<Box<dyn SelectionTransaction + '_>, SelectionError> {
            Ok(Box::new(FailingTransaction))
        }
    }
    impl SelectionTransaction for FailingTransaction {
        fn read(&mut self, path: &Path) -> Result<Option<String>, SelectionError> {
            Ok(match path.to_str().unwrap(){"c"=>Some(json!({"schema_version":1,"candidates":[candidate("a","anthropic","claude","ok")]}).to_string()),"p"=>Some("{}".into()),_=>None})
        }
        fn write_atomic(&mut self, path: &Path, _: &[u8]) -> Result<(), SelectionError> {
            Err(SelectionError::Io {
                operation: "publish",
                path: path.into(),
                source: std::io::Error::from_raw_os_error(28),
            })
        }
    }
    let (policies, aliases, efforts) = tables();
    assert!(matches!(
        select(
            &FailingStore,
            &SelectionPaths {
                candidates: Path::new("c"),
                preferences: Path::new("p"),
                state: Path::new("s")
            },
            &context(&policies, &aliases, &efforts)
        ),
        Err(SelectionError::Io {
            operation: "publish",
            ..
        })
    ));
}

#[test]
fn extended_json_owner_gap_keeps_selection_partial() {
    let fixtures = [
        r#"{"schema_version":1,"assignments":{},"selection_count":0,"extra":NaN}"#,
        r#"{"schema_version":1,"assignments":{},"selection_count":0,"extra":"\ud800"}"#,
    ];
    let Some(expected) = crate::oracle::python::<Value>(r#"
import importlib.util,json,pathlib,sys,tempfile
spec=importlib.util.spec_from_file_location('prefs',pathlib.Path.cwd()/'scripts/model_route_preferences.py')
m=importlib.util.module_from_spec(spec);sys.modules[spec.name]=m;spec.loader.exec_module(m)
result=[]
for raw in json.load(sys.stdin):
 with tempfile.TemporaryDirectory() as root:
  path=pathlib.Path(root)/'state.json';path.write_text(raw)
  state,status=m._load_spread_state(path)
  result.append(status)
print(json.dumps(result))
"#, &json!(fixtures)) else { return; };
    assert_eq!(expected, json!(["loaded", "loaded"]));
    for raw in fixtures {
        // Explicit counterexamples: records lacks Python extended JSON values.
        assert_eq!(load_spread_state(Some(raw)).unwrap().1, StateStatus::Invalid);
    }
}
