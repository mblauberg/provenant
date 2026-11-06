//! Explicit field catalogue, shared schema pieces and a cached MCP catalogue.
//! Runtime validates inputs; exported bounds are shared and schema probes detect drift.
use provenant_core::edit_distance;
use provenant_fabric_runtime::coordination::input::{
    MAX_ARTIFACT_PATH_BYTES, MAX_FETCH_BYTES, MAX_REFERENCE_BYTES, MAX_ROUTE_BYTES, MAX_TEXT_BYTES,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::OnceLock};

/// Fabric tool contract, independent of the MCP transport protocol date.
pub const SCHEMA_VERSION: u64 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolKind {
    Identity,
    Mail,
    Task,
    Events,
    Claim,
    Run,
    Output,
    Discover,
    Inbox,
    Console,
    Chair,
}

pub(crate) const TOOL_NAMES: [(ToolKind, &str); 11] = [
    (ToolKind::Identity, "fabric_context"),
    (ToolKind::Mail, "fabric_mail"),
    (ToolKind::Task, "fabric_task"),
    (ToolKind::Events, "fabric_events"),
    (ToolKind::Claim, "fabric_lease"),
    (ToolKind::Run, "fabric_run"),
    (ToolKind::Output, "fabric_output"),
    (ToolKind::Discover, "fabric_discover"),
    (ToolKind::Inbox, "fabric_inbox"),
    (ToolKind::Console, "fabric_console"),
    (ToolKind::Chair, "fabric_chair"),
];

pub(crate) fn tool_name(kind: ToolKind) -> &'static str {
    TOOL_NAMES
        .iter()
        .find_map(|(known, name)| (*known == kind).then_some(*name))
        .unwrap_or("unknown")
}

pub(crate) fn tool_kind(name: &str) -> Option<ToolKind> {
    TOOL_NAMES
        .iter()
        .find_map(|(kind, known)| (*known == name).then_some(*kind))
}

fn integer(min: u64, max: u64) -> Value {
    json!({"type":"integer","minimum":min,"maximum":max})
}
fn text(max: usize) -> Value {
    json!({"type":"string","minLength":1,"maxLength":max,"pattern":r"^[^\u0000]*$","x-provenant-maxUtf8Bytes":max})
}
/// Text that may be empty, as the CLI's summary patch accepts it.
fn text_allowing_empty(max: usize) -> Value {
    let mut schema = text(max);
    if let Some(fields) = schema.as_object_mut() {
        fields.remove("minLength");
    }
    schema
}
fn pattern(value: &str) -> Value {
    json!({"type":"string","pattern":value})
}
fn choices(values: &[&str]) -> Value {
    json!({"type":"string","enum":values})
}
fn nullable(schema: Value) -> Value {
    json!({"anyOf":[schema,{"type":"null"}]})
}
fn array(items: Value, min: u64, max: u64) -> Value {
    json!({"type":"array","items":items,"minItems":min,"maxItems":max})
}
fn reference(name: &str) -> Value {
    json!({"$ref":format!("#/$defs/{name}")})
}
/// A trailing `?` marks optional fields; every object rejects unknown fields.
fn object(fields: Value) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    if let Some(fields) = fields.as_object() {
        for (key, value) in fields {
            let name = key.strip_suffix('?').unwrap_or(key);
            if name == key {
                required.push(name);
            }
            properties.insert(name.to_owned(), value.clone());
        }
    }
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn tagged(tag: &str, variants: Value) -> Value {
    json!({"oneOf": tagged_variants(tag, variants)})
}
fn tagged_variants(tag: &str, variants: Value) -> Vec<Value> {
    let mut schemas = Vec::new();
    if let Some(variants) = variants.as_object() {
        for (name, fields) in variants {
            let mut fields = fields.clone();
            fields[tag] = json!({"const":name});
            schemas.push(object(fields));
        }
    }
    schemas
}
fn output_object(fields: Value) -> Value {
    object(fields)
}
fn output_absolute_path() -> Value {
    output_object(json!({
        "relative_to_root":output_object(json!({
            "encoding":choices(&["utf8","native_bytes"]),
            "path":{"type":"string"}
        }))
    }))
}
/// Whether `fabric_run` and `fabric_output` advertise the typed `outputSchema`
/// from `provenant_records::wire`. Off until the catalogue tests that pin
/// `fabric_context` as the only schema-bearing tool (`schema_tests.rs`,
/// `protocol_tests.rs`) are updated in the same change, and until
/// `tools::tool_error` is checked: it moves error details out of
/// `structuredContent` for any tool that has an output schema.
pub(crate) const TYPED_OUTPUT_PUBLISHED: bool = false;

pub(crate) fn output_schema(kind: ToolKind) -> Option<Value> {
    let text_value = json!({"type":"string"});
    match kind {
        ToolKind::Run | ToolKind::Output if TYPED_OUTPUT_PUBLISHED => {
            provenant_records::wire::mcp_output_schema(tool_name(kind))
        }
        ToolKind::Identity => Some(json!({"type":"object","anyOf":[
            output_object(json!({
                "project":pattern("^[0-9a-f]{32}$"),"host":pattern("^[0-9a-f]{32}$"),
                "inbox":output_object(json!({"host":pattern("^[0-9a-f]{32}$"),"agent":text_value,"actor":pattern("^[0-9a-f]{32}$")})),
                "client":text_value,"cwd":output_absolute_path(),"common_dir":output_absolute_path(),
                "federation":{"anyOf":[{"type":"object"},{"type":"null"}]},"server_version":text_value
            })),
            output_object(json!({"hosts":{"type":"array"}}))
        ]})),
        _ => None,
    }
}

