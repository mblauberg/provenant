use crate::preset_roles::{RoleLibrary, prompt_file_reference};
use provenant_fabric_runtime::read_roots::ReadRoots;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

const DEPTH: usize = 8;

#[path = "preset_cli.rs"]
mod presentation;

#[cfg(test)]
#[allow(
    dead_code,
    reason = "integration fixtures include the preset module without unit tests"
)]
pub(super) fn human_for_test(data: &Value, full: bool) -> String {
    presentation::human(data, full)
}

#[path = "preset_plan.rs"]
pub mod planner;

mod panel;
#[cfg(test)]
#[allow(unused_imports, reason = "preset tests construct a selector result")]
pub use panel::ShapeHint;
#[cfg(test)]
#[allow(
    unused_imports,
    reason = "used by library tests; integration targets also include this module"
)]
pub(crate) use panel::fixture_protected_selector;
pub(crate) use panel::format_family_group;
pub use panel::{ShapeSelector, review_selector};

#[derive(Default)]
pub struct Layers {
    /// Host limits used by the runtime's composite structural check.
    pub graph_limits: provenant_fabric_runtime::config::GraphLimits,
    built_nodes: std::cell::Cell<usize>,
    /// Routing review shapes. Each review stage selects its own shape from its
    /// final `review_shape` and `risk_tier` parameters.
    pub review: Option<ShapeSelector>,
    pub project_dir: Option<PathBuf>,
    /// Launch directory, used to resolve relative worktree bindings.
    pub launch_dir: Option<PathBuf>,
    pub default_worktree: Option<PathBuf>,
    /// One inventory captured for this preset command, shared by its planner,
    /// spec lookup and read-root selection. Library callers without a command
    /// snapshot retain the ordinary fresh lookup.
    command_worktrees: Option<Vec<PathBuf>>,
    used_shapes: std::cell::RefCell<BTreeSet<String>>,
    /// Bases of the deferred reviews this expansion emitted; a graph has one.
    review_bases: std::cell::RefCell<BTreeSet<String>>,
    /// The role library: bundled roles, then user and repository `roles.toml`.
    pub roles: RoleLibrary,
    /// Named routes the routing configuration defines. `None` skips the
    /// check that a role's tier names a configured route.
    pub known_routes: Option<BTreeSet<String>>,
    used_roles: std::cell::RefCell<BTreeMap<String, Value>>,
    pub substitutions: std::cell::RefCell<Vec<Value>>,
    warnings: std::cell::RefCell<Vec<String>>,
    spec_contexts: std::cell::RefCell<BTreeMap<String, Option<String>>>,
    entries: BTreeMap<String, Vec<(String, Value)>>,
    errors: BTreeMap<String, Vec<String>>,
    repo_files: BTreeSet<String>,
    /// Resolve without repository layers, for the landing floor.
    exclude_repo: std::cell::Cell<bool>,
    /// The planner front-end's host inputs. `None` never plans.
    pub plan: Option<planner::PlanContext>,
    delegated_plan_stage: std::cell::RefCell<Option<String>>,
}

#[derive(Debug)]
pub struct Expansion {
    pub review_shape: Option<String>,
    pub document: Value,
    /// The preset's explicitly bound worktree, independent of its read roots.
    pub worktree: Option<PathBuf>,
    pub origins: BTreeMap<String, String>,
    pub repo_files: Vec<String>,
    /// Library roles the graph used: origin, `extends` chain, digest and the
    /// content digest of each reference.
    pub roles: BTreeMap<String, Value>,
    /// Requested routes replaced by a fallback, for example a missing web route.
    pub substitutions: Vec<Value>,
    pub warnings: Vec<String>,
    /// The planner decision, for a preset that declares planner patterns.
    pub plan: Option<planner::Decision>,
}

/// Decode a lowered node and attach its stage and field to a type error.
/// The individual-field probes run only on an error, so valid expansions keep
/// the normal single decode per node.
fn decode_lowered_node(
    value: &Value,
) -> Result<provenant_fabric_runtime::graph::GraphNode, String> {
    // Every preset node goes through the template compiler: `use`, `over`
    // and `repeat` lower to compiled nodes, and compiled nodes pass through.
    let library = bundled_templates();
    let lowered = provenant_fabric_runtime::graph::Compiler::new(library).lower(value)?;
    decode_compiled_node(&lowered)
}

fn bundled_templates() -> &'static provenant_fabric_runtime::graph::TemplateLibrary {
    static LIBRARY: std::sync::OnceLock<provenant_fabric_runtime::graph::TemplateLibrary> =
        std::sync::OnceLock::new();
    LIBRARY.get_or_init(provenant_fabric_runtime::graph::TemplateLibrary::bundled)
}

