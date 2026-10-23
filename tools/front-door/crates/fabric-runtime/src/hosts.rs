//! Federation placement policy. Transport and group cancellation remain with supervisor.
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const HOST_STDOUT_LIMIT: usize = 4 * 1024 * 1024;
pub const HOST_STDERR_LIMIT: usize = 4096;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaneRequest {
    pub action: String,
    pub cwd: String,
    pub input: Map<String, Value>,
}

/// Environment and config existence are explicit inputs, so no global mutable
/// state or filesystem probing is needed to decide whether federation applies.
pub fn needs_host_owner(local_only: bool, config_exists: bool, input: &Map<String, Value>) -> bool {
    if local_only {
        return false;
    }
    if config_exists || input.contains_key("host") {
        return true;
    }
    ["id", "resume", "handoff", "session"]
        .iter()
        .filter_map(|k| input.get(*k))
        .chain(
            input
                .get("ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
        .any(|v| v.as_str().is_some_and(|s| s.contains('@')))
}

pub fn needs_operation_id(action: &str, input: &Map<String, Value>) -> bool {
    matches!(action, "dispatch" | "resume" | "handoff") && !input.contains_key("operation_id")
}

/// Apply the owner's local-placement rewrite, or retain the remote response.
/// Input is mutated only after a local response, apart from the operation ID
/// allocated by the caller before transport.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("non-object local host rewrites await the JavaScript-compatible response codec")]
pub struct UnsupportedRewrite;

pub fn apply_lane_response(
    input: &mut Map<String, Value>,
    result: Map<String, Value>,
) -> Result<Option<Map<String, Value>>, UnsupportedRewrite> {
    if result
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
        == Some("hosts_cancelled")
    {
        let mut refusal = json!({"status":"rejected", "error":"hosts_cancelled",
            "fix":"Retry with the same --operation-id to resume or settle the interrupted transfer."}).as_object().cloned().unwrap_or_default();
        if let Some(id) = input.get("operation_id") {
            refusal.insert("operation_id".into(), id.clone());
        }
        return Ok(Some(refusal));
    }
    if result.get("local").is_some_and(truthy) {
        if let Some(rewrite) = result.get("input").filter(|v| truthy(v)) {
            let replacement = match rewrite {
                Value::Object(rewrite) => rewrite.clone(),
                Value::Array(values) => values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| (index.to_string(), value.clone()))
                    .collect(),
                Value::String(value) => {
                    // Object.assign enumerates UTF-16 code units. A lone
                    // surrogate needs a lossless JS value owner, not U+FFFD.
                    if value.chars().any(|c| u32::from(c) > 0xffff) {
                        return Err(UnsupportedRewrite);
                    }
                    value
                        .chars()
                        .enumerate()
                        .map(|(index, value)| (index.to_string(), Value::String(value.to_string())))
                        .collect()
                }
                _ => Map::new(),
            };
            *input = replacement;
        } else {
            input.remove("host");
            input.remove("operation_id");
        }
        if let Some(fallback) = result.get("fallback").filter(|v| truthy(v)) {
            input.insert("placement_fallback".into(), fallback.clone());
        }
        return Ok(None);
    }
    Ok(Some(result))
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.0),
        _ => true,
    }
}

