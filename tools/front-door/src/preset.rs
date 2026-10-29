use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Prompt files shared with the code-review skill, keyed by role `prompt_file`.
const PROMPT_FILES: [(&str, &str); 9] = [
    (
        "code-review/reviewer-context",
        include_str!("../../../skills/code-review/references/reviewer-context.md"),
    ),
    (
        "code-review/lenses/correctness-spec",
        include_str!("../../../skills/code-review/references/lenses/correctness-spec.md"),
    ),
    (
        "code-review/lenses/design-fit-reuse",
        include_str!("../../../skills/code-review/references/lenses/design-fit-reuse.md"),
    ),
    (
        "code-review/lenses/simplicity",
        include_str!("../../../skills/code-review/references/lenses/simplicity.md"),
    ),
    (
        "code-review/lenses/conventions-readability",
        include_str!("../../../skills/code-review/references/lenses/conventions-readability.md"),
    ),
    (
        "code-review/lenses/tests",
        include_str!("../../../skills/code-review/references/lenses/tests.md"),
    ),
    (
        "code-review/lenses/security-privacy",
        include_str!("../../../skills/code-review/references/lenses/security-privacy.md"),
    ),
    (
        "code-review/lenses/concurrency-data-errors",
        include_str!("../../../skills/code-review/references/lenses/concurrency-data-errors.md"),
    ),
    (
        "code-review/lenses/adversarial",
        include_str!("../../../skills/code-review/references/lenses/adversarial.md"),
    ),
];

fn prompt_file(name: &str) -> Result<&'static str, String> {
    PROMPT_FILES
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, text)| *text)
        .ok_or_else(|| {
            format!(
                "unknown prompt_file {name}; available: {}",
                PROMPT_FILES.map(|(key, _)| key).join(", ")
            )
        })
}

const NAMES: [&str; 4] = ["single", "chain", "fan-out", "fan-out-synthesise"];
const DEPTH: usize = 8;

/// The review shape selected for this run. Review shapes are canonical for who
/// reviews; presets compose them, so lens counts and tiers default from here.
#[derive(Clone, Debug, Default)]
pub struct ShapeHint {
    pub name: String,
    pub source: String,
    /// Reviewer pools in shape order, one entry per reviewer.
    pub slots: Vec<String>,
    /// Lens names a shape reviewer group names explicitly, with its pool.
    pub named: Vec<(String, String)>,
}

#[derive(Default)]
pub struct Layers {
    pub shape: Option<ShapeHint>,
    entries: BTreeMap<String, Vec<(String, Value)>>,
    errors: BTreeMap<String, Vec<String>>,
    repo_files: BTreeSet<String>,
}

#[derive(Debug)]
pub struct Expansion {
    pub review_shape: Option<String>,
    pub document: Value,
    pub origins: BTreeMap<String, String>,
    pub repo_files: Vec<String>,
}

impl Layers {
    #[allow(
        clippy::expect_used,
        reason = "bundled preset literals are built-in invariants"
    )]
    pub fn empty() -> Self {
        let mut layers = Self::default();
        for name in NAMES {
            let text = match name {
                "single" => "structure = 'single'\n[parameters]\nroute = 'senior'\n[slots.task]\n",
                "chain" => {
                    "structure = 'chain'\n[parameters]\nroute = 'senior'\n[slots.first]\n[slots.second]\n"
                }
                "fan-out" => {
                    "structure = 'fan-out'\n[parameters]\nroute = 'senior'\nwidth = 2\n[slots.branch]\n"
                }
                _ => {
                    "structure = 'fan-out-synthesise'\n[parameters]\nroute = 'senior'\nwidth = 2\n[slots.branch]\n[slots.synthesis]\n"
                }
            };
            // These literals are part of the binary and are checked by golden tests.
            layers
                .insert("bundled", name, text)
                .expect("valid bundled preset");
        }
        for (name, text) in [
            (
                "scout-swarm",
                include_str!("../assets/presets/scout-swarm.toml"),
            ),
            (
                "implement-with-lens",
                include_str!("../assets/presets/implement-with-lens.toml"),
            ),
            (
                "review-council",
                include_str!("../assets/presets/review-council.toml"),
            ),
            (
                "big-batch",
                include_str!("../assets/presets/big-batch.toml"),
            ),
        ] {
            layers
                .insert("bundled", name, text)
                .expect("valid bundled preset");
        }
        layers
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
                json!({"name": name, "layers": layers, "errors": errors})
            })
            .collect()
    }

    pub fn resolve(&self, name: &str) -> Result<(Value, BTreeMap<String, String>), String> {
        self.resolve_inner(name, &mut Vec::new(), &mut Vec::new())
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
        let entries = self
            .entries
            .get(name)
            .ok_or_else(|| format!("unknown preset: {name}"))?;
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
            layer
                .as_object_mut()
                .ok_or("preset must be a table")?
                .remove("extends");
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

fn merge(
    target: &mut Value,
    source: &Value,
    path: &str,
    origin: &str,
    origins: &mut BTreeMap<String, String>,
) -> Result<(), String> {
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

fn override_value(
    value: &mut Value,
    path: &str,
    replacement: Value,
    origins: &mut BTreeMap<String, String>,
    origin: &str,
) -> Result<(), String> {
    if let Some(rest) = path.strip_prefix("slots.")
        && let Some((slot, parameter)) = rest.split_once(".parameters.")
        && !slot.contains('.')
        && !parameter.contains('.')
        && !parameter.is_empty()
    {
        let slots = value
            .get_mut("slots")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| format!("unknown slot: {slot}"))?;
        let entry = slots
            .get_mut(slot)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| format!("unknown slot: {slot}"))?;
        if !entry.contains_key("preset") {
            return Err(format!("slot {slot} parameters require a preset"));
        }
        let params = entry
            .entry("parameters")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("slot parameters must be a table")?;
        params.insert(parameter.into(), replacement);
        origins.insert(path.into(), origin.into());
        return Ok(());
    }
    let mut current = value;
    let parts: Vec<_> = path.split('.').collect();
    for (index, part) in parts.iter().enumerate() {
        let map = current
            .as_object_mut()
            .ok_or_else(|| format!("unknown path: {path}"))?;
        if index + 1 == parts.len() {
            if !map.contains_key(*part)
                && !(*part == "preset" && parts.len() == 3 && parts[0] == "slots")
            {
                return Err(format!("unknown parameter or slot: {path}"));
            }
            map.insert((*part).into(), replacement);
            origins.insert(path.into(), origin.into());
            return Ok(());
        }
        current = map
            .get_mut(*part)
            .ok_or_else(|| format!("unknown parameter or slot: {path}"))?;
    }
    Err(format!("unknown path: {path}"))
}

