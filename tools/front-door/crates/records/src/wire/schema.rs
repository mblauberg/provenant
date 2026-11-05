//! Small JSON Schema (draft 2020-12) builders and a validator for the subset
//! they emit. The workspace allows no schema-derivation dependency (ADR 0028),
//! so each wire type pairs its `Serialize` impl with a builder-written schema,
//! and tests serialise real values and validate them against it, which is how
//! drift between the two is caught.
use serde_json::{Map, Value, json};

/// Dialect every published schema declares.
pub const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Any JSON value. Used where another crate's record owns the shape.
pub fn any() -> Value {
    json!({})
}
pub fn string() -> Value {
    json!({"type": "string"})
}
/// Pattern for padded RFC 4648 base64 strings.
pub const BASE64_PATTERN: &str = r"^([A-Za-z0-9+/]{4})*([A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$";
pub fn base64() -> Value {
    json!({"type": "string", "pattern": BASE64_PATTERN})
}
pub fn boolean() -> Value {
    json!({"type": "boolean"})
}
pub fn number() -> Value {
    json!({"type": "number"})
}
pub fn integer() -> Value {
    json!({"type": "integer"})
}
pub fn unsigned() -> Value {
    json!({"type": "integer", "minimum": 0})
}
pub fn constant(value: impl Into<Value>) -> Value {
    json!({"const": value.into()})
}
pub fn choices(values: &[&str]) -> Value {
    json!({"type": "string", "enum": values})
}
pub fn array(items: Value) -> Value {
    json!({"type": "array", "items": items})
}
/// A generic object whose members this schema does not describe.
pub fn open_object() -> Value {
    json!({"type": "object"})
}
/// `inner` or null.
pub fn nullable(inner: Value) -> Value {
    json!({"anyOf": [inner, {"type": "null"}]})
}
pub fn one_of(variants: Vec<Value>) -> Value {
    json!({"oneOf": variants})
}
/// A closed object: unknown members are invalid, so a field added to a type
/// without its schema fails the validation tests.
pub fn object(required: &[(&str, Value)], optional: &[(&str, Value)]) -> Value {
    let mut properties = Map::new();
    for (name, schema) in required.iter().chain(optional) {
        properties.insert((*name).to_owned(), schema.clone());
    }
    let names: Vec<&str> = required.iter().map(|(name, _)| *name).collect();
    json!({"type": "object", "properties": properties, "required": names, "additionalProperties": false})
}
/// Like [`object`] but members from newer writers are allowed, for records that
/// flatten unknown fields.
pub fn open(required: &[(&str, Value)], optional: &[(&str, Value)]) -> Value {
    let mut schema = object(required, optional);
    if let Some(map) = schema.as_object_mut() {
        map.remove("additionalProperties");
    }
    schema
}
/// A contained path: `PathWire`'s `{encoding, path}`. Native-byte paths carry
/// lowercase hex in `path`, so the encoding tag is part of the contract.
pub fn path_wire() -> Value {
    open(
        &[
            ("encoding", choices(&["utf8", "native_bytes"])),
            ("path", string()),
        ],
        &[],
    )
}
/// The explicit root-plus-path-wire envelope used for absolute paths.
pub fn absolute_path() -> Value {
    object(
        &[(
            "relative_to_root",
            object(
                &[
                    ("encoding", choices(&["utf8", "native_bytes"])),
                    ("path", string()),
                ],
                &[],
            ),
        )],
        &[],
    )
}
/// The tagged absolute path form used by worktree listings.
pub fn worktree_path() -> Value {
    one_of(vec![
        object(&[("kind", constant("root"))], &[]),
        object(
            &[
                ("kind", constant("path")),
                ("root", constant("/")),
                ("relative", path_wire()),
            ],
            &[],
        ),
    ])
}
/// Add the dialect, identifier and title to a root schema.
pub fn root(id: &str, title: &str, mut schema: Value) -> Value {
    if let Some(map) = schema.as_object_mut() {
        map.insert("$schema".into(), DIALECT.into());
        map.insert("$id".into(), id.into());
        map.insert("title".into(), title.into());
    }
    schema
}

/// Validate `instance` against the subset of JSON Schema the builders emit:
/// `type`, `const`, `enum`, `properties`, `required`, `additionalProperties:
/// false`, `items`, `oneOf`, `anyOf`, `minimum`, `maximum`, and the base64
/// pattern emitted above. Other keywords are ignored. Errors name the failing
/// JSON path.
pub fn validate(schema: &Value, instance: &Value) -> Result<(), String> {
    check(schema, instance, "$")
}

fn type_matches(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        _ => false,
    }
}

fn check(schema: &Value, value: &Value, at: &str) -> Result<(), String> {
    let Some(map) = schema.as_object() else {
        return Ok(());
    };
    if let Some(expected) = map.get("const")
        && expected != value
    {
        return Err(format!("{at}: expected {expected}, found {value}"));
    }
    if let Some(Value::Array(allowed)) = map.get("enum")
        && !allowed.contains(value)
    {
        return Err(format!("{at}: {value} is not an allowed value"));
    }
    if let Some(kind) = map.get("type") {
        let ok = match kind {
            Value::String(name) => type_matches(name, value),
            Value::Array(names) => names
                .iter()
                .filter_map(Value::as_str)
                .any(|n| type_matches(n, value)),
            _ => true,
        };
        if !ok {
            return Err(format!("{at}: {value} is not of type {kind}"));
        }
    }
    if map.get("pattern").and_then(Value::as_str) == Some(BASE64_PATTERN)
        && let Some(text) = value.as_str()
    {
        let bytes = text.as_bytes();
        let padding = bytes.iter().rev().take_while(|&&b| b == b'=').count();
        if bytes.len() % 4 != 0
            || padding > 2
            || !bytes[..bytes.len() - padding]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
        {
            return Err(format!("{at}: invalid base64"));
        }
    }
    if let Some(number) = value.as_f64() {
        if let Some(min) = map.get("minimum").and_then(Value::as_f64)
            && number < min
        {
            return Err(format!("{at}: {number} is below {min}"));
        }
        if let Some(max) = map.get("maximum").and_then(Value::as_f64)
            && number > max
        {
            return Err(format!("{at}: {number} is above {max}"));
        }
    }
    if let Some(object) = value.as_object() {
        let properties = map.get("properties").and_then(Value::as_object);
        if let Some(Value::Array(required)) = map.get("required") {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    return Err(format!("{at}: missing required member `{name}`"));
                }
            }
        }
        for (name, member) in object {
            match properties.and_then(|p| p.get(name)) {
                Some(member_schema) => check(member_schema, member, &format!("{at}.{name}"))?,
                None if map.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{at}: unexpected member `{name}`"));
                }
                None => {}
            }
        }
    }
    if let (Some(items), Some(array)) = (map.get("items"), value.as_array()) {
        for (index, item) in array.iter().enumerate() {
            check(items, item, &format!("{at}[{index}]"))?;
        }
    }
    if let Some(Value::Array(variants)) = map.get("oneOf") {
        let matching = variants
            .iter()
            .filter(|v| check(v, value, at).is_ok())
            .count();
        if matching != 1 {
            return Err(format!(
                "{at}: matches {matching} of {} oneOf variants",
                variants.len()
            ));
        }
    }
    if let Some(Value::Array(variants)) = map.get("anyOf")
        && !variants.iter().any(|v| check(v, value, at).is_ok())
    {
        return Err(format!("{at}: matches no anyOf variant"));
    }
    Ok(())
}
