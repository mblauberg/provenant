use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{json, Value};

use super::{
    candidates, confidential_alternatives, disclosure_risk, model_families, registered_model,
    route_command, snapshot, ExecutionCooldownStore, ExecutionRoutePlan, ExecutionRouting,
    ExecutionRoutingError, ProviderQuota, QuotaUnavailable, RouterHost, RouterHostError,
};

struct Host {
    product: PathBuf,
    python: OsString,
    executable: bool,
    output: Vec<u8>,
    catalogue: Value,
    host_id: Option<String>,
    calls: RefCell<Vec<(Vec<OsString>, Duration)>>,
    warnings: RefCell<Vec<String>>,
}

impl Host {
    fn new(catalogue: Value) -> Self {
        let output = serde_json::to_vec(&json!({
            "schema": "fabric.catalogue.v1",
            "adapters": catalogue.get("adapters").cloned().unwrap_or_else(|| json!({})),
            "models": catalogue.get("models").cloned().unwrap_or_else(|| json!({})),
            "catalogue": catalogue.clone(),
        }))
        .unwrap();
        Self {
            product: PathBuf::from("/fixture/product"),
            python: "/python".into(),
            executable: false,
            output,
            catalogue: json!({}),
            host_id: Some("studio".into()),
            calls: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
        }
        .with_catalogue(catalogue)
    }

    fn with_catalogue(mut self, catalogue: Value) -> Self {
        self.catalogue = catalogue;
        self
    }
}

impl RouterHost for Host {
    fn product_root(&self) -> PathBuf {
        self.product.clone()
    }
    fn python_executable(&self) -> OsString {
        self.python.clone()
    }
    fn is_executable(&self, _path: &Path) -> bool {
        self.executable
    }
    fn run_bounded(
        &self,
        argv: &[OsString],
        timeout: Duration,
    ) -> Result<Vec<u8>, RouterHostError> {
        self.calls.borrow_mut().push((argv.to_vec(), timeout));
        Ok(self.output.clone())
    }
    fn load_catalog(&self, _product: &Path) -> Result<Value, RouterHostError> {
        Ok(self.catalogue.clone())
    }
    fn host_id(&self) -> String {
        self.host_id
            .clone()
            .unwrap_or_else(|| "unknown-host".into())
    }
    fn warning(&self, message: &str) {
        self.warnings.borrow_mut().push(message.to_owned());
    }
}

#[derive(Default)]
struct Store {
    rows: provenant_records::fabric_records::CooldownRows,
    scopes: RefCell<Option<BTreeMap<String, provenant_records::fabric_records::Scope>>>,
}
impl ExecutionCooldownStore for Store {
    fn read_cooldowns(
        &self,
        scopes: Option<&BTreeMap<String, provenant_records::fabric_records::Scope>>,
    ) -> Result<
        provenant_records::fabric_records::CooldownRows,
        provenant_records::fabric_records::RecordError,
    > {
        *self.scopes.borrow_mut() = scopes.cloned();
        Ok(self.rows.clone())
    }
}

struct Quota {
    needs_reselection: Result<bool, QuotaUnavailable>,
    selected: Result<Option<String>, QuotaUnavailable>,
    pinned_state: Result<Option<String>, QuotaUnavailable>,
    cooldowns: Result<provenant_records::fabric_records::CooldownRows, QuotaUnavailable>,
    valid_label: bool,
}
impl ProviderQuota for Quota {
    fn claude_account_needs_reselection(
        &self,
        _: &str,
        _: jiff::Timestamp,
    ) -> Result<bool, QuotaUnavailable> {
        self.needs_reselection
    }
    fn reselect_claude_account(
        &self,
        _: jiff::Timestamp,
    ) -> Result<Option<String>, QuotaUnavailable> {
        self.selected.clone()
    }
    fn label_is_valid(&self, _: &str) -> bool {
        self.valid_label
    }
    fn pinned_account_state(&self, _: &str, _: &str) -> Result<Option<String>, QuotaUnavailable> {
        self.pinned_state.clone()
    }
    fn apply_to_cooldowns(
        &self,
        _: &provenant_records::fabric_records::CooldownRows,
        _: Option<(&str, &str)>,
        _: bool,
        _: jiff::Timestamp,
    ) -> Result<provenant_records::fabric_records::CooldownRows, QuotaUnavailable> {
        self.cooldowns.clone()
    }
}

