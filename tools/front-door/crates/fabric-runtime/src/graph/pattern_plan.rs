//! Instantiate allow-listed planner patterns from typed parameters.
use super::*;
use serde_json::{Map, Value};

/// A fragment's request to instantiate one allow-listed pattern. Only the
/// instance identity, parameter values and local wiring are model-controlled;
/// the pattern's nodes, roles, sources, roots and access are fixed in the plan.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PatternInstance {
    pub id: String,
    pub pattern: String,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub inputs: Vec<GraphInput>,
    #[serde(default)]
    pub title: Option<String>,
}

/// Check a pattern definition before a graph is admitted. The fragment cannot
/// change access, role, source or read roots through a parameter, so those
/// fields are fixed text in the definition.
pub(super) fn validate(pattern: &PatternSpec) -> Result<(), &'static str> {
    if !valid_identifier(&pattern.name) || pattern.nodes.is_empty() || pattern.exports.is_empty() {
        return Err("plan pattern requires a name, nodes and exports");
    }
    let members: std::collections::BTreeSet<&str> = pattern
        .nodes
        .iter()
        .filter_map(|node| node.get("id").and_then(Value::as_str))
        .collect();
    if members.len() != pattern.nodes.len()
        || pattern.nodes.iter().any(|node| {
            !node
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(valid_identifier)
        })
        || pattern
            .exports
            .iter()
            .any(|export| !members.contains(export.node.as_str()) || export.outputs.is_empty())
    {
        return Err("plan pattern requires unique node IDs and a unique exported node");
    }
    let mut exported = std::collections::BTreeSet::new();
    if pattern
        .exports
        .iter()
        .any(|export| !exported.insert(&export.node))
    {
        return Err("plan pattern exports a node twice");
    }
    for (name, parameter) in &pattern.parameters {
        if !valid_identifier(name) {
            return Err("plan pattern parameter name");
        }
        if let Some(default) = &parameter.default {
            check_value(parameter, default).map_err(|_| "plan pattern parameter default")?;
        }
        if parameter
            .min
            .as_ref()
            .is_some_and(|value| !value.is_number())
            || parameter
                .max
                .as_ref()
                .is_some_and(|value| !value.is_number())
        {
            return Err("plan pattern parameter bound");
        }
        if let (Some(min), Some(max)) = (
            parameter.min.as_ref().and_then(Value::as_f64),
            parameter.max.as_ref().and_then(Value::as_f64),
        ) && min > max
        {
            return Err("plan pattern parameter bound order");
        }
        if parameter
            .allowed
            .iter()
            .any(|value| !value.is_number() && !value.is_string())
        {
            return Err("plan pattern parameter enum");
        }
    }
    for node in &pattern.nodes {
        if matches!(
            node.get("kind").and_then(Value::as_str),
            Some("loop" | "plan" | "composite")
        ) {
            return Err("plan pattern node");
        }
        let mut sensitive = Vec::new();
        for key in [
            "kind",
            "access",
            "role",
            "source",
            "read_roots",
            "route",
            "routes",
        ] {
            if let Some(value) = node.get(key) {
                placeholders(value, &mut sensitive)?;
            }
        }
        if !sensitive.is_empty() {
            return Err("plan pattern cannot vary fixed authority fields by parameter");
        }
        let mut names = Vec::new();
        placeholders(node, &mut names)?;
        if names
            .iter()
            .any(|name| !pattern.parameters.contains_key(name))
        {
            return Err("plan pattern uses an undeclared parameter");
        }
    }
    // Bounds do not affect template types. Relax them only for synthetic
    // samples so a required parameter with a positive minimum is admissible.
    let mut sample_pattern = pattern.clone();
    let samples = sample_pattern
        .parameters
        .iter_mut()
        .map(|(name, parameter)| {
            let value = parameter
                .default
                .clone()
                .unwrap_or_else(|| match parameter.kind {
                    PatternParameterKind::String => Value::String("sample".into()),
                    PatternParameterKind::Integer | PatternParameterKind::Number => {
                        serde_json::json!(0)
                    }
                    PatternParameterKind::Boolean => serde_json::json!(false),
                    PatternParameterKind::List => serde_json::json!([]),
                    PatternParameterKind::Table => serde_json::json!({}),
                });
            if parameter.default.is_none() {
                parameter.min = None;
                parameter.max = None;
                parameter.allowed.clear();
            }
            (name.clone(), value)
        })
        .collect();
    instantiate(
        &sample_pattern,
        &PatternInstance {
            id: "sample".into(),
            pattern: pattern.name.clone(),
            parameters: Value::Object(samples),
            depends_on: Vec::new(),
            inputs: Vec::new(),
            title: None,
        },
    )
    .map_err(|_| "plan pattern node template")?;
    Ok(())
}

