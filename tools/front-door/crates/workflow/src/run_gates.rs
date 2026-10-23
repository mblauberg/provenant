//! Approval predicates over receipt gate fields. Shape validation is separate.
use serde_json::Value;

pub fn gate_approved(run: &Value, path: &[&str]) -> bool {
    let mut value = run;
    for key in path {
        let Some(next) = value.as_object().and_then(|o| o.get(*key)) else { return false; };
        value = next;
    }
    value.as_object().and_then(|o| o.get("status")).is_some_and(|s| s == "approved")
}
pub fn release_approved(run: &Value) -> bool {
    gate_approved(run, &["human_gates", "release"])
}
pub fn run_closed(run: &Value) -> bool {
    let settled = run.get("observation").and_then(Value::as_object).and_then(|o| o.get("status"))
        .and_then(Value::as_str).is_some_and(|s| ["pass", "not_applicable"].contains(&s));
    release_approved(run) && settled
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn release_and_settled_observation_are_both_required() {
        for status in ["pass", "not_applicable"] {
            assert!(run_closed(&json!({"human_gates":{"release":{"status":"approved"}},"observation":{"status":status}})));
        }
        assert!(!run_closed(&json!({"human_gates":{"release":{"status":"approved"}},"observation":{"status":"pending"}})));
        assert!(!run_closed(&json!({"human_gates":{"release":{"status":"pending"}},"observation":{"status":"pass"}})));
        assert!(gate_approved(&json!({"status":"approved"}), &[]));
        assert!(!gate_approved(&json!({"x":null}), &["x", "status"]));
    }
    #[test]
    fn malformed_missing_and_unknown_gate_fields_match_python() {
        let mut cases = vec![json!({}), json!({"human_gates":null}), json!({"human_gates":[]}), json!({"human_gates":{"release":true}})];
        for release in [json!("approved"), json!("pass"), json!("Approved"), Value::Null, json!(true), json!(4)] {
            for observation in [json!("pass"), json!("not_applicable"), json!("pending"), json!([]), Value::Null, json!({}), json!("approved")] {
                cases.push(json!({"human_gates":{"release":{"status":release}},"observation":{"status":observation}}));
            }
        }
        let actual: Vec<_> = cases.iter().map(|c| [gate_approved(c, &["human_gates","release"]), release_approved(c), run_closed(c)]).collect();
        if let Some(expected) = crate::test_support::oracle("run_gates", &json!(cases)) { assert_eq!(json!(actual), expected); }
    }
}