fn timestamp() -> jiff::Timestamp {
    jiff::Timestamp::from_second(1_797_120_000).unwrap()
}
fn catalogue() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../config/model-routing.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn runtime<'a>(host: &'a Host, store: &'a Store) -> ExecutionRouting<'a> {
    ExecutionRouting {
        host,
        store,
        quota: None,
        now: timestamp(),
    }
}
fn row(until: &str, source: Option<&str>) -> BTreeMap<String, Value> {
    let mut row = BTreeMap::from([("cooling_until".into(), json!(until))]);
    if let Some(source) = source {
        row.insert("source".into(), json!(source));
    }
    row
}
fn plan(text: &str) -> ExecutionRoutePlan {
    crate::pending::from_json(text).unwrap()
}

#[test]
fn route_command_and_snapshot_use_the_host_boundary_and_bounded_timeout() {
    let catalog = catalogue();
    let mut host = Host::new(catalog.clone());
    let fallback = route_command(&host, &host.product);
    assert_eq!(
        fallback,
        vec![
            OsString::from("/python"),
            OsString::from("/fixture/product/scripts/model_route.py")
        ]
    );
    host.executable = true;
    assert_eq!(
        route_command(&host, &host.product),
        vec![OsString::from("/fixture/product/scripts/model-route")]
    );
    assert_eq!(snapshot(&host)["schema"], "fabric.catalogue.v1");
    let calls = host.calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].0[1..],
        [OsString::from("snapshot"), OsString::from("--json")]
    );
    assert_eq!(calls[0].1, Duration::from_secs(15));
}

