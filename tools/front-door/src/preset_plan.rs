//! The planner front-end: whether a preset run puts a principal planner ahead
//! of its graph, and how the preset's pattern catalogue becomes that node.
//!
//! A preset opts in with a `[plan]` table. `replaces` names the top-level stage
//! the planner shapes in place of the static one; `explicit` lists parameters
//! whose explicit setting means the chair already shaped the graph. The
//! `[[plan.patterns]]` entries are lowered through the ordinary role path, so a
//! planned lane runs the same role as a static one. The runtime owns the
//! bounded expansion; nothing here widens what it allows.
use super::{Layers, Value, flatten_parameters, interpolate, json, lower_leaf, stage_id};
use provenant_fabric_runtime::config::{PlanAuto, PlanMode};
use std::collections::{BTreeMap, BTreeSet};

/// Host inputs for the planner decision, supplied by `provenant run`.
#[derive(Clone, Debug)]
pub struct PlanContext {
    pub mode: PlanMode,
    pub route: String,
    pub lane_deadline_seconds: u64,
    pub max_nodes: usize,
    pub auto: PlanAuto,
    /// Why the planner route cannot run now, when the host knows.
    pub unavailable: Option<String>,
    /// Paths the caller declared for the work, counted with the brief's.
    pub paths: Vec<String>,
}

/// What the front-end decided, and why. Recorded in the graph's provenance.
#[derive(Clone, Debug)]
pub struct Decision {
    pub requested: PlanMode,
    pub applied: bool,
    pub reason: String,
    pub route: String,
    /// Set when planning was wanted but the preset runs on its defaults.
    pub degraded: Option<String>,
}

impl Decision {
    pub fn line(&self) -> String {
        format!(
            "plan: {} -> {} ({})",
            self.requested.name(),
            if self.applied { "on" } else { "off" },
            self.reason
        )
    }

    pub fn to_json(&self) -> Value {
        json!({
            "requested": self.requested.name(),
            "applied": if self.applied { "on" } else { "off" },
            "reason": self.reason,
            "route": self.route,
            "degraded": self.degraded,
        })
    }
}

/// `- item`, `* item`, `+ item` and `1. item` or `1) item` lines.
fn list_item(line: &str) -> bool {
    let line = line.trim_start();
    if let Some(rest) = line.strip_prefix(['-', '*', '+']) {
        return rest.starts_with(' ');
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0
        && line[digits..]
            .strip_prefix(['.', ')'])
            .is_some_and(|rest| rest.starts_with(' '))
}

/// List items under an "Acceptance" heading or label, up to the next heading.
pub fn acceptance_items(text: &str) -> usize {
    let mut inside = false;
    let mut count = 0;
    for line in text.lines() {
        let label = line.trim().trim_start_matches('#').trim_matches(['*', ':', ' ']);
        let heading = line.trim_start().starts_with('#');
        if label.to_ascii_lowercase().starts_with("acceptance") && !list_item(line) {
            inside = true;
        } else if heading {
            inside = false;
        } else if inside && list_item(line) {
            count += 1;
        }
    }
    count
}

/// Distinct issue numbers (`#123`) the text names.
pub fn issues(text: &str) -> usize {
    let mut found = BTreeSet::new();
    let bytes = text.as_bytes();
    for (at, byte) in bytes.iter().enumerate() {
        if *byte != b'#' || at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'&')
        {
            continue;
        }
        let digits: String = text[at + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            found.insert(digits);
        }
    }
    found.len()
}

/// Crates a set of paths touches: the directory after `crates/`, or the first
/// path segment when a path is outside a `crates` directory.
fn crates_of<'a>(paths: impl Iterator<Item = &'a str>, nested_only: bool) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for path in paths {
        let path = path.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-');
        let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty() && *part != ".").collect();
        match segments.iter().position(|part| *part == "crates") {
            Some(at) if at + 1 < segments.len() => {
                found.insert(segments[at + 1].to_owned());
            }
            _ if !nested_only && segments.len() > 1 => {
                found.insert(segments[0].to_owned());
            }
            _ => {}
        }
    }
    found
}

