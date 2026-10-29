use super::*;
use crate::tools::{IdentityReply, InvalidInput, ToolCall};
use regex::Regex;
use serde_json::{Map, Number, Value, json};
use std::{fs, path::Path};

#[test]
fn generated_schema_snapshot_is_checked_in() {
    let snapshot: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/all-tool-schemas.json"
    )))
    .expect("snapshot parses");
    let names = [
        "fabric_context",
        "fabric_mail",
        "fabric_run",
        "fabric_output",
        "fabric_discover",
    ];
    let subset: Vec<_> = snapshot
        .into_iter()
        .filter(|tool| {
            tool["name"]
                .as_str()
                .is_some_and(|name| names.contains(&name))
        })
        .collect();
    assert_eq!(catalogue(), subset);
    let actual = serde_json::to_string_pretty(&all_catalogue()).expect("catalogue serialises");
    let expected = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/all-tool-schemas.json"
    ));
    assert_eq!(actual, expected);
}

#[test]
#[ignore = "run explicitly to refresh the checked schema snapshot"]
fn update_schema_snapshot() {
    let actual = serde_json::to_string_pretty(&all_catalogue()).expect("catalogue serialises");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/all-tool-schemas.json");
    fs::write(path, actual).expect("write schema snapshot");
}

fn deref<'a>(root: &'a Value, schema: &'a Value) -> Option<&'a Value> {
    root.pointer(schema.get("$ref")?.as_str()?.strip_prefix('#')?)
}

fn integer(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
}

/// Independent evaluator for the schema keywords emitted by the catalogue.
/// Unknown keywords fail closed so catalogue growth cannot silently bypass it.
fn accepts(root: &Value, schema: &Value, value: &Value) -> bool {
    const KEYS: &[&str] = &[
        "$defs",
        "$ref",
        "additionalProperties",
        "anyOf",
        "const",
        "enum",
        "items",
        "maxItems",
        "maxLength",
        "maximum",
        "minItems",
        "minLength",
        "minimum",
        "oneOf",
        "pattern",
        "properties",
        "required",
        "type",
        "uniqueItems",
        "x-provenant-maxUtf8Bytes",
    ];
    if schema
        .as_object()
        .is_some_and(|s| s.keys().any(|k| !KEYS.contains(&k.as_str())))
    {
        return false;
    }
    if schema.get("$ref").is_some() {
        return deref(root, schema).is_some_and(|s| accepts(root, s, value));
    }
    if schema
        .get("oneOf")
        .and_then(Value::as_array)
        .is_some_and(|ss| ss.iter().filter(|s| accepts(root, s, value)).count() != 1)
        || schema
            .get("anyOf")
            .and_then(Value::as_array)
            .is_some_and(|ss| !ss.iter().any(|s| accepts(root, s, value)))
        || schema.get("const").is_some_and(|v| v != value)
        || schema
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|vs| !vs.contains(value))
    {
        return false;
    }
    let kind = schema.get("type").and_then(Value::as_str);
    if kind.is_some_and(|k| match k {
        "object" => !value.is_object(),
        "array" => !value.is_array(),
        "string" => !value.is_string(),
        "integer" => !value.as_number().is_some_and(|n| n.is_i64() || n.is_u64()),
        "boolean" => !value.is_boolean(),
        "null" => !value.is_null(),
        _ => true,
    }) {
        return false;
    }
    if let Some(s) = value.as_str() {
        let n = s.chars().count() as u64;
        if schema
            .get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|m| n < m)
            || schema
                .get("maxLength")
                .and_then(Value::as_u64)
                .is_some_and(|m| n > m)
            || schema
                .get("x-provenant-maxUtf8Bytes")
                .and_then(Value::as_u64)
                .is_some_and(|m| s.len() as u64 > m)
            || schema
                .get("pattern")
                .and_then(Value::as_str)
                .is_some_and(|p| Regex::new(p).map_or(true, |r| !r.is_match(s)))
        {
            return false;
        }
    }
    if let Some(n) = integer(value)
        && (schema
            .get("minimum")
            .and_then(integer)
            .is_some_and(|m| n < m)
            || schema
                .get("maximum")
                .and_then(integer)
                .is_some_and(|m| n > m))
    {
        return false;
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        if schema
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|rs| {
                rs.iter()
                    .filter_map(Value::as_str)
                    .any(|k| !object.contains_key(k))
            })
            || (schema["additionalProperties"] == false
                && object
                    .keys()
                    .any(|k| !properties.is_some_and(|p| p.contains_key(k))))
            || object.iter().any(|(k, v)| {
                properties
                    .and_then(|p| p.get(k))
                    .is_some_and(|s| !accepts(root, s, v))
            })
        {
            return false;
        }
    }
    if let Some(array) = value.as_array()
        && (schema
            .get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|n| (array.len() as u64) < n)
            || schema
                .get("maxItems")
                .and_then(Value::as_u64)
                .is_some_and(|n| array.len() as u64 > n)
            || (schema["uniqueItems"] == true
                && array
                    .iter()
                    .enumerate()
                    .any(|(i, v)| array[..i].contains(v)))
            || schema
                .get("items")
                .is_some_and(|s| array.iter().any(|v| !accepts(root, s, v))))
    {
        return false;
    }
    true
}

