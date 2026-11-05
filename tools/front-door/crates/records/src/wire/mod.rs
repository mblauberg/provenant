//! Planned typed wire projections for the CLI, MCP and generated reference.
//!
//! Each planned projection has a builder-written
//! JSON Schema (draft 2020-12) beside it; tests validate serialised values
//! against the schema, and `scripts/generate-reference --check` fails when the
//! published schema files drift from [`published`].
//!
//! # Versioning
//!
//! [`ENVELOPE_VERSION`] is the `schema_version` the CLI emits today
//! (`CommandResult::new`), and the published command schemas require it, so
//! they accept current `--json` output. [`SCHEMA_VERSION`] (2) is the planned
//! typed generation, which is not emitted yet: it will be bumped to in one
//! change with the callers, with no dual-emit period (chair decision, 2 Nov
//! 2026). The one exception to "accepts current output" is `status`: its `data`
//! schema is the planned lane-row shape, while the CLI still emits raw attempt
//! records and a numeric cursor.
//!
//! # Failure envelopes
//!
//! A non-ok status may carry `error: null` (`lanes --wait` timeout, cancel
//! receipts that are unobservable) and may carry partial `data` that is not the
//! command's success shape (a failed cancel reports `{operation_id}` only).
//! So command schemas leave `data` unconstrained on non-ok statuses and accept
//! a null or set `error`; `error.schema.json` validates only failures that
//! carry an error object.
pub mod cancel;
pub mod error;
pub mod lane;
pub mod mcp;
pub mod output;
pub mod schema;

use crate::Outcome;
use schema::{choices, constant, nullable, object, root, string};
use serde_json::{Value, json};

pub use cancel::{
    CANCEL_STATES, CancelData, CancelReceiptWire, CancelRecovered, CancelRow, CancelRowError,
};
pub use error::{CodeKind, EDGE_ERROR_CODES, EdgeErrorCode, error_schema};
pub use lane::{LaneList, LaneRow, LaneSettlement, LaneTranscript, provider_seconds};
pub use output::{ChunkBytes, OutputChunk, OutputData, ReviewerReceipt};

/// Version of the planned typed wire contract; see the module notes.
pub const SCHEMA_VERSION: u64 = 2;

/// The `schema_version` the CLI envelope carries today.
pub const ENVELOPE_VERSION: u64 = 1;

/// Base of the `$id` of every published schema.
pub const SCHEMA_ID_BASE: &str = "https://provenant.dev/schemas/v1/";

/// CLI commands with a typed `data` schema, in publication order. The name is
/// the envelope's `command` member.
pub const COMMANDS: &[&str] = &["lanes", "status", "output", "cancel"];

fn outcome_names() -> Vec<String> {
    Outcome::ALL
        .iter()
        .filter_map(|outcome| {
            serde_json::to_value(outcome)
                .ok()?
                .as_str()
                .map(str::to_owned)
        })
        .collect()
}

/// The envelope schema around a `data` schema. `data` is checked against it
/// only for ok, pending and waiting; other statuses may carry null or partial
/// data and a null or set `error`.
pub fn envelope_schema(command: &str, data: Value) -> Value {
    let statuses = outcome_names();
    let statuses: Vec<&str> = statuses.iter().map(String::as_str).collect();
    let mut schema = object(
        &[
            ("schema_version", constant(ENVELOPE_VERSION)),
            (
                "command",
                if command.is_empty() {
                    string()
                } else {
                    constant(command)
                },
            ),
            ("status", choices(&statuses)),
            ("data", json!({})),
            ("error", nullable(error_schema())),
        ],
        &[],
    );
    let failures: Vec<&str> = statuses
        .iter()
        .copied()
        .filter(|name| !matches!(*name, "ok" | "pending" | "waiting"))
        .collect();
    schema["oneOf"] = json!([
        {"properties": {"status": {"enum": ["ok", "pending", "waiting"]}, "data": data, "error": {"type": "null"}}},
        {"properties": {"status": {"enum": failures}}}
    ]);
    schema
}

/// The data schema for a command in [`COMMANDS`].
pub fn data_schema(command: &str) -> Option<Value> {
    Some(match command {
        "lanes" | "status" => lane::lane_list_schema(),
        "output" => output::output_data_schema(),
        "cancel" => cancel::cancel_data_schema(),
        _ => return None,
    })
}

/// The planned versioned schema of a command's `--json` envelope.
/// `None` for commands without a typed data schema yet.
pub fn command_schema(command: &str) -> Option<Value> {
    let data = data_schema(command)?;
    Some(root(
        &format!("{SCHEMA_ID_BASE}{command}.schema.json"),
        &format!("provenant {command} --json"),
        envelope_schema(command, data),
    ))
}

/// The schema of a failed command: any command, no data, an error envelope.
pub fn error_envelope_schema() -> Value {
    let statuses: Vec<String> = outcome_names()
        .into_iter()
        .filter(|name| !matches!(name.as_str(), "ok" | "pending" | "waiting"))
        .collect();
    let statuses: Vec<&str> = statuses.iter().map(String::as_str).collect();
    let mut schema = object(
        &[
            ("schema_version", constant(ENVELOPE_VERSION)),
            ("command", string()),
            ("status", choices(&statuses)),
            ("data", json!({})),
            ("error", error_schema()),
        ],
        &[],
    );
    if let Some(map) = schema.as_object_mut() {
        // Failed commands may still carry partial data (cancel receipts).
        // Failures with `error: null` are valid in the command schemas only.
        map.insert(
            "description".into(),
            "A refused or failed command: `error` is set.".into(),
        );
    }
    root(
        &format!("{SCHEMA_ID_BASE}error.schema.json"),
        "provenant error envelope",
        schema,
    )
}

/// Schemas MCP publishes as `outputSchema`, by tool name.
pub fn mcp_output_schema(tool: &str) -> Option<Value> {
    let (schema, title) = match tool {
        "fabric_run" => (mcp::run_result_schema(), "fabric_run structuredContent"),
        "fabric_output" => (
            mcp::output_result_schema(),
            "fabric_output structuredContent",
        ),
        _ => return None,
    };
    Some(root(
        &format!("{SCHEMA_ID_BASE}mcp/{tool}.schema.json"),
        title,
        schema,
    ))
}

/// MCP tools with a typed output schema from this module.
pub const MCP_TOOLS: &[&str] = &["fabric_run", "fabric_output"];

/// Every published schema as `(relative file name, schema)`, in stable order.
/// `scripts/generate-reference` writes them under `docs/reference/schemas/`.
pub fn published() -> Vec<(String, Value)> {
    let mut files = Vec::new();
    for command in COMMANDS {
        if let Some(schema) = command_schema(command) {
            files.push((format!("{command}.schema.json"), schema));
        }
    }
    files.push(("error.schema.json".to_owned(), error_envelope_schema()));
    for tool in MCP_TOOLS {
        if let Some(schema) = mcp_output_schema(tool) {
            files.push((format!("mcp/{tool}.schema.json"), schema));
        }
    }
    files
}

/// The edge error codes as JSON for the reference.
pub fn edge_error_codes() -> Value {
    Value::Array(
        EDGE_ERROR_CODES
            .iter()
            .map(|entry| json!({"code": entry.code, "summary": entry.summary, "kind": match entry.kind { CodeKind::Specific => "specific", CodeKind::Umbrella => "umbrella" }}))
            .collect(),
    )
}

#[cfg(test)]
mod tests;
