// port: belongs in provenant-core
// Unicode decimal digits, as accepted by Python re \d and int.
pub(crate) fn decimal_digit(c: char) -> Option<u8> {
    let code = u32::from(c);
    // port: belongs in provenant-core
    const ZEROS: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
        0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
        0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
        0xff10, 0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450,
        0x114d0, 0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50,
        0x11d50, 0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce,
        0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
    ];
    ZEROS
        .iter()
        .find_map(|&zero| code.checked_sub(zero).filter(|&d| d < 10).map(|d| d as u8))
}

pub(crate) use provenant_core::py::{
    python_json_eq as python_equal, python_repr, python_str, python_strip, python_truthy,
    python_whitespace,
};

/// Crate bound and error mapping around the records owner's checked codec.
pub fn decode_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> crate::Result<T> {
    if bytes.len() > crate::MAX_JSON_BYTES {
        return Err(crate::Error::InputTooLarge {
            limit: crate::MAX_JSON_BYTES,
        });
    }
    let value =
        provenant_records::checked_json::from_slice(bytes).map_err(|error| match error {
            provenant_records::DecodeError::NumberOutOfRange(_) => crate::Error::NumberOutOfRange,
            provenant_records::DecodeError::Json(error) => crate::Error::Json(error),
        })?;
    Ok(serde_json::from_value(value)?)
}

// port: belongs in provenant-core
pub(crate) fn python_alpha(text: &str) -> bool {
    static LETTERS: std::sync::LazyLock<std::result::Result<regex::Regex, regex::Error>> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\A\p{L}+\z"));
    LETTERS.as_ref().is_ok_and(|r| r.is_match(text))
}

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
    /// Original spelling retained by the selection/normalisation owner.
    pub model_raw: Option<String>,
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
    pub task_id: Option<String>,
    pub pr: Option<serde_json::Value>,
    pub provider: Option<String>,
    pub account: Option<String>,
    pub model: Option<String>,
    pub harness_rev: Option<String>,
    pub attrs: Value,
    /// Merge refreshed attributes when the existing ledger event ID conflicts.
    #[serde(default)]
    pub refresh: bool,
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
    pub used_percent: crate::Numeric,
    pub window_minutes: Option<crate::Numeric>,
    pub reset_at: Option<String>,
}
// port: belongs in provenant-core
pub fn parse_time(value: &str) -> crate::Result<jiff::Timestamp> {
    let mut parsed = provenant_core::py::python_fromisoformat(&value.replace('Z', "+00:00"))
        .map_err(|_| crate::Error::Timestamp(value.into()))?;
    if parsed.offset_microseconds.is_none() {
        parsed.offset_microseconds = Some(0);
    }
    parsed
        .timestamp()
        .and_then(|t| t.to_jiff())
        .map_err(|_| crate::Error::Timestamp(value.into()))
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

// port: belongs in provenant-core
pub fn timestamp_seconds(seconds: f64) -> crate::Result<jiff::Timestamp> {
    provenant_core::py::from_timestamp(seconds)
        .and_then(|timestamp| timestamp.to_jiff())
        .map_err(|_| crate::Error::Timestamp(seconds.to_string()))
}