fn strings() -> Vec<String> {
    vec![
        "x".into(),
        "a".into(),
        "0".into(),
        "a=1".into(),
        "2033-05-18T03:33:20.000000Z".into(),
        "0".repeat(32),
        "0".repeat(40),
        "0".repeat(64),
        format!("sha256:{}", "0".repeat(64)),
        format!("{}.sha256:{}.0", "0".repeat(32), "0".repeat(64)),
        "a".repeat(128),
        "x".repeat(4096),
        "x".repeat(65_536),
    ]
}

fn sample(root: &Value, schema: &Value) -> Option<Value> {
    if schema.get("$ref").is_some() {
        return sample(root, deref(root, schema)?);
    }
    if let Some(options) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
    {
        return options
            .iter()
            .find_map(|s| sample(root, s).filter(|v| accepts(root, s, v)));
    }
    if let Some(v) = schema.get("const") {
        return Some(v.clone());
    }
    if let Some(v) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|vs| vs.first())
    {
        return Some(v.clone());
    }
    match schema.get("type")?.as_str()? {
        "object" => Some(Value::Object(
            schema["properties"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, s)| Some((k.clone(), sample(root, s)?)))
                .collect::<Option<Map<_, _>>>()?,
        )),
        "array" => {
            let count = schema["minItems"]
                .as_u64()
                .unwrap_or(0)
                .max(1)
                .min(schema["maxItems"].as_u64().unwrap_or(u64::MAX));
            Some(Value::Array(vec![
                sample(root, schema.get("items")?)?;
                usize::try_from(count).ok()?
            ]))
        }
        "string" => strings()
            .into_iter()
            .map(Value::String)
            .find(|v| accepts(root, schema, v)),
        "integer" => Some(json!(schema["minimum"].as_u64().unwrap_or(0))),
        "boolean" => Some(Value::Bool(false)),
        "null" => Some(Value::Null),
        _ => None,
    }
}

fn array_of(root: &Value, item: &Value, count: usize, unique: bool) -> Option<Value> {
    let item = deref(root, item).unwrap_or(item);
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let value =
            if unique && item.get("pattern").and_then(Value::as_str) == Some("^[0-9a-f]{32}$") {
                json!(format!("{index:032x}"))
            } else if unique && item.get("enum").and_then(Value::as_array).is_some() {
                item["enum"].get(index)?.clone()
            } else {
                sample(root, item)?
            };
        values.push(value);
    }
    Some(Value::Array(values))
}