/// The crates the subject names: `crates/NAME` mentions in the brief, plus the
/// declared paths and the write scopes of any explicit lanes.
pub fn write_crates(brief: &str, ctx: &PlanContext, parameters: &Value) -> usize {
    let mut found = crates_of(brief.split_whitespace(), true);
    found.extend(crates_of(ctx.paths.iter().map(String::as_str), false));
    let scopes: Vec<&str> = parameters
        .get("lanes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|lane| lane.get("scope").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    found.extend(crates_of(scopes.into_iter(), false));
    found.len()
}

fn explicit(origins: &BTreeMap<String, String>, parameter: &str) -> bool {
    let path = format!("parameters.{parameter}");
    origins
        .get(&path)
        .or_else(|| {
            origins
                .iter()
                .find(|(origin, _)| origin.starts_with(&format!("{path}.")))
                .map(|(_, origin)| origin)
        })
        .is_some_and(|origin| matches!(origin.as_str(), "--set" | "--params-file"))
}

/// The `auto` rule. Explicit shape wins; then any large or underspecified
/// signal turns planning on; otherwise the work is small and specified.
fn auto_rule(
    ctx: &PlanContext,
    catalogue: &Value,
    origins: &BTreeMap<String, String>,
    brief: &str,
    parameters: &Value,
) -> (bool, String) {
    let shaped: Vec<&str> = catalogue
        .get("explicit")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if let Some(parameter) = shaped.iter().find(|name| explicit(origins, name)) {
        return (false, format!("{parameter} given explicitly"));
    }
    let items = acceptance_items(brief);
    if items > ctx.auto.acceptance_items {
        return (
            true,
            format!(
                "{items} acceptance items, more than {}",
                ctx.auto.acceptance_items
            ),
        );
    }
    let crates = write_crates(brief, ctx, parameters);
    if crates > ctx.auto.write_crates {
        return (
            true,
            format!("write scope spans {crates} crates, more than {}", ctx.auto.write_crates),
        );
    }
    let named = issues(brief);
    if named >= ctx.auto.batch_issues {
        return (true, format!("{named} issues in one batch"));
    }
    if let Some(parameter) = shaped.first() {
        return (true, format!("{parameter} not set"));
    }
    (
        false,
        format!("small, fully specified work ({items} acceptance items, {crates} crates)"),
    )
}

/// Decide whether this run plans. `None` means the preset declares no
/// catalogue and the run is unchanged.
pub fn decide(
    ctx: &PlanContext,
    name: &str,
    config: &Value,
    origins: &BTreeMap<String, String>,
    brief: &str,
) -> Result<Option<Decision>, String> {
    let Some(catalogue) = config.get("plan") else {
        return if ctx.mode == PlanMode::On {
            Err(format!(
                "preset {name} declares no planner patterns; run it with --plan auto or --plan off"
            ))
        } else {
            Ok(None)
        };
    };
    let parameters = flatten_parameters(
        name,
        config.get("parameters").unwrap_or(&json!({})),
        "",
        origins,
    )?;
    let (wanted, reason) = match ctx.mode {
        PlanMode::Off => (false, "planning turned off".to_owned()),
        PlanMode::On => (true, "planning requested".to_owned()),
        PlanMode::Auto => auto_rule(ctx, catalogue, origins, brief, &parameters),
    };
    let mut decision = Decision {
        requested: ctx.mode,
        applied: wanted,
        reason,
        route: ctx.route.clone(),
        degraded: None,
    };
    // Never substitute a weaker planner: without the configured route the
    // preset runs on its deterministic defaults and says why.
    if wanted && let Some(why) = &ctx.unavailable {
        decision.applied = false;
        decision.degraded = Some(why.clone());
        decision.reason = format!(
            "{} route unavailable ({why}); running the preset's deterministic defaults. Wanted: {}",
            ctx.route, decision.reason
        );
    }
    Ok(Some(decision))
}

/// Swap the shaped stage for the planner node and lower the catalogue it
/// instantiates. Returns the plan specification to attach once the graph is
/// built, because pattern templates keep their own `${...}` placeholders.
pub fn apply(
    layers: &Layers,
    ctx: &PlanContext,
    name: &str,
    config: &mut Value,
    origins: &BTreeMap<String, String>,
    brief: &str,
) -> Result<Value, String> {
    let catalogue = config.get("plan").cloned().ok_or("missing plan table")?;
    let replaces = catalogue
        .get("replaces")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("preset {name}: plan.replaces must name a stage"))?
        .to_owned();
    let worktrees: Vec<Value> = config
        .pointer("/parameters/worktrees")
        .map(|value| value.get("default").unwrap_or(value))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let listed = |values: &[Value]| {
        values
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let routes = layers
        .known_routes
        .as_ref()
        .map(|routes| routes.iter().cloned().collect::<Vec<_>>().join(", "))
        .unwrap_or_else(|| "junior, senior, principal".into());
    let guidance = catalogue
        .get("guidance")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let parameters = config
        .get_mut("parameters")
        .and_then(Value::as_object_mut)
        .ok_or("preset parameters must be a table")?;
    for (key, value) in [
        ("plan_lane_deadline_seconds", json!(ctx.lane_deadline_seconds)),
        ("plan_max_nodes", json!(ctx.max_nodes)),
        ("plan_routes", json!(routes)),
        ("plan_worktrees", json!(if worktrees.is_empty() { "none registered".into() } else { listed(&worktrees) })),
        ("plan_guidance", json!(guidance)),
    ] {
        parameters.insert(key.into(), value);
    }
    let stages = config
        .get_mut("stages")
        .and_then(Value::as_array_mut)
        .ok_or("preset stages must be a list")?;
    let position = stages
        .iter()
        .enumerate()
        .position(|(at, stage)| stage_id(stage, at) == replaces)
        .ok_or_else(|| format!("preset {name}: plan.replaces names no top-level stage: {replaces}"))?;
    stages[position] = json!({
        "id": "plan",
        "role": "plan.shape",
        "tier": ctx.route,
        "access": "read_only",
        "plan": {"kinds": ["task", "synthesise"], "max_nodes": ctx.max_nodes, "patterns": []},
    });
    let plan_values = flatten_parameters(
        name,
        config.get("parameters").unwrap_or(&json!({})),
        "",
        origins,
    )?;
    let patterns = catalogue
        .get("patterns")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("preset {name}: plan.patterns must list patterns"))?;
    let lowered = patterns
        .iter()
        .map(|pattern| lower_pattern(layers, config.get("roles"), &plan_values, brief, pattern))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "kinds": ["task", "synthesise"],
        "max_nodes": ctx.max_nodes,
        "patterns": lowered,
        "patterns_only": true,
        "disjoint_waves": true,
    }))
}