fn proof(key: &str, landing: bool) -> Value {
    let mut fields = json!({key: reference("Id"), "generation":integer(0,u64::MAX),"token":pattern("^[0-9a-f]{32}$")});
    if landing {
        fields["reference"] = text(MAX_REFERENCE_BYTES);
    }
    object(fields)
}
fn definitions() -> Value {
    let message_ref = tagged(
        "scope",
        json!({
            "local":{"id":reference("Id")},
            "remote":{"host":reference("Id"),"id":reference("Id")}
        }),
    );
    json!({
        "Id":pattern("^[0-9a-f]{32}$"),
        "OperationId":pattern("^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"),
        "GitOid":pattern("^(?:[0-9a-f]{40}|[0-9a-f]{64})$"),
        "AttemptTarget":object(json!({"attempt":reference("Id"),"host?":nullable(reference("Id"))})),
        "TaskProof":proof("task",false),"ClaimProof":proof("id",false),"LandingProof":proof("id",true),
        "MessageRef":message_ref,
        "Artifact":object(json!({"path":object(json!({"encoding":choices(&["utf8","native_bytes"]),"path":text(MAX_ARTIFACT_PATH_BYTES)})),"digest":nullable(pattern("^sha256:[0-9a-f]{64}$"))})),
        "ContextLabel":nullable(text(256)),
        "WorkContext":object(json!({"work_item?":reference("ContextLabel"),"graph_id?":reference("ContextLabel"),"node_id?":reference("ContextLabel"),"preset?":reference("ContextLabel"),"pattern?":reference("ContextLabel"),"role?":reference("ContextLabel"),"role_digest?":reference("ContextLabel"),"experiment_arm?":reference("ContextLabel"),"shadow_of?":reference("ContextLabel"),"selection_source?":reference("ContextLabel")})),
        "Route":tagged("kind",json!({"adapter":{"provider":text(MAX_ROUTE_BYTES),"name":text(MAX_ROUTE_BYTES)},"model":{"provider":text(MAX_ROUTE_BYTES),"model":text(MAX_ROUTE_BYTES),"effort?":nullable(text(MAX_ROUTE_BYTES))},"named":{"name":text(MAX_ROUTE_BYTES),"effort?":nullable(text(MAX_ROUTE_BYTES))}})),
        "Scope":tagged("kind",json!({"issue":{"key":text(MAX_REFERENCE_BYTES)},"paths":{"paths":array(text(MAX_ARTIFACT_PATH_BYTES),1,100)},"worktree":{"path":text(MAX_ARTIFACT_PATH_BYTES)}}))
    })
}
fn build_strict() -> Vec<Value> {
    let page = integer(1, 100);
    let lease = integer(1, 3_600);
    let wait = integer(1, crate::MAX_WAIT_SECONDS);
    let seq = integer(0, i64::MAX as u64);
    let count = integer(0, u64::MAX);
    let artifacts = array(reference("Artifact"), 0, 100);
    let mut recipients = array(reference("Id"), 1, 100);
    recipients["uniqueItems"] = json!(true);
    let mut inbox_session = pattern(r"^[a-z0-9]([a-z0-9_-]|\.[a-z0-9_-])*\.?$");
    inbox_session["maxLength"] = json!(128);
    let inbox_request = object(json!({
        "supersedes?":nullable(object(json!({"id":reference("Id"),"expect_revision":integer(1,u64::MAX)}))),
        "deadline?":nullable(pattern(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{6}Z$")),
        "work?":nullable(object(json!({
            "run_id":reference("Id"),"task_id?":nullable(reference("Id")),
            "attempt_id?":nullable(reference("Id")),"repository?":nullable(object(json!({"relative_to_root":object(json!({
                "encoding":choices(&["utf8","native_bytes"]),"path":text(provenant_records::inbox::REQUEST_REPOSITORY_BYTES)
            }))}))),
            "head?":nullable(reference("GitOid"))
        }))),
        "kind":choices(&["decision","assumption","question"]),"title":text(256),
        "body":text(provenant_records::inbox::REQUEST_BODY_BYTES),"blocking?":{"type":"boolean"},
        "options?":array(object(json!({"id":reference("OperationId"),"label":text(256),"description":text(1024)})),0,16),
        "recommended_option_id?":nullable(reference("OperationId")),"proceeding_with?":nullable(text(1024))
    }));
    let tools: [(&str, Value); 11] = [
        (
            "Inspect the authenticated Fabric identity or list peer hosts.",
            json!({"inspect":{},"hosts":{"after?":nullable(reference("Id")),"limit":page.clone()}}),
        ),
        (
            "Send and read mail with explicit cursors.",
            json!({
                "send":{"recipients":recipients,"kind":choices(&["note","request","response"]),"body":text(MAX_TEXT_BYTES),"artifacts?":artifacts,"reply_to?":nullable(reference("MessageRef")),"task?":nullable(reference("MessageRef"))},
                "read":{"after?":nullable(seq.clone()),"limit":page.clone(),"session?":nullable(inbox_session.clone())},"mark_read":{"through":seq.clone(),"session?":nullable(inbox_session.clone())}
            }),
        ),
        (
            "Create and manage fenced project tasks.",
            json!({
                "create":{"objective":text(MAX_TEXT_BYTES),"note?":nullable(text(MAX_TEXT_BYTES))},
                "list":{"state?":nullable(choices(&["open","done","cancelled"])),"after?":nullable(reference("Id")),"limit":page.clone()},
                "claim":{"id":reference("Id"),"lease_seconds":lease.clone()},
                "update":{"proof":reference("TaskProof"),"objective":text(MAX_TEXT_BYTES),"note?":nullable(text(MAX_TEXT_BYTES))},
                "finish":{"proof":reference("TaskProof"),"state":choices(&["done","cancelled"])},"release":{"proof":reference("TaskProof")}
            }),
        ),
        (
            "Read the activity journal or append a note.",
            json!({
                "read":{"after?":nullable(seq),"limit":page.clone(),"wait_seconds?":nullable(wait.clone())},
                "note":{"note":text(MAX_TEXT_BYTES),"dedup_key?":nullable(text(MAX_REFERENCE_BYTES))}
            }),
        ),
        (
            "Manage advisory work claims and landing leases.",
            json!({
                "claim_acquire":{"scope":reference("Scope"),"lease_seconds":lease.clone()},"claim_renew":{"proof":reference("ClaimProof"),"lease_seconds":lease.clone()},
                "claim_verify":{"proof":reference("ClaimProof")},"claim_release":{"proof":reference("ClaimProof")},"claim_list":{"after?":nullable(reference("Id")),"limit":page.clone()},
                "landing_acquire":{"reference":text(MAX_REFERENCE_BYTES),"expected":reference("GitOid"),"candidate":reference("GitOid"),"lease_seconds":lease.clone()},
                "landing_renew":{"proof":reference("LandingProof"),"lease_seconds":lease},"landing_verify":{"proof":reference("LandingProof")},"landing_release":{"proof":reference("LandingProof")},
                "landing_wait":{"reference":text(MAX_REFERENCE_BYTES),"generation":count.clone(),"wait_seconds":wait.clone()}
            }),
        ),
        (
            "Run template briefs or graph documents, edit graphs, inspect templates and report status or trace. Documents and patches are JSON objects, never file paths.",
            json!({
                "list":{"after?":nullable(reference("AttemptTarget")),"limit?":page},"status":{"target":reference("AttemptTarget"),"detail?":{"type":"boolean"}},
                "wait":{"targets":array(reference("AttemptTarget"),1,100),"condition":choices(&["any","all"]),"wait_seconds":wait,"detail?":{"type":"boolean"}},
            }),
        ),
        (
            "Read bounded output or fetch a manifest artefact.",
            json!({
                "read":{"target":reference("AttemptTarget"),"part?":choices(&["prompt","result","stderr","events"]),"offset?":count,"max_bytes?":integer(1,provenant_records::run::OUTPUT_LIMIT)},
                "fetch":{"target":reference("AttemptTarget"),"artifact":reference("Artifact"),"max_bytes":integer(1,MAX_FETCH_BYTES)}
            }),
        ),
        (
            "Discover inbox, console, chair, graph, task, lease and event tools. Call discovered tools by name.",
            json!({"discover":{}}),
        ),
        (
            "Post human requests, list, answer, close or acknowledge; route to an owner host when needed. Steer a lane or message a chair session, both local-only and refused when unsupported.",
            json!({
                "ask":{"session":inbox_session.clone(),"request":inbox_request,"operation_id?":nullable(reference("OperationId"))},
                "list":{"host?":nullable(text(128)),"id?":nullable(reference("Id")),"session?":nullable(inbox_session.clone()),"state?":choices(&["open","all"]),"limit?":integer(1,100),"cursor?":nullable(pattern(r"^[0-9a-f]{32}\.sha256:[0-9a-f]{64}\.[0-9]{1,20}$"))},
                "answer":{"host?":nullable(text(128)),"id":reference("Id"),"action":choices(&["choose","reply","confirm","override"]),"option_id?":nullable(reference("OperationId")),"text?":nullable(text(provenant_records::inbox::ANSWER_TEXT_BYTES)),"expect_revision":integer(1,u64::MAX),"channel":choices(&["chat","console","phone_remote","cli","unknown"]),"operation_id?":nullable(reference("OperationId"))},
                "close":{"host?":nullable(text(128)),"id":reference("Id"),"state":choices(&["withdrawn","superseded","expired"]),"reason":text(1024),"expect_revision":integer(1,u64::MAX),"operation_id?":nullable(reference("OperationId"))},
                "acknowledge":{"host?":nullable(text(128)),"id":reference("Id"),"answer_id":reference("Id"),"session":inbox_session.clone(),"operation_id?":nullable(reference("OperationId"))},
                "steer":{"attempt":reference("Id"),"text":text(provenant_records::control::CONTROL_TEXT_BYTES),"operation_id?":nullable(reference("OperationId"))},
                "chair_message":{"session":inbox_session.clone(),"text":text(provenant_records::control::CONTROL_TEXT_BYTES),"channel":choices(&["chat","console","phone_remote","cli","unknown"]),"operation_id?":nullable(reference("OperationId"))}
            }),
        ),
        (
            "Read projects, sessions, board, tree, overview and attempt detail. Search is local only, returns the first match per artifact and rescans on each cursor page; board and tree can include peers.",
            json!({
                "projects":{}, "overview":{},
                "sessions":{"session?":nullable(inbox_session.clone()),"limit?":integer(1,100),"cursor?":nullable(pattern(r"^sha256:[0-9a-f]{64}:[0-9]{1,20}$"))},
                "board":{"all?":{"type":"boolean"},"limit?":integer(1,100),"cursor?":nullable(pattern(r"^2\.[0-9]{1,20}\.[0-9]{1,20}\.sha256:[0-9a-f]{64}$")),"hosts?":nullable(text(1024))},
                "tree":{"max_depth?":integer(1,16),"max_nodes?":integer(1,100),"detail?":{"type":"boolean"},"hosts?":nullable(text(1024))},
                "detail":{"id":text(256)},
                "search":{"query":text(1024),"part":choices(&["prompt","result"]),"session?":nullable(text(256)),"limit?":integer(1,100),"cursor?":nullable(text(512))}
            }),
        ),
        (
            "Read and update chair summaries and append or list chair logs.",
            json!({
                "summary_show":{"session":inbox_session.clone()},
                "summary_diff":{"session":inbox_session.clone(),"from":integer(1,u64::MAX),"to?":nullable(integer(1,u64::MAX))},
                "summary_update":{"session":inbox_session.clone(),"patch":object(json!({"now?":nullable(text_allowing_empty(256 * 1024)),"next?":nullable(text_allowing_empty(256 * 1024)),"needs_you?":nullable(json!({"type":"array","items":text_allowing_empty(256 * 1024)})),"risk?":nullable(text_allowing_empty(256 * 1024))})),"expect_revision":integer(0,u64::MAX),"operation_id?":nullable(reference("OperationId"))},
                "log_list":{"session":inbox_session.clone(),"after?":nullable(integer(0,u64::MAX)),"limit?":integer(1,100)},
                "log_append":{"session":inbox_session,"entry":object(json!({"kind":text(16 * 1024),"markdown":text(256 * 1024),"channel":text(16 * 1024),"request_ids?":json!({"type":"array","items":text(256 * 1024)}),"attempt_ids?":json!({"type":"array","items":text(256 * 1024)}),"issue_ids?":json!({"type":"array","items":text(256 * 1024)}),"pr_ids?":json!({"type":"array","items":text(256 * 1024)}),"correction_of?":nullable(integer(0,u64::MAX))})),"operation_id?":nullable(reference("OperationId")),"issued_at?":nullable(json!({"type":"integer"}))}
            }),
        ),
    ];
    TOOL_NAMES
        .iter()
        .zip(tools)
        .map(|((kind, name), (description, operations))| {
            let mut variants = tagged_variants("op", operations);
            if *kind == ToolKind::Run {
                let graph = tagged_variants("op", json!({
                    "run":{"operation_id?":reference("OperationId"),"detached?":{"type":"boolean"},"access?":choices(&["read_only","worktree_write"]),"worktree?":text(MAX_ARTIFACT_PATH_BYTES),"branch?":text(1024),"base?":text(1024),"write_scope?":array(text(MAX_ARTIFACT_PATH_BYTES),0,32),"idempotency_key?":text(200),"document?":{"type":"object"},"brief?":text(MAX_TEXT_BYTES),"use?":text(128),"tier?":text(128),"over?":{"anyOf":[{"type":"array","items":text(1024)},text(128)]},"set?":{"type":"object"},"explain?":{"type":"boolean"},"wait_seconds?":integer(1,crate::MAX_WAIT_SECONDS)},
                    "status":{"id?":text(200),"stats?":{"type":"boolean"},"by?":choices(&["tier","template","node"]),"wait_seconds?":integer(1,crate::MAX_WAIT_SECONDS)},
                    "stop":{"id":text(200),"reason?":text_allowing_empty(provenant_fabric_runtime::graph::MAX_CANCEL_REASON_BYTES),"wait_seconds?":integer(1,crate::MAX_WAIT_SECONDS)},
                    "retry":{"id":text(200),"node?":text(200),"brief?":text(MAX_TEXT_BYTES),"extend?":array(text(256),0,64),"wait_seconds?":integer(1,crate::MAX_WAIT_SECONDS)},
                    "edit":{"id":text(200),"patch":{"type":"object"}},
                    "templates":{"name?":text(128),"templates?":{"type":"object"}},
                    "output":{"id":text(200),"trace":{"const":true}}
                }));
                variants.extend(graph);
            }
            if *kind == ToolKind::Output {
                variants.extend(tagged_variants("op", json!({"trace":{"id":text(200)}})));
            }
            let mut input = json!({"oneOf": variants});
            if matches!(
                kind,
                ToolKind::Mail
                    | ToolKind::Task
                    | ToolKind::Events
                    | ToolKind::Claim
            ) && let Some(variants) = input["oneOf"].as_array_mut()
            {
                for variant in variants {
                    let operation = variant
                        .pointer("/properties/op/const")
                        .and_then(Value::as_str);
                    let is_mutation = matches!(
                        (*kind, operation),
                        (ToolKind::Mail, Some("send" | "mark_read"))
                            | (
                                ToolKind::Task,
                                Some("create" | "claim" | "update" | "finish" | "release")
                            )
                            | (ToolKind::Events, Some("note"))
                            | (ToolKind::Claim, Some("claim_acquire" | "claim_renew" | "claim_release" | "landing_acquire" | "landing_renew" | "landing_release"))
                    );
                    if is_mutation {
                        variant["properties"]["operation_id"] = reference("OperationId");
                    }
                }
            }
            // `fabric_context` defaults to inspect, so that variant may omit `op`.
            if *kind == ToolKind::Identity
                && let Some(variants) = input["oneOf"].as_array_mut()
            {
                for variant in variants {
                    if variant.pointer("/properties/op/const") == Some(&json!("inspect"))
                        && let Some(required) = variant["required"].as_array_mut()
                    {
                        required.retain(|field| field != "op");
                    }
                }
            }
            // A single-operation tool is a plain object: a one-variant `oneOf`
            // is rendered by clients as "exactly one of" over the union of its
            // properties, which contradicts the required fields. `op` is then
            // optional because only one value exists.
            if let Some([only]) = input["oneOf"].as_array().map(Vec::as_slice) {
                let mut only = only.clone();
                if let Some(required) = only["required"].as_array_mut() {
                    required.retain(|field| field != "op");
                }
                input = only;
            }
            input["type"] = json!("object");
            input["$defs"] = definitions();
            let mut tool = json!({"name":name,"description":description,"inputSchema":input,"_meta":{"io.provenant/schemaVersion":SCHEMA_VERSION}});
            if let Some(output) = output_schema(*kind) {
                tool["outputSchema"] = output;
            }
            tool
        })
        .collect()
}

fn strip_string_constraints(node: &mut Value) {
    match node {
        Value::Object(fields) => {
            if fields.get("type").and_then(Value::as_str) == Some("string") {
                fields.remove("minLength");
                fields.remove("x-provenant-maxUtf8Bytes");
                if fields.get("pattern").and_then(Value::as_str) == Some(r"^[^\u0000]*$") {
                    fields.remove("pattern");
                }
            }
            for child in fields.values_mut() {
                strip_string_constraints(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                strip_string_constraints(child);
            }
        }
        _ => {}
    }
}

/// Count physical references in the reachable schema, visiting each def once.
fn count_refs<'a>(
    node: &Value,
    defs: &'a serde_json::Map<String, Value>,
    counts: &mut BTreeMap<&'a str, usize>,
) {
    match node {
        Value::Object(fields) => {
            if let Some((name, definition)) = fields
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|pointer| pointer.strip_prefix("#/$defs/"))
                .and_then(|name| defs.get_key_value(name))
            {
                let count = counts.entry(name.as_str()).or_default();
                *count += 1;
                if *count == 1 {
                    count_refs(definition, defs, counts);
                }
            }
            for (key, child) in fields {
                if key != "$defs" {
                    count_refs(child, defs, counts);
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                count_refs(child, defs, counts);
            }
        }
        _ => {}
    }
}

fn inline_single_use_defs(
    node: &mut Value,
    defs: &serde_json::Map<String, Value>,
    counts: &BTreeMap<&str, usize>,
) {
    while let Some(name) = node["$ref"]
        .as_str()
        .and_then(|pointer| pointer.strip_prefix("#/$defs/"))
        && counts.get(name) == Some(&1)
    {
        *node = defs[name].clone();
    }
    match node {
        Value::Object(fields) => {
            for child in fields.values_mut() {
                inline_single_use_defs(child, defs, counts);
            }
        }
        Value::Array(items) => {
            for child in items {
                inline_single_use_defs(child, defs, counts);
            }
        }
        _ => {}
    }
}

/// Publish relaxed text bounds and only definitions that save repeated bytes.
fn compact(tool: &Value) -> Value {
    let mut tool = tool.clone();
    strip_string_constraints(&mut tool);
    let input = &mut tool["inputSchema"];
    let Some(Value::Object(defs)) = input
        .as_object_mut()
        .and_then(|fields| fields.remove("$defs"))
    else {
        return tool;
    };
    let mut counts = BTreeMap::new();
    count_refs(input, &defs, &mut counts);
    inline_single_use_defs(input, &defs, &counts);
    let retained: serde_json::Map<String, Value> = defs
        .iter()
        .filter(|(name, _)| counts.get(name.as_str()).is_some_and(|count| *count >= 2))
        .map(|(name, definition)| {
            let mut definition = definition.clone();
            inline_single_use_defs(&mut definition, &defs, &counts);
            (name.clone(), definition)
        })
        .collect();
    let mut counts = BTreeMap::new();
    count_refs(input, &retained, &mut counts);
    let retained: serde_json::Map<String, Value> = retained
        .iter()
        .filter(|(name, _)| counts.get(name.as_str()).is_some_and(|count| *count >= 2))
        .map(|(name, definition)| (name.clone(), definition.clone()))
        .collect();
    if !retained.is_empty() {
        input["$defs"] = Value::Object(retained);
    }
    tool
}

/// The only operation of a single-operation tool, which callers may omit.
pub fn sole_operation(name: &str) -> Option<&'static str> {
    let input = &all_catalogue().iter().find(|tool| tool["name"] == name)?["inputSchema"];
    if input["oneOf"].is_array() {
        return None;
    }
    input["properties"]["op"]["const"].as_str()
}

pub(crate) fn strict_catalogue() -> &'static [Value] {
    static CATALOGUE: OnceLock<Vec<Value>> = OnceLock::new();
    CATALOGUE.get_or_init(build_strict)
}