fn candidates(root: &Value, schema: &Value, _value: &Value, valid: bool) -> Vec<Value> {
    let mut out = Vec::new();
    if schema.get("const").is_some() {
        out.push(if valid {
            schema["const"].clone()
        } else if schema["const"].is_string() {
            json!("__not_the_const__")
        } else {
            Value::Null
        });
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array) {
        out.extend(if valid {
            choices.clone()
        } else {
            vec![json!("__not_an_enum_value__")]
        });
    }
    match (schema.get("type").and_then(Value::as_str), valid) {
        (Some("string"), true) => {
            out.extend(
                strings()
                    .into_iter()
                    .map(Value::String)
                    .filter(|v| accepts(root, schema, v)),
            );
            for n in [schema["minLength"].as_u64(), schema["maxLength"].as_u64()]
                .into_iter()
                .flatten()
            {
                out.push(json!("x".repeat(n as usize)));
            }
            if let Some(n) = schema["x-provenant-maxUtf8Bytes"].as_u64() {
                let mut s = "é".repeat(n as usize / 2);
                if n % 2 == 1 {
                    s.push('x');
                }
                out.push(json!(s));
            }
        }
        (Some("string"), false) => {
            out.extend([json!(1), json!("")]);
            for n in [
                schema["maxLength"].as_u64(),
                schema["x-provenant-maxUtf8Bytes"].as_u64(),
            ]
            .into_iter()
            .flatten()
            {
                out.push(json!("x".repeat(n.saturating_add(1) as usize)));
            }
            if let Some(n) = schema["x-provenant-maxUtf8Bytes"].as_u64() {
                let mut s = "é".repeat(n as usize / 2);
                if n % 2 == 0 {
                    s.push('x');
                } else {
                    s.push('é');
                }
                out.push(json!(s));
            }
            if let Some(p) = schema["pattern"].as_str().and_then(|p| Regex::new(p).ok())
                && let Some(s) = strings()
                    .into_iter()
                    .chain(["\0".into(), "!".into(), "g".into()])
                    .find(|s| !p.is_match(s))
            {
                out.push(json!(s));
            }
        }
        (Some("integer"), true) => {
            out.extend([schema["minimum"].clone(), schema["maximum"].clone()])
        }
        (Some("integer"), false) => {
            out.push(json!("not an integer"));
            let min = schema["minimum"].as_u64().unwrap_or(0);
            out.push(if min == 0 { json!(-1) } else { json!(min - 1) });
            if let Some(max) = schema["maximum"].as_u64() {
                if max < u64::MAX {
                    out.push(json!(max + 1));
                } else if let Some(n) = Number::from_f64(max as f64 + 4096.0) {
                    out.push(Value::Number(n));
                }
            }
        }
        (Some("array"), true) => {
            for n in [schema["minItems"].as_u64(), schema["maxItems"].as_u64()]
                .into_iter()
                .flatten()
            {
                if let (Ok(n), Some(items)) = (usize::try_from(n), schema.get("items"))
                    && let Some(a) = array_of(root, items, n, schema["uniqueItems"] == true)
                {
                    out.push(a);
                }
            }
        }
        (Some("array"), false) => {
            out.push(json!({"wrong":"type"}));
            if schema["minItems"].as_u64().unwrap_or(0) > 0 {
                out.push(json!([]));
            }
            if let (Some(max), Some(item)) = (
                schema["maxItems"].as_u64(),
                schema.get("items").and_then(|s| sample(root, s)),
            ) {
                if let Ok(n) = usize::try_from(max.saturating_add(1)) {
                    out.push(Value::Array(vec![item.clone(); n]));
                }
                if schema["uniqueItems"] == true {
                    out.push(json!([item, item]));
                }
            }
        }
        (Some("boolean"), true) => out.extend([json!(false), json!(true)]),
        (Some("boolean"), false) => {
            out.extend([Value::Null, json!(0), json!("false"), json!([]), json!({})]);
        }
        (Some("object"), false) => out.push(json!("wrong type")),
        (Some("null"), true) => out.push(Value::Null),
        (Some("null"), false) => out.push(json!({"not":"null"})),
        _ => {}
    }
    out.retain(|v| accepts(root, schema, v) == valid);
    out.sort_by_key(Value::to_string);
    out.dedup();
    out
}

fn child_path(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}

fn replace(request: &Value, path: &str, value: Value) -> Option<Value> {
    let mut result = request.clone();
    if path.is_empty() {
        result = value;
    } else {
        *result.pointer_mut(path)? = value;
    }
    Some(result)
}

fn normalise(value: &mut Value) {
    fn paths(value: &mut Value) {
        if let Some(object) = value.as_object_mut() {
            if object.get("encoding") == Some(&json!("native_bytes")) {
                object.insert("path".into(), json!("ff"));
            }
            for child in object.values_mut() {
                paths(child);
            }
        } else if let Some(array) = value.as_array_mut() {
            for child in array {
                paths(child);
            }
        }
    }
    paths(value);
}

fn compare(
    tool: &str,
    root: &Value,
    value: &Value,
    path: &str,
    valid: bool,
    counts: &mut (usize, usize),
) {
    let schema = accepts(root, root, value);
    let serde = ToolCall::parse(tool, value.clone()).is_ok();
    assert_eq!(
        schema, serde,
        "schema/serde mismatch at {tool} {path}: {value}"
    );
    assert_eq!(
        schema, valid,
        "unexpected generated case at {tool} {path}: {value}"
    );
    if valid {
        let wire = &all_catalogue()
            .iter()
            .find(|wire| wire["name"] == tool)
            .expect("wire tool")["inputSchema"];
        assert!(
            accepts(wire, wire, value),
            "wire rejects valid call at {tool} {path}: {value}"
        );
        counts.0 += 1;
    } else {
        counts.1 += 1;
    }
}

