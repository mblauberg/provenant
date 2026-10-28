use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

const NAMES: [&str; 4] = ["single", "chain", "fan-out", "fan-out-synthesise"];
const DEPTH: usize = 8;

#[derive(Default)]
pub struct Layers {
    entries: BTreeMap<String, Vec<(String, Value)>>,
}

#[derive(Debug)]
pub struct Expansion {
    pub document: Value,
    pub origins: BTreeMap<String, String>,
}

impl Layers {
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
            if let Err(error) = layers.insert("bundled", name, text) {
                debug_assert!(false, "invalid bundled preset: {error}");
            }
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
        for (origin, dir) in [("user", user), ("repo", repo)] {
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
                    .ok_or("invalid preset filename")?;
                let mut file = provenant_os::fs::FileHandle::open_source(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                let bytes = file
                    .read_bounded(1_048_576)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                let content =
                    String::from_utf8(bytes).map_err(|e| format!("{}: {e}", path.display()))?;
                layers.insert(origin, name, &content)?;
            }
        }
        Ok(layers)
    }

    pub fn listing(&self) -> Vec<Value> {
        self.entries.iter().map(|(name, layers)| {
            json!({"name": name, "layers": layers.iter().map(|(origin, _)| origin).collect::<Vec<_>>()})
        }).collect()
    }

    pub fn resolve(&self, name: &str) -> Result<(Value, BTreeMap<String, String>), String> {
        self.resolve_inner(name, &mut Vec::new())
    }

    fn resolve_inner(
        &self,
        name: &str,
        stack: &mut Vec<String>,
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
        let entries = self
            .entries
            .get(name)
            .ok_or_else(|| format!("unknown preset: {name}"))?;
        let parent = entries
            .iter()
            .rev()
            .find_map(|(_, value)| value.get("extends"));
        let (mut value, mut origins) = match parent {
            Some(parent) => self.resolve_inner(
                parent.as_str().ok_or("extends must be a preset name")?,
                stack,
            )?,
            None => (json!({}), BTreeMap::new()),
        };
        for (origin, layer) in entries {
            let mut layer = layer.clone();
            layer
                .as_object_mut()
                .ok_or("preset must be a table")?
                .remove("extends");
            merge(&mut value, &layer, "", origin, &mut origins)?;
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

pub fn expand(
    layers: &Layers,
    name: &str,
    prompt: &str,
    overrides: &[String],
) -> Result<Expansion, String> {
    let (mut config, mut origins) = layers.resolve(name)?;
    for item in overrides {
        let (path, raw) = item
            .split_once('=')
            .ok_or_else(|| format!("expected path=value: {item}"))?;
        let parsed: Value = toml_edit::de::from_str(&format!("value = {raw}"))
            .map_err(|e| format!("{path}: {e}"))?;
        override_value(
            &mut config,
            path,
            parsed["value"].clone(),
            &mut origins,
            "--set",
        )?;
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
    )?;
    let digest = provenant_records::CanonicalV1::file_digest(
        &serde_json::to_vec(&nodes).map_err(|e| e.to_string())?,
    )
    .to_string();
    let id = format!("preset-{}", &digest[7..39]);
    Ok(Expansion {
        document: json!({"schema_version":1,"graph_id":id,"nodes":nodes}),
        origins,
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
) -> Result<Vec<String>, String> {
    if stack.iter().any(|v| v == name) {
        return Err(format!("preset cycle: {} -> {name}", stack.join(" -> ")));
    }
    if stack.len() >= DEPTH {
        return Err(format!("preset expansion depth exceeds {DEPTH}"));
    }
    stack.push(name.into());
    let params = config
        .get("parameters")
        .and_then(Value::as_object)
        .ok_or("parameters must be a table")?;
    let slots = config
        .get("slots")
        .and_then(Value::as_object)
        .ok_or("slots must be a table")?;
    let shape = config
        .get("structure")
        .and_then(Value::as_str)
        .ok_or("missing preset structure; use extends")?;
    let allowed: &[&str] = match shape {
        "single" => &["task"],
        "chain" => &["first", "second"],
        "fan-out" => &["branch"],
        "fan-out-synthesise" => &["branch", "synthesis"],
        _ => return Err(format!("unknown preset structure: {shape}")),
    };
    if config.as_object().is_none_or(|m| {
        m.keys()
            .any(|k| k != "structure" && k != "parameters" && k != "slots")
    }) {
        return Err("unknown preset field".into());
    }
    if params
        .keys()
        .any(|k| k != "route" && !(k == "width" && shape.starts_with("fan-out")))
    {
        return Err("unknown parameter".into());
    }
    if slots.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err("unknown slot".into());
    }
    let route = params
        .get("route")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("parameters.route must be a nonempty string")?;
    let width = if shape.starts_with("fan-out") {
        params
            .get("width")
            .and_then(Value::as_u64)
            .filter(|n| (1..=32).contains(n))
            .ok_or("parameters.width must be 1 to 32")?
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
        let config_slot = slots
            .get(slot)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("missing slot: {slot}"))?;
        if config_slot
            .keys()
            .any(|k| k != "preset" && k != "parameters")
        {
            return Err(format!("unknown field in slot {slot}"));
        }
        let outputs = if let Some(child) = config_slot.get("preset") {
            let child = child.as_str().ok_or("slot preset must be a string")?;
            let (mut child_config, mut child_origins) = layers.resolve(child)?;
            if let Some(parameters) = config_slot.get("parameters") {
                for (key, value) in parameters
                    .as_object()
                    .ok_or("slot parameters must be a table")?
                {
                    let context = if prefix.is_empty() {
                        String::new()
                    } else {
                        format!("expanded.{}.", prefix.trim_end_matches('-'))
                    };
                    let origin = origins
                        .get(&format!("{context}slots.{slot}.parameters.{key}"))
                        .map(String::as_str)
                        .unwrap_or("slot");
                    override_value(
                        &mut child_config,
                        &format!("parameters.{key}"),
                        value.clone(),
                        &mut child_origins,
                        origin,
                    )?;
                }
            }
            for (key, origin) in child_origins {
                origins.insert(format!("expanded.{id}.{key}"), origin);
            }
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
            )?
        } else {
            if config_slot.contains_key("parameters") {
                return Err(format!("slot {slot} parameters require a preset"));
            }
            let mut node = json!({"id":id,"access":"read_only","prompt":prompt,"route":{"alias":route},"depends_on":incoming});
            if synthesis {
                node["kind"] = json!("synthesise");
            }
            if !incoming.is_empty() {
                node["inputs"] = json!(
                    incoming
                        .iter()
                        .map(|n| json!({"node":n,"outputs":["result"]}))
                        .collect::<Vec<_>>()
                );
            }
            nodes.push(node);
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

pub fn command(command: provenant_cli::Preset, globals: &provenant_cli::Globals) -> u8 {
    use provenant_cli::Preset;
    let result = (|| {
        let layers = configured(globals)?;
        match command {
            Preset::List => Ok(json!({"presets": layers.listing()})),
            Preset::Show { name } => {
                let (values, origins) = layers.resolve(&name)?;
                Ok(json!({"name":name,"values":values,"origins":origins}))
            }
        }
    })();
    report("preset", result, globals)
}

pub fn run(
    request: provenant_cli::Dispatch,
    globals: &provenant_cli::Globals,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
) -> u8 {
    let result = (|| {
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
        expand(&configured(globals)?, name, prompt, &request.preset_set)
    })();
    match result {
        Err(error) => crate::fail(
            "run",
            provenant_records::Outcome::Invalid,
            "preset",
            &error,
            globals,
        ),
        Ok(expansion) if request.explain => report(
            "run",
            Ok(json!({"graph":expansion.document,"origins":expansion.origins})),
            globals,
        ),
        Ok(expansion) => {
            match crate::fabric::retain_preset_graph(&expansion.document, globals, workers) {
                Ok(document) => crate::fabric::graph(
                    provenant_cli::fabric::Graph::Run { document },
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
