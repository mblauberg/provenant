use super::*;
use crate::tools::ToolCall;
use std::{fs, path::Path};

fn snapshot(path: &Path, actual: &str, update: bool) {
    if update {
        assert!(fs::write(path, actual).is_ok());
    } else {
        let expected = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tool-schemas.json"
        ));
        for (index, (actual_line, expected_line)) in actual
            .trim()
            .lines()
            .zip(expected.trim().lines())
            .enumerate()
        {
            assert_eq!(
                actual_line,
                expected_line,
                "schema snapshot line {}",
                index + 1
            );
        }
        assert_eq!(
            actual.trim().lines().count(),
            expected.trim().lines().count(),
            "schema snapshot line count"
        );
    }
}

#[test]
fn generated_schema_snapshot_is_checked_in() {
    let actual_result = serde_json::to_string_pretty(&catalogue());
    assert!(actual_result.is_ok());
    let actual = actual_result.unwrap_or_default();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tool-schemas.json");
    snapshot(&path, &actual, false);
}

#[test]
#[ignore = "run explicitly to refresh the checked schema snapshot"]
fn update_schema_snapshot() {
    let actual_result = serde_json::to_string_pretty(&catalogue());
    assert!(actual_result.is_ok());
    let actual = actual_result.unwrap_or_default();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tool-schemas.json");
    snapshot(&path, &actual, true);
}

#[test]
fn every_catalogue_example_is_accepted_by_the_tagged_serde_input() {
    for (name, arguments) in examples() {
        assert!(
            ToolCall::parse(name, arguments).is_ok(),
            "schema example for {name} did not deserialise"
        );
    }
}

#[test]
fn catalogue_examples_reject_unknown_fields_and_unknown_tools() {
    for (name, mut arguments) in examples() {
        let object = arguments.as_object_mut();
        assert!(object.is_some(), "all tool inputs are objects");
        if let Some(object) = object {
            object.insert("unexpected".into(), Value::Bool(true));
        }
        assert!(
            ToolCall::parse(name, arguments).is_err(),
            "{name} accepted an unknown field"
        );
    }
    assert!(ToolCall::parse("fabric_not_a_tool", serde_json::json!({"op":"inspect"})).is_err());
}

#[test]
fn serde_inputs_reject_wrong_types_and_nested_unknown_fields() {
    for (name, arguments) in [
        (
            "fabric_mail",
            serde_json::json!({"op":"send","recipients":"one","kind":"note","body":"x","artifacts":[]}),
        ),
        (
            "fabric_events",
            serde_json::json!({"op":"note","detail":"legacy field"}),
        ),
        (
            "fabric_task",
            serde_json::json!({"op":"update","proof":{"task":"00000000000000000000000000000001","generation":1,"token":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"extra":true},"objective":"x"}),
        ),
        (
            "fabric_identity",
            serde_json::json!({"op":"inspect","extra":true}),
        ),
        (
            "fabric_claim",
            serde_json::json!({"op":"acquire","scope":{"kind":"issue","key":"PROV-1"},"lease_ms":"60000"}),
        ),
    ] {
        assert!(
            ToolCall::parse(name, arguments).is_err(),
            "{name} accepted malformed input"
        );
    }
}

#[test]
fn schema_constraints_match_request_validation_bounds() {
    fn field_schema(tool_name: &str, operation: &str, field_name: &str) -> Value {
        let tools = catalogue();
        let tool = tools.iter().find(|tool| tool["name"] == tool_name);
        assert!(tool.is_some());
        let Some(tool) = tool else {
            return Value::Null;
        };
        let variants = tool["inputSchema"]["oneOf"].as_array();
        assert!(variants.is_some());
        let Some(variants) = variants else {
            return Value::Null;
        };
        let variant = variants
            .iter()
            .find(|variant| variant["properties"]["op"]["const"] == operation);
        assert!(variant.is_some());
        let Some(variant) = variant else {
            return Value::Null;
        };
        variant["properties"][field_name].clone()
    }

    let lease = field_schema("fabric_claim", "acquire", "lease_ms");
    assert_eq!(lease["minimum"], 1_000);
    assert_eq!(lease["maximum"], 3_600_000);
    let concurrency = field_schema("fabric_submit", "submission", "concurrency");
    assert_eq!(concurrency["minimum"], 1);
    assert_eq!(concurrency["maximum"], 100);
    let body = field_schema("fabric_mail", "send", "body");
    assert_eq!(body["minLength"], 1);
    assert_eq!(body["maxLength"], 65_536);
    assert!(body["pattern"].as_str().is_some());
    let recipients = field_schema("fabric_mail", "send", "recipients");
    assert_eq!(recipients["uniqueItems"], true);
    let host_limit = field_schema("fabric_hosts", "list", "limit");
    assert_eq!(host_limit["minimum"], 1);
    assert_eq!(host_limit["maximum"], 100);
}

#[test]
fn schema_catalogue_is_fixed_order_and_has_no_legacy_aliases() {
    let values = catalogue();
    let names: Vec<_> = values
        .iter()
        .filter_map(|value| value.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, crate::tools::TOOL_NAMES);
    let serialised_result = serde_json::to_string(&values);
    assert!(serialised_result.is_ok());
    let serialised = serialised_result.unwrap_or_default();
    assert!(!serialised.contains("\"detail\""));
}

#[test]
fn text_byte_limit_is_enforced_even_when_codepoint_length_is_smaller() {
    let at_limit = "😀".repeat(16_384);
    assert_eq!(at_limit.len(), 65_536);
    assert!(
        ToolCall::parse(
            "fabric_events",
            serde_json::json!({"op":"note","note":at_limit})
        )
        .is_ok()
    );
    let over_limit = "😀".repeat(16_385);
    assert!(
        ToolCall::parse(
            "fabric_events",
            serde_json::json!({"op":"note","note":over_limit})
        )
        .is_err()
    );
}
