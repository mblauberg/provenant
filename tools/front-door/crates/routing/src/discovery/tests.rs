use super::*;
use serde_json::json;
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