/// Keep bytes, decoding only once complete, so split UTF-8 characters survive.
#[derive(Default)]
pub struct StderrTail(Vec<u8>);
impl StderrTail {
    pub fn push(&mut self, chunk: &[u8]) {
        if chunk.len() >= HOST_STDERR_LIMIT {
            self.0.clear();
            self.0
                .extend_from_slice(&chunk[chunk.len() - HOST_STDERR_LIMIT..]);
        } else {
            let drop = (self.0.len() + chunk.len()).saturating_sub(HOST_STDERR_LIMIT);
            self.0.drain(..drop);
            self.0.extend_from_slice(chunk);
        }
    }
    pub fn message(&self) -> String {
        let detail = String::from_utf8_lossy(&self.0);
        let detail = crate::pending::trim_ecmascript(&detail);
        if detail.is_empty() {
            "Host owner did not return a JSON result".into()
        } else {
            detail.into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn object(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }
    #[test]
    fn federation_is_bypassed_only_without_configuration_or_selectors() {
        assert!(!needs_host_owner(false, false, &Map::new()));
        assert!(!needs_host_owner(
            true,
            true,
            &object(json!({"host":"peer"}))
        ));
        for selector in [
            json!({"host":null}),
            json!({"ids":[1,"task@peer"]}),
            json!({"resume":"run@peer"}),
        ] {
            assert!(needs_host_owner(false, false, &object(selector)));
        }
        assert!(needs_host_owner(false, true, &Map::new()));
    }
    #[test]
    fn operation_id_is_allocated_only_for_new_transfer() {
        for action in ["dispatch", "resume", "handoff"] {
            assert!(needs_operation_id(action, &Map::new()));
            assert!(!needs_operation_id(
                action,
                &object(json!({"operation_id":null}))
            ));
        }
        assert!(!needs_operation_id("cancel", &Map::new()));
    }
    #[test]
    fn local_response_replaces_input_and_preserves_fallback() {
        let mut input = object(json!({"host":"peer","operation_id":"op","prompt":"before"}));
        assert!(apply_lane_response(
            &mut input,
            object(
                json!({"local":true,"input":{"prompt":"after"},"fallback":{"reason":"offline"}})
            )
        )
        .unwrap()
        .is_none());
        assert_eq!(
            input,
            object(json!({"prompt":"after","placement_fallback":{"reason":"offline"}}))
        );
        input.insert("operation_id".into(), json!("op"));
        assert!(
            apply_lane_response(&mut input, object(json!({"local":true})))
                .unwrap()
                .is_none()
        );
        assert!(!input.contains_key("operation_id"));
    }
    #[test]
    fn cancelled_response_retains_retry_identity_and_does_not_rewrite() {
        let mut input = object(json!({"operation_id":"op"}));
        let result = apply_lane_response(
            &mut input,
            object(json!({"error":{"code":"hosts_cancelled"}})),
        )
        .unwrap()
        .unwrap();
        assert_eq!(result["operation_id"], "op");
        assert_eq!(result["error"], "hosts_cancelled");
        assert_eq!(input["operation_id"], "op");
    }
    #[test]
    fn diagnostics_are_a_bounded_byte_tail() {
        let mut tail = StderrTail::default();
        assert_eq!(tail.message(), "Host owner did not return a JSON result");
        tail.push(&vec![b'x'; 5000]);
        tail.push(b"\xc3");
        tail.push(b"\xa9");
        assert_eq!(tail.0.len(), 4096);
        assert!(tail.message().ends_with("é"));
    }
}

#[cfg(test)]
mod incomplete_rewrite_tests {
    use super::*;
    #[test]
    fn incomplete_non_object_rewrite_is_explicit_and_leaves_input_intact() {
        {
            let rewrite = json!("🦀");
            let mut input = Map::from_iter([("prompt".into(), json!("retained"))]);
            let before = input.clone();
            assert_eq!(
                apply_lane_response(
                    &mut input,
                    json!({"local":true,"input":rewrite})
                        .as_object()
                        .unwrap()
                        .clone()
                ),
                Err(UnsupportedRewrite)
            );
            assert_eq!(input, before);
        }
    }
}

#[cfg(test)]
mod trim_tests {
    use super::*;
    #[test]
    fn stderr_trimming_matches_ecmascript_bom_and_non_whitespace() {
        let mut tail = StderrTail::default();
        tail.push("\u{feff}".as_bytes());
        assert_eq!(tail.message(), "Host owner did not return a JSON result");
        let mut tail = StderrTail::default();
        tail.push("\u{0085}".as_bytes());
        assert_eq!(tail.message(), "\u{0085}");
    }
    #[test]
    fn assign_rewrites_enumerate_arrays_and_strings_and_clear_primitive_inputs() {
        for (rewrite, expected) in [
            (json!("aé"), json!({"0":"a","1":"é"})),
            (json!([1, null]), json!({"0":1,"1":null})),
            (json!(1), json!({})),
            (json!(true), json!({})),
        ] {
            let mut input = json!({"old":1}).as_object().unwrap().clone();
            apply_lane_response(
                &mut input,
                json!({"local":true,"input":rewrite})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
            assert_eq!(Value::Object(input), expected);
        }
    }
}