fn valid_paths(config: &Value, layers: &Layers) -> String {
    let mut paths = Vec::new();
    if let Some(params) = config.get("parameters").and_then(Value::as_object) {
        paths.extend(params.keys().map(|key| format!("parameters.{key}")));
    }
    if let Some(slots) = config.get("slots").and_then(Value::as_object) {
        for (name, slot) in slots {
            paths.push(format!("slots.{name}.preset"));
            if let Some(child) = slot.get("preset").and_then(Value::as_str)
                && let Ok((child, _)) = layers.resolve(child)
                && let Some(params) = child.get("parameters").and_then(Value::as_object)
            {
                paths.extend(
                    params
                        .keys()
                        .map(|key| format!("slots.{name}.parameters.{key}")),
                );
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

fn invalid_config(
    name: &str,
    key: &str,
    message: &str,
    prefix: &str,
    origins: &BTreeMap<String, String>,
) -> String {
    let context = if prefix.is_empty() {
        String::new()
    } else {
        format!("expanded.{}.", prefix.trim_end_matches('-'))
    };
    let path = format!("{context}{key}");
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
    format!("preset {name}: {key} ({origin}): {message}")
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
        let (path, raw) = item
            .split_once('=')
            .ok_or_else(|| format!("expected path=value: {item}"))?;
        let parsed: Value = toml_edit::de::from_str(&format!("value = {raw}"))
            .map_err(|e| format!("{path}: {e}; string values need quotes, for example --set 'parameters.route=\"utility\"'"))?;
        if parsed["value"].is_object() {
            return Err(format!("{path}: --set requires a scalar or array value"));
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
    let digest = provenant_records::CanonicalV1::file_digest(
        &serde_json::to_vec(&nodes).map_err(|e| e.to_string())?,
    )
    .to_string();
    let id = format!("preset-{}", &digest[7..39]);
    check_expansion(&nodes, "", 0)?;
    let mut document = json!({"schema_version":1,"graph_id":id,"nodes":nodes});
    for key in ["limits", "integration_branch", "delivery"] {
        if let Some(value) = config.get(key) {
            let parameters = config
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({}));
            document[key] = interpolate(value, &parameters, prompt, 1)?;
        }
    }
    if serde_json::to_vec(&document)
        .map_err(|e| e.to_string())?
        .len()
        > 8_388_608
    {
        return Err("expanded graph exceeds the 8 MiB graph document limit".into());
    }
    Ok(Expansion {
        review_shape: layers
            .shape
            .as_ref()
            .map(|shape| format!("{} ({})", shape.name, shape.source)),
        document,
        origins,
        repo_files,
    })
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
    prefix: &str,
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
    if config.get("stages").is_some() {
        let result = build_stages(
            layers, name, config, prompt, prefix, deps, stack, nodes, origins, repo_files,
        );
        stack.pop();
        return result;
    }
    let params = config
        .get("parameters")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_config(name, "parameters", "must be a table", prefix, origins))?;
    let slots = config
        .get("slots")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_config(name, "slots", "must be a table", prefix, origins))?;
    let shape = config
        .get("structure")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            invalid_config(
                name,
                "structure",
                "missing structure; use extends",
                prefix,
                origins,
            )
        })?;
    let allowed: &[&str] = match shape {
        "single" => &["task"],
        "chain" => &["first", "second"],
        "fan-out" => &["branch"],
        "fan-out-synthesise" => &["branch", "synthesis"],
        _ => {
            return Err(invalid_config(
                name,
                "structure",
                &format!("unknown structure: {shape}"),
                prefix,
                origins,
            ));
        }
    };
    if let Some(key) = config.as_object().and_then(|m| {
        m.keys().find(|k| {
            ![
                "structure",
                "parameters",
                "slots",
                "roles",
                "limits",
                "integration_branch",
                "delivery",
                "exports",
            ]
            .contains(&k.as_str())
        })
    }) {
        return Err(invalid_config(name, key, "unknown field", prefix, origins));
    }
    if let Some(key) = params
        .keys()
        .find(|k| *k != "route" && !(*k == "width" && shape.starts_with("fan-out")))
    {
        return Err(invalid_config(
            name,
            &format!("parameters.{key}"),
            "unknown parameter",
            prefix,
            origins,
        ));
    }
    if let Some(key) = slots.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid_config(
            name,
            &format!("slots.{key}"),
            "unknown slot",
            prefix,
            origins,
        ));
    }
    let route = params
        .get("route")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            invalid_config(
                name,
                "parameters.route",
                "must be a nonempty string",
                prefix,
                origins,
            )
        })?;
    let width = if shape.starts_with("fan-out") {
        params
            .get("width")
            .and_then(Value::as_u64)
            .filter(|n| (1..=32).contains(n))
            .ok_or_else(|| {
                invalid_config(name, "parameters.width", "must be 1 to 32", prefix, origins)
            })?
    } else {
        1
    };
    let mut ends = Vec::new();
    let sequence: Vec<(String, &str, bool)> = match shape {
        "single" => vec![("task".into(), "task", false)],
        "chain" => vec![
            ("first".into(), "first", false),
            ("second".into(), "second", false),
        ],
        _ => (1..=width)
            .map(|i| (format!("branch-{i}"), "branch", false))
            .chain((shape == "fan-out-synthesise").then_some((
                "synthesis".into(),
                "synthesis",
                true,
            )))
            .collect(),
    };
    for (local, slot, synthesis) in sequence {
        let incoming = if (shape == "chain" && local == "second") || synthesis {
            ends.clone()
        } else {
            deps.to_vec()
        };
        let id = format!("{prefix}{local}");
        if id.len() > 64 {
            return Err(format!("expanded node ID exceeds 64 bytes: {id}"));
        }
        let config_slot = slots.get(slot).and_then(Value::as_object).ok_or_else(|| {
            invalid_config(
                name,
                &format!("slots.{slot}"),
                "missing slot or slot is not a table",
                prefix,
                origins,
            )
        })?;
        let outputs = if let Some(child) = config_slot.get("preset") {
            if let Some(key) = config_slot
                .keys()
                .find(|key| *key != "preset" && *key != "parameters")
            {
                return Err(invalid_config(
                    name,
                    &format!("slots.{slot}.{key}"),
                    "unknown preset slot field",
                    prefix,
                    origins,
                ));
            }
            let child = child.as_str().ok_or_else(|| {
                invalid_config(
                    name,
                    &format!("slots.{slot}.preset"),
                    "must be a preset name",
                    prefix,
                    origins,
                )
            })?;
            let (mut child_config, mut child_origins) =
                layers.resolve_inner(child, &mut Vec::new(), repo_files)?;
            if let Some(parameters) = config_slot.get("parameters") {
                for (key, value) in parameters.as_object().ok_or_else(|| {
                    invalid_config(
                        name,
                        &format!("slots.{slot}.parameters"),
                        "must be a table",
                        prefix,
                        origins,
                    )
                })? {
                    let context = if prefix.is_empty() {
                        String::new()
                    } else {
                        format!("expanded.{}.", prefix.trim_end_matches('-'))
                    };
                    let origin = origins
                        .get(&format!("{context}slots.{slot}.parameters.{key}"))
                        .map(String::as_str)
                        .unwrap_or("slot");
                    let valid = valid_paths(&child_config, layers);
                    override_value(
                        &mut child_config,
                        &format!("parameters.{key}"),
                        value.clone(),
                        &mut child_origins,
                        origin,
                    )
                    .map_err(|error| {
                        format!(
                            "{}; valid parameter and slot paths: {valid}",
                            invalid_config(
                                name,
                                &format!("slots.{slot}.parameters.{key}"),
                                &error,
                                prefix,
                                origins
                            )
                        )
                    })?;
                }
            }
            for (key, origin) in child_origins {
                origins.insert(format!("expanded.{id}.{key}"), origin);
            }
            if child_config.get("stages").is_some() {
                let mut children = Vec::new();
                let ends = build(
                    layers,
                    child,
                    &child_config,
                    prompt,
                    "",
                    &[],
                    stack,
                    &mut children,
                    origins,
                    repo_files,
                )?;
                let exports = child_config
                    .get("exports")
                    .cloned()
                    .unwrap_or_else(|| selected_inputs(&ends, &json!(["result", "findings"])));
                nodes.push(composite(
                    &id,
                    children,
                    exports,
                    &incoming,
                    &json!(["result"]),
                ));
                vec![id]
            } else {
                build(
                    layers,
                    child,
                    &child_config,
                    prompt,
                    &format!("{id}-"),
                    &incoming,
                    stack,
                    nodes,
                    origins,
                    repo_files,
                )?
            }
        } else {
            if config_slot.contains_key("parameters") {
                return Err(invalid_config(
                    name,
                    &format!("slots.{slot}.parameters"),
                    "requires a preset",
                    prefix,
                    origins,
                ));
            }
            let mut node = lower_leaf(
                config_slot,
                config.get("roles"),
                &json!({"route":route}),
                prompt,
                1,
                &id,
                &incoming,
            )?;
            if synthesis {
                node["kind"] = json!("synthesise");
            }
            nodes.push(node);
            if prompt.len().saturating_mul(nodes.len()) > 8_388_608 {
                return Err("expanded graph exceeds the 8 MiB graph document limit".into());
            }
            vec![id]
        };
        if shape == "chain" || synthesis || shape == "single" {
            ends = outputs;
        } else {
            ends.extend(outputs);
        }
        if nodes.len() > 512 {
            return Err("preset expands to more than 512 nodes".into());
        }
    }
    stack.pop();
    Ok(ends)
}