fn decode_compiled_node(
    value: &Value,
) -> Result<provenant_fabric_runtime::graph::GraphNode, String> {
    provenant_fabric_runtime::graph::GraphNode::deserialize(value).map_err(|error| {
        for children in [
            value.get("composite").and_then(|group| group.get("nodes")),
            value.get("loop").and_then(|body| body.get("body")),
            value.pointer("/plan/fallback/nodes"),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        {
            for child in children {
                if let Err(error) = decode_compiled_node(child) {
                    return error;
                }
            }
        }
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let field = value.as_object().and_then(|fields| {
            fields.iter().find_map(|(key, value)| {
                let mut probe = json!({"id": "probe", "access": "read_only"});
                probe[key] = value.clone();
                serde_json::from_value::<provenant_fabric_runtime::graph::GraphNode>(probe)
                    .is_err()
                    .then_some(key.as_str())
            })
        });
        match field {
            Some(field) => format!("node {id}: {field}: {error}"),
            None => format!("node {id}: {error}"),
        }
    })
}

/// Raw graph nodes embedded in a leaf are not passed through `lower_leaf`;
/// reject their undeclared fields at the same boundary.
fn unknown_embedded_node_field(
    node: &provenant_fabric_runtime::graph::GraphNode,
) -> Option<String> {
    let children = node
        .composite
        .iter()
        .flat_map(|spec| &spec.nodes)
        .chain(node.loop_body.iter().flat_map(|spec| &spec.body))
        .chain(
            node.plan
                .iter()
                .filter_map(|spec| spec.fallback.as_ref())
                .flat_map(|spec| &spec.nodes),
        );
    for child in children {
        if let Some(field) = child.extra.keys().next() {
            return Some(format!("node {}: unknown leaf field: {field}", child.id));
        }
        if let Some(error) = unknown_embedded_node_field(child) {
            return Some(error);
        }
    }
    None
}

/// When a known field has the wrong type, typed decoding cannot reach nested
/// extras. Inspect each embedded node independently on that error path.
fn unknown_embedded_node_in_invalid_value(value: &Value) -> Option<String> {
    for path in ["/composite/nodes", "/loop/body", "/plan/fallback/nodes"] {
        let Some(children) = value.pointer(path).and_then(Value::as_array) else {
            continue;
        };
        for child in children {
            let id = child
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            if let Ok(node) = provenant_fabric_runtime::graph::GraphNode::deserialize(child) {
                if let Some(field) = node.extra.keys().next() {
                    return Some(format!("node {id}: unknown leaf field: {field}"));
                }
                if let Some(error) = unknown_embedded_node_field(&node) {
                    return Some(error);
                }
            } else if let Some(fields) = child.as_object() {
                for (key, value) in fields {
                    if key == "id" || key == "access" {
                        continue;
                    }
                    let probe = json!({"id": "probe", "access": "read_only", (key): value});
                    if serde_json::from_value::<provenant_fabric_runtime::graph::GraphNode>(probe)
                        .is_ok_and(|node| node.extra.contains_key(key))
                    {
                        return Some(format!("node {id}: unknown leaf field: {key}"));
                    }
                }
            }
            if let Some(error) = unknown_embedded_node_in_invalid_value(child) {
                return Some(error);
            }
        }
    }
    None
}

impl Layers {
    #[allow(
        clippy::expect_used,
        reason = "bundled preset literals are built-in invariants"
    )]
    pub fn empty() -> Self {
        let mut layers = Self::default();
        for (name, text) in [
            ("single", include_str!("../assets/presets/single.toml")),
            ("scope", include_str!("../assets/presets/scope.toml")),
            ("chain", include_str!("../assets/presets/chain.toml")),
            ("fan-out", include_str!("../assets/presets/fan-out.toml")),
            (
                "fan-out-synthesise",
                include_str!("../assets/presets/fan-out-synthesise.toml"),
            ),
            (
                "scout-swarm",
                include_str!("../assets/presets/scout-swarm.toml"),
            ),
            (
                "generator-evaluator",
                include_str!("../assets/presets/generator-evaluator.toml"),
            ),
            ("review", include_str!("../assets/presets/review.toml")),
            (
                "big-batch",
                include_str!("../assets/presets/big-batch.toml"),
            ),
            (
                "scope-then-run",
                include_str!("../assets/presets/scope-then-run.toml"),
            ),
            ("deliver", include_str!("../assets/presets/deliver.toml")),
        ] {
            layers
                .insert("bundled", name, text)
                .expect("valid bundled preset");
        }
        layers
    }

    /// The initial layer declares a preset's defaults; later layers and dispatch
    /// overrides can supply their own values.
    fn is_initial_origin(&self, name: &str, parameter: &str, origin: &str) -> bool {
        let mut current = name;
        let mut seen = BTreeSet::new();
        let mut declaration = None;
        while seen.insert(current) {
            let Some(layers) = self.entries.get(current) else {
                return false;
            };
            if let Some((first, _)) = layers.iter().find(|(_, value)| {
                value
                    .get("parameters")
                    .and_then(|parameters| parameters.get(parameter))
                    .is_some()
            }) {
                declaration = Some(first.as_str());
            }
            let Some(parent) = layers
                .iter()
                .rev()
                .find_map(|(_, value)| value.get("extends").and_then(Value::as_str))
            else {
                return declaration == Some(origin);
            };
            current = parent;
        }
        false
    }

    pub fn insert(&mut self, origin: &str, name: &str, text: &str) -> Result<(), String> {
        if !valid_name(name) {
            return Err(format!("invalid preset name: {name}"));
        }
        let value: Value = toml_edit::de::from_str(text).map_err(|e| format!("{origin}: {e}"))?;
        self.entries
            .entry(name.into())
            .or_default()
            .push((origin.into(), value));
        Ok(())
    }

    pub fn load(user: &Path, repo: &Path) -> Result<Self, String> {
        let mut layers = Self::empty();
        for (is_repo, dir) in [(false, user), (true, repo)] {
            // `roles.toml` sits beside the `presets` directory.
            if let Some(parent) = dir.parent() {
                layers.roles.add_file(&parent.join("roles.toml"), is_repo)?;
            }
        }
        for (is_repo, dir) in [(false, user), (true, repo)] {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("{}: {e}", dir.display())),
            };
            let mut paths = entries
                .map(|e| e.map(|v| v.path()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            paths.sort();
            for path in paths {
                if path.extension().is_none_or(|v| v != "toml") {
                    continue;
                }
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("<invalid filename>")
                    .to_owned();
                let origin = path.display().to_string();
                let result = (|| {
                    let mut file = provenant_os::fs::FileHandle::open_source(&path)
                        .map_err(|e| e.to_string())?;
                    let bytes = file.read_bounded(1_048_576).map_err(|e| e.to_string())?;
                    let content = String::from_utf8(bytes).map_err(|e| e.to_string())?;
                    layers.insert(&origin, &name, &content)
                })();
                if let Err(error) = result {
                    layers
                        .errors
                        .entry(name)
                        .or_default()
                        .push(format!("{origin}: {error}"));
                } else if is_repo {
                    layers.repo_files.insert(origin);
                }
            }
        }
        Ok(layers)
    }

    pub fn listing(&self) -> Vec<Value> {
        let names: BTreeSet<_> = self.entries.keys().chain(self.errors.keys()).collect();
        names
            .into_iter()
            .map(|name| {
                let layers = self
                    .entries
                    .get(name)
                    .into_iter()
                    .flatten()
                    .map(|(origin, _)| origin)
                    .collect::<Vec<_>>();
                let mut errors = self.errors.get(name).cloned().unwrap_or_default();
                if errors.is_empty()
                    && let Err(error) = expand(self, name, "", &[])
                {
                    errors.push(error);
                }
                let pattern = self
                    .resolve(name)
                    .ok()
                    .and_then(|(config, _)| config.get("pattern").cloned())
                    .unwrap_or(Value::Null);
                json!({
                    "name": name,
                    "level": pattern.get("level"),
                    "summary": pattern.get("summary"),
                    "cost": pattern.get("cost"),
                    "layers": layers,
                    "errors": errors
                })
            })
            .collect()
    }

    /// The role library, or one role with its `extends` chain applied.
    pub fn roles_view(&self, name: Option<&str>) -> Result<Value, String> {
        let describe = |role: &str| -> Result<Value, String> {
            let resolved = self.roles.resolve(role, None, false)?;
            let profile = &resolved.profile;
            let mut references = Vec::new();
            for item in profile
                .get("references")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let reference = item.get("ref").and_then(Value::as_str).unwrap_or_default();
                let when = item.get("when").cloned().unwrap_or(Value::Null);
                // Dynamic references (`${item.name}`) resolve per node.
                let digest = if reference.contains("${") {
                    Value::Null
                } else {
                    json!(self.roles.reference(reference)?.digest)
                };
                references.push(json!({"ref": reference, "when": when, "digest": digest}));
            }
            Ok(json!({
                "name": role,
                "origin": resolved.origin,
                "extends": resolved.chain[1..],
                "kind": profile.get("kind"),
                "tier": profile.get("tier"),
                "access": profile.get("access"),
                "family": profile.get("family"),
                "purpose": profile.get("purpose"),
                "card": profile.get("card"),
                "references": references,
            }))
        };
        match name {
            Some(name) => describe(name),
            None => Ok(json!({"roles": self
                .roles
                .names()
                .iter()
                .map(|role| describe(role))
                .collect::<Result<Vec<_>, _>>()?})),
        }
    }

    /// The typed interface of a preset, rendered for a reader. Creates no state.
    pub fn explain(&self, name: &str) -> Result<Value, String> {
        let expansion = expand(self, name, "", &[])?;
        let (config, origins) = self.resolve(name)?;
        let origin_of = |key: &str| {
            let path = format!("parameters.{key}");
            origins
                .get(&path)
                .or_else(|| {
                    origins
                        .iter()
                        .find(|(origin, _)| origin.starts_with(&format!("{path}.")))
                        .map(|(_, origin)| origin)
                })
                .cloned()
        };
        let parameters = flatten_parameters(
            name,
            config.get("parameters").unwrap_or(&json!({})),
            "",
            &origins,
        )?;
        let (policy_origin, policy_text) = self.local_check_policy(&parameters, &origins)?;
        let parameters: Vec<Value> = config
            .get("parameters")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(key, value)| {
                let mut entry = if is_spec(value) {
                    value.clone()
                } else {
                    json!({"default": value})
                };
                entry["name"] = json!(key);
                entry["origin"] = json!(origin_of(key));
                entry
            })
            .collect();
        let stages: Vec<Value> = config
            .get("stages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(position, stage)| {
                let mut entry = json!({"id": stage_id(stage, position)});
                for key in ["preset", "role", "kind", "count", "for_each"] {
                    if let Some(value) = stage.get(key) {
                        entry[key] = value.clone();
                    }
                }
                for key in ["parallel", "stages", "loop"] {
                    if stage.get(key).is_some() {
                        entry["group"] = json!(key);
                    }
                }
                entry
            })
            .collect();
        let roles: Vec<Value> = config
            .get("roles")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(role, profile)| json!({"name": role, "tier": profile.get("tier")}))
            .collect();
        let prompts: BTreeMap<_, _> = expansion
            .roles
            .keys()
            .filter_map(|role| {
                find_prompt(&expansion.document["nodes"], role).map(|prompt| (role, prompt))
            })
            .collect();
        Ok(json!({
            "name": name,
            "pattern": config.get("pattern"),
            "parameters": parameters,
            "stages": stages,
            "roles": roles,
            "prompts": prompts,
            "local_check_policy": json!({
                "prompt": policy_text,
                "origin": policy_origin,
            }),
            "layers": self.entries.get(name).into_iter().flatten().map(|(origin, _)| origin).collect::<Vec<_>>(),
            "next": [
                format!("provenant run --preset {name} --prompt-file FILE --explain"),
                format!("provenant run --preset {name} --prompt-file FILE --set STAGE.PARAMETER=VALUE")
            ]
        }))
    }

    /// A non-empty preset parameter wins; otherwise use the layered role library.
    fn local_check_policy(
        &self,
        parameters: &Value,
        origins: &BTreeMap<String, String>,
    ) -> Result<(String, String), String> {
        if let Some(text) = parameters
            .get("local_check_policy")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            let origin = origins
                .get("parameters.local_check_policy")
                .or_else(|| origins.get("parameters.local_check_policy.default"))
                .map(String::as_str)
                .unwrap_or("preset");
            return Ok((origin.to_owned(), text.to_owned()));
        }
        self.roles
            .policy(self.exclude_repo.get())
            .map(|(origin, text)| (origin.to_owned(), text.to_owned()))
            .ok_or_else(|| "roles library is missing local_check_policy".into())
    }

    pub fn resolve(&self, name: &str) -> Result<(Value, BTreeMap<String, String>), String> {
        let (config, mut origins) = self.resolve_inner(name, &mut Vec::new(), &mut Vec::new())?;
        origins.retain(|path, _| !path.ends_with("$scope_read_only"));
        Ok((config, origins))
    }

    fn resolve_inner(
        &self,
        name: &str,
        stack: &mut Vec<String>,
        repo_files: &mut Vec<String>,
    ) -> Result<(Value, BTreeMap<String, String>), String> {
        if stack.iter().any(|v| v == name) {
            return Err(format!(
                "preset inheritance cycle: {} -> {name}",
                stack.join(" -> ")
            ));
        }
        if stack.len() >= DEPTH {
            return Err(format!("preset inheritance depth exceeds {DEPTH}"));
        }
        stack.push(name.into());
        if let Some(errors) = self.errors.get(name) {
            return Err(errors.join("; "));
        }
        let entries: Vec<&(String, Value)> = self
            .entries
            .get(name)
            .into_iter()
            .flatten()
            .filter(|(origin, _)| !self.exclude_repo.get() || !self.repo_files.contains(origin))
            .collect();
        if entries.is_empty() {
            return Err(format!("unknown preset: {name}"));
        }
        let parent = entries
            .iter()
            .rev()
            .find_map(|(origin, value)| value.get("extends").map(|parent| (origin, parent)));
        let (mut value, mut origins) = match parent {
            Some((origin, parent)) => self
                .resolve_inner(
                    parent.as_str().ok_or_else(|| {
                        format!("preset {name}: extends ({origin}): must be a preset name")
                    })?,
                    stack,
                    repo_files,
                )
                .map_err(|error| format!("preset {name}: extends ({origin}): {error}"))?,
            None => (json!({}), BTreeMap::new()),
        };
        for (origin, layer) in entries {
            if self.repo_files.contains(origin) && !repo_files.contains(origin) {
                repo_files.push(origin.clone());
            }
            let mut layer = layer.clone();
            let table = layer.as_object_mut().ok_or("preset must be a table")?;
            table.remove("extends");
            if let Some(stages) = table.get_mut("stages").and_then(Value::as_array_mut) {
                for stage in stages {
                    if let Some(floor) = stage
                        .as_object_mut()
                        .and_then(|stage| stage.remove("read_only_floor"))
                    {
                        match floor.as_bool() {
                            Some(true) => {
                                origins.insert("$scope_read_only".into(), origin.clone());
                            }
                            Some(false) => {}
                            None => {
                                return Err(format!(
                                    "preset {name}: stage read_only_floor must be a boolean ({origin})"
                                ));
                            }
                        }
                    }
                }
            }
            merge(&mut value, &layer, "", origin, &mut origins)
                .map_err(|error| format!("preset {name}: {error} ({origin})"))?;
        }
        stack.pop();
        Ok((value, origins))
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

const SPEC_KEYS: [&str; 11] = [
    "type",
    "default",
    "min",
    "max",
    "enum",
    "doc",
    "bind",
    "tracker_advice",
    "inherit",
    "inherit_explicit",
    "optional",
];
const TYPE_NAMES: [&str; 6] = ["string", "integer", "number", "boolean", "list", "table"];

/// A table made only of typed-parameter keys. Other tables are plain values.
/// A `type` that is not a type name marks a plain value (`{ type = "commit" }`)
/// unless the table has a `default`, which only a schema carries.
fn is_fragment(value: &Value) -> bool {
    value.as_object().is_some_and(|map| {
        !map.is_empty()
            && map.keys().all(|key| SPEC_KEYS.contains(&key.as_str()))
            && (map.contains_key("default")
                || map
                    .get("type")
                    .is_none_or(|kind| kind.as_str().is_none_or(|kind| TYPE_NAMES.contains(&kind))))
    })
}

/// A typed parameter: `{ type, default, min, max, enum, doc }`.
fn is_spec(value: &Value) -> bool {
    is_fragment(value)
}

type RequestBindings = (Vec<(String, Value)>, Vec<String>);

/// Offer request values only to parameters that explicitly bind them.
pub(super) fn request_bindings(
    layers: &Layers,
    name: &str,
    risk: Option<&str>,
    kind: Option<&str>,
) -> Result<RequestBindings, String> {
    let (config, _) = layers.resolve(name)?;
    let mut bindings = Vec::new();
    let mut warnings = Vec::new();
    for (key, flag, value) in [
        ("risk_tier", "--risk-tier", risk),
        ("kind", "--task-kind", kind),
    ] {
        let Some(value) = value else { continue };
        let mut bound = false;
        if let Some(parameters) = config.get("parameters").and_then(Value::as_object) {
            for (parameter, spec) in parameters {
                if spec.get("bind").and_then(Value::as_str) == Some(key) {
                    bindings.push((parameter.clone(), json!(value)));
                    bound = true;
                }
            }
        }
        if !bound {
            warnings.push(format!("{flag} ignored: preset {name} declares no binding"));
        }
    }
    Ok((bindings, warnings))
}

/// Resolve request and parameter-file values in dispatch order for both
/// experiment eligibility and the graph that will actually run.
pub(super) fn dispatch_parameters(
    layers: &Layers,
    name: &str,
    selected: impl IntoIterator<Item = (String, Value)>,
    risk: Option<&str>,
    kind: Option<&str>,
    file_params: &[(String, Value)],
) -> Result<RequestBindings, String> {
    let (bindings, warnings) = request_bindings(layers, name, risk, kind)?;
    let mut values = selected.into_iter().collect::<Vec<_>>();
    for (key, value) in bindings.into_iter().chain(file_params.iter().cloned()) {
        if let Some(existing) = values.iter_mut().find(|(name, _)| name == &key) {
            existing.1 = value;
        } else {
            values.push((key, value));
        }
    }
    Ok((values, warnings))
}

pub(super) fn tracker_advice_parameter(config: &Value) -> Option<&str> {
    config
        .get("parameters")?
        .as_object()?
        .iter()
        .find_map(|(name, spec)| {
            (spec.get("tracker_advice").and_then(Value::as_bool) == Some(true))
                .then_some(name.as_str())
        })
}

fn selects_review_lenses(entry: &Value) -> bool {
    entry.get("review_lenses").and_then(Value::as_bool) == Some(true)
}

fn merge(
    target: &mut Value,
    source: &Value,
    path: &str,
    origin: &str,
    origins: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    if path.starts_with("parameters.")
        && path.matches('.').count() == 1
        && is_spec(target)
        && !is_fragment(source)
    {
        target["default"] = source.clone();
        origins.insert(path.into(), origin.into());
        return Ok(());
    }
    if let Some(map) = source.as_object() {
        if map.is_empty() && !path.is_empty() {
            origins.insert(path.into(), origin.into());
        }
        let target = target
            .as_object_mut()
            .ok_or_else(|| format!("{path} must be a table"))?;
        for (key, value) in map {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            let initial = if value.is_object() {
                json!({})
            } else {
                Value::Null
            };
            merge(
                target.entry(key).or_insert(initial),
                value,
                &child,
                origin,
                origins,
            )?;
        }
    } else {
        *target = source.clone();
        origins.insert(path.into(), origin.into());
    }
    Ok(())
}

/// The id of a top-level stage: its `id`, or `sN` by position.
fn stage_id(entry: &Value, position: usize) -> String {
    entry
        .get("id")
        .and_then(Value::as_str)
        .map_or_else(|| format!("s{}", position + 1), str::to_owned)
}

fn set_nested(target: &mut Value, parts: &[&str], replacement: Value) -> Result<(), String> {
    let (last, parents) = parts.split_last().ok_or("empty path")?;
    let mut current = target;
    for part in parents {
        current = current
            .as_object_mut()
            .ok_or_else(|| format!("{part} is not a table"))?
            .entry(*part)
            .or_insert_with(|| json!({}));
    }
    let map = current
        .as_object_mut()
        .ok_or_else(|| format!("{last} is not in a table"))?;
    match map.get_mut(*last) {
        Some(existing) if is_spec(existing) && !is_fragment(&replacement) => {
            existing["default"] = replacement;
        }
        _ => {
            map.insert((*last).into(), replacement);
        }
    }
    Ok(())
}

/// `STAGE.FIELD=value` sets a field of a top-level stage entry. On a stage that
/// runs a preset, FIELD is one of that preset's parameters.
fn override_stage(
    value: &mut Value,
    path: &str,
    parts: &[&str],
    replacement: &Value,
    append: bool,
    origins: &mut BTreeMap<String, String>,
    origin: &str,
) -> Result<bool, String> {
    let Some(stages) = value.get_mut("stages").and_then(Value::as_array_mut) else {
        return Ok(false);
    };
    let Some((_, entry)) = stages
        .iter_mut()
        .enumerate()
        .find(|(position, entry)| stage_id(entry, *position) == parts[0])
    else {
        return Ok(false);
    };
    let field = match parts {
        [_, "parameters", rest @ ..] if !rest.is_empty() => rest,
        [_, rest @ ..] => rest,
        _ => return Ok(false),
    };
    if field[0] == "id" || field[0] == "preset" {
        return Err(format!("{path}: a stage {} cannot be overridden", field[0]));
    }
    if entry.get("preset").is_some() {
        let entry = entry
            .as_object_mut()
            .ok_or("stage entries must be tables")?;
        // Applied to the child's resolved defaults, not the entry, so the rest
        // of a table value survives, and in command-line order.
        entry
            .entry("set")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("stage set must be a list")?
            .push(json!({"path": field.join("."), "value": replacement, "append": append}));
    } else if append {
        return Err(format!(
            "{path}: += appends to a list parameter of a preset stage"
        ));
    } else {
        set_nested(entry, field, replacement.clone())?;
    }
    origins.insert(format!("{}.{}", parts[0], field.join(".")), origin.into());
    Ok(true)
}

fn override_value(
    value: &mut Value,
    path: &str,
    replacement: Value,
    origins: &mut BTreeMap<String, String>,
    origin: &str,
) -> Result<(), String> {
    // A trailing `+` (`path+=value`) appends to a list instead of replacing it.
    let (path, append) = match path.strip_suffix('+') {
        Some(path) => (path, true),
        None => (path, false),
    };
    let parts: Vec<_> = path.split('.').collect();
    if parts.len() >= 2
        && parts[0] != "parameters"
        && parts[0] != "roles"
        && override_stage(value, path, &parts, &replacement, append, origins, origin)?
    {
        return Ok(());
    }
    let mut current = value;
    for (index, part) in parts.iter().enumerate() {
        if is_spec(current) && !SPEC_KEYS.contains(part) {
            current = current
                .get_mut("default")
                .ok_or_else(|| format!("unknown path: {path}"))?;
        }
        let map = current
            .as_object_mut()
            .ok_or_else(|| format!("unknown path: {path}"))?;
        if index + 1 == parts.len() {
            let existing = map
                .get_mut(*part)
                .ok_or_else(|| format!("unknown parameter, role or stage: {path}"))?;
            let slot = if is_spec(existing) && !is_fragment(&replacement) {
                &mut existing["default"]
            } else {
                existing
            };
            if append {
                let list = slot.as_array_mut().ok_or_else(|| {
                    format!("{path}: += appends to a list parameter, but this one is not a list")
                })?;
                match replacement {
                    Value::Array(items) => list.extend(items),
                    item => list.push(item),
                }
            } else {
                *slot = replacement;
            }
            origins.insert(path.into(), origin.into());
            return Ok(());
        }
        current = map
            .get_mut(*part)
            .ok_or_else(|| format!("unknown parameter, role or stage: {path}"))?;
    }
    Err(format!("unknown path: {path}"))
}

fn valid_paths(config: &Value, layers: &Layers) -> String {
    let mut paths = Vec::new();
    if let Some(params) = config.get("parameters").and_then(Value::as_object) {
        paths.extend(params.keys().map(|key| format!("parameters.{key}")));
    }
    if let Some(stages) = config.get("stages").and_then(Value::as_array) {
        for (position, stage) in stages.iter().enumerate() {
            let id = stage_id(stage, position);
            match stage.get("preset").and_then(Value::as_str) {
                Some(child) => {
                    if let Ok((child, _)) = layers.resolve(child)
                        && let Some(params) = child.get("parameters").and_then(Value::as_object)
                    {
                        paths.extend(params.keys().map(|key| format!("{id}.{key}")));
                    }
                }
                None => paths.push(format!("{id}.FIELD")),
            }
        }
    }
    if let Some(roles) = config.get("roles").and_then(Value::as_object) {
        for (name, profile) in roles {
            if let Some(profile) = profile.as_object() {
                paths.extend(profile.keys().map(|key| format!("roles.{name}.{key}")));
            }
        }
    }
    paths.join(", ")
}

fn scalar_text(value: &Value) -> String {
    value.to_string()
}

/// Check each typed parameter against its schema and return plain values.
/// Errors name the parameter and where its value came from.
fn flatten_parameters(
    name: &str,
    parameters: &Value,
    scope: &str,
    origins: &BTreeMap<String, String>,
) -> Result<Value, String> {
    let map = parameters
        .as_object()
        .ok_or_else(|| invalid_config(name, "parameters", "must be a table", scope, origins))?;
    let mut plain = serde_json::Map::new();
    for (key, value) in map {
        if !is_fragment(value) {
            plain.insert(key.clone(), value.clone());
            continue;
        }
        let fail = |message: String| {
            invalid_config(name, &format!("parameters.{key}"), &message, scope, origins)
        };
        if value
            .get("optional")
            .is_some_and(|optional| !optional.is_boolean())
        {
            return Err(fail("optional must be a boolean".into()));
        }
        let absent = Value::Null;
        let default = match value.get("default") {
            Some(default) => default,
            None if value.get("optional") == Some(&Value::Bool(true)) => &absent,
            None => return Err(fail("required parameter is missing".into())),
        };
        if let Some(kind) = value.get("type")
            && !kind.as_str().is_some_and(|kind| TYPE_NAMES.contains(&kind))
        {
            return Err(fail(format!(
                "type must be one of {}",
                TYPE_NAMES.join(", ")
            )));
        }
        if let Some(binding) = value.get("bind")
            && !binding
                .as_str()
                .is_some_and(|binding| ["risk_tier", "kind"].contains(&binding))
        {
            return Err(fail("bind must be risk_tier or kind".into()));
        }
        if value
            .get("tracker_advice")
            .is_some_and(|flag| !flag.is_boolean())
        {
            return Err(fail("tracker_advice must be a boolean".into()));
        }
        if value.get("inherit").is_some_and(|flag| !flag.is_boolean()) {
            return Err(fail("inherit must be a boolean".into()));
        }
        if value
            .get("inherit_explicit")
            .is_some_and(|flag| !flag.is_boolean())
        {
            return Err(fail("inherit_explicit must be a boolean".into()));
        }
        for bound in ["min", "max"] {
            if let Some(limit) = value.get(bound)
                && !limit.is_number()
            {
                return Err(fail(format!("{bound} must be a number")));
            }
        }
        if let Some(allowed) = value.get("enum")
            && !allowed.as_array().is_some_and(|list| !list.is_empty())
        {
            return Err(fail("enum must be a nonempty list".into()));
        }
        if let (Some(min), Some(max)) = (
            value.get("min").and_then(Value::as_f64),
            value.get("max").and_then(Value::as_f64),
        ) && min > max
        {
            return Err(fail("min must not exceed max".into()));
        }
        if default.is_null() {
            plain.insert(key.clone(), Value::Null);
            continue;
        }
        if (value.get("min").is_some() || value.get("max").is_some())
            && !matches!(
                default,
                Value::Number(_) | Value::Array(_) | Value::String(_)
            )
        {
            return Err(fail("min and max apply to numbers, lists and text".into()));
        }
        let fits = match value.get("type").and_then(Value::as_str) {
            Some("string") => default.is_string(),
            Some("integer") => default.is_i64() || default.is_u64(),
            Some("number") => default.is_number(),
            Some("boolean") => default.is_boolean(),
            Some("list") => default.is_array(),
            Some("table") => default.is_object(),
            _ => true,
        };
        if !fits {
            return Err(fail(format!(
                "must be {}, received {}",
                value["type"].as_str().unwrap_or_default(),
                scalar_text(default)
            )));
        }
        let size = match default {
            Value::Number(number) => number.as_f64(),
            Value::Array(list) => Some(list.len() as f64),
            Value::String(text) => Some(text.chars().count() as f64),
            _ => None,
        };
        let (min, max) = (
            value.get("min").and_then(Value::as_f64),
            value.get("max").and_then(Value::as_f64),
        );
        if let Some(size) = size {
            let bound = |limit: f64| limit.to_string();
            match (min, max) {
                (Some(min), Some(max)) if size < min || size > max => {
                    return Err(fail(format!("must be {} to {}", bound(min), bound(max))));
                }
                (Some(min), None) if size < min => {
                    return Err(fail(format!("must be at least {}", bound(min))));
                }
                (None, Some(max)) if size > max => {
                    return Err(fail(format!("must be at most {}", bound(max))));
                }
                _ => {}
            }
        }
        if let Some(allowed) = value.get("enum").and_then(Value::as_array)
            && !allowed.contains(default)
        {
            return Err(fail(format!(
                "must be one of {}, received {}",
                Value::Array(allowed.clone()),
                scalar_text(default)
            )));
        }
        plain.insert(key.clone(), default.clone());
    }
    Ok(Value::Object(plain))
}

fn invalid_config(
    name: &str,
    key: &str,
    message: &str,
    scope: &str,
    origins: &BTreeMap<String, String>,
) -> String {
    let path = key.to_owned();
    let origin = origins
        .get(&path)
        .or_else(|| {
            origins
                .iter()
                .find(|(key, _)| key.starts_with(&format!("{path}.")))
                .map(|(_, value)| value)
        })
        .map(String::as_str)
        .unwrap_or("unknown origin");
    // Origins are local to the preset being built; the scope names the stage
    // path that reached it, so only the path is qualified.
    let shown = match (scope.is_empty(), key.strip_prefix("parameters.")) {
        (true, _) => key.to_owned(),
        (false, Some(parameter)) => format!("{scope}.{parameter}"),
        (false, None) => format!("{scope}.{key}"),
    };
    format!("preset {name}: {shown} ({origin}): {message}")
}

pub fn expand(
    layers: &Layers,
    name: &str,
    prompt: &str,
    overrides: &[String],
) -> Result<Expansion, String> {
    expand_with(layers, name, prompt, &[], overrides)
}

/// `params` are parameter values from a `--params-file`; `overrides` are
/// `--set` items and win over them.
pub fn expand_with(
    layers: &Layers,
    name: &str,
    prompt: &str,
    params: &[(String, Value)],
    overrides: &[String],
) -> Result<Expansion, String> {
    // Landing floor: the same expansion without repository presets and
    // roles, so they never lower a land tier or add a land that user
    // configuration and the command line did not set.
    let repo_roles = layers.roles.has_repo();
    let trusted = ((!layers.repo_files.is_empty() || repo_roles) && !layers.exclude_repo.get())
        .then(|| {
            layers.exclude_repo.set(true);
            let trusted = expand_with(layers, name, prompt, params, overrides);
            layers.exclude_repo.set(false);
            trusted
                .ok()
                .map(|expansion| expansion.document["nodes"].clone())
        });
    layers.used_shapes.borrow_mut().clear();
    layers.built_nodes.set(0);
    layers.review_bases.borrow_mut().clear();
    layers.used_roles.borrow_mut().clear();
    layers.substitutions.borrow_mut().clear();
    layers.warnings.borrow_mut().clear();
    layers.spec_contexts.borrow_mut().clear();
    layers.delegated_plan_stage.borrow_mut().take();
    let mut repo_files = Vec::new();
    let (mut config, mut origins) = layers.resolve_inner(name, &mut Vec::new(), &mut repo_files)?;
    for (key, value) in params {
        let path = format!("parameters.{key}");
        let valid = valid_paths(&config, layers);
        override_value(
            &mut config,
            &path,
            value.clone(),
            &mut origins,
            "--params-file",
        )
        .map_err(|error| format!("{error}; valid parameter and slot paths: {valid}"))?;
    }
    for item in overrides {
        let (path, parsed) = parse_preset_override(item)?;
        if parsed["value"].is_object() {
            return Err(format!(
                "{path}: --set requires a scalar or array value; set table fields with dotted paths, for example --set 'parameters.subject.commit=\"<sha>\"'"
            ));
        }
        let valid = valid_paths(&config, layers);
        override_value(
            &mut config,
            path,
            parsed["value"].clone(),
            &mut origins,
            "--set",
        )
        .map_err(|error| format!("{error}; valid parameter and slot paths: {valid}"))?;
    }
    let mut plan = None;
    let mut catalogue = None;
    if let Some(context) = &layers.plan {
        plan = planner::decide(layers, context, name, &config, &origins, prompt)?;
        if plan.as_ref().is_some_and(|decision| decision.applied) {
            *layers.delegated_plan_stage.borrow_mut() = config
                .pointer("/plan/delegate")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if plan.as_ref().is_some_and(|decision| decision.applied)
            && config.pointer("/plan/delegate").is_none()
        {
            apply_shape_stages(layers, name, &mut config, &origins, prompt)?;
            catalogue = Some(planner::apply(
                layers,
                context,
                name,
                &mut config,
                &origins,
                prompt,
            )?);
        }
    }
    if !plan.as_ref().is_some_and(|decision| decision.applied)
        || config.pointer("/plan/delegate").is_some()
    {
        apply_shape_stages(layers, name, &mut config, &origins, prompt)?;
    }
    let mut nodes = Vec::new();
    build(
        layers,
        name,
        &config,
        prompt,
        "",
        &[],
        &mut Vec::new(),
        &mut nodes,
        &mut origins,
        &mut repo_files,
    )?;
    fn has_deferred_planner(value: &Value) -> bool {
        match value {
            Value::Object(map) => {
                map.contains_key("review_planner") || map.values().any(has_deferred_planner)
            }
            Value::Array(items) => items.iter().any(has_deferred_planner),
            _ => false,
        }
    }
    if config.pointer("/plan/delegate").is_some()
        && !nodes.iter().any(has_deferred_planner)
        && let Some(decision) = plan.as_mut().filter(|decision| decision.applied)
    {
        let reason = "delegated review cannot be planned; using deterministic selection".to_owned();
        decision.applied = false;
        decision.reason = reason.clone();
        decision.degraded = Some(reason.clone());
        decision.warnings.push(reason);
    }
    if let Some(catalogue) = catalogue {
        planner::attach(&mut nodes, catalogue)?;
    }
    // Request bindings affect how inputs reach parameters, not the graph itself.
    origins.retain(|path, _| {
        !path.ends_with("$scope_read_only")
            && !path.split('.').any(|part| part == "shape_stages")
            && !((path.ends_with(".bind")
                || path.ends_with(".tracker_advice")
                || path.ends_with(".inherit")
                || path.ends_with(".inherit_explicit"))
                && (path.starts_with("parameters.") || path.contains(".parameters.")))
    });
    if let Some(trusted) = trusted
        && (!repo_files.is_empty() || repo_roles)
    {
        floor_land_tiers(&mut nodes, trusted.as_ref());
    }
    let digest = provenant_records::CanonicalV1::file_digest(
        &serde_json::to_vec(&nodes).map_err(|e| e.to_string())?,
    )
    .to_string();
    let id = format!("preset-{}", &digest[7..39]);
    let bases = layers.review_bases.take();
    if bases.len() > 1 {
        return Err(format!(
            "preset {name}: review stages disagree on the base ({}); a graph has one review base",
            bases.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    // The structural and schema checks operate on the typed graph, without
    // decoding a second copy of the fully assembled document.
    let mut typed = provenant_fabric_runtime::graph::GraphDocument {
        review_base: bases.into_iter().next(),
        context: Some(provenant_records::run::WorkContext {
            preset: Some(name.into()),
            pattern: Some(name.into()),
            ..Default::default()
        }),
        extra: Default::default(),
        limits: None,
        integration_branch: None,
        delivery: None,
        decision_session: None,
        schema_version: 1,
        graph_id: id,
        nodes: nodes
            .iter()
            .map(decode_lowered_node)
            .collect::<Result<_, _>>()?,
    };
    let parameters = flatten_parameters(
        name,
        config.get("parameters").unwrap_or(&json!({})),
        "",
        &origins,
    )?;
    for key in [
        "limits",
        "integration_branch",
        "delivery",
        "decision_session",
        "retry",
    ] {
        if let Some(value) = config.get(key) {
            let value = interpolate(value, &parameters, prompt, 1)?;
            // An empty knob, such as an unset decision session, means unset.
            if value != "" {
                match key {
                    "limits" => typed.limits = Some(value),
                    "integration_branch" => {
                        typed.integration_branch = serde_json::from_value(value)
                            .map_err(|error| format!("{key}: {error}"))?
                    }
                    "delivery" => {
                        typed.delivery = serde_json::from_value(value)
                            .map_err(|error| format!("{key}: {error}"))?
                    }
                    "decision_session" => {
                        typed.decision_session = serde_json::from_value(value)
                            .map_err(|error| format!("{key}: {error}"))?
                    }
                    _ => {
                        typed.extra.insert(key.into(), value);
                    }
                }
            }
        }
    }
    typed.check_composites(&layers.graph_limits)?;
    typed.schema_version = u64::from(provenant_fabric_runtime::graph::minimum_schema_version(
        &typed,
    ));
    let roles: BTreeMap<String, Value> = layers.used_roles.take().into_iter().collect();
    let substitutions = layers.substitutions.take();
    let mut warnings = layers.warnings.take();
    if let Some(why) = plan
        .as_ref()
        .and_then(|decision| decision.degraded.as_ref())
    {
        warnings.push(format!(
            "planner unavailable, running the preset's deterministic defaults: {why}"
        ));
    }
    if let Some(decision) = &plan {
        warnings.extend(decision.warnings.iter().cloned());
    }
    for (role, used) in &roles {
        origins.insert(
            format!("roles.{role}"),
            used["origin"].as_str().unwrap_or("").into(),
        );
    }
    // Provenance rides in the graph document so it is hashed into the graph
    // identity and retained with the run: where each value came from, which
    // roles (with digests) were used and any route substituted.
    let mut provenance = json!({
        "preset": name,
        "origins": origins,
        "roles": roles,
        "substitutions": substitutions,
        "warnings": warnings,
    });
    if let Some(decision) = &plan {
        provenance["plan"] = decision.to_json();
    }
    typed.extra.insert("provenance".into(), provenance);
    let mut document = serde_json::to_value(&typed).map_err(|error| error.to_string())?;
    let map = document.as_object_mut().ok_or("graph must be a table")?;
    // Retain explicit empty fields until node lowering itself is typed. The
    // graph's schema and document fields come from the typed value above.
    map.insert("nodes".into(), Value::Array(nodes));
    let document_bytes = serde_json::to_vec(&document)
        .map_err(|e| e.to_string())?
        .len();
    let document_bound = provenant_fabric_runtime::config::GraphLimits::document_bytes_bound();
    if let Some(reason) = document_bound.refusal(document_bytes) {
        return Err(format!(
            "expanded graph exceeds the {} MiB graph document limit: {reason}",
            document_bound.maximum / (1024 * 1024)
        ));
    }
    Ok(Expansion {
        review_shape: Some(
            layers
                .used_shapes
                .take()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", "),
        )
        .filter(|shapes| !shapes.is_empty()),
        document,
        worktree: parameters
            .get("worktree")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from),
        origins,
        repo_files,
        roles,
        substitutions,
        warnings,
        plan,
    })
}

const LAND_TIERS: [&str; 4] = ["routine", "substantial", "crucial", "terminal"];

/// Land nodes keyed by their composite path, with their tier rank.
fn land_tiers(nodes: &[Value], prefix: &str, tiers: &mut BTreeMap<String, usize>) {
    for node in nodes {
        let path = format!("{prefix}{}", node["id"].as_str().unwrap_or_default());
        if let Some(rank) = node
            .pointer("/land/tier")
            .and_then(Value::as_str)
            .and_then(|tier| LAND_TIERS.iter().position(|known| *known == tier))
        {
            tiers.insert(path.clone(), rank);
        }
        if let Some(children) = node.pointer("/composite/nodes").and_then(Value::as_array) {
            land_tiers(children, &format!("{path}/"), tiers);
        }
    }
}

/// Raise each land tier to the stricter of this expansion and `trusted`, the
/// expansion without repository layers. A land with no trusted counterpart,
/// or any land when no trusted expansion exists, takes the strictest tier.
fn floor_land_tiers(nodes: &mut [Value], trusted: Option<&Value>) {
    let mut floors = BTreeMap::new();
    if let Some(trusted) = trusted.and_then(Value::as_array) {
        land_tiers(trusted, "", &mut floors);
    }
    fn raise(nodes: &mut [Value], prefix: &str, floors: &BTreeMap<String, usize>) {
        for node in nodes {
            let path = format!("{prefix}{}", node["id"].as_str().unwrap_or_default());
            let floor = floors.get(&path).copied().unwrap_or(LAND_TIERS.len() - 1);
            if let Some(tier) = node.pointer_mut("/land/tier")
                && let Some(rank) = tier
                    .as_str()
                    .and_then(|tier| LAND_TIERS.iter().position(|known| *known == tier))
                && rank < floor
            {
                *tier = json!(LAND_TIERS[floor]);
            }
            if let Some(children) = node
                .pointer_mut("/composite/nodes")
                .and_then(Value::as_array_mut)
            {
                raise(children, &format!("{path}/"), floors);
            }
        }
    }
    raise(nodes, "", &floors);
}

#[allow(
    clippy::too_many_arguments,
    reason = "recursive expansion shares the bounded graph and origin accumulators"
)]
fn build(
    layers: &Layers,
    name: &str,
    config: &Value,
    prompt: &str,
    scope: &str,
    deps: &[String],
    stack: &mut Vec<String>,
    nodes: &mut Vec<Value>,
    origins: &mut BTreeMap<String, String>,
    repo_files: &mut Vec<String>,
) -> Result<Vec<String>, String> {
    if stack.iter().any(|v| v == name) {
        return Err(format!("preset cycle: {} -> {name}", stack.join(" -> ")));
    }
    if stack.len() >= DEPTH {
        return Err(format!("preset expansion depth exceeds {DEPTH}"));
    }
    stack.push(name.into());
    let result = if config.get("stages").is_some() {
        build_stages(
            layers, name, config, prompt, scope, deps, stack, nodes, origins, repo_files,
        )
    } else {
        Err(invalid_config(
            name,
            "stages",
            "missing stages; use extends",
            scope,
            origins,
        ))
    };
    if result.is_ok() && origins.contains_key("$scope_read_only") {
        if config.get("delivery").is_some() {
            return Err("scope preset refuses write-broadening graph delivery".into());
        }
        visit_nodes(nodes, &mut |node| {
            if node["access"] != "read_only"
                || node.get("write_scope").is_some()
                || node.get("delivery").is_some()
                || node.get("land").is_some()
                || node.get("kind").is_some_and(|kind| kind != "task")
            {
                return Err(format!(
                    "scope preset refuses write-broadening override on node {}",
                    node["id"]
                ));
            }
            Ok(())
        })?;
    }
    stack.pop();
    result
}

fn interpolate(
    value: &Value,
    parameters: &Value,
    prompt: &str,
    index: usize,
) -> Result<Value, String> {
    interpolate_inner(value, parameters, prompt, index, false)
}

/// The prompt of the first node in `nodes` that runs `role`, searching nested
/// composites, loops and plans.
pub(crate) fn find_prompt(nodes: &Value, role: &str) -> Option<String> {
    if let Some(items) = nodes.as_array() {
        for node in items {
            if let Some(found) = find_prompt(node, role) {
                return Some(found);
            }
        }
    } else if let Some(map) = nodes.as_object() {
        if map.get("role").and_then(Value::as_str) == Some(role)
            && let Some(prompt) = map.get("prompt").and_then(Value::as_str)
        {
            return Some(prompt.to_string());
        }
        for (key, val) in map {
            if matches!(
                key.as_str(),
                "composite" | "loop" | "plan" | "patterns" | "nodes" | "body"
            ) && let Some(found) = find_prompt(val, role)
            {
                return Some(found);
            }
        }
    }
    None
}

fn interpolate_inner(
    value: &Value,
    parameters: &Value,
    prompt: &str,
    index: usize,
    defer_index: bool,
) -> Result<Value, String> {
    match value {
        Value::String(text) => {
            let lookup = |name: &str| match name {
                "prompt" => Ok(json!(prompt)),
                "index" if defer_index => Ok(json!("${index}")),
                "index" => Ok(json!(index)),
                _ => {
                    let mut parts = name.split('.');
                    let first = parts.next().unwrap_or_default();
                    parts
                        .try_fold(parameters.get(first), |value, part| {
                            value.map(|value| value.get(part))
                        })
                        .flatten()
                        .cloned()
                        .ok_or_else(|| format!("unknown preset parameter: {name}"))
                }
            };
            if let Some(name) = text
                .strip_prefix("${")
                .and_then(|rest| rest.strip_suffix('}'))
                && !name.contains(['{', '}'])
            {
                return lookup(name);
            }
            let mut remaining = text.as_str();
            let mut rendered = String::new();
            while let Some((before, rest)) = remaining.split_once("${") {
                rendered.push_str(before);
                let (name, after) = rest
                    .split_once('}')
                    .ok_or("unclosed preset parameter interpolation")?;
                match lookup(name)? {
                    Value::String(text) => rendered.push_str(&text),
                    value @ (Value::Number(_) | Value::Bool(_)) => {
                        rendered.push_str(&value.to_string())
                    }
                    _ => {
                        return Err(format!(
                            "parameter {name} must be scalar when embedded in text"
                        ));
                    }
                }
                remaining = after;
            }
            rendered.push_str(remaining);
            Ok(json!(rendered))
        }
        Value::Array(values) => values
            .iter()
            .map(|value| interpolate_inner(value, parameters, prompt, index, defer_index))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| {
                Ok((
                    key.clone(),
                    interpolate_inner(value, parameters, prompt, index, defer_index)?,
                ))
            })
            .collect::<Result<serde_json::Map<_, _>, String>>()
            .map(Value::Object),
        value => Ok(value.clone()),
    }
}

