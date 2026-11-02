//! Output schemas for the MCP `structuredContent` of `fabric_run` and
//! `fabric_output`. MCP results come from `fabric-runtime`'s own tagged types
//! and flatten unknown members from newer writers, so these schemas are open:
//! they pin the tag, the stable members and the MCP-specific encodings
//! (`provider_seconds` instead of `provider_ms`, `AbsolutePath` as
//! root-plus-path-wire) and let unknown members through.
use super::schema::{
    any, array, boolean, choices, constant, nullable, number, one_of, open, path_wire, string,
    unsigned,
};
use serde_json::Value;

/// One attempt as MCP encodes it: the record with `provider_ms` replaced by
/// `provider_seconds` (`provider_ms / 1000`, null when unknown).
pub fn mcp_attempt_schema() -> Value {
    open(
        &[
            ("attempt_id", string()),
            ("run_id", string()),
            ("task_id", string()),
            ("state", any()),
            ("provider_seconds", nullable(number())),
        ],
        &[("provider_silent_for_ms", unsigned())],
    )
}

fn tagged(tag: &str, name: &str, required: &[(&str, Value)]) -> Value {
    let mut fields = vec![(tag, constant(name))];
    fields.extend(required.iter().cloned());
    open(&fields, &[])
}

/// `structuredContent` of `fabric_run`, tagged by `result`.
pub fn run_result_schema() -> Value {
    let summary = open(
        &[
            ("attempt_id", string()),
            ("run_id", string()),
            ("task_id", string()),
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
        open(&[("encoding", constant("base64")), ("data", string())], &[]),
    ])
}

/// `structuredContent` of `fabric_output`, tagged by `kind`.
pub fn output_result_schema() -> Value {
    let target = open(&[("attempt", string())], &[("host", nullable(string()))]);
    one_of(vec![
        tagged(
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
        ),
        tagged(
            "kind",
            "artifact",
            &[
                ("target", target),
                (
                    "artifact",
                    open(
                        &[("path", path_wire()), ("digest", nullable(string()))],
                        &[],
                    ),
                ),
                ("digest", string()),
                ("bytes", bytes()),
            ],
        ),
    ])
}