fn interpolate(
    value: &Value,
    parameters: &Value,
    prompt: &str,
    index: usize,
) -> Result<Value, String> {
    match value {
        Value::String(text) => {
            let lookup = |name: &str| match name {
                "prompt" => Ok(json!(prompt)),
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
            .map(|value| interpolate(value, parameters, prompt, index))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), interpolate(value, parameters, prompt, index)?)))
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

fn lower_leaf(
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
    if let Some(role) = role {
        profile = roles
            .and_then(|roles| roles.get(role))
            .cloned()
            .ok_or_else(|| format!("unknown preset role: {role}"))?;
        if !profile.is_object() {
            return Err(format!("role {role} must be a table"));
        }
    }
    merge(
        &mut profile,
        &Value::Object(entry.clone()),
        "",
        "entry",
        &mut BTreeMap::new(),
    )?;
    let mut node = interpolate(&profile, parameters, prompt, index)?;
    let map = node.as_object_mut().ok_or("leaf must be a table")?;
    for key in map.keys() {
        if ![
            "id",
            "role",
            "tier",
            "effort",
            "outputs",
            "access",
            "worktree",
            "title",
            "prompt",
            "kind",
            "plan",
            "loop",
            "composite",
            "condition",
            "on_fail",
            "delivery",
            "write_scope",
            "acceptance_checks",
            "inputs",
            "depends_on",
            "routes",
            "read_roots",
            "source",
            "prompt_file",
            "distinct_from",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("node {id}: unknown leaf field: {key}"));
        }
    }
    map.insert("id".into(), json!(id));
    if let Some(files) = map.remove("prompt_file") {
        let names: Vec<&str> = match &files {
            Value::String(name) => vec![name.as_str()],
            Value::Array(names) => names
                .iter()
                .map(|name| name.as_str().ok_or("prompt_file entries must be names"))
                .collect::<Result<_, _>>()?,
            _ => return Err("prompt_file must be a name or a list of names".into()),
        };
        let mut texts = Vec::new();
        for name in names.into_iter().filter(|name| !name.is_empty()) {
            texts.push(prompt_file(name)?);
        }
        if !texts.is_empty() {
            let brief = map
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or(prompt)
                .to_owned();
            map.insert(
                "prompt".into(),
                json!(format!("{brief}\n\n{}", texts.join("\n\n"))),
            );
        }
    }
    if map.get("distinct_from").is_some_and(|value| value == "") {
        map.remove("distinct_from");
    }
    if let Some(value) = map.get("distinct_from")
        && value != "writer"
    {
        return Err(format!("node {id}: distinct_from must be \"writer\""));
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
    if let Some(role) = role {
        map.entry("title").or_insert_with(|| json!(role));
    }
    let control = matches!(
        map.get("kind").and_then(Value::as_str),
        Some("gate" | "loop" | "composite")
    );
    if !control {
        map.entry("prompt").or_insert_with(|| json!(prompt));
        let tier = map.remove("tier");
        let effort = map.remove("effort");
        if tier.is_some() && map.contains_key("routes") {
            return Err(format!("node {id}: tier and routes are mutually exclusive"));
        }
        if map.contains_key("routes") {
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
            let mut route = match route {
                Value::String(name) if !name.is_empty() => json!({"alias":name}),
                Value::Object(_) => route,
                _ => {
                    return Err(format!("node {id}: tier must be a name or route table"));
                }
            };
            if let Some(effort) = effort {
                route["effort"] = effort;
            }
            map.insert("route".into(), route);
        }
    } else {
        map.remove("tier");
        map.remove("effort");
    }
    let outputs = map.remove("outputs").unwrap_or_else(|| json!(["result"]));
    if !deps.is_empty() && !map.contains_key("inputs") && !control {
        map.insert("inputs".into(), selected_inputs(deps, &outputs));
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

#[allow(
    clippy::too_many_arguments,
    reason = "preset compilation shares bounded expansion state"
)]
fn build_stages(
    layers: &Layers,
    name: &str,
    config: &Value,
    prompt: &str,
    prefix: &str,
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
            ]
            .contains(&key.as_str())
        })
    }) {
        return Err(invalid_config(name, key, "unknown field", prefix, origins));
    }
    let parameters = config
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !parameters.is_object() {
        return Err("parameters must be a table".into());
    }
    let mut builder = StageBuilder {
        layers,
        name,
        prompt,
        parameters,
        roles: config.get("roles"),
        stack,
        origins,
        repo_files,
    };
    builder.sequence(
        config.get("stages").ok_or("missing stages")?,
        prefix,
        deps,
        nodes,
        0,
    )
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
}