pub fn catalogue() -> &'static [Value] {
    static CATALOGUE: OnceLock<Vec<Value>> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        all_catalogue()
            .iter()
            .filter(|tool| !is_advanced(&tool["name"]))
            .cloned()
            .collect()
    })
}

fn is_advanced(name: &Value) -> bool {
    matches!(
        name.as_str(),
        Some(
            "fabric_task"
                | "fabric_events"
                | "fabric_lease"
                | "fabric_inbox"
                | "fabric_console"
                | "fabric_chair"
        )
    )
}

pub(crate) fn all_catalogue() -> &'static [Value] {
    static CATALOGUE: OnceLock<Vec<Value>> = OnceLock::new();
    CATALOGUE.get_or_init(|| strict_catalogue().iter().map(compact).collect())
}

pub(crate) fn advanced_catalogue() -> Vec<Value> {
    all_catalogue()
        .iter()
        .filter(|tool| is_advanced(&tool["name"]))
        .cloned()
        .collect()
}

/// Name the one valid argument an unknown key or action most likely meant. The
/// path comes from `invalid_path`; the call is still refused by the caller.
pub fn near_argument(name: &str, value: &Value, path: &str) -> Option<String> {
    let root = strict_catalogue()
        .iter()
        .find(|tool| tool["name"] == name)
        .map(|tool| &tool["inputSchema"])?;
    let normalised = path.replace('[', ".").replace(']', "");
    let segments: Vec<&str> = normalised.split('.').skip(1).collect();
    let (given, parents) = segments.split_last()?;
    if parents.is_empty()
        && let Some(options) = root["oneOf"].as_array()
    {
        let matching: Vec<_> = options
            .iter()
            .filter(|option| {
                value
                    .get("op")
                    .is_some_and(|op| option["properties"]["op"]["const"] == *op)
            })
            .collect();
        if matching.len() > 1 {
            let fields: std::collections::BTreeSet<_> = matching
                .iter()
                .flat_map(|variant| {
                    variant["properties"]
                        .as_object()
                        .into_iter()
                        .flat_map(|fields| fields.keys().map(String::as_str))
                })
                .collect();
            return if fields.contains(*given) {
                None
            } else {
                nearest(given, fields.into_iter()).map(str::to_owned)
            };
        }
    }
    let (holder, owner) = locate(root, value, parents)?;
    let object = settle(root, holder, owner);
    if let Some(variants) = object["oneOf"].as_array() {
        if !matches!(*given, "op" | "kind" | "scope") {
            return None;
        }
        let typed = owner.get(*given)?.as_str()?;
        let choices: std::collections::BTreeSet<_> = variants
            .iter()
            .filter_map(|variant| variant["properties"][*given]["const"].as_str())
            .collect();
        return nearest(typed, choices.into_iter()).map(str::to_owned);
    }
    let fields = object["properties"].as_object()?;
    if fields.contains_key(*given) {
        return None;
    }
    nearest(given, fields.keys().map(String::as_str)).map(str::to_owned)
}