fn selected_inputs(deps: &[String], outputs: &Value) -> Value {
    json!(
        deps.iter()
            .map(|node| json!({"node":node,"outputs":outputs}))
            .collect::<Vec<_>>()
    )
}

fn spec_context(layers: &Layers, parameters: &Value) -> Option<String> {
    let key = json!([
        parameters.get("spec"),
        parameters.get("worktree"),
        parameters.get("spec_max_bytes")
    ])
    .to_string();
    if let Some(context) = layers.spec_contexts.borrow().get(&key) {
        return context.clone();
    }
    let context = read_spec_context(layers, parameters);
    layers
        .spec_contexts
        .borrow_mut()
        .insert(key, context.clone());
    context
}

fn spec_candidates(layers: &Layers, parameters: &Value, spec: &str) -> Option<Vec<PathBuf>> {
    let project = layers
        .project_dir
        .clone()
        .or_else(|| std::env::current_dir().ok())?;
    let worktree = parameters
        .get("worktree")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| layers.default_worktree.clone());
    let path = Path::new(spec);
    let launch = layers.launch_dir.as_deref().unwrap_or(&project);
    let mut candidates: Vec<_> = worktree
        .into_iter()
        .map(|w| launch.join(w).join(path))
        .chain(std::iter::once(project.join(path)))
        .collect();
    let discovered;
    let worktrees = match layers.command_worktrees.as_deref() {
        Some(worktrees) => worktrees,
        None => {
            discovered = project_worktrees(&project);
            &discovered
        }
    };
    if let Some(main) = worktrees.first() {
        let candidate = main.join(path);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    Some(candidates)
}

fn advice_spec_path(layers: &Layers, parameters: &Value, spec: &str) -> Option<PathBuf> {
    let candidates = spec_candidates(layers, parameters, spec.trim())?;
    candidates
        .iter()
        .find(|path| std::fs::read_to_string(path).is_ok())
        .or_else(|| candidates.first())
        .cloned()
}

fn read_spec_context(layers: &Layers, parameters: &Value) -> Option<String> {
    use sha2::{Digest, Sha256};
    let spec = parameters.get("spec")?.as_str()?.trim();
    if spec.is_empty()
        || spec
            .trim_start_matches('#')
            .bytes()
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let cap = parameters.get("spec_max_bytes")?.as_u64()? as usize;
    let candidates = spec_candidates(layers, parameters, spec)?;
    let mut errors = Vec::new();
    for candidate in candidates {
        let error = match std::fs::File::open(&candidate) {
            Ok(mut file) => {
                let mut hash = Sha256::new();
                let mut excerpt = Vec::new();
                let mut buffer = [0u8; 8192];
                let result = loop {
                    match file.read(&mut buffer) {
                        Ok(0) => break Ok(()),
                        Ok(n) => {
                            hash.update(&buffer[..n]);
                            let take = n.min(cap.saturating_sub(excerpt.len()));
                            excerpt.extend_from_slice(&buffer[..take]);
                        }
                        Err(e) => break Err(e),
                    }
                };
                if let Err(error) = result {
                    error
                } else {
                    let digest: String = hash
                        .finalize()
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect();
                    let mut text = String::from_utf8_lossy(&excerpt).into_owned();
                    if text.len() > cap {
                        let mut end = cap;
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        text.truncate(end);
                    }
                    return Some(format!(
                        "\n\nSpec file: {} (sha256:{digest}; excerpt up to {cap} bytes)\n{text}",
                        candidate.display()
                    ));
                }
            }
            Err(e) => e,
        };
        errors.push(format!("{}: {error}", candidate.display()));
    }
    let warning = format!(
        "spec file '{spec}' could not be read: {}; pass an issue-number spec instead",
        errors.join("; ")
    );
    let mut warnings = layers.warnings.borrow_mut();
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
    None
}

