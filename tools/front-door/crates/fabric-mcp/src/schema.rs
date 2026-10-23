//! The explicit MCP field catalogue. Runtime schemas are generated only from
//! this table; tests deserialise its examples through the actual serde enums.
use serde_json::{Value, json};

#[derive(Clone)]
struct Field {
    name: &'static str,
    schema: Value,
    required: bool,
    example: Value,
}

struct Operation {
    name: &'static str,
    fields: Vec<Field>,
}

struct Tool {
    name: &'static str,
    description: &'static str,
    operations: Vec<Operation>,
}

fn field(name: &'static str, schema: Value, required: bool, example: Value) -> Field {
    Field {
        name,
        schema,
        required,
        example,
    }
}
fn string() -> Value {
    json!({"type":"string"})
}
fn id() -> Value {
    json!({"type":"string","pattern":"^[0-9a-f]{32}$"})
}
fn int() -> Value {
    json!({"type":"integer","minimum":0,"maximum":18446744073709551615_u64})
}
fn signed_int() -> Value {
    json!({"type":"integer","minimum":0,"maximum":9223372036854775807_i64})
}
fn concurrency() -> Value {
    json!({"type":"integer","minimum":1,"maximum":100})
}
fn lease_ms() -> Value {
    json!({"type":"integer","minimum":1000,"maximum":3600000})
}
fn text(maximum: usize) -> Value {
    json!({"type":"string","minLength":1,"maxLength":maximum,"pattern":r"^[^\u0000]*$","x-provenant-maxUtf8Bytes":maximum,"description":format!("At most {maximum} UTF-8 bytes; NUL is refused.")})
}
fn wait_ms() -> Value {
    json!({"type":"integer","minimum":1000,"maximum":55000})
}
fn string_enum(values: &[&str]) -> Value {
    json!({"type":"string","enum":values})
}
fn nullable(schema: Value) -> Value {
    let mut output = json!({"anyOf":[schema,{"type":"null"}]});
    if let Some(any_of) = output.get_mut("anyOf").and_then(Value::as_array_mut) {
        any_of.sort_by_key(|value| value.get("type").and_then(Value::as_str) == Some("null"));
    }
    output
}
fn array(items: Value, min: usize, max: usize) -> Value {
    json!({"type":"array","items":items,"minItems":min,"maxItems":max})
}
fn object(fields: Vec<Field>) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for field in fields {
        if field.required {
            required.push(field.name);
        }
        properties.insert(field.name.into(), field.schema);
    }
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn tagged(tag: &str, variants: &[(&str, Vec<Field>)]) -> Value {
    let choices: Vec<Value> = variants.iter().map(|(name, fields)| {
        let mut properties = serde_json::Map::new();
        properties.insert(tag.to_owned(), json!({"const":name}));
        let mut required = vec![tag];
        for field in fields {
            if field.required { required.push(field.name); }
            properties.insert(field.name.into(), field.schema.clone());
        }
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
    }).collect();
    json!({"oneOf":choices})
}
fn example(tag: &str, operation: &str, fields: &[Field]) -> Value {
    let mut value = serde_json::Map::new();
    value.insert(tag.into(), json!(operation));
    for field in fields.iter().filter(|field| field.required) {
        value.insert(field.name.into(), field.example.clone());
    }
    Value::Object(value)
}