/// The schema and value at a dotted argument path, before the final segment's
/// variant is settled.
fn locate<'a>(
    root: &'a Value,
    value: &'a Value,
    segments: &[&str],
) -> Option<(&'a Value, &'a Value)> {
    let mut schema = root;
    let mut current = value;
    for segment in segments {
        schema = settle(root, schema, current);
        if let Ok(index) = segment.parse::<usize>() {
            schema = &schema["items"];
            current = current.get(index)?;
        } else {
            schema = &schema["properties"][*segment];
            current = current.get(*segment)?;
        }
    }
    Some((schema, current))
}

/// Follow references and anyOf, then the variant a discriminator selects. An
/// unmatched oneOf is returned as it is, so its tag values can be listed.
fn settle<'a>(root: &'a Value, mut schema: &'a Value, current: &Value) -> &'a Value {
    loop {
        if let Some(next) = schema["$ref"]
            .as_str()
            .and_then(|pointer| root.pointer(pointer.strip_prefix('#')?))
        {
            schema = next;
        } else if let Some(first) = schema["anyOf"]
            .as_array()
            .and_then(|options| options.first())
        {
            schema = first;
        } else if let Some(variant) = schema["oneOf"].as_array().and_then(|options| {
            options.iter().find(|option| {
                ["op", "kind", "scope"].iter().any(|tag| {
                    current
                        .get(*tag)
                        .is_some_and(|v| option["properties"][*tag]["const"] == *v)
                })
            })
        }) {
            schema = variant;
        } else {
            return schema;
        }
    }
}

