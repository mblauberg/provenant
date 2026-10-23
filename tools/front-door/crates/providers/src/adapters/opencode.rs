use super::*;
use provenant_core::py::python_strip;
use regex::Regex;
use serde_json::{Value, json};
use std::path::Path;

pub const MIN_MAJOR: u64 = 2;
pub const EXIT_AFTER_OUTPUT: bool = true;
pub const UPGRADE_FIX: &str =
    "upgrade OpenCode to v2 (`opencode upgrade`); Fabric no longer drives v1";
pub const EMPTY_FAILURE: &str = "provider exited {exit_code} without final assistant output";

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum OutputError {
    #[error("OpenCode event must be a JSON object with a hashable type")]
    MalformedEvent,
}

pub fn continues_turn(event: &Value) -> Result<bool, OutputError> {
    let Some(event) = event.as_object() else {
        return Err(OutputError::MalformedEvent);
    };
    let kind = event.get("type");
    if kind.is_some_and(|value| value.is_array() || value.is_object()) {
        return Err(OutputError::MalformedEvent);
    }
    let kind = kind.and_then(Value::as_str).unwrap_or_default();
    let part = event.get("part");
    Ok(matches!(kind, "step_start" | "tool_use")
        || (kind == "step_finish"
            && part
                .and_then(|value| value.get("reason"))
                .and_then(Value::as_str)
                == Some("tool-calls")))
}

pub fn version_error(text: Option<&str>) -> Option<String> {
    let text = text.unwrap_or_default();
    let expression = Regex::new(r"v?(\d+)\.\d+(?:\.\d+)?").ok()?;
    for found in expression.captures_iter(text) {
        let span = found.get(0)?;
        let previous = text[..span.start()].chars().next_back();
        let next = text[span.end()..].chars().next();
        if previous.is_some_and(|ch| ch == '.' || ch == '_' || ch.is_alphanumeric())
            || next.is_some_and(|ch| ch == '.' || ch == '_' || ch.is_alphanumeric())
        {
            continue;
        }
        let digits = found
            .get(1)?
            .as_str()
            .chars()
            .map(crate::pending::decimal_digit)
            .collect::<Option<Vec<_>>>()?;
        let significant = digits
            .iter()
            .skip_while(|&&d| d == 0)
            .copied()
            .collect::<Vec<_>>();
        if significant.len() <= 1 && significant.first().copied().unwrap_or(0) < MIN_MAJOR as u8 {
            return Some(format!(
                "OpenCode {} is unsupported; fix: {UPGRADE_FIX}",
                span.as_str()
            ));
        }
        return None;
    }
    None
}

pub fn model_with_variant(model: Option<&str>, effort: Option<&str>) -> Option<String> {
    let model = model?;
    if model.is_empty() || effort.is_none_or(str::is_empty) || model.contains('#') {
        return Some(model.to_owned());
    }
    Some(format!("{model}#{}", effort?))
}

pub fn opencode(plan: &Plan) -> Result<AdapterProfile, BuildError> {
    let mut argv = vec![
        "run".into(),
        "--standalone".into(),
        "--format".into(),
        "json".into(),
        "--auto".into(),
        "--print-logs".into(),
        "--log-level".into(),
        "error".into(),
    ];
    if let Some(session) = plan
        .resume_session
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        argv.extend(["-s".into(), session.into()]);
    }
    if let Some(model) = model_with_variant(
        plan.model.as_deref().filter(|s| !s.is_empty()),
        plan.effort.as_deref(),
    ) {
        argv.extend(["--model".into(), model]);
    }
    argv.push(plan.prompt.clone());
    profile("opencode", argv)
}

const LOG_LEVEL: &str = r"(?:^|\s|\[)(?:level=)?(DEBUG|INFO|WARN|ERROR)\b";
pub fn diagnostics(stderr: &str) -> (String, String) {
    let Ok(level_re) = regex::RegexBuilder::new(LOG_LEVEL)
        .case_insensitive(true)
        .build()
    else {
        return (stderr.to_owned(), String::new());
    };
    let mut warning = String::new();
    let mut error = String::new();
    let mut lines = Vec::new();
    for line in crate::pending::python_splitlines(stderr, false) {
        let level = level_re
            .captures(line)
            .and_then(|capture| capture.get(1))
            .map(|m| m.as_str().to_ascii_uppercase());
        if matches!(level.as_deref(), Some("INFO" | "DEBUG")) {
            continue;
        }
        lines.push(line);
        if warning.is_empty() && level.as_deref() == Some("WARN") {
            warning = python_strip(line).to_owned();
        }
        if error.is_empty() && level.as_deref() == Some("ERROR") {
            error = python_strip(line).to_owned();
        }
    }
    (
        lines.join("\n"),
        if error.is_empty() { warning } else { error },
    )
}

pub fn text_event(text: impl Into<String>) -> Value {
    json!({"type":"text", "part":{"text":text.into()}})
}

