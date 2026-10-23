//! Observer-cycle messages and retained summary. No process control is exposed.
use crate::lane_exit::Member;
pub use crate::pending::OrderedMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObserverPlan {
    pub adapter: String,
    pub model: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ObserverStream {
    #[serde(rename = "out")]
    Out,
    #[serde(rename = "err")]
    Err,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObserverLine {
    pub s: ObserverStream,
    pub line: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ObserverReply {
    Terminal { terminal: TerminalTrue },
    Stop { stop: SemanticStop },
}
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalTrue;
impl Serialize for TerminalTrue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(true)
    }
}
impl<'de> Deserialize<'de> for TerminalTrue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if bool::deserialize(deserializer)? {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(
                "observer terminal event must be true",
            ))
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SemanticStop(String);
#[derive(Debug, thiserror::Error)]
#[error("silence cannot request a stop")]
pub struct InvalidSemanticStop;
impl SemanticStop {
    pub fn new(value: String) -> Result<Self, InvalidSemanticStop> {
        if value == "stalled" {
            Err(InvalidSemanticStop)
        } else {
            Ok(Self(value))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for SemanticStop {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextUsage {
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub context_tokens: Option<u64>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub input_tokens: Option<u64>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub output_tokens: Option<u64>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub cached_input_tokens: Option<u64>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub cache_write_tokens: Option<u64>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub reasoning_tokens: Option<u64>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub context_window_tokens: Option<u64>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub context_percent: Option<f64>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub source: Option<String>,
}
impl ContextUsage {
    /// Untrusted wire records enter through the checked integer boundary.
    pub fn from_json(raw: &[u8]) -> Result<Self, provenant_records::checked_json::DecodeError> {
        crate::pending::checked_ordered_decode(raw)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelUsage {
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub init: Option<String>,
    pub answered: Vec<String>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub r#final: Option<String>,
    pub usage: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelChange {
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub from: Option<String>,
    pub to: String,
}
pub type SemanticFailure = crate::pending::OrderedRecord<SemanticFailureFields>;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticFailureFields {
    pub status: String,
    pub signature: String,
    pub excerpt: String,
    pub reset_at: Value,
    pub retry_after: Value,
    #[serde(flatten)]
    pub extra: crate::pending::OrderedMap<crate::pending::OrderedValue>,
}
pub type ObserverSummary = crate::pending::OrderedRecord<ObserverSummaryFields>;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObserverSummaryFields {
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub session_id: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub observed_model: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub reset_at: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub retry_after: Member<Value>,
    pub terminal_event_seen: bool,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub live_failure: Option<SemanticFailure>,
    pub semantic_failures: OrderedMap<SemanticFailure>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub retry_failure: Option<SemanticFailure>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub terminal_text: Option<String>,
    pub text_chunks: Vec<String>,
    pub text_truncated: bool,
    pub model_changes: Vec<ModelChange>,
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub last_model: Option<String>,
    pub context: ContextUsage,
    pub models: ModelUsage,
}

impl ObserverSummary {
    pub fn from_json(raw: &[u8]) -> Result<Self, provenant_records::checked_json::DecodeError> {
        Self::decode(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn token_fields_refuse_negative_and_fractional_usage() {
        let raw = r#"{"context_tokens":null,"input_tokens":TOKEN,"output_tokens":null,"cached_input_tokens":null,"cache_write_tokens":null,"reasoning_tokens":null,"context_window_tokens":null,"context_percent":null,"source":null}"#;
        for token in ["-1", "1.5", "true"] {
            assert!(serde_json::from_str::<ContextUsage>(&raw.replace("TOKEN", token)).is_err());
        }
        let typed = ContextUsage::from_json(raw.replace("TOKEN", "-0").as_bytes()).unwrap();
        assert_eq!(typed.input_tokens, Some(0));
        assert!(
            serde_json::to_string(&typed)
                .unwrap()
                .contains("\"input_tokens\":0,")
        );
    }

    #[test]
    fn context_integer_overflow_is_typed_before_serde_can_round() {
        let raw = br#"{"input_tokens":1180591620717411303424}"#;
        assert!(matches!(
            ContextUsage::from_json(raw),
            Err(provenant_records::checked_json::DecodeError::NumberOutOfRange(_))
        ));
    }

    #[test]
    fn real_observer_summaries_round_trip_without_invented_wire_tags() {
        let mut cases = vec![];
        for adapter in [
            "claude", "codex", "cursor", "opencode", "agy", "kiro", "copilot",
        ] {
            cases.push(json!({"plan":{"adapter":adapter,"model":"fixture"},"lines":[]}));
        }
        cases.push(json!({"plan":{"adapter":"claude","model":"fixture"},"lines":[{"type":"system","subtype":"init","session_id":"session-1","model":"fixture"},{"type":"assistant","message":{"model":"answered","content":[{"type":"text","text":"DONE"}],"usage":{"input_tokens":4,"output_tokens":2}}},{"type":"result","result":"DONE","is_error":false}]}));
        cases.push(json!({"plan":{"adapter":"codex","model":"fixture"},"lines":[{"type":"turn.completed","usage":{"input_tokens":9007199254740993_u64,"output_tokens":2,"cached_input_tokens":0}}]}));
        if let Some(expected) = crate::test_support::oracle("observer_contract_wire", &json!(cases))
        {
            for raw in expected.as_array().unwrap() {
                let wire = raw["wire"].as_str().unwrap();
                let typed = ObserverSummary::from_json(wire.as_bytes()).unwrap();
                assert_eq!(serde_json::to_string(&typed).unwrap(), wire);
            }
        }
        for raw in [json!({"terminal":true}), json!({"stop":"usage_limited"})] {
            let typed: ObserverReply = serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(
                serde_json::to_string(&typed).unwrap(),
                serde_json::to_string(&raw).unwrap()
            );
        }
        for wire in [r#"{"s":"out","line":"a"}"#, r#"{"s":"err","line":"b\n"}"#] {
            let typed: ObserverLine = serde_json::from_str(wire).unwrap();
            assert_eq!(serde_json::to_string(&typed).unwrap(), wire);
        }
    }
    #[test]
    fn semantic_failure_order_survives_raw_source_record_round_trip() {
        let cases = json!([{"plan":{"adapter":"claude","model":"fixture"},"lines":[
            {"type":"result","is_error":true,"result":"usage limit"},
            {"type":"result","is_error":true,"result":"authentication required"}
        ]}]);
        if let Some(outputs) = crate::test_support::oracle("observer_contract_wire", &cases) {
            let wire = outputs[0]["wire"].as_str().unwrap();
            let typed = ObserverSummary::from_json(wire.as_bytes()).unwrap();
            let keys: Vec<_> = typed
                .semantic_failures
                .entries()
                .map(|(key, _)| key)
                .collect();
            assert_eq!(json!(keys), outputs[0]["failure_keys"]);
            assert_eq!(keys, vec!["usage_limited", "auth_required"]);
            let serialised = serde_json::to_string(&typed).unwrap();
            assert_eq!(serialised, wire);
            let again: ObserverSummary = serde_json::from_str(&serialised).unwrap();
            assert_eq!(again.semantic_failures, typed.semantic_failures);
        }
    }

    #[test]
    fn stderr_retry_and_huge_integer_summaries_match_source_wire() {
        let cases = json!([
            {"plan":{"adapter":"opencode","model":"m"},"lines":[],"diagnostics":[{"stream":"stderr","text":"retrying: weekly usage limit reached\n"}]},
            {"plan":{"adapter":"kiro","model":"m"},"lines":[],"diagnostics":[{"stream":"stderr","text":"Opening browser…\n"}]},
            {"plan":{"adapter":"claude","model":"m"},"lines":[{"type":"api_retry","error":{"message":"rate limit 429"}}]},
            {"plan":{"adapter":"codex","model":"m"},"lines":[{"type":"error","message":"Reconnecting... 1/5"}]},
            {"plan":{"adapter":"codex","model":"m"},"lines":[{"type":"error","message":"Reconnecting... 1/5"},{"type":"turn.completed"}]},
            {"plan":{"adapter":"claude","model":"m"},"lines":[{"type":"api_retry","error":{"message":"rate limit 429"}},{"type":"result","is_error":false,"result":"DONE","num_turns":1}]},
            {"plan":{"adapter":"kiro","model":"m"},"lines":[{"type":"metadata","data":{"contextUsagePercentage":42.0}}]}
        ]);
        if let Some(outputs) = crate::test_support::oracle("observer_contract_wire", &cases) {
            for (i, raw) in outputs.as_array().unwrap().iter().enumerate() {
                let wire = raw["wire"].as_str().unwrap();
                let typed = ObserverSummary::from_json(wire.as_bytes()).unwrap();
                assert_eq!(serde_json::to_string(&typed).unwrap(), wire);
                if i < 2 {
                    assert!(typed.live_failure.is_some());
                }
                if i == 2 {
                    assert!(typed.retry_failure.is_some());
                }
                if i == 4 || i == 5 {
                    assert!(typed.retry_failure.is_none());
                }
            }
        }
        // The source retains 2**70 exactly; R4 deliberately refuses it.
        // Keep the large integer as source text rather than passing it through Value.
        let cases = json!([{"plan":{"adapter":"codex","model":"m"},"lines":[],"raw_lines":["{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1180591620717411303424}}"]}]);
        if let Some(outputs) = crate::test_support::oracle("observer_contract_wire", &cases) {
            let wire = outputs[0]["wire"].as_str().unwrap();
            assert!(wire.contains("1180591620717411303424"));
            assert!(matches!(
                ObserverSummary::from_json(wire.as_bytes()),
                Err(provenant_records::checked_json::DecodeError::NumberOutOfRange(_))
            ));
        }
    }
    #[test]
    fn observer_notifications_cannot_invent_terminal_false_or_a_silence_stop() {
        for raw in [
            json!({"terminal":false}),
            json!({"stop":"stalled"}),
            json!({"terminal":true,"stop":"usage_limited"}),
        ] {
            assert!(serde_json::from_value::<ObserverReply>(raw).is_err());
        }
    }
}