/// The lone closest candidate, when it is within a small edit distance after
/// case and separator folding. A tie refuses to guess.
fn nearest<'a>(given: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    if given.len() > 128 || given.chars().any(char::is_control) {
        return None;
    }
    let given = fold(given);
    let mut best: Option<(usize, &'a str)> = None;
    let mut tied = false;
    for candidate in candidates {
        let distance = edit_distance(&given, &fold(candidate));
        match best {
            Some((best_distance, _)) if distance > best_distance => {}
            Some((best_distance, _)) if distance == best_distance => tied = true,
            _ => {
                best = Some((distance, candidate));
                tied = false;
            }
        }
    }
    let (distance, candidate) = best?;
    let limit = (given.len().max(fold(candidate).len()) / 3).clamp(1, 2);
    (!tied && distance <= limit).then_some(candidate)
}

fn fold(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Diagnose only a rejected serde value. Runtime is the validation owner;
/// this structural walk recovers paths that serde's errors omit.
pub fn invalid_path(name: &str, value: &Value) -> Option<String> {
    let root = &strict_catalogue()
        .iter()
        .find(|tool| tool["name"] == name)?["inputSchema"];
    structural_path(root, root, value, "arguments")
}

/// Project only catalogue-owned field names and array indices. Unknown object
/// keys can contain arbitrary caller data and must not enter an error envelope.
pub(crate) fn safe_argument_path(name: &str, value: &Value, path: &str) -> String {
    let Some(root) = strict_catalogue()
        .iter()
        .find(|tool| tool["name"] == name)
        .map(|tool| &tool["inputSchema"])
    else {
        return "arguments".into();
    };
    let normalised = path.replace('[', ".").replace(']', "");
    let mut schema = root;
    let mut current = value;
    let mut safe = String::from("arguments");
    for segment in normalised.split('.').skip(1) {
        schema = settle(root, schema, current);
        if let Ok(index) = segment.parse::<usize>() {
            if !current.is_array() {
                break;
            }
            safe.push_str(&format!("[{index}]"));
            current = current.get(index).unwrap_or(&Value::Null);
            schema = &schema["items"];
        } else if let Some(field) = schema["properties"].get(segment) {
            safe.push('.');
            safe.push_str(segment);
            current = current.get(segment).unwrap_or(&Value::Null);
            schema = field;
        } else if matches!(segment, "op" | "kind" | "scope")
            && schema["oneOf"].as_array().is_some_and(|variants| {
                variants
                    .iter()
                    .any(|variant| variant["properties"].get(segment).is_some())
            })
        {
            safe.push('.');
            safe.push_str(segment);
            break;
        } else {
            safe.push_str(".<unknown>");
            break;
        }
    }
    provenant_records::diagnostic::safe_text(&safe)
}

/// Describe the schema constraint at a rejected argument path, without echoing input.
pub fn expected(name: &str, value: &Value, path: &str) -> String {
    let Some(root) = strict_catalogue()
        .iter()
        .find(|tool| tool["name"] == name)
        .map(|tool| &tool["inputSchema"])
    else {
        return "valid tool arguments".into();
    };
    let mut schema = root;
    let mut current = value;
    let normalised = path.replace('[', ".").replace(']', "");
    for segment in normalised.split('.').skip(1) {
        loop {
            if let Some(pointer) = schema["$ref"].as_str().and_then(|p| p.strip_prefix('#'))
                && let Some(next) = root.pointer(pointer)
            {
                schema = next;
                continue;
            }
            if let Some(options) = schema["oneOf"].as_array() {
                if let Some(next) = options.iter().find(|option| {
                    ["op", "kind", "scope"].iter().any(|tag| {
                        current
                            .get(*tag)
                            .is_some_and(|v| option["properties"][*tag]["const"] == *v)
                    })
                }) {
                    schema = next;
                    continue;
                }
                if segment == "op" || segment == "kind" || segment == "scope" {
                    let choices: Vec<_> = options
                        .iter()
                        .filter_map(|option| option["properties"][segment]["const"].as_str())
                        .collect();
                    if !choices.is_empty() {
                        return format!("one of {}", choices.join(", "));
                    }
                }
            }
            if let Some(options) = schema["anyOf"].as_array() {
                schema = &options[0];
                continue;
            }
            break;
        }
        if let Ok(index) = segment.parse::<usize>() {
            schema = &schema["items"];
            current = current.get(index).unwrap_or(&Value::Null);
        } else {
            schema = &schema["properties"][segment];
            current = current.get(segment).unwrap_or(&Value::Null);
        }
    }
    while let Some(pointer) = schema["$ref"].as_str().and_then(|p| p.strip_prefix('#')) {
        let Some(next) = root.pointer(pointer) else {
            break;
        };
        schema = next;
    }
    if let Some(options) = schema["anyOf"].as_array() {
        schema = &options[0];
    }
    if let Some(values) = schema["enum"].as_array() {
        return format!(
            "one of {}",
            values
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(value) = schema.get("const") {
        return format!("{value}");
    }
    if let Some(max) = schema["x-provenant-maxUtf8Bytes"].as_u64()
        && current.as_str().is_some_and(|text| text.len() as u64 > max)
    {
        return format!("a string of at most {max} UTF-8 bytes");
    }
    match schema["type"].as_str() {
        Some("integer") => format!(
            "an integer from {} to {}",
            schema["minimum"], schema["maximum"]
        ),
        Some("string") if schema["pattern"].is_string() => {
            format!("a string matching {}", schema["pattern"])
        }
        Some("string") => format!(
            "a non-empty string of at most {} UTF-8 bytes",
            schema["maxLength"]
        ),
        Some("array") => format!(
            "an array with {} to {} items",
            schema["minItems"], schema["maxItems"]
        ),
        Some("object") => "an object with the documented fields".into(),
        Some("boolean") => "a boolean".into(),
        _ => "a documented argument".into(),
    }
}
fn structural_path(root: &Value, schema: &Value, value: &Value, path: &str) -> Option<String> {
    if let Some(pointer) = schema["$ref"].as_str() {
        return structural_path(root, root.pointer(pointer.strip_prefix('#')?)?, value, path);
    }
    if let Some(options) = schema["anyOf"].as_array() {
        return if options
            .iter()
            .any(|option| structural_path(root, option, value, path).is_none())
        {
            None
        } else {
            structural_path(root, &options[0], value, path)
        };
    }
    if let Some(options) = schema["oneOf"].as_array() {
        if options
            .iter()
            .any(|option| structural_path(root, option, value, path).is_none())
        {
            return None;
        }
        for tag in ["op", "scope", "kind"] {
            if let Some(selected) = value.get(tag) {
                let option = options
                    .iter()
                    .find(|s| s["properties"][tag]["const"] == *selected);
                return match option {
                    Some(option) => structural_path(root, option, value, path),
                    None => Some(format!("{path}.{tag}")),
                };
            }
        }
        // No discriminator present: name the first one the variants use.
        let missing = ["op", "scope", "kind"].into_iter().find(|tag| {
            options
                .iter()
                .any(|s| s["properties"][*tag]["const"].is_string())
        });
        return Some(missing.map_or_else(|| path.to_owned(), |tag| format!("{path}.{tag}")));
    }
    let valid_type = match schema["type"].as_str() {
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("integer") => value.is_u64() || value.is_i64(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        _ => true,
    };
    if !valid_type
        || schema.get("const").is_some_and(|c| c != value)
        || schema["enum"]
            .as_array()
            .is_some_and(|e| !e.contains(value))
    {
        return Some(path.to_owned());
    }
    if let Some(text) = value.as_str() {
        let length = text.chars().count() as u64;
        if schema["minLength"].as_u64().is_some_and(|min| length < min)
            || schema["maxLength"].as_u64().is_some_and(|max| length > max)
            || schema["x-provenant-maxUtf8Bytes"]
                .as_u64()
                .is_some_and(|max| text.len() as u64 > max)
        {
            return Some(path.to_owned());
        }
        let pattern_ok = match schema["pattern"].as_str() {
            Some("^[0-9a-f]{32}$") => provenant_core::ActorId::parse(text).is_ok(),
            Some("^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$") => {
                provenant_core::OperationId::parse(text).is_ok()
            }
            Some("^(?:[0-9a-f]{40}|[0-9a-f]{64})$") => provenant_core::GitOid::parse(text).is_ok(),
            Some("^sha256:[0-9a-f]{64}$") => provenant_core::Digest::parse(text).is_ok(),
            Some(r"^[a-z0-9]([a-z0-9_-]|\.[a-z0-9_-])*\.?$") => {
                let mut chars = text.chars();
                chars
                    .next()
                    .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                    && !text.contains("..")
                    && chars.all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.')
                    })
            }
            Some(_) => !text.contains('\0'),
            None => true,
        };
        if !pattern_ok {
            return Some(path.to_owned());
        }
    }
    if let Some(n) = value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
    {
        let number = |v: &Value| {
            v.as_i64()
                .map(i128::from)
                .or_else(|| v.as_u64().map(i128::from))
        };
        if number(&schema["minimum"]).is_some_and(|min| n < min)
            || number(&schema["maximum"]).is_some_and(|max| n > max)
        {
            return Some(path.to_owned());
        }
    }
    if let Some(object) = value.as_object() {
        let props = schema["properties"].as_object();
        for (key, value) in object {
            let next = format!("{path}.{key}");
            let Some(field) = props.and_then(|properties| properties.get(key)) else {
                if schema["additionalProperties"] == false {
                    return Some(next);
                }
                if let Some(additional) = schema.get("additionalProperties")
                    && additional.is_object()
                    && let Some(path) = structural_path(root, additional, value, &next)
                {
                    return Some(path);
                }
                continue;
            };
            if let Some(path) = structural_path(root, field, value, &next) {
                return Some(path);
            }
        }
        if object.contains_key("encoding")
            && object.contains_key("path")
            && serde_json::from_value::<provenant_records::PathWire>(value.clone()).is_err()
        {
            return Some(format!("{path}.path"));
        }
        for key in schema["required"].as_array().into_iter().flatten() {
            let key = key.as_str()?;
            if !object.contains_key(key) {
                return Some(format!("{path}.{key}"));
            }
        }
    }
    if let Some(items) = value.as_array() {
        if schema["minItems"]
            .as_u64()
            .is_some_and(|n| (items.len() as u64) < n)
            || schema["maxItems"]
                .as_u64()
                .is_some_and(|n| items.len() as u64 > n)
        {
            return Some(path.to_owned());
        }
        return items.iter().enumerate().find_map(|(index, item)| {
            structural_path(root, &schema["items"], item, &format!("{path}[{index}]"))
        });
    }
    None
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;

#[cfg(test)]
mod wire_parity {
    //! The typed schemas in `provenant_records::wire` must accept what the
    //! runtime really serialises, including MCP's `provider_seconds` and its
    //! path encodings.
    use super::*;
    use provenant_core::{AttemptId, Digest, HostId};
    use provenant_fabric_runtime::coordination::facade::{
        AttemptTarget, OutputBytes, OutputPart, OutputResult,
    };
    use provenant_fabric_runtime::runs::{AttemptPage, AttemptSummary, RunResult};
    use provenant_records::run::AttemptRecord;
    use provenant_records::wire::{mcp_output_schema, schema::validate};

    fn attempt(provider_ms: Option<u64>) -> AttemptRecord {
        let mut record = json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": provenant_core::RunId::from_bytes([2; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": provenant_core::TaskId::from_bytes([4; 16]),
            "operation_id": "wire-parity",
            "attempt_number": 1, "revision": 1, "generation": 0,
            "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
            "authority_digest": Digest::from_bytes([5; 32]),
            "queued_at": provenant_core::WallTime::from_micros(1).unwrap(),
            "state": {"state":"running"}, "usage": {},
            "prompt": {"path":{"encoding":"utf8","path":"prompt.txt"},
                "digest":provenant_records::CanonicalV1::file_digest(b""),
                "bytes":0,"complete":true,"redacted":false}
        });
        record["provider_ms"] = json!(provider_ms);
        serde_json::from_value(record).unwrap()
    }

    fn check(tool: &str, value: &impl serde::Serialize) -> Value {
        let value = serde_json::to_value(value).unwrap();
        let schema = mcp_output_schema(tool).unwrap();
        if let Err(error) = validate(&schema, &value) {
            panic!("{error}\n{value:#}");
        }
        value
    }

    #[test]
    fn run_results_carry_provider_seconds_and_validate() {
        for (ms, seconds) in [(Some(1500), json!(1.5)), (None, Value::Null)] {
            let record = attempt(ms);
            let status = RunResult::Status {
                extra: Default::default(),
                attempt: record.clone(),
            };
            let mut value = serde_json::to_value(&status).unwrap();
            crate::tools::project_run(&mut value, false).unwrap();
            let value = check("fabric_run", &value);
            assert_eq!(value["attempt"]["provider_seconds"], seconds);
            assert_eq!(value["attempt"]["state"], "running");
            assert_eq!(value["attempt"]["run_id"], record.run_id.to_string());
            assert_eq!(value["attempt"]["task_id"], record.task_id.to_string());
            assert_eq!(
                value["attempt"]["attempt_id"],
                record.attempt_id.to_string()
            );
            assert!(value["attempt"]["retry_of"].is_null());
            assert!(value["attempt"]["superseded_by"].is_null());
            assert!(value["attempt"].get("provider_ms").is_none());
            assert_eq!(
                provenant_records::wire::provider_seconds(ms),
                seconds.as_f64()
            );
            let page = RunResult::List {
                extra: Default::default(),
                page: AttemptPage {
                    extra: Default::default(),
                    items: vec![AttemptSummary::from(record.clone())],
                    next_after: None,
                    truncated: false,
                },
            };
            check("fabric_run", &page);
            let mut cancelled = serde_json::to_value(RunResult::Cancel {
                extra: Default::default(),
                attempt: record,
            })
            .unwrap();
            crate::tools::project_run(&mut cancelled, true).unwrap();
            check("fabric_run", &cancelled);
        }
    }

    #[test]
    fn run_refusal_projects_safe_skipped_route_for_both_mcp_views() {
        use provenant_records::{
            RouteExclusionReason,
            run::{SkipKind, SkippedRoute},
        };
        let mut record = attempt(None);
        record.skipped_routes.push(SkippedRoute {
            extra: Default::default(),
            route: "fixture/model".into(),
            kind: SkipKind::Unavailable,
            cause: Some(RouteExclusionReason::StaleAvailability),
            detail: format!("TOKEN_SENTINEL /private/source \u{1b} {}", "é".repeat(2048)),
        });
        record.state = provenant_records::run::AttemptState::Terminal {
            extra: Default::default(),
            outcome: provenant_records::run::Outcome::Unsettled,
            exit_code: None,
            finished_at: provenant_core::WallTime::from_micros(2).unwrap(),
            reason: Some("TOKEN_SENTINEL /private/source \u{1b}".into()),
        };
        for detailed in [false, true] {
            let mut value = serde_json::to_value(RunResult::Status {
                extra: Default::default(),
                attempt: record.clone(),
            })
            .unwrap();
            crate::tools::project_run(&mut value, detailed).unwrap();
            let reason = value["attempt"]["reason"].as_str().unwrap();
            assert!(reason.contains("wire-parity"));
            assert!(!reason.contains("TOKEN_SENTINEL"));
            assert!(!reason.contains("/private/source"));
            assert!(
                !value["attempt"]["settlement"]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("TOKEN_SENTINEL")
            );
            let skipped = &value["attempt"]["skipped_routes"][0];
            assert_eq!(skipped["code"], "REFUSED");
            assert!(skipped["detail"].as_str().unwrap().contains("stale"));
            assert!(
                skipped["next"]
                    .as_str()
                    .unwrap()
                    .contains("provenant quota tick")
            );
            let wire = skipped.to_string();
            assert!(wire.len() < 4096);
            assert!(!wire.contains("TOKEN_SENTINEL"));
            assert!(!wire.contains("/private/source"));
            assert!(!wire.contains('\u{1b}'));
        }

        // CLI status, output and console all use this same lane owner. Both
        // MCP views must preserve its terminal cause and reason as well.
        for cause in [
            provenant_records::run::TerminalCause::HandoffRefused,
            provenant_records::run::TerminalCause::UnregisteredAccount,
            provenant_records::run::TerminalCause::CommandUnavailable,
        ] {
            let mut record = attempt(None);
            let mut extra = provenant_records::ExtraFields::default();
            extra.insert(
                "terminal_cause".into(),
                serde_json::to_value(cause).unwrap(),
            );
            record.state = provenant_records::run::AttemptState::Terminal {
                extra,
                outcome: provenant_records::run::Outcome::Failed,
                exit_code: None,
                finished_at: provenant_core::WallTime::from_micros(2).unwrap(),
                reason: Some("PROMPT_SENTINEL\u{1b}".into()),
            };
            let owner = provenant_records::wire::LaneRow::project(&record, Default::default());
            for detailed in [false, true] {
                let mut value = serde_json::to_value(RunResult::Status {
                    extra: Default::default(),
                    attempt: record.clone(),
                })
                .unwrap();
                crate::tools::project_run(&mut value, detailed).unwrap();
                assert_eq!(
                    value["attempt"]["terminal_cause"],
                    serde_json::to_value(cause).unwrap()
                );
                assert_eq!(value["attempt"]["reason"], serde_json::json!(owner.reason));
                assert!(!value.to_string().contains("PROMPT_SENTINEL"));
            }
        }
    }

    #[test]
    fn output_results_validate() {
        let target = AttemptTarget {
            extra: Default::default(),
            attempt: AttemptId::from_bytes([1; 16]),
            host: Some(HostId::from_bytes([7; 16])),
        };
        for bytes in [
            OutputBytes::Utf8("ok".into()),
            OutputBytes::Base64("//8=".into()),
        ] {
            let chunk = OutputResult::Chunk {
                extra: Default::default(),
                target: target.clone(),
                part: OutputPart::Result,
                state: "terminal".into(),
                reason: None,
                offset: 0,
                next_offset: 2,
                eof: true,
                digest: Digest::from_bytes([0; 32]),
                bytes,
            };
            let mut value = serde_json::to_value(&chunk).unwrap();
            let mut lane = serde_json::to_value(provenant_records::wire::LaneRow::project(
                &attempt(None),
                Default::default(),
            ))
            .unwrap();
            lane["provider_seconds"] = Value::Null;
            value
                .as_object_mut()
                .unwrap()
                .extend(lane.as_object().unwrap().clone());
            check("fabric_output", &value);
        }
    }

    #[test]
    fn typed_output_stays_off_the_catalogue_until_enabled() {
        assert!(mcp_output_schema("fabric_run").is_some());
    }
}