/// Check each node in place. JSON pointers locate the current field in the
/// whole request, so every generated value is compared immediately.
fn walk(
    tool: &str,
    root: &Value,
    schema: &Value,
    request: &Value,
    path: &str,
    variants: bool,
    counts: &mut (usize, usize),
) {
    let Some(value) = request.pointer(path) else {
        panic!("missing generated pointer {path}");
    };
    if schema.get("$ref").is_some() {
        if let Some(s) = deref(root, schema) {
            walk(tool, root, s, request, path, variants, counts);
        }
        return;
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(options) = schema.get(key).and_then(Value::as_array) {
            let Some((active, selected)) = options
                .iter()
                .enumerate()
                .find(|(_, s)| accepts(root, s, value))
            else {
                return;
            };
            if variants {
                for (i, option) in options.iter().enumerate().filter(|(i, _)| *i != active) {
                    if let Some(v) =
                        sample(root, option).filter(|v| accepts(root, schema, v) && v != value)
                        && let Some(mut changed) = replace(request, path, v)
                    {
                        normalise(&mut changed);
                        compare(tool, root, &changed, path, true, counts);
                        walk(tool, root, schema, &changed, path, false, counts);
                    }
                    let _ = i;
                }
            }
            walk(tool, root, selected, request, path, variants, counts);
            return;
        }
    }
    for candidate in candidates(root, schema, value, true)
        .into_iter()
        .filter(|v| v != value)
    {
        if let Some(mut changed) = replace(request, path, candidate) {
            normalise(&mut changed);
            compare(tool, root, &changed, path, true, counts);
        }
    }
    for candidate in candidates(root, schema, value, false) {
        if let Some(changed) = replace(request, path, candidate) {
            compare(tool, root, &changed, path, false, counts);
        }
    }
    if let (Some(object), Some(properties)) = (
        value.as_object(),
        schema.get("properties").and_then(Value::as_object),
    ) {
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            let mut changed = request.clone();
            let target = if path.is_empty() {
                Some(&mut changed)
            } else {
                changed.pointer_mut(path)
            };
            if let Some(target) = target.and_then(Value::as_object_mut) {
                target.insert("__schema_probe_unknown".into(), Value::Bool(true));
                compare(tool, root, &changed, path, false, counts);
            }
        }
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if object.contains_key(required) {
                let mut changed = request.clone();
                let target = if path.is_empty() {
                    Some(&mut changed)
                } else {
                    changed.pointer_mut(path)
                };
                if let Some(target) = target.and_then(Value::as_object_mut) {
                    target.remove(required);
                    compare(
                        tool,
                        root,
                        &changed,
                        &child_path(path, required),
                        false,
                        counts,
                    );
                }
            }
        }
        for (key, child) in properties {
            if object.contains_key(key) {
                walk(
                    tool,
                    root,
                    child,
                    request,
                    &child_path(path, key),
                    variants,
                    counts,
                );
            }
        }
    }
    if let (Some(array), Some(items)) = (value.as_array(), schema.get("items")) {
        for index in 0..array.len() {
            walk(
                tool,
                root,
                items,
                request,
                &child_path(path, &index.to_string()),
                variants,
                counts,
            );
        }
    }
}

#[test]
fn generated_schema_values_match_strict_serde_for_every_operation_and_field() {
    let mut counts = (0, 0);
    for tool in strict_catalogue() {
        let name = tool["name"].as_str().expect("tool name");
        let root = &tool["inputSchema"];
        let operations = root["oneOf"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![root.clone()]);
        for operation in &operations {
            let mut request = sample(root, operation).unwrap_or_else(|| {
                panic!("cannot generate valid operation for {name}: {operation}")
            });
            normalise(&mut request);
            compare(name, root, &request, "operation", true, &mut counts);
            walk(name, root, operation, &request, "", true, &mut counts);
        }
    }
    assert!(
        counts.0 > 0 && counts.1 > counts.0,
        "insufficient generated probes: {counts:?}"
    );
}

/// Enumerate every object shape, including optional objects, array elements and
/// every tagged variant, in otherwise valid complete tool requests.
fn unknown_field_cases(
    root: &Value,
    schema: &Value,
    request: &Value,
    path: &str,
    cases: &mut Vec<(String, Value)>,
) {
    if let Some(resolved) = deref(root, schema) {
        unknown_field_cases(root, resolved, request, path, cases);
        return;
    }
    if let Some(options) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
    {
        for option in options {
            // Scalar alternatives cannot contain unknown object fields.
            if matches!(
                option.get("type").and_then(Value::as_str),
                Some("null" | "string" | "integer" | "boolean")
            ) {
                continue;
            }

            let value = sample(root, option)
                .unwrap_or_else(|| panic!("no variant sample at {path}: {option}"));
            let mut request = replace(request, path, value).expect("variant path exists");
            normalise(&mut request);
            unknown_field_cases(root, option, &request, path, cases);
        }
        return;
    }
    let value = request.pointer(path).expect("sample path exists");
    if value.is_object() {
        assert_eq!(
            schema["additionalProperties"], false,
            "open input at {path}"
        );
        let mut changed = request.clone();
        changed
            .pointer_mut(path)
            .and_then(Value::as_object_mut)
            .expect("object path")
            .insert("__unknown_input".into(), json!(true));
        cases.push((path.into(), changed));
        for (key, field) in schema["properties"].as_object().into_iter().flatten() {
            unknown_field_cases(root, field, request, &child_path(path, key), cases);
        }
    } else if let Some(items) = value.as_array() {
        for index in 0..items.len() {
            unknown_field_cases(
                root,
                &schema["items"],
                request,
                &child_path(path, &index.to_string()),
                cases,
            );
        }
    }
}