#[allow(
    clippy::too_many_arguments,
    reason = "leaf lowering shares the preset's layers, roles and parameters"
)]
fn lower_leaf(
    layers: &Layers,
    entry: &serde_json::Map<String, Value>,
    roles: Option<&Value>,
    parameters: &Value,
    prompt: &str,
    index: usize,
    id: &str,
    deps: &[String],
) -> Result<Value, String> {
    let mut profile = json!({});
    let role = entry
        .get("role")
        .map(|value| value.as_str().ok_or("role must be a name"))
        .transpose()?;
    // A role name may interpolate, for example `lens.${item.name}`.
    let role = match role {
        Some(name) if name.contains("${") => Some(
            interpolate(&json!(name), parameters, prompt, index)?
                .as_str()
                .ok_or("role must be a name")?
                .to_owned(),
        ),
        other => other.map(str::to_owned),
    };
    let role = role.as_deref();
    let mut resolved_role = None;
    if let Some(role) = role {
        let resolved = layers
            .roles
            .resolve(role, roles, layers.exclude_repo.get())?;
        profile = resolved.profile.clone();
        resolved_role = Some(resolved);
    }
    // Source bindings are a tagged union. An explicit stage source replaces
    // the role's binding rather than inheriting fields from another variant.
    if entry.contains_key("source")
        && let Some(map) = profile.as_object_mut()
    {
        map.remove("source");
    }
    let mut entry = entry.clone();
    // An unset writer parameter inherits the role, including project overrides.
    if entry.get("tier").and_then(Value::as_str) == Some("${writer}")
        && parameters.get("writer").and_then(Value::as_str) == Some("")
    {
        entry.remove("tier");
    }
    merge(
        &mut profile,
        &Value::Object(entry),
        "",
        "entry",
        &mut BTreeMap::new(),
    )?;
    // Conditional values belong to the role or stage that uses them. Explicit
    // parameters win, just as they did for ordinary interpolation.
    let mut effective_parameters = parameters.clone();
    if let Some(rules) = profile
        .as_object_mut()
        .and_then(|map| map.remove("conditional_parameters"))
    {
        let rules = rules
            .as_object()
            .ok_or_else(|| format!("node {id}: conditional_parameters must be a table"))?;
        let target = effective_parameters
            .as_object_mut()
            .ok_or("parameters must be a table")?;
        for (key, rule) in rules {
            let rule = rule
                .as_object()
                .ok_or_else(|| format!("node {id}: conditional parameter {key} must be a table"))?;
            if rule.keys().any(|key| {
                !["parameter", "equals_any", "value", "otherwise"].contains(&key.as_str())
            }) {
                return Err(format!(
                    "node {id}: conditional parameter {key} has an unknown field"
                ));
            }
            let source = rule
                .get("parameter")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("node {id}: conditional parameter {key} needs parameter"))?;
            let choices = rule
                .get("equals_any")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    format!("node {id}: conditional parameter {key} needs equals_any")
                })?;
            let value = rule
                .get("value")
                .ok_or_else(|| format!("node {id}: conditional parameter {key} needs value"))?;
            let otherwise = rule
                .get("otherwise")
                .ok_or_else(|| format!("node {id}: conditional parameter {key} needs otherwise"))?;
            let selected = if target
                .get(source)
                .is_some_and(|actual| choices.contains(actual))
            {
                value
            } else {
                otherwise
            };
            if !target.contains_key(key) {
                target.insert(key.clone(), selected.clone());
            }
        }
    }
    let mut node = interpolate(&profile, &effective_parameters, prompt, index)?;
    let map = node.as_object_mut().ok_or("leaf must be a table")?;
    // Compiler-only fields are consumed below; GraphNode owns the remaining
    // field vocabulary. Its flattened extras are retained for graph records,
    // but a preset must not introduce an unknown node field through them.
    let mut probe = map.clone();
    for key in [
        "tier",
        "effort",
        "outputs",
        "prompt_file",
        "card",
        "references",
        "family",
        "family_strength",
        "fallback_tier",
        "purpose",
        "retry",
    ] {
        probe.remove(key);
    }
    probe.insert("id".into(), json!(id));
    probe.entry("access").or_insert_with(|| json!("read_only"));
    let unknown = match serde_json::from_value::<provenant_fabric_runtime::graph::GraphNode>(
        Value::Object(probe.clone()),
    ) {
        Ok(node) => {
            if let Some(error) = unknown_embedded_node_field(&node) {
                return Err(error);
            }
            node.extra.keys().next().cloned()
        }
        Err(_) => {
            if let Some(error) =
                unknown_embedded_node_in_invalid_value(&Value::Object(probe.clone()))
            {
                return Err(error);
            }
            probe.iter().find_map(|(key, value)| {
                if key == "id" || key == "access" {
                    return None;
                }
                let candidate = json!({"id": id, "access": "read_only", (key): value});
                serde_json::from_value::<provenant_fabric_runtime::graph::GraphNode>(candidate)
                    .ok()
                    .and_then(|node| node.extra.contains_key(key).then(|| key.clone()))
            })
        }
    };
    if let Some(key) = unknown {
        return Err(format!("node {id}: unknown leaf field: {key}"));
    }
    for alias in ["goal", "brief"] {
        if let Some(value) = map.remove(alias) {
            if map.contains_key("prompt") {
                return Err(format!(
                    "node {id}: prompt and {alias} are mutually exclusive"
                ));
            }
            map.insert("prompt".into(), value);
        }
    }
    map.insert("id".into(), json!(id));
    // Carry the preset's diff reference to read-only graph nodes.
    if map.get("access").and_then(Value::as_str) == Some("read_only")
        && let Some(base) = parameters
            .get("base")
            .and_then(Value::as_str)
            .filter(|base| !base.is_empty())
    {
        map.insert("review_base".into(), json!(base));
    }
    // An empty land session, such as an unset decision session, means unset.
    if let Some(land) = map.get_mut("land").and_then(Value::as_object_mut)
        && land.get("session").is_some_and(|session| session == "")
    {
        land.remove("session");
    }
    // The brief: card, then the task, then reference text. References are
    // resolved against the release root and their digests recorded.
    let mut references = Vec::new();
    if let Some(list) = map.remove("references") {
        for item in list.as_array().ok_or("references must be an array")? {
            let (name, when, inline) = match item {
                Value::String(name) => (name.as_str(), "", false),
                Value::Object(table) => (
                    table
                        .get("ref")
                        .and_then(Value::as_str)
                        .ok_or("each reference needs a ref = \"skill:...\"")?,
                    table
                        .get("when")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    table
                        .get("inline")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ),
                _ => return Err("references entries must be tables or names".into()),
            };
            if name.is_empty() {
                continue;
            }
            references.push((layers.roles.reference(name)?, when.to_owned(), inline));
        }
    }
    if let Some(files) = map.remove("prompt_file") {
        let names: Vec<&str> = match &files {
            Value::String(name) => vec![name.as_str()],
            Value::Array(names) => names
                .iter()
                .map(|name| name.as_str().ok_or("prompt_file entries must be names"))
                .collect::<Result<_, _>>()?,
            _ => return Err("prompt_file must be a name or a list of names".into()),
        };
        for name in names.into_iter().filter(|name| !name.is_empty()) {
            references.push((
                layers.roles.reference(&prompt_file_reference(name)?)?,
                String::new(),
                true,
            ));
        }
    }
    let card = map
        .remove("card")
        .map(|card| card.as_str().map(str::to_owned).ok_or("card must be text"))
        .transpose()?;
    let family = map.remove("family");
    let family_strength = map.remove("family_strength");
    map.remove("purpose");
    let fallback_tier = map.remove("fallback_tier");
    if card.is_some() || !references.is_empty() {
        let task = map
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or(prompt)
            .to_owned();
        let mut brief = String::new();
        if let Some(card) = &card {
            brief.push_str(card);
            brief.push_str("\n\n");
        }
        brief.push_str(&task);
        let listed: Vec<String> = references
            .iter()
            .filter(|(reference, _, inline)| !inline && reference.path.is_some())
            .map(|(reference, when, _)| {
                let location = reference
                    .path
                    .as_ref()
                    .map_or(String::new(), |path| format!(" ({})", path.display()));
                let when = if when.is_empty() {
                    String::new()
                } else {
                    format!(": {when}")
                };
                format!("- {}{location}{when}", reference.name)
            })
            .collect();
        if !listed.is_empty() {
            brief.push_str("\n\nReferences to read when relevant:\n");
            brief.push_str(&listed.join("\n"));
        }
        for (reference, _, _) in references
            .iter()
            .filter(|(reference, _, inline)| *inline || reference.path.is_none())
        {
            brief.push_str("\n\n");
            brief.push_str(&reference.text);
        }
        map.insert("prompt".into(), json!(brief));
    }
    if role.is_some_and(|role| {
        role.starts_with("lens.") || role.starts_with("review.") || role == "plan.review"
    }) && let Some(context) = spec_context(layers, parameters)
    {
        let brief = map.get("prompt").and_then(Value::as_str).unwrap_or(prompt);
        map.insert("prompt".into(), json!(format!("{brief}{context}")));
    }
    match family.as_ref().and_then(Value::as_str) {
        None | Some("" | "any") => {}
        Some("cross_preferred" | "cross_required") => {
            // The scheduler excludes the families of every upstream attempt.
            // `cross_required` fails the attempt rather than substitute one.
            map.entry("distinct_from")
                .or_insert_with(|| json!("dependencies"));
            if family.as_ref().and_then(Value::as_str) == Some("cross_required") {
                map.entry("distinct_strength")
                    .or_insert_with(|| json!("require"));
            }
        }
        Some("distinct_from_writer") => {
            map.entry("distinct_from")
                .or_insert_with(|| json!("writer"));
        }
        Some(other) => {
            return Err(format!(
                "node {id}: unknown family constraint {other}; use any, cross_preferred, cross_required or distinct_from_writer"
            ));
        }
    }
    match family_strength.as_ref().and_then(Value::as_str) {
        None | Some("") => {}
        Some(strength @ ("prefer" | "require")) => {
            map.insert("distinct_strength".into(), json!(strength));
        }
        Some(other) => {
            return Err(format!(
                "node {id}: unknown family_strength {other}; use prefer or require"
            ));
        }
    }
    if let (Some(role), Some(resolved)) = (role, &resolved_role) {
        let recorded: Vec<Value> = references
            .iter()
            .map(|(reference, when, _)| {
                json!({"ref": reference.name, "when": when, "digest": reference.digest})
            })
            .collect();
        // The whole effective profile (prompt, tier, access, kind, outputs,
        // inline flags and the rest) plus the content digests of references.
        let digest = provenant_records::CanonicalV1::file_digest(
            &serde_json::to_vec(&json!({
                "role": role,
                "chain": resolved.chain,
                "profile": resolved.profile,
                "references": recorded,
            }))
            .map_err(|e| e.to_string())?,
        )
        .to_string();
        map.insert("role_digest".into(), json!(digest));
        if !recorded.is_empty() {
            map.insert("references".into(), json!(recorded));
        }
        layers.used_roles.borrow_mut().insert(
            role.to_owned(),
            json!({
                "origin": resolved.origin,
                "extends": resolved.chain,
                "digest": digest,
                "references": recorded,
            }),
        );
    }
    if let Some(requested) = map.get("tier").and_then(Value::as_str).map(str::to_owned)
        && let Some(fallback) = fallback_tier.as_ref().and_then(Value::as_str)
        && let Some(routes) = &layers.known_routes
        && !requested.contains('/')
        && !routes.contains(&requested)
        && requested != fallback
    {
        map.insert("tier".into(), json!(fallback));
        layers.substitutions.borrow_mut().push(json!({
            "node": id,
            "requested": requested,
            "used": fallback,
            "reason": format!("route {requested} is not defined in the routing configuration"),
        }));
    }
    if map.get("distinct_from").is_some_and(|value| value == "") {
        map.remove("distinct_from");
    }
    if let Some(value) = map.get("distinct_from")
        && value != "writer"
        && value != "dependencies"
    {
        return Err(format!(
            "node {id}: distinct_from must be \"writer\" or \"dependencies\""
        ));
    }
    let mut dependencies = deps.to_vec();
    if let Some(explicit) = map.remove("depends_on") {
        for dependency in explicit.as_array().ok_or("depends_on must be an array")? {
            let dependency = dependency
                .as_str()
                .ok_or("dependencies must be node names")?;
            if !dependencies.iter().any(|name| name == dependency) {
                dependencies.push(dependency.into());
            }
        }
    }
    let input_dependencies = dependencies.clone();
    if let Some(source) = map
        .get("source")
        .filter(|source| source["type"] == "node")
        .and_then(|source| source["node"].as_str())
        && !dependencies.iter().any(|name| name == source)
    {
        dependencies.push(source.into());
    }
    map.insert("depends_on".into(), json!(dependencies));
    map.entry("access").or_insert_with(|| json!("read_only"));
    map.entry("title").or_insert_with(|| json!(id));
    let control = matches!(
        map.get("kind").and_then(Value::as_str),
        Some("gate" | "loop" | "composite" | "land")
    );
    if !control {
        map.entry("prompt").or_insert_with(|| json!(prompt));
        let tier = map.remove("tier");
        let effort = map.remove("effort");
        if tier.is_some() && (map.contains_key("route") || map.contains_key("routes")) {
            return Err(format!("node {id}: tier and routes are mutually exclusive"));
        }
        if map.contains_key("route") && map.contains_key("routes") {
            return Err(format!(
                "node {id}: route and routes are mutually exclusive"
            ));
        }
        if map.contains_key("route") || map.contains_key("routes") {
            if effort.is_some() {
                return Err(format!("node {id}: routes cannot be combined with effort"));
            }
        } else {
            let route = tier.unwrap_or_else(|| {
                parameters
                    .get("route")
                    .cloned()
                    .unwrap_or_else(|| json!("senior"))
            });
            let read_only = map.get("access").is_none_or(|access| access == "read_only");
            let (key, value) = tier_route(id, &route, effort.as_ref(), read_only)?;
            map.insert(key.into(), value);
        }
    } else {
        map.remove("tier");
        map.remove("effort");
    }
    let outputs = map.remove("outputs").unwrap_or_else(|| json!(["result"]));
    if !control
        && (!deps.is_empty() || (map.contains_key("dispositions") && !dependencies.is_empty()))
    {
        if !map.contains_key("inputs") {
            map.insert(
                "inputs".into(),
                selected_inputs(&input_dependencies, &outputs),
            );
        }
        if map.contains_key("dispositions") {
            let inputs = map
                .get_mut("inputs")
                .and_then(Value::as_array_mut)
                .ok_or("inputs must be an array")?;
            for input in inputs.iter_mut() {
                let outputs = input
                    .get_mut("outputs")
                    .and_then(Value::as_array_mut)
                    .ok_or("input outputs must be an array")?;
                if !outputs.iter().any(|output| output == "findings") {
                    outputs.push(json!("findings"));
                }
            }
            for dep in &dependencies {
                if !inputs
                    .iter()
                    .any(|input| input.get("node").and_then(Value::as_str) == Some(dep))
                {
                    inputs.push(json!({"node": dep, "outputs": ["findings"]}));
                }
            }
        }
    }
    if map.get("worktree").is_some_and(|value| value == "") {
        map.remove("worktree");
    }
    if map
        .get("access")
        .is_some_and(|access| access == "read_only")
        && let Some(worktree) = map.remove("worktree")
    {
        let roots = map
            .entry("read_roots")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("read_roots must be an array")?;
        if !roots.contains(&worktree) {
            roots.push(worktree);
        }
    }
    if !control {
        let mut contract = String::new();
        for (key, label) in [
            ("write_scope", "Write scope (relative literal paths)"),
            ("acceptance_checks", "Acceptance checks"),
        ] {
            if let Some(values) = map.get(key) {
                let values = values
                    .as_array()
                    .ok_or_else(|| format!("{key} must be an array"))?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .ok_or_else(|| format!("{key} entries must be strings"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if !values.is_empty() {
                    contract.push_str(&format!("\n\n{label}:\n- {}", values.join("\n- ")));
                }
            }
        }
        if !contract.is_empty() {
            let brief = map
                .get("prompt")
                .and_then(Value::as_str)
                .ok_or("prompt must be a string")?;
            map.insert("prompt".into(), json!(format!("{brief}{contract}")));
        }
    }
    Ok(node)
}

/// Lower a tier (a pool name, an exact `ADAPTER/ID[@effort]`, a route table or
/// a route list) to the `route` or `routes` entry a node carries. A read-only
/// pool name repeats once, so a usage-limited first member falls back to the
/// next resolution. Writers keep one entry. A route table that names neither
/// an alias nor an adapter and model is refused rather than dropped.
pub(crate) fn tier_route(
    id: &str,
    tier: &Value,
    effort: Option<&Value>,
    read_only: bool,
) -> Result<(&'static str, Value), String> {
    if tier.is_array() {
        if effort.is_some() {
            return Err(format!(
                "node {id}: route lists cannot be combined with effort"
            ));
        }
        return Ok(("routes", tier.clone()));
    }
    let mut route = match tier {
        Value::String(name) if !name.is_empty() => {
            if let Some((adapter, model)) = name.split_once('/') {
                let (model, embedded_effort) = model
                    .split_once('@')
                    .map_or((model, None), |(model, effort)| (model, Some(effort)));
                if adapter.is_empty() || model.is_empty() || embedded_effort == Some("") {
                    return Err(format!("node {id}: invalid exact-model tier {name}"));
                }
                if embedded_effort.is_some() && effort.is_some() {
                    return Err(format!(
                        "node {id}: exact-model tier and effort both specify effort"
                    ));
                }
                let mut route = json!({"adapter":adapter,"model":model});
                if let Some(embedded_effort) = embedded_effort {
                    route["effort"] = json!(embedded_effort);
                }
                route
            } else {
                json!({"alias":name})
            }
        }
        Value::Object(table) => {
            let named = |key: &str| table.get(key).is_some_and(|value| value.is_string());
            if !named("alias") && !(named("adapter") && named("model")) {
                return Err(format!(
                    "node {id}: route table needs an alias, or an adapter and a model"
                ));
            }
            tier.clone()
        }
        _ => {
            return Err(format!(
                "node {id}: tier must be a name, route table or route list"
            ));
        }
    };
    if let Some(effort) = effort {
        route["effort"] = effort.clone();
    }
    let alias_only = route.get("alias").is_some() && route.get("model").is_none();
    if read_only && alias_only {
        Ok(("routes", json!([route.clone(), route])))
    } else {
        Ok(("route", route))
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "preset compilation shares bounded expansion state"
)]
fn build_stages(
    layers: &Layers,
    name: &str,
    config: &Value,
    prompt: &str,
    scope: &str,
    deps: &[String],
    stack: &mut Vec<String>,
    nodes: &mut Vec<Value>,
    origins: &mut BTreeMap<String, String>,
    repo_files: &mut Vec<String>,
) -> Result<Vec<String>, String> {
    if let Some(key) = config.as_object().and_then(|map| {
        map.keys().find(|key| {
            ![
                "stages",
                "parameters",
                "roles",
                "exports",
                "limits",
                "integration_branch",
                "delivery",
                "decision_session",
                "retry",
                "pattern",
                "plan",
                "shape_stages",
            ]
            .contains(&key.as_str())
        })
    }) {
        return Err(invalid_config(name, key, "unknown field", scope, origins));
    }
    check_pattern(name, config.get("pattern"), scope, origins)?;
    let mut parameters = flatten_parameters(
        name,
        config.get("parameters").unwrap_or(&json!({})),
        scope,
        origins,
    )?;
    let (policy_origin, policy_text) = layers.local_check_policy(&parameters, origins)?;
    parameters["local_check_policy"] = json!(policy_text);
    origins.insert("parameters.local_check_policy".into(), policy_origin);
    let mut builder = StageBuilder {
        layers,
        name,
        prompt,
        parameters,
        roles: config.get("roles"),
        stack,
        origins,
        repo_files,
        scope,
    };
    let start = nodes.len();
    let mut stage_entries = config.get("stages").cloned().ok_or("missing stages")?;
    let explicit_verifiers = builder
        .origins
        .get("parameters.verifiers")
        .is_some_and(|origin| !builder.layers.is_initial_origin(name, "verifiers", origin));
    let verifier_override = explicit_verifiers
        .then(|| builder.parameters.get("verifiers").and_then(Value::as_u64))
        .flatten();
    let deferred_shape = builder.defers_review_size();
    if deferred_shape
        && let Some(verifier) = config
            .pointer("/shape_stages/verifier")
            .and_then(Value::as_str)
        && let Some(stage) = stage_entries
            .as_array_mut()
            .and_then(|stages| stages.iter_mut().find(|stage| stage["id"] == verifier))
    {
        stage["count"] = json!(1);
    }
    let mut ends = builder.sequence(&stage_entries, "", deps, nodes, 0)?;
    if let Some(stages) = config.get("shape_stages").and_then(Value::as_object) {
        let id = |key: &str| stages.get(key).and_then(Value::as_str).unwrap_or("");
        let (frame, panel, verifier, synthesis) =
            (id("frame"), id("panel"), id("verifier"), id("synthesis"));
        if let Some(position) = nodes[start..]
            .iter()
            .position(|node| node["id"] == panel && node.get("review").is_some())
            .map(|position| start + position)
        {
            let frame_node = nodes[start..]
                .iter()
                .find(|node| node["id"] == frame)
                .cloned()
                .ok_or("deferred review frame missing")?;
            let verifier_node = nodes[start..]
                .iter()
                .find(|node| node["id"] == verifier)
                .cloned();
            let synthesis_node = nodes[start..]
                .iter()
                .find(|node| node["id"] == synthesis)
                .cloned()
                .ok_or("deferred review synthesis missing")?;
            let node = &mut nodes[position];
            node["panel_templates"] = json!({"frame": frame_node, "verifier": verifier_node, "synthesis": synthesis_node});
            let mut roots = node
                .get("read_roots")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for template in [
                Some(&frame_node),
                verifier_node.as_ref(),
                Some(&synthesis_node),
            ]
            .into_iter()
            .flatten()
            {
                for root in template
                    .get("read_roots")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if !roots.contains(root) {
                        roots.push(root.clone());
                    }
                }
            }
            if !roots.is_empty() {
                node["read_roots"] = json!(roots);
            }
            if let Some(count) = verifier_override {
                node["panel_verifiers"] = json!(count);
            }
            for key in ["source", "depends_on", "inputs"] {
                if let Some(value) = frame_node.get(key) {
                    node[key] = value.clone();
                } else {
                    node.as_object_mut()
                        .ok_or("deferred review must be a table")?
                        .remove(key);
                }
            }
            // The original bound covers the lens group and its candidates.
            // The outer boundary and all possible shape stages also count.
            let hint = builder
                .shape_hint(true)?
                .ok_or("deferred review needs shape configuration")?;
            node["panel_shapes"] = json!(hint.panel_shapes);
            node["panel_max_verifiers"] = json!(
                hint.max_verifiers
                    .max(verifier_override.unwrap_or(0) as usize)
            );
            let stages = hint
                .max_panel_stages
                .max(verifier_override.unwrap_or(0) as usize + 2);
            if let Some(max) = node["review"]["max_nodes"].as_u64() {
                node["review"]["max_nodes"] =
                    json!(max.saturating_add(stages as u64).saturating_add(1));
            }
            let children = nodes.split_off(start);
            nodes.extend(children.into_iter().filter(|node| {
                node["id"] != frame && node["id"] != verifier && node["id"] != synthesis
            }));
            ends = vec![panel.to_owned()];
        }
    }
    if let Some(retry) = config.get("retry") {
        let retry = interpolate(retry, &builder.parameters, prompt, 1)?;
        inherit_preset_retry(&mut nodes[start..], &retry);
    }
    Ok(ends)
}

/// Apply a resolved review shape to the stage names declared by a preset.
/// Writer-sourced subjects are left for launch-time resolution.
fn apply_shape_stages(
    layers: &Layers,
    name: &str,
    config: &mut Value,
    origins: &BTreeMap<String, String>,
    prompt: &str,
) -> Result<(), String> {
    let Some(declaration) = config.get("shape_stages").cloned() else {
        return Ok(());
    };
    let declaration = declaration
        .as_object()
        .ok_or("shape_stages must be a table")?;
    if declaration
        .keys()
        .any(|key| !["frame", "panel", "verifier", "synthesis"].contains(&key.as_str()))
    {
        return Err("shape_stages has an unknown field".into());
    }
    let stage = |key: &str| -> Result<String, String> {
        declaration
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("shape_stages needs {key}"))
    };
    let frame = stage("frame")?;
    let panel = stage("panel")?;
    let verifier = stage("verifier")?;
    let synthesis = stage("synthesis")?;
    if [&frame, &panel, &verifier, &synthesis]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != 4
    {
        return Err("shape_stages must name distinct stages".into());
    }
    let parameters = flatten_parameters(
        name,
        config.get("parameters").unwrap_or(&json!({})),
        "",
        origins,
    )?;
    let subject = parameters
        .get("subject")
        .cloned()
        .ok_or("shape_stages needs a subject")?;
    if layers.review.is_some()
        && matches!(
            subject.get("type").and_then(Value::as_str),
            Some("head" | "node")
        )
    {
        config["exports"] =
            json!([{ "node": panel, "outputs": ["result", "findings", "verdict", "commit"] }]);
        return Ok(());
    }
    let declared_verifiers = parameters
        .get("verifiers")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let (mut stack, mut local, mut files) = (Vec::new(), origins.clone(), Vec::new());
    let builder = StageBuilder {
        layers,
        name,
        prompt,
        parameters,
        roles: config.get("roles"),
        stack: &mut stack,
        origins: &mut local,
        repo_files: &mut files,
        scope: "",
    };
    let Some(shape) = builder.shape_hint(false)? else {
        return Ok(());
    };
    let explicit_verifiers = origins
        .get("parameters.verifiers")
        .is_some_and(|origin| !layers.is_initial_origin(name, "verifiers", origin));
    let stages = config
        .get_mut("stages")
        .and_then(Value::as_array_mut)
        .ok_or("shape_stages needs stages")?;
    for id in [&frame, &panel, &verifier, &synthesis] {
        if stages
            .iter()
            .filter(|stage| stage.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .count()
            != 1
        {
            return Err(format!("shape_stages needs exactly one stage {id}"));
        }
    }
    if !shape.frame {
        stages.retain(|stage| stage.get("id").and_then(Value::as_str) != Some(frame.as_str()));
        for id in [&panel, &verifier, &synthesis] {
            if let Some(stage) = stages
                .iter_mut()
                .find(|stage| stage.get("id").and_then(Value::as_str) == Some(id.as_str()))
            {
                stage["source"] = subject.clone();
            }
        }
    }
    if !explicit_verifiers
        && let Some(stage) = stages
            .iter_mut()
            .find(|stage| stage.get("id").and_then(Value::as_str) == Some(verifier.as_str()))
    {
        stage["count"] = json!(shape.verifiers);
    }
    if shape.synthesis == provenant_routing::ReviewSynthesis::Reduce {
        stages.retain(|stage| stage.get("id").and_then(Value::as_str) != Some(synthesis.as_str()));
        let stage = stages
            .iter_mut()
            .find(|stage| stage.get("id").and_then(Value::as_str) == Some(panel.as_str()))
            .ok_or("missing review panel")?;
        stage["reduce_verdict"] = json!(true);
        stage["export_first"] = json!(["commit"]);
        let mut exports =
            vec![json!({ "node": panel, "outputs": ["result", "findings", "verdict", "commit"] })];
        let verifier_count = if explicit_verifiers {
            declared_verifiers
        } else {
            shape.verifiers as u64
        };
        if verifier_count > 0 {
            let id = if verifier_count == 1 {
                verifier.clone()
            } else {
                format!("{verifier}-{verifier_count}")
            };
            exports.push(json!({"node": id, "outputs": ["result", "findings"]}));
        }
        config["exports"] = json!(exports);
    }
    Ok(())
}

// Preserve the nearest preset default on lowered leaves, including composite
// and loop bodies. Explicit node retry settings retain their own origin.
fn inherit_preset_retry(nodes: &mut [Value], retry: &Value) {
    for node in nodes {
        if node.get("preset_retry").is_none() {
            node["preset_retry"] = retry.clone();
        }
        for pointer in [
            "/composite/nodes",
            "/loop/body",
            "/fallback_stage/nodes",
            "/plan/fallback/nodes",
        ] {
            if let Some(children) = node.pointer_mut(pointer).and_then(Value::as_array_mut) {
                inherit_preset_retry(children, retry);
            }
        }
    }
}

struct StageBuilder<'a> {
    layers: &'a Layers,
    name: &'a str,
    prompt: &'a str,
    parameters: Value,
    roles: Option<&'a Value>,
    stack: &'a mut Vec<String>,
    origins: &'a mut BTreeMap<String, String>,
    repo_files: &'a mut Vec<String>,
    /// Stage path of this preset's caller, for diagnostics.
    scope: &'a str,
}

