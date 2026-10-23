//! Success evidence validation shared by dispatch, recovery and collection.
//!
//! Inputs remain JSON values until validated so malformed retained records get
//! the same refusal reason as the old reader, including booleans in exit fields.
use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use thiserror::Error;

use crate::{digest_bytes, pending};

#[derive(Debug, Error, PartialEq, Eq)]
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
    AdapterJson,
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
    let text = std::str::from_utf8(adapter_bytes).map_err(|_| AdapterJson)?;
    // Python str.splitlines includes VT, FF, separators and NEL as well as CR/LF.
    let last = text
        .split([
            '\n', '\r', '\u{000b}', '\u{000c}', '\u{001c}', '\u{001d}', '\u{001e}', '\u{0085}',
            '\u{2028}', '\u{2029}',
        ])
        .rfind(|line| !line.trim().is_empty());
    let adapter: Value = match last {
        Some(line) => serde_json::from_str(line).map_err(|_| AdapterJson)?,
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
