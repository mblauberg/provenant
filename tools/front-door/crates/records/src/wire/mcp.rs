//! Output schemas for the MCP `structuredContent` of `fabric_run` and
//! `fabric_output`. MCP results come from `fabric-runtime`'s own tagged types
//! and flatten unknown members from newer writers, so these schemas are open:
//! they pin the tag, the stable members and the MCP-specific encodings
//! (`provider_seconds` instead of `provider_ms`, `AbsolutePath` as
//! root-plus-path-wire) and let unknown members through.
use super::schema::{
    any, array, boolean, choices, constant, nullable, one_of, open, string, unsigned,
};
use serde_json::Value;

/// One projected attempt, with MCP's provider duration in seconds.
pub fn mcp_attempt_schema() -> Value {
    let mut schema = super::lane::lane_row_schema();
    schema["additionalProperties"] = true.into();
    if let Some(required) = schema["required"].as_array_mut() {
        required.push("provider_seconds".into());
    }
    schema
}

fn tagged(tag: &str, name: &str, required: &[(&str, Value)]) -> Value {
    let mut fields = vec![(tag, constant(name))];
    fields.extend(required.iter().cloned());
    open(&fields, &[])
}

/// `structuredContent` of `fabric_run`, tagged by `result`.
pub fn run_result_schema() -> Value {
    let lane = super::lane::lane_row_schema();
    let summary = open(
        &[
            ("attempt_id", string()),
            ("run_id", string()),
            ("task_id", string()),
            ("state", lane["properties"]["state"].clone()),
            ("reason", lane["properties"]["reason"].clone()),
            ("exit_code", lane["properties"]["exit_code"].clone()),
            ("finished_at", nullable(string())),
            ("retry_of", lane["properties"]["retry_of"].clone()),
            ("superseded_by", lane["properties"]["superseded_by"].clone()),
            ("cause", lane["properties"]["cause"].clone()),
        ],
        &[],
    );
    one_of(vec![
        tagged(
            "result",
            "list",
            &[(
                "page",
                open(
                    &[
                        ("items", array(summary)),
                        (
                            "next_after",
                            nullable(open(
                                &[("attempt", string())],
                                &[("host", nullable(string()))],
                            )),
                        ),
                        ("truncated", boolean()),
                    ],
                    &[],
                ),
            )],
        ),
        tagged("result", "status", &[("attempt", mcp_attempt_schema())]),
        tagged(
            "result",
            "wait",
            &[
                ("condition", any()),
                ("attempts", array(mcp_attempt_schema())),
                ("satisfied", boolean()),
                ("timed_out", boolean()),
            ],
        ),
        tagged("result", "cancel", &[("attempt", mcp_attempt_schema())]),
        tagged("result", "resume", &[("attempt", mcp_attempt_schema())]),
    ])
}

fn bytes() -> Value {
    one_of(vec![
        open(&[("encoding", constant("utf8")), ("data", string())], &[]),
        open(
            &[
                ("encoding", constant("base64")),
                ("data", super::schema::base64()),
            ],
            &[],
        ),
    ])
}

/// `structuredContent` of `fabric_output`, tagged by `kind`.
fn with_lane(mut schema: Value) -> Value {
    let lane = mcp_attempt_schema();
    if let (Some(properties), Some(fields)) = (
        schema["properties"].as_object_mut(),
        lane["properties"].as_object(),
    ) {
        properties.extend(fields.clone());
    }
    if let (Some(required), Some(fields)) = (
        schema["required"].as_array_mut(),
        lane["required"].as_array(),
    ) {
        for field in fields {
            if !required.contains(field) {
                required.push(field.clone());
            }
        }
    }
    schema
}

pub fn output_result_schema() -> Value {
    let target = open(&[("attempt", string())], &[("host", nullable(string()))]);
    one_of(vec![
        with_lane(tagged(
            "kind",
            "chunk",
            &[
                ("target", target.clone()),
                ("part", choices(&["prompt", "result", "stderr", "events"])),
                ("state", string()),
                ("reason", nullable(string())),
                ("offset", unsigned()),
                ("next_offset", unsigned()),
                ("eof", boolean()),
                ("digest", string()),
                ("bytes", bytes()),
            ],
        )),
        with_lane(tagged(
            "kind",
            "artifact",
            &[
                ("target", target),
                (
                    "artifact",
                    open(
                        &[
                            ("path", super::schema::worktree_path()),
                            ("digest", nullable(string())),
                        ],
                        &[],
                    ),
                ),
                ("digest", string()),
                ("bytes", bytes()),
            ],
        )),
    ])
}
