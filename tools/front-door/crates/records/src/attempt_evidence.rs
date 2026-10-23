//! Success evidence validation shared by dispatch, recovery and collection.
//!
//! Inputs remain JSON values until validated so malformed retained records get
//! the same refusal reason as the old reader, including booleans in exit fields.
use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use thiserror::Error;

use crate::{digest_bytes, pending};

#[derive(Debug, Error)]
pub enum AttemptEvidenceError {
    #[error("successful attempt status is invalid")]
    Status,
    #[error("successful attempt does not prove process exit")]
    ProcessExit,
    #[error("successful attempt does not prove exit 0")]
    ExitCode,
    #[error("successful attempt has no result evidence")]
    MissingResult,
    #[error("successful attempt result is empty")]
    EmptyResult,
    #[error("successful attempt result digest is invalid")]
    ResultDigest,
    #[error("successful attempt has no adapter receipt")]
    MissingAdapter,
    #[error("successful adapter receipt is not valid JSON")]
    AdapterJson(#[source] JsonCause),
    #[error("attempt record is not valid JSON")]
    RecordJson(#[source] JsonCause),
    #[error("successful adapter receipt is invalid")]
    Adapter,
    #[error("successful attempt route is invalid")]
    Route,
    #[error("successful adapter receipt is missing route identity")]
    RouteIdentity,
    #[error("successful adapter receipt does not match the requested adapter")]
    RequestedAdapter,
    #[error("successful adapter receipt does not match the requested intent")]
    RequestedIntent,
    #[error("successful adapter receipt does not match the retained output")]
    Output,
    #[error("successful adapter receipt must record exit 0")]
    AdapterExit,
    #[error("successful adapter receipt is missing assurance flags")]
    AssuranceFlags,
    #[error("ordinary execution cannot be certification eligible")]
    OrdinaryCertification,
    #[error("successful attempt route disagrees with adapter receipt: {0}")]
    RouteDisagreement(&'static str),
}

#[derive(Debug, Error)]
pub enum JsonCause {
    #[error("invalid UTF-8")]
    Utf8(#[source] std::str::Utf8Error),
    #[error("invalid JSON syntax")]
    Json(#[source] serde_json::Error),
    #[error("{0}")]
    NumberOutOfRange(#[source] crate::NumberOutOfRange),
}

/// Validate original record JSON without losing Python's integer `-0` spelling.
/// The temporary policy projection must never be used to rewrite retained bytes.
/// Callers already holding a correctly decoded value may use the value API.
pub fn validate_successful_attempt_json(
    run_dir: &Path,
    record_bytes: &[u8],
    payloads: &BTreeMap<String, Vec<u8>>,
) -> Result<(), AttemptEvidenceError> {
    let text = std::str::from_utf8(record_bytes)
        .map_err(|source| AttemptEvidenceError::RecordJson(JsonCause::Utf8(source)))?;
    let record = decode_validation_json(text)
        .map_err(|source| AttemptEvidenceError::RecordJson(json_cause(source)))?;
    validate_successful_attempt(run_dir, &record, payloads)
}

/// Preserve all statuses except the retained pre-upgrade success alias.
pub fn canonical_success_status(status: &Value) -> Value {
    if status == "succeeded" {
        Value::String("ok".into())
    } else {
        status.clone()
    }
}

pub fn is_success_status(status: &Value) -> bool {
    status == "ok" || status == "succeeded"
}

pub fn successful_adapter_error(
    adapter: &Value,
    expected_adapter: &Value,
    expected_intent: &Value,
    result_path: &Path,
    result_digest: &str,
) -> Option<AttemptEvidenceError> {
    use AttemptEvidenceError::*;
    const REQUIRED: &[&str] = &[
        "tool",
        "adapter",
        "execution_intent",
        "resolved_model",
        "provider_family",
        "model_family",
        "endpoint_provider",
        "identity_source",
        "output_path",
        "output_digest",
        "read_only_guarantee",
    ];
    if REQUIRED
        .iter()
        .any(|key| adapter[key].as_str().is_none_or(str::is_empty))
    {
        return Some(RouteIdentity);
    }
    if adapter["tool"] != *expected_adapter || adapter["adapter"] != *expected_adapter {
        return Some(RequestedAdapter);
    }
    if adapter["execution_intent"] != *expected_intent {
        return Some(RequestedIntent);
    }
    let output_matches = adapter["output_path"].as_str().is_some_and(|output| {
        match (
            pending::resolve(Path::new(output)),
            pending::resolve(result_path),
        ) {
            (Ok(output), Ok(result)) => output == result,
            _ => false,
        }
    });
    if !output_matches || adapter["output_digest"] != result_digest {
        return Some(Output);
    }
    if adapter["exit"].as_i64() != Some(0) {
        return Some(AdapterExit);
    }
    if !adapter["cross_family"].is_boolean() || !adapter["certification_eligible"].is_boolean() {
        return Some(AssuranceFlags);
    }
    if expected_intent == "ordinary" && adapter["certification_eligible"] == true {
        return Some(OrdinaryCertification);
    }
    None
}

fn json_cause(error: crate::DecodeError) -> JsonCause {
    match error {
        crate::DecodeError::Json(error) => JsonCause::Json(error),
        crate::DecodeError::NumberOutOfRange(error) => JsonCause::NumberOutOfRange(error),
    }
}

// Python accepts nonfinite floats; they cannot satisfy numeric success fields.
// Only that validation projection maps them to null; integers use the shared
// checked codec and are never rounded or silently discarded.
fn decode_validation_json(line: &str) -> Result<Value, crate::DecodeError> {
    let mut normalized = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if ch == '"' {
            normalized.push(ch);
            while let Some((_, ch)) = chars.next() {
                normalized.push(ch);
                if ch == '\\' {
                    if let Some((_, escaped)) = chars.next() {
                        normalized.push(escaped);
                    }
                } else if ch == '"' {
                    break;
                }
            }
        } else if matches!(
            ch,
            '{' | '}' | '[' | ']' | ',' | ':' | ' ' | '\t' | '\r' | '\n'
        ) {
            normalized.push(ch);
        } else {
            let mut end = start + ch.len_utf8();
            while let Some(&(index, next)) = chars.peek() {
                if matches!(
                    next,
                    '{' | '}' | '[' | ']' | ',' | ':' | ' ' | '\t' | '\r' | '\n' | '"'
                ) {
                    break;
                }
                end = index + next.len_utf8();
                chars.next();
            }
            let token = &line[start..end];
            let beyond_range = token.contains(['.', 'e', 'E'])
                && serde_json::from_str::<Value>(token).is_err()
                && serde_json::from_str::<serde::de::IgnoredAny>(token).is_ok();
            normalized.push_str(if token == "-0" {
                "0"
            } else if matches!(token, "NaN" | "Infinity" | "-Infinity") || beyond_range {
                "null"
            } else {
                token
            });
        }
    }
    crate::checked_json::from_str(&normalized)
}

pub fn validate_successful_attempt(
    run_dir: &Path,
    record: &Value,
    payloads: &BTreeMap<String, Vec<u8>>,
) -> Result<(), AttemptEvidenceError> {
    use AttemptEvidenceError::*;
    if !is_success_status(&record["status"]) {
        return Err(Status);
    }
    if !record["process"].is_object() || record["process"]["observed_exit"] != true {
        return Err(ProcessExit);
    }
    if record["process"]["exit_code"].as_i64() != Some(0) {
        return Err(ExitCode);
    }
    let result_bytes = payloads
        .get("result")
        .filter(|_| record["result"].is_object())
        .ok_or(MissingResult)?;
    if result_bytes.is_empty() {
        return Err(EmptyResult);
    }
    let result_digest = digest_bytes(result_bytes);
    let result_path = record["result"]["path"].as_str().ok_or(ResultDigest)?;
    if record["result"]["digest"] != result_digest {
        return Err(ResultDigest);
    }
    let adapter_bytes = payloads.get("adapter_receipt").ok_or(MissingAdapter)?;
    let text = std::str::from_utf8(adapter_bytes)
        .map_err(|source| AdapterJson(JsonCause::Utf8(source)))?;
    // Python str.splitlines includes VT, FF, separators and NEL as well as CR/LF.
    let last = text
        .split([
            '\n', '\r', '\u{000b}', '\u{000c}', '\u{001c}', '\u{001d}', '\u{001e}', '\u{0085}',
            '\u{2028}', '\u{2029}',
        ])
        .rfind(|line| !pending::python_strip(line).is_empty());
    let adapter: Value = match last {
        Some(line) => {
            decode_validation_json(line).map_err(|source| AdapterJson(json_cause(source)))?
        }
        None => Value::Null,
    };
    if !adapter.is_object() || adapter["status"] != "ok" {
        return Err(Adapter);
    }
    if !record["requested_route"].is_object() || !record["route"].is_object() {
        return Err(Route);
    }
    if let Some(error) = successful_adapter_error(
        &adapter,
        &record["requested_route"]["adapter"],
        &record["requested_route"]["intent"],
        &run_dir.join(result_path),
        &result_digest,
    ) {
        return Err(error);
    }
    for field in [
        "adapter",
        "execution_intent",
        "provider_family",
        "resolved_model",
    ] {
        if record["route"][field] != adapter[field] {
            return Err(RouteDisagreement(field));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
