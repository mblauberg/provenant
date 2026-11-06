//! Deferred review planner support retained for the review panel.
use super::{Layers, StageBuilder, Value, interpolate, json, lower_leaf, spec_context};
use provenant_fabric_runtime::config::PlanMode;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct PlanContext {
    pub mode: PlanMode,
    pub route: String,
    pub lane_deadline_seconds: u64,
    pub max_nodes: usize,
    pub unavailable: Option<String>,
}

/// Replace `${item.name}` and `${item.tier}` through a catalogue entry.
fn bind_item(value: &Value, item: &Value) -> Value {
    match value {
        Value::String(text) => {
            let mut text = text.clone();
            for key in ["name", "tier"] {
                if let Some(replacement) = item.get(key).and_then(Value::as_str) {
                    text = text.replace(&format!("${{item.{key}}}"), replacement);
                }
            }
            Value::String(text)
        }
        Value::Array(values) => Value::Array(values.iter().map(|v| bind_item(v, item)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, v)| (key.clone(), bind_item(v, item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Lower the review catalogue before its subject is known. The launch host
/// narrows it to the measured panel and binds its leaves to the pinned commit.
pub(super) fn deferred_review_catalogue(
    builder: &StageBuilder<'_>,
    names: &[String],
) -> Result<Value, String> {
    let (config, _) = builder.layers.resolve(builder.name)?;
    let catalogue = config
        .get("plan")
        .ok_or("review preset needs a plan catalogue")?;
    let patterns = catalogue
        .get("patterns")
        .and_then(Value::as_array)
        .ok_or("review plan needs patterns")?;
    let mut values = builder.parameters.clone();
    values["subject"] = json!({"type":"commit", "commit":"HEAD"});
    let mut lowered = Vec::new();
    for pattern in patterns {
        if pattern.get("for_each").and_then(Value::as_str) != Some("lenses") {
            return Err("deferred review patterns must iterate lenses".into());
        }
        for name in names {
            let item = json!({"name": name, "tier": "senior"});
            let mut entry = bind_item(pattern, &item);
            let map = entry
                .as_object_mut()
                .ok_or("review pattern must be a table")?;
            map.remove("for_each");
            for node in map
                .get_mut("nodes")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                node.as_object_mut()
                    .ok_or("review pattern node must be a table")?
                    .entry("item")
                    .or_insert_with(|| item.clone());
            }
            lowered.push(lower_pattern(
                builder.layers,
                builder.roles,
                &values,
                builder.prompt,
                &entry,
            )?);
        }
    }
    for name in names {
        let expected = format!("lens-{name}");
        if !lowered.iter().any(|pattern| pattern["name"] == expected) {
            return Err(format!("review catalogue lacks {expected}"));
        }
    }
    Ok(json!({"patterns":lowered,"guidance":catalogue.get("guidance")}))
}

fn lower_pattern(
    layers: &Layers,
    roles: Option<&Value>,
    values: &Value,
    brief: &str,
    pattern: &Value,
) -> Result<Value, String> {
    let name = pattern
        .get("name")
        .and_then(Value::as_str)
        .ok_or("plan pattern needs a name")?;
    let declared = pattern
        .get("parameters")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("plan pattern {name} needs parameters"))?;
    let mut bound = values.clone();
    let slots = bound.as_object_mut().ok_or("parameters must be a table")?;
    slots
        .entry("subject")
        .or_insert_with(|| json!({"type": "commit", "commit": "HEAD"}));
    for parameter in declared.keys() {
        slots.insert(parameter.clone(), json!(format!("${{{parameter}}}")));
    }
    // lower_leaf must not resolve the runtime worktree placeholder again.
    slots.remove("spec_max_bytes");
    let parameters = interpolate(&Value::Object(declared.clone()), values, brief, 1)
        .map_err(|error| format!("plan pattern {name}: {error}"))?;
    // Pattern worktrees are runtime choices. Resolve one shared spec from the
    // bound batch worktree (or the preset's ordinary worktree), never a lane.
    let mut spec_values = values.clone();
    if let Some(worktree) = values.get("batch_worktree") {
        spec_values["worktree"] = worktree.clone();
    }
    let context = spec_context(layers, &spec_values);
    // The brief is text for the planner's lanes, never a template.
    let brief = brief.replace("${", "$ {");
    let mut nodes = Vec::new();
    for entry in pattern
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("plan pattern {name} needs nodes"))?
    {
        let mut entry = entry
            .as_object()
            .cloned()
            .ok_or("pattern nodes must be tables")?;
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or("pattern nodes need an id")?
            .to_owned();
        if entry.contains_key("preset") {
            let mut parameters = bound.clone();
            if let Some(limit) = values.get("spec_max_bytes") {
                parameters["spec_max_bytes"] = limit.clone();
            }
            // Pattern worktrees are not available until launch. Bind the shared
            // batch spec once, as for the other catalogue leaves.
            let key = json!([
                parameters.get("spec"),
                parameters.get("worktree"),
                parameters.get("spec_max_bytes")
            ])
            .to_string();
            layers
                .spec_contexts
                .borrow_mut()
                .insert(key, context.as_ref().map(|text| text.replace("${", "$ {")));
            let (mut stack, mut origins, mut repo_files) =
                (Vec::new(), BTreeMap::new(), Vec::new());
            let mut builder = StageBuilder {
                layers,
                name,
                prompt: &brief,
                parameters,
                roles,
                stack: &mut stack,
                origins: &mut origins,
                repo_files: &mut repo_files,
                scope: "",
            };
            builder.entry(&Value::Object(entry), &id, &[], &mut nodes, 0)?;
            continue;
        }
        // The runtime appends the write scope and acceptance checks itself.
        let mut kept = Vec::new();
        for key in ["write_scope", "acceptance_checks"] {
            if let Some(value) = entry.insert(key.into(), json!([])) {
                kept.push((key, value));
            }
        }
        // A node's own brief, then the pattern's, follows the subject text.
        let suffix = entry
            .remove("brief")
            .and_then(|text| text.as_str().map(str::to_owned))
            .or_else(|| {
                pattern
                    .get("brief")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        let task = match suffix {
            Some(suffix) => format!("{brief}\n\n{suffix}"),
            None => brief.clone(),
        };
        // A lens leg names its lens the way a `for_each` item would.
        let mut values = bound.clone();
        if let Some(item) = entry.remove("item") {
            values["item"] = item;
        }
        let mut node = lower_leaf(layers, &entry, roles, &values, &task, 1, &id, &[])?;
        if node["role"].as_str().is_some_and(|role| {
            role.starts_with("lens.") || role.starts_with("review.") || role == "plan.review"
        }) && let Some(context) = &context
        {
            // Like the brief, file contents are text, never pattern parameters.
            let context = context.replace("${", "$ {");
            let prompt = node["prompt"].as_str().unwrap_or_default();
            node["prompt"] = json!(format!("{prompt}{context}"));
        }
        for (key, value) in kept {
            node[key] = interpolate(&value, &bound, &task, 1)?;
        }
        if node
            .get("acceptance_checks")
            .is_none_or(|checks| checks == &json!([]))
        {
            node.as_object_mut()
                .map(|map| map.remove("acceptance_checks"));
        }
        if node
            .get("write_scope")
            .is_some_and(|scope| scope == &json!([]))
        {
            node.as_object_mut().map(|map| map.remove("write_scope"));
        }
        node.as_object_mut().map(|map| {
            map.remove("depends_on")
                .filter(|deps| deps != &json!([]))
                .map(|deps| map.insert("depends_on".into(), deps))
        });
        nodes.push(node);
    }
    Ok(json!({
        "name": name,
        "parameters": parameters,
        "nodes": nodes,
        "exports": pattern.get("exports").cloned().ok_or_else(|| format!("plan pattern {name} needs exports"))?,
    }))
}
