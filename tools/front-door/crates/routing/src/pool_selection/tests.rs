use super::*;
use serde_json::{json, Number, Value};
use std::collections::BTreeMap;
use std::sync::LazyLock;

static ALL_INSTALLED: AllInstalled = AllInstalled;
static NO_COOLDOWNS: LazyLock<Cooling> = LazyLock::new(Cooling::default);
static NO_QUOTA: LazyLock<Quota> = LazyLock::new(Quota::default);

#[derive(Default)]
struct AllInstalled;

impl InstalledAdapterSource for AllInstalled {
    fn installed(&self, _: &str) -> bool {
        true
    }
}

#[derive(Default)]
struct NoCompatibility;

impl CompatibilitySource for NoCompatibility {
    fn load(&mut self, _: &str) -> Result<CompatibilityLoad, String> {
        Ok(CompatibilityLoad::default())
    }
}

#[derive(Default)]
struct RulesCompatibility(BTreeMap<String, CompatibilityLoad>);

impl CompatibilitySource for RulesCompatibility {
    fn load(&mut self, adapter: &str) -> Result<CompatibilityLoad, String> {
        Ok(self.0.get(adapter).cloned().unwrap_or_default())
    }
}

#[derive(Default)]
struct Cooling(BTreeMap<(String, String), String>);

impl CooldownSource for Cooling {
    fn cooling_until(&self, adapter: &str, model: &str) -> Result<Option<String>, String> {
        Ok(self.0.get(&(adapter.to_owned(), model.to_owned())).cloned())
    }
}

#[derive(Default)]
struct Quota(OrderedObject);

impl QuotaStateSource for Quota {
    fn states(&self) -> Result<OrderedObject, String> {
        Ok(self.0.clone())
    }
}

struct MemoryRotation(OrderedValue);

impl Default for MemoryRotation {
    fn default() -> Self {
        Self(OrderedValue::Null)
    }
}

impl RotationStateStore for MemoryRotation {
    fn update_state(
        &mut self,
        update: &mut dyn FnMut(&mut OrderedValue) -> Result<usize, PoolSelectionError>,
    ) -> Result<usize, String> {
        update(&mut self.0).map_err(|error| error.to_string())
    }
}

#[derive(Default)]
struct FirstDraw;

impl WeightedDraw for FirstDraw {
    fn choose(&mut self, weights: &[Weight]) -> Result<usize, String> {
        if weights.is_empty() {
            Err("empty weighted draw".into())
        } else {
            Ok(0)
        }
    }
}

struct QueuedDraw(Vec<usize>);

impl WeightedDraw for QueuedDraw {
    fn choose(&mut self, _: &[Weight]) -> Result<usize, String> {
        if self.0.is_empty() {
            return Err("no queued draw".into());
        }
        Ok(self.0.remove(0))
    }
}

fn effort_order() -> EffortOrder {
    EffortOrder::new(
        ["low", "medium", "high", "xhigh", "ultra"]
            .into_iter()
            .enumerate()
            .map(|(rank, name)| (name.to_owned(), rank)),
    )
}

fn catalog() -> OrderedValue {
    serde_json::from_str(
        r#"{
          "adapters": {
            "claude": {"models": [{"id": "claude-opus-5-5", "efforts": ["low", "medium", "high"]}]},
            "codex": {"models": [
              {"id": "gpt-6.1-sol", "names": ["gpt-6-sol", "sol"], "efforts": ["low", "high", "xhigh"]},
              {"id": "gpt-6.1-astra"}
            ]}
          },
          "model_patterns": [
            {"pattern": "claude-", "family": "anthropic"},
            {"pattern": "gpt-", "family": "openai"}
          ],
          "routes": {
            "senior": [
              {"model": "claude/claude-opus-5-5", "weight": "high"},
              {"model": "codex/gpt-6.1-sol", "weight": "normal", "effort": ["high", "xhigh"]},
              {"model": "codex/gpt-6.1-astra", "weight": "sparing"}
            ],
            "utility": [{"model": "codex/gpt-6.1-sol", "weight": "normal"}]
          },
          "route_defaults": {"utility": {"confidential": true, "allow_free": false}},
          "route_synonyms": {"review": "senior"},
          "model_traits": {}
        }"#,
    )
    .unwrap()
}

