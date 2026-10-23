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
        json!(names
            .iter()
            .map(|n| resolve_opencode_name(n, &live))
            .collect::<Vec<_>>()),
        expected
    );
    assert_eq!(
        opencode_live_models::<()>(true, Some(13.0), 10.0, |_, _| panic!(
            "short refresh budget invoked probe"
        )),
        (vec![], false)
    );
    assert_eq!(
        opencode_live_models(false, None, 10.0, |deadline, refresh| {
            assert_eq!(deadline, 22.0);
            assert!(!refresh);
            Ok::<_, ()>(json!({"models":["a",3,"b"],"cache_hit":true}))
        }),
        (vec!["a".into(), "b".into()], true)
    );
    assert_eq!(
        opencode_live_models(false, None, 10.0, |_, _| Err::<Value, _>(())),
        (vec![], false)
    );
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
    let Some(expected):Option<Vec<f64>>=crate::oracle::python("import difflib,json,sys;print(json.dumps([difflib.SequenceMatcher(None,a,b).ratio() for a,b in json.load(sys.stdin)]))",&json!(cases))else{return;};
    for ((a, b), expected) in cases.iter().zip(expected) {
        assert_eq!(similarity(a, b), expected);
    }
}
#[test]
fn fallback_privacy_opt_in_canonical_deduplication_and_cooldowns_match_python() {
    let catalog = json!({"adapters":{"fixture":{"trains_on_prompts":false,"aliases":{"principal":["primary","paid","free","zero","alias","cool"]},"models":[{"id":"primary"},{"id":"paid","names":["alias"],"plan_cap_usd":1},{"id":"free","plan_cap_usd":0,"trains_on_prompts":true},{"id":"zero","plan_cap_usd":0,"trains_on_prompts":false},{"id":"cool","plan_cap_usd":1}]},"opencode":{"aliases":{"senior":["provider/paid"]},"models":[{"id":"provider/paid","plan_cap_usd":2}]}}});
    let ordered = OrderedValue::from(catalog.clone());
    let cached = BTreeMap::new();
    let resolver = ModelCatalog {
        catalog: &catalog,
        cached: &cached,
    };
    let cooldowns =
        crate::pending::from_json(r#"{"fixture/cool":{"cooling_until":"2026-10-24T00:00:00Z"}}"#)
            .unwrap();
    let modes = [None, Some("false"), Some("any"), Some("true")];
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
print(json.dumps([m.model_route_live._fallback_candidates('fixture','principal','primary',d['catalog'],d['cooldowns'],f,[],False) for f in d['modes']]))
"#,
        &json!({"catalog":catalog,"cooldowns":cooldowns,"modes":modes}),
    ) else {
        return;
    };
    let actual = modes
        .into_iter()
        .map(|fallback| {
            fallback_candidates(
                &FallbackRequest {
                    adapter: "fixture",
                    tier: "principal",
                    model: "primary",
                    fallback,
                    requested_routes: &[],
                    explicit: false,
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