impl StageBuilder<'_> {
    /// Members that must succeed. Zero takes the configured review ladder's
    /// minimum for `risk_tier`, and never less than one member.
    fn minimum_successful(&self, value: &Value) -> Result<u64, String> {
        let minimum = interpolate(value, &self.parameters, self.prompt, 1)?
            .as_u64()
            .ok_or("min_successful must be a non-negative integer")?;
        if minimum > 0 {
            return Ok(minimum);
        }
        let ladder = provenant_records::workflow::ReviewPolicy::default();
        let tier = self.parameters.get("risk_tier").and_then(Value::as_str);
        Ok(match tier {
            Some("substantial") => ladder.substantial,
            Some("crucial") => ladder.crucial,
            Some("terminal") => ladder.terminal,
            _ => ladder.routine,
        }
        .max(1))
    }

    fn sequence(
        &mut self,
        stages: &Value,
        prefix: &str,
        deps: &[String],
        nodes: &mut Vec<Value>,
        depth: usize,
    ) -> Result<Vec<String>, String> {
        let stages = stages
            .as_array()
            .filter(|stages| !stages.is_empty())
            .ok_or("stages must be a nonempty array")?;
        let mut incoming = deps.to_vec();
        for (position, entry) in stages.iter().enumerate() {
            incoming = self.entry(
                entry,
                &format!("{prefix}s{}", position + 1),
                &incoming,
                nodes,
                depth,
            )?;
        }
        Ok(incoming)
    }

    fn entry(
        &mut self,
        entry: &Value,
        default_id: &str,
        deps: &[String],
        nodes: &mut Vec<Value>,
        depth: usize,
    ) -> Result<Vec<String>, String> {
        if let Some(reason) = self
            .layers
            .graph_limits
            .composite_safety_depth_bound()
            .refusal(depth)
        {
            return Err(reason);
        }
        let object = entry.as_object().ok_or("stage entries must be tables")?;
        if object.contains_key("reduce_verdict") && !object.contains_key("parallel") {
            return Err("reduce_verdict requires a parallel group".into());
        }
        if object.contains_key("export_first") && !object.contains_key("parallel") {
            return Err("export_first requires a parallel group".into());
        }
        if let Some(mode) = object.get("zero_minimum")
            && (mode.as_str() != Some("all_but_one") || !object.contains_key("parallel"))
        {
            return Err("zero_minimum must be all_but_one on a parallel group".into());
        }
        if object
            .get("review_lenses")
            .is_some_and(|flag| !flag.is_boolean())
        {
            return Err("review_lenses must be a boolean".into());
        }
        if selects_review_lenses(entry) && !object.contains_key("for_each") {
            return Err("review_lenses requires for_each".into());
        }
        // A list can replace a counted stage when it contains explicit items.
        if let Some(list) = object.get("for_each_if_nonempty") {
            let values = interpolate(list, &self.parameters, self.prompt, 1)?;
            let values = values
                .as_array()
                .ok_or("for_each_if_nonempty must name a list parameter")?;
            let mut selected = object.clone();
            selected.remove("for_each_if_nonempty");
            let item_fields = selected.remove("item_fields");
            if !values.is_empty() {
                if let Some(field) = selected.remove("item_route_field") {
                    let field = field.as_str().ok_or("item_route_field must be a string")?;
                    if let Some(routes) = &self.layers.known_routes {
                        for item in values {
                            let tier = item
                                .get(field)
                                .and_then(Value::as_str)
                                .ok_or_else(|| format!("{list}: item needs {field}"))?;
                            if !routes.contains(tier) {
                                return Err(format!(
                                    "{list}: unknown route {tier}; configured routes: {}",
                                    routes.iter().cloned().collect::<Vec<_>>().join(", ")
                                ));
                            }
                        }
                    }
                }
                if let Some(count) = selected.remove("count")
                    && let Some(parameter) = count
                        .as_str()
                        .and_then(|text| text.strip_prefix("${"))
                        .and_then(|text| text.strip_suffix('}'))
                {
                    let current = self.parameters.get(parameter);
                    let default = self.layers.resolve(self.name).ok().and_then(|(config, _)| {
                        config
                            .pointer(&format!("/parameters/{parameter}/default"))
                            .cloned()
                    });
                    if current != default.as_ref() {
                        self.layers.warnings.borrow_mut().push(format!(
                            "{list} replaces {parameter}={}",
                            current.map(scalar_text).unwrap_or_default()
                        ));
                    }
                }
                selected.insert("for_each".into(), list.clone());
                if let Some(fields) = item_fields {
                    for (key, value) in fields.as_object().ok_or("item_fields must be a table")? {
                        selected.insert(key.clone(), value.clone());
                    }
                }
            } else {
                selected.remove("item_route_field");
            }
            return self.entry(&Value::Object(selected), default_id, deps, nodes, depth);
        }
        if (object.contains_key("count") || object.contains_key("for_each"))
            && let Some(minimum) = object.get("min_successful")
            && !interpolate(minimum, &self.parameters, self.prompt, 1)?.is_null()
        {
            let mut member = object.clone();
            member.remove("min_successful");
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(default_id);
            if !valid_name(id) {
                return Err(format!("invalid stage node ID: {id}"));
            }
            if nodes.iter().any(|node| node["id"] == id) {
                return Err(format!("duplicate stage node ID: {id}"));
            }
            let mut children = Vec::new();
            let ends = self.entry(
                &Value::Object(member),
                default_id,
                &[],
                &mut children,
                depth + 1,
            )?;
            let minimum = interpolate(minimum, &self.parameters, self.prompt, 1)?
                .as_u64()
                .filter(|minimum| *minimum > 0)
                .ok_or("min_successful must be a positive integer")?;
            if minimum as usize > ends.len() {
                return Err(format!(
                    "min_successful ({}) {minimum} exceeds group {id} width ({}) {}",
                    object
                        .get("min_successful")
                        .and_then(Value::as_str)
                        .unwrap_or("literal"),
                    object
                        .get("count")
                        .and_then(Value::as_str)
                        .unwrap_or("members"),
                    ends.len()
                ));
            }
            let outputs = interpolate(
                object.get("outputs").unwrap_or(&json!(["result"])),
                &self.parameters,
                self.prompt,
                1,
            )?;
            let exports = selected_inputs(&ends, &outputs);
            let mut boundary_deps = deps.to_vec();
            for child in &children {
                if let Some(dependencies) = child.get("depends_on").and_then(Value::as_array) {
                    for dependency in dependencies {
                        let name = dependency
                            .as_str()
                            .ok_or("dependencies must be node names")?;
                        if !children.iter().any(|child| child["id"] == name)
                            && !boundary_deps.iter().any(|existing| existing == name)
                        {
                            boundary_deps.push(name.to_owned());
                        }
                    }
                }
            }
            nodes.push(composite(
                id,
                children,
                exports,
                &boundary_deps,
                &outputs,
                Some(minimum),
            ));
            return Ok(vec![id.to_owned()]);
        }
        let items: Vec<Option<Value>> = if let Some(list) = object.get("for_each") {
            if object.contains_key("count") {
                return Err("for_each and count are exclusive".into());
            }
            match interpolate(list, &self.parameters, self.prompt, 1)? {
                Value::Array(list) => {
                    let list = if selects_review_lenses(entry) {
                        self.select_items(list)?
                    } else {
                        list
                    };
                    list.into_iter().map(Some).collect()
                }
                _ => return Err("for_each must name a list parameter".into()),
            }
        } else {
            let count = interpolate(
                object.get("count").unwrap_or(&json!(1)),
                &self.parameters,
                self.prompt,
                1,
            )?
            .as_u64()
            .filter(|count| (0..=32).contains(count))
            .ok_or("entry count must be 0 to 32")?;
            (0..count).map(|_| None).collect()
        };
        let count = items.len();
        if count == 0 {
            // A zero count skips the stage; later stages depend on what it would have followed.
            return Ok(deps.to_vec());
        }
        let base = object
            .get("id")
            .map(|id| id.as_str().ok_or("entry id must be a string"))
            .transpose()?
            .unwrap_or(default_id);
        if !valid_name(base) {
            return Err(format!("invalid stage node ID: {base}"));
        }
        let mut ends = Vec::new();
        for (position, item) in items.iter().enumerate() {
            let index = position as u64 + 1;
            let id = match item {
                Some(item) => {
                    let name = item.get("name").or_else(|| item.get("id")).and_then(Value::as_str).filter(|n| valid_name(n)).ok_or(
                        "for_each items need an id or name of lowercase letters, digits and dashes",
                    )?;
                    format!("{base}-{name}")
                }
                None if count == 1 => base.to_owned(),
                None => format!("{base}-{index}"),
            };
            if nodes.iter().any(|node| node["id"] == id) {
                return Err(format!(
                    "duplicate stage node ID: {id} in {}",
                    object
                        .get("for_each")
                        .and_then(Value::as_str)
                        .unwrap_or("stage")
                ));
            }
            if let Some(item) = item {
                self.parameters["item"] = item.clone();
            }
            let mut object = object.clone();
            object.remove("count");
            object.remove("for_each");
            object.remove("review_lenses");
            if object.get("min_successful").is_some_and(|value| {
                interpolate(value, &self.parameters, self.prompt, 1)
                    .is_ok_and(|value| value.is_null())
            }) {
                object.remove("min_successful");
            }
            object.remove("id");
            if object.get("tier").is_some_and(|value| {
                interpolate(value, &self.parameters, self.prompt, index as usize)
                    .is_ok_and(|value| value.is_null())
            }) {
                object.remove("tier");
            }
            let fallback_stage = object.remove("fallback_stage");
            let distinct_families = object.remove("distinct_families");
            let outputs = interpolate(
                object.get("outputs").unwrap_or(&json!(["result"])),
                &self.parameters,
                self.prompt,
                index as usize,
            )?;
            let (mut node, decorate_wrapper) = if let Some(spec) = object
                .get("loop")
                .filter(|spec| spec.get("stages").is_some())
                .cloned()
            {
                object.remove("loop");
                let mut spec = spec.as_object().cloned().ok_or("loop must be a table")?;
                let stages = spec.remove("stages").ok_or("loop requires stages")?;
                let mut children = Vec::new();
                self.sequence(&stages, "", &[], &mut children, depth + 1)?;
                let mut spec = interpolate(
                    &Value::Object(spec),
                    &self.parameters,
                    self.prompt,
                    index as usize,
                )?;
                if spec
                    .get("decision_session")
                    .is_some_and(|session| session == "")
                {
                    spec.as_object_mut()
                        .ok_or("loop must be a table")?
                        .remove("decision_session");
                }
                spec["body"] = json!(children);
                let mut node = json!({"id":id,"kind":"loop","access":"read_only","depends_on":deps,"loop":spec});
                if !deps.is_empty() {
                    node["inputs"] = selected_inputs(deps, &outputs);
                }
                (node, true)
            } else if let Some(child) = object.remove("preset") {
                let child = child.as_str().ok_or("preset must be a name")?;
                let (mut config, resolved_origins) =
                    self.layers
                        .resolve_inner(child, &mut Vec::new(), self.repo_files)?;
                // Origins local to this child: its layers, then this stage's
                // entry values (or the `--set` that wrote them), then ambient ones.
                let mut local = resolved_origins;
                let stage_origin = |origins: &BTreeMap<String, String>, path: &str| {
                    origins
                        .get(&format!("{id}.{path}"))
                        .cloned()
                        .unwrap_or_else(|| "entry".into())
                };
                let mut passed: Vec<String> = Vec::new();
                if let Some(parameters) = object.remove("parameters") {
                    let parameters =
                        interpolate(&parameters, &self.parameters, self.prompt, index as usize)?;
                    passed.extend(
                        parameters
                            .as_object()
                            .into_iter()
                            .flat_map(|map| map.keys().cloned()),
                    );
                    for (key, value) in parameters
                        .as_object()
                        .ok_or("entry parameters must be a table")?
                    {
                        // Read roots are host-injected context, optional at each
                        // child interface. Other misspellings remain errors.
                        if key == "read_roots" && config.pointer("/parameters/read_roots").is_none()
                        {
                            continue;
                        }
                        override_value(
                            &mut config,
                            &format!("parameters.{key}"),
                            value.clone(),
                            &mut local,
                            &stage_origin(self.origins, key),
                        )
                        .map_err(|error| format!("stage {id}: {error}"))?;
                    }
                }
                if let Some(sets) = object.remove("set") {
                    for item in sets.as_array().ok_or("stage set must be a list")? {
                        let path = item["path"].as_str().unwrap_or_default();
                        if path == "read_roots"
                            && config.pointer("/parameters/read_roots").is_none()
                        {
                            continue;
                        }
                        let value = interpolate(
                            &item["value"],
                            &self.parameters,
                            self.prompt,
                            index as usize,
                        )?;
                        passed.push(path.split('.').next().unwrap_or(path).to_owned());
                        let append = if item["append"] == true { "+" } else { "" };
                        override_value(
                            &mut config,
                            &format!("parameters.{path}{append}"),
                            value,
                            &mut local,
                            &stage_origin(self.origins, path),
                        )
                        .map_err(|error| format!("stage {id}: {error}"))?;
                    }
                }
                let inherited: Vec<String> = config
                    .get("parameters")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .filter_map(|(key, spec)| {
                        let always = spec.get("inherit").and_then(Value::as_bool) == Some(true);
                        let explicit = spec.get("inherit_explicit").and_then(Value::as_bool)
                            == Some(true)
                            && self.origins.get(&format!("parameters.{key}")).is_some_and(
                                |origin| !self.layers.is_initial_origin(self.name, key, origin),
                            );
                        (always || explicit).then_some(key.clone())
                    })
                    .collect();
                for key in inherited {
                    if let Some(value) = self.parameters.get(key.as_str())
                        && !passed.iter().any(|passed| passed == &key)
                    {
                        override_value(
                            &mut config,
                            &format!("parameters.{key}"),
                            value.clone(),
                            &mut local,
                            "ambient",
                        )?;
                    }
                }
                apply_shape_stages(self.layers, child, &mut config, &local, self.prompt)?;
                if let Some(roles) = object.remove("roles") {
                    let roles = interpolate_inner(
                        &roles,
                        &self.parameters,
                        self.prompt,
                        index as usize,
                        true,
                    )?;
                    let target = config
                        .as_object_mut()
                        .ok_or("preset must be a table")?
                        .entry("roles")
                        .or_insert_with(|| json!({}));
                    merge(target, &roles, "roles", "entry", &mut local)?;
                }
                let mut children = Vec::new();
                let child_scope = if self.scope.is_empty() {
                    id.clone()
                } else {
                    format!("{}.{id}", self.scope)
                };
                let built = build(
                    self.layers,
                    child,
                    &config,
                    self.prompt,
                    &child_scope,
                    &[],
                    self.stack,
                    &mut children,
                    &mut local,
                    self.repo_files,
                );
                for (key, origin) in local {
                    let key = match key.strip_prefix("expanded.") {
                        Some(rest) => format!("expanded.{id}.{rest}"),
                        None => format!("expanded.{id}.{key}"),
                    };
                    self.origins.insert(key, origin);
                }
                let ends = built?;
                let exports = object
                    .remove("exports")
                    .or_else(|| config.get("exports").cloned())
                    .unwrap_or_else(|| selected_inputs(&ends, &json!(["result", "findings"])));
                (
                    composite(&id, children, exports, deps, &outputs, None),
                    true,
                )
            } else if let Some(parallel) = object.remove("parallel") {
                let entries = parallel
                    .as_array()
                    .filter(|entries| !entries.is_empty())
                    .ok_or("parallel must be a nonempty array")?;
                if let Some(deferred) =
                    self.deferred_review(&id, entries, &object, deps, &outputs, depth, nodes)?
                {
                    object.remove("min_successful");
                    object.remove("zero_minimum");
                    object.remove("reduce_verdict");
                    object.remove("export_first");
                    object.remove("exports");
                    (deferred, true)
                } else {
                    let mut children = Vec::new();
                    let mut ends = Vec::new();
                    for (position, entry) in entries.iter().enumerate() {
                        ends.extend(self.entry(
                            entry,
                            &format!("p{}", position + 1),
                            &[],
                            &mut children,
                            depth + 1,
                        )?);
                    }
                    let mut exports = object
                        .remove("exports")
                        .unwrap_or_else(|| selected_inputs(&ends, &json!(["result", "findings"])));
                    if let Some(requested) = object.remove("export_first") {
                        let requested =
                            interpolate(&requested, &self.parameters, self.prompt, index as usize)?;
                        let first = exports
                            .as_array_mut()
                            .and_then(|exports| exports.first_mut())
                            .and_then(|export| export.get_mut("outputs"))
                            .and_then(Value::as_array_mut)
                            .ok_or("export_first requires at least one group export")?;
                        for output in requested
                            .as_array()
                            .ok_or("export_first must be an array")?
                        {
                            if !first.contains(output) {
                                first.push(output.clone());
                            }
                        }
                    }
                    let all_but_one = object
                        .remove("zero_minimum")
                        .is_some_and(|mode| mode == "all_but_one");
                    let min_successful = object
                        .remove("min_successful")
                        .map(|value| {
                            if all_but_one {
                                let requested =
                                    interpolate(&value, &self.parameters, self.prompt, 1)?
                                        .as_u64()
                                        .ok_or("min_successful must be a non-negative integer")?;
                                Ok(if requested == 0 {
                                    ends.len().saturating_sub(1).max(1) as u64
                                } else {
                                    requested
                                })
                            } else {
                                self.minimum_successful(&value)
                            }
                        })
                        .transpose()?;
                    let reduce_verdict = object
                        .remove("reduce_verdict")
                        .map(|value| {
                            interpolate(&value, &self.parameters, self.prompt, index as usize)?
                                .as_bool()
                                .ok_or_else(|| "reduce_verdict must be a boolean".to_owned())
                        })
                        .transpose()?
                        .unwrap_or(false);
                    let mut group =
                        composite(&id, children, exports, deps, &outputs, min_successful);
                    if reduce_verdict {
                        group["composite"]["reduce_verdict"] = json!(true);
                    }
                    (group, true)
                }
            } else if let Some(stages) = object.remove("stages") {
                let mut children = Vec::new();
                let ends = self.sequence(&stages, "", &[], &mut children, depth + 1)?;
                let exports = object
                    .remove("exports")
                    .unwrap_or_else(|| selected_inputs(&ends, &json!(["result", "findings"])));
                (
                    composite(&id, children, exports, deps, &outputs, None),
                    true,
                )
            } else {
                (
                    lower_leaf(
                        self.layers,
                        &object,
                        self.roles,
                        &self.parameters,
                        self.prompt,
                        index as usize,
                        &id,
                        deps,
                    )?,
                    false,
                )
            };
            if selects_review_lenses(entry)
                && !decorate_wrapper
                && let Some(group) = item.as_ref().and_then(|item| item.get("family_group"))
            {
                node["family_group"] = group.clone();
            }
            // Siblings of a fan-out prefer different model families. It is a
            // preference only: every branch still runs.
            if let Some(flag) = distinct_families {
                let on = interpolate(&flag, &self.parameters, self.prompt, index as usize)?;
                match on {
                    Value::Bool(true) if !decorate_wrapper => {
                        if count > 1 {
                            node["family_group"] = json!(base);
                        }
                    }
                    Value::Bool(false) => {}
                    Value::String(ref text) if text.is_empty() => {}
                    _ => {
                        return Err(format!(
                            "stage {id}: distinct_families must be a boolean on a leaf stage"
                        ));
                    }
                }
            }
            if decorate_wrapper {
                object.remove("outputs");
                for key in object.keys() {
                    if ![
                        "title",
                        "prompt",
                        "source",
                        "read_roots",
                        "attachments",
                        "inputs",
                        "depends_on",
                        "advisory",
                    ]
                    .contains(&key.as_str())
                    {
                        return Err(format!(
                            "preset {}: composite {id}: unknown entry field {key}",
                            self.name
                        ));
                    }
                }
                if let Some(title) = object.get("title") {
                    node["title"] =
                        interpolate(title, &self.parameters, self.prompt, index as usize)?;
                }
                for key in ["source", "read_roots", "attachments", "inputs", "advisory"] {
                    if let Some(value) = object.get(key) {
                        node[key] =
                            interpolate(value, &self.parameters, self.prompt, index as usize)?;
                    }
                }
                if let Some(extra) = object.get("depends_on") {
                    let extra = interpolate(extra, &self.parameters, self.prompt, index as usize)?;
                    let extra = extra.as_array().ok_or("depends_on must be an array")?;
                    let existing = node["depends_on"]
                        .as_array_mut()
                        .ok_or("depends_on must be an array")?;
                    for dependency in extra {
                        if !existing.contains(dependency) {
                            existing.push(dependency.clone());
                        }
                    }
                }
                if let Some(brief) = object.get("prompt") {
                    return Err(format!(
                        "composite {id}: brief belongs on its leaves, received {brief}"
                    ));
                }
            }
            if let Some(fallback_stage) = fallback_stage {
                let fallback_outputs = interpolate(
                    fallback_stage.get("outputs").unwrap_or(&json!(["result"])),
                    &self.parameters,
                    self.prompt,
                    index as usize,
                )?;
                let mut fallback_dependencies = deps.to_vec();
                if let Some(extra) = fallback_stage.get("depends_on") {
                    let extra = interpolate(extra, &self.parameters, self.prompt, index as usize)?;
                    for dependency in extra
                        .as_array()
                        .ok_or("fallback depends_on must be an array")?
                    {
                        let dependency = dependency
                            .as_str()
                            .ok_or("fallback dependency must be a string")?
                            .to_owned();
                        if !fallback_dependencies.contains(&dependency) {
                            fallback_dependencies.push(dependency);
                        }
                    }
                }
                let fallback_inputs = match fallback_stage.get("inputs") {
                    Some(inputs) => {
                        interpolate(inputs, &self.parameters, self.prompt, index as usize)?
                    }
                    None => selected_inputs(&fallback_dependencies, &fallback_outputs),
                };
                let mut fallback_nodes = Vec::new();
                let ends = self.entry(
                    &fallback_stage,
                    "fallback",
                    &[],
                    &mut fallback_nodes,
                    depth + 1,
                )?;
                node["fallback_stage"] = json!({
                    "nodes": fallback_nodes,
                    "exports": selected_inputs(&ends, &fallback_outputs),
                    "depends_on": fallback_dependencies,
                    "inputs": fallback_inputs,
                    "source": fallback_stage.get("source").map(|source| interpolate(source, &self.parameters, self.prompt, index as usize)).transpose()?,
                    "read_roots": fallback_stage.get("read_roots").map(|roots| interpolate(roots, &self.parameters, self.prompt, index as usize)).transpose()?,
                    "title": fallback_stage.get("title").map(|title| interpolate(title, &self.parameters, self.prompt, index as usize)).transpose()?,
                });
            }
            if let Some(parameters) = self.parameters.as_object_mut() {
                parameters.remove("item");
            }
            let count = self.layers.built_nodes.get().saturating_add(1);
            if let Some(reason) = self
                .layers
                .graph_limits
                .composite_safety_nodes_bound()
                .refusal(count)
            {
                return Err(reason);
            }
            self.layers.built_nodes.set(count);
            nodes.push(node);
            ends.push(id);
        }
        Ok(ends)
    }
}

