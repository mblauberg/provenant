use super::*;
use serde_json::{Value, json};

#[test]
fn structured_and_plain_listings_match_python_without_accepting_error_text() {
    let raw = [
        "",
        "null",
        "garbage",
        "Available models:\ngpt-6\nclaude-opus-5\n\nSome footer text",
        "gpt-6\nerror text",
        "Models:\n* opencode/big-pickle  Default\n- opencode-go/kimi-k2\n\nFooter",
        "auto\n",
        "gpt-6\r\n\r\nFooter prose",
        "gpt-6\u{2028}gpt-6",
        r#"{"models":["gpt-6","gpt-6"]}"#,
        r#"{"data":{"z":{},"a":{}}}"#,
        r#"{"models":[{"id":null,"name":"gpt-6"}]}"#,
        r#"{"models":[{"model_id":"gpt-6"},{"slug":"claude-5"}]}"#,
        r#"[1,"gpt-6"]"#,
    ];
    let cases: Vec<_> = [
        None,
        Some("codex"),
        Some("kiro"),
        Some("cursor"),
        Some("opencode"),
    ]
    .into_iter()
    .flat_map(|a| raw.iter().map(move |s| json!([a, s])))
    .collect();
    let Some(expected): Option<Value> = crate::oracle::python(
        r#"
import json,pathlib,sys
sys.path.insert(0,str(pathlib.Path('scripts').resolve()))
import model_route_probe as m
print(json.dumps([m._listed_models(raw,adapter) for adapter,raw in json.load(sys.stdin)]))
"#,
        &json!(cases),
    ) else {
        return;
    };
    let actual: Vec<_> = cases
        .iter()
        .map(|c| listed_models(c[1].as_str().unwrap(), c[0].as_str()).unwrap())
        .collect();
    assert_eq!(json!(actual), expected);
}
