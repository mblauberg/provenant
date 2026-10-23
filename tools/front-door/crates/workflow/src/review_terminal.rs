//! Terminal evidence and lineage checks for review ladder legs.
use crate::policy::{hashable, strip};
use crate::PolicyError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const REVIEW_RESULT_KEYS: &[&str] = &["angle", "verdict", "issues", "crossFamily", "path"];
const VERDICTS: &[&str] = &["approve", "approve-with-nits", "block"];
const UNAVAILABLE: &[&str] = &[
    "adapter_account_default_only",
    "all_failed",
    "auth_or_quota_error",
    "model_required_for_broker",
    "provider_unavailable",
    "routing_record_invalid",
    "same_family_forbidden",
    "tool_not_found",
    "unknown_tool",
    "unsafe_by_default",
    "unavailable",
];

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerReviewResult {
    pub angle: String,
    pub verdict: Value,
    pub issues: Vec<ReviewIssue>,
    #[serde(rename = "crossFamily")]
    pub cross_family: CrossFamilyResult,
    pub path: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewIssue {
    pub severity: Severity,
    #[serde(rename = "patchPath")]
    pub patch_path: String,
    pub detail: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum Severity {
    P0,
    P1,
    P2,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CrossFamilyResult {
    pub ran: bool,
    pub tool: String,
    pub status: String,
    pub model_family: String,
    pub endpoint_provider: String,
    pub cross_family: bool,
    pub certification_eligible: bool,
    pub read_only_guarantee: String,
    pub output_path: String,
    pub route_receipt: String,
    pub not_run_reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalisedReview {
    pub family: String,
    pub provider_family: String,
    pub endpoint_provider: String,
    pub orchestrator_family: String,
    pub cross_family: bool,
    pub status: String,
    pub reason: String,
    pub terminal_available: bool,
    pub certifying_vote: bool,
}

fn text<'a>(object: &'a Map<String, Value>, key: &str) -> &'a str {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(strip)
        .unwrap_or_default()
}
fn verdict(value: &Value) -> Result<(String, &'static str), PolicyError> {
    let Some(object) = value.as_object().filter(|o| o.contains_key("verdict")) else {
        return Ok((String::new(), "no-verdict"));
    };
    let invalid = || Ok((String::new(), "invalid-terminal-result"));
    if object.len() != REVIEW_RESULT_KEYS.len()
        || !REVIEW_RESULT_KEYS
            .iter()
            .all(|key| object.contains_key(*key))
        || !object["angle"].is_string()
        || !object["path"].is_string()
    {
        return invalid();
    }
    let Some(issues) = object["issues"].as_array() else {
        return invalid();
    };
    for issue in issues {
        let Some(issue) = issue.as_object() else {
            return invalid();
        };
        if issue.len() != 3
            || !["severity", "patchPath", "detail"]
                .iter()
                .all(|key| issue.contains_key(*key))
        {
            return invalid();
        }
        hashable(&issue["severity"], "issues.severity")?;
        if !issue["severity"]
            .as_str()
            .is_some_and(|s| ["P0", "P1", "P2"].contains(&s))
            || !issue["patchPath"].is_string()
            || !issue["detail"].is_string()
        {
            return invalid();
        }
    }
    if !object["crossFamily"].is_object()
        || serde_json::from_value::<WorkerReviewResult>(value.clone()).is_err()
    {
        return invalid();
    }
    let Some(verdict) = object["verdict"].as_str() else {
        return Ok((String::new(), "no-verdict"));
    };
    let verdict = strip(verdict);
    if VERDICTS.contains(&verdict) {
        Ok((verdict.into(), ""))
    } else if verdict == "EXECUTION-UNAVAILABLE" {
        Ok((String::new(), "execution-unavailable"))
    } else {
        Ok((String::new(), "unparseable-verdict"))
    }
}
fn lineage_reason(
    result: &Map<String, Value>,
    chair: &Value,
    family: &str,
    provider: &str,
    endpoint: &str,
    orchestrator: &str,
) -> &'static str {
    if [family, provider, endpoint, orchestrator].contains(&"") {
        return "provider-unavailable";
    }
    let adapter = result
        .get("adapter")
        .or_else(|| result.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if adapter.is_empty() {
        return "endpoint/provider identity unavailable";
    }
    let tool = result
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if tool.is_empty() || tool != adapter {
        return "tool/adapter identity mismatch";
    }
    let expected_endpoint = match adapter {
        "claude" => "anthropic",
        "codex" => "openai",
        "cursor" => "cursor",
        "agy" => "agy",
        "kiro" => "aws",
        "copilot" => "github",
        "opencode" => "opencode",
        _ => return "endpoint/provider identity mismatch",
    };
    if endpoint != expected_endpoint {
        return "endpoint/provider identity mismatch";
    }
    if ["claude", "codex"].contains(&adapter) {
        if provider != expected_endpoint {
            return "endpoint/provider identity mismatch";
        }
        if family != expected_endpoint {
            return "endpoint/provider/model identity mismatch";
        }
    }
    let chair = strip(chair.as_str().unwrap_or_default());
    if chair.is_empty() {
        return "chair-family-unavailable";
    }
    if orchestrator != chair {
        return "orchestrator-family-mismatch";
    }
    if provider == chair {
        return "same-provider-lineage";
    }
    ""
}

/// The dispatch owner validates its full record. This boundary consumes only
/// evidence fields and deliberately retains source handling of malformed data.
pub fn normalise_dispatch_review(
    result: &Map<String, Value>,
    terminal: &Value,
    review_verdict: &Value,
    chair: &Value,
    transcript_available: bool,
    dispatcher_output_available: bool,
) -> Result<NormalisedReview, PolicyError> {
    let family = text(result, "model_family");
    let provider = text(result, "provider_family");
    let endpoint = text(result, "endpoint_provider");
    let orchestrator = text(result, "orchestrator_family");
    let lineage = lineage_reason(result, chair, family, provider, endpoint, orchestrator);
    let cross_family = lineage.is_empty();
    let exit = result.get("exit");
    let terminal_available =
        exit.is_some_and(|v| v.as_number().is_some_and(|n| n.is_i64() || n.is_u64()));
    let status = result.get("status").and_then(Value::as_str);
    let (worker_verdict, worker_reason) = verdict(terminal)?;
    let wrapper = strip(review_verdict.as_str().unwrap_or_default());
    if terminal_available {
        hashable(result.get("status").unwrap_or(&Value::Null), "status")?;
    }
    let read_only = result.get("read_only_guarantee").unwrap_or(&Value::Null);
    let safe_read = result
        .get("read_only_guarantee")
        .and_then(Value::as_str)
        .is_some_and(|s| ["enforced", "oauth_safe_mode"].contains(&s));
    let (leg_status, reason) = if !terminal_available {
        ("unavailable", "terminal-unavailable")
    } else if status.is_some_and(|s| UNAVAILABLE.contains(&s)) {
        ("unavailable", "provider-unavailable")
    } else if exit.is_none_or(|v| v.as_i64() != Some(0)) {
        ("failed", "nonzero-exit")
    } else if text(result, "output_path").is_empty()
        || !dispatcher_output_available
        || !transcript_available
    {
        ("failed", "missing-transcript")
    } else if worker_verdict.is_empty() {
        (
            if worker_reason == "execution-unavailable" {
                "unavailable"
            } else {
                "failed"
            },
            worker_reason,
        )
    } else if !VERDICTS.contains(&wrapper) {
        ("failed", "wrapper-verdict-unavailable")
    } else if worker_verdict != wrapper {
        ("failed", "verdict-mismatch")
    } else if lineage == "provider-unavailable" {
        ("unavailable", lineage)
    } else if lineage.starts_with("endpoint/provider")
        || lineage.starts_with("tool/adapter")
        || lineage == "orchestrator-family-mismatch"
    {
        ("failed", lineage)
    } else if status != Some("ok") {
        ("failed", "dispatch-failed")
    } else if !cross_family {
        ("pass", lineage)
    } else if !safe_read {
        ("pass", "read-only-guarantee-unavailable")
    } else {
        ("pass", "")
    };
    if leg_status == "pass" && cross_family {
        hashable(read_only, "read_only_guarantee")?;
    }
    Ok(NormalisedReview {
        family: family.into(),
        provider_family: provider.into(),
        endpoint_provider: endpoint.into(),
        orchestrator_family: orchestrator.into(),
        cross_family,
        status: leg_status.into(),
        reason: reason.into(),
        terminal_available,
        certifying_vote: leg_status == "pass" && cross_family && safe_read,
    })
}

/// Decode a review case at its JSON boundary, refusing oversized integer
/// tokens before serde_json can represent them as floating-point numbers.
pub fn normalise_dispatch_review_json(input: &[u8]) -> Result<NormalisedReview, PolicyError> {
    let value =
        provenant_records::checked_json::from_slice(input).map_err(|error| match error {
            provenant_records::checked_json::DecodeError::NumberOutOfRange(_) => {
                PolicyError::NumberOutOfRange
            }
            provenant_records::checked_json::DecodeError::Json(error) => {
                PolicyError::InvalidJson(error.to_string())
            }
        })?;
    let object = value
        .as_object()
        .ok_or_else(|| PolicyError::InvalidJson("expected an object".into()))?;
    let dispatch = object
        .get("dispatch")
        .and_then(Value::as_object)
        .ok_or_else(|| PolicyError::InvalidJson("expected dispatch object".into()))?;
    let transcript = object
        .get("transcript")
        .and_then(Value::as_bool)
        .ok_or_else(|| PolicyError::InvalidJson("expected transcript boolean".into()))?;
    let output = object
        .get("output")
        .and_then(Value::as_bool)
        .ok_or_else(|| PolicyError::InvalidJson("expected output boolean".into()))?;
    normalise_dispatch_review(
        dispatch,
        object.get("terminal").unwrap_or(&Value::Null),
        object.get("verdict").unwrap_or(&Value::Null),
        object.get("chair").unwrap_or(&Value::Null),
        transcript,
        output,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn run(input: &Value) -> Value {
        match normalise_dispatch_review(
            input["dispatch"].as_object().unwrap(),
            &input["terminal"],
            &input["verdict"],
            &input["chair"],
            input["transcript"].as_bool().unwrap(),
            input["output"].as_bool().unwrap(),
        ) {
            Ok(result) => serde_json::to_value(result).unwrap(),
            Err(PolicyError::Unhashable { kind, .. }) => crate::test_support::unhashable(kind),
            Err(error) => panic!("unexpected typed input error: {error}"),
        }
    }

    #[test]
    fn oversized_exit_integer_is_a_typed_improvement_refusal() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../tests/fixtures/review_terminal.json")).unwrap();
        let encoded = fixtures[0]["input"]
            .to_string()
            .replace("\"exit\":0", "\"exit\":18446744073709551616");
        assert!(matches!(
            normalise_dispatch_review_json(encoded.as_bytes()),
            Err(PolicyError::NumberOutOfRange)
        ));
    }

    #[test]
    fn translated_terminal_policy_and_python_agree() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../tests/fixtures/review_terminal.json")).unwrap();
        let cases: Vec<_> = fixtures
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["input"].clone())
            .collect();
        for fixture in fixtures.as_array().unwrap() {
            assert_eq!(run(&fixture["input"]), fixture["expected"]);
        }
        if let Some(expected) = crate::test_support::oracle("review_terminal", &json!(cases)) {
            assert_eq!(expected, json!(cases.iter().map(run).collect::<Vec<_>>()));
        }
    }
    #[test]
    fn error_precedence_closed_shapes_and_adapter_matrix_match_python() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../tests/fixtures/review_terminal.json")).unwrap();
        let base = fixtures[0]["input"].clone();
        let mut cases = vec![base.clone()];
        for key in ["angle", "path", "issues", "crossFamily", "verdict"] {
            let mut c = base.clone();
            c["terminal"].as_object_mut().unwrap().remove(key);
            cases.push(c);
        }
        for (key, value) in [
            ("exit", json!(true)),
            ("exit", json!(0.0)),
            ("exit", json!(u64::MAX)),
            ("status", json!("other")),
            ("adapter", Value::Null),
            ("adapter", json!("")),
            ("tool", json!("other")),
            ("read_only_guarantee", json!("none")),
            ("read_only_guarantee", json!([])),
            ("status", json!([])),
            ("orchestrator_family", json!("openai")),
            ("output_path", json!(" ")),
        ] {
            let mut c = base.clone();
            c["dispatch"][key] = value;
            cases.push(c);
        }
        for status in UNAVAILABLE {
            let mut c = base.clone();
            c["dispatch"]["status"] = json!(status);
            c["dispatch"]["exit"] = json!(7);
            cases.push(c);
        }
        for (adapter, endpoint) in [
            ("claude", "anthropic"),
            ("codex", "openai"),
            ("cursor", "cursor"),
            ("agy", "agy"),
            ("kiro", "aws"),
            ("copilot", "github"),
            ("opencode", "opencode"),
            ("unknown", "unknown"),
        ] {
            let mut c = base.clone();
            for key in ["tool", "adapter"] {
                c["dispatch"][key] = json!(adapter);
            }
            c["dispatch"]["endpoint_provider"] = json!(endpoint);
            c["dispatch"]["provider_family"] = json!(endpoint);
            c["dispatch"]["model_family"] = json!(endpoint);
            cases.push(c);
        }
        for value in [
            Value::Null,
            json!(42),
            json!("EXECUTION-UNAVAILABLE"),
            json!("block"),
            json!(" approve "),
        ] {
            let mut c = base.clone();
            c["terminal"]["verdict"] = value;
            cases.push(c);
        }
        for extra in [
            json!({"severity":"P3","patchPath":"a","detail":"bad"}),
            json!({"severity":[],"patchPath":"a","detail":"bad"}),
            json!({"severity":"P0","patchPath":"a","detail":"bad","extra":1}),
        ] {
            let mut c = base.clone();
            c["terminal"]["issues"] = json!([extra]);
            cases.push(c);
        }
        let mut c = base.clone();
        c["terminal"]["crossFamily"]["ran"] = json!(1);
        cases.push(c);
        let mut c = base.clone();
        c["terminal"]["crossFamily"] = json!([
            true,
            "codex",
            "ok",
            "openai",
            "openai",
            true,
            true,
            "enforced",
            "reviews/luna.out",
            "reviews/luna.route.json",
            ""
        ]);
        assert_eq!(run(&c)["reason"], "invalid-terminal-result");
        cases.push(c);
        let mut c = base.clone();
        c["terminal"]["extra"] = json!(1);
        cases.push(c);
        let mut c = base.clone();
        c["chair"] = Value::Null;
        cases.push(c);
        let mut c = base;
        c["dispatch"].as_object_mut().unwrap().remove("adapter");
        cases.push(c);
        let actual: Vec<_> = cases.iter().map(run).collect();
        if let Some(expected) = crate::test_support::oracle("review_terminal", &json!(cases)) {
            assert_eq!(json!(actual), expected);
        }
    }
}
