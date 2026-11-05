//! The planner front-end: whether a preset run puts a principal planner ahead
//! of its graph, and how the preset's pattern catalogue becomes that node.
//!
//! A preset opts in with a `[plan]` table. `replaces` names the top-level stage
//! the planner shapes in place of the static one; `explicit` lists parameters
//! whose explicit setting means the chair already shaped the graph. The
//! `[[plan.patterns]]` entries are lowered through the ordinary role path, so a
//! planned lane runs the same role as a static one. The runtime owns the
//! bounded expansion; nothing here widens what it allows.
//!
//! `after` lists stages inserted behind the planner, for a reconciliation the
//! static stage ran inside itself.
//!
//! A catalogue entry with `enabled_by = "param"` is offered only while that
//! parameter is non-zero. A catalogue entry with `for_each = "lenses"` stands for one pattern per lens
//! the routing review panel selects for the subject, so a planner can only
//! choose lenses that panel allows, at a tier it may lower or raise but never
//! at a lens the panel left out.
use super::{
    Layers, StageBuilder, Value, flatten_parameters, interpolate, json, lower_leaf, spec_context,
    stage_id,
};
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
    /// True when the mode came from an explicit `--plan` flag, not config.
    pub from_cli: bool,
    /// Paths the caller declared for the work, counted with the brief's.
    pub paths: Vec<String>,
    /// The project's linked worktrees: the default a planner may assign.
    pub worktrees: Vec<String>,
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
    /// Warnings the decision raises for the chair.
    pub warnings: Vec<String>,
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

/// A list item is top level when it is no more indented than the first one
/// under the heading; deeper items are sub-bullets of the one above.
fn top_level(line: &str, margin: &mut Option<usize>) -> bool {
    let indent = line
        .chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum::<usize>();
    let first = *margin.get_or_insert(indent);
    indent <= first
}

/// List items under an "Acceptance" heading or label, up to the next heading.
pub fn acceptance_items(text: &str) -> usize {
    let mut inside = false;
    let mut count = 0;
    let mut margin = None;
    for line in text.lines() {
        let label = line
            .trim()
            .trim_start_matches('#')
            .trim_matches(['*', ':', ' ']);
        let heading = line.trim_start().starts_with('#');
        if label.to_ascii_lowercase().starts_with("acceptance") && !list_item(line) {
            inside = true;
            margin = None;
        } else if heading {
            inside = false;
        } else if inside && list_item(line) && top_level(line, &mut margin) {
            count += 1;
        }
    }
    count
}

/// Distinct issue numbers (`#123`) the text names. A number is decimal, at
/// most six digits and stands alone: a hex colour such as `#1a2b3c`, an
/// identifier such as `#12ab` or a run of more than six digits is not one.
pub fn issues(text: &str) -> usize {
    let mut found = BTreeSet::new();
    let bytes = text.as_bytes();
    for (at, byte) in bytes.iter().enumerate() {
        if *byte != b'#'
            || at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'&')
        {
            continue;
        }
        let run = text[at + 1..]
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>();
        let digits = run.chars().take_while(char::is_ascii_digit).count();
        // Digits followed by letters are a hex colour or an identifier.
        if digits == 0 || digits != run.len() || digits > 6 {
            continue;
        }
        found.insert(run);
    }
    found.len()
}