fn host(catalog: &OrderedValue) -> CatalogueModelHost {
    CatalogueModelHost::new(catalog, BTreeMap::new())
}

fn availability<'a>(
    catalog: &'a OrderedValue,
    compatibility: &'a mut dyn CompatibilitySource,
) -> Availability<'a> {
    Availability::new(
        catalog,
        &ALL_INSTALLED,
        compatibility,
        &*NO_COOLDOWNS,
        &*NO_QUOTA,
    )
}

fn request(text: &str) -> OrderedValue {
    serde_json::from_str(text).unwrap()
}

fn picks(value: &OrderedValue) -> Vec<String> {
    let Some(OrderedValue::Array(picks)) = value.get("picks") else {
        return Vec::new();
    };
    picks
        .iter()
        .filter_map(|pick| {
            Some(format!(
                "{}/{}",
                pick.get("adapter")?.as_str()?,
                pick.get("model")?.as_str()?
            ))
        })
        .collect()
}

#[test]
fn canonical_spellings_keep_the_first_entry_and_its_position() {
    let catalog: OrderedValue = serde_json::from_str(
        r#"{"adapters":{"codex":{"models":[{"id":"gpt-6.1-sol","names":["gpt-6-sol"]}]}},
        "model_patterns":[{"pattern":"gpt-","family":"openai"}],
        "routes":{"senior":[{"model":"codex/gpt-6-sol","weight":"off"},
        {"model":"codex/gpt-6.1-sol","weight":"high"}]}}"#,
    )
    .unwrap();
    let host = host(&catalog);
    let entries = pool(&host, &catalog, "senior", &effort_order()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "codex/gpt-6.1-sol");
    assert_eq!(entries[0].weight_label, OrderedValue::String("off".into()));
    assert!(entries[0].weight.is_zero());
}

#[test]
fn exact_integer_weights_beat_nearby_float_weights_without_rounding() {
    let integer = Weight::Integer(9_007_199_254_740_993);
    let float = Weight::Float(9_007_199_254_740_992.0);
    assert_eq!(integer.compare(&float), Ordering::Greater);
}

