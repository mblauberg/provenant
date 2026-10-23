use super::Mode;
use super::Plan;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum AdapterError {
    #[error("CF_DISPATCH_CODEX_CONFIG must be clean or inherited")]
    InvalidCodexConfig,
    #[error("Codex MCP transport is not an object")]
    MalformedTransport,
    #[error("Codex MCP transport type is not hashable")]
    MalformedTransportType,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigDecision {
    pub config: String,
    pub args: Vec<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Inputs {
    pub intent_assurance: bool,
    pub capabilities: bool,
    pub choice: String,
}

/// JSON scalars are valid TOML scalars; serde's compact string encoder also
/// leaves non-BMP characters as Unicode instead of producing surrogate escapes.
pub fn toml_scalar(value: &Value) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

/// Whether an MCP server transport starts a local process or enables command
/// execution. The set of data-only HTTP fields mirrors Codex's input contract.
pub fn transport_executes(transport: &Value) -> Result<bool, AdapterError> {
    const INERT: &[&str] = &[
        "type",
        "url",
        "bearer_token_env_var",
        "http_headers",
        "env_http_headers",
    ];
    let fields = transport
        .as_object()
        .ok_or(AdapterError::MalformedTransport)?;
    let kind = fields.get("type");
    if kind.is_some_and(|value| value.is_array() || value.is_object()) {
        return Err(AdapterError::MalformedTransportType);
    }
    Ok(match kind.and_then(Value::as_str) {
        Some("stdio") => true,
        Some("streamable_http") => fields.iter().any(|(key, value)| {
            !INERT.contains(&key.as_str()) && provenant_core::py::python_truth(value)
        }),
        _ => true,
    })
}

/// Encode an insertion-ordered list of MCP server tables as Codex inline TOML.
pub fn toml_table(entries: &[(String, Vec<(String, Value)>)]) -> Result<String, serde_json::Error> {
    let mut result = String::from("{");
    for (entry_index, (name, fields)) in entries.iter().enumerate() {
        if entry_index != 0 {
            result.push_str(", ");
        }
        result.push_str(&toml_scalar(&Value::String(name.clone()))?);
        result.push_str(" = {");
        for (field_index, (key, value)) in fields.iter().enumerate() {
            if field_index != 0 {
                result.push_str(", ");
            }
            result.push_str(key);
            result.push_str(" = ");
            result.push_str(&toml_scalar(value)?);
        }
        result.push('}');
    }
    result.push('}');
    Ok(result)
}

/// Implements the clean/inherited decision without launching Codex discovery.
pub fn config_decision(
    plan: &Plan,
    inputs: &Inputs,
    discovery_succeeded: bool,
) -> Result<ConfigDecision, AdapterError> {
    if !matches!(inputs.choice.as_str(), "" | "clean" | "inherited") {
        return Err(AdapterError::InvalidCodexConfig);
    }
    let clean = || ConfigDecision {
        config: "clean".into(),
        args: vec!["--ignore-user-config".into(), "--ignore-rules".into()],
        note: None,
    };
    if inputs.intent_assurance || inputs.capabilities || inputs.choice == "clean" {
        return Ok(clean());
    }
    if !discovery_succeeded && plan.mode == Mode::ReadOnly {
        let mut decision = clean();
        decision.note =
            Some("MCP server discovery failed; a non-writer lane cannot inherit blind".into());
        return Ok(decision);
    }
    let mut decision = ConfigDecision {
        config: "inherited".into(),
        args: vec![
            "-c".into(),
            "notify=[]".into(),
            "-c".into(),
            "web_search=\"live\"".into(),
        ],
        note: None,
    };
    if !discovery_succeeded {
        decision.note = Some("MCP server discovery failed; no server approvals set".into());
    }
    if plan.mode == Mode::ReadOnly {
        decision
            .args
            .extend(["-c".into(), "features.hooks=false".into()]);
    }
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn codex_config_fails_closed_for_failed_reader_discovery() {
        let p = plan(Mode::ReadOnly);
        let decision = config_decision(&p, &Inputs::default(), false).unwrap();
        assert_eq!(decision.config, "clean");
        assert_eq!(
            decision.note.as_deref(),
            Some("MCP server discovery failed; a non-writer lane cannot inherit blind")
        );
        assert_eq!(
            config_decision(
                &p,
                &Inputs {
                    choice: "typo".into(),
                    ..Default::default()
                },
                true
            ),
            Err(AdapterError::InvalidCodexConfig)
        );
    }

    #[test]
    fn codex_transport_and_toml_helpers_match_python() {
        let input = serde_json::json!({
            "transports": [
                {}, {"type":"stdio"}, {"type":"streamable_http","url":"https://example.invalid"},
                {"type":"streamable_http","http_headers":{"X-Key":"v"}},
                {"type":"streamable_http","command":"exec"},
                {"type":"http","command":null}
            ],
            "entries": [["mcp.one", [["enabled", false], ["default_tools_approval_mode", "approve"]]], ["工具", [["mode", "🦀"]]]]
        });
        let actual = serde_json::json!({
            "executes": input["transports"].as_array().unwrap().iter().map(|transport| transport_executes(transport).unwrap()).collect::<Vec<_>>(),
            "toml": toml_table(&input["entries"].as_array().unwrap().iter().map(|entry| (
                entry[0].as_str().unwrap().to_owned(),
                entry[1].as_array().unwrap().iter().map(|pair| (pair[0].as_str().unwrap().to_owned(), pair[1].clone())).collect()
            )).collect::<Vec<_>>()).unwrap()
        });
        let Some(expected): Option<Value> = crate::oracle::python(
            r#"import importlib,json,sys; m=importlib.import_module('skills.orchestrate.scripts.adapters.codex'); x=json.load(sys.stdin); print(json.dumps({'executes':[m._executes(t) for t in x['transports']], 'toml':m._toml_table({name:dict(fields) for name,fields in x['entries']})}))"#,
            &input,
            None,
        ) else {
            return;
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn codex_inherits_only_when_safe_and_respects_assurance_and_capability_lanes() {
        let reader = plan(Mode::ReadOnly);
        let inherited = config_decision(&reader, &Inputs::default(), true).unwrap();
        assert_eq!(inherited.config, "inherited");
        assert!(inherited.args.contains(&"features.hooks=false".into()));

        let mut writer = plan(Mode::WorktreeWrite);
        let writer_policy = config_decision(&writer, &Inputs::default(), true).unwrap();
        assert_eq!(writer_policy.config, "inherited");
        assert!(!writer_policy.args.contains(&"features.hooks=false".into()));

        let assurance = config_decision(
            &writer,
            &Inputs {
                intent_assurance: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        assert_eq!(assurance.config, "clean");
        writer.mode = Mode::ReadOnly;
        let capability = config_decision(
            &writer,
            &Inputs {
                capabilities: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        assert_eq!(capability.config, "clean");
    }
}