fn path_wire_schema() -> Value {
    object(vec![
        field(
            "encoding",
            string_enum(&["utf8", "native_bytes"]),
            true,
            json!("utf8"),
        ),
        field("path", string(), true, json!("fixture.txt")),
    ])
}
fn artifact_schema() -> Value {
    object(vec![
        field(
            "path",
            path_wire_schema(),
            true,
            json!({"encoding":"utf8","path":"fixture.txt"}),
        ),
        field(
            "digest",
            nullable(json!({"type":"string","pattern":"^sha256:[0-9a-f]{64}$"})),
            false,
            Value::Null,
        ),
    ])
}
fn artifact_example() -> Value {
    json!({"path":{"encoding":"utf8","path":"fixture.txt"},"digest":null})
}
fn token_schema() -> Value {
    array(json!({"type":"integer","minimum":0,"maximum":255}), 16, 16)
}
fn token_example() -> Value {
    json!([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
}
fn proof_schema(fields: Vec<Field>) -> Value {
    object(fields)
}
fn task_proof_schema() -> Value {
    proof_schema(vec![
        field(
            "task",
            id(),
            true,
            json!("00000000000000000000000000000001"),
        ),
        field("generation", int(), true, json!(1)),
        field("token", token_schema(), true, token_example()),
    ])
}
fn claim_proof_schema() -> Value {
    proof_schema(vec![
        field("id", id(), true, json!("00000000000000000000000000000001")),
        field("generation", int(), true, json!(1)),
        field("token", token_schema(), true, token_example()),
    ])
}
fn landing_proof_schema() -> Value {
    proof_schema(vec![
        field("id", id(), true, json!("00000000000000000000000000000001")),
        field("generation", int(), true, json!(1)),
        field("token", token_schema(), true, token_example()),
        field("reference", string(), true, json!("refs/heads/main")),
    ])
}
fn work_scope_schema() -> Value {
    tagged(
        "kind",
        &[
            ("issue", vec![field("key", string(), true, json!("PROV-1"))]),
            (
                "paths",
                vec![field(
                    "paths",
                    array(string(), 1, 100),
                    true,
                    json!(["src/lib.rs"]),
                )],
            ),
            (
                "worktree",
                vec![field("path", string(), true, json!("/tmp/worktree"))],
            ),
        ],
    )
}
fn work_scope_example() -> Value {
    json!({"kind":"issue","key":"PROV-1"})
}
fn artifact_array() -> Value {
    array(artifact_schema(), 0, 100)
}
fn page_limit() -> Value {
    json!({"type":"integer","minimum":1,"maximum":100})
}
fn task_state_schema() -> Value {
    string_enum(&["open", "active", "done", "cancelled"])
}
fn message_kind_schema() -> Value {
    string_enum(&["note", "request", "response"])
}

fn tools() -> Vec<Tool> {
    let id_value = json!("00000000000000000000000000000001");
    let artifact = artifact_example();
    let message_ref = json!({"scope":"local","id":"00000000000000000000000000000001"});
    let task_ref = json!({"scope":"local","id":"00000000000000000000000000000001"});
    let work_proof = json!({"id":id_value,"generation":1,"token":token_example()});
    let task_proof = json!({"task":id_value,"generation":1,"token":token_example()});
    let landing_proof =
        json!({"id":id_value,"generation":1,"token":token_example(),"reference":"refs/heads/main"});
    vec![
        Tool {
            name: "fabric_identity",
            description: "Inspect the authenticated project, host and agent identity.",
            operations: vec![Operation {
                name: "inspect",
                fields: vec![],
            }],
        },
        Tool {
            name: "fabric_mail",
            description: "Send, read and mark project-scoped agent mail.",
            operations: vec![
                Operation {
                    name: "send",
                    fields: vec![
                        field(
                            "recipients",
                            {
                                let mut recipients = array(id(), 1, 100);
                                recipients["uniqueItems"] = json!(true);
                                recipients
                            },
                            true,
                            json!([id_value]),
                        ),
                        field("kind", message_kind_schema(), true, json!("note")),
                        field("body", text(65_536), true, json!("Status update")),
                        field("artifacts", artifact_array(), true, json!([artifact])),
                        field(
                            "reply_to",
                            nullable(json!({"oneOf":[
                                object(vec![field("scope",string_enum(&["local"]),true,json!("local")),field("id",id(),true,id_value.clone())]),
                                object(vec![field("scope",string_enum(&["remote"]),true,json!("remote")),field("host",id(),true,id_value.clone()),field("id",id(),true,id_value.clone())]),
                                object(vec![field("scope",string_enum(&["legacy"]),true,json!("legacy")),field("reference",object(vec![field("source",string(),true,json!("agent-fabric")),field("project",string(),true,json!("project")),field("id",string(),true,json!("old-id"))]),true,json!({"source":"agent-fabric","project":"project","id":"old-id"}))])
                            ]})),
                            false,
                            message_ref,
                        ),
                        field(
                            "task",
                            nullable(json!({"oneOf":[
                                object(vec![field("scope",string_enum(&["local"]),true,json!("local")),field("id",id(),true,id_value.clone())]),
                                object(vec![field("scope",string_enum(&["remote"]),true,json!("remote")),field("host",id(),true,id_value.clone()),field("id",id(),true,id_value.clone())]),
                                object(vec![field("scope",string_enum(&["legacy"]),true,json!("legacy")),field("reference",object(vec![field("source",string(),true,json!("agent-fabric")),field("project",string(),true,json!("project")),field("id",string(),true,json!("old-id"))]),true,json!({"source":"agent-fabric","project":"project","id":"old-id"}))])
                            ]})),
                            false,
                            task_ref,
                        ),
                    ],
                },
                Operation {
                    name: "read",
                    fields: vec![
                        field("after", nullable(signed_int()), false, json!(0)),
                        field("limit", page_limit(), true, json!(20)),
                    ],
                },
                Operation {
                    name: "mark_read",
                    fields: vec![field("through", signed_int(), true, json!(1))],
                },
            ],
        },
        Tool {
            name: "fabric_task",
            description: "Create and manage fenced project tasks.",
            operations: vec![
                Operation {
                    name: "create",
                    fields: vec![
                        field(
                            "objective",
                            text(65_536),
                            true,
                            json!("Implement the change"),
                        ),
                        field("note", nullable(text(65_536)), false, json!("Details")),
                    ],
                },
                Operation {
                    name: "list",
                    fields: vec![
                        field("state", nullable(task_state_schema()), false, json!("open")),
                        field("after", nullable(id()), false, id_value.clone()),
                        field("limit", page_limit(), true, json!(20)),
                    ],
                },
                Operation {
                    name: "claim",
                    fields: vec![
                        field("id", id(), true, id_value.clone()),
                        field("lease_ms", lease_ms(), true, json!(60000)),
                    ],
                },
                Operation {
                    name: "update",
                    fields: vec![
                        field("proof", task_proof_schema(), true, task_proof.clone()),
                        field("objective", text(65_536), true, json!("Updated objective")),
                        field("note", nullable(text(65_536)), false, json!("New note")),
                    ],
                },
                Operation {
                    name: "finish",
                    fields: vec![
                        field("proof", task_proof_schema(), true, task_proof.clone()),
                        field(
                            "state",
                            string_enum(&["done", "cancelled"]),
                            true,
                            json!("done"),
                        ),
                    ],
                },
                Operation {
                    name: "release",
                    fields: vec![field("proof", task_proof_schema(), true, task_proof)],
                },
            ],
        },
        Tool {
            name: "fabric_events",
            description: "Read the activity journal or append a note.",
            operations: vec![
                Operation {
                    name: "read",
                    fields: vec![
                        field("after", nullable(signed_int()), false, json!(0)),
                        field("limit", page_limit(), true, json!(20)),
                        field("wait_ms", nullable(wait_ms()), false, json!(1000)),
                    ],
                },
                Operation {
                    name: "note",
                    fields: vec![
                        field("note", text(65_536), true, json!("Deployment reviewed")),
                        field("dedup_key", nullable(text(1024)), false, json!("review-1")),
                    ],
                },
            ],
        },
        Tool {
            name: "fabric_claim",
            description: "Acquire, inspect and release advisory work claims.",
            operations: vec![
                Operation {
                    name: "acquire",
                    fields: vec![
                        field("scope", work_scope_schema(), true, work_scope_example()),
                        field("lease_ms", lease_ms(), true, json!(60000)),
                    ],
                },
                Operation {
                    name: "renew",
                    fields: vec![
                        field("proof", claim_proof_schema(), true, work_proof.clone()),
                        field("lease_ms", lease_ms(), true, json!(60000)),
                    ],
                },
                Operation {
                    name: "verify",
                    fields: vec![field(
                        "proof",
                        claim_proof_schema(),
                        true,
                        work_proof.clone(),
                    )],
                },
                Operation {
                    name: "release",
                    fields: vec![field("proof", claim_proof_schema(), true, work_proof)],
                },
                Operation {
                    name: "list",
                    fields: vec![
                        field("after", nullable(id()), false, id_value.clone()),
                        field("limit", page_limit(), true, json!(20)),
                    ],
                },
            ],
        },
        Tool {
            name: "fabric_landing",
            description: "Manage a fenced landing lease and observe a generation.",
            operations: vec![
                Operation {
                    name: "acquire",
                    fields: vec![
                        field("reference", string(), true, json!("refs/heads/main")),
                        field(
                            "expected",
                            json!({"type":"string","pattern":"^(?:[0-9a-f]{40}|[0-9a-f]{64})$"}),
                            true,
                            json!("1111111111111111111111111111111111111111"),
                        ),
                        field(
                            "candidate",
                            json!({"type":"string","pattern":"^(?:[0-9a-f]{40}|[0-9a-f]{64})$"}),
                            true,
                            json!("2222222222222222222222222222222222222222"),
                        ),
                        field("lease_ms", lease_ms(), true, json!(60000)),
                    ],
                },
                Operation {
                    name: "renew",
                    fields: vec![
                        field("proof", landing_proof_schema(), true, landing_proof.clone()),
                        field("lease_ms", lease_ms(), true, json!(60000)),
                    ],
                },
                Operation {
                    name: "verify",
                    fields: vec![field(
                        "proof",
                        landing_proof_schema(),
                        true,
                        landing_proof.clone(),
                    )],
                },
                Operation {
                    name: "release",
                    fields: vec![field("proof", landing_proof_schema(), true, landing_proof)],
                },
                Operation {
                    name: "wait",
                    fields: vec![
                        field("reference", string(), true, json!("refs/heads/main")),
                        field("generation", int(), true, json!(1)),
                        field("wait_ms", wait_ms(), true, json!(1000)),
                    ],
                },
            ],
        },
        Tool {
            name: "fabric_submit",
            description: "Submit one batch of tasks to the run service.",
            operations: vec![Operation {
                name: "submission",
                fields: vec![
                    field(
                        "operation_id",
                        json!({"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"}),
                        true,
                        json!("submit-1"),
                    ),
                    field(
                        "tasks",
                        array(
                            object(vec![
                                field("prompt", text(65_536), true, json!("Review the patch")),
                                field("inputs", artifact_array(), false, json!([artifact])),
                                field(
                                    "route",
                                    object(vec![
                                        field("adapter", nullable(string()), false, json!("codex")),
                                        field("model", nullable(string()), false, json!("model")),
                                        field("role", nullable(string()), false, json!("review")),
                                        field(
                                            "task_class",
                                            nullable(string()),
                                            false,
                                            json!("code_review"),
                                        ),
                                        field("effort", nullable(string()), false, json!("high")),
                                    ]),
                                    true,
                                    json!({"adapter":"codex","model":"model","role":"review","task_class":"code_review","effort":"high"}),
                                ),
                                field(
                                    "timeout_ms",
                                    json!({"type":"integer","minimum":1000,"maximum":14400000}),
                                    true,
                                    json!(60000),
                                ),
                                field("delivery_id", nullable(id()), false, json!(id_value)),
                            ]),
                            1,
                            100,
                        ),
                        true,
                        json!([{"prompt":"Review the patch","inputs":[],"route":{"adapter":"codex"},"timeout_ms":60000}]),
                    ),
                    field("concurrency", concurrency(), true, json!(1)),
                ],
            }],
        },
        Tool {
            name: "fabric_run",
            description: "List, inspect, wait for, cancel or resume runs.",
            operations: vec![
                Operation {
                    name: "list",
                    fields: vec![
                        field(
                            "after",
                            nullable(json!({"$ref":"#/$defs/AttemptTarget"})),
                            false,
                            json!({"attempt":id_value}),
                        ),
                        field("limit", page_limit(), true, json!(20)),
                    ],
                },
                Operation {
                    name: "status",
                    fields: vec![field(
                        "target",
                        json!({"$ref":"#/$defs/AttemptTarget"}),
                        true,
                        json!({"attempt":id_value}),
                    )],
                },
                Operation {
                    name: "wait",
                    fields: vec![
                        field(
                            "targets",
                            array(json!({"$ref":"#/$defs/AttemptTarget"}), 1, 100),
                            true,
                            json!([{"attempt":id_value}]),
                        ),
                        field(
                            "condition",
                            string_enum(&["any", "all"]),
                            true,
                            json!("any"),
                        ),
                        field("wait_ms", wait_ms(), true, json!(1000)),
                    ],
                },
                Operation {
                    name: "cancel",
                    fields: vec![
                        field(
                            "target",
                            json!({"$ref":"#/$defs/AttemptTarget"}),
                            true,
                            json!({"attempt":id_value}),
                        ),
                        field(
                            "operation_id",
                            json!({"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"}),
                            true,
                            json!("cancel-1"),
                        ),
                    ],
                },
                Operation {
                    name: "resume",
                    fields: vec![
                        field(
                            "target",
                            json!({"$ref":"#/$defs/AttemptTarget"}),
                            true,
                            json!({"attempt":id_value}),
                        ),
                        field(
                            "operation_id",
                            json!({"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"}),
                            true,
                            json!("resume-1"),
                        ),
                        field("prompt", text(65_536), true, json!("Continue the review")),
                        field("expected_generation", int(), true, json!(1)),
                    ],
                },
            ],
        },
        Tool {
            name: "fabric_output",
            description: "Read bounded output or fetch a manifest artefact.",
            operations: vec![
                Operation {
                    name: "read",
                    fields: vec![
                        field(
                            "target",
                            json!({"$ref":"#/$defs/AttemptTarget"}),
                            true,
                            json!({"attempt":id_value}),
                        ),
                        field(
                            "part",
                            string_enum(&["stdout", "stderr", "combined"]),
                            true,
                            json!("stdout"),
                        ),
                        field("offset", int(), true, json!(0)),
                        field(
                            "max_bytes",
                            json!({"type":"integer","minimum":1,"maximum":65536}),
                            true,
                            json!(4096),
                        ),
                    ],
                },
                Operation {
                    name: "fetch",
                    fields: vec![
                        field(
                            "target",
                            json!({"$ref":"#/$defs/AttemptTarget"}),
                            true,
                            json!({"attempt":id_value}),
                        ),
                        field("artifact", artifact_schema(), true, artifact),
                        field(
                            "max_bytes",
                            json!({"type":"integer","minimum":1,"maximum":1048576}),
                            true,
                            json!(65536),
                        ),
                    ],
                },
            ],
        },
        Tool {
            name: "fabric_hosts",
            description: "List configured peer hosts and readiness observations.",
            operations: vec![Operation {
                name: "list",
                fields: vec![
                    field("after", nullable(id()), false, id_value.clone()),
                    field("limit", page_limit(), true, json!(20)),
                ],
            }],
        },
    ]
}

fn tool_schema(tool: &Tool) -> Value {
    let variants: Vec<(&str, Vec<Field>)> = tool
        .operations
        .iter()
        .map(|operation| (operation.name, operation.fields.clone()))
        .collect();
    let mut input = tagged("op", &variants);
    input["type"] = json!("object");
    if tool.name == "fabric_run" || tool.name == "fabric_output" {
        input["$defs"] = json!({"AttemptTarget":object(vec![field("attempt",id(),true,json!("00000000000000000000000000000001")),field("host",nullable(id()),false,id_value_example())])});
    }
    json!({"name":tool.name,"description":tool.description,"inputSchema":input})
}
fn id_value_example() -> Value {
    json!("00000000000000000000000000000001")
}

pub fn catalogue() -> Vec<Value> {
    tools().iter().map(tool_schema).collect()
}

/// Minimal valid JSON values generated from the same table as the schemas.
pub fn examples() -> Vec<(&'static str, Value)> {
    let mut output = Vec::new();
    for tool in tools() {
        for operation in &tool.operations {
            output.push((tool.name, example("op", operation.name, &operation.fields)));
        }
    }
    output
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