#[test]
fn top_selection_filters_off_members_and_clamps_effort() {
    let catalog = catalog();
    let host = host(&catalog);
    let mut compatibility = NoCompatibility;
    let mut available = availability(&catalog, &mut compatibility);
    let mut rotation = MemoryRotation::default();
    let mut draw = FirstDraw;
    let result = pick(
        &host,
        &catalog,
        &request(r#"{"route":"senior","effort":"low"}"#),
        &mut available,
        &mut rotation,
        &mut draw,
        &effort_order(),
    )
    .unwrap();
    assert_eq!(picks(&result), ["claude/claude-opus-5-5"]);
    let Some(OrderedValue::Array(selected)) = result.get("picks") else {
        panic!("successful selection must have picks");
    };
    assert_eq!(
        selected[0].get("reason").and_then(OrderedValue::as_str),
        Some("senior top")
    );
    assert_eq!(
        selected[0].get("effort").and_then(OrderedValue::as_str),
        Some("low")
    );
}

#[test]
fn null_rotate_uses_the_host_profile_default() {
    let catalog = catalog();
    let host = CatalogueModelHost::with_mode(&catalog, BTreeMap::new(), PoolMode::Rotate);
    let mut compatibility = NoCompatibility;
    let mut available = availability(&catalog, &mut compatibility);
    let result = pick(
        &host,
        &catalog,
        &request(r#"{"route":"senior","rotate":null}"#),
        &mut available,
        &mut MemoryRotation::default(),
        &mut FirstDraw,
        &effort_order(),
    )
    .unwrap();
    assert_eq!(
        result.get("mode").and_then(OrderedValue::as_str),
        Some("rotate")
    );
}

#[test]
fn compatibility_patterns_follow_fnmatch_character_classes() {
    assert!(glob_match("gpt-[6-7].*", "gpt-6.1-sol"));
    assert!(glob_match("gpt-[!6].*", "gpt-7.1-sol"));
    assert!(!glob_match("gpt-[!6].*", "gpt-6.1-sol"));
}

#[test]
fn council_uses_fresh_safe_families_before_repeating() {
    let mut catalog = catalog();
    let OrderedValue::Object(root) = &mut catalog else {
        unreachable!()
    };
    root.insert(
        "routes".into(),
        serde_json::from_str(
            r#"{"senior":[
              {"model":"claude/claude-opus-5-5","weight":"high"},
              {"model":"codex/gpt-6.1-sol","weight":"normal"}
            ]}"#,
        )
        .unwrap(),
    );
    let host = host(&catalog);
    let mut compatibility = NoCompatibility;
    let mut available = availability(&catalog, &mut compatibility);
    let mut rotation = MemoryRotation::default();
    let mut draw = QueuedDraw(vec![0, 0]);
    let result = pick(
        &host,
        &catalog,
        &request(r#"{"route":"senior","council":2}"#),
        &mut available,
        &mut rotation,
        &mut draw,
        &effort_order(),
    )
    .unwrap();
    assert_eq!(
        picks(&result),
        ["claude/claude-opus-5-5", "codex/gpt-6.1-sol"]
    );
    let Some(OrderedValue::Array(selected)) = result.get("picks") else {
        panic!("successful selection must have picks");
    };
    assert_eq!(
        selected[1].get("reason").and_then(OrderedValue::as_str),
        Some("senior council 2/2")
    );
}

#[test]
fn rotation_advances_smoothly_by_exact_integer_weight() {
    let catalog = catalog();
    let host = host(&catalog);
    let mut compatibility = NoCompatibility;
    let mut available = availability(&catalog, &mut compatibility);
    let mut rotation = MemoryRotation::default();
    let mut draw = FirstDraw;
    let mut sequence = Vec::new();
    for _ in 0..13 {
        let result = pick(
            &host,
            &catalog,
            &request(r#"{"route":"senior","rotate":true,"project":"/work/a"}"#),
            &mut available,
            &mut rotation,
            &mut draw,
            &effort_order(),
        )
        .unwrap();
        sequence.extend(picks(&result));
    }
    let counts = sequence
        .into_iter()
        .fold(BTreeMap::<String, usize>::new(), |mut counts, key| {
            *counts.entry(key).or_default() += 1;
            counts
        });
    assert_eq!(counts["claude/claude-opus-5-5"], 8);
    assert_eq!(counts["codex/gpt-6.1-sol"], 4);
    assert_eq!(counts["codex/gpt-6.1-astra"], 1);
}

#[test]
fn rotation_resets_bad_schema_and_keeps_projects_separate() {
    let catalog = catalog();
    let host = host(&catalog);
    let entries = pool(&host, &catalog, "senior", &effort_order()).unwrap();
    let mut state: OrderedValue =
        serde_json::from_str(r#"{"schema_version":2,"projects":{"old":{"senior":{"stale":8}}}}"#)
            .unwrap();

    rotate_document(&mut state, &entries, "/work/one", "senior").unwrap();
    rotate_document(&mut state, &entries, "/work/two", "senior").unwrap();

    assert_eq!(
        state.get("schema_version"),
        Some(&OrderedValue::Number(Number::from(1)))
    );
    let projects = state
        .get("projects")
        .and_then(OrderedValue::as_object)
        .unwrap();
    assert!(projects.get("/work/one").is_some());
    assert!(projects.get("/work/two").is_some());
    assert!(projects.get("old").is_none());
}

#[test]
fn rotation_matches_python_float_cursor_wire_after_malformed_state_recovery() {
    let catalog = catalog();
    let host = host(&catalog);
    let entries = pool(&host, &catalog, "senior", &effort_order()).unwrap();
    let wire_entries: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let weight = match &entry.weight {
                Weight::Integer(value) => Value::Number(Number::from(*value)),
                Weight::Float(value) => {
                    Value::Number(Number::from_f64(*value).expect("finite fixture weight"))
                }
            };
            json!({"key": entry.key, "weight": weight})
        })
        .collect();
    let input = json!({"entries": wire_entries});
    let Some(expected): Option<Value> = crate::oracle::python(
        r#"
import json,pathlib,sys,tempfile
sys.path.insert(0,str(pathlib.Path('scripts').resolve()))
import model_route_pools as p
d=json.load(sys.stdin)
with tempfile.TemporaryDirectory() as directory:
 path=pathlib.Path(directory)/'rotate.json'
 path.write_text('invalid json')
 picks=[p._rotate(d['entries'],path,'/work/a','senior')['key'] for _ in range(3)]
 print(json.dumps({'picks':picks,'state':path.read_text()}))
"#,
        &input,
    ) else {
        return;
    };

    let mut state = OrderedValue::Null;
    let mut selected = Vec::new();
    for _ in 0..3 {
        let index = rotate_document(&mut state, &entries, "/work/a", "senior").unwrap();
        selected.push(entries[index].key.clone());
    }
    let actual = json!({
        "picks": selected,
        "state": format!("{}\n", serde_json::to_string_pretty(&state.to_value()).unwrap()),
    });
    assert_eq!(actual, expected);
}

#[test]
fn rotation_persisted_state_matches_python_after_multiple_rounds() {
    let catalog = catalog();
    let host = host(&catalog);
    let entries = pool(&host, &catalog, "senior", &effort_order()).unwrap();
    let wire_entries: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let weight = match &entry.weight {
                Weight::Integer(value) => Value::Number(Number::from(*value)),
                Weight::Float(value) => {
                    Value::Number(Number::from_f64(*value).expect("finite fixture weight"))
                }
            };
            json!({"key": entry.key, "weight": weight})
        })
        .collect();
    let input = json!({"entries": wire_entries});
    let Some(expected): Option<Value> = crate::oracle::python(
        r#"
import json,pathlib,sys,tempfile
sys.path.insert(0,str(pathlib.Path('scripts').resolve()))
import model_route_pools as p
d=json.load(sys.stdin)
with tempfile.TemporaryDirectory() as directory:
 path=pathlib.Path(directory)/'rotate.json'
 picks=[p._rotate(d['entries'],path,'/work/a','senior')['key'] for _ in range(5)]
 print(json.dumps({'picks':picks,'state':path.read_text()}))
"#,
        &input,
    ) else {
        return;
    };

    let mut state = OrderedValue::Null;
    let mut selected = Vec::new();
    for _ in 0..5 {
        let index = rotate_document(&mut state, &entries, "/work/a", "senior").unwrap();
        selected.push(entries[index].key.clone());
    }
    let actual = json!({
        "picks": selected,
        "state": format!("{}\n", serde_json::to_string_pretty(&state.to_value()).unwrap()),
    });
    assert_eq!(actual, expected);
}