/// One catalogue entry as a runtime `PatternSpec`. Pattern parameters stay as
/// `${name}` placeholders: each is bound to its own placeholder text while the
/// role lowers, so role prompts keep them for the runtime to fill.
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
    slots.entry("subject").or_insert_with(|| json!({"type": "commit", "commit": "HEAD"}));
    for parameter in declared.keys() {
        slots.insert(parameter.clone(), json!(format!("${{{parameter}}}")));
    }
    let parameters = interpolate(&Value::Object(declared.clone()), values, brief, 1)
        .map_err(|error| format!("plan pattern {name}: {error}"))?;
    // The brief is text for the planner's lanes, never a template.
    let brief = brief.replace("${", "$ {");
    let mut nodes = Vec::new();
    for entry in pattern
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("plan pattern {name} needs nodes"))?
    {
        let mut entry = entry.as_object().cloned().ok_or("pattern nodes must be tables")?;
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or("pattern nodes need an id")?
            .to_owned();
        // The runtime appends the write scope and acceptance checks itself.
        let mut kept = Vec::new();
        for key in ["write_scope", "acceptance_checks"] {
            if let Some(value) = entry.insert(key.into(), json!([])) {
                kept.push((key, value));
            }
        }
        let task = match pattern.get("brief").and_then(Value::as_str) {
            Some(suffix) => format!("{brief}\n\n{suffix}"),
            None => brief.clone(),
        };
        let mut node = lower_leaf(layers, &entry, roles, &bound, &task, 1, &id, &[])?;
        for (key, value) in kept {
            node[key] = interpolate(&value, &bound, &task, 1)?;
        }
        if node.get("acceptance_checks").is_none_or(|checks| checks == &json!([])) {
            node.as_object_mut().map(|map| map.remove("acceptance_checks"));
        }
        if node.get("write_scope").is_some_and(|scope| scope == &json!([])) {
            node.as_object_mut().map(|map| map.remove("write_scope"));
        }
        node.as_object_mut().map(|map| map.remove("depends_on").filter(|deps| deps != &json!([])).map(|deps| map.insert("depends_on".into(), deps)));
        nodes.push(node);
    }
    Ok(json!({
        "name": name,
        "parameters": parameters,
        "nodes": nodes,
        "exports": pattern.get("exports").cloned().ok_or_else(|| format!("plan pattern {name} needs exports"))?,
    }))
}

/// Attach the lowered catalogue to the built planner node.
pub fn attach(nodes: &mut [Value], plan: Value) -> Result<(), String> {
    let node = nodes
        .iter_mut()
        .find(|node| node["id"] == "plan")
        .ok_or("the planner node was not built")?;
    node["plan"] = plan;
    Ok(())
}