fn placeholders(value: &Value, names: &mut Vec<String>) -> Result<(), &'static str> {
    match value {
        Value::String(text) => {
            let mut remaining = text.as_str();
            while let Some((_, rest)) = remaining.split_once("${") {
                let (name, after) = rest.split_once('}').ok_or("plan pattern placeholder")?;
                names.push(name.to_owned());
                remaining = after;
            }
        }
        Value::Array(values) => {
            for value in values {
                placeholders(value, names)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                placeholders(value, names)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_value(parameter: &PatternParameter, value: &Value) -> Result<(), String> {
    let fits = match parameter.kind {
        PatternParameterKind::String => value.is_string(),
        PatternParameterKind::Integer => value.is_i64() || value.is_u64(),
        PatternParameterKind::Number => value.is_number(),
        PatternParameterKind::Boolean => value.is_boolean(),
        PatternParameterKind::List => value.is_array(),
        PatternParameterKind::Table => value.is_object(),
    };
    if !fits {
        return Err(format!("must be {}", parameter.kind.name()));
    }
    let size = match value {
        Value::Number(number) => number.as_f64(),
        Value::Array(list) => Some(list.len() as f64),
        Value::String(text) => Some(text.chars().count() as f64),
        _ => None,
    };
    let (min, max) = (
        parameter.min.as_ref().and_then(Value::as_f64),
        parameter.max.as_ref().and_then(Value::as_f64),
    );
    if let Some(size) = size {
        let bound = |limit: f64| limit.to_string();
        match (min, max) {
            (Some(min), Some(max)) if size < min || size > max => {
                return Err(format!("must be {} to {}", bound(min), bound(max)));
            }
            (Some(min), None) if size < min => {
                return Err(format!("must be at least {}", bound(min)));
            }
            (None, Some(max)) if size > max => {
                return Err(format!("must be at most {}", bound(max)));
            }
            _ => {}
        }
    }
    if !parameter.allowed.is_empty() && !parameter.allowed.contains(value) {
        return Err(format!(
            "must be one of {}, received {}",
            Value::Array(parameter.allowed.clone()),
            value
        ));
    }
    Ok(())
}

fn resolve(pattern: &PatternSpec, supplied: &Value) -> Result<Map<String, Value>, String> {
    let supplied = match supplied {
        Value::Null => Map::new(),
        Value::Object(map) => map.clone(),
        _ => {
            return Err(format!(
                "pattern {} parameters must be a table",
                pattern.name
            ));
        }
    };
    if let Some(name) = supplied
        .keys()
        .find(|name| !pattern.parameters.contains_key(name.as_str()))
    {
        return Err(format!("pattern {} has no parameter {name}", pattern.name));
    }
    let mut effective = Map::new();
    for (name, parameter) in &pattern.parameters {
        let value = supplied
            .get(name)
            .or(parameter.default.as_ref())
            .ok_or_else(|| format!("pattern {} requires parameter {name}", pattern.name))?;
        check_value(parameter, value)
            .map_err(|reason| format!("pattern {} parameter {name}: {reason}", pattern.name))?;
        effective.insert(name.clone(), value.clone());
    }
    Ok(effective)
}

fn interpolate(value: &Value, parameters: &Map<String, Value>) -> Result<Value, String> {
    match value {
        Value::String(text) => {
            if let Some(name) = text
                .strip_prefix("${")
                .and_then(|rest| rest.strip_suffix('}'))
                && !name.contains(['{', '}'])
            {
                return parameters
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("unknown pattern parameter: {name}"));
            }
            let mut remaining = text.as_str();
            let mut rendered = String::new();
            while let Some((before, rest)) = remaining.split_once("${") {
                rendered.push_str(before);
                let (name, after) = rest
                    .split_once('}')
                    .ok_or("unclosed pattern parameter interpolation")?;
                match parameters
                    .get(name)
                    .ok_or_else(|| format!("unknown pattern parameter: {name}"))?
                {
                    Value::String(text) => rendered.push_str(text),
                    value @ (Value::Number(_) | Value::Bool(_)) => {
                        rendered.push_str(&value.to_string());
                    }
                    _ => {
                        return Err(format!(
                            "pattern parameter {name} must be scalar when embedded in text"
                        ));
                    }
                }
                remaining = after;
            }
            rendered.push_str(remaining);
            Ok(Value::String(rendered))
        }
        Value::Array(values) => values
            .iter()
            .map(|value| interpolate(value, parameters))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), interpolate(value, parameters)?)))
            .collect::<Result<Map<_, _>, String>>()
            .map(Value::Object),
        value => Ok(value.clone()),
    }
}

/// Build the composite wrapper that the scheduler expands exactly as a
/// statically written composite with the same template and parameters.
pub(super) fn instantiate(
    pattern: &PatternSpec,
    instance: &PatternInstance,
) -> Result<(GraphNode, PatternInstantiation), String> {
    let parameters = resolve(pattern, &instance.parameters)?;
    let mut nodes = Vec::with_capacity(pattern.nodes.len());
    for template in &pattern.nodes {
        let id = template
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let value = interpolate(template, &parameters)?;
        let node: GraphNode = serde_json::from_value(value)
            .map_err(|error| format!("pattern {} node {id}: {error}", pattern.name))?;
        nodes.push(node);
    }
    let wrapper: GraphNode = serde_json::from_value(serde_json::json!({
        "id": instance.id,
        "kind": "composite",
        "access": "read_only",
        "title": instance.title,
        "depends_on": instance.depends_on,
        "inputs": instance.inputs,
        "composite": {"nodes": nodes, "exports": pattern.exports},
    }))
    .map_err(|error| format!("pattern {} instance {}: {error}", pattern.name, instance.id))?;
    Ok((
        wrapper,
        PatternInstantiation {
            extra: Default::default(),
            id: instance.id.clone(),
            name: pattern.name.clone(),
            parameters: Value::Object(parameters),
            nodes: Vec::new(),
        },
    ))
}
