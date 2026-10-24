//! Explicit field catalogue, shared schema pieces and a cached MCP catalogue.
//! Runtime owns bounds; generated schema/serde probes detect catalogue drift.
use serde_json::{Value, json};
use std::sync::OnceLock;

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
fn array(items: Value, min: usize, max: usize) -> Value {
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
fn proof(key: &str, landing: bool) -> Value {
    let mut fields = json!({key: reference("Id"), "generation":integer(0,u64::MAX),"token":array(integer(0,255),16,16)});
    if landing {
        fields["reference"] = text(1024);
    }
    object(fields)
}
fn definitions() -> Value {
    let message_ref = tagged(
        "scope",
        json!({
            "local":{"id":reference("Id")},
            "remote":{"host":reference("Id"),"id":reference("Id")},
            "legacy":{"reference":object(json!({"source":text(1024),"project":text(1024),"id":text(1024)}))}
        }),
    );
    json!({
        "Id":pattern("^[0-9a-f]{32}$"),
        "OperationId":pattern("^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"),
        "GitOid":pattern("^(?:[0-9a-f]{40}|[0-9a-f]{64})$"),
        "AttemptTarget":object(json!({"attempt":reference("Id"),"host?":nullable(reference("Id"))})),
        "TaskProof":proof("task",false),"ClaimProof":proof("id",false),"LandingProof":proof("id",true),
        "MessageRef":message_ref,
        "Artifact":object(json!({"path":object(json!({"encoding":choices(&["utf8","native_bytes"]),"path":text(4096)})),"digest":nullable(pattern("^sha256:[0-9a-f]{64}$"))})),
        "Route":object(json!({"adapter?":nullable(text(128)),"model?":nullable(text(128)),"route?":nullable(text(128)),"role?":nullable(text(128)),"task_class?":nullable(text(128)),"effort?":nullable(text(128))})),
        "Scope":tagged("kind",json!({"issue":{"key":text(1024)},"paths":{"paths":array(text(4096),1,100)},"worktree":{"path":text(4096)}}))
    })
}
fn build_catalogue() -> Vec<Value> {
    let page = integer(1, 100);
    let lease = integer(1000, 3_600_000);
    let wait = integer(1000, 55_000);
    let seq = integer(0, i64::MAX as u64);
    let count = integer(0, u64::MAX);
    let artifacts = array(reference("Artifact"), 0, 100);
    let mut recipients = array(reference("Id"), 1, 100);
    recipients["uniqueItems"] = json!(true);
    let tools = [
        (
            "fabric_identity",
            "Inspect the authenticated Fabric identity.",
            json!({"inspect":{}}),
        ),
        (
            "fabric_mail",
            "Send and read mail with explicit cursors.",
            json!({
                "send":{"recipients":recipients,"kind":choices(&["note","request","response"]),"body":text(65_536),"artifacts":artifacts,"reply_to?":nullable(reference("MessageRef")),"task?":nullable(reference("MessageRef"))},
                "read":{"after?":nullable(seq.clone()),"limit":page.clone()},"mark_read":{"through":seq.clone()}
            }),
        ),
        (
            "fabric_task",
            "Create and manage fenced project tasks.",
            json!({
                "create":{"objective":text(65_536),"note?":nullable(text(65_536))},
                "list":{"state?":nullable(choices(&["open","done","cancelled"])),"after?":nullable(reference("Id")),"limit":page.clone()},
                "claim":{"id":reference("Id"),"lease_ms":lease.clone()},
                "update":{"proof":reference("TaskProof"),"objective":text(65_536),"note?":nullable(text(65_536))},
                "finish":{"proof":reference("TaskProof"),"state":choices(&["done","cancelled"])},"release":{"proof":reference("TaskProof")}
            }),
        ),
        (
            "fabric_events",
            "Read the activity journal or append a note.",
            json!({
                "read":{"after?":nullable(seq),"limit":page.clone(),"wait_ms?":nullable(wait.clone())},
                "note":{"note":text(65_536),"dedup_key?":nullable(text(1024))}
            }),
        ),
        (
            "fabric_claim",
            "Manage advisory work claims.",
            json!({
                "acquire":{"scope":reference("Scope"),"lease_ms":lease.clone()},"renew":{"proof":reference("ClaimProof"),"lease_ms":lease.clone()},
                "verify":{"proof":reference("ClaimProof")},"release":{"proof":reference("ClaimProof")},"list":{"after?":nullable(reference("Id")),"limit":page.clone()}
            }),
        ),
        (
            "fabric_landing",
            "Manage landing leases and observe a generation.",
            json!({
                "acquire":{"reference":text(1024),"expected":reference("GitOid"),"candidate":reference("GitOid"),"lease_ms":lease.clone()},
                "renew":{"proof":reference("LandingProof"),"lease_ms":lease},"verify":{"proof":reference("LandingProof")},"release":{"proof":reference("LandingProof")},
                "wait":{"reference":text(1024),"generation":count.clone(),"wait_ms":wait.clone()}
            }),
        ),
        (
            "fabric_submit",
            "Submit one batch to the run service.",
            json!({
                "submission":{"operation_id":reference("OperationId"),"tasks":array(object(json!({"prompt":text(65_536),"account?":nullable(text(64)),"access?":choices(&["read_only","worktree_write"]),"worktree?":nullable(text(4096)),"route":reference("Route"),"timeout_ms":integer(1000,14_400_000),"delivery_id?":nullable(reference("Id"))})),1,100),"concurrency?":nullable(integer(1,8)),"knobs?":array(text(256),0,64)}
            }),
        ),
        (
            "fabric_run",
            "List, inspect, wait for, cancel or resume runs.",
            json!({
                "list":{"after?":nullable(reference("AttemptTarget")),"limit":page.clone()},"status":{"target":reference("AttemptTarget")},
                "wait":{"targets":array(reference("AttemptTarget"),1,100),"condition":choices(&["any","all"]),"wait_ms":wait},
                "cancel":{"target":reference("AttemptTarget"),"operation_id":reference("OperationId")},
                "resume":{"target":reference("AttemptTarget"),"operation_id":reference("OperationId"),"prompt":text(65_536),"expected_generation":count.clone()}
            }),
        ),
        (
            "fabric_graph",
            "Run, inspect, cancel or resume a checkpointed graph. Run and resume take a project-local document path.",
            json!({
                "run":{"document":text(4096)},"status":{"id":pattern("^[A-Za-z0-9._-]{1,64}$")},
                "cancel":{"id":pattern("^[A-Za-z0-9._-]{1,64}$")},"resume":{"document":text(4096)}
            }),
        ),
        (
            "fabric_output",
            "Read bounded output or fetch a manifest artefact.",
            json!({
                "read":{"target":reference("AttemptTarget"),"part":choices(&["stdout","stderr","combined"]),"offset":count,"max_bytes":integer(1,65_536)},
                "fetch":{"target":reference("AttemptTarget"),"artifact":reference("Artifact"),"max_bytes":integer(1,provenant_fabric_runtime::coordination::input::MAX_FETCH_BYTES)}
            }),
        ),
        (
            "fabric_hosts",
            "List peer hosts and readiness observations.",
            json!({"list":{"after?":nullable(reference("Id")),"limit":page}}),
        ),
    ];
    tools
        .into_iter()
        .map(|(name, description, operations)| {
            let mut input = tagged("op", operations);
            input["type"] = json!("object");
            input["$defs"] = definitions();
            json!({"name":name,"description":description,"inputSchema":input})
        })
        .collect()
}

pub fn catalogue() -> &'static [Value] {
    static CATALOGUE: OnceLock<Vec<Value>> = OnceLock::new();
    CATALOGUE.get_or_init(build_catalogue)
}

/// Diagnose only a rejected serde value. Runtime is the validation owner;
/// this structural walk recovers paths that serde's errors omit.
pub fn invalid_path(name: &str, value: &Value) -> Option<String> {
    let root = &catalogue().iter().find(|tool| tool["name"] == name)?["inputSchema"];
    structural_path(root, root, value, "arguments")
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
        return Some(path.to_owned());
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