#[test]
fn every_tool_rejects_unknown_fields_in_every_input_object() {
    let mut checked_tools = Vec::new();
    let mut checked_paths = Vec::new();
    for tool in strict_catalogue() {
        let name = tool["name"].as_str().expect("tool name");
        let root = &tool["inputSchema"];
        let mut request = sample(root, root).expect("tool has a sample");
        normalise(&mut request);
        assert!(
            ToolCall::parse(name, request.clone()).is_ok(),
            "{name}: {request}"
        );
        let mut cases = Vec::new();
        unknown_field_cases(root, root, &request, "", &mut cases);
        assert!(!cases.is_empty(), "no object probes for {name}");
        for (path, request) in cases {
            assert!(!accepts(root, root, &request), "{name} {path}: {request}");
            assert!(
                invalid_path(name, &request).is_some_and(|path| path.ends_with(".__unknown_input")),
                "schema walker accepted unknown field at {name} {path}: {request}"
            );
            assert!(ToolCall::parse(name, request).is_err(), "{name} {path}");
            checked_paths.push(format!("{name}{path}"));
        }
        checked_tools.push(name);
    }
    assert_eq!(checked_tools, TOOL_NAMES.map(|(_, name)| name));
    // These nested shared records tolerate future fields on peer ingress, so
    // their strict MCP boundary must be exercised below the top-level input.
    for path in [
        "fabric_run/tasks/0/route",
        "fabric_run/tasks/0/deliver",
        "fabric_mail/artifacts/0/path",
        "fabric_lease/scope",
        "fabric_inbox/request/work/repository/relative_to_root",
    ] {
        assert!(
            checked_paths.iter().any(|checked| checked == path),
            "missing {path}"
        );
    }
}

#[test]
fn closed_object_without_properties_rejects_unknown_fields() {
    let schema = json!({"type": "object", "additionalProperties": false});
    assert!(accepts(&schema, &schema, &json!({})));
    assert!(!accepts(&schema, &schema, &json!({"unknown": true})));
    assert_eq!(
        structural_path(&schema, &schema, &json!({}), "arguments"),
        None
    );
    assert_eq!(
        structural_path(&schema, &schema, &json!({"unknown": true}), "arguments"),
        Some("arguments.unknown".into())
    );
}

#[test]
fn wire_schema_has_no_dangling_or_single_use_defs() {
    fn refs(root: &Value, node: &Value, counts: &mut std::collections::BTreeMap<String, usize>) {
        match node {
            Value::Object(fields) => {
                if let Some(pointer) = fields.get("$ref").and_then(Value::as_str) {
                    let name = pointer
                        .strip_prefix("#/$defs/")
                        .expect("root definition reference");
                    let definition = root
                        .pointer(pointer.strip_prefix('#').unwrap())
                        .expect("resolved reference");
                    let count = counts.entry(name.to_owned()).or_default();
                    *count += 1;
                    if *count == 1 {
                        refs(root, definition, counts);
                    }
                }
                for (key, child) in fields {
                    if key != "$defs" {
                        refs(root, child, counts);
                    } else {
                        assert!(std::ptr::eq(node, root), "nested definitions");
                    }
                }
            }
            Value::Array(items) => {
                for child in items {
                    refs(root, child, counts);
                }
            }
            _ => {}
        }
    }
    for tool in all_catalogue() {
        let root = &tool["inputSchema"];
        let mut counts = std::collections::BTreeMap::new();
        refs(root, root, &mut counts);
        if let Some(defs) = root.get("$defs").and_then(Value::as_object) {
            assert!(!defs.is_empty(), "empty definitions in {}", tool["name"]);
            for name in defs.keys() {
                assert!(
                    counts.get(name).is_some_and(|count| *count >= 2),
                    "unused or single-use {name} in {}",
                    tool["name"]
                );
            }
        }
    }
}

#[test]
fn wire_strings_carry_only_type_and_max_length() {
    fn check(node: &Value) {
        match node {
            Value::Object(fields) => {
                if fields.get("type").and_then(Value::as_str) == Some("string") {
                    assert!(!fields.contains_key("minLength"));
                    assert!(!fields.keys().any(|key| key.starts_with("x-provenant-")));
                    assert_ne!(
                        fields.get("pattern").and_then(Value::as_str),
                        Some(r"^[^\u0000]*$")
                    );
                }
                for child in fields.values() {
                    check(child);
                }
            }
            Value::Array(items) => {
                for child in items {
                    check(child);
                }
            }
            _ => {}
        }
    }
    for tool in all_catalogue() {
        check(tool);
    }
}

