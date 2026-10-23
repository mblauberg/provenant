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
        use std::process::Command;

        let node = std::env::var_os("PROVENANT_PORT_NODE").unwrap_or_else(|| "node".into());
        let script = r#"import { canonicalSuccessStatus, isSuccessStatus } from './src/success-status.ts';
const values = JSON.parse(process.argv.at(-1));
console.log(JSON.stringify(values.map(value => [canonicalSuccessStatus(value), isSuccessStatus(value)])));"#;
        let fixtures = json!(["succeeded", "ok", "failed", null, 0, false, {}, []]);
        let output = Command::new(node)
            .args([
                "--experimental-strip-types",
                "--input-type=module",
                "-e",
                script,
                "--",
            ])
            .arg(fixtures.to_string())
            .current_dir(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../../runtime/fabric"
            ))
            .output();
        let Ok(output) = output else {
            if std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() == Ok("1") {
                panic!("PROVENANT_PORT_NODE is required but unavailable");
            }
            eprintln!("skip: Node differential interpreter unavailable");
            return;
        };
        if !output.status.success() {
            if std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() == Ok("1") {
                panic!(
                    "Node differential failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            eprintln!("skip: Node does not support source type stripping");
            return;
        }
        let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
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