impl StageBuilder<'_> {
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

    /// Apply `when` triggers and the selected review shape to a lens list.
    /// An item without a tier takes it from the shape, and only such items count
    /// against the shape's reviewer total; a tier set in the preset is an override.
    fn select_items(&self, list: Vec<Value>) -> Result<Vec<Value>, String> {
        let tags: Vec<&str> = self
            .parameters
            .get("risk_tags")
            .and_then(Value::as_array)
            .map(|tags| tags.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let shape = self.layers.shape.as_ref();
        let limit = shape.map_or(usize::MAX, |shape| shape.slots.len().max(1));
        let mut governed = 0usize;
        let mut selected = Vec::new();
        for mut item in list {
            let when = item.get("when").and_then(Value::as_str).unwrap_or_default();
            if !when.is_empty() && !tags.contains(&when) {
                continue;
            }
            let tier = item.get("tier").and_then(Value::as_str).unwrap_or_default();
            if tier.is_empty() {
                let name = item["name"].as_str().unwrap_or_default().to_owned();
                if when.is_empty() {
                    if governed >= limit {
                        continue;
                    }
                    governed += 1;
                }
                let pool = shape.and_then(|shape| {
                    shape
                        .named
                        .iter()
                        .find(|(lens, _)| name == *lens || name.starts_with(&format!("{lens}-")))
                        .map(|(_, pool)| pool.clone())
                        .or_else(|| {
                            shape
                                .slots
                                .get(governed.saturating_sub(1))
                                .or(shape.slots.last())
                                .cloned()
                        })
                });
                item["tier"] = json!(pool.unwrap_or_else(|| "senior".into()));
            }
            selected.push(item);
        }
        if selected.is_empty() {
            return Err("for_each selected no items".into());
        }
        Ok(selected)
    }

    fn entry(
        &mut self,
        entry: &Value,
        default_id: &str,
        deps: &[String],
        nodes: &mut Vec<Value>,
        depth: usize,
    ) -> Result<Vec<String>, String> {
        if depth >= DEPTH {
            return Err(format!("preset composition depth exceeds {DEPTH}"));
        }
        let object = entry.as_object().ok_or("stage entries must be tables")?;
        let items: Vec<Option<Value>> = if let Some(list) = object.get("for_each") {
            if object.contains_key("count") {
                return Err("for_each and count are exclusive".into());
            }
            match interpolate(list, &self.parameters, self.prompt, 1)? {
                Value::Array(list) => self.select_items(list)?.into_iter().map(Some).collect(),
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
            .filter(|count| (1..=32).contains(count))
            .ok_or("entry count must be 1 to 32")?;
            (0..count).map(|_| None).collect()
        };
        let count = items.len();
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
                    let name = item["name"].as_str().filter(|n| valid_name(n)).ok_or(
                        "for_each items need a name of lowercase letters, digits and dashes",
                    )?;
                    format!("{base}-{name}")
                }
                None if count == 1 => base.to_owned(),
                None => format!("{base}-{index}"),
            };
            if nodes.iter().any(|node| node["id"] == id) {
                return Err(format!("duplicate stage node ID: {id}"));
            }
            if let Some(item) = item {
                self.parameters["item"] = item.clone();
            }
            let mut object = object.clone();
            object.remove("count");
            object.remove("for_each");
            object.remove("id");
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
                spec["body"] = json!(children);
                let mut node = json!({"id":id,"kind":"loop","access":"read_only","depends_on":deps,"loop":spec});
                if !deps.is_empty() {
                    node["inputs"] = selected_inputs(deps, &outputs);
                }
                (node, true)
            } else if let Some(child) = object.remove("preset") {
                let child = child.as_str().ok_or("preset must be a name")?;
                let (mut config, child_origins) =
                    self.layers
                        .resolve_inner(child, &mut Vec::new(), self.repo_files)?;
                if let Some(parameters) = object.remove("parameters") {
                    let parameters =
                        interpolate(&parameters, &self.parameters, self.prompt, index as usize)?;
                    for (key, value) in parameters
                        .as_object()
                        .ok_or("entry parameters must be a table")?
                    {
                        override_value(
                            &mut config,
                            &format!("parameters.{key}"),
                            value.clone(),
                            &mut BTreeMap::new(),
                            "entry",
                        )?;
                    }
                }
                if let Some(roles) = object.remove("roles") {
                    let roles = interpolate(&roles, &self.parameters, self.prompt, index as usize)?;
                    let target = config
                        .as_object_mut()
                        .ok_or("preset must be a table")?
                        .entry("roles")
                        .or_insert_with(|| json!({}));
                    merge(target, &roles, "roles", "entry", &mut BTreeMap::new())?;
                }
                for (key, origin) in child_origins {
                    self.origins.insert(format!("expanded.{id}.{key}"), origin);
                }
                let mut children = Vec::new();
                let ends = build(
                    self.layers,
                    child,
                    &config,
                    self.prompt,
                    "",
                    &[],
                    self.stack,
                    &mut children,
                    self.origins,
                    self.repo_files,
                )?;
                let exports = object
                    .remove("exports")
                    .or_else(|| config.get("exports").cloned())
                    .unwrap_or_else(|| selected_inputs(&ends, &json!(["result", "findings"])));
                (composite(&id, children, exports, deps, &outputs), true)
            } else if let Some(parallel) = object.remove("parallel") {
                let entries = parallel
                    .as_array()
                    .filter(|entries| !entries.is_empty())
                    .ok_or("parallel must be a nonempty array")?;
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
                let exports = object
                    .remove("exports")
                    .unwrap_or_else(|| selected_inputs(&ends, &json!(["result", "findings"])));
                (composite(&id, children, exports, deps, &outputs), true)
            } else if let Some(stages) = object.remove("stages") {
                let mut children = Vec::new();
                let ends = self.sequence(&stages, "", &[], &mut children, depth + 1)?;
                let exports = object
                    .remove("exports")
                    .unwrap_or_else(|| selected_inputs(&ends, &json!(["result", "findings"])));
                (composite(&id, children, exports, deps, &outputs), true)
            } else {
                (
                    lower_leaf(
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
            if decorate_wrapper {
                object.remove("outputs");
                for key in object.keys() {
                    if !["title", "prompt", "source", "read_roots"].contains(&key.as_str()) {
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
                for key in ["source", "read_roots"] {
                    if let Some(value) = object.get(key) {
                        node[key] =
                            interpolate(value, &self.parameters, self.prompt, index as usize)?;
                    }
                }
                if let Some(brief) = object.get("prompt") {
                    return Err(format!(
                        "composite {id}: brief belongs on its leaves, received {brief}"
                    ));
                }
            }
            if let Some(parameters) = self.parameters.as_object_mut() {
                parameters.remove("item");
            }
            nodes.push(node);
            if nodes.len() > 512 {
                return Err("preset expands to more than 512 nodes".into());
            }
            ends.push(id);
        }
        Ok(ends)
    }
}

fn composite(
    id: &str,
    nodes: Vec<Value>,
    exports: Value,
    deps: &[String],
    outputs: &Value,
) -> Value {
    let mut node = json!({"id":id,"kind":"composite","access":"read_only","depends_on":deps,"composite":{"nodes":nodes,"exports":exports}});
    if !deps.is_empty() {
        node["inputs"] = selected_inputs(deps, outputs);
    }
    node
}

fn check_expansion(nodes: &[Value], prefix: &str, depth: usize) -> Result<usize, String> {
    if depth > DEPTH {
        return Err(format!("preset composition depth exceeds {DEPTH}"));
    }
    let mut count = 0usize;
    for node in nodes {
        let id = format!(
            "{prefix}{}",
            node["id"].as_str().ok_or("node ID must be a string")?
        );
        if id.len() > 64 {
            return Err(format!("expanded node ID exceeds 64 bytes: {id}"));
        }
        count = count.saturating_add(1);
        if let Some(children) = node
            .get("composite")
            .and_then(|spec| spec.get("nodes"))
            .and_then(Value::as_array)
        {
            count = count.saturating_add(check_expansion(children, &format!("{id}--"), depth + 1)?);
        }
        if let Some(spec) = node.get("loop") {
            let children = spec
                .get("body")
                .and_then(Value::as_array)
                .ok_or("loop.body must be an array")?;
            let rounds = spec
                .get("max_rounds")
                .and_then(Value::as_u64)
                .ok_or("loop.max_rounds must be positive")? as usize;
            count = count.saturating_add(
                check_expansion(children, &format!("{id}--r{rounds}--"), depth + 1)?
                    .saturating_mul(rounds),
            );
        }
        if count > 512 {
            return Err("preset expands to more than 512 nodes".into());
        }
    }
    Ok(count)
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
    Layers::load(
        &config.join("provenant/presets"),
        &resolved.project.join(".provenant/presets"),
    )
}

fn report(command: &str, result: Result<Value, String>, globals: &provenant_cli::Globals) -> u8 {
    match result {
        Ok(data) => {
            let human = serde_json::to_string_pretty(&data).unwrap_or_default() + "\n";
            crate::emit(
                &provenant_records::CommandResult::new(
                    command,
                    provenant_records::Outcome::Ok,
                    data,
                ),
                globals,
                &human,
                true,
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
        for path in ["/composite/nodes", "/loop/body"] {
            if let Some(children) = node.pointer_mut(path).and_then(Value::as_array_mut) {
                visit_nodes(children, visitor)?;
            }
        }
    }
    Ok(())
}

/// Directories a graph node may read: the project's primary checkout and
/// registered worktrees, plus the host's explicit allow-list.
#[derive(Default)]
pub(crate) struct ReadRoots {
    project: Vec<std::path::PathBuf>,
    allowed: Vec<std::path::PathBuf>,
}

impl ReadRoots {
    pub(crate) fn new(project: Vec<std::path::PathBuf>, allowed: &[String]) -> Self {
        let canonical = |path: &std::path::PathBuf| std::fs::canonicalize(path).ok();
        Self {
            project: project.iter().filter_map(canonical).collect(),
            allowed: allowed
                .iter()
                .filter_map(|root| canonical(&root.into()))
                .collect(),
        }
    }

    pub(crate) fn discover(directory: &Path, allowed: &[String]) -> Result<Self, String> {
        // Outside a repository the project is only its own directory.
        let project = crate::handoff::git_text(directory, &["worktree", "list", "--porcelain"])
            .map(|listing| {
                listing
                    .lines()
                    .filter_map(|line| line.strip_prefix("worktree "))
                    .map(std::path::PathBuf::from)
                    .collect()
            })
            .unwrap_or_else(|_| vec![directory.to_path_buf()]);
        Ok(Self::new(project, allowed))
    }

    /// Resolve symlinks and `..` before comparing, and return the canonical path.
    pub(crate) fn confine(&self, path: &Path) -> Result<std::path::PathBuf, String> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| format!("read root {}: {e}", path.display()))?;
        if self
            .project
            .iter()
            .chain(&self.allowed)
            .any(|root| canonical.starts_with(root))
        {
            return Ok(canonical);
        }
        Err(format!(
            "read root {} is outside this project's registered worktrees; add it to [domains.fabric.graph] allowed_read_roots to permit it",
            canonical.display()
        ))
    }
}

/// Bind CLI context before hashing, explaining or retaining the executed graph.
pub(crate) fn bind_run_context(
    expansion: &mut Expansion,
    request: &provenant_cli::Dispatch,
    directory: &Path,
    policy: &ReadRoots,
) -> Result<(), String> {
    if let Some(concurrency) = request.concurrency {
        if expansion.document.get("limits").is_none() {
            expansion.document["limits"] = json!({});
        }
        expansion.document["limits"]["concurrency"] = json!(concurrency);
    }
    let nodes = expansion.document["nodes"]
        .as_array_mut()
        .ok_or("missing graph nodes")?;
    visit_nodes(nodes, &mut |node| {
        let container = node.get("composite").is_some() || node.get("loop").is_some();
        if !container && node["kind"] != "gate" {
            let writer = node["access"] == "worktree_write";
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
    let digest = provenant_records::CanonicalV1::file_digest(&identity).to_string();
    expansion.document["graph_id"] = json!(format!("preset-{}", &digest[7..39]));
    Ok(())
}

fn explain_tree(nodes: &[Value], indent: usize, text: &mut String) {
    for node in nodes {
        let field = |name: &str| node.get(name).and_then(Value::as_str).unwrap_or("-");
        let tier = node
            .pointer("/route/alias")
            .or_else(|| node.pointer("/route/model"))
            .and_then(Value::as_str)
            .unwrap_or("-");
        text.push_str(&format!(
            "{}{}  role={} tier={} kind={} access={} worktree={}\n",
            "  ".repeat(indent),
            field("id"),
            field("role"),
            tier,
            field("kind"),
            field("access"),
            field("worktree")
        ));
        for key in [
            "write_scope",
            "read_roots",
            "source",
            "inputs",
            "acceptance_checks",
            "required_verdict",
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
    }
}

// Explain is read-only and must not publish an overflow artifact. The graph
// was already bounded by the owner's 8 MiB document limit in expand().
fn explain(expansion: Expansion, globals: &provenant_cli::Globals) -> u8 {
    use std::io::Write;
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
        let document: provenant_fabric_runtime::graph::GraphDocument =
            serde_json::from_value(expansion.document.clone()).map_err(|e| e.to_string())?;
        let limits = document
            .limits
            .as_ref()
            .cloned()
            .unwrap_or_default()
            .resolve(&config.domains.fabric.graph)?;
        let inspection = document.inspect(&limits).map_err(str::to_owned);
        Ok((limits, inspection))
    })();
    let (limits, metrics, validation_error) = match inspection {
        Ok((limits, Ok(metrics))) => (json!(limits), json!(metrics), Value::Null),
        Ok((limits, Err(error))) => (json!(limits), Value::Null, json!(error)),
        Err(error) => (Value::Null, Value::Null, json!(error)),
    };
    let mut human = format!(
        "Graph {}\nLimits: {}\nExecution: {}\n",
        expansion.document["graph_id"], limits, metrics
    );
    if let Some(shape) = &expansion.review_shape {
        human.push_str(&format!("Review shape: {shape}\n"));
    }
    if !validation_error.is_null() {
        human.push_str(&format!("Requires configuration: {validation_error}\n"));
    }
    if let Some(nodes) = expansion.document["nodes"].as_array() {
        explain_tree(nodes, 0, &mut human);
    }
    let data = json!({"graph": expansion.document, "origins": expansion.origins, "limits":limits,"execution":metrics,"validation_error":validation_error,"review_shape":expansion.review_shape});
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
            Preset::Show { name } => {
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

/// A top-level string parameter as supplied by `--params-file` or `--set`.
fn requested_parameter(
    key: &str,
    params: &[(String, Value)],
    overrides: &[String],
) -> Option<String> {
    let mut found = params
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| value.as_str().map(str::to_owned));
    for item in overrides {
        if let Some((path, raw)) = item.split_once('=')
            && path == format!("parameters.{key}")
            && let Ok(parsed) = toml_edit::de::from_str::<Value>(&format!("value = {raw}"))
        {
            found = parsed["value"].as_str().map(str::to_owned);
        }
    }
    found.filter(|value| !value.is_empty())
}

pub fn run(
    request: provenant_cli::Dispatch,
    globals: &provenant_cli::Globals,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
) -> u8 {
    let result = (|| -> Result<Expansion, String> {
        let name = request.preset.as_deref().ok_or("missing preset")?;
        let path = request
            .input
            .prompt_file
            .as_ref()
            .ok_or("preset requires --prompt-file")?;
        let mut file = provenant_os::fs::FileHandle::open_source(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let bytes = file
            .read_bounded(1_048_576)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let prompt = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        let params = match &request.preset_params_file {
            Some(path) => params_file(path)?,
            None => Vec::new(),
        };
        let directory = crate::roots::selected_project(globals.project.as_deref(), false)
            .map_err(|e| e.to_string())?;
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
        let requested = |key: &str| requested_parameter(key, &params, &request.preset_set);
        let (named, risk) = (requested("review_shape"), requested("risk"));
        let selected = config.domains.routing.review.select_shape(
            named.as_deref(),
            risk.as_deref(),
            None,
            &[],
        )?;
        let mut hint = ShapeHint {
            name: selected.name.to_owned(),
            source: selected.source.to_owned(),
            ..Default::default()
        };
        for group in &selected.shape.reviewers {
            hint.slots
                .extend(std::iter::repeat_n(group.pool.clone(), group.count));
            for lens in &group.lenses {
                hint.named.push((lens.clone(), group.pool.clone()));
            }
        }
        layers.shape = Some(hint);
        let mut expansion = expand_with(&layers, name, prompt, &params, &request.preset_set)?;
        let policy =
            ReadRoots::discover(&directory, &config.domains.fabric.graph.allowed_read_roots)?;
        bind_run_context(&mut expansion, &request, &directory, &policy)?;
        Ok(expansion)
    })();
    match result {
        Err(error) => crate::fail(
            "run",
            provenant_records::Outcome::Invalid,
            "preset",
            &error,
            globals,
        ),
        Ok(expansion) if request.explain => explain(expansion, globals),
        Ok(expansion) => {
            if !expansion.repo_files.is_empty() {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "Repository preset: {}",
                    expansion.repo_files.join(", ")
                );
            }
            match crate::fabric::retain_preset_graph(&expansion.document, globals, workers) {
                Ok(document) => crate::fabric::run_preset_graph(
                    document,
                    request.wait.unwrap_or(240),
                    globals,
                    workers,
                ),
                Err(error) => crate::fail(
                    "run",
                    provenant_records::Outcome::Error,
                    "preset_graph",
                    &error.to_string(),
                    globals,
                ),
            }
        }
    }
}