#[test]
fn unhashable_effort_and_duplicate_failure_warnings_match_python() {
    let catalog = catalog();
    let effort_request = request(r#"{"route":"senior","adapter":"codex","effort":["low"]}"#);
    let effort_input = json!({
        "catalog": catalog.to_value(),
        "request": effort_request.to_value(),
    });
    let Some(expected_effort): Option<Value> = crate::oracle::python(
        r#"
import json,pathlib,random,sys,tempfile
sys.path.insert(0,str(pathlib.Path('scripts').resolve()))
import model_route as m
d=json.load(sys.stdin); c=d['catalog']
a=m._pools.Availability(m,c,installed=lambda adapter:True,cooldowns={})
a._enabled={name:'' for name in c.get('adapters',{})}
a._compatibility={name:None for name in c.get('adapters',{})}
try:
 with tempfile.TemporaryDirectory() as directory:
  result=m._pools.pick(m,c,d['request'],availability=a,state_path=pathlib.Path(directory)/'rotate.json',
   effort_order=m.EFFORT_ORDER,rng=random.Random(4))
 print(json.dumps({'result':result}))
except TypeError as error:
 print(json.dumps({'error':str(error)}))
"#,
        &effort_input,
    ) else {
        return;
    };
    let host = host(&catalog);
    let mut compatibility = NoCompatibility;
    let mut available = availability(&catalog, &mut compatibility);
    let effort_result = pick(
        &host,
        &catalog,
        &effort_request,
        &mut available,
        &mut MemoryRotation::default(),
        &mut FirstDraw,
        &effort_order(),
    );
    let actual_effort = match effort_result {
        Err(error @ PoolSelectionError::UnhashableEffort { .. }) => {
            json!({"error": error.to_string()})
        }
        Ok(result) => json!({"result": result.to_value()}),
        Err(error) => panic!("unexpected policy error: {error}"),
    };
    assert_eq!(actual_effort, expected_effort);

    let empty_effort_request = request(r#"{"route":"senior","adapter":"codex","effort":[]}"#);
    let mut compatibility = NoCompatibility;
    let mut available = availability(&catalog, &mut compatibility);
    let result = pick(
        &host,
        &catalog,
        &empty_effort_request,
        &mut available,
        &mut MemoryRotation::default(),
        &mut FirstDraw,
        &effort_order(),
    )
    .unwrap();
    let Some(OrderedValue::Array(picks)) = result.get("picks") else {
        panic!("successful selection must contain picks");
    };
    assert_eq!(
        picks
            .first()
            .and_then(|pick| pick.get("effort"))
            .and_then(OrderedValue::as_str),
        Some("high")
    );

    let mut catalog_with_traits = catalog.clone();
    let OrderedValue::Object(root) = &mut catalog_with_traits else {
        unreachable!()
    };
    root.insert(
        "model_traits".into(),
        serde_json::from_str(r#"{"codex/gpt-6.1-sol":["trains-on-prompts"]}"#).unwrap(),
    );
    let repeated_request =
        request(r#"{"models":["codex/gpt-6.1-sol","codex/gpt-6.1-sol"],"confidential":true}"#);
    let duplicate_input = json!({
        "catalog": catalog_with_traits.to_value(),
        "request": repeated_request.to_value(),
    });
    let Some(expected_duplicates): Option<Vec<Value>> = crate::oracle::python(
        r#"
import json,pathlib,random,sys,tempfile
sys.path.insert(0,str(pathlib.Path('scripts').resolve()))
import model_route as m
d=json.load(sys.stdin); c=d['catalog']
a=m._pools.Availability(m,c,installed=lambda adapter:True,cooldowns={})
a._enabled={name:'' for name in c.get('adapters',{})}
a._compatibility={name:None for name in c.get('adapters',{})}
with tempfile.TemporaryDirectory() as directory:
 result=m._pools.pick_many(m,c,[d['request']],availability=a,state_path=pathlib.Path(directory)/'rotate.json',
  effort_order=m.EFFORT_ORDER,rng=random.Random(4))
 print(json.dumps(result))
"#,
        &duplicate_input,
    ) else {
        return;
    };
    let host_with_traits = CatalogueModelHost::new(&catalog_with_traits, BTreeMap::new());
    let mut compatibility = NoCompatibility;
    let mut available = availability(&catalog_with_traits, &mut compatibility);
    let actual_duplicates = pick_many(
        &host_with_traits,
        &catalog_with_traits,
        &[repeated_request],
        &mut available,
        &mut MemoryRotation::default(),
        &mut FirstDraw,
        &effort_order(),
    )
    .unwrap()
    .into_iter()
    .map(|value| value.to_value())
    .collect::<Vec<_>>();
    assert_eq!(actual_duplicates, expected_duplicates);
}

#[test]
fn compatibility_gate_runs_before_installed_and_cooldown_checks() {
    let catalog = catalog();
    let host = host(&catalog);
    let mut compat = RulesCompatibility(BTreeMap::from([(
        "codex".into(),
        CompatibilityLoad {
            policy: Some(CompatibilityPolicy {
                enabled: true,
                allowed_families: vec!["anthropic".into()],
                ..Default::default()
            }),
            status: String::new(),
        },
    )]));
    let mut available = availability(&catalog, &mut compat);
    let entry = pool(&host, &catalog, "utility", &effort_order()).unwrap();
    assert_eq!(
        available
            .reason(&entry[0].adapter, &entry[0].model, &entry[0].weight, &host)
            .unwrap(),
        "codex cannot run the openai family"
    );
}

#[test]
fn pick_many_returns_policy_refusals_without_aborting_later_requests() {
    let catalog = catalog();
    let host = host(&catalog);
    let mut compatibility = NoCompatibility;
    let mut available = availability(&catalog, &mut compatibility);
    let mut rotation = MemoryRotation::default();
    let mut draw = FirstDraw;
    let results = pick_many(
        &host,
        &catalog,
        &[
            request(r#"{"route":"missing"}"#),
            request(r#"{"route":"senior"}"#),
            OrderedValue::String("bad".into()),
        ],
        &mut available,
        &mut rotation,
        &mut draw,
        &effort_order(),
    )
    .unwrap();
    assert_eq!(
        results[0].get("error").and_then(OrderedValue::as_str),
        Some("route_invalid")
    );
    assert_eq!(
        results[1].get("status").and_then(OrderedValue::as_str),
        Some("ok")
    );
    assert_eq!(
        results[2].get("error").and_then(OrderedValue::as_str),
        Some("invalid_input")
    );
}

#[test]
fn selection_matches_python_for_alias_collapse_and_effort_clamping() {
    let catalog = catalog();
    let request = request(r#"{"route":"senior","effort":"low","rotate":false}"#);
    let input = json!({
        "catalog": catalog.to_value(),
        "request": request.to_value(),
    });
    let Some(expected): Option<Value> = crate::oracle::python(
        r#"
import json,pathlib,random,sys,tempfile
sys.path.insert(0,str(pathlib.Path('scripts').resolve()))
import model_route as m
d=json.load(sys.stdin)
m.model_route_live._quota_states=lambda:{}
c=d['catalog']
a=m._pools.Availability(m,c,installed=lambda adapter:True,cooldowns={})
a._enabled={name:'' for name in c.get('adapters',{})}
a._compatibility={name:None for name in c.get('adapters',{})}
with tempfile.TemporaryDirectory() as directory:
 result=m._pools.pick(m,c,d['request'],availability=a,state_path=pathlib.Path(directory)/'rotate.json',
  effort_order=m.EFFORT_ORDER,rng=random.Random(4))
 print(json.dumps(result))
"#,
        &input,
    ) else {
        return;
    };
    let host = host(&catalog);
    let mut compatibility = NoCompatibility;
    let mut available = availability(&catalog, &mut compatibility);
    let mut rotation = MemoryRotation::default();
    let mut draw = FirstDraw;
    let actual = pick(
        &host,
        &catalog,
        &request,
        &mut available,
        &mut rotation,
        &mut draw,
        &effort_order(),
    )
    .unwrap()
    .to_value();
    assert_eq!(actual, expected);
}
