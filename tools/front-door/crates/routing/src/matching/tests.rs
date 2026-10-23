use super::*;

#[test]
fn registered_and_live_model_choices_match_python_including_ambiguity_and_retirement() {
    let catalog: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../config/model-routing.json"
    )))
    .unwrap();
    let cached = BTreeMap::from([
        (
            "codex".into(),
            vec!["gpt-6.2-sol".into(), "gpt-7-sol".into()],
        ),
        ("agy".into(), vec!["gemini-3.9-flash-high".into()]),
    ]);
    let resolver = ModelCatalog {
        catalog: &catalog,
        cached: &cached,
    };
    let cases: Vec<_> = ["codex", "claude", "agy", "cursor", "opencode", "kiro"]
        .into_iter()
        .flat_map(|a| {
            [
                "",
                "sol",
                "GPT_SOL",
                "gpt-5-sol",
                "gpt-6.1-sol",
                "gpt-6.2-sol",
                "gpt-7-sol",
                "luna",
                "sonnet",
                "opus",
                "claude-sonnet-5",
                "gemini-3.9-flash-high",
                "flash",
                "grok-4.7-high",
                "unknown",
                "opencode/big-pickle",
            ]
            .into_iter()
            .map(move |m| json!([a, m]))
        })
        .collect();
    let input = json!({"catalog":catalog,"cached":cached,"cases":cases});
    let Some(expected): Option<Value> = crate::oracle::python(
        r#"
import importlib.util,json,sys,tempfile,types,pathlib
from datetime import datetime,timezone
s=importlib.util.spec_from_file_location('live',pathlib.Path('scripts/model_route_live.py')); m=importlib.util.module_from_spec(s); s.loader.exec_module(m)
d=json.load(sys.stdin)
class Clock(datetime):
 @classmethod
 def now(cls,tz=None): return cls(2026,10,23,tzinfo=timezone.utc)
m.datetime=Clock
with tempfile.TemporaryDirectory() as home:
 root=pathlib.Path(home); (root/'capabilities.json').write_text(json.dumps({a:{'observed_at':'2026-10-23T00:00:00Z','models':v} for a,v in d['cached'].items()}))
 m.model_route_probe=types.SimpleNamespace(_state_root=lambda:root)
 print(json.dumps([m._registered_match(a,x,d['catalog']) for a,x in d['cases']]))
"#,
        &input,
    ) else {
        return;
    };
    let actual: Vec<_> = cases
        .iter()
        .map(|x| {
            resolver
                .registered_match(x[0].as_str().unwrap(), x[1].as_str().unwrap())
                .unwrap()
        })
        .collect();
    assert_eq!(serde_json::to_value(actual).unwrap(), expected);
}

#[test]
fn first_default_and_first_equal_version_win_and_unknown_free_terms_stay_unknown() {
    let catalog = json!({"adapters":{"codex":{"models":[{"id":"gpt-8-sol","names":["sol"],"default":true},{"id":"gpt-9-sol","names":["sol"],"default":true}]}}});
    let cached = BTreeMap::new();
    let resolver = ModelCatalog {
        catalog: &catalog,
        cached: &cached,
    };
    let (entry, notes) = resolver.registered_match("codex", "sol").unwrap();
    assert_eq!(entry.unwrap()["id"], "gpt-8-sol");
    assert!(notes[0].contains("ambiguous"));
    assert_eq!(
        training_flag(&json!({"trains_on_prompts":false}), None, "opencode/free").unwrap(),
        None
    );
    assert_eq!(
        training_flag(
            &json!({"trains_on_prompts":false}),
            Some(&json!({"trains_on_prompts":false})),
            "opencode/free"
        )
        .unwrap(),
        Some(false)
    );
    assert!(no_effort_control(Some(&json!({"efforts":[]}))));
    assert!(!no_effort_control(None));
    assert!(matches!(
        resolver.registered_match("codex", "gpt-9999999999999999999999-sol"),
        Err(MatchError::NumberOutOfRange)
    ));
    assert_eq!(version("model-٢.٣").unwrap(), vec![2, 3]);
}
