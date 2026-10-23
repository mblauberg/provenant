//! Compatibility for the pre-upgrade `succeeded` receipt status.
use serde_json::Value;

/// Canonicalise only the legacy success spelling; preserve every other value.
pub fn canonical_success_status(status: Value) -> Value {
    if status == Value::String("succeeded".into()) {
        Value::String("ok".into())
    } else {
        status
    }
}

pub fn is_success_status(status: &Value) -> bool {
    matches!(status, Value::String(value) if value == "ok" || value == "succeeded")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_legacy_spelling_is_rewritten() {
        for (input, expected) in [
            (json!("succeeded"), json!("ok")),
            (json!("ok"), json!("ok")),
            (json!("failed"), json!("failed")),
            (json!(null), json!(null)),
            (json!(1), json!(1)),
        ] {
            assert_eq!(canonical_success_status(input), expected);
        }
        assert!(is_success_status(&json!("ok")));
        assert!(is_success_status(&json!("succeeded")));
        assert!(!is_success_status(&json!("failed")));
        assert!(!is_success_status(&json!(null)));
    }

    #[test]
    fn node_differential_matches_source_alias_policy() {
        let script = r#"import { canonicalSuccessStatus, isSuccessStatus } from './src/success-status.ts';
const values = JSON.parse(process.argv.at(-1));
console.log(JSON.stringify(values.map(value => [canonicalSuccessStatus(value), isSuccessStatus(value)])));"#;
        let fixtures = json!(["succeeded", "ok", "failed", null, 0, false, {}, []]);
        let Some(actual) = crate::test_support::node(script, &[fixtures.to_string()]) else {
            return;
        };
        let expected: Vec<_> = fixtures
            .as_array()
            .unwrap()
            .iter()
            .map(|value| {
                json!([
                    canonical_success_status(value.clone()),
                    is_success_status(value)
                ])
            })
            .collect();
        assert_eq!(actual, json!(expected));
    }
}