const LEVELS: [&str; 3] = ["default", "recommended", "experimental"];

fn check_pattern(
    name: &str,
    pattern: Option<&Value>,
    scope: &str,
    origins: &BTreeMap<String, String>,
) -> Result<(), String> {
    let Some(pattern) = pattern else {
        return Ok(());
    };
    let fail = |key: &str, message: &str| {
        invalid_config(name, &format!("pattern.{key}"), message, scope, origins)
    };
    let map = pattern
        .as_object()
        .ok_or_else(|| invalid_config(name, "pattern", "must be a table", scope, origins))?;
    for (key, value) in map {
        match key.as_str() {
            "level" if value.as_str().is_some_and(|level| LEVELS.contains(&level)) => {}
            "level" => return Err(fail(key, "must be default, recommended or experimental")),
            "summary" | "cost" | "select_when" if value.is_string() => {}
            "citations" | "tags"
                if value
                    .as_array()
                    .is_some_and(|list| list.iter().all(Value::is_string)) => {}
            "summary" | "cost" | "select_when" | "citations" | "tags" => {
                return Err(fail(key, "has the wrong type"));
            }
            _ => return Err(fail(key, "unknown field")),
        }
    }
    Ok(())
}

fn composite(
    id: &str,
    nodes: Vec<Value>,
    exports: Value,
    deps: &[String],
    outputs: &Value,
    min_successful: Option<u64>,
) -> Value {
    let mut spec = json!({"nodes":nodes,"exports":exports});
    if let Some(minimum) = min_successful {
        spec["min_successful"] = json!(minimum);
    }
    let mut node =
        json!({"id":id,"kind":"composite","access":"read_only","depends_on":deps,"composite":spec});
    if !deps.is_empty() {
        node["inputs"] = selected_inputs(deps, outputs);
    }
    node
}

fn configured(globals: &provenant_cli::Globals) -> Result<Layers, String> {
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Optional,
    )
    .map_err(|e| e.to_string())?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| resolved.home.join(".config"));
    if !config.is_absolute() {
        return Err("XDG_CONFIG_HOME must be absolute".into());
    }
    let mut layers = Layers::load(
        &config.join("provenant/presets"),
        &resolved.project.join(".provenant/presets"),
    )?;
    layers.roles.skill_root = Some(resolved.assets.clone());
    Ok(layers)
}

fn report(command: &str, result: Result<Value, String>, globals: &provenant_cli::Globals) -> u8 {
    match result {
        Ok(data) => {
            let human = presentation::human(&data, globals.full);
            let summary = provenant_cli::Summary::verbatim(&human, None);
            crate::emit_summarised(
                &provenant_records::CommandResult::new(
                    command,
                    provenant_records::Outcome::Ok,
                    data,
                ),
                globals,
                &human,
                true,
                Some(&summary),
            )
        }
        Err(error) => crate::fail(
            command,
            provenant_records::Outcome::Invalid,
            "preset",
            &error,
            globals,
        ),
    }
}

fn visit_nodes(
    nodes: &mut [Value],
    visitor: &mut impl FnMut(&mut Value) -> Result<(), String>,
) -> Result<(), String> {
    for node in nodes {
        visitor(node)?;
        for path in ["/composite/nodes", "/loop/body", "/plan/fallback/nodes"] {
            if let Some(children) = node.pointer_mut(path).and_then(Value::as_array_mut) {
                visit_nodes(children, visitor)?;
            }
        }
        // A planner's catalogue templates bind the same run context as static
        // nodes, so a planned lane starts from the pinned commit and read roots.
        if let Some(patterns) = node
            .pointer_mut("/plan/patterns")
            .and_then(Value::as_array_mut)
        {
            for pattern in patterns {
                if let Some(children) = pattern.get_mut("nodes").and_then(Value::as_array_mut) {
                    visit_nodes(children, visitor)?;
                }
            }
        }
    }
    Ok(())
}

/// The worktrees of the project `directory` belongs to, main checkout first,
/// which a graph node may always read. Outside a repository the project is only
/// its own directory.
pub(crate) fn project_worktrees(directory: &Path) -> Vec<PathBuf> {
    crate::handoff::git_text(directory, &["worktree", "list", "--porcelain"])
        .map(|listing| {
            listing
                .lines()
                .filter_map(|line| line.strip_prefix("worktree "))
                .map(std::path::PathBuf::from)
                .collect()
        })
        .unwrap_or_else(|_| vec![directory.to_path_buf()])
}

fn discover_read_roots(worktrees: &[PathBuf], allowed: &[String], extra: &[PathBuf]) -> ReadRoots {
    let mut project = worktrees.to_vec();
    project.extend(extra.iter().cloned());
    ReadRoots::new(project, allowed)
}

/// The linked worktrees, not the main checkout, which can host a writer lane.
pub(crate) fn linked_worktrees_from(worktrees: &[PathBuf]) -> Vec<String> {
    worktrees
        .iter()
        .skip(1)
        .map(|path| path.display().to_string())
        .collect()
}

/// Whether a read root is exactly one `${parameter}`, with nothing around it.
fn bare_placeholder(text: &str) -> bool {
    text.strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .is_some_and(|name| !name.is_empty() && !name.contains(['$', '{', '}']))
}

/// A planner pattern names a read root by a closed-list parameter. The planner
/// can pick any listed value, so every one is confined to the project's
/// registered worktrees and allow-list now, exactly like a literal root, and a
/// root that is not exactly one parameter is refused. Each value is replaced by
/// the canonical path it confined to, so a link retargeted after admission
/// cannot move the grant, and the planner's catalogue lists the same paths.
pub(crate) fn confine_pattern_roots(node: &mut Value, policy: &ReadRoots) -> Result<(), String> {
    let Some(patterns) = node
        .pointer_mut("/plan/patterns")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    let mut renamed: Vec<(String, String)> = Vec::new();
    for pattern in patterns {
        let name = pattern
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let mut parameters = Vec::new();
        fn collect_roots<'a>(value: &'a Value, roots: &mut Vec<&'a Value>) {
            match value {
                Value::Object(map) => {
                    if let Some(items) = map.get("read_roots").and_then(Value::as_array) {
                        roots.extend(items);
                    }
                    for (key, child) in map {
                        if key != "read_roots" {
                            collect_roots(child, roots);
                        }
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        collect_roots(item, roots);
                    }
                }
                _ => {}
            }
        }
        let mut roots = Vec::new();
        collect_roots(&pattern["nodes"], &mut roots);
        for root in roots {
            let text = root.as_str().ok_or("read_roots entries must be strings")?;
            if !text.contains("${") {
                continue;
            }
            if !bare_placeholder(text) {
                return Err(format!(
                    "plan pattern {name}: read root {text} must be exactly one parameter"
                ));
            }
            parameters.push(text[2..text.len() - 1].to_owned());
        }
        for parameter in parameters {
            let values = pattern
                .pointer_mut(&format!("/parameters/{parameter}/enum"))
                .and_then(Value::as_array_mut)
                .filter(|values| !values.is_empty())
                .ok_or_else(|| {
                    format!("plan pattern {name}: read root ${{{parameter}}} needs a closed list")
                })?;
            for value in values {
                let given = value
                    .as_str()
                    .ok_or("read root values must be strings")?
                    .to_owned();
                if !Path::new(&given).is_absolute() {
                    return Err(format!(
                        "plan pattern {name}: read root {given} must be absolute"
                    ));
                }
                let canonical = policy
                    .confine(Path::new(&given))?
                    .to_str()
                    .ok_or("read root is not UTF-8")?
                    .to_owned();
                if canonical != given {
                    renamed.push((given, canonical.clone()));
                    *value = json!(canonical);
                }
            }
            // A default that named a link follows its value.
            if let Some(default) = pattern.pointer_mut(&format!("/parameters/{parameter}/default"))
                && let Some((_, canonical)) = default
                    .as_str()
                    .and_then(|text| renamed.iter().find(|(given, _)| given == text))
            {
                *default = json!(canonical);
            }
        }
    }
    // The planner reads the same canonical paths the catalogue will accept.
    if let Some(prompt) = node.get("prompt").and_then(Value::as_str) {
        let mut text = prompt.to_owned();
        for (given, canonical) in &renamed {
            text = replace_path(&text, given, canonical);
        }
        node["prompt"] = json!(text);
    }
    Ok(())
}

/// Replace `from` with `to` where `from` is a whole path, not the tail of a
/// longer one such as `/var/x` inside `/private/var/x`.
fn replace_path(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(from) {
        let before = rest[..at].chars().next_back().or(out.chars().next_back());
        let after = rest[at + from.len()..].chars().next();
        let inside = |c: char| c.is_alphanumeric() || matches!(c, '/' | '_' | '-');
        out.push_str(&rest[..at]);
        if before.is_some_and(|c| inside(c) || c == '.') || after.is_some_and(inside) {
            out.push_str(from);
        } else {
            out.push_str(to);
        }
        rest = &rest[at + from.len()..];
    }
    out.push_str(rest);
    out
}

/// Bind CLI context before hashing, explaining or retaining the executed graph.
pub(crate) fn bind_run_context(
    expansion: &mut Expansion,
    request: &provenant_cli::Dispatch,
    directory: &Path,
    policy: &ReadRoots,
    default_capabilities: &[String],
    selection_source: &str,
    experiment: Option<&crate::experiment::Assignment>,
) -> Result<(), String> {
    if let Some(concurrency) = request.concurrency {
        if expansion.document.get("limits").is_none() {
            expansion.document["limits"] = json!({});
        }
        expansion.document["limits"]["concurrency"] = json!(concurrency);
    }
    // An explicit network reach binds every node lane the graph launches.
    if let Some(network) = request.access.network {
        expansion.document["network"] = json!(match network {
            provenant_cli::Switch::On => "on",
            provenant_cli::Switch::Off => "off",
        });
    }
    if !request.access.capability.is_empty() {
        expansion.document["capability"] = json!(request.access.capability);
    }
    if !request.access.no_capability.is_empty() {
        expansion.document["no_capability"] = json!(request.access.no_capability);
    }
    // `--exclude` binds every node the graph launches; an unknown selector
    // was refused before this point.
    if !request.route.exclude.is_empty() {
        expansion.document["exclude"] = json!(request.route.exclude);
    }
    // Resolve review references in the checkout bound to this execution, and
    // carry their OIDs in the graph document. A later advance may run from a
    // different checkout or after a named ref moves.
    let review_directory = bound_worktree_path(expansion, request.access.worktree.as_deref())
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                directory.join(path)
            }
        })
        .unwrap_or_else(|| directory.to_path_buf());
    let pin_review_base = |value: &mut Value| -> Result<(), String> {
        let Some(reference) = value.as_str().filter(|reference| !reference.is_empty()) else {
            return Ok(());
        };
        if reference.parse::<provenant_core::GitOid>().is_err() {
            let git = provenant_os::git::git_executable()
                .ok_or("Git unavailable for review base binding")?;
            let oid = provenant_run::resolve_git_base(&git, &review_directory, reference)
                .map_err(|error| error.to_string())?;
            *value = json!(oid);
        }
        Ok(())
    };
    if let Some(base) = expansion.document.get_mut("review_base") {
        pin_review_base(base)?;
    }
    let mut capability_warnings = Vec::new();
    let nodes = expansion.document["nodes"]
        .as_array_mut()
        .ok_or("missing graph nodes")?;
    visit_nodes(nodes, &mut |node| {
        if let Some(base) = node.get_mut("review_base") {
            pin_review_base(base)?;
        }
        confine_pattern_roots(node, policy)?;
        let container = node.get("composite").is_some() || node.get("loop").is_some();
        if !container && !matches!(node["kind"].as_str(), Some("gate" | "land" | "decide")) {
            if request.require_skill_location {
                node["require_skill_location"] = json!(true);
            }
            let writer = node["access"] == "worktree_write";
            if writer || node.get("capabilities").is_some() {
                let invalid_capabilities = node
                    .get("capabilities")
                    .is_some_and(|value| !value.is_array());
                if invalid_capabilities {
                    capability_warnings.push("ignored malformed node capabilities".to_owned());
                }
                let supplied = node.get("capabilities").and_then(Value::as_array);
                let mut sources: Vec<(String, &str)> = supplied.map_or_else(
                    || {
                        if invalid_capabilities {
                            return Vec::new();
                        }
                        default_capabilities
                            .iter()
                            .cloned()
                            .map(|name| (name, "project default"))
                            .collect()
                    },
                    |values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(|name| (name.to_owned(), "node"))
                            .collect()
                    },
                );
                sources.retain(|(capability, origin)| {
                    if capability == "postgres" {
                        true
                    } else {
                        capability_warnings.push(format!(
                            "ignored unknown {origin} capability {}",
                            provenant_fabric_runtime::hosts::plain_text(capability, 128)
                        ));
                        false
                    }
                });
                if writer {
                    for capability in &request.access.capability {
                        if capability != "postgres" {
                            capability_warnings.push(format!(
                                "ignored unknown flag capability {}",
                                provenant_fabric_runtime::hosts::plain_text(capability, 128)
                            ));
                            continue;
                        }
                        sources.retain(|(name, _)| name != capability);
                        sources.push((capability.clone(), "flag"));
                    }
                }
                let mut seen = BTreeSet::new();
                sources.retain(|(name, _)| {
                    !request.access.no_capability.contains(name) && seen.insert(name.clone())
                });
                if supplied.is_some()
                    || invalid_capabilities
                    || !default_capabilities.is_empty()
                    || !request.access.capability.is_empty()
                {
                    node["capabilities"] =
                        json!(sources.iter().map(|(name, _)| name).collect::<Vec<_>>());
                }
                if !sources.is_empty() {
                    node["capability_sources"] = json!(
                        sources
                            .iter()
                            .map(|(name, source)| json!({"name": name, "source": source}))
                            .collect::<Vec<_>>()
                    );
                }
            }

            // A deferred review measures in the bound worktree unless it names one.
            if let Some(worktree) = &request.access.worktree
                && node.get("review").is_some_and(Value::is_object)
                && node["review"]["worktree"]
                    .as_str()
                    .is_none_or(str::is_empty)
            {
                node["review"]["worktree"] = json!(if worktree.is_absolute() {
                    worktree.clone()
                } else {
                    directory.join(worktree)
                });
            }
            if writer
                && node
                    .get("worktree")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                && let Some(worktree) = &request.access.worktree
            {
                node["worktree"] = json!(if worktree.is_absolute() {
                    worktree.clone()
                } else {
                    directory.join(worktree)
                });
            }
            if !request.access.attach.is_empty() {
                let attachments = node
                    .as_object_mut()
                    .ok_or("node must be a table")?
                    .entry("attachments")
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .ok_or("attachments must be an array")?;
                for path in &request.access.attach {
                    let path = if path.is_absolute() {
                        path.clone()
                    } else {
                        directory.join(path)
                    };
                    let value = json!(path);
                    if !attachments.contains(&value) {
                        attachments.push(value);
                    }
                }
            }
            let mut roots = node
                .get("read_roots")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for path in request.access.read_root.iter().chain(
                (!writer)
                    .then_some(request.access.worktree.as_ref())
                    .flatten(),
            ) {
                let value = json!(if path.is_absolute() {
                    path.clone()
                } else {
                    directory.join(path)
                });
                if !roots.contains(&value) {
                    roots.push(value);
                }
            }
            if !roots.is_empty() {
                let mut confined = Vec::new();
                for root in &roots {
                    let text = root.as_str().ok_or("read_roots entries must be strings")?;
                    // A planner catalogue names the root by a closed-list
                    // parameter (a registered worktree); the runtime fills it.
                    // `confine_pattern_roots` has confined every value it can take.
                    if bare_placeholder(text) {
                        confined.push(root.clone());
                        continue;
                    }
                    let path = Path::new(text);
                    let path = if path.is_absolute() {
                        path.to_path_buf()
                    } else {
                        directory.join(path)
                    };
                    let value = json!(policy.confine(&path)?);
                    if !confined.contains(&value) {
                        confined.push(value);
                    }
                }
                node["read_roots"] = json!(confined);
            }
        }
        if node.pointer("/source/type").and_then(Value::as_str) == Some("commit") {
            let reference = node
                .pointer("/source/commit")
                .and_then(Value::as_str)
                .ok_or("source.commit must be a Git reference")?;
            if reference.parse::<provenant_core::GitOid>().is_err() {
                let git = provenant_os::git::git_executable()
                    .ok_or("Git unavailable for source binding")?;
                let oid = provenant_run::resolve_git_base(&git, directory, reference)
                    .map_err(|e| e.to_string())?;
                node["source"]["commit"] = json!(oid);
            }
        }
        Ok(())
    })?;
    expansion.warnings.extend(capability_warnings);
    // The graph context is the single carrier of run labels; the scheduler
    // adds graph and node identity to each submitted task.
    expansion.document["context"]["preset"] = expansion.document["provenance"]["preset"].clone();
    expansion.document["context"]["selection_source"] = json!(selection_source);
    if let Some(work_item) = request.task_id.as_ref() {
        expansion.document["context"]["work_item"] = json!(work_item.to_string());
    }
    if let Some(experiment) = experiment {
        // The pattern and arm name what executed. A shadow candidate is only
        // recorded in `shadow_of`, never as the outcome's arm.
        expansion.document["context"]["pattern"] = json!(format!(
            "experiment:{}:{}",
            experiment.experiment, experiment.arm
        ));
        expansion.document["context"]["experiment_id"] = json!(experiment.experiment);
        expansion.document["context"]["experiment_seed"] = json!(experiment.seed);
        expansion.document["context"]["experiment_arm"] = json!(experiment.arm);
        if let Some(candidate) = &experiment.shadow_of {
            expansion.document["context"]["shadow_of"] = json!(candidate);
        }
    }
    expansion.document["graph_id"] = json!("");
    // An integration source resolves at run time, so fold the current head into
    // the identity: a moved branch must never reuse a completed graph.
    let mut identity = serde_json::to_vec(&expansion.document).map_err(|e| e.to_string())?;
    let mut integration = false;
    visit_nodes(
        expansion.document["nodes"]
            .as_array_mut()
            .ok_or("missing graph nodes")?,
        &mut |node| {
            integration |=
                node.pointer("/source/type").and_then(Value::as_str) == Some("integration");
            Ok(())
        },
    )?;
    if integration && let Some(branch) = expansion.document["integration_branch"].as_str() {
        let reference = match branch.strip_prefix("origin/") {
            Some(branch) => format!("refs/remotes/origin/{branch}"),
            None => format!("refs/heads/{branch}"),
        };
        let git =
            provenant_os::git::git_executable().ok_or("Git unavailable for source binding")?;
        // An absent branch has no head to fold in; the graph owner refuses it later.
        let head = provenant_run::resolve_git_base(&git, directory, &reference)
            .map(|oid| oid.to_string())
            .unwrap_or_else(|_| "absent".into());
        identity.extend_from_slice(format!("\nintegration-head:{head}").as_bytes());
    }
    // `--fresh` makes this run a new graph even when its inputs are unchanged,
    // for example to rerun after a cancel.
    if request.fresh {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        use provenant_core::Clock;
        let nanos = provenant_os::clock::SystemClock::new()
            .wall_now()
            .map_or(0, |now| now.as_timestamp().as_nanosecond());
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        identity.extend_from_slice(
            format!("\nfresh:{nanos}:{}:{count}", std::process::id()).as_bytes(),
        );
    }
    let digest = provenant_records::CanonicalV1::file_digest(&identity).to_string();
    let graph_id = format!("preset-{}", &digest[7..39]);
    expansion.document["graph_id"] = json!(graph_id);
    Ok(())
}

