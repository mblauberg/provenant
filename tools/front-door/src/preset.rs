use crate::preset_roles::{RoleLibrary, prompt_file_reference};
use provenant_fabric_runtime::read_roots::ReadRoots;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const DEPTH: usize = 8;

/// The review shape selected for one review stage. Review shapes are canonical
/// for who reviews; presets compose them, so lens counts and tiers default from here.
/// Picks a review shape from a stage's `review_shape` and `risk_tier`. The host
/// owns routing, so it supplies this from its review configuration.
pub type ShapeSelector = Box<dyn Fn(Option<&str>, Option<&str>) -> Result<ShapeHint, String>>;

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
    /// Routing review shapes. Each review stage selects its own shape from its
    /// final `review_shape` and `risk_tier` parameters.
    pub review: Option<ShapeSelector>,
    used_shapes: std::cell::RefCell<BTreeSet<String>>,
    /// The role library: bundled roles, then user and repository `roles.toml`.
    pub roles: RoleLibrary,
    /// Named routes the routing configuration defines. `None` skips the
    /// check that a role's tier names a configured route.
    pub known_routes: Option<BTreeSet<String>>,
    used_roles: std::cell::RefCell<BTreeMap<String, Value>>,
    pub substitutions: std::cell::RefCell<Vec<Value>>,
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
    /// Library roles the graph used: origin, `extends` chain, digest and the
    /// content digest of each reference.
    pub roles: BTreeMap<String, Value>,
    /// Requested routes replaced by a fallback, for example a missing web route.
    pub substitutions: Vec<Value>,
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
            (
                "review-council",
                include_str!("../assets/presets/review-council.toml"),
            ),
            (
                "big-batch",
                include_str!("../assets/presets/big-batch.toml"),
            ),
            (
                "scope-then-run",
                include_str!("../assets/presets/scope-then-run.toml"),
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
        for dir in [user, repo] {
            // `roles.toml` sits beside the `presets` directory.
            if let Some(parent) = dir.parent() {
                layers.roles.add_file(&parent.join("roles.toml"))?;
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
            let resolved = self.roles.resolve(role, None)?;
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
        expand(self, name, "", &[])?;
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
        Ok(json!({
            "name": name,
            "pattern": config.get("pattern"),
            "parameters": parameters,
            "stages": stages,
            "roles": roles,
            "layers": self.entries.get(name).into_iter().flatten().map(|(origin, _)| origin).collect::<Vec<_>>(),
            "next": [
                format!("provenant run --preset {name} --prompt-file FILE --explain"),
                format!("provenant run --preset {name} --prompt-file FILE --set STAGE.PARAMETER=VALUE")
            ]
        }))
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

const SPEC_KEYS: [&str; 6] = ["type", "default", "min", "max", "enum", "doc"];
const TYPE_NAMES: [&str; 6] = ["string", "integer", "number", "boolean", "list", "table"];
/// Parameters every parent hands to a child preset that declares them.
const AMBIENT: [&str; 2] = ["risk_tier", "subject"];

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
    is_fragment(value) && value.get("default").is_some()
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
            .push(json!({"path": field.join("."), "value": replacement}));
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
    let parts: Vec<_> = path.split('.').collect();
    if parts.len() >= 2
        && parts[0] != "parameters"
        && parts[0] != "roles"
        && override_stage(value, path, &parts, &replacement, origins, origin)?
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
            if is_spec(existing) && !is_fragment(&replacement) {
                existing["default"] = replacement;
            } else {
                *existing = replacement;
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
        let default = value
            .get("default")
            .ok_or_else(|| fail("typed parameter needs a default".into()))?;
        if let Some(kind) = value.get("type")
            && !kind.as_str().is_some_and(|kind| TYPE_NAMES.contains(&kind))
        {
            return Err(fail(format!(
                "type must be one of {}",
                TYPE_NAMES.join(", ")
            )));
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
    layers.used_shapes.borrow_mut().clear();
    layers.used_roles.borrow_mut().clear();
    layers.substitutions.borrow_mut().clear();
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
    let parameters = flatten_parameters(
        name,
        config.get("parameters").unwrap_or(&json!({})),
        "",
        &origins,
    )?;
    for key in ["limits", "integration_branch", "delivery"] {
        if let Some(value) = config.get(key) {
            document[key] = interpolate(value, &parameters, prompt, 1)?;
        }
    }
    let roles: BTreeMap<String, Value> = layers.used_roles.take().into_iter().collect();
    let substitutions = layers.substitutions.take();
    for (role, used) in &roles {
        origins.insert(
            format!("roles.{role}"),
            used["origin"].as_str().unwrap_or("").into(),
        );
    }
    // Provenance rides in the graph document so it is hashed into the graph
    // identity and retained with the run: where each value came from, which
    // roles (with digests) were used and any route substituted.
    document["provenance"] = json!({
        "preset": name,
        "origins": origins,
        "roles": roles,
        "substitutions": substitutions,
    });
    if serde_json::to_vec(&document)
        .map_err(|e| e.to_string())?
        .len()
        > 8_388_608
    {
        return Err("expanded graph exceeds the 8 MiB graph document limit".into());
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
        origins,
        repo_files,
        roles,
        substitutions,
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
    stack.pop();
    result
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
        let resolved = layers.roles.resolve(role, roles)?;
        profile = resolved.profile.clone();
        resolved_role = Some(resolved);
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
            "card",
            "references",
            "family",
            "family_strength",
            "fallback_tier",
            "purpose",
            "dispositions",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("node {id}: unknown leaf field: {key}"));
        }
    }
    map.insert("id".into(), json!(id));
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
                "pattern",
            ]
            .contains(&key.as_str())
        })
    }) {
        return Err(invalid_config(name, key, "unknown field", scope, origins));
    }
    check_pattern(name, config.get("pattern"), scope, origins)?;
    let parameters = flatten_parameters(
        name,
        config.get("parameters").unwrap_or(&json!({})),
        scope,
        origins,
    )?;
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
    builder.sequence(
        config.get("stages").ok_or("missing stages")?,
        "",
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
    /// Stage path of this preset's caller, for diagnostics.
    scope: &'a str,
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

    /// The shape this stage's final parameters select, if shapes are configured.
    fn shape_hint(&self) -> Result<Option<ShapeHint>, String> {
        let Some(select) = &self.layers.review else {
            return Ok(None);
        };
        let text = |key: &str| {
            self.parameters
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        };
        let hint = select(text("review_shape"), text("risk_tier"))?;
        self.layers
            .used_shapes
            .borrow_mut()
            .insert(format!("{} ({})", hint.name, hint.source));
        Ok(Some(hint))
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
        let hint = self.shape_hint()?;
        let shape = hint.as_ref();
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
                        let value = interpolate(
                            &item["value"],
                            &self.parameters,
                            self.prompt,
                            index as usize,
                        )?;
                        passed.push(path.split('.').next().unwrap_or(path).to_owned());
                        override_value(
                            &mut config,
                            &format!("parameters.{path}"),
                            value,
                            &mut local,
                            &stage_origin(self.origins, path),
                        )
                        .map_err(|error| format!("stage {id}: {error}"))?;
                    }
                }
                for key in AMBIENT {
                    if let Some(value) = self.parameters.get(key)
                        && config.pointer(&format!("/parameters/{key}")).is_some()
                        && !passed.iter().any(|passed| passed == key)
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
                if let Some(roles) = object.remove("roles") {
                    let roles = interpolate(&roles, &self.parameters, self.prompt, index as usize)?;
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
            if decorate_wrapper {
                object.remove("outputs");
                for key in object.keys() {
                    if ![
                        "title",
                        "prompt",
                        "source",
                        "read_roots",
                        "inputs",
                        "depends_on",
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
                for key in ["source", "read_roots", "inputs"] {
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

/// The project's primary checkout and registered worktrees, which a graph node
/// may always read. Outside a repository the project is only its own directory.
fn discover_read_roots(directory: &Path, allowed: &[String], extra: &[PathBuf]) -> ReadRoots {
    let mut project: Vec<PathBuf> =
        crate::handoff::git_text(directory, &["worktree", "list", "--porcelain"])
            .map(|listing| {
                listing
                    .lines()
                    .filter_map(|line| line.strip_prefix("worktree "))
                    .map(std::path::PathBuf::from)
                    .collect()
            })
            .unwrap_or_else(|_| vec![directory.to_path_buf()]);
    project.extend(extra.iter().cloned());
    ReadRoots::new(project, allowed)
}

/// Bind CLI context before hashing, explaining or retaining the executed graph.
pub(crate) fn bind_run_context(
    expansion: &mut Expansion,
    request: &provenant_cli::Dispatch,
    directory: &Path,
    policy: &ReadRoots,
    selection_source: &str,
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
    expansion.document["work_context"] = json!({
        "preset": expansion.document["preset"],
        "selection_source": selection_source,
    });
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
    let graph_id = format!("preset-{}", &digest[7..39]);
    expansion.document["graph_id"] = json!(graph_id);
    expansion.document["work_context"]["graph_id"] = json!(graph_id);
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

fn explain_tree(nodes: &[Value], indent: usize, text: &mut String) {
    for node in nodes {
        let field = |name: &str| node.get(name).and_then(Value::as_str).unwrap_or("-");
        let tier = node
            .pointer("/route/alias")
            .or_else(|| node.pointer("/route/model"))
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
        let limits = config.domains.fabric.graph.limits.clone();
        let inspection = document.inspect(&limits);
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
    if let Some(context) = expansion.document.get("work_context") {
        human.push_str(&format!(
            "Preset selection: {} ({})\n",
            context["preset"].as_str().unwrap_or("-"),
            context["selection_source"].as_str().unwrap_or("-")
        ));
    }
    if let Some(shape) = &expansion.review_shape {
        human.push_str(&format!("Review shape: {shape}\n"));
    }
    if !validation_error.is_null() {
        human.push_str(&format!("Requires configuration: {validation_error}\n"));
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
    let data = json!({"roles": expansion.roles, "substitutions": expansion.substitutions, "graph": expansion.document, "origins": expansion.origins, "limits":limits,"execution":metrics,"validation_error":validation_error,"review_shape":expansion.review_shape});
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
                        crate::preset_select::PresetSelectionSource::Experiment => "the future experiment source selected this preset",
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
    let result = (|| -> Result<Expansion, String> {
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
                return Err("prompt exceeds 1048576 bytes".into());
            }
            bytes
        } else {
            return Err("preset selection requires a prompt input".into());
        };
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
        let selection = crate::preset_select::select(
            Some(&rules_path),
            &crate::preset_select::SelectionContext {
                risk: request.risk_tier.clone(),
                kind: request.task_kind.clone(),
                paths: request.selection_paths.clone(),
                size: Some(size.into()),
                ready: Some(ready),
            },
            request.preset.as_deref(),
        )?;
        let name = selection.choice.preset.as_str();
        let mut params = selection.choice.parameters.into_iter().collect::<Vec<_>>();
        match &request.preset_params_file {
            Some(path) => params_file(path)?,
            None => Vec::new(),
        }
        .into_iter()
        .for_each(|(key, value)| {
            if let Some(existing) = params.iter_mut().find(|(name, _)| name == &key) {
                existing.1 = value;
            } else {
                params.push((key, value));
            }
        });
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
        layers.known_routes = Some(config.domains.routing.routes.keys().cloned().collect());
        let review = config.domains.routing.review.clone();
        layers.review = Some(Box::new(move |named, risk| {
            let selected = review.select_shape(named, risk, None, &[])?;
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
            Ok(hint)
        }));
        let mut expansion = expand_with(&layers, name, prompt, &params, &request.preset_set)?;
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
                    if node.get("references").is_some() && node["access"] == "read_only" {
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
            &directory,
            &config.domains.fabric.graph.allowed_read_roots,
            &policy_roots,
        );
        bind_run_context(
            &mut expansion,
            &request,
            &directory,
            &policy,
            selection.source.as_str(),
        )?;
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
