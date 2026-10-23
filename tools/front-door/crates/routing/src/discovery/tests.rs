use super::*;
use serde_json::json;
#[test]
fn opencode_name_resolution_matches_python_for_success_ambiguity_and_suggestions() {
    let live = vec![
        "opencode/kimi-k2",
        "moonshot/kimi-k2",
        "vendor/qwen3.8-flash",
        "vendor/deepseek-v4",
        "other/deepseek-v5",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let names = [
        "",
        "kimi-k2",
        "kimi",
        "qwen3.8-flash",
        "qwen3.8",
        "deepseek",
        "depseek-v4",
        "gpt-99",
        "vendor/qwen",
        "qwen#x",
        "KIMI",
        "qwen3.8-flsh",
    ];
    let Some(expected): Option<Value> = crate::oracle::python(
        r#"
import importlib.util,json,pathlib,sys
s=importlib.util.spec_from_file_location('live',pathlib.Path('scripts/model_route_live.py'));m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
d=json.load(sys.stdin);print(json.dumps([m._resolve_opencode_name(n,d['live']) for n in d['names']]))
"#,
        &json!({"live":live,"names":names}),
    ) else {
        return;
    };
    assert_eq!(
        json!(
            names
                .iter()
                .map(|n| resolve_opencode_name(n, &live))
                .collect::<Vec<_>>()
        ),
        expected
    );
    assert_eq!(
        opencode_live_models::<()>(true, Some(13.0), 10.0, |_| panic!(
            "short refresh budget invoked probe"
        )),
        (vec![], false)
    );
    assert_eq!(
        opencode_live_models(false, None, 10.0, |request| {
            assert_eq!(
                request,
                ProviderListingRequest {
                    adapter: "opencode".into(),
                    executable: "opencode".into(),
                    deadline: 22.0,
                    listing_only: true,
                    refresh: false
                }
            );
            Ok::<_, ()>(json!({"models":["a",3,"b"],"cache_hit":true}))
        }),
        (vec!["a".into(), "b".into()], true)
    );
    assert_eq!(
        opencode_live_models(false, None, 10.0, |_| Err::<Value, _>(())),
        (vec![], false)
    );
    assert_eq!(
        opencode_live_models::<()>(true, Some(14.0), 10.0, |request| {
            assert_eq!(
                request,
                ProviderListingRequest {
                    adapter: "opencode".into(),
                    executable: "opencode".into(),
                    deadline: 14.0,
                    listing_only: true,
                    refresh: true
                }
            );
            Ok(json!({"models":["x/y"]}))
        }),
        (vec!["x/y".into()], false)
    );
    for record in [
        json!(null),
        json!([]),
        json!({"status":"probe_unavailable","models":["x/y"],"cache_hit":true}),
        json!({"models":3}),
    ] {
        assert_eq!(
            opencode_live_models::<()>(false, None, 10.0, |_| Ok(record)),
            (vec![], false)
        );
    }
    assert_eq!(resolve_opencode_name("kimi", &[]), (None, vec![], None));
}
#[test]
fn close_match_ratios_match_cpython_including_popular_characters() {
    let cases = vec![
        ("abxcd".into(), "abcd".into()),
        ("tide".into(), "diet".into()),
        ("diet".into(), "tide".into()),
        ("a".repeat(250), format!("{}b", "a".repeat(249))),
        ("abab".repeat(80), "baba".repeat(80)),
        ("日本語a".into(), "日本b語".into()),
    ];
    let Some(expected): Option<Vec<f64>> = crate::oracle::python(
        "import difflib,json,sys;print(json.dumps([difflib.SequenceMatcher(None,a,b).ratio() for a,b in json.load(sys.stdin)]))",
        &json!(cases),
    ) else {
        return;
    };
    for ((a, b), expected) in cases.iter().zip(expected) {
        assert_eq!(similarity(a, b), expected);
    }
}
#[test]
fn fallback_privacy_opt_in_canonical_deduplication_and_cooldowns_match_python() {
    let catalog = json!({"adapters":{"fixture":{"trains_on_prompts":false,"aliases":{"principal":["primary","paid","free","zero","alias","cool"]},"models":[{"id":"primary"},{"id":"paid","names":["alias"],"plan_cap_usd":1},{"id":"free","plan_cap_usd":0,"trains_on_prompts":true},{"id":"zero","plan_cap_usd":0,"trains_on_prompts":false},{"id":"cool","plan_cap_usd":1}]},"opencode":{"aliases":{"senior":["provider/paid"]},"models":[{"id":"provider/paid","plan_cap_usd":2}]}}});
    let cooldowns =
        crate::pending::from_json(r#"{"fixture/cool":{"cooling_until":"2026-10-24T00:00:00Z"}}"#)
            .unwrap();
    let modes = [None, Some("false"), Some("any"), Some("true")];
    let mut fixtures = vec![];
    for flag in [Some(true), Some(false), None] {
        let mut catalog = catalog.clone();
        if let Some(flag) = flag {
            catalog["adapters"]["fixture"]["trains_on_prompts"] = json!(flag);
        } else {
            catalog["adapters"]["fixture"]
                .as_object_mut()
                .unwrap()
                .remove("trains_on_prompts");
        }
        for fallback in modes {
            for explicit in [true, false] {
                for requested in [vec![], vec!["fixture/free"], vec!["opencode/provider/paid"]] {
                    fixtures.push(json!({"catalog":catalog,"fallback":fallback,"explicit":explicit,"requested":requested}));
                }
            }
        }
    }
    let Some(expected): Option<Value> = crate::oracle::python(
        r#"
import json,pathlib,sys
from datetime import datetime,timezone
sys.path.insert(0,str(pathlib.Path('scripts').resolve()))
import model_route as m
d=json.load(sys.stdin)
class Clock(datetime):
 @classmethod
 def now(cls,tz=None):return cls(2026,10,23,tzinfo=timezone.utc)
m.model_route_live.datetime=Clock
print(json.dumps([m.model_route_live._fallback_candidates('fixture','principal','primary',f['catalog'],d['cooldowns'],f['fallback'],f['requested'],f['explicit']) for f in d['fixtures']]))
"#,
        &json!({"fixtures":fixtures,"cooldowns":cooldowns}),
    ) else {
        return;
    };
    let actual = fixtures
        .iter()
        .map(|fixture| {
            let catalog = &fixture["catalog"];
            let ordered = OrderedValue::from(catalog.clone());
            let cached = BTreeMap::new();
            let resolver = ModelCatalog {
                catalog,
                cached: &cached,
            };
            let requested: Vec<String> =
                serde_json::from_value(fixture["requested"].clone()).unwrap();
            fallback_candidates(
                &FallbackRequest {
                    adapter: "fixture",
                    tier: "principal",
                    model: "primary",
                    fallback: fixture["fallback"].as_str(),
                    requested_routes: &requested,
                    explicit: fixture["explicit"].as_bool().unwrap(),
                },
                &resolver,
                &ordered,
                &cooldowns,
                "2026-10-23T00:00:00Z".parse().unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(json!(actual), expected);
}

#[test]
fn discovery_budget_and_probe_request_match_python_on_failure_and_refresh_edges() {
    let fixtures = vec![
        json!({"refresh":false,"deadline":null,"record":{"models":["x/y",3],"cache_hit":true}}),
        json!({"refresh":true,"deadline":13.999999,"record":{"models":["x/y"]}}),
        json!({"refresh":true,"deadline":14.0,"record":{"models":["x/y"]}}),
        json!({"refresh":false,"deadline":9.0,"record":{"models":["x/y"]}}),
        json!({"refresh":false,"deadline":null,"record":{"status":"probe_unavailable","models":["x/y"],"cache_hit":true}}),
        json!({"refresh":false,"deadline":null,"record":null}),
        json!({"refresh":false,"deadline":null,"record":{"models":{},"cache_hit":[1]}}),
        json!({"refresh":false,"deadline":null,"record":{},"raises":true}),
    ];
    let Some(expected): Option<Value> = crate::oracle::python(
        r#"
import importlib.util,json,pathlib,sys,types
s=importlib.util.spec_from_file_location('live',pathlib.Path('scripts/model_route_live.py'));m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
m.time=types.SimpleNamespace(monotonic=lambda:10.0)
output=[]
for fixture in json.load(sys.stdin):
 calls=[]
 def probe(adapter,executable,deadline,**kwargs):
  calls.append(dict(adapter=adapter,executable=executable,deadline=deadline,**kwargs))
  if fixture.get('raises'):raise OSError('fixture probe unavailable')
  return fixture['record'],0
 m.model_route_probe=types.SimpleNamespace(probe_capabilities=probe)
 output.append([m._opencode_live_models(refresh=fixture['refresh'],deadline=fixture['deadline']),calls])
print(json.dumps(output))
"#,
        &json!(fixtures),
    ) else {
        return;
    };
    let actual: Vec<_> = fixtures
        .iter()
        .map(|f| {
            let mut calls = vec![];
            let result = opencode_live_models(
                f["refresh"].as_bool().unwrap(),
                f["deadline"].as_f64(),
                10.0,
                |request| {
                    calls.push(request);
                    if f["raises"] == true {
                        Err(())
                    } else {
                        Ok(f["record"].clone())
                    }
                },
            );
            json!([result, calls])
        })
        .collect();
    assert_eq!(json!(actual), expected);
}