#[cfg(unix)]
#[test]
fn route_command_matches_python_wrapper_and_interpreter_selection() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let product = temp.path().canonicalize().unwrap();
    let scripts = product.join("scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    let wrapper = scripts.join("model-route");
    std::fs::write(&wrapper, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let input = json!({ "product": product.to_string_lossy() });
    let Some(expected): Option<(Vec<String>, Vec<String>)> = crate::oracle::python(
        "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\np=pathlib.Path(json.load(sys.stdin)['product']); print(json.dumps((m.route_command(p), m.route_command(p/'missing'))))",
        &input,
    ) else {
        return;
    };
    let host = Host {
        product: product.clone(),
        python: "/python".into(),
        executable: true,
        output: vec![],
        catalogue: json!({}),
        host_id: Some("studio".into()),
        calls: RefCell::new(Vec::new()),
        warnings: RefCell::new(Vec::new()),
    };
    let wrapper_command = route_command(&host, &product)
        .into_iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(wrapper_command, expected.0);
    let missing_product = product.join("missing");
    let mut no_wrapper = Host {
        executable: false,
        ..host
    };
    let python = expected.1.first().cloned().unwrap_or_default();
    no_wrapper.python = python.into();
    let fallback = route_command(&no_wrapper, &missing_product)
        .into_iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(fallback, expected.1);
}

#[test]
fn malformed_snapshot_falls_back_to_empty_object() {
    let mut host = Host::new(catalogue());
    host.output = br#"{"schema":"other"}"#.to_vec();
    assert_eq!(snapshot(&host), json!({}));
}

#[test]
fn model_family_lookup_loads_catalog_without_a_provider_snapshot() {
    let host = Host::new(catalogue());
    assert_eq!(
        model_families(&host, "claude-sonnet-5-5", None),
        vec!["anthropic"]
    );
    assert!(host.calls.borrow().is_empty());
}

#[test]
fn registered_model_matches_python_identity_and_effort_stripping() {
    let input = json!({
        "models": [
            ["codex", "gpt-6.1-sol@high"],
            ["codex", "unknown-model@low"],
            ["unknown", "gpt-6.1-sol@high"],
            ["codex", "*"]
        ]
    });
    let Some(expected): Option<Vec<String>> = crate::oracle::python(
        "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\nd=json.load(sys.stdin); c=json.load(open('config/model-routing.json')); print(json.dumps([m.registered_model(a,x,c) for a,x in d['models']]))",
        &input,
    ) else {
        return;
    };
    let catalog = catalogue();
    let host = Host::new(catalog.clone());
    let actual = input["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            registered_model(
                &host,
                pair[0].as_str().unwrap(),
                pair[1].as_str().unwrap(),
                Some(&catalog),
                None,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn registered_model_warning_classes_match_python_for_malformed_catalogues() {
    let catalogues = json!([
        [],
        {"adapters": null},
        {"adapters": {"codex": null}},
        {"adapters": {"codex": {"models": null}}},
        {"adapters": {"codex": {"models": [{}]}}},
        {"adapters": {"codex": {"models": [{"id": 7}]}}},
        {"adapters": {"codex": {"models": [{"id": "model", "names": null}]}}},
        {"adapters": {"codex": {"models": [{"id": "model", "names": null}, {}]}}},
        {"adapters": {"codex": {"models": [{"id": "model", "names": [7]}]}}},
        {"adapters": {"codex": {"models": [{"id": "model", "effort_transport": "model-suffix", "suffix": null}]}}},
        {"adapters": {"codex": {"models": [{"id": "model", "effort_transport": "model-suffix", "suffix": {"high": 7}}]}}},
        {"adapters": {"codex": {"models": {}}}},
        {"adapters": {"codex": {"models": ""}}}
    ]);
    let Some(expected): Option<Vec<Value>> = crate::oracle::python(
        "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\ncatalogues=json.load(sys.stdin); out=[]\nfor catalogue in catalogues:\n warnings=[]; result=m.registered_model('codex','wanted',catalogue,warnings); out.append({'model':result,'warnings':warnings})\nprint(json.dumps(out))",
        &catalogues,
    ) else {
        return;
    };
    let host = Host::new(json!({}));
    let actual = catalogues
        .as_array()
        .unwrap()
        .iter()
        .map(|catalogue| {
            let mut warnings = Vec::new();
            let model = registered_model(
                &host,
                "codex",
                "wanted",
                Some(catalogue),
                Some(&mut warnings),
            );
            json!({"model":model,"warnings":warnings})
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn execution_candidates_preserve_source_order_explicit_effort_and_policy_filtering() {
    let host = Host::new(catalogue());
    let store = Store::default();
    let runtime = runtime(&host, &store);
    let plan = plan(
        r#"{"adapter":"codex","model":"gpt-6.1-sol","effort":"high","route":{"fallback_candidates":["codex/gpt-6-luna@low",{"adapter":"claude","model":"sonnet","effort_applied":false},"codex/gpt-6-luna"]}}"#,
    );
    let policy = crate::ordered_json::OrderedValue::String("any".into());
    let candidates = candidates(
        &runtime,
        &plan,
        Some(&policy),
        Some(&json!({"adapters":{},"models":{}})),
        false,
    )
    .unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].adapter, "codex");
    assert_eq!(candidates[0].model, "gpt-6-luna");
    assert_eq!(
        candidates[0].effort,
        crate::ordered_json::OrderedValue::String("low".into())
    );
    assert_eq!(
        candidates[1].effort,
        crate::ordered_json::OrderedValue::Bool(false)
    );
}

#[test]
fn candidate_adapter_lists_keep_python_dict_comprehension_order_and_last_value() {
    let input = json!({
        "plan": {
            "adapter": "codex", "model": "primary",
            "route": {"alias": "senior"}
        },
        "catalogue": {
            "adapters": [
                "codex",
                {"name": "codex", "aliases": {"senior": ["codex/first"]}},
                {"name": "codex", "aliases": {"senior": ["codex/last"]}}
            ],
            "models": {}
        }
    });
    let Some(expected): Option<Vec<Value>> = crate::oracle::python(
        "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\nd=json.load(sys.stdin); print(json.dumps(m.candidates(d['plan'], True, d['catalogue'])))",
        &input,
    ) else {
        return;
    };
    let host = Host::new(json!({}));
    let store = Store::default();
    let runtime = runtime(&host, &store);
    let plan = plan(&serde_json::to_string(&input["plan"]).unwrap());
    let actual = candidates(
        &runtime,
        &plan,
        Some(&crate::ordered_json::OrderedValue::Bool(true)),
        Some(&input["catalogue"]),
        false,
    )
    .unwrap();
    assert_eq!(serde_json::to_value(actual).unwrap(), json!(expected));
}

#[test]
fn adapter_policy_adds_opencode_default_only_for_other_adapters() {
    let input = json!({
        "plans": [
            {"adapter":"codex", "model":"primary", "route":{}},
            {"adapter":"opencode", "model":"primary", "route":{}}
        ],
        "catalogue": {
            "adapters": {"opencode":{"default_model":"plain-model"}},
            "models": {}
        }
    });
    let Some(expected): Option<Vec<Value>> = crate::oracle::python(
        "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\nd=json.load(sys.stdin); print(json.dumps([m.candidates(plan,True,d['catalogue']) for plan in d['plans']]))",
        &input,
    ) else {
        return;
    };
    let host = Host::new(json!({}));
    let store = Store::default();
    let runtime = runtime(&host, &store);
    let policy = crate::ordered_json::OrderedValue::Bool(true);
    let actual = input["plans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| {
            let plan = plan(&serde_json::to_string(value).unwrap());
            serde_json::to_value(
                candidates(
                    &runtime,
                    &plan,
                    Some(&policy),
                    Some(&input["catalogue"]),
                    false,
                )
                .unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn confidential_alternatives_keep_alias_order_and_drop_training_or_cooling_routes() {
    let mut catalog = catalogue();
    catalog["adapters"]["opencode"]["aliases"]["junior"] = json!([
        "opencode/mimo-v2.6-flash-free",
        "opencode-go/deepseek-v4.1-flash",
        "codex/gpt-6-luna"
    ]);
    let host = Host::new(catalog.clone());
    let store = Store {
        rows: BTreeMap::from([(
            "opencode/opencode-go/deepseek-v4.1-flash".into(),
            row("2026-10-24T00:00:00Z", None),
        )]),
        ..Default::default()
    };
    let runtime = runtime(&host, &store);
    assert_eq!(
        confidential_alternatives(
            &runtime,
            "opencode",
            "opencode/mimo-v2.6-flash-free",
            Some("junior"),
            Some(&catalog)
        )
        .unwrap(),
        vec!["codex/gpt-6-luna"]
    );
    assert!(
        confidential_alternatives(&runtime, "opencode", "x", Some("missing"), Some(&catalog))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cooling_reads_account_scope_and_returns_the_later_exact_or_wildcard_row() {
    let host = Host::new(catalogue());
    let store = Store {
        rows: BTreeMap::from([
            (
                "codex/gpt-6.1-sol".into(),
                row("2026-10-23T10:00:00+00:00", None),
            ),
            ("codex/*".into(), row("2026-10-23T11:00:00Z", None)),
        ]),
        ..Default::default()
    };
    let runtime = runtime(&host, &store);
    let found = runtime
        .cooling("codex", "gpt-6.1-sol@high", None, Some("work"), false)
        .unwrap()
        .unwrap();
    assert_eq!(
        found.get("cooling_until"),
        Some(&json!("2026-10-23T11:00:00Z"))
    );
    let scope = store.scopes.borrow();
    let scope = scope.as_ref().unwrap().get("codex").unwrap();
    assert_eq!(scope.account.value().map(String::as_str), Some("work"));
    assert_eq!(scope.host.value().map(String::as_str), Some("studio"));
}

#[test]
fn cooling_skips_empty_record_dicts_like_python_truthiness() {
    let host = Host::new(catalogue());
    let store = Store::default();
    let runtime = runtime(&host, &store);
    let rows = BTreeMap::from([
        ("codex/gpt-6.1-sol".into(), BTreeMap::<String, Value>::new()),
        ("codex/*".into(), BTreeMap::<String, Value>::new()),
    ]);
    assert!(runtime
        .cooling("codex", "gpt-6.1-sol", Some(&rows), None, false)
        .unwrap()
        .is_none());
}

#[test]
fn cooling_failure_categories_match_python_for_missing_bad_and_mixed_timestamps() {
    let input = json!({
        "records": [
            {"codex/model": {"status": "usage_limited"}},
            {"codex/model": {"cooling_until": "not-a-timestamp"}},
            {
                "codex/model": {"cooling_until": "2026-10-23T10:00:00Z"},
                "codex/*": {"cooling_until": "2026-10-23T11:00:00"}
            }
        ]
    });
    let Some(expected): Option<Vec<String>> = crate::oracle::python(
        "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\nm.snapshot=lambda:{}\nd=json.load(sys.stdin); out=[]\nfor records in d['records']:\n try: m.cooling('codex','model',records)\n except Exception as exc: out.append(type(exc).__name__)\n else: out.append('none')\nprint(json.dumps(out))",
        &input,
    ) else {
        return;
    };
    let host = Host::new(json!({}));
    let store = Store::default();
    let runtime = runtime(&host, &store);
    let actual = input["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| {
            let rows: provenant_records::fabric_records::CooldownRows =
                serde_json::from_value(value.clone()).unwrap();
            match runtime.cooling("codex", "model", Some(&rows), None, false) {
                Err(ExecutionRoutingError::MissingCoolingTimestamp) => "KeyError",
                Err(ExecutionRoutingError::InvalidCoolingTimestamp) => "ValueError",
                Err(ExecutionRoutingError::IncompatibleCoolingTimestamps) => "TypeError",
                _ => "none",
            }
            .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn quota_advice_is_advisory_and_explicit_pins_suppress_quota_cooldowns() {
    let host = Host::new(catalogue());
    let store = Store {
        rows: BTreeMap::from([("codex/*".into(), row("2026-10-24T00:00:00Z", None))]),
        ..Default::default()
    };
    let quota = Quota {
        needs_reselection: Err(QuotaUnavailable),
        selected: Ok(Some("backup".into())),
        pinned_state: Ok(Some("exhausted".into())),
        cooldowns: Ok(BTreeMap::new()),
        valid_label: true,
    };
    let runtime = ExecutionRouting {
        host: &host,
        store: &store,
        quota: Some(&quota),
        now: timestamp(),
    };
    assert!(runtime.claude_account_needs_reselection(Some("work")));
    assert_eq!(runtime.reselect_claude_account().as_deref(), Some("backup"));
    assert!(runtime
        .pinned_account_warning("codex", Some("work"))
        .unwrap()
        .contains("(exhausted)"));
    assert!(runtime
        .cooling("codex", "gpt-6.1-sol", None, Some("work"), true)
        .unwrap()
        .is_none());
}

#[test]
fn confidential_candidate_policy_matches_the_python_source() {
    let input = json!({
        "plan": {
            "adapter": "codex", "model": "gpt-6.1-sol", "effort": "high",
            "route": {"fallback_candidates": [
                {"adapter":"opencode", "model":"opencode/mimo-v2.6-flash-free"},
                {"adapter":"opencode", "model":"opencode-go/deepseek-v4.1-flash", "trains_on_prompts":true},
                {"adapter":"codex", "model":"gpt-6-luna"}
            ]}
        },
        "catalogue": {"adapters":{},"models":{}},
        "policies": ["any", "any"]
    });
    let Some(expected): Option<Vec<Vec<Value>>> = crate::oracle::python(
        "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\nd=json.load(sys.stdin); p=d['plan']; c=d['catalogue']; print(json.dumps([m.candidates(p, policy, c, confidential=private) for policy,private in zip(d['policies'],(False,True))]))",
        &input,
    ) else {
        return;
    };
    let catalog = catalogue();
    let host = Host::new(catalog);
    let store = Store::default();
    let runtime = runtime(&host, &store);
    let plan = plan(&serde_json::to_string(&input["plan"]).unwrap());
    let policy = crate::ordered_json::OrderedValue::String("any".into());
    let catalogue = &input["catalogue"];
    let ordinary = candidates(&runtime, &plan, Some(&policy), Some(catalogue), false).unwrap();
    let private = candidates(&runtime, &plan, Some(&policy), Some(catalogue), true).unwrap();
    let actual: Vec<Vec<Value>> = [ordinary, private]
        .into_iter()
        .map(|items| {
            serde_json::to_value(items)
                .unwrap()
                .as_array()
                .unwrap()
                .clone()
        })
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn default_and_explicit_fallback_policies_match_python_training_rules() {
    let cases = json!([
        {
            "plan": {
                "adapter": "codex", "model": "gpt-6.1-sol",
                "route": {"fallback_candidates": [
                    "codex/gpt-6-luna",
                    {"adapter":"codex", "model":"gpt-6-astra", "trains_on_prompts":true},
                    "opencode/mimo-v2.6-flash-free"
                ]}
            },
            "policy": null,
            "confidential": false
        },
        {
            "plan": {
                "adapter": "codex", "model": "gpt-6.1-sol",
                "route": {"fallback_candidates": ["codex/gpt-6-luna"]}
            },
            "policy": [
                "opencode/mimo-v2.6-flash-free",
                {"adapter":"codex", "model":"gpt-6-astra", "trains_on_prompts":true},
                "codex/gpt-6-luna"
            ],
            "confidential": false
        },
        {
            "plan": {
                "adapter": "codex", "model": "gpt-6.1-sol",
                "route": {"fallback_candidates": ["codex/gpt-6-luna"]}
            },
            "policy": [
                "opencode/mimo-v2.6-flash-free",
                {"adapter":"codex", "model":"gpt-6-astra", "trains_on_prompts":true},
                "codex/gpt-6-luna"
            ],
            "confidential": true
        }
    ]);
    let Some(expected): Option<Vec<Value>> = crate::oracle::python(
        "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\ncases=json.load(sys.stdin); print(json.dumps([m.candidates(case['plan'],case['policy'],{'adapters':{},'models':{}},confidential=case['confidential']) for case in cases]))",
        &cases,
    ) else {
        return;
    };
    let host = Host::new(catalogue());
    let store = Store::default();
    let runtime = runtime(&host, &store);
    let actual = cases
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let plan = plan(&serde_json::to_string(&case["plan"]).unwrap());
            let policy = if case["policy"].is_null() {
                None
            } else {
                Some(crate::pending::from_json(&case["policy"].to_string()).unwrap())
            };
            serde_json::to_value(
                candidates(
                    &runtime,
                    &plan,
                    policy.as_ref(),
                    Some(&json!({"adapters":{},"models":{}})),
                    case["confidential"].as_bool().unwrap_or(false),
                )
                .unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn direct_disclosure_risk_honours_per_model_zero_retention_and_configured_traits() {
    let host = Host::new(catalogue());
    let no_training = crate::ordered_json::OrderedObject(vec![
        (
            "plan_cap_usd".into(),
            crate::ordered_json::OrderedValue::Number(0.into()),
        ),
        (
            "trains_on_prompts".into(),
            crate::ordered_json::OrderedValue::Bool(false),
        ),
    ]);
    assert!(disclosure_risk(
        &host,
        "opencode",
        "opencode/deepseek-v4.1-flash-free",
        Some(&no_training)
    )
    .is_none());
    assert!(disclosure_risk(&host, "opencode", "opencode/mimo-v2.6-flash-free", None).is_some());
}

#[test]
fn model_families_and_disclosure_risks_match_the_python_policy() {
    let input = json!({
        "models": [
            {"model":"claude-sonnet-5-5", "adapter":"codex", "meta":{}},
            {"model":"opencode/deepseek-v4.1-flash-free", "adapter":"opencode", "meta":{"plan_cap_usd":0,"trains_on_prompts":false}},
            {"model":"opencode/mimo-v2.6-flash-free", "adapter":"opencode", "meta":{"plan_cap_usd":0,"trains_on_prompts":false}},
            {"model":"opencode/unknown-free", "adapter":"opencode", "meta":{}},
            {"model":"gpt-6.1-sol", "adapter":"codex", "meta":{"trains_on_prompts":true}}
        ]
    });
    let Some(expected): Option<(Vec<String>, Vec<Option<String>>)> = crate::oracle::python(
        "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\nd=json.load(sys.stdin); print(json.dumps((m.model_families(d['models'][0]['model']),[m.disclosure_risk(x['adapter'],x['model'],x['meta']) for x in d['models']])))",
        &input,
    ) else {
        return;
    };
    let host = Host::new(catalogue());
    assert_eq!(
        model_families(&host, input["models"][0]["model"].as_str().unwrap(), None),
        expected.0
    );
    let actual: Vec<Option<String>> = input["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let meta: crate::ordered_json::OrderedObject =
                crate::pending::from_json(&entry["meta"].to_string()).unwrap();
            disclosure_risk(
                &host,
                entry["adapter"].as_str().unwrap(),
                entry["model"].as_str().unwrap(),
                Some(&meta),
            )
            .map(str::to_owned)
        })
        .collect();
    assert_eq!(actual, expected.1);
}

#[test]
fn cooling_matches_python_datetime_max_for_exact_and_wildcard_rows() {
    let input = json!({
        "adapter":"codex", "model":"gpt-6.1-sol@high",
        "records": {
            "codex/gpt-6.1-sol": {"cooling_until":"2026-10-23T10:00:00+00:00","status":"usage_limited"},
            "codex/*": {"cooling_until":"2026-10-23T12:00:00Z","status":"quota_exhausted"}
        }
    });
    let Some(expected): Option<Value> = crate::oracle::python(
        "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\nd=json.load(sys.stdin); print(json.dumps(m.cooling(d['adapter'],d['model'],d['records'])))",
        &input,
    ) else {
        return;
    };
    let host = Host::new(catalogue());
    let store = Store::default();
    let runtime = runtime(&host, &store);
    let records: provenant_records::fabric_records::CooldownRows =
        serde_json::from_value(input["records"].clone()).unwrap();
    let actual = serde_json::to_value(
        runtime
            .cooling(
                input["adapter"].as_str().unwrap(),
                input["model"].as_str().unwrap(),
                Some(&records),
                None,
                false,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(actual, expected);
}