/// Resolve a preset's default `HEAD` subject from its bound writer or preset
/// worktree. An explicitly supplied subject stays under the usual
/// project-directory resolution in `bind_run_context`.
fn bound_worktree_path(expansion: &Expansion, cli_worktree: Option<&Path>) -> Option<PathBuf> {
    fn writer_path(nodes: &[Value]) -> Option<PathBuf> {
        for node in nodes {
            if node["access"] == "worktree_write"
                && let Some(path) = node.get("worktree").and_then(Value::as_str)
                && !path.is_empty()
            {
                return Some(PathBuf::from(path));
            }
            for key in ["composite", "loop"] {
                let nested = node
                    .pointer(&format!("/{key}/nodes"))
                    .or_else(|| node.pointer(&format!("/{key}/body")))
                    .and_then(Value::as_array);
                if let Some(path) = nested.and_then(|nested| writer_path(nested)) {
                    return Some(path);
                }
            }
        }
        None
    }
    let nodes = expansion.document["nodes"].as_array()?;
    if let Some(path) = writer_path(nodes) {
        return Some(path);
    }
    expansion
        .worktree
        .clone()
        .or_else(|| cli_worktree.map(Path::to_path_buf))
}

fn bind_default_subject_to_worktree(
    expansion: &mut Expansion,
    directory: &Path,
    explicit_subject: bool,
    cli_worktree: Option<&Path>,
) -> Result<(), String> {
    if explicit_subject {
        return Ok(());
    }
    let nodes = expansion.document["nodes"]
        .as_array()
        .ok_or("missing graph nodes")?;
    fn has_default_head(nodes: &[Value]) -> bool {
        nodes.iter().any(|node| {
            (node.pointer("/source/type").and_then(Value::as_str) == Some("commit")
                && node.pointer("/source/commit").and_then(Value::as_str) == Some("HEAD"))
                || ["composite/nodes", "loop/body"]
                    .iter()
                    .find_map(|path| node.pointer(&format!("/{path}")).and_then(Value::as_array))
                    .is_some_and(|nested| has_default_head(nested))
        })
    }
    if !has_default_head(nodes) {
        return Ok(());
    }
    let Some(worktree) = bound_worktree_path(expansion, cli_worktree) else {
        return Ok(());
    };
    let worktree = if worktree.is_absolute() {
        worktree
    } else {
        directory.join(worktree)
    };
    let git = provenant_os::git::git_executable().ok_or("Git unavailable for subject binding")?;
    let oid = provenant_run::resolve_git_base(&git, &worktree, "HEAD").map_err(|error| {
        format!(
            "cannot resolve default subject in {}: {error}",
            worktree.display()
        )
    })?;
    let mut resolved = false;
    let mut multiple_subjects = false;
    visit_nodes(
        expansion.document["nodes"]
            .as_array_mut()
            .ok_or("missing graph nodes")?,
        &mut |node| {
            if node.pointer("/source/type").and_then(Value::as_str) == Some("commit")
                && node.pointer("/source/commit").and_then(Value::as_str) == Some("HEAD")
            {
                // Each writer can belong to a different lane. Its default
                // source is its own worktree HEAD, not the first writer's.
                let lane = (node["access"] == "worktree_write")
                    .then(|| node.get("worktree").and_then(Value::as_str))
                    .flatten()
                    .filter(|path| !path.is_empty() && !path.contains("${"));
                let source = if let Some(lane) = lane {
                    let path = Path::new(lane);
                    let path = if path.is_absolute() {
                        path.to_path_buf()
                    } else {
                        directory.join(path)
                    };
                    provenant_run::resolve_git_base(&git, &path, "HEAD").map_err(|error| {
                        format!(
                            "cannot resolve default subject for node {} in {}: {error}",
                            node["id"].as_str().unwrap_or("unknown"),
                            path.display()
                        )
                    })?
                } else {
                    oid.clone()
                };
                multiple_subjects |= source != oid;
                node["source"]["commit"] = json!(source);
                resolved = true;
            }
            Ok(())
        },
    )?;
    if resolved && !multiple_subjects {
        expansion.document["provenance"]["resolved_subject"] = json!({
            "reference": "HEAD",
            "worktree": worktree,
            "commit": oid,
        });
    }
    Ok(())
}

/// Warn when a resolved node commit source differs from the bound worktree's HEAD.
fn warn_differing_source_and_worktree(
    expansion: &mut Expansion,
    directory: &Path,
    cli_worktree: Option<&Path>,
) -> Result<(), String> {
    let nodes = expansion.document["nodes"]
        .as_array()
        .ok_or("missing graph nodes")?;
    let Some(worktree) = bound_worktree_path(expansion, cli_worktree) else {
        return Ok(());
    };
    let worktree = if worktree.is_absolute() {
        worktree
    } else {
        directory.join(worktree)
    };
    let git = match provenant_os::git::git_executable() {
        Some(git) => git,
        None => return Ok(()),
    };
    let worktree_head = match provenant_run::resolve_git_base(&git, &worktree, "HEAD") {
        Ok(oid) => oid,
        Err(_) => return Ok(()),
    };

    let mut warnings = Vec::new();
    fn check_node(
        node: &Value,
        worktree_head: &str,
        directory: &Path,
        git: &Path,
        warnings: &mut Vec<String>,
    ) {
        let lane_head = (node["access"] == "worktree_write")
            .then(|| node.get("worktree").and_then(Value::as_str))
            .flatten()
            .filter(|path| !path.is_empty() && !path.contains("${"))
            .and_then(|path| {
                let path = Path::new(path);
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    directory.join(path)
                };
                provenant_run::resolve_git_base(git, &path, "HEAD").ok()
            });
        let worktree_head = lane_head.as_deref().unwrap_or(worktree_head);
        if node.pointer("/source/type").and_then(Value::as_str) == Some("commit")
            && let Some(commit) = node.pointer("/source/commit").and_then(Value::as_str)
            && commit != worktree_head
        {
            let warning =
                format!("source commit {commit} differs from bound worktree HEAD {worktree_head}");
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        for key in ["composite", "loop"] {
            if let Some(nested) = node
                .pointer(&format!("/{key}/nodes"))
                .or_else(|| node.pointer(&format!("/{key}/body")))
                .and_then(Value::as_array)
            {
                for child in nested {
                    check_node(child, worktree_head, directory, git, warnings);
                }
            }
        }
    }
    for node in nodes {
        check_node(node, &worktree_head, directory, &git, &mut warnings);
    }
    for warning in warnings {
        if !expansion.warnings.contains(&warning) {
            expansion.warnings.push(warning.clone());
            if let Some(list) = expansion.document["provenance"]["warnings"].as_array_mut() {
                list.push(json!(warning));
            }
        }
    }
    Ok(())
}

/// The family constraint a node carries: `any`, `distinct_from_writer`,
/// `cross_preferred` or `cross_required`, with `+require` when a writer
/// separation is strict.
fn family(node: &Value) -> String {
    let required = node["distinct_strength"] == "require";
    match node["distinct_from"].as_str() {
        Some("dependencies") if required => "cross_required".into(),
        Some("dependencies") => "cross_preferred".into(),
        Some("writer") if required => "distinct_from_writer+require".into(),
        Some("writer") => "distinct_from_writer".into(),
        _ => "any".into(),
    }
}

pub(super) fn pending_checks(
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> Vec<Value> {
    use provenant_fabric_runtime::graph::{AccessMode, GraphNode, NodeKind, SourceBinding};
    use provenant_records::run::DeliveryMode;
    let mut ids = BTreeSet::new();
    fn walk(nodes: &[GraphNode], ids: &mut BTreeSet<&'static str>) {
        for node in nodes {
            if node
                .source
                .as_ref()
                .is_some_and(|source| !matches!(source, SourceBinding::Commit { .. }))
            {
                ids.insert("source");
            }
            if node.access == AccessMode::WorktreeWrite {
                ids.insert("worktree");
            }
            if node.kind == Some(NodeKind::Plan) {
                ids.insert("planner_authority");
            }
            let agent = !matches!(
                node.kind,
                Some(
                    NodeKind::Composite
                        | NodeKind::Loop
                        | NodeKind::Land
                        | NodeKind::Gate
                        | NodeKind::Decide
                )
            );
            if agent {
                ids.insert("route_readiness");
            }
            if let Some(group) = &node.composite {
                walk(&group.nodes, ids);
            }
            if let Some(body) = &node.loop_body {
                walk(&body.body, ids);
            }
        }
    }
    walk(&document.nodes, &mut ids);
    let delivery_needs_target = |delivery: &Option<DeliveryMode>| {
        matches!(
            delivery,
            Some(DeliveryMode::Pr | DeliveryMode::Integrate(_))
        )
    };
    if delivery_needs_target(&document.delivery)
        || document
            .nodes
            .iter()
            .any(|node| delivery_needs_target(&node.delivery))
    {
        ids.insert("delivery_target");
    }
    ids.into_iter()
        .map(|id| {
            let reason = match id {
                "source" => "Source binding must be resolved and verified at execution.",
                "worktree" => "Writer worktree ownership is checked at execution.",
                "delivery_target" => "Delivery target is checked before delivery.",
                "planner_authority" => "Planner authority is checked when the plan runs.",
                _ => "Route readiness and quota are checked before launch.",
            };
            json!({"id":id,"reason":reason})
        })
        .collect()
}

pub(super) fn explain_tree(nodes: &[Value], indent: usize, text: &mut String) {
    for node in nodes {
        let field = |name: &str| node.get(name).and_then(Value::as_str).unwrap_or("-");
        let tier = node
            .pointer("/route/alias")
            .or_else(|| node.pointer("/route/model"))
            .or_else(|| node.pointer("/routes/0/alias"))
            .or_else(|| node.pointer("/routes/0/model"))
            .and_then(Value::as_str)
            .unwrap_or("-");
        text.push_str(&format!(
            "{}{}  role={} tier={} family={} kind={} access={} worktree={}\n",
            "  ".repeat(indent),
            field("id"),
            field("role"),
            tier,
            family(node),
            field("kind"),
            field("access"),
            field("worktree")
        ));
        for reference in node
            .get("references")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            text.push_str(&format!(
                "{}  reference: {} {}\n",
                "  ".repeat(indent),
                reference["ref"].as_str().unwrap_or("-"),
                reference["digest"].as_str().unwrap_or("-")
            ));
        }
        for key in [
            "write_scope",
            "read_roots",
            "attachments",
            "capability_sources",
            "source",
            "inputs",
            "acceptance_checks",
            "required_verdict",
            "deadline_seconds",
        ] {
            if let Some(value) = node.get(key) {
                text.push_str(&format!("{}  {key}: {value}\n", "  ".repeat(indent)));
            }
        }
        if let Some(children) = node.pointer("/composite/nodes").and_then(Value::as_array) {
            explain_tree(children, indent + 1, text);
            if let Some(exports) = node.pointer("/composite/exports") {
                text.push_str(&format!("{}  exports: {exports}\n", "  ".repeat(indent)));
            }
        }
        if let Some(children) = node.pointer("/loop/body").and_then(Value::as_array) {
            text.push_str(&format!(
                "{}  max_rounds: {}\n",
                "  ".repeat(indent),
                node["loop"]["max_rounds"]
            ));
            explain_tree(children, indent + 1, text);
        }
        if let Some(patterns) = node.pointer("/plan/patterns").and_then(Value::as_array) {
            for pattern in patterns {
                let name = pattern["name"].as_str().unwrap_or("pattern");
                text.push_str(&format!("{}  pattern {name}:\n", "  ".repeat(indent)));
                if let Some(nodes) = pattern.get("nodes").and_then(Value::as_array) {
                    explain_tree(nodes, indent + 2, text);
                }
            }
        }
    }
}

/// Count concrete writer nodes by their requested alias, retaining unknown and
/// exact-model routes outside the advisory mix.
pub(super) fn writer_tier_mix(nodes: &[provenant_fabric_runtime::graph::GraphNode]) -> [usize; 3] {
    use provenant_fabric_runtime::graph::AccessMode;
    let mut counts = [0; 3];
    for node in nodes {
        if node.access == AccessMode::WorktreeWrite {
            let tier = node
                .route
                .as_ref()
                .and_then(|route| route.alias.as_deref())
                .or_else(|| {
                    node.routes
                        .as_ref()
                        .and_then(|routes| routes.first())
                        .and_then(|route| route.alias.as_deref())
                });
            if let Some(index) = ["junior", "senior", "principal"]
                .iter()
                .position(|name| Some(*name) == tier)
            {
                counts[index] += 1;
            }
        }
        for children in node
            .composite
            .iter()
            .map(|group| group.nodes.as_slice())
            .chain(node.loop_body.iter().map(|body| body.body.as_slice()))
        {
            let nested = writer_tier_mix(children);
            for (count, increment) in counts.iter_mut().zip(nested) {
                *count += increment;
            }
        }
    }
    counts
}

// Explain is read-only and must not publish an overflow artifact. The graph
// was already bounded by the owner's 8 MiB document limit in expand().
fn explain(
    expansion: Expansion,
    brief_warnings: Vec<String>,
    globals: &provenant_cli::Globals,
) -> u8 {
    use std::io::Write;
    let document: Result<provenant_fabric_runtime::graph::GraphDocument, String> =
        serde_json::from_value(expansion.document.clone()).map_err(|error| error.to_string());
    let inspection = (|| -> Result<_, String> {
        let roots = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Optional,
        )
        .map_err(|e| e.to_string())?;
        let config =
            provenant_config::read_with_domains(&roots.roots, &provenant_cli::domains::SECTIONS)
                .map_err(|e| e.to_string())?;
        let document = document.as_ref().map_err(Clone::clone)?;
        let limits = config.domains.fabric.graph.limits.clone();
        let admission = document.admit(&limits);
        let inspection = document.inspect(&limits);
        Ok((limits, admission, inspection))
    })();
    let (limits, admission, metrics) = match inspection {
        Ok((limits, Ok(()), Ok(metrics))) => {
            (json!(limits), json!({"status":"admitted"}), json!(metrics))
        }
        Ok((limits, Err(reason), metrics)) => (
            json!(limits),
            json!({"status":"refused","reason":reason}),
            metrics.map_or(Value::Null, |m| json!(m)),
        ),
        Ok((limits, Ok(()), Err(reason))) => (
            json!(limits),
            json!({"status":"admitted"}),
            json!({"unavailable":reason}),
        ),
        Err(error) => (
            Value::Null,
            json!({"status":"refused","reason":error}),
            Value::Null,
        ),
    };
    let pending = document
        .as_ref()
        .map_or_else(|_| Vec::new(), pending_checks);
    let mut human = format!(
        "Graph {}\nLimits: {}\nAdmission: {}\nInspection: {}\n",
        expansion.document["graph_id"], limits, admission, metrics
    );
    let tier_mix = document
        .as_ref()
        .map_or([0; 3], |document| writer_tier_mix(&document.nodes));
    let inverted = tier_mix[2] > tier_mix[1] || tier_mix[1] > tier_mix[0];
    let tier_mix_line = format!(
        "tier mix: junior {} / senior {} / principal {}",
        tier_mix[0], tier_mix[1], tier_mix[2]
    );
    human.push_str(&format!("{tier_mix_line}\n"));
    if inverted {
        human.push_str(
            "Warning: writer tier mix is inverted; consider whether lower tiers fit the work.\n",
        );
    }
    if let Some(context) = expansion.document.get("context") {
        human.push_str(&format!(
            "Preset selection: {} ({})\n",
            context["preset"].as_str().unwrap_or("-"),
            context["selection_source"].as_str().unwrap_or("-")
        ));
    }
    if let Some(shape) = &expansion.review_shape {
        human.push_str(&format!("Review shape: {shape}\n"));
    }
    if let Some(decision) = &expansion.plan {
        human.push_str(&format!("{}\n", decision.line()));
        if decision.requested != provenant_fabric_runtime::config::PlanMode::Off {
            human.push_str(&format!("Planner route: {}\n", decision.route));
        }
    }
    for item in &pending {
        human.push_str(&format!("Pending {}: {}\n", item["id"], item["reason"]));
    }
    if let Some(nodes) = expansion.document["nodes"].as_array() {
        explain_tree(nodes, 0, &mut human);
    }
    if !expansion.roles.is_empty() {
        human.push_str("Roles:\n");
        for (role, used) in &expansion.roles {
            human.push_str(&format!(
                "  {role}  origin={} digest={}\n",
                used["origin"].as_str().unwrap_or("-"),
                used["digest"].as_str().unwrap_or("-")
            ));
        }
    }
    for substitution in &expansion.substitutions {
        human.push_str(&format!(
            "Substituted {}: {} -> {} ({})\n",
            substitution["node"].as_str().unwrap_or("-"),
            substitution["requested"].as_str().unwrap_or("-"),
            substitution["used"].as_str().unwrap_or("-"),
            substitution["reason"].as_str().unwrap_or("-")
        ));
    }
    for warning in &expansion.warnings {
        human.push_str(&format!("Warning: {warning}\n"));
    }
    let mut data = json!({"roles": expansion.roles, "substitutions": expansion.substitutions, "graph": expansion.document, "origins": expansion.origins, "limits":limits,"admission":admission,"inspection":metrics,"pending":pending,"review_shape":expansion.review_shape,"plan":expansion.plan.as_ref().map(planner::Decision::to_json)});
    data["tier_mix"] = json!({"junior":tier_mix[0], "senior":tier_mix[1], "principal":tier_mix[2], "inverted":inverted, "line":tier_mix_line});
    if !brief_warnings.is_empty() {
        data["warnings"] = json!(brief_warnings);
    }
    human.push_str(&provenant_cli::run::warnings_text(&data));
    let output = if globals.json {
        serde_json::to_vec(&provenant_records::CommandResult::new(
            "run",
            provenant_records::Outcome::Ok,
            data,
        ))
    } else {
        Ok(human.into_bytes())
    };
    match output {
        Ok(mut bytes) => {
            bytes.push(b'\n');
            match std::io::stdout().lock().write_all(&bytes) {
                Ok(()) => 0,
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => 0,
                Err(error) => crate::fail(
                    "run",
                    provenant_records::Outcome::Error,
                    "IO",
                    &error.to_string(),
                    globals,
                ),
            }
        }
        Err(error) => crate::fail(
            "run",
            provenant_records::Outcome::Error,
            "IO",
            &error.to_string(),
            globals,
        ),
    }
}

pub fn command(command: provenant_cli::Preset, globals: &provenant_cli::Globals) -> u8 {
    use provenant_cli::Preset;
    let result = (|| {
        let layers = configured(globals)?;
        match command {
            Preset::List => Ok(json!({"presets": layers.listing()})),
            Preset::Explain {
                name: Some(name), ..
            } => layers.explain(&name),
            Preset::Explain {
                name: None,
                risk,
                kind,
                paths,
                size,
                ready,
                underspecified,
            } => {
                let resolved = crate::roots::resolve(
                    globals,
                    crate::roots::Workspace::Project,
                    crate::roots::Product::Optional,
                )
                .map_err(|error| error.to_string())?;
                let selection = crate::preset_select::select(
                    Some(&resolved.project.join(".provenant/preset-rules.toml")),
                    &crate::preset_select::SelectionContext {
                        risk,
                        kind,
                        paths,
                        size,
                        ready: underspecified.then_some(false).or(ready.then_some(true)),
                    },
                    None,
                )?;
                Ok(json!({
                    "preset": selection.choice.preset,
                    "parameters": selection.choice.parameters,
                    "source": selection.source.as_str(),
                    "matched": selection.matched,
                    "reason": match selection.source {
                        crate::preset_select::PresetSelectionSource::Readiness => "readiness rule matched an underspecified task",
                        crate::preset_select::PresetSelectionSource::Rule => "the highest-precedence matching selector rule was used",
                        crate::preset_select::PresetSelectionSource::Default => "no selector rule matched; the bundled or repo default applies",
                        crate::preset_select::PresetSelectionSource::Override => "an explicit preset override was used",
                        crate::preset_select::PresetSelectionSource::Triage => "the future triage source selected this preset",
                        crate::preset_select::PresetSelectionSource::Experiment => "a pipeline experiment assigned this preset",
                    }
                }))
            }
            Preset::Show { name, roles: true } => layers.roles_view(name.as_deref()),
            Preset::Show { name: None, .. } => Err("preset show needs a preset name".into()),
            Preset::Show {
                name: Some(name), ..
            } => {
                let expanded = expand(&layers, &name, "", &[])?;
                let (values, origins) = layers.resolve(&name)?;
                let mut origins = origins;
                origins.extend(expanded.origins);
                Ok(json!({"name":name,"values":values,"origins":origins}))
            }
        }
    })();
    report("preset", result, globals)
}

