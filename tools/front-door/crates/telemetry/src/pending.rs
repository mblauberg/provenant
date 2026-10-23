mod numeric;
pub use numeric::Numeric;
// Temporary contracts until the records/core owners are ported.
use serde::{Deserialize, Serialize};
use serde_json::Value;

// port: belongs in provenant-records
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub source: String,
    pub event_id: String,
    pub host: String,
    pub session: Option<String>,
    pub lane: Option<String>,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub attempt: Option<crate::Numeric>,
    pub graph_node: Option<String>,
    pub adapter: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account: Option<String>,
    pub repository: Option<String>,
    pub revision: Option<String>,
    pub timestamp: Option<String>,
    pub input_tokens: Option<crate::Numeric>,
    pub cache_read_tokens: Option<crate::Numeric>,
    pub cache_write_tokens: Option<crate::Numeric>,
    pub output_tokens: Option<crate::Numeric>,
    pub reasoning_tokens: Option<crate::Numeric>,
    pub usage_source: Option<String>,
    pub reported_cost: Option<f64>,
    pub status: Option<String>,
    pub error_class: Option<String>,
    pub task_class: Option<String>,
    pub route: Option<String>,
    pub duration_ms: Option<crate::Numeric>,
    pub provider_ms: Option<crate::Numeric>,
    pub classified_by: Option<String>,
    pub classifier_confidence: Option<f64>,
    pub shadow_answers: Option<String>,
    pub parent_run_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub parent_attempt: Option<crate::Numeric>,
    pub requested_model: Option<String>,
    pub resolved_model: Option<String>,
    pub observed_model: Option<String>,
    pub delivery_run_id: Option<String>,
}

// port: belongs in provenant-records
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub kind: String,
    pub ts: Option<String>,
    pub source_ref: String,
    pub session: Option<String>,
    pub run_id: Option<String>,
    pub lane: Option<String>,
    pub model: Option<String>,
    pub harness_rev: Option<String>,
    pub attrs: Value,
}
// port: belongs in provenant-records
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Observation {
    pub source: String,
    pub origin_path: String,
    pub provider: Option<String>,
    pub adapter: Option<String>,
    pub model: Option<String>,
    pub account: Option<String>,
    pub session: Option<String>,
    pub timestamp: Option<String>,
    pub window: String,
    pub used_percent: f64,
    pub window_minutes: Option<f64>,
    pub reset_at: Option<String>,
}
// port: belongs in provenant-core
pub fn parse_time(value: &str) -> crate::Result<jiff::Timestamp> {
    if let Ok(t) = value.parse::<jiff::Timestamp>() {
        return Ok(t);
    }
    if let Ok(t) = value.parse::<jiff::civil::DateTime>() {
        return t
            .to_zoned(jiff::tz::TimeZone::UTC)
            .map(|t| t.timestamp())
            .map_err(|_| crate::Error::Timestamp(value.into()));
    }
    value
        .parse::<jiff::civil::Date>()
        .ok()
        .and_then(|t| t.at(0, 0, 0, 0).to_zoned(jiff::tz::TimeZone::UTC).ok())
        .map(|t| t.timestamp())
        .ok_or_else(|| crate::Error::Timestamp(value.into()))
}
// port: belongs in provenant-records
// Python json.dumps(sort_keys=True) defaults: ASCII escapes and spaced separators.
pub fn python_json(value: &Value) -> String {
    match value {
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(python_json).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!(
                    "{}: {}",
                    python_json(&Value::String(k.clone())),
                    python_json(v)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::String(s) => {
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\u{0008}' => out.push_str("\\b"),
                    '\u{000c}' => out.push_str("\\f"),
                    ' '..='~' => out.push(c),
                    _ => {
                        let mut units = [0; 2];
                        for u in c.encode_utf16(&mut units) {
                            out.push_str(&format!("\\u{u:04x}"));
                        }
                    }
                }
            }
            out.push('"');
            out
        }
        Value::Number(n) if n.is_f64() => {
            let text = format!("{:?}", n.as_f64().unwrap_or(0.0));
            if let Some((mantissa, exponent)) = text.split_once('e') {
                if let Ok(exponent) = exponent.parse::<i32>() {
                    return format!(
                        "{mantissa}e{}{abs:02}",
                        if exponent < 0 { '-' } else { '+' },
                        abs = exponent.abs()
                    );
                }
            }
            text
        }
        _ => value.to_string(),
    }
}
// port: belongs in provenant-records
pub fn identity(values: &[Value]) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(
        python_json(&Value::Array(values.to_vec())).as_bytes(),
    ))
}

// port: belongs in provenant-core
pub fn iso(value: jiff::Timestamp) -> String {
    let utc = value.to_zoned(jiff::tz::TimeZone::UTC);
    let base = utc.strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = value.as_nanosecond().rem_euclid(1_000_000_000) / 1000;
    if micros == 0 {
        format!("{base}Z")
    } else {
        format!("{base}.{micros:06}Z")
    }
}

// port: belongs in provenant-records
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
