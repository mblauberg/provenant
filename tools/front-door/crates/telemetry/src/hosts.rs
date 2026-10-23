//! Read-only peer boundary. Transport, host configuration and quota display have separate owners.
use crate::{
    Error, Numeric, Result,
    costs::{Group, SUMMED, TOKEN_FIELDS},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const SUMMARY_SCHEMA: &str = "provenant.telemetry.summary.v1";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub schema_version: u32,
    pub since: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Partial,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HostStatus {
    pub host: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
/// Public metadata fields are validated before a payload becomes an accepted summary.
/// Unknown v1 report-row fields must remain nullable strings, matching older peers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub schema: String,
    pub host: String,
    pub session_usage: Vec<Group>,
    #[serde(default)]
    pub outcomes: Vec<BTreeMap<String, Value>>,
    #[serde(default)]
    pub lanes: Vec<BTreeMap<String, Value>>,
    #[serde(default)]
    pub attempts: Vec<BTreeMap<String, Value>>,
    #[serde(default)]
    pub limits_per_day: Vec<BTreeMap<String, Value>>,
    #[serde(default)]
    pub quota_observations: Vec<BTreeMap<String, Value>>,
    #[serde(default)]
    pub unattributed: Vec<BTreeMap<String, Value>>,
    #[serde(default)]
    pub last_limit_events: Vec<BTreeMap<String, Value>>,
    #[serde(default)]
    pub compactions_per_lane: Vec<BTreeMap<String, Value>>,
    pub excluded_rows: BTreeMap<String, Numeric>,
    pub receipt_usage_rows_excluded: Numeric,
    pub pending_source_rebuilds: Vec<Value>,
    #[serde(default)]
    pub quota_snapshots: Vec<Value>,
}
/// Implemented by the existing bounded SSH transport at integration, never by a second peer owner.
pub trait PeerTransport {
    fn stats(&self, host: &str, request: &Request) -> Result<PeerReply>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerReply {
    pub ok: bool,
    #[serde(default)]
    pub reads_flagged: bool,
    pub result: Option<Value>,
    pub error: Option<PeerError>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerError {
    pub code: String,
}
fn numeric(v: &Value, nullable: bool) -> bool {
    nullable && v.is_null() || v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
}
fn count(v: &Value) -> bool {
    numeric(v, false) && v.as_f64().is_some_and(|n| n.fract() == 0.0)
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
/// Match the permissive v1 envelope while refusing booleans and invalid numeric counters.
pub fn validate(value: &Value, host: &str) -> Result<Summary> {
    if value["schema"] != SUMMARY_SCHEMA
        || value["host"] != host
        || !value["session_usage"].is_array()
    {
        return Err(invalid("invalid summary"));
    }
    for g in value["session_usage"].as_array().into_iter().flatten() {
        if !g.is_object()
            || g["host"] != host
            || SUMMED
                .iter()
                .any(|k| !numeric(&g[*k], false) || *k != "known_cost_usd" && !count(&g[*k]))
            || !g["missing_token_fields"].is_object()
            || TOKEN_FIELDS
                .iter()
                .any(|k| !count(&g["missing_token_fields"][*k]))
            || !numeric(&g["reported_cost_usd"], true)
            || ["model_raw", "billing"].iter().any(|k| {
                g[*k]
                    .as_array()
                    .is_none_or(|a| a.iter().any(|v| !v.is_string()))
            })
            || [
                "source", "session", "provider", "adapter", "model", "day", "account",
            ]
            .iter()
            .any(|k| !g[*k].is_null() && !g[*k].is_string())
        {
            return Err(invalid("invalid aggregate"));
        }
    }
    let required: &[(&str, &[&str])] = &[
        (
            "outcomes",
            &[
                "adapter",
                "model",
                "effort",
                "task_class",
                "route",
                "error_classes",
                "lanes",
                "succeeded",
                "attempts",
                "retries",
                "success_rate",
                "latency_p50_s",
                "latency_p90_s",
                "output_tokens_per_s",
            ],
        ),
        ("lanes", &["status", "error_class", "lanes"]),
        ("attempts", &["status", "error_class", "attempts"]),
        ("compactions_per_lane", &["lane", "compactions"]),
        (
            "limits_per_day",
            &["day", "provider", "account", "status", "events"],
        ),
        (
            "quota_observations",
            &[
                "provider",
                "window",
                "used_percent",
                "reset_at",
                "timestamp",
            ],
        ),
        (
            "unattributed",
            &["source", "adapter", "rows", "input_tokens", "statuses"],
        ),
        (
            "last_limit_events",
            &["provider", "account", "timestamp", "age_seconds"],
        ),
    ];
    for (field, keys) in required {
        let Some(rows) = value.get(*field) else {
            continue;
        };
        let rows = rows
            .as_array()
            .ok_or_else(|| invalid("invalid summary rows"))?;
        for row in rows {
            let row = row
                .as_object()
                .filter(|r| keys.iter().all(|k| r.contains_key(*k)))
                .ok_or_else(|| invalid("invalid summary row fields"))?;
            for (key, item) in row {
                let valid = match key.as_str() {
                    "lanes" | "succeeded" | "attempts" | "retries" | "events" | "rows"
                    | "input_tokens" | "age_seconds" | "compactions" | "latency_samples" => {
                        count(item)
                    }
                    "success_rate"
                    | "latency_p50_s"
                    | "latency_p90_s"
                    | "output_tokens_per_s"
                    | "used_percent"
                    | "window_minutes" => numeric(item, key != "used_percent"),
                    "statuses" | "error_classes" => {
                        item.as_object().is_some_and(|m| m.values().all(count))
                    }
                    _ => item.is_null() || item.is_string(),
                };
                if !valid {
                    return Err(invalid("invalid summary row value"));
                }
            }
        }
    }
    if value["excluded_rows"]
        .as_object()
        .is_none_or(|m| m.values().any(|v| !numeric(v, false)))
        || !numeric(&value["receipt_usage_rows_excluded"], false)
        || !value["pending_source_rebuilds"].is_array()
    {
        return Err(invalid("invalid summary counts"));
    }
    Ok(serde_json::from_value(value.clone())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn summary() -> Value {
        json!({"schema":SUMMARY_SCHEMA,"host":"local","session_usage":[],"excluded_rows":{},"receipt_usage_rows_excluded":0,"pending_source_rebuilds":[]})
    }
    #[test]
    fn old_peers_allow_missing_optional_sections() {
        assert!(validate(&summary(), "local").is_ok());
    }
    #[test]
    fn malformed_peers_do_not_become_accepted_summaries() {
        let mut s = summary();
        s["receipt_usage_rows_excluded"] = json!(true);
        assert!(validate(&s, "local").is_err());
        let mut s = summary();
        s["lanes"] = json!([{"status":"ok","error_class":null,"lanes":-1}]);
        assert!(validate(&s, "local").is_err());
        assert!(validate(&summary(), "remote").is_err());
    }
    #[test]
    fn unknown_numeric_outcome_enrichment_is_refused_by_v1() {
        let mut s = summary();
        s["lanes"] = json!([{"status":"ok","error_class":null,"lanes":1,"cost":1}]);
        assert!(validate(&s, "local").is_err());
    }
}