/// Parameter values from a `--params-file`: one TOML table of `name = value`.
fn params_file(path: &Path) -> Result<Vec<(String, Value)>, String> {
    let mut file = provenant_os::fs::FileHandle::open_source(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let bytes = file
        .read_bounded(1_048_576)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let table: Value =
        toml_edit::de::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(table
        .as_object()
        .ok_or("params file must be a table")?
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect())
}

fn parse_preset_override(item: &str) -> Result<(&str, Value), String> {
    let (path, raw) = item
        .split_once('=')
        .ok_or_else(|| format!("expected path=value: {item}"))?;
    let parsed = toml_edit::de::from_str(&format!("value = {raw}"))
        .map_err(|e| format!("{path}: {e}; string values need quotes, for example --set 'parameters.route=\"utility\"'"))?;
    Ok((path, parsed))
}

enum PresetInputError {
    Invalid(String),
    Secret,
}
impl From<String> for PresetInputError {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}
fn scan_preset_value(value: &Value) -> Result<(), PresetInputError> {
    match value {
        Value::String(text) => {
            provenant_run::Service::scan(text).map_err(|_| PresetInputError::Secret)
        }
        Value::Array(values) => values.iter().try_for_each(scan_preset_value),
        Value::Object(values) => values.iter().try_for_each(|(key, value)| {
            provenant_run::Service::scan(key).map_err(|_| PresetInputError::Secret)?;
            scan_preset_value(value)
        }),
        _ => Ok(()),
    }
}
// Path fields are execution context, not content sent as a user secret. Keep
// the rest of the document (including role cards and provenance) in the scan.
fn scan_preset_graph(document: &Value) -> Result<(), PresetInputError> {
    let mut content = document.clone();
    if let Some(nodes) = content.get_mut("nodes").and_then(Value::as_array_mut) {
        visit_nodes(nodes, &mut |node| {
            if let Some(object) = node.as_object_mut() {
                object.remove("worktree");
                object.remove("read_roots");
                object.remove("attachments");
                if let Some(review) = object.get_mut("review").and_then(Value::as_object_mut) {
                    review.remove("worktree");
                }
            }
            Ok(())
        })
        .map_err(PresetInputError::Invalid)?;
    }
    if let Some(subject) = content
        .pointer_mut("/provenance/resolved_subject")
        .and_then(Value::as_object_mut)
    {
        subject.remove("worktree");
    }
    // Origins record the filesystem location of preset and role definitions.
    if let Some(provenance) = content.get_mut("provenance").and_then(Value::as_object_mut) {
        if let Some(origins) = provenance.get_mut("origins").and_then(Value::as_object_mut) {
            for origin in origins.values_mut() {
                *origin = Value::Null;
            }
        }
        if let Some(roles) = provenance.get_mut("roles").and_then(Value::as_object_mut) {
            for role in roles.values_mut().filter_map(Value::as_object_mut) {
                role.remove("origin");
            }
        }
    }
    scan_preset_value(&content)
}
struct PresetInputs {
    bytes: Vec<u8>,
    params: Vec<(String, Value)>,
}

fn preset_inputs(request: &provenant_cli::Dispatch) -> Result<PresetInputs, PresetInputError> {
    let bytes = if let Some(path) = &request.input.prompt_file {
        let mut file = provenant_os::fs::FileHandle::open_source(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        file.read_bounded(1_048_576)
            .map_err(|e| format!("{}: {e}", path.display()))?
    } else if request.input.prompt_stdin {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > 1_048_576 {
            return Err(PresetInputError::Invalid(
                "prompt exceeds 1048576 bytes".into(),
            ));
        }
        bytes
    } else {
        return Err(PresetInputError::Invalid(
            "preset selection requires a prompt input".into(),
        ));
    };

    let prompt = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    provenant_run::Service::scan(prompt).map_err(|_| PresetInputError::Secret)?;
    let params = match &request.preset_params_file {
        Some(path) => params_file(path)?,
        None => Vec::new(),
    };
    for (_, value) in &params {
        scan_preset_value(value)?;
    }
    for item in &request.preset_set {
        let (_, parsed) = parse_preset_override(item)?;
        scan_preset_value(&parsed["value"])?;
    }
    Ok(PresetInputs { bytes, params })
}

fn prompt_ready(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    let named_surface = [
        "module", "surface", "crate", "package", "command", "src/", "docs/",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let risk_value = lower
        .split_once("risk tier:")
        .and_then(|(_, value)| value.split_whitespace().next())
        .unwrap_or("")
        .trim_matches(['.', ',', ';']);
    let risk_is_set = !matches!(risk_value, "" | "?" | "tbd" | "unknown" | "unset");
    let design_is_set = [
        "design decision is settled",
        "design decisions are resolved",
        "design decision: resolved",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    lower.contains("acceptance criteria")
        && named_surface
        && risk_is_set
        && lower.contains("verification")
        && design_is_set
        && ["test", "verify", "check"]
            .iter()
            .any(|marker| lower.contains(marker))
}

pub fn run(
    request: provenant_cli::Dispatch,
    globals: &provenant_cli::Globals,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
) -> u8 {
    run_with_review(request, globals, workers, false)
}

pub(crate) fn run_with_review(
    request: provenant_cli::Dispatch,
    globals: &provenant_cli::Globals,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
    review: bool,
) -> u8 {
    let PresetInputs {
        bytes,
        params: file_params,
    } = match preset_inputs(&request) {
        Ok(inputs) => inputs,
        Err(PresetInputError::Secret) => {
            return crate::fail(
                "run",
                provenant_records::Outcome::Refused,
                "SECRET_INPUT",
                "input matches a secret pattern; remove the secret and retry",
                globals,
            );
        }
        Err(PresetInputError::Invalid(error)) => {
            return crate::fail(
                "run",
                provenant_records::Outcome::Invalid,
                "preset",
                &error,
                globals,
            );
        }
    };
    let mut deferred_stderr = Vec::new();
    let result = (|| -> Result<(Expansion, Vec<String>), String> {
        let prompt = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        let directory = crate::roots::selected_project(globals.project.as_deref(), false)
            .map_err(|e| e.to_string())?;
        let rules_path = directory.join(".provenant/preset-rules.toml");
        let size = match bytes.len() {
            0..=16_384 => "small",
            16_385..=65_536 => "medium",
            _ => "large",
        };
        let ready = prompt_ready(prompt);
        let context = crate::preset_select::SelectionContext {
            risk: request.risk_tier.clone(),
            kind: request.task_kind.clone(),
            paths: request.selection_paths.clone(),
            size: Some(size.into()),
            ready: Some(ready),
        };
        let mut selection =
            crate::preset_select::select(Some(&rules_path), &context, request.preset.as_deref())?;
        let roots = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Optional,
        )
        .map_err(|e| e.to_string())?;
        let config =
            provenant_config::read_with_domains(&roots.roots, &provenant_cli::domains::SECTIONS)
                .map_err(|e| e.to_string())?;
        let mut layers = configured(globals)?;
        layers.graph_limits = config.domains.fabric.graph.limits.clone();
        layers.launch_dir = Some(directory.clone());
        // Default project selection may be a subdirectory of a checkout.
        layers.project_dir = Some(
            provenant_os::git::repository_path(
                &directory,
                provenant_os::git::RepositoryPath::Toplevel,
            )
            .unwrap_or_else(|_| directory.clone()),
        );
        layers.default_worktree = request.access.worktree.clone();
        let worktrees = project_worktrees(&directory);
        layers.command_worktrees = Some(worktrees.clone());
        let catalogue = provenant_routing::Catalogue::bundled().map_err(|e| e.to_string())?;
        layers.known_routes = Some(config.domains.routing.route_names(&catalogue));
        {
            let settings = &config.domains.fabric.plan;
            let mode = match request.preset_plan.as_deref() {
                Some(text) => provenant_fabric_runtime::config::PlanMode::parse(text)
                    .ok_or_else(|| format!("--plan must be auto, on or off: {text}"))?,
                None => settings.mode,
            };
            // Resolve the planner route the way dispatch does: configured pool,
            // `--exclude` and configured exclusions, account evidence and quota.
            let unavailable = (mode != provenant_fabric_runtime::config::PlanMode::Off)
                .then(|| {
                    provenant_cli::route::route_blocked(
                        &settings.route,
                        &request.route.exclude,
                        &roots.roots,
                    )
                })
                .flatten();
            layers.plan = Some(planner::PlanContext {
                mode,
                from_cli: request.preset_plan.is_some(),
                worktrees: linked_worktrees_from(&worktrees),
                route: settings.route.clone(),
                lane_deadline_seconds: config.domains.fabric.limits.graph_timeout_seconds,
                max_nodes: settings.max_nodes,
                auto: settings.auto.clone(),
                unavailable,
                paths: request.selection_paths.clone(),
            });
        }
        // Request values enter the review selector through declared parameters.
        layers.review = Some(review_selector(
            config.domains.routing.review.clone(),
            directory.clone(),
            None,
        ));
        // Experiments apply only to work items and never beside an explicit
        // preset. A malformed entry warns and is skipped; the others still apply.
        let placement = match (request.preset.is_none(), request.task_id.as_ref()) {
            (true, Some(work_item)) => {
                let settings: provenant_config::experiment::FabricExperiments =
                    provenant_config::read_domain(roots.roots.state.as_path(), "fabric").map_err(
                        |error| format!("cannot read experiment configuration: {error}"),
                    )?;
                let experiments = settings.pipelines.parsed();
                use provenant_core::Clock;
                let now = provenant_os::clock::SystemClock::new()
                    .wall_now()
                    .map_err(|error| error.to_string())?
                    .as_timestamp();
                let work_item = work_item.to_string();
                let task = crate::experiment::Task {
                    work_item: &work_item,
                    risk_tier: request.risk_tier.as_deref(),
                    rule_preset: selection.choice.preset.as_str(),
                    now,
                };
                let preset_check = |preset: &str| layers.resolve(preset).map(|_| ());
                // An arm is accepted only when the normal selection and expansion accept it.
                let arm_check = |preset: &str| {
                    let chosen = selection.choice.clone().for_preset(preset);
                    let (params, _) = dispatch_parameters(
                        &layers,
                        &chosen.preset,
                        chosen.parameters,
                        request.risk_tier.as_deref(),
                        request.task_kind.as_deref(),
                        &file_params,
                    )?;
                    expand_with(
                        &layers,
                        &chosen.preset,
                        prompt,
                        &params,
                        &request.preset_set,
                    )
                    .map(|_| ())
                };
                // Outcome rows are read once per run, and only when a budget or ratio needs them.
                let observed = || {
                    let stats = provenant_cli::Stats {
                        outcomes: true,
                        suggest: false,
                        pipeline: provenant_cli::PipelineOptions {
                            main_ref: "main".into(),
                            ci_file: None,
                            outcome_days: 14,
                            min_samples: 1,
                            expiry_days: 14,
                        },
                        claude_subagents: None,
                        usage: false,
                        since: None,
                        until: None,
                        run_id: None,
                        by: None,
                        detail: false,
                    };
                    let report = provenant_cli::run::stats(
                        stats,
                        &provenant_cli::run::Edge {
                            roots: &roots.roots,
                            home: &roots.home,
                            assets: &roots.assets,
                            cwd: &directory,
                        },
                    );
                    crate::experiment::observed_rows(&report.result.data)
                };
                crate::experiment::place(&experiments, &task, &preset_check, &arm_check, &observed)
            }
            _ => crate::experiment::Placement::default(),
        };
        {
            use std::io::Write;
            for warning in &placement.warnings {
                let _ = writeln!(deferred_stderr, "warning: {warning}");
            }
        }
        if let Some(assignment) = &placement.assignment {
            // Parameters stay only when the executed preset is the rule's preset.
            selection.choice = selection.choice.for_preset(&assignment.preset);
            selection.source = crate::preset_select::PresetSelectionSource::Experiment;
        }
        let name = selection.choice.preset.as_str();
        let (params, binding_warnings) = dispatch_parameters(
            &layers,
            name,
            selection.choice.parameters,
            request.risk_tier.as_deref(),
            request.task_kind.as_deref(),
            &file_params,
        )?;
        let explicit_subject = params.iter().any(|(key, _)| key == "subject")
            || request.preset_set.iter().any(|item| {
                item.split_once('=').is_some_and(|(path, _)| {
                    path == "parameters.subject" || path.starts_with("parameters.subject.")
                })
            });
        let mut expansion = expand_with(&layers, name, prompt, &params, &request.preset_set)?;
        if let Some(risk) = &request.risk_tier {
            expansion.document["context"]["risk_tier"] = json!(risk);
        }
        if let Some(kind) = &request.task_kind {
            expansion.document["context"]["task_kind"] = json!(kind);
        }
        expansion.warnings.extend(binding_warnings);
        expansion.document["provenance"]["warnings"] = json!(expansion.warnings);
        let mut spec = None;
        let (preset_config, _) = layers.resolve(name)?;
        let advice_parameter = tracker_advice_parameter(&preset_config);
        if let Some(parameter) = advice_parameter.filter(|_| !request.explain) {
            spec = params
                .iter()
                .find(|(key, _)| key == parameter)
                .and_then(|(_, value)| value.as_str())
                .map(str::to_owned);
            for item in &request.preset_set {
                if let Some(raw) = item.strip_prefix(&format!("parameters.{parameter}="))
                    && let Ok(value) =
                        toml_edit::de::from_str::<serde_json::Value>(&format!("value = {raw}"))
                {
                    spec = value
                        .get("value")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                }
            }
        }
        let advice_parameters = json!({"worktree": expansion.worktree});
        let advice_path = spec
            .as_deref()
            .and_then(|spec| advice_spec_path(&layers, &advice_parameters, spec));
        let mut bind = || {
            bind_default_subject_to_worktree(
                &mut expansion,
                &directory,
                explicit_subject,
                request.access.worktree.as_deref(),
            )
        };
        if let Some(spec) = spec {
            crate::tracker::advise_then(
                &directory,
                &spec,
                advice_path.as_deref(),
                &mut deferred_stderr,
                bind,
            )?;
        } else {
            bind()?;
        }
        if !placement.skipped.is_empty() {
            expansion.document["provenance"]["experiments"] = json!(placement.skipped);
        }
        let skills = roots.assets.join("skills");
        let mut policy_roots = vec![];
        if skills.is_dir() {
            policy_roots.push(skills.clone());
            // Nodes with `skill:` references may read the release skills, read-only.
            visit_nodes(
                expansion.document["nodes"]
                    .as_array_mut()
                    .ok_or("missing graph nodes")?,
                &mut |node| {
                    if (node.get("references").is_some() || node.get("review").is_some())
                        && node["access"] == "read_only"
                    {
                        let mut list = node
                            .get("read_roots")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        list.push(json!(skills));
                        node["read_roots"] = json!(list);
                    }
                    Ok(())
                },
            )?;
        }
        let policy = discover_read_roots(
            &worktrees,
            &config.domains.fabric.graph.allowed_read_roots,
            &policy_roots,
        );
        provenant_cli::route::validate_exclusions(&request.route.exclude, &roots.roots)?;
        let project_id = provenant_cli::run::preview_project_id(&roots.roots, &directory);
        let defaults = project_id
            .as_ref()
            .and_then(|id| config.domains.lanes.projects.get(&id.to_string()))
            .map(|settings| settings.capabilities.as_slice())
            .unwrap_or_default();
        bind_run_context(
            &mut expansion,
            &request,
            &directory,
            &policy,
            defaults,
            selection.source.as_str(),
            placement.assignment.as_ref(),
        )?;
        warn_differing_source_and_worktree(
            &mut expansion,
            &directory,
            request.access.worktree.as_deref(),
        )?;
        // Every bound worktree and read root counts as readable; the brief's
        // own paths are warned about, never refused.
        let mut effective = Vec::new();
        if let Some(worktree) = &request.access.worktree {
            effective.push(directory.join(worktree));
        }
        visit_nodes(
            expansion.document["nodes"]
                .as_array_mut()
                .ok_or("missing graph nodes")?,
            &mut |node| {
                let bound = node.get("worktree").and_then(Value::as_str).into_iter();
                let listed = node
                    .get("read_roots")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str);
                effective.extend(bound.chain(listed).map(|path| directory.join(path)));
                Ok(())
            },
        )?;
        let run_config =
            provenant_config::read_with_domains(&roots.roots, &provenant_cli::domains::SECTIONS)
                .map(|config| config.domains.run)
                .unwrap_or_default();
        let brief_warnings = if provenant_cli::run::brief_read_root_warnings_enabled(
            &run_config,
            &roots.roots,
            &directory,
        ) {
            provenant_cli::run::brief_read_root_warnings(
                prompt,
                &directory,
                None,
                &effective,
                &roots.home,
            )
        } else {
            Vec::new()
        };
        Ok((expansion, brief_warnings))
    })();
    let (expansion, brief_warnings) = match result {
        Ok(result) => result,
        Err(error) => {
            return crate::fail(
                "run",
                provenant_records::Outcome::Invalid,
                "preset",
                &error,
                globals,
            );
        }
    };
    if scan_preset_graph(&expansion.document).is_err() {
        return crate::fail(
            "run",
            provenant_records::Outcome::Refused,
            "SECRET_INPUT",
            "input matches a secret pattern; remove the secret and retry",
            globals,
        );
    }
    // Advice may include text outside the graph, such as the full spec.
    // Suppress matches without changing whether explain and launch accept it.
    if provenant_run::Service::scan(&String::from_utf8_lossy(&deferred_stderr)).is_err() {
        deferred_stderr.clear();
    }
    {
        use std::io::Write;
        let _ = std::io::stderr().lock().write_all(&deferred_stderr);
        for warning in &expansion.warnings {
            if provenant_run::Service::scan(warning).is_ok() {
                let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
            }
        }
    }
    if request.explain {
        explain(expansion, brief_warnings, globals)
    } else {
        if !expansion.repo_files.is_empty() {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "Repository preset: {}",
                expansion.repo_files.join(", ")
            );
        }
        crate::fabric::run_preset_graph(
            expansion.document,
            request.wait.unwrap_or(240),
            &brief_warnings,
            globals,
            workers,
            review,
        )
    }
}

#[cfg(test)]
mod tracker_spec_tests {
    #[allow(
        unused_imports,
        reason = "the worktree driver includes this module without libtest"
    )]
    use super::*;

    #[test]
    fn tracker_spec_uses_context_candidate_order() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("project");
        let writer = dir.path().join("writer");
        std::fs::create_dir(&project)?;
        std::fs::create_dir(&writer)?;
        std::fs::write(project.join("issue.md"), "project")?;
        std::fs::write(writer.join("issue.md"), "writer")?;
        let layers = Layers {
            project_dir: Some(project.clone()),
            launch_dir: Some(dir.path().into()),
            ..Layers::default()
        };
        let params = json!({"worktree": "writer"});
        assert_eq!(
            advice_spec_path(&layers, &params, "issue.md"),
            Some(writer.join("issue.md"))
        );
        std::fs::remove_file(writer.join("issue.md"))?;
        assert_eq!(
            advice_spec_path(&layers, &params, "issue.md"),
            Some(project.join("issue.md"))
        );
        Ok(())
    }
}

#[cfg(test)]
mod readiness_tests {
    #[allow(unused_imports)] // The worktree driver includes this module without libtest.
    use super::prompt_ready;

    #[test]
    fn readiness_requires_acceptance_risk_verification_and_design_markers() {
        let ready = "Acceptance criteria are explicit. Surface: the run command. Risk tier: low. Verification uses cargo test. Design decision is settled.";
        assert!(prompt_ready(ready));
        assert!(!prompt_ready("Improve the run command when possible."));
        assert!(!prompt_ready(
            "Acceptance criteria and tests are present in src/run.rs. Risk tier: low. Verification uses cargo test, but the design remains open."
        ));
        assert!(!prompt_ready(
            "Acceptance criteria in src/run.rs. Risk tier: TBD. Verification uses cargo test. Design decision is settled."
        ));
    }
}

#[cfg(test)]
mod template_lowering_tests {
    use serde_json::json;

    #[test]
    fn preset_nodes_lower_through_the_template_compiler() {
        let swarm = super::decode_lowered_node(
            &json!({"id": "map", "use": "explore", "over": ["crates/run", "src"]}),
        )
        .unwrap();
        let members = &swarm.composite.as_ref().unwrap().nodes;
        assert_eq!(members.len(), 2);
        assert!(members[1].prompt.contains("Map src"));
        let raw = super::decode_lowered_node(
            &json!({"id": "raw", "access": "read_only", "prompt": "as written"}),
        )
        .unwrap();
        assert_eq!(raw.prompt, "as written");
        let error =
            super::decode_lowered_node(&json!({"id": "bad", "use": "no-such-template"})).unwrap_err();
        assert!(error.contains("no-such-template"), "{error}");
    }
}