#[test]
fn tool_schema_size_is_bounded() {
    // Measured compact tool bytes plus 10%, rounded up.
    let bounds = [
        ("fabric_context", 2_103),
        ("fabric_mail", 2_800),
        ("fabric_run", 5_000),
        ("fabric_output", 1_600),
        ("fabric_discover", 1_000),
    ];
    assert_eq!(catalogue().len(), bounds.len());
    let mut total = 0;
    for ((name, bound), tool) in bounds.into_iter().zip(catalogue()) {
        assert_eq!(tool["name"], name);
        assert_eq!(tool["_meta"]["io.provenant/schemaVersion"], 2);
        let size = serde_json::to_vec(tool).unwrap().len();
        assert!(size <= bound, "{name}: {size} bytes exceeds {bound}");
        assert!(size <= 5_000, "{name}: {size} bytes exceeds hard cap");
        total += size;
    }
    assert!(total <= 12 * 1024, "catalogue: {total} bytes");
}

#[test]
fn relaxed_wire_text_still_reports_strict_invalid_paths() {
    let wire = &all_catalogue()
        .iter()
        .find(|tool| tool["name"] == "fabric_events")
        .unwrap()["inputSchema"];
    for note in ["", "\0", &"é".repeat(32_769)] {
        let value = json!({"op":"note", "note":note});
        assert!(accepts(wire, wire, &value));
        assert!(
            matches!(ToolCall::parse("fabric_events", value), Err(InvalidInput::Arguments { path }) if path == "arguments.note")
        );
    }
    let value = json!({"op":"note", "note":"\0"});
    assert_eq!(
        invalid_path("fabric_events", &value).as_deref(),
        Some("arguments.note")
    );
    let value = json!({"op":"send", "recipients":["00000000000000000000000000000001"], "kind":"note", "body":"body", "artifacts":[{"digest":null, "path":{"encoding":"utf8", "path":"\0"}}]});
    assert_eq!(
        invalid_path("fabric_mail", &value).as_deref(),
        Some("arguments.artifacts[0].path.path")
    );
}

#[test]
fn near_argument_suggests_only_a_unique_close_name() {
    assert_eq!(nearest("cut", ["cat", "cot"].into_iter()), None);
    assert_eq!(nearest("ab", ["xy"].into_iter()), None);
    assert_eq!(nearest("tmp", ["sessions", "lease"].into_iter()), None);
    assert_eq!(
        nearest("sessionId", ["session"].into_iter()),
        Some("session")
    );
    assert_eq!(
        nearest("Session-ID", ["session_id"].into_iter()),
        Some("session_id")
    );
    assert_eq!(nearest("op", ["ok"].into_iter()), Some("ok"));
    let value = json!({"op":"summary_show","sessionId":"s"});
    assert_eq!(
        near_argument("fabric_chair", &value, "arguments.sessionId").as_deref(),
        Some("session")
    );
    let value = json!({"op":"summary_show","zzz":"s"});
    assert_eq!(near_argument("fabric_chair", &value, "arguments.zzz"), None);
}

#[test]
fn every_tool_names_its_case_folded_keys_and_actions() {
    for tool in strict_catalogue() {
        let name = tool["name"].as_str().unwrap();
        let root = &tool["inputSchema"];
        let variants: Vec<&Value> = match root["oneOf"].as_array() {
            Some(options) => options.iter().collect(),
            None => vec![root],
        };
        for variant in variants {
            let tag = ["op", "kind", "scope"]
                .into_iter()
                .find(|tag| variant["properties"][*tag]["const"].is_string());
            let mut base = Map::new();
            if let Some(tag) = tag {
                base.insert(tag.into(), variant["properties"][tag]["const"].clone());
            }
            let Some(fields) = variant["properties"].as_object() else {
                continue;
            };
            for key in fields
                .keys()
                .filter(|key| !matches!(key.as_str(), "op" | "kind" | "scope"))
            {
                let typed = key.to_uppercase();
                let mut value = base.clone();
                value.insert(typed.clone(), Value::Null);
                assert_eq!(
                    near_argument(name, &Value::Object(value), &format!("arguments.{typed}"))
                        .as_deref(),
                    Some(key.as_str()),
                    "{name}: {key}"
                );
            }
        }
        if let Some(options) = root["oneOf"].as_array() {
            let Some(tag) = ["op", "kind", "scope"].into_iter().find(|tag| {
                options
                    .iter()
                    .any(|o| o["properties"][*tag]["const"].is_string())
            }) else {
                continue;
            };
            for option in options {
                let Some(action) = option["properties"][tag]["const"].as_str() else {
                    continue;
                };
                let value = json!({ tag: action.to_uppercase() });
                assert_eq!(
                    near_argument(name, &value, &format!("arguments.{tag}")).as_deref(),
                    Some(action),
                    "{name}: {action}"
                );
            }
        }
    }
}