/// Caller supplies the already-resolved executable path; no filesystem access occurs here.
pub fn environment(
    plan: &Plan,
    network_requested: Option<bool>,
    confinement: Option<&str>,
    fabric_mcp: &Path,
) -> Value {
    let writer = plan.mode.is_writer();
    let bash = if writer {
        json!({"*":"allow"})
    } else {
        json!({"*":"deny", "provenant-no-shell":"allow"})
    };
    let mut config = json!({"permission": {
        "edit": {"*": if writer {"allow"} else {"deny"}},
        "bash": bash,
        "question": "deny",
        "external_directory": {"*":"deny"},
        "webfetch": if network_requested == Some(false) {"deny"} else {"allow"}
    }});
    if writer {
        config["mcp"] = json!({"servers":{"fabric": {
            "type":"local", "command":[fabric_mcp.to_string_lossy()], "disabled":false,
            "environment":{"AGENT_FABRIC_SEAT":"opencode"}
        }}});
    } else if confinement == Some("sandbox-exec") {
        config["mcp"] = json!({"servers":{"fabric": {
            "type":"local", "command":["provenant"], "disabled":true
        }}});
    }
    let config_content = python_config_json(&config, writer, fabric_mcp);
    json!({
        "OPENCODE_CONFIG_CONTENT": config_content,
        "OPENCODE_DISABLE_CLAUDE_CODE":"1",
        "OPENCODE_DISABLE_CLAUDE_CODE_SKILLS":"1",
        "OPENCODE_DISABLE_AUTOUPDATE":"1",
        "OPENCODE_DISABLE_FILEWATCHER":"1"
    })
}

