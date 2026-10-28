//! Explicit field catalogue, shared schema pieces and a cached MCP catalogue.
//! Runtime validates inputs; exported bounds are shared and schema probes detect drift.
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
    Graph,
    Discover,
    Inbox,
}

pub(crate) const TOOL_NAMES: [(ToolKind, &str); 10] = [
    (ToolKind::Identity, "fabric_context"),
    (ToolKind::Mail, "fabric_mail"),
    (ToolKind::Task, "fabric_task"),
    (ToolKind::Events, "fabric_events"),
    (ToolKind::Claim, "fabric_lease"),
    (ToolKind::Run, "fabric_run"),
    (ToolKind::Graph, "fabric_graph"),
    (ToolKind::Output, "fabric_output"),
    (ToolKind::Discover, "fabric_discover"),
    (ToolKind::Inbox, "fabric_inbox"),
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
    let mut schemas = Vec::new();
    if let Some(variants) = variants.as_object() {
        for (name, fields) in variants {
            let mut fields = fields.clone();
            fields[tag] = json!({"const":name});
            schemas.push(object(fields));
        }
    }
    json!({"oneOf":schemas})
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
pub(crate) fn output_schema(kind: ToolKind) -> Option<Value> {
    let text_value = json!({"type":"string"});
    match kind {
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
        "Route":tagged("kind",json!({"model":{"provider":text(MAX_ROUTE_BYTES),"model":text(MAX_ROUTE_BYTES),"effort?":nullable(text(MAX_ROUTE_BYTES))},"named":{"name":text(MAX_ROUTE_BYTES),"effort?":nullable(text(MAX_ROUTE_BYTES))}})),
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
                "encoding":choices(&["utf8","native_bytes"]),"path":text(4096)
            }))}))),
            "head?":nullable(reference("GitOid"))
        }))),
        "kind":choices(&["decision","assumption","question"]),"title":text(256),
        "body":text(provenant_records::inbox::REQUEST_BODY_BYTES),"blocking?":{"type":"boolean"},
        "options?":array(object(json!({"id":reference("OperationId"),"label":text(256),"description":text(1024)})),0,16),
        "recommended_option_id?":nullable(reference("OperationId")),"proceeding_with?":nullable(text(1024))
    }));
    let tools: [(&str, Value); 10] = [
        (
            "Inspect the authenticated Fabric identity or list peer hosts.",
            json!({"inspect":{},"hosts":{"after?":nullable(reference("Id")),"limit":page.clone()}}),
        ),
        (
            "Send and read mail with explicit cursors.",
            json!({
                "send":{"recipients":recipients,"kind":choices(&["note","request","response"]),"body":text(MAX_TEXT_BYTES),"artifacts":artifacts,"reply_to?":nullable(reference("MessageRef")),"task?":nullable(reference("MessageRef"))},
                "read":{"after?":nullable(seq.clone()),"limit":page.clone()},"mark_read":{"through":seq.clone()}
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
            "Submit and wait for results, list, inspect, wait for, cancel or resume runs. List returns compact attempts newest first; pass next_after as after to continue. Set detail for full records.",
            json!({
                "submit":{"operation_id?":reference("OperationId"),"wait_seconds?":wait.clone(),"detail?":{"type":"boolean"},"tasks":array(object(json!({"build_heavy?":{"type":"boolean"},"prompt":text(MAX_TEXT_BYTES),"title?":nullable(text(256)),"base?":nullable(text(1024)),"head?":nullable(text(64)),"account?":nullable(text(64)),"access?":choices(&["read_only","worktree_write"]),"worktree?":nullable(text(MAX_ARTIFACT_PATH_BYTES)),"route":reference("Route"),"timeout_seconds":integer(1,14_400),"delivery_id?":nullable(reference("Id")),"deliver?":nullable(object(json!({"remote":text(64),"branch":text(1024),"open_pr?":{"type":"boolean"}})))})),1,64),"concurrency?":nullable(integer(1,16)),"knobs?":array(text(256),0,64),"host?":nullable(reference("Id"))},
                "list":{"after?":nullable(reference("AttemptTarget")),"limit?":page},"status":{"target":reference("AttemptTarget"),"detail?":{"type":"boolean"}},
                "wait":{"targets":array(reference("AttemptTarget"),1,100),"condition":choices(&["any","all"]),"wait_seconds":wait,"detail?":{"type":"boolean"}},
                "cancel":{"target":reference("AttemptTarget"),"operation_id?":reference("OperationId"),"detail?":{"type":"boolean"}},
                "resume":{"target":reference("AttemptTarget"),"operation_id?":reference("OperationId"),"prompt":text(MAX_TEXT_BYTES),"expected_generation":count.clone(),"no_fallback?":{"type":"boolean"}}
            }),
        ),
        (
            "Run, inspect, cancel or resume a checkpointed graph. Run and resume take a project-local document path.",
            json!({
                "run":{"document":text(MAX_ARTIFACT_PATH_BYTES)},"status":{"id":pattern("^[A-Za-z0-9._-]{1,64}$")},
                "cancel":{"id":pattern("^[A-Za-z0-9._-]{1,64}$")},"resume":{"document":text(MAX_ARTIFACT_PATH_BYTES),"extend?":array(json!({"type":"string","minLength":3,"maxLength":256,"pattern":r"^[^=\u0000]+=[^\u0000]+$","x-provenant-maxUtf8Bytes":256}),0,64)}
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
            "Discover inbox, graph, task, lease and event tools. Call discovered tools by name.",
            json!({"discover":{}}),
        ),
        (
            "Post human requests, list the local inbox, answer, close or acknowledge. Fabric mail is queued, not live injection; peers are unsupported.",
            json!({
                "ask":{"session":inbox_session.clone(),"request":inbox_request,"operation_id?":nullable(reference("OperationId"))},
                "list":{"id?":nullable(reference("Id")),"session?":nullable(inbox_session.clone()),"state?":choices(&["open","all"]),"limit?":integer(1,100),"cursor?":nullable(pattern(r"^[0-9a-f]{32}\.sha256:[0-9a-f]{64}\.[0-9]{1,20}$"))},
                "answer":{"id":reference("Id"),"action":choices(&["choose","reply","confirm","override"]),"option_id?":nullable(reference("OperationId")),"text?":nullable(text(provenant_records::inbox::ANSWER_TEXT_BYTES)),"expect_revision":integer(1,u64::MAX),"channel":choices(&["chat","console","phone_remote","cli","unknown"]),"operation_id?":nullable(reference("OperationId"))},
                "close":{"id":reference("Id"),"state":choices(&["withdrawn","superseded","expired"]),"reason":text(1024),"expect_revision":integer(1,u64::MAX),"operation_id?":nullable(reference("OperationId"))},
                "acknowledge":{"id":reference("Id"),"answer_id":reference("Id"),"session":inbox_session,"operation_id?":nullable(reference("OperationId"))}
            }),
        ),
    ];
    TOOL_NAMES
        .iter()
        .zip(tools)
        .map(|((kind, name), (description, operations))| {
            let mut input = tagged("op", operations);
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
        Some("fabric_task" | "fabric_events" | "fabric_lease" | "fabric_graph" | "fabric_inbox")
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

pub fn legacy_catalogue() -> &'static [Value] {
    static CATALOGUE: OnceLock<Vec<Value>> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        catalogue()
            .iter()
            .map(|tool| {
                let mut tool = tool.clone();
                if let Some(fields) = tool.as_object_mut() {
                    fields.remove("outputSchema");
                }
                tool
            })
            .collect()
    })
}

/// Diagnose only a rejected serde value. Runtime is the validation owner;
/// this structural walk recovers paths that serde's errors omit.
pub fn invalid_path(name: &str, value: &Value) -> Option<String> {
    let root = &strict_catalogue()
        .iter()
        .find(|tool| tool["name"] == name)?["inputSchema"];
    structural_path(root, root, value, "arguments")
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
        return if value.is_null() {
            None
        } else {
            structural_path(root, &options[0], value, path)
        };
    }
    if let Some(options) = schema["oneOf"].as_array() {
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
        let pattern_ok = match schema["pattern"].as_str() {
            Some("^[0-9a-f]{32}$") => provenant_core::ActorId::parse(text).is_ok(),
            Some("^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$") => {
                provenant_core::OperationId::parse(text).is_ok()
            }
            Some("^(?:[0-9a-f]{40}|[0-9a-f]{64})$") => provenant_core::GitOid::parse(text).is_ok(),
            Some("^sha256:[0-9a-f]{64}$") => provenant_core::Digest::parse(text).is_ok(),
            Some(_) => !text.contains('\0'),
            None => true,
        };
        if !pattern_ok {
            return Some(path.to_owned());
        }
    }
    if let Some(object) = value.as_object() {
        let props = schema["properties"].as_object()?;
        for (key, value) in object {
            let next = format!("{path}.{key}");
            let Some(field) = props.get(key) else {
                return Some(next);
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
        for key in schema["required"].as_array()? {
            let key = key.as_str()?;
            if !object.contains_key(key) {
                return Some(format!("{path}.{key}"));
            }
        }
    }
    if let Some(items) = value.as_array() {
        return items.iter().enumerate().find_map(|(index, item)| {
            structural_path(root, &schema["items"], item, &format!("{path}[{index}]"))
        });
    }
    None
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
