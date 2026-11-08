//! Decoding for the configurable Codex cloud thread command's JSON protocol.
use crate::cloud::{CloudResult, RemoteJob, RemoteState};
use serde_json::Value;

/// Bound diagnostics by bytes, including when the response contains Unicode.
fn seen(output: &str) -> String {
    let mut end = output.len().min(2048);
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    let suffix = if end < output.len() { "…" } else { "" };
    format!("{}{suffix}", &output[..end])
}

fn unexpected(context: &str, output: &str) -> String {
    format!("unrecognised codex thread {context}: {}", seen(output))
}

pub(super) fn submission_id(output: &str) -> Result<String, String> {
    let output = super::strip_ansi(output);
    // Progress lines precede a possibly pretty-printed JSON object. Prefer its
    // explicit threadId over any progress-line identity.
    for (offset, _) in output.match_indices('{').take(64) {
        // Read one complete value; diagnostics may follow it.
        if let Some(Ok(value)) = serde_json::Deserializer::from_str(&output[offset..])
            .into_iter::<Value>()
            .next()
            && let Some(id) = value.get("threadId").and_then(Value::as_str)
            && !id.trim().is_empty()
        {
            return Ok(id.to_owned());
        }
    }
    for line in output.lines() {
        if let Some(id) = line.trim().strip_prefix("Created cloud thread ") {
            let id = id.trim();
            if !id.is_empty() && !id.contains(char::is_whitespace) {
                return Ok(id.to_owned());
            }
        }
    }
    Err(unexpected("submission identity", &output))
}

/// Only text/content within an agentMessage is eligible as result text. Tool
/// output and userMessage contents cannot become verification evidence.
fn message_text(value: &Value, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => parts
            .iter()
            .map(|part| message_text(part, depth + 1))
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join("\n")),
        Value::Object(_) => value
            .get("text")
            .or_else(|| value.get("content"))
            .and_then(|text| message_text(text, depth + 1)),
        _ => None,
    }
}

fn result_text(text: &str) -> Result<CloudResult, String> {
    if let Ok(result) = serde_json::from_str(text) {
        return Ok(result);
    }
    // The final response may wrap the result file's JSON in Markdown or prose.
    // Decode whole JSON values rather than matching field-like substrings.
    for (offset, _) in text.match_indices('{').take(64) {
        let mut values = serde_json::Deserializer::from_str(&text[offset..]).into_iter::<Value>();
        if let Some(Ok(value)) = values.next()
            && let Ok(result) = serde_json::from_value(value)
        {
            return Ok(result);
        }
    }
    Err(unexpected("final agentMessage result JSON", text))
}

pub(super) fn observe(output: &str, job: &RemoteJob) -> Result<RemoteState, String> {
    let value: Value =
        serde_json::from_str(output).map_err(|_| unexpected("status JSON", output))?;
    let thread = value
        .get("thread")
        .ok_or_else(|| unexpected("thread", output))?;
    if thread.get("id").and_then(Value::as_str) != Some(job.remote_id.as_str()) {
        return Err(unexpected("thread identity", output));
    }
    let turns = value
        .pointer("/turns/data/values")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected("turns", output))?;
    let Some(turn) = turns.last() else {
        // A just-created thread may be queued before its first turn is visible.
        if ["active", "queued"].iter().any(|status| {
            thread
                .get("status")
                .and_then(|value| value.get(*status))
                .is_some_and(|value| value.is_object() || value.as_bool() == Some(true))
        }) {
            return Ok(RemoteState::Running);
        }
        return Err(unexpected("empty turns", output));
    };
    let status = turn
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| unexpected("turn status", output))?;
    let status = status
        .strip_prefix("TURN_STATUS_")
        .unwrap_or(status)
        .to_ascii_uppercase();
    match status.as_str() {
        "IN_PROGRESS" | "QUEUED" | "PENDING" => Ok(RemoteState::Running),
        "FAILED" | "CANCELLED" | "CANCELED" => Ok(RemoteState::Failed {
            detail: format!("Codex thread turn {status}: {}", seen(&turn.to_string())),
        }),
        "COMPLETED" => {
            let items = turn
                .pointer("/itemsPage/data/values")
                .and_then(Value::as_array)
                .ok_or_else(|| unexpected("completed turn items", output))?;
            let message = items
                .iter()
                .rev()
                .find_map(|row| row.pointer("/item/agentMessage"))
                .ok_or_else(|| unexpected("completed turn agentMessage", output))?;
            let text =
                message_text(message, 0).ok_or_else(|| unexpected("agentMessage text", output))?;
            let mut result = result_text(&text)?;
            // A thread model is provider observation; the requested model is
            // never substituted when the service does not expose one.
            result.observed_model = thread
                .get("model")
                .and_then(Value::as_str)
                .filter(|model| !model.trim().is_empty())
                .map(str::to_owned);
            result
                .clone()
                .check(&job.job_id, &job.nonce, &job.commit, &job.requested_model)
                .map_err(|error| error.to_string())?;
            Ok(RemoteState::Finished { result })
        }
        _ => Err(unexpected("turn status", output)),
    }
}