#[test]
fn rejected_values_describe_expected_constraints() {
    let input = json!({"op":"read", "target":{"attempt":"bad"}, "part":"other"});
    assert!(expected("fabric_output", &input, "arguments.part").contains("result"));
    assert!(expected("fabric_output", &input, "arguments.target.attempt").contains("[0-9a-f]"));
    let input = json!({"op":"wat"});
    assert!(expected("fabric_run", &input, "arguments.op").contains("submit"));
}

#[test]
fn schema_catalogue_is_fixed_order_and_has_no_legacy_aliases() {
    let expected = [
        "fabric_context",
        "fabric_mail",
        "fabric_run",
        "fabric_output",
        "fabric_discover",
    ];
    let names: Vec<_> = catalogue()
        .iter()
        .filter_map(|v| v["name"].as_str())
        .collect();
    assert_eq!(names, expected);
    let advanced: Vec<_> = advanced_catalogue()
        .iter()
        .map(|v| v["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        advanced,
        [
            "fabric_task",
            "fabric_events",
            "fabric_lease",
            "fabric_graph",
            "fabric_inbox",
            "fabric_console",
            "fabric_chair"
        ]
    );
    assert!(ToolCall::parse("fabric_not_a_tool", json!({"op":"inspect"})).is_err());
}

#[test]
fn output_schema_lists_only_supported_parts() {
    let output = all_catalogue()
        .iter()
        .find(|tool| tool["name"] == "fabric_output")
        .expect("output tool");
    let read = output["inputSchema"]["oneOf"]
        .as_array()
        .expect("output operations")
        .iter()
        .find(|operation| operation["properties"]["op"]["const"] == "read")
        .expect("read operation");

    assert_eq!(
        read["properties"]["part"]["enum"],
        json!(["prompt", "result", "stderr", "events"])
    );
    for tool in all_catalogue() {
        let supported = matches!(tool["name"].as_str(), Some("fabric_context"));
        assert_eq!(tool.get("outputSchema").is_some(), supported);
        if supported {
            assert_eq!(tool["outputSchema"]["type"], "object");
        }
    }
}

#[test]
fn published_output_schemas_accept_real_runtime_records() {
    use provenant_core::{HostId, ProjectId};
    use provenant_fabric_runtime::identity::{
        Address, CrossHostProjectKey, FederatedProject, Identity,
    };
    use provenant_records::AbsolutePath;

    let host = HostId::from_bytes([1; 16]);
    let mut identity = Identity {
        project: ProjectId::from_bytes([2; 16]),
        host,
        inbox: Address::new(host, "schema-test").unwrap(),
        client: "codex".into(),
        cwd: AbsolutePath::new(Path::new("/repo/worktree")).unwrap(),
        common_dir: AbsolutePath::new(Path::new("/repo/.git")).unwrap(),
        federation: None,
    };
    let output_schema = |name| {
        &all_catalogue()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()["outputSchema"]
    };
    let check = |name, value: Value| {
        let schema = output_schema(name);
        assert!(accepts(schema, schema, &value), "{name}: {value}");
    };
    let reply = IdentityReply {
        identity: &identity,
        server_version: env!("CARGO_PKG_VERSION"),
    };
    check("fabric_context", serde_json::to_value(reply).unwrap());

    identity.federation = Some(FederatedProject {
        key: CrossHostProjectKey::from_home(Path::new("/"), Path::new("/repo/.git")).unwrap(),
        project: ProjectId::from_bytes([4; 16]),
    });
    let reply = IdentityReply {
        identity: &identity,
        server_version: env!("CARGO_PKG_VERSION"),
    };
    check("fabric_context", serde_json::to_value(reply).unwrap());
}

#[test]
fn submit_accepts_optional_bounded_work_context() {
    use provenant_fabric_runtime::coordination::facade::SubmitInput;
    let schema = &all_catalogue()
        .iter()
        .find(|tool| tool["name"] == "fabric_run")
        .unwrap()["inputSchema"];
    let mut input = json!({
        "op":"submit", "operation_id":"work-context", "tasks":[{
            "prompt":"review", "route":{"kind":"named","name":"utility"}, "timeout_seconds":1,
            "context":{"graph_id":"graph", "node_id":"inspect", "pattern":"council"}
        }]
    });
    assert!(accepts(schema, schema, &input));
    assert!(ToolCall::parse("fabric_run", input.clone()).is_ok());
    let SubmitInput::Submit { tasks, .. } = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(
        tasks[0].context.as_ref().unwrap().pattern.as_deref(),
        Some("council")
    );
    for invalid in ["".to_owned(), "x".repeat(257), "line\nbreak".to_owned()] {
        input["tasks"][0]["context"]["pattern"] = json!(invalid);
        assert!(ToolCall::parse("fabric_run", input.clone()).is_err());
    }
    input["tasks"][0].as_object_mut().unwrap().remove("context");
    assert!(accepts(schema, schema, &input));
    let SubmitInput::Submit { tasks, .. } = serde_json::from_value(input).unwrap();
    assert!(tasks[0].context.is_none());
}

#[test]
fn submit_accepts_optional_bounded_git_base() {
    use provenant_fabric_runtime::coordination::facade::SubmitInput;

    let schema = &all_catalogue()
        .iter()
        .find(|tool| tool["name"] == "fabric_run")
        .unwrap()["inputSchema"];
    let mut input = json!({
        "op":"submit", "operation_id":"git-context", "tasks":[{
            "prompt":"review", "route":{"kind":"named","name":"utility"}, "timeout_seconds":1, "base":"main", "build_heavy":true
        }]
    });
    assert!(accepts(schema, schema, &input));
    assert!(ToolCall::parse("fabric_run", input.clone()).is_ok());
    let SubmitInput::Submit { tasks, .. } = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(tasks[0].base.as_deref(), Some("main"));
    assert!(tasks[0].build_heavy);
    input["tasks"][0]["base"] = json!("a".repeat(1025));
    assert!(!accepts(schema, schema, &input));
    assert!(ToolCall::parse("fabric_run", input).is_err());
}

#[test]
fn submit_head_matches_schema_length_bound() {
    let schema = &all_catalogue()
        .iter()
        .find(|tool| tool["name"] == "fabric_run")
        .unwrap()["inputSchema"];
    let mut input = json!({
        "op":"submit", "operation_id":"git-head", "tasks":[{
            "prompt":"review", "route":{"kind":"named","name":"utility"}, "timeout_seconds":1,
            "head":"a".repeat(64)
        }]
    });
    assert!(accepts(schema, schema, &input));
    assert!(ToolCall::parse("fabric_run", input.clone()).is_ok());
    input["tasks"][0]["head"] = json!("a".repeat(65));
    assert!(!accepts(schema, schema, &input));
    assert!(ToolCall::parse("fabric_run", input).is_err());
}

#[test]
fn submit_accepts_optional_task_title() {
    use provenant_fabric_runtime::coordination::facade::SubmitInput;

    let schema = &all_catalogue()
        .iter()
        .find(|tool| tool["name"] == "fabric_run")
        .unwrap()["inputSchema"];
    let mut input = json!({
        "op":"submit", "operation_id":"titled-task", "tasks":[{
            "prompt":"review", "title":"Review the change",
            "route":{"kind":"named","name":"utility"}, "timeout_seconds":1
        }]
    });
    assert!(accepts(schema, schema, &input));
    assert!(ToolCall::parse("fabric_run", input.clone()).is_ok());
    let SubmitInput::Submit { tasks, .. } = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(tasks[0].title.as_deref(), Some("Review the change"));
    input["tasks"][0].as_object_mut().unwrap().remove("title");
    let SubmitInput::Submit { tasks, .. } = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(tasks[0].title, None);
    assert!(accepts(schema, schema, &input));
    input["tasks"][0]["title"] = json!("");
    assert!(ToolCall::parse("fabric_run", input).is_err());
}

#[test]
fn peer_worktree_requests_are_not_mcp_submit_inputs() {
    use provenant_fabric_runtime::coordination::facade::SubmitInput;

    let schema = &all_catalogue()
        .iter()
        .find(|tool| tool["name"] == "fabric_run")
        .unwrap()["inputSchema"];
    for new_worktree in [
        json!({"base":"main","branch":"feat/peer-writer"}),
        Value::Null,
    ] {
        let input = json!({
            "op":"submit", "operation_id":"peer-writer", "tasks":[{
                "prompt":"work", "route":{"kind":"named","name":"utility"}, "timeout_seconds":1,
                "access":"worktree_write", "new_worktree":new_worktree
            }]
        });
        // Peer ingress still deserialises the shared wire type.
        assert!(serde_json::from_value::<SubmitInput>(input.clone()).is_ok());
        assert!(!accepts(schema, schema, &input));
        assert!(matches!(
            ToolCall::parse("fabric_run", input),
            Err(InvalidInput::Arguments { path }) if path == "arguments.tasks[0].new_worktree"
        ));
    }
}

#[test]
fn text_byte_limit_is_enforced_even_when_codepoint_length_is_smaller() {
    let at_limit = "😀".repeat(16_384);
    assert_eq!(at_limit.len(), 65_536);
    assert!(ToolCall::parse("fabric_events", json!({"op":"note","note":at_limit})).is_ok());
    let over_limit = "😀".repeat(16_385);
    assert!(ToolCall::parse("fabric_events", json!({"op":"note","note":over_limit})).is_err());
}
