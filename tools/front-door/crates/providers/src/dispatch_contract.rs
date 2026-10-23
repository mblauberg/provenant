//! Typed dispatcher cycle contracts, from cf_dispatch Options/Attempt/emit/parse_route.
use crate::ordered_json::{self, Node};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchOptions {
    pub tool: String,
    pub model: String,
    pub effort: String,
    pub out: String,
    pub run_dir: String,
    pub prompt: String,
    pub prompt_file: String,
    pub original_prompt_file: String,
    pub chain: String,
    pub orchestrator_family: String,
    pub model_alias: String,
    pub task_class: String,
    pub route_role: String,
    pub risk_tier: String,
    pub model_override_tier: String,
    pub reviewer_id: String,
    pub intent: String,
    pub sandbox: String,
    pub network: String,
    pub capabilities: String,
    pub account: String,
    pub resume_session: String,
    pub provider_cwd: String,
    pub fallback: String,
    pub access_mode: String,
    pub timeout_seconds: String,
    pub worktree: String,
    pub doctor: bool,
    pub plan_only: bool,
    pub preface: bool,
    pub alias_explicit: bool,
    pub add_dirs: Vec<String>,
    pub read_roots: Vec<String>,
}
impl Default for DispatchOptions {
    fn default() -> Self {
        Self {
            tool: "".into(),
            model: "".into(),
            effort: "".into(),
            out: "".into(),
            run_dir: "".into(),
            prompt: "".into(),
            prompt_file: "".into(),
            original_prompt_file: "".into(),
            chain: "".into(),
            orchestrator_family: "".into(),
            model_alias: "".into(),
            task_class: "".into(),
            route_role: "reviewer".into(),
            risk_tier: "".into(),
            model_override_tier: "".into(),
            reviewer_id: "".into(),
            intent: "assurance".into(),
            sandbox: "".into(),
            network: "".into(),
            capabilities: "".into(),
            account: "".into(),
            resume_session: "".into(),
            provider_cwd: "".into(),
            fallback: "".into(),
            access_mode: "read_only".into(),
            timeout_seconds: "".into(),
            worktree: "".into(),
            doctor: false,
            plan_only: false,
            preface: true,
            alias_explicit: false,
            add_dirs: Vec::new(),
            read_roots: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchAttempt {
    pub tool: String,
    pub model: String,
    pub effort: String,
    pub status: String,
    pub guarantee: String,
    pub rc: i64,
    pub alias: String,
    pub route_alias: String,
    pub family: String,
    pub endpoint: String,
    pub identity: String,
    pub requested_model: String,
    pub requested_effort: String,
    pub effort_source: String,
    pub effort_capability_source: String,
    pub effort_substitution: String,
    pub substitution: String,
    pub fallback_model: String,
    pub catalog_model: String,
    pub model_selection: String,
    pub policy_override: String,
    pub route_model_override_tier: String,
    pub route_reason: String,
}
impl DispatchAttempt {
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            model: "".into(),
            effort: "".into(),
            status: "".into(),
            guarantee: "".into(),
            rc: 1,
            alias: "".into(),
            route_alias: "".into(),
            family: "".into(),
            endpoint: "".into(),
            identity: "".into(),
            requested_model: "".into(),
            requested_effort: "".into(),
            effort_source: "".into(),
            effort_capability_source: "".into(),
            effort_substitution: "".into(),
            substitution: "".into(),
            fallback_model: "".into(),
            catalog_model: "".into(),
            model_selection: "".into(),
            policy_override: "".into(),
            route_model_override_tier: "".into(),
            route_reason: "".into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteFields {
    pub status: String,
    pub resolved_model: String,
    pub model_family: String,
    pub endpoint_provider: String,
    pub identity_source: String,
    pub requested_effort: String,
    pub effort: String,
    pub effort_source: String,
    pub effort_capability_source: String,
    pub effort_substitution: String,
    pub substitution: String,
    pub fallback_model: String,
    pub catalog_model: String,
    pub model_selection: String,
    pub model_override_tier: String,
    pub policy_override: String,
    pub alias: String,
    pub reason: String,
    pub endpoint_profile: String,
    pub endpoint_base_url: String,
    pub endpoint_token_env: String,
    pub endpoint_wire_api: String,
}
pub const ROUTE_FIELDS: &[&str] = &[
    "status",
    "resolved_model",
    "model_family",
    "endpoint_provider",
    "identity_source",
    "requested_effort",
    "effort",
    "effort_source",
    "effort_capability_source",
    "effort_substitution",
    "substitution",
    "fallback_model",
    "catalog_model",
    "model_selection",
    "model_override_tier",
    "policy_override",
    "alias",
    "reason",
    "endpoint_profile",
    "endpoint_base_url",
    "endpoint_token_env",
    "endpoint_wire_api",
];
pub const REQUIRED_FIELDS: &[&str] = &[
    "resolved_model",
    "model_family",
    "endpoint_provider",
    "identity_source",
];
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchReceipt {
    pub tool: String,
    pub adapter: String,
    pub adapter_gate: String,
    pub execution_intent: String,
    pub model: String,
    pub requested_model: String,
    pub resolved_model: String,
    pub fallback_model: String,
    pub requested_effort: String,
    pub effort: String,
    pub effort_source: String,
    pub effort_capability_source: String,
    pub effort_substitution: String,
    pub substitution: String,
    pub status: String,
    pub reason: String,
    #[serde(rename = "exit")]
    pub exit_code: i64,
    pub output_path: String,
    pub output_digest: String,
    pub read_only_guarantee: String,
    pub provider_sandbox: (),
    pub provider_network: (),
    pub access_mode: String,
    pub worktree: String,
    pub orchestrator_family: String,
    pub provider_family: String,
    pub model_family: String,
    pub endpoint_provider: String,
    pub identity_source: String,
    pub catalog_model: String,
    pub model_selection: String,
    pub route_alias: String,
    pub reviewer_id: String,
    pub risk_tier: String,
    pub model_override_tier: String,
    pub policy_override: String,
    pub cross_family: bool,
    pub certification_eligible: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    #[error(transparent)]
    Decode(#[from] crate::DecodeError),
    #[error("duplicate route member: {0}")]
    Duplicate(String),
    #[error("route must be a JSON object")]
    NotObject,
    #[error("successful route requires non-empty {0}")]
    Required(&'static str),
    #[error("route field {0} must be a NUL-free string")]
    InvalidField(&'static str),
}
fn duplicate(node: &Node) -> Option<String> {
    match node {
        Node::Object(pairs) => {
            for (_, v) in pairs {
                if let Some(k) = duplicate(v) {
                    return Some(k);
                }
            }
            let mut seen = std::collections::HashSet::new();
            for (k, _) in pairs {
                if !seen.insert(k) {
                    return Some(k.clone());
                }
            }
            None
        }
        Node::Array(items) => items.iter().find_map(duplicate),
        _ => None,
    }
}
pub fn parse_route(text: &str) -> Result<RouteFields, RouteError> {
    let node = ordered_json::parse(text)?;
    if let Some(k) = duplicate(&node) {
        return Err(RouteError::Duplicate(k));
    }
    if !node.is_object() {
        return Err(RouteError::NotObject);
    }
    if node.get("status").and_then(Node::string) == Some("ok") {
        for &key in REQUIRED_FIELDS {
            if node
                .get(key)
                .and_then(Node::string)
                .is_none_or(|s| provenant_core::py::python_strip(s).is_empty())
            {
                return Err(RouteError::Required(key));
            }
        }
    }
    let mut fields = serde_json::Map::new();
    for &key in ROUTE_FIELDS {
        let value = match node.get(key) {
            None => "",
            Some(n) if n.is_null() => "",
            Some(n) => n.string().ok_or(RouteError::InvalidField(key))?,
        };
        if value.contains('\0') {
            return Err(RouteError::InvalidField(key));
        }
        fields.insert(key.into(), Value::String(value.into()));
    }
    serde_json::from_value(Value::Object(fields))
        .map_err(|e| RouteError::Decode(crate::DecodeError::InvalidJson(e)))
}

impl DispatchReceipt {
    /// The dispatcher prints a compact, ASCII-escaped receipt and a final newline.
    pub fn encode_line(&self) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(self)?;
        let mut out = String::new();
        for ch in json.chars() {
            if ch.is_ascii() && ch != '\u{7f}' {
                out.push(ch);
            } else {
                let mut buffer = [0; 2];
                for unit in ch.encode_utf16(&mut buffer) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
        out.push('\n');
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dispatcher_record_defaults_and_route_failures_match_python() {
        type Oracle = (Value, Value, Vec<(String, Value)>);
        let Some((options, attempt, cases)): Option<Oracle> = crate::oracle::python(
            r#"
import json,sys,dataclasses
from skills.orchestrate.scripts import cf_dispatch as m
inputs=['{}','[]','null','{"status":"ok"}','{"status":"failed","reason":null}', '{"status":"failed","model_family":2}', '{"status":"failed","reason":"bad\\u0000"}', '{"x":1,"x":2}', '{"extra":{"x":1,"x":2}}', '{"status":"ok","resolved_model":"m","model_family":"f","endpoint_provider":"e","identity_source":"i","extra":2}']
cases=[]
for text in inputs:
 try: result=m.parse_route(text)
 except ValueError as e: result={'error':str(e)}
 cases.append([text,result])
print(json.dumps([dataclasses.asdict(m.Options()),dataclasses.asdict(m.Attempt('agy')),cases]))
"#,
            &Value::Null,
            None,
        ) else {
            return;
        };
        assert_eq!(
            serde_json::to_value(DispatchOptions::default()).unwrap(),
            options
        );
        assert_eq!(
            serde_json::to_value(DispatchAttempt::new("agy")).unwrap(),
            attempt
        );
        for (text, expected) in cases {
            let actual = match parse_route(&text) {
                Ok(r) => serde_json::to_value(r).unwrap(),
                Err(e) => serde_json::json!({"error":e.to_string()}),
            };
            assert_eq!(actual, expected, "{text}");
        }
    }
    #[test]
    fn dispatch_receipt_wire_order_ascii_and_newline_match_source() {
        let Some(lines): Option<Vec<String>> = crate::oracle::python(
            r#"
import json,sys
from skills.orchestrate.scripts.cf_dispatch import Dispatcher,Options,Attempt
lines=[]
for full in [False,True]:
 for status in ['ok','failed','rejected']:
  d=Dispatcher(Options(intent='assurance',reviewer_id='reviewer🦀',orchestrator_family='openai'))
  a=Attempt('agy',model='fixture',family='google',route_reason='reason\x7f🦀',catalog_model='catalog',requested_effort='high')
  line,ok=d.emit(a,status,3,'','prompt_only',full)
  lines.append(line)
print(json.dumps(lines))
"#,
            &Value::Null,
            None,
        ) else {
            return;
        };
        for line in lines {
            let receipt: DispatchReceipt = crate::decode_json(line.as_bytes()).unwrap();
            assert_eq!(receipt.encode_line().unwrap(), line);
        }
    }
}
