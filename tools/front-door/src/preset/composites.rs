//! Template submission adapter. Preset names are aliases, not a second schema.
use super::*;
use provenant_fabric_runtime::graph::{self, Compiler, GraphDocument};

pub const PRESET_TEMPLATES: &[(&str, &str)] = &[
    ("single", "task-once"),
    ("scope", "plan-once"),
    ("chain", "task-chain"),
    ("fan-out", "task-fan-out"),
    ("fan-out-synthesise", "task-fan-out-synthesise"),
    ("scout-swarm", "explore-swarm"),
    ("generator-evaluator", "deliver"),
    ("review", "review-panel"),
    ("big-batch", "batch"),
    ("scope-then-run", "plan-then-deliver"),
    ("deliver", "deliver"),
];

/// Routes and work bindings belong to individual named batch items.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositeLane {
    pub name: String,
    pub worktree: String,
    pub scope: Vec<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub brief: Option<String>,
}

/// Resolve production inputs before lowering through the template compiler.
pub(super) fn expand_for_run(
    layers: &Layers,
    name: &str,
    prompt: &str,
    params: &[(String, Value)],
    overrides: &[String],
) -> Result<Expansion, String> {
    let (config, mut origins) = layers
        .resolve(name)
        .unwrap_or_else(|_| (json!({}), BTreeMap::new()));
    let mut values = flatten_parameters(
        name,
        config.get("parameters").unwrap_or(&json!({})),
        "",
        &origins,
    )?;
    for (key, value) in params {
        values[key] = value.clone();
        origins.insert(format!("parameters.{key}"), "--params-file".into());
    }
    for item in overrides {
        let (path, parsed) = parse_preset_override(item)?;
        let path = path.strip_prefix("parameters.").unwrap_or(path);
        let mut fields = path.split('.').peekable();
        let mut target = &mut values;
        while let Some(field) = fields.next() {
            if fields.peek().is_none() {
                target[field] = parsed["value"].clone();
            } else {
                if target.get(field).is_none() {
                    target[field] = json!({});
                }
                target = &mut target[field];
            }
        }
        origins.insert(format!("parameters.{path}"), "--set".into());
    }
    values["brief"] = values
        .get("brief")
        .cloned()
        .unwrap_or_else(|| json!(prompt));
    if values
        .get("worktree")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
        && let Some(worktree) = &layers.default_worktree
    {
        values["worktree"] = json!(worktree);
    }
    if values.get("write_scope").is_none()
        && let Some(scope) = values.get("scope").cloned()
    {
        values["write_scope"] = scope;
    }
    let mut stack = Vec::new();
    let mut files = Vec::new();
    let builder = StageBuilder {
        layers,
        name,
        prompt,
        parameters: values.clone(),
        roles: None,
        stack: &mut stack,
        origins: &mut origins,
        repo_files: &mut files,
        scope: "",
    };
    let explicit_lenses = params.iter().any(|(key, _)| key == "lenses")
        || overrides
            .iter()
            .any(|item| item.starts_with("parameters.lenses="));
    let lenses = if explicit_lenses {
        values
            .get("lenses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    } else if layers.review.is_some() {
        Vec::new()
    } else {
        vec![json!({"name":"correctness", "tier":"senior"})]
    };
    let selected = builder.select_items(lenses)?;
    let mut resolvers = BTreeMap::from([
        ("review-topology".into(), vec!["f0-v0-s0".into()]),
        ("review-verifiers".into(), vec!["1".into()]),
    ]);
    for lens in selected.iter().filter_map(|lens| lens["name"].as_str()).chain([
        "correctness", "security", "tests", "contract", "design", "adversarial",
    ]) {
        resolvers.insert(format!("review-advice-{lens}"), vec![String::new()]);
    }
    resolvers.insert(
        "review-panel".into(),
        selected.iter().filter_map(|lens| lens["name"].as_str().map(str::to_owned)).collect(),
    );
    let lanes: Vec<CompositeLane> = values
        .get("lanes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|lane| {
            Ok(CompositeLane {
                name: lane["name"]
                    .as_str()
                    .ok_or("batch lane needs a name")?
                    .into(),
                worktree: lane["worktree"]
                    .as_str()
                    .ok_or("batch lane needs a worktree")?
                    .into(),
                scope: serde_json::from_value(lane["scope"].clone()).map_err(|e| e.to_string())?,
                tier: lane
                    .get("tier")
                    .or_else(|| lane.get("route"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                brief: lane
                    .get("brief")
                    .or_else(|| lane.get("spec"))
                    .and_then(Value::as_str)
                    .filter(|brief| !brief.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect::<Result<_, String>>()?;
    let template = if name == "single" {
        "task"
    } else {
        PRESET_TEMPLATES
            .iter()
            .find_map(|(alias, template)| (*alias == name).then_some(*template))
            .unwrap_or(name)
    };
    let library = layers
        .templates
        .as_ref()
        .unwrap_or_else(|| bundled_templates());
    let definition = library.resolve(template)?;
    let known = |key: &str| {
        definition.parameters.contains_key(key)
            || config
                .get("parameters")
                .is_some_and(|parameters| parameters.get(key).is_some())
            || matches!(
                key,
                "brief"
                    | "write_scope"
                    | "lenses"
                    | "risk_tier"
                    | "kind"
                    | "review_shape"
                    | "size"
                    | "risk_tags"
                    | "subject"
                    | "base"
                    | "selection_range"
                    | "lanes"
            )
    };
    for key in params
        .iter()
        .map(|(key, _)| key.as_str())
        .chain(overrides.iter().filter_map(|item| {
            item.split_once('=').map(|(path, _)| {
                path.strip_prefix("parameters.")
                    .unwrap_or(path)
                    .split('.')
                    .next()
                    .unwrap_or(path)
            })
        }))
    {
        if !known(key) {
            return Err(format!("unknown template parameter: {key}"));
        }
    }
    let mut parameters = definition
        .parameters
        .keys()
        .filter_map(|key| values.get(key).map(|value| (key.clone(), value.clone())))
        .collect::<serde_json::Map<_, _>>();
    if !definition.is_composite() {
        for key in ["brief", "worktree", "write_scope"] {
            if let Some(value) = values.get(key) {
                parameters.insert(key.into(), value.clone());
            }
        }
    }
    let mut expansion = expand_composite(
        layers,
        template,
        &Value::Object(parameters),
        &lanes,
        &resolvers,
    )?;
    if expansion.document["nodes"][0].get("composite").is_none() {
        expansion.document["nodes"][0]["prompt"] = values["brief"].clone();
    }
    visit_nodes(
        expansion.document["nodes"]
            .as_array_mut()
            .ok_or("missing nodes")?,
        &mut |node| {
            let id = node["id"].as_str().unwrap_or_default();
            let lens = selected
                .iter()
                .find(|lens| lens["name"].as_str() == Some(id))
                .or_else(|| {
                    (selected.len() == 1 && id == "review" && node.get("composite").is_none())
                        .then(|| &selected[0])
                });
            if let Some(lens) = lens {
                if let Some(tier) = lens["tier"].as_str() {
                    node["route"] = json!({"alias":tier});
                }
                if let Some(group) = lens.get("family_group") {
                    node["family_group"] = group.clone();
                }
                if let Some(subject) = values.get("subject") {
                    node["source"] = subject.clone();
                }
                node["prompt"] = json!(format!(
                    "{}\n\n{}",
                    node["prompt"].as_str().unwrap_or_default(),
                    prompt
                ));
            }
            Ok(())
        },
    )?;
    expansion.document["context"] = json!({});
    expansion.document["provenance"]["preset"] = json!(name);
    expansion.document["provenance"]["parameters"] = values.clone();
    expansion.origins = origins;
    expansion.warnings = layers.warnings.borrow().clone();
    expansion.review_shape = Some(
        layers
            .used_shapes
            .borrow()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
    );
    if let Some(base) = values
        .get("base")
        .filter(|base| base.as_str().is_some_and(|base| !base.is_empty()))
    {
        expansion.document["review_base"] = base.clone();
    }
    Ok(expansion)
}

/// Lower a template or preset alias for submission. The host supplies
/// resolver lists, including `review-panel`. This creates no Git refs.
pub fn expand_composite(
    layers: &Layers,
    name: &str,
    parameters: &Value,
    lanes: &[CompositeLane],
    resolvers: &BTreeMap<String, Vec<String>>,
) -> Result<Expansion, String> {
    let template = PRESET_TEMPLATES
        .iter()
        .find_map(|(preset, template)| (*preset == name).then_some(*template))
        .unwrap_or(name);
    let mut parameters = parameters.clone();
    let parameters = parameters
        .as_object_mut()
        .ok_or("template parameters must be a table")?;
    if template == "batch" && !lanes.is_empty() {
        let names: BTreeSet<_> = lanes.iter().map(|lane| &lane.name).collect();
        if names.len() != lanes.len() || names.iter().any(|name| name.trim().is_empty()) {
            return Err("batch lane names must be nonempty and unique".into());
        }
        parameters.insert(
            "items".into(),
            json!(lanes.iter().map(|lane| &lane.name).collect::<Vec<_>>()),
        );
    }
    let compiler = Compiler {
        library: if layers.exclude_repo.get() {
            layers.trusted_templates.as_ref()
        } else {
            layers.templates.as_ref()
        }
        .unwrap_or_else(|| bundled_templates()),
        resolvers,
    };
    let definition = compiler.library.resolve(template)?;
    let mut supplied = parameters.clone();
    let mut entry = json!({"id":"work", "use":template});
    if !definition.is_composite() {
        for key in ["brief", "worktree", "write_scope"] {
            if !definition.parameters.contains_key(key)
                && let Some(value) = supplied.remove(key)
            {
                entry[key] = value;
            }
        }
        if definition.access != Some(graph::AccessMode::WorktreeWrite) {
            entry
                .as_object_mut()
                .ok_or("missing template node")?
                .remove("worktree");
            entry
                .as_object_mut()
                .ok_or("missing template node")?
                .remove("write_scope");
        }
        if definition.access == Some(graph::AccessMode::WorktreeWrite)
            && entry.get("worktree").is_none()
            && let Some(worktree) = &layers.default_worktree
        {
            entry["worktree"] = json!(worktree);
        }
    }
    entry["with"] = json!(supplied);
    let mut node = compiler.compile(&entry)?;
    if template == "batch" && !lanes.is_empty() {
        let children = &mut node
            .composite
            .as_mut()
            .ok_or("batch must be composite")?
            .nodes;
        let slices = children
            .iter_mut()
            .find(|child| child.id == "slices")
            .and_then(|child| child.composite.as_mut())
            .ok_or("batch must have slices")?;
        for (member, lane) in slices.nodes.iter_mut().zip(lanes) {
            member.worktree = Some(lane.worktree.clone());
            member.write_scope = lane.scope.clone();
            if let Some(tier) = &lane.tier {
                if layers
                    .known_routes
                    .as_ref()
                    .is_some_and(|routes| !routes.contains(tier))
                {
                    return Err(format!("unknown lane tier: {tier}"));
                }
                member.route =
                    Some(serde_json::from_value(json!({"alias":tier})).map_err(|e| e.to_string())?);
            }
            if let Some(brief) = &lane.brief {
                member.prompt = brief.clone();
            }
        }
    }
    let digest = provenant_records::CanonicalV1::file_digest(
        &serde_json::to_vec(&node).map_err(|e| e.to_string())?,
    )
    .to_string();
    let mut document = GraphDocument {
        schema_version: 1,
        graph_id: format!("template-{}", &digest[7..39]),
        nodes: vec![node],
        review_base: None,
        context: None,
        extra: Default::default(),
        limits: None,
        integration_branch: (template == "batch").then(|| {
            parameters
                .get("integration_branch")
                .and_then(Value::as_str)
                .unwrap_or("batch/integration")
                .to_owned()
        }),
        delivery: None,
        decision_session: None,
    };
    document.schema_version = u64::from(graph::minimum_schema_version(&document));
    graph::patch::scan_for_secrets(&serde_json::to_string(&document).map_err(|e| e.to_string())?)?;
    document
        .admit(&layers.graph_limits)
        .map_err(|error| error.to_string())?;
    let mut document = serde_json::to_value(document).map_err(|e| e.to_string())?;
    document["provenance"] = json!({"preset":name,"template":template});
    Ok(Expansion {
        document,
        worktree: parameters
            .get("worktree")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from),
        review_shape: None,
        origins: BTreeMap::new(),
        repo_files: Vec::new(),
        roles: BTreeMap::new(),
        substitutions: Vec::new(),
        warnings: Vec::new(),
    })
}

/// Create a local integration ref without moving the checkout or resetting an
/// existing integration head. Called only when executing, never by explain.
pub(super) fn ensure_integration_branch(directory: &Path, branch: &str) -> Result<(), String> {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use std::ffi::OsString;
    let executable =
        provenant_os::git::git_executable().ok_or("Git unavailable for integration branch")?;
    let executor = SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let run = |args: &[&str]| {
        let mut argv = vec![executable.clone().into_os_string()];
        argv.extend(args.iter().map(OsString::from));
        let env: Vec<(OsString, OsString)> = [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_NO_REPLACE_OBJECTS", "1"),
            ("LC_ALL", "C"),
        ]
        .map(|(key, value)| (key.into(), value.into()))
        .into();
        let mut options = ExecOptions::new(&argv, directory, std::time::Duration::from_secs(30));
        options.env = Some(&env);
        let output = executor
            .execute(options)
            .map_err(|error| error.to_string())?;
        if output.timed_out {
            return Err("Git timed out preparing integration branch".to_owned());
        }
        Ok(output)
    };
    let valid = run(&["check-ref-format", "--branch", branch])?;
    if valid.termination != Termination::Exit(0)
        || branch.starts_with('-')
        || branch.starts_with("origin/")
    {
        return Err("integration branch must be a valid local branch name".into());
    }
    let reference = format!("refs/heads/{branch}");
    let existing = run(&["show-ref", "--verify", "--quiet", &reference])?;
    if existing.termination == Termination::Exit(0) {
        return Ok(());
    }
    if existing.termination != Termination::Exit(1) {
        return Err("cannot inspect integration branch".into());
    }
    let result = run(&["branch", "--", branch, "HEAD"])?;
    if result.termination == Termination::Exit(0) {
        Ok(())
    } else {
        Err(format!(
            "cannot create integration branch: {}",
            String::from_utf8_lossy(&result.stderr.bytes).trim()
        ))
    }
}

#[cfg(test)]
#[path = "composite_tests.rs"]
mod tests;
