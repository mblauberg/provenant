//! Observer-cycle messages and retained summary. No process control is exposed.
use crate::lane_exit::Member;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;

#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct ObserverPlan { pub adapter:String,pub model:String }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub enum ObserverStream { #[serde(rename="out")] Out, #[serde(rename="err")] Err }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct ObserverLine { pub s:ObserverStream,pub line:String }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum ObserverReply { Terminal {terminal:bool}, Stop {stop:String} }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct ContextUsage {
    pub context_tokens:Option<Number>,pub input_tokens:Option<Number>,pub output_tokens:Option<Number>,
    pub cached_input_tokens:Option<Number>,pub cache_write_tokens:Option<Number>,pub reasoning_tokens:Option<Number>,
    pub context_window_tokens:Option<Number>,pub context_percent:Option<Number>,pub source:Option<String>,
}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct ModelUsage { pub init:Option<String>,pub answered:Vec<String>,pub r#final:Option<String>,pub usage:Vec<String> }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct ModelChange { pub from:Option<String>,pub to:String }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct SemanticFailure {
    pub status:String,pub signature:String,pub excerpt:String,pub reset_at:Value,pub retry_after:Value,
    #[serde(flatten)] pub extra:Map<String,Value>,
}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct ObserverSummary {
    #[serde(default,skip_serializing_if="Member::is_absent")] pub session_id:Member<Value>,
    #[serde(default,skip_serializing_if="Member::is_absent")] pub observed_model:Member<Value>,
    #[serde(default,skip_serializing_if="Member::is_absent")] pub reset_at:Member<Value>,
    #[serde(default,skip_serializing_if="Member::is_absent")] pub retry_after:Member<Value>,
    pub terminal_event_seen:bool,
    pub live_failure:Option<SemanticFailure>,
    pub semantic_failures:BTreeMap<String,SemanticFailure>,
    pub retry_failure:Option<SemanticFailure>,
    pub terminal_text:Option<String>,pub text_chunks:Vec<String>,pub text_truncated:bool,
    pub model_changes:Vec<ModelChange>,pub last_model:Option<String>,pub context:ContextUsage,pub models:ModelUsage,
}

#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    #[test]
    fn real_observer_summaries_round_trip_without_invented_wire_tags() {
        let mut cases=vec![];
        for adapter in ["claude","codex","cursor","opencode","agy","kiro","copilot"] {
            cases.push(json!({"plan":{"adapter":adapter,"model":"fixture"},"lines":[]}));
        }
        cases.push(json!({"plan":{"adapter":"claude","model":"fixture"},"lines":[{"type":"system","subtype":"init","session_id":"session-1","model":"fixture"},{"type":"assistant","message":{"model":"answered","content":[{"type":"text","text":"DONE"}],"usage":{"input_tokens":4,"output_tokens":2}}},{"type":"result","result":"DONE","is_error":false}]}));
        cases.push(json!({"plan":{"adapter":"codex","model":"fixture"},"lines":[{"type":"turn.completed","usage":{"input_tokens":9007199254740993_u64,"output_tokens":2,"cached_input_tokens":0}}]}));
        if let Some(expected)=crate::test_support::oracle("observer_contract",&json!(cases)) {
            for raw in expected.as_array().unwrap() {
                let typed:ObserverSummary=serde_json::from_value(raw.clone()).unwrap();
                assert_eq!(serde_json::to_value(typed).unwrap(),*raw);
            }
        }
        for raw in [json!({"terminal":true}),json!({"stop":"usage_limited"})] {
            let typed:ObserverReply=serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(serde_json::to_value(typed).unwrap(),raw);
        }
        for raw in [json!({"s":"out","line":"a"}),json!({"s":"err","line":"b\n"})] {
            let typed:ObserverLine=serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(serde_json::to_value(typed).unwrap(),raw);
        }
    }
}
