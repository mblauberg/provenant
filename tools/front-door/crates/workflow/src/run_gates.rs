//! Approval predicates over receipt gate fields. Shape validation is separate.
use serde_json::Value;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GateError {
    #[error("cannot use '{0}' as a set element (unhashable type: '{0}')")]
    UnhashableStatus(&'static str),
}

pub fn gate_approved(run: &Value, path: &[&str]) -> bool {
    let mut value = run;
    for key in path {
        let Some(next) = value.as_object().and_then(|o| o.get(*key)) else {
            return false;
        };
        value = next;
    }
    value
        .as_object()
        .and_then(|o| o.get("status"))
        .is_some_and(|s| s == "approved")
}
pub fn release_approved(run: &Value) -> bool {
    gate_approved(run, &["human_gates", "release"])
}

pub fn run_closed(run: &Value) -> Result<bool, GateError> {
    let status = run
        .get("observation")
        .and_then(Value::as_object)
        .and_then(|o| o.get("status"));
    // The source computes settled before consulting release approval, including
    // its TypeError for an unhashable JSON status. Do not turn it into success.
    match status {
        Some(Value::Array(_)) => return Err(GateError::UnhashableStatus("list")),
        Some(Value::Object(_)) => return Err(GateError::UnhashableStatus("dict")),
        _ => {}
    }
    let settled = status
        .and_then(Value::as_str)
        .is_some_and(|s| ["pass", "not_applicable"].contains(&s));
    Ok(release_approved(run) && settled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn release_and_settled_observation_are_both_required() {
        for status in ["pass", "not_applicable"] {
            assert!(run_closed(&json!({"human_gates":{"release":{"status":"approved"}},"observation":{"status":status}})).unwrap());
        }
        assert!(!run_closed(&json!({"human_gates":{"release":{"status":"approved"}},"observation":{"status":"pending"}})).unwrap());
        assert!(!run_closed(
            &json!({"human_gates":{"release":{"status":"pending"}},"observation":{"status":"pass"}})
        )
        .unwrap());
        assert!(gate_approved(&json!({"status":"approved"}), &[]));
        assert!(!gate_approved(&json!({"x":null}), &["x", "status"]));
        assert_eq!(
            run_closed(&json!({"observation":{"status":[]}})),
            Err(GateError::UnhashableStatus("list"))
        );
    }
    #[test]
    fn malformed_missing_and_unknown_gate_fields_match_python() {
        let mut cases = vec![
            json!({}),
            json!({"human_gates":null}),
            json!({"human_gates":[]}),
            json!({"human_gates":{"release":true}}),
        ];
        for release in [
            json!("approved"),
            json!("pass"),
            json!("Approved"),
            Value::Null,
            json!(true),
            json!(4),
        ] {
            for observation in [
                json!("pass"),
                json!("not_applicable"),
                json!("pending"),
                json!([]),
                Value::Null,
                json!({}),
                json!("approved"),
            ] {
                cases.push(json!({"human_gates":{"release":{"status":release}},"observation":{"status":observation}}));
            }
        }
        let actual: Vec<_> = cases
            .iter()
            .map(|c| match run_closed(c) {
                Ok(closed) => json!([
                    gate_approved(c, &["human_gates", "release"]),
                    release_approved(c),
                    closed
                ]),
                Err(GateError::UnhashableStatus(kind)) => crate::test_support::unhashable(kind),
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle("run_gates", &json!(cases)) {
            assert_eq!(json!(actual), expected);
        }
    }
}