fn python_config_json(config: &Value, writer: bool, fabric_mcp: &Path) -> String {
    let edit = if writer { "allow" } else { "deny" };
    let bash = if writer {
        r#"{"*": "allow"}"#
    } else {
        r#"{"*": "deny", "provenant-no-shell": "allow"}"#
    };
    let webfetch = if config["permission"]["webfetch"] == "deny" {
        "deny"
    } else {
        "allow"
    };
    let mut content = format!(
        r#"{{"permission": {{"edit": {{"*": "{edit}"}}, "bash": {bash}, "question": "deny", "external_directory": {{"*": "deny"}}, "webfetch": "{webfetch}"}}}}"#
    );
    if writer {
        let path = python_json_string(fabric_mcp.to_string_lossy().as_ref());
        content.pop();
        content.push_str(&format!(
            r#", "mcp": {{"servers": {{"fabric": {{"type": "local", "command": [{path}], "disabled": false, "environment": {{"AGENT_FABRIC_SEAT": "opencode"}}}}}}}}}}"#
        ));
    } else if config["mcp"].is_object() {
        content.pop();
        content.push_str(r#", "mcp": {"servers": {"fabric": {"type": "local", "command": ["provenant"], "disabled": true}}}}"#);
    }
    content
}

fn python_json_string(value: &str) -> String {
    let encoded = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into());
    let mut result = String::with_capacity(encoded.len());
    for ch in encoded.chars() {
        if ch.is_ascii() && ch != '\u{7f}' {
            result.push(ch);
        } else if u32::from(ch) <= 0xffff {
            result.push_str(&format!(r#"\u{:04x}"#, u32::from(ch)));
        } else {
            let scalar = u32::from(ch) - 0x1_0000;
            let high = 0xd800 + (scalar >> 10);
            let low = 0xdc00 + (scalar & 0x3ff);
            result.push_str(&format!(r#"\u{high:04x}\u{low:04x}"#));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn opencode_argv_uses_v2_standalone_and_variant_model() {
        let mut plan = plan(Mode::ReadOnly);
        plan.model = Some("provider/model".into());
        plan.effort = Some("high".into());
        plan.resume_session = Some("session-id".into());
        let actual = opencode(&plan).unwrap();
        assert_eq!(actual.cli, "opencode");
        assert_eq!(
            actual.argv,
            [
                "opencode",
                "run",
                "--standalone",
                "--format",
                "json",
                "--auto",
                "--print-logs",
                "--log-level",
                "error",
                "-s",
                "session-id",
                "--model",
                "provider/model#high",
                "do it"
            ]
        );
    }

    #[test]
    fn opencode_turn_version_diagnostics_and_environment_match_policy() {
        assert!(
            continues_turn(&json!({"type":"step_finish", "part":{"reason":"tool-calls"}})).unwrap()
        );
        assert!(matches!(
            continues_turn(&json!([])),
            Err(OutputError::MalformedEvent)
        ));
        assert_eq!(model_with_variant(Some(""), Some("high")), Some("".into()));
        assert_eq!(
            version_error(Some("opencode 1.9.8")),
            Some(format!("OpenCode 1.9.8 is unsupported; fix: {UPGRADE_FIX}"))
        );
        assert_eq!(version_error(Some("opencode 2.0.0")), None);
        assert_eq!(
            diagnostics("INFO startup\nWARN warning\nERROR failed"),
            ("WARN warning\nERROR failed".into(), "ERROR failed".into())
        );
        assert_eq!(
            text_event("answer"),
            json!({"type":"text", "part":{"text":"answer"}})
        );
        let path = Path::new("/workspace/runtime/fabric/bin/fabric-mcp");
        let env = environment(&plan(Mode::WorktreeWrite), Some(false), None, path);
        let config: Value =
            serde_json::from_str(env["OPENCODE_CONFIG_CONTENT"].as_str().unwrap()).unwrap();
        assert_eq!(config["permission"]["webfetch"], "deny");
        assert_eq!(
            config["mcp"]["servers"]["fabric"]["command"][0],
            path.to_string_lossy().as_ref()
        );
        assert_eq!(env["OPENCODE_DISABLE_FILEWATCHER"], "1");

        let reader = environment(&plan(Mode::ReadOnly), None, Some("sandbox-exec"), path);
        let config: Value =
            serde_json::from_str(reader["OPENCODE_CONFIG_CONTENT"].as_str().unwrap()).unwrap();
        assert_eq!(config["permission"]["edit"]["*"], "deny");
        assert_eq!(config["permission"]["bash"]["provenant-no-shell"], "allow");
        assert_eq!(config["permission"]["webfetch"], "allow");
        assert_eq!(config["mcp"]["servers"]["fabric"]["disabled"], true);
    }

    #[test]
    fn opencode_functions_match_python_oracle_on_policy_fixtures() {
        let input = json!({
            "events": [
                {"type":"step_start"}, {"type":"tool_use"},
                {"type":"step_finish","part":{"reason":"tool-calls"}},
                {"type":"step_finish","part":{"reason":"stop"}}
            ],
            "malformed_events": [[], null, {"type":[]}, {"type":{}}],
            "versions": [null,"v1.0.0","opencode 1.9.8","2.0.0","Version 12.2.1","x1.0.0","opencode 2.0.0 (bun 1.1.3)","9999999999999999999999999.0.0 1.1.0","١.٢.٣","٢.٠.٠ 1.0.0"],
            "variants": [["provider/model","high"],["provider/model#old","high"],[null,"high"],["m",null],["","high"]],
            "logs": "info startup\rwarn warning\u{2028}level=ERROR failed\u{001c}DEBUG hidden\nplain line",
            "writer_config": {"mode":"worktree_write","prompt":"fixture","boundary_prompt":"","cwd":"/workspace","timeout_seconds":30,"agy_sandbox":false,"resume_session":null,"model":"","effort":null,"session_id":"session","add_dirs":[],"endpoint_base_url":null,"context_ceiling":null,"applied":{},"network_requested":false},
            "reader_config": {"mode":"read_only","prompt":"fixture","boundary_prompt":"","cwd":"/workspace","timeout_seconds":30,"agy_sandbox":false,"resume_session":null,"model":"","effort":null,"session_id":"session","add_dirs":[],"endpoint_base_url":null,"context_ceiling":null,"applied":{"confinement":"sandbox-exec"}},
            "network_requested": false
        });
        let plan: Plan = serde_json::from_value(input["writer_config"].clone()).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap();
        let fabric_mcp = root
            .join("runtime/fabric/bin/fabric-mcp")
            .canonicalize()
            .unwrap();
        let reader_plan: Plan = serde_json::from_value(input["reader_config"].clone()).unwrap();
        let actual = json!({
            "empty_argv": opencode(&plan).unwrap().argv,
            "continues": input["events"].as_array().unwrap().iter().map(|event| continues_turn(event).unwrap()).collect::<Vec<_>>(),
            "malformed": input["malformed_events"].as_array().unwrap().iter().map(|event| continues_turn(event).is_err()).collect::<Vec<_>>(),
            "versions": input["versions"].as_array().unwrap().iter().map(|v| version_error(v.as_str())).collect::<Vec<_>>(),
            "variants": input["variants"].as_array().unwrap().iter().map(|v| model_with_variant(v[0].as_str(), v[1].as_str())).collect::<Vec<_>>(),
            "diagnostics": diagnostics(input["logs"].as_str().unwrap()),
            "environment": environment(&plan, input["network_requested"].as_bool(), None, &fabric_mcp)
            ,"reader_environment": environment(&reader_plan, None, Some("sandbox-exec"), &fabric_mcp)
        });
        let Some(expected): Option<Value> = crate::oracle::python(
            r#"import importlib,json,sys; m=importlib.import_module('skills.orchestrate.scripts.adapters.opencode'); p=json.load(sys.stdin); plan=p['writer_config']; malformed=[]
for e in p['malformed_events']:
    try: m.continues_turn(e); malformed.append(False)
    except (AttributeError,TypeError): malformed.append(True)
print(json.dumps({'empty_argv':m.argv(plan), 'continues':[m.continues_turn(x) for x in p['events']], 'malformed':malformed, 'versions':[m.version_error(x) for x in p['versions']], 'variants':[m.model_with_variant(*x) for x in p['variants']], 'diagnostics':m.diagnostics(p['logs']), 'environment':m.environment(plan), 'reader_environment':m.environment(p['reader_config'])}))"#,
            &input,
            None,
        ) else {
            return;
        };
        assert_eq!(actual, expected);
    }
}