/// Crates a set of paths touches: the directory after `crates/`, or the first
/// path segment when a path is outside a `crates` directory.
fn crates_of<'a>(paths: impl Iterator<Item = &'a str>, nested_only: bool) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for path in paths {
        let path = path.trim_matches(|c: char| {
            !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-'
        });
        let segments: Vec<&str> = path
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
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

fn explicit(
    layers: &Layers,
    name: &str,
    origins: &BTreeMap<String, String>,
    parameter: &str,
) -> bool {
    let path = format!("parameters.{parameter}");
    origins
        .get(&path)
        .or_else(|| {
            origins
                .iter()
                .find(|(origin, _)| origin.starts_with(&format!("{path}.")))
                .map(|(_, origin)| origin)
        })
        .is_some_and(|origin| !layers.is_initial_origin(name, parameter, origin))
}

/// The `auto` rule. Explicit shape wins; then any large or underspecified
/// signal turns planning on; otherwise the work is small and specified.
#[allow(
    clippy::too_many_arguments,
    reason = "planner selection needs the preset's layered inputs"
)]
fn auto_rule(
    layers: &Layers,
    name: &str,
    ctx: &PlanContext,
    catalogue: &Value,
    origins: &BTreeMap<String, String>,
    brief: &str,
    parameters: &Value,
    size: Option<&str>,
) -> (bool, String) {
    let shaped: Vec<&str> = catalogue
        .get("explicit")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if let Some(parameter) = shaped
        .iter()
        .find(|parameter| explicit(layers, name, origins, parameter))
    {
        return (false, format!("{parameter} given explicitly"));
    }
    // A review panel is scaled by the subject's size: only a large one plans,
    // however long the brief.
    if catalogue.get("by_size").and_then(Value::as_bool) == Some(true) {
        return match size {
            Some("large") => (true, "review subject is large".into()),
            other => (
                false,
                format!(
                    "review subject is not large (size={})",
                    other.unwrap_or("unknown")
                ),
            ),
        };
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
            format!(
                "write scope spans {crates} crates, more than {}",
                ctx.auto.write_crates
            ),
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
    layers: &Layers,
    ctx: &PlanContext,
    name: &str,
    config: &Value,
    origins: &BTreeMap<String, String>,
    brief: &str,
) -> Result<Option<Decision>, String> {
    let Some(catalogue) = config.get("plan") else {
        return match ctx.mode {
            PlanMode::On if ctx.from_cli => Err(format!(
                "preset {name} declares no planner patterns; run it with --plan auto or --plan off"
            )),
            PlanMode::On => Ok(Some(Decision {
                requested: ctx.mode,
                applied: false,
                reason: "preset declares no planner patterns".into(),
                route: ctx.route.clone(),
                degraded: None,
                warnings: Vec::new(),
            })),
            _ => Ok(None),
        };
    };
    if let Some(stage) = catalogue.get("delegate").and_then(Value::as_str) {
        if !config
            .get("stages")
            .and_then(Value::as_array)
            .is_some_and(|stages| {
                stages.iter().enumerate().any(|(index, entry)| {
                    stage_id(entry, index) == stage && entry.get("preset").is_some()
                })
            })
        {
            return Err(format!(
                "preset {name}: plan.delegate must name a preset stage"
            ));
        }
        let wanted = ctx.mode == PlanMode::On;
        let applied = wanted && ctx.unavailable.is_none();
        let reason = if applied {
            "review panel planned at launch".to_owned()
        } else if wanted {
            format!(
                "planner unavailable; review panel uses deterministic selection: {}",
                ctx.unavailable.as_deref().unwrap_or_default()
            )
        } else if ctx.mode == PlanMode::Auto {
            "review panel uses deterministic selection in auto mode".to_owned()
        } else {
            "planning turned off".to_owned()
        };
        return Ok(Some(Decision {
            requested: ctx.mode,
            applied,
            reason,
            route: ctx.route.clone(),
            degraded: ctx.unavailable.clone().filter(|_| wanted),
            warnings: Vec::new(),
        }));
    }
    let parameters = flatten_parameters(
        name,
        config.get("parameters").unwrap_or(&json!({})),
        "",
        origins,
    )?;
    let (wanted, reason) = match ctx.mode {
        PlanMode::Off => (false, "planning turned off".to_owned()),
        PlanMode::On => (true, "planning requested".to_owned()),
        PlanMode::Auto => {
            let size = if catalogue.get("by_size").and_then(Value::as_bool) == Some(true) {
                with_builder(layers, name, config, origins, brief, |builder| {
                    Ok(builder
                        .shape_hint(builder.defers_review_size())?
                        .and_then(|hint| hint.size))
                })?
            } else {
                None
            };
            auto_rule(
                layers,
                name,
                ctx,
                catalogue,
                origins,
                brief,
                &parameters,
                size.as_deref(),
            )
        }
    };
    let mut decision = Decision {
        requested: ctx.mode,
        applied: wanted,
        reason,
        route: ctx.route.clone(),
        degraded: None,
        warnings: Vec::new(),
    };
    // An extending preset can inherit `[plan]` without the stage it shapes.
    let replaces = catalogue.get("replaces").and_then(Value::as_str);
    let missing = replaces.filter(|replaces| {
        !config
            .get("stages")
            .and_then(Value::as_array)
            .is_some_and(|stages| {
                stages
                    .iter()
                    .enumerate()
                    .any(|(at, stage)| stage_id(stage, at) == *replaces)
            })
    });
    if wanted && let Some(replaces) = missing {
        decision.applied = false;
        let why = format!("plan.replaces names no top-level stage: {replaces}");
        decision.reason = format!(
            "{why}; running the preset's deterministic defaults. Wanted: {}",
            decision.reason
        );
        decision.degraded = Some(why);
        return Ok(Some(decision));
    }
    // Lens legs pin the subject themselves, which only a commit can do from
    // inside a planned fragment.
    let subject = parameters.pointer("/subject/type").and_then(Value::as_str);
    if wanted
        && let Some(allowed) = catalogue.get("subject_types").and_then(Value::as_array)
        && let Some(kind) = subject.filter(|kind| !allowed.iter().any(|a| a == kind))
    {
        decision.applied = false;
        let why = format!("a {kind} subject cannot be pinned from a planned fragment");
        decision.reason = format!(
            "{why}; running the preset's deterministic defaults. Wanted: {}",
            decision.reason
        );
        decision.degraded = Some(why);
        return Ok(Some(decision));
    }
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
    // `--plan on` is the chair's word, but explicit lanes are the chair's too:
    // say which of them the planner replaces.
    if decision.applied && ctx.mode == PlanMode::On {
        let given: Vec<&str> = catalogue
            .get("explicit")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|parameter| explicit(layers, name, origins, parameter))
            .collect();
        if !given.is_empty() {
            decision.warnings.push(format!(
                "--plan on discards the explicit {}; the planner shapes the work instead",
                given.join(", ")
            ));
        }
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
    // Unless the chair listed worktrees, the project's own linked ones are the
    // set a planner may assign.
    if !explicit(layers, name, origins, "worktrees")
        && !ctx.worktrees.is_empty()
        && let Some(slot) = config.pointer_mut("/parameters/worktrees")
    {
        let listed = json!(ctx.worktrees);
        match slot.get_mut("default") {
            Some(default) => *default = listed,
            None => *slot = listed,
        }
    }
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
        (
            "plan_lane_deadline_seconds",
            json!(ctx.lane_deadline_seconds),
        ),
        ("plan_max_nodes", json!(ctx.max_nodes)),
        ("plan_routes", json!(routes)),
        (
            "plan_worktrees",
            json!(if worktrees.is_empty() {
                "none registered".into()
            } else {
                listed(&worktrees)
            }),
        ),
        ("plan_guidance", json!(guidance)),
    ] {
        parameters.insert(key.into(), value);
    }
    let mut plan_values = flatten_parameters(
        name,
        config.get("parameters").unwrap_or(&json!({})),
        "",
        origins,
    )?;
    plan_values["plan_route_names"] = json!(
        layers
            .known_routes
            .as_ref()
            .map(|routes| routes.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    );
    // Catalogue leaves bypass build_stages, so bind the same policy fallback
    // before lowering their role prompts.
    plan_values["local_check_policy"] = json!(layers.local_check_policy(&plan_values, origins)?.1);
    let extra_roots = plan_values
        .get("read_roots")
        .filter(|roots| roots.as_array().is_some_and(|roots| !roots.is_empty()))
        .cloned();
    let (floor_lenses, floor_classes, floor_min_reviewers) = if catalogue
        .get("patterns")
        .and_then(Value::as_array)
        .is_some_and(|patterns| {
            patterns
                .iter()
                .any(|p| p.get("for_each").and_then(Value::as_str) == Some("lenses"))
        }) {
        with_builder(layers, name, config, origins, brief, |builder| {
            Ok(builder
                .shape_hint(false)?
                .map(|hint| {
                    (
                        hint.required_floor_lenses,
                        hint.floor_classes,
                        hint.floor_min_reviewers,
                    )
                })
                .unwrap_or_default())
        })?
    } else {
        (Vec::new(), Vec::new(), 0)
    };
    let stages = config
        .get_mut("stages")
        .and_then(Value::as_array_mut)
        .ok_or("preset stages must be a list")?;
    let position = stages
        .iter()
        .enumerate()
        .position(|(at, stage)| stage_id(stage, at) == replaces)
        .ok_or_else(|| {
            format!("preset {name}: plan.replaces names no top-level stage: {replaces}")
        })?;
    let mut fallback_stage = stages[position].clone();
    let reduced = fallback_stage
        .get("reduce_verdict")
        .and_then(Value::as_bool)
        == Some(true);
    if reduced {
        fallback_stage["outputs"] = json!(["result", "findings", "verdict", "commit"]);
    }
    // Planned workers and their deterministic fallback require all members.
    if (fallback_stage.get("count").is_some() || fallback_stage.get("for_each").is_some())
        && let Some(stage) = fallback_stage.as_object_mut()
    {
        stage.remove("min_successful");
    }
    let mut planner = json!({
        "id": "plan",
        "role": "plan.shape",
        "tier": ctx.route,
        "access": "read_only",
        "plan": {"kinds": ["task", "synthesise"], "max_nodes": ctx.max_nodes, "patterns": []},
        "fallback_stage": fallback_stage,
    });
    // A fragment cannot widen its planner's read roots, so a preset whose
    // planned scouts read the caller's extra roots gives the planner the same.
    if let Some(roots) = extra_roots {
        planner["read_roots"] = roots;
    }
    if reduced {
        planner["source"] = json!("${subject}");
        stages[position] = json!({"id":replaces, "parallel":[planner], "reduce_verdict":true,
            "min_successful":fallback_stage.get("min_successful").cloned().unwrap_or(json!(1)),
            "exports":[{"node":"plan","outputs":["result","findings","verdict","commit"]}]});
    } else {
        stages[position] = planner;
    }
    // Explicit references to the replaced stage must follow its planner too.
    for stage in stages.iter_mut().skip(position + 1) {
        if let Some(deps) = stage.get_mut("depends_on").and_then(Value::as_array_mut) {
            for dep in deps {
                if !reduced && dep.as_str() == Some(replaces.as_str()) {
                    *dep = json!("plan");
                }
            }
        }
        if let Some(inputs) = stage.get_mut("inputs").and_then(Value::as_array_mut) {
            for input in inputs {
                if !reduced && input.get("node").and_then(Value::as_str) == Some(replaces.as_str())
                {
                    input["node"] = json!("plan");
                }
            }
        }
    }
    // Stages the planned fragment needs after it, for example the synthesis
    // that reconciles the evidence the static stage reconciled in itself.
    if let Some(after) = catalogue.get("after") {
        let after = after
            .as_array()
            .ok_or_else(|| format!("preset {name}: plan.after must be a list of stages"))?;
        for (offset, stage) in after.iter().enumerate() {
            let id = stage
                .as_object()
                .and_then(|stage| stage.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| format!("preset {name}: plan.after stages need an id"))?;
            if stages
                .iter()
                .enumerate()
                .any(|(at, existing)| stage_id(existing, at) == id)
            {
                return Err(format!(
                    "preset {name}: plan.after stage {id} repeats a stage id"
                ));
            }
            stages.insert(position + 1 + offset, stage.clone());
        }
    }
    let patterns = catalogue
        .get("patterns")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("preset {name}: plan.patterns must list patterns"))?;
    let mut entries = Vec::new();
    for pattern in patterns {
        // A pattern gated by a parameter the chair turned off is not offered.
        if let Some(gate) = pattern.get("enabled_by") {
            let gate = gate.as_str().ok_or_else(|| {
                format!("preset {name}: plan pattern enabled_by must name a parameter")
            })?;
            let value = plan_values.get(gate).ok_or_else(|| {
                format!("preset {name}: plan pattern enabled_by names no parameter: {gate}")
            })?;
            let off = match value {
                Value::Null | Value::Bool(false) => true,
                Value::Number(number) => number.as_f64() == Some(0.0),
                Value::String(text) => text.is_empty(),
                Value::Array(items) => items.is_empty(),
                Value::Object(_) | Value::Bool(true) => false,
            };
            if off {
                continue;
            }
        }
        match pattern.get("for_each").and_then(Value::as_str) {
            Some(list) => entries.extend(each_item(
                layers,
                name,
                config,
                origins,
                brief,
                &plan_values,
                pattern,
                list,
            )?),
            None => entries.push(pattern.clone()),
        }
    }
    let lowered = entries
        .iter()
        .map(|pattern| lower_pattern(layers, config.get("roles"), &plan_values, brief, pattern))
        .collect::<Result<Vec<_>, _>>()?;
    let required_patterns: Vec<String> = floor_lenses
        .iter()
        .map(|lens| format!("lens-{lens}"))
        .collect();
    let mut plan = json!({
        "kinds": ["task", "synthesise"],
        "max_nodes": ctx.max_nodes,
        "patterns": lowered,
        "patterns_only": true,
        "disjoint_waves": true,
        "wire_exports": true,
    });
    if !required_patterns.is_empty() || floor_min_reviewers > 0 {
        let candidates: Vec<String> = plan["patterns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|pattern| pattern["name"].as_str())
            .filter(|name| name.starts_with("lens-"))
            .map(str::to_owned)
            .collect();
        let reserve = required_patterns.len().max(floor_min_reviewers);
        let model_limit = ctx.max_nodes.min(512_usize.saturating_sub(reserve)).max(1);
        plan["max_nodes"] = json!(model_limit.saturating_add(reserve));
        plan["floor_candidate_patterns"] = json!(candidates);
        plan["floor_min_reviewers"] = json!(floor_min_reviewers);
        plan["model_max_nodes"] = json!(model_limit);
        plan["required_floor_patterns"] = json!(required_patterns);
        plan["floor_classes"] = json!(floor_classes);
    }
    Ok(plan)
}

/// Run `f` with a stage builder over the preset's final parameters, for the
/// selections a static stage would make: the review shape and its lenses.
fn with_builder<T>(
    layers: &Layers,
    name: &str,
    config: &Value,
    origins: &BTreeMap<String, String>,
    brief: &str,
    f: impl FnOnce(&StageBuilder<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let parameters = flatten_parameters(
        name,
        config.get("parameters").unwrap_or(&json!({})),
        "",
        origins,
    )?;
    let (mut stack, mut origins, mut files) = (Vec::new(), origins.clone(), Vec::new());
    let builder = StageBuilder {
        layers,
        name,
        prompt: brief,
        parameters,
        roles: config.get("roles"),
        stack: &mut stack,
        origins: &mut origins,
        repo_files: &mut files,
        scope: "",
    };
    f(&builder)
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

/// One catalogue entry per item of `list`. The `lenses` list is first narrowed
/// by the review panel exactly as a static review stage narrows it, so the
/// entries name only the lenses the panel selects for this subject.
#[allow(
    clippy::too_many_arguments,
    reason = "the panel needs the preset's state"
)]
fn each_item(
    layers: &Layers,
    name: &str,
    config: &Value,
    origins: &BTreeMap<String, String>,
    brief: &str,
    values: &Value,
    pattern: &Value,
    list: &str,
) -> Result<Vec<Value>, String> {
    let items = values
        .get(list)
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| format!("preset {name}: plan pattern for_each names no list: {list}"))?;
    let items = if list == "lenses" {
        with_builder(layers, name, config, origins, brief, |builder| {
            builder.select_items(items)
        })?
    } else {
        items
    };
    let mut entries = Vec::new();
    for item in items {
        let mut entry = bind_item(pattern, &item);
        if let Some(map) = entry.as_object_mut() {
            map.remove("for_each");
            // The lens role prompt reads its own item.
            for node in map
                .get_mut("nodes")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
                .filter_map(Value::as_object_mut)
            {
                node.entry("item").or_insert_with(|| item.clone());
            }
            // A tier the panel chose stays a legal choice for the planner.
            for parameter in map
                .get_mut("parameters")
                .and_then(Value::as_object_mut)
                .into_iter()
                .flat_map(|parameters| parameters.values_mut())
            {
                let default = parameter.get("default").cloned();
                if let (Some(default), Some(allowed)) = (
                    default,
                    parameter.get_mut("enum").and_then(Value::as_array_mut),
                ) && !allowed.contains(&default)
                {
                    allowed.push(default);
                }
            }
        }
        entries.push(entry);
    }
    Ok(entries)
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

/// Attach the lowered catalogue to the built planner node.
pub fn attach(nodes: &mut [Value], plan: Value) -> Result<(), String> {
    fn find(nodes: &mut [Value]) -> Option<&mut Value> {
        for node in nodes {
            if node["id"] == "plan" {
                return Some(node);
            }
            if let Some(children) = node
                .pointer_mut("/composite/nodes")
                .and_then(Value::as_array_mut)
                && let Some(found) = find(children)
            {
                return Some(found);
            }
        }
        None
    }
    let node = find(nodes).ok_or("the planner node was not built")?;
    let mut plan = plan;
    if let Some(fallback) = node
        .as_object_mut()
        .and_then(|node| node.remove("fallback_stage"))
    {
        plan["fallback"] = json!({
            "nodes": fallback["nodes"],
            "exports": fallback["exports"],
        });
        for (field, value) in [
            ("fallback_depends_on", fallback["depends_on"].clone()),
            ("fallback_inputs", fallback["inputs"].clone()),
            ("fallback_source", fallback["source"].clone()),
            ("fallback_read_roots", fallback["read_roots"].clone()),
            ("fallback_title", fallback["title"].clone()),
        ] {
            let empty = value.is_null() || value.as_array().is_some_and(Vec::is_empty);
            if !empty {
                plan[field] = value;
            }
        }
    }
    node["plan"] = plan;
    Ok(())
}

#[cfg(test)]
mod declaration_tests {
    #[test]
    fn initial_custom_parameter_is_not_an_explicit_override() {
        let mut layers = super::Layers::empty();
        layers
            .insert(
                "custom.toml",
                "custom",
                "stages=[]\n[parameters]\nwidth=2\n",
            )
            .unwrap();
        let (_, origins) = layers.resolve("custom").unwrap();
        assert!(!super::explicit(&layers, "custom", &origins, "width"));
        layers
            .insert("override.toml", "custom", "[parameters]\nwidth=3\n")
            .unwrap();
        let (_, origins) = layers.resolve("custom").unwrap();
        assert!(super::explicit(&layers, "custom", &origins, "width"));
    }
}
