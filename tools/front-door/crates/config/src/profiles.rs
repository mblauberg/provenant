//! Harness knob registry and resolution. All paths, environments and layers are explicit.
use crate::{
    invalid,
    ordered::{self, Json},
    Result,
};
use provenant_core::py::{python_json_eq, python_repr, python_strip};
use serde::{Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const REGISTRY: &str = "config/harness-knobs.json";
pub const USER_FILE: &str = "config/harness-profile.json";
pub const PROJECT_FILE: &str = ".agents/harness-profile.json";
pub const DEFAULT_PRESET: &str = "balanced";
pub const PRESET_ENV: &str = "PROVENANT_PROFILE";
pub const KNOB_ENV_PREFIX: &str = "PROVENANT_KNOB_";
pub const RISK_TIER_ENV: &str = "PROVENANT_RISK_TIER";
pub const PROFILE_ROOT_ENV: &str = "PROVENANT_PROFILE_ROOT";
const KNOB_FIELDS: &[&str] = &[
    "type",
    "default",
    "status",
    "reader",
    "effect",
    "trade_off",
    "min",
    "max",
    "nullable",
    "values",
    "floor",
    "project",
];
const LAYER_FIELDS: &[&str] = &[
    "schema_version",
    "description",
    "preset",
    "knobs",
    "task_classes",
];

pub fn env_name(knob: &str) -> String {
    format!(
        "{KNOB_ENV_PREFIX}{}",
        knob.chars()
            .map(|c| if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            })
            .collect::<String>()
    )
}

pub fn user_path(
    environment: &BTreeMap<String, String>,
    homes: &provenant_core::paths::Homes,
) -> Result<PathBuf> {
    let instance = environment
        .get(PROFILE_ROOT_ENV)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            environment
                .get("AGENT_FABRIC_INSTANCE_ROOT")
                .filter(|s| !s.is_empty())
        });
    let path = match instance {
        Some(value) => provenant_core::paths::expand_user(Path::new(value), homes)?,
        None => homes
            .current
            .as_ref()
            .ok_or(provenant_core::paths::PathError::Home)?
            .join(".agents"),
    };
    Ok(path.join(USER_FILE))
}
pub fn project_path(start: &Path) -> Result<PathBuf> {
    let start = crate::resolve(start)?;
    for directory in start.ancestors() {
        if directory.join(".git").exists() {
            return Ok(directory.join(PROJECT_FILE));
        }
    }
    Ok(start.join(PROJECT_FILE))
}

fn read(path: &Path) -> Result<Json> {
    let bytes = fs::read(path).map_err(|source| crate::Error::Read {
        context: "unreadable JSON",
        path: path.into(),
        source,
    })?;
    ordered::decode(&bytes, "unreadable JSON", path)
}
fn object<'a>(value: &'a Json, message: impl Into<String>) -> Result<&'a [(String, Json)]> {
    value.object().ok_or_else(|| invalid(message))
}
fn keys(value: &Json) -> Vec<&str> {
    value
        .object()
        .unwrap_or(&[])
        .iter()
        .map(|(k, _)| k.as_str())
        .collect()
}
fn equal(a: &Json, b: &Json) -> bool {
    python_json_eq(&a.value(), &b.value())
}
fn names(values: &[(String, Json)]) -> String {
    values
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}
fn integer(v: &Json) -> Option<i128> {
    match v {
        Json::Bool(v) => Some(i128::from(*v)),
        Json::Number(n) => n
            .as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from)),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct Registry {
    pub(crate) knobs: Vec<(String, Json)>,
    pub(crate) presets: Vec<(String, Json)>,
    pub tier_order: Vec<String>,
    pub task_classes: Vec<String>,
}
impl Registry {
    pub fn load(root: &Path) -> Result<Self> {
        let read_required = |relative: &str| match read(&root.join(relative)) {
            Err(crate::Error::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Err(invalid(format!(
                    "harness profile registry unavailable: {}",
                    root.join(relative).display()
                )))
            }
            other => other,
        };
        let document = read_required(REGISTRY)?;
        let risk = read_required("config/risk-policy.json")?;
        let routing = read_required("config/model-routing.json")?;
        let tier_order = risk["tier_order"]
            .array()
            .ok_or_else(|| invalid("config/risk-policy.json: tier_order missing"))?
            .iter()
            .map(|v| {
                v.string()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("config/risk-policy.json: tier_order missing"))
            })
            .collect::<Result<Vec<_>>>()?;
        let task_classes = keys(&routing["task_class_routes"])
            .into_iter()
            .map(str::to_owned)
            .collect();
        Self::from_document(document, tier_order, task_classes)
    }
    pub fn from_json(bytes: &[u8], tiers: Vec<String>, classes: Vec<String>) -> Result<Self> {
        Self::from_document(
            ordered::decode(bytes, "profile registry", Path::new(REGISTRY))?,
            tiers,
            classes,
        )
    }
    fn from_document(
        document: Json,
        tier_order: Vec<String>,
        mut task_classes: Vec<String>,
    ) -> Result<Self> {
        let knobs = document["knobs"]
            .object()
            .ok_or_else(|| invalid(format!("{REGISTRY}: needs knobs and presets objects")))?
            .to_vec();
        let presets = document["presets"]
            .object()
            .ok_or_else(|| invalid(format!("{REGISTRY}: needs knobs and presets objects")))?
            .to_vec();
        task_classes.sort();
        task_classes.dedup();
        for (name, spec) in &knobs {
            check_knob(name, spec, &tier_order)?;
        }
        let registry = Self {
            knobs,
            presets,
            tier_order,
            task_classes,
        };
        for (name, spec) in &registry.knobs {
            let default = spec
                .get("default")
                .ok_or_else(|| invalid(format!("{REGISTRY} knobs.{name}: default missing")))?;
            registry.check_json(name, default, &format!("{REGISTRY} knobs.{name}.default"))?;
            for (tier, value) in spec["floor"].object().unwrap_or(&[]) {
                registry.check_json(
                    name,
                    value,
                    &format!("{REGISTRY} knobs.{name}.floor.{tier}"),
                )?;
            }
        }
        if registry
            .preset(DEFAULT_PRESET)
            .is_none_or(|p| p["knobs"].truth())
        {
            return Err(invalid(format!(
                "{REGISTRY}: preset 'balanced' must exist and change nothing"
            )));
        }
        for (name, preset) in &registry.presets {
            registry.check_layer_json(preset, &format!("{REGISTRY} presets.{name}"), false)?;
        }
        Ok(registry)
    }
    fn spec(&self, name: &str) -> Option<&Json> {
        self.knobs.iter().find(|(n, _)| n == name).map(|(_, s)| s)
    }
    pub(crate) fn preset(&self, name: &str) -> Option<&Json> {
        self.presets.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }
    pub fn knob_names(&self) -> impl Iterator<Item = &str> {
        self.knobs.iter().map(|(n, _)| n.as_str())
    }
    pub fn preset_names(&self) -> impl Iterator<Item = &str> {
        self.presets.iter().map(|(n, _)| n.as_str())
    }
    fn check_json(&self, name: &str, value: &Json, origin: &str) -> Result<()> {
        let spec = self.spec(name).ok_or_else(|| {
            invalid(format!(
                "{origin}: unknown knob {}; `provenant profile list` names them",
                python_repr(name)
            ))
        })?;
        if matches!(value, Json::Null) && spec["nullable"].truth() {
            return Ok(());
        }
        match spec["type"].string() {
            Some("int") => {
                let valid = !matches!(value, Json::Bool(_))
                    && integer(value).is_some_and(|v| {
                        integer(&spec["min"]).is_some_and(|min| v >= min)
                            && integer(&spec["max"]).is_some_and(|max| v <= max)
                    });
                if !valid {
                    return Err(invalid(format!(
                        "{origin}: {name} takes an integer from {} to {}{}",
                        provenant_core::py::python_str(&spec["min"].value()),
                        provenant_core::py::python_str(&spec["max"].value()),
                        if spec["nullable"].truth() {
                            " or null"
                        } else {
                            ""
                        }
                    )));
                }
            }
            Some("enum") => {
                if !spec["values"]
                    .array()
                    .unwrap_or(&[])
                    .iter()
                    .any(|v| equal(v, value))
                {
                    let allowed = spec["values"]
                        .array()
                        .unwrap_or(&[])
                        .iter()
                        .map(|v| v.string().unwrap_or("<non-string>"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(invalid(format!("{origin}: {name} takes one of {allowed}")));
                }
            }
            _ => {
                if !matches!(value, Json::Bool(_)) {
                    return Err(invalid(format!("{origin}: {name} takes true or false")));
                }
            }
        }
        Ok(())
    }
    pub fn check(&self, name: &str, value: &Value, origin: &str) -> Result<Value> {
        self.check_json(name, &Json::from_value(value.clone()), origin)?;
        Ok(value.clone())
    }
    pub fn parse(&self, name: &str, raw: &str, origin: &str) -> Result<Value> {
        let Some(spec) = self.spec(name) else {
            return self.check(name, &Value::String(raw.into()), origin);
        };
        let text = python_strip(raw);
        let lower = text.to_lowercase();
        let value =
            if spec["nullable"].truth() && matches!(lower.as_str(), "null" | "none" | "default") {
                Value::Null
            } else if spec["type"].string() == Some("int") {
                parse_int(text).unwrap_or_else(|| Value::String(text.into()))
            } else if spec["type"].string() == Some("bool") {
                match lower.as_str() {
                    "true" | "1" | "on" => Value::Bool(true),
                    "false" | "0" | "off" => Value::Bool(false),
                    _ => Value::String(text.into()),
                }
            } else {
                Value::String(text.into())
            };
        self.check(name, &value, origin)
    }
    fn rank(&self, name: &str, value: &Json) -> Result<i128> {
        let spec = self
            .spec(name)
            .ok_or_else(|| invalid(format!("unknown knob {name}")))?;
        if spec["type"].string() == Some("enum") {
            spec["values"]
                .array()
                .unwrap_or(&[])
                .iter()
                .position(|v| equal(v, value))
                .map(|v| v as i128)
                .ok_or_else(|| invalid("enum value is outside registry"))
        } else {
            integer(value).ok_or_else(|| invalid("knob rank is not an integer"))
        }
    }
    pub fn check_layer(&self, bytes: &[u8], origin: &str, allow_presets: bool) -> Result<()> {
        self.check_layer_json(
            &ordered::decode(bytes, "profile layer", Path::new(origin))?,
            origin,
            allow_presets,
        )
    }
    fn check_layer_json(&self, layer: &Json, origin: &str, allow_presets: bool) -> Result<()> {
        let fields = object(layer, format!("{origin}: must be a JSON object"))?;
        let mut unknown = fields
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| !LAYER_FIELDS.contains(k))
            .collect::<Vec<_>>();
        unknown.sort();
        if !unknown.is_empty() {
            return Err(invalid(format!(
                "{origin}: unknown field(s) {}",
                unknown.join(", ")
            )));
        }
        self.check_part(layer, origin, allow_presets)?;
        if let Some(classes) = layer.get("task_classes") {
            for (task_class, part) in
                object(classes, format!("{origin}: task_classes must be an object"))?
            {
                let where_ = format!("{origin} task_classes.{task_class}");
                if !self.task_classes.contains(task_class) {
                    return Err(invalid(format!(
                        "{where_}: unknown task class; known: {}",
                        self.task_classes.join(", ")
                    )));
                }
                if part.object().is_none()
                    || keys(part).iter().any(|k| !matches!(*k, "preset" | "knobs"))
                {
                    return Err(invalid(format!("{where_}: takes only preset and knobs")));
                }
                self.check_part(part, &where_, allow_presets)?;
            }
        }
        Ok(())
    }
    fn check_part(&self, part: &Json, origin: &str, allow_presets: bool) -> Result<()> {
        if let Some(preset) = part.get("preset") {
            let preset = preset
                .string()
                .ok_or_else(|| invalid(format!("{origin}: preset must be a preset name")))?;
            if !allow_presets {
                return Err(invalid(format!(
                    "{origin}: a preset cannot name another preset"
                )));
            }
            if self.preset(preset).is_none() {
                return Err(invalid(format!(
                    "{origin}: unknown preset {}; known: {}",
                    python_repr(preset),
                    names(&self.presets)
                )));
            }
        }
        if let Some(knobs) = part.get("knobs") {
            for (name, value) in object(knobs, format!("{origin}: knobs must be an object"))? {
                self.check_json(name, value, origin)?;
            }
        }
        Ok(())
    }
    pub fn env_layer(&self, environment: &BTreeMap<String, String>) -> Result<Layer> {
        let names = self
            .knobs
            .iter()
            .map(|(n, _)| (env_name(n), n))
            .collect::<BTreeMap<_, _>>();
        let unknown = environment
            .keys()
            .filter(|k| k.starts_with(KNOB_ENV_PREFIX) && !names.contains_key(*k))
            .map(String::as_str)
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Err(invalid(format!(
                "unknown knob variable(s) {}; `provenant profile list` names the knobs",
                unknown.join(", ")
            )));
        }
        let mut knobs = Json::default();
        for (key, raw) in environment {
            if let Some(name) = names.get(key) {
                knobs.set(name.as_str(), Json::from_value(self.parse(name, raw, key)?))?;
            }
        }
        let mut layer = Json::default();
        layer.set("knobs", knobs)?;
        if let Some(preset) = environment.get(PRESET_ENV).filter(|v| !v.is_empty()) {
            layer.set("preset", Json::String(preset.clone()))?;
        }
        self.check_layer_json(&layer, PRESET_ENV, true)?;
        Ok(Layer {
            origin: "env".into(),
            document: layer,
        })
    }
    pub fn read_layer(&self, path: &Path, origin: &str) -> Result<Layer> {
        let document = match read(path) {
            Err(crate::Error::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Json::default()
            }
            other => other?,
        };
        self.check_layer_json(&document, &path.display().to_string(), true)?;
        Ok(Layer {
            origin: origin.into(),
            document,
        })
    }
    fn merge(&self, task_class: Option<&str>, layers: &[Layer]) -> Result<(String, Json, Json)> {
        let preset = layers
            .iter()
            .rev()
            .find_map(|layer| {
                class_part(&layer.document, task_class)["preset"]
                    .string()
                    .filter(|s| !s.is_empty())
                    .or_else(|| layer.document["preset"].string().filter(|s| !s.is_empty()))
            })
            .unwrap_or(DEFAULT_PRESET)
            .to_owned();
        let preset_layer = self.preset(&preset).ok_or_else(|| {
            invalid(format!(
                "unknown preset {}; known: {}",
                python_repr(&preset),
                names(&self.presets)
            ))
        })?;
        let mut knobs = Json::Object(
            self.knobs
                .iter()
                .map(|(n, s)| (n.clone(), s["default"].clone()))
                .collect(),
        );
        let mut sources = Json::Object(
            self.knobs
                .iter()
                .map(|(n, _)| (n.clone(), Json::String("default".into())))
                .collect(),
        );
        let preset_origin = format!("preset:{preset}");
        for (source, layer) in std::iter::once((preset_origin.as_str(), preset_layer))
            .chain(layers.iter().map(|l| (l.origin.as_str(), &l.document)))
        {
            for (part, suffix) in [
                (layer, String::new()),
                (
                    class_part(layer, task_class),
                    task_class.map(|t| format!(" ({t})")).unwrap_or_default(),
                ),
            ] {
                for (name, value) in part["knobs"].object().unwrap_or(&[]) {
                    self.check_json(name, value, source)?;
                    knobs.set(name, value.clone())?;
                    sources.set(name, Json::String(format!("{source}{suffix}")))?;
                }
            }
        }
        Ok((preset, knobs, sources))
    }
    pub fn resolve(
        &self,
        task_class: Option<&str>,
        risk_tier: Option<&str>,
        layers: &[Layer],
    ) -> Result<Resolution> {
        if let Some(tier) = risk_tier {
            if !self.tier_order.iter().any(|t| t == tier) {
                return Err(invalid(format!(
                    "unknown risk tier {}; known: {}",
                    python_repr(tier),
                    self.tier_order.join(", ")
                )));
            }
        }
        let (preset, mut knobs, mut sources) = self.merge(task_class, layers)?;
        if layers.iter().any(|l| l.origin == "project") {
            let trusted_layers = layers
                .iter()
                .filter(|l| l.origin != "project")
                .cloned()
                .collect::<Vec<_>>();
            let (_, trusted, trusted_sources) = self.merge(task_class, &trusted_layers)?;
            for (name, spec) in &self.knobs {
                if spec["project"].string() == Some("strengthen")
                    && self.rank(name, &trusted[name.as_str()])?
                        > self.rank(name, &knobs[name.as_str()])?
                {
                    knobs.set(name, trusted[name.as_str()].clone())?;
                    sources.set(
                        name,
                        Json::String(format!(
                            "{} (project may only strengthen)",
                            trusted_sources[name.as_str()].string().unwrap_or("default")
                        )),
                    )?;
                }
            }
        }
        let mut floored = vec![];
        if let Some(tier) = risk_tier {
            let position = self
                .tier_order
                .iter()
                .position(|t| t == tier)
                .ok_or_else(|| invalid("unknown risk tier"))?;
            for (name, spec) in &self.knobs {
                let mut floor: Option<&Json> = None;
                for (t, v) in spec["floor"].object().unwrap_or(&[]) {
                    if self
                        .tier_order
                        .iter()
                        .position(|tier| tier == t)
                        .is_some_and(|p| p <= position)
                        && floor.is_none_or(|f| self.rank(name, v).ok() > self.rank(name, f).ok())
                    {
                        floor = Some(v);
                    }
                }
                if let Some(floor) = floor {
                    if self.rank(name, &knobs[name.as_str()])? < self.rank(name, floor)? {
                        knobs.set(name, floor.clone())?;
                        sources.set(name, Json::String(format!("floor:{tier}")))?;
                        floored.push(name.clone());
                    }
                }
            }
        }
        let overrides = Json::Object(
            knobs
                .object()
                .unwrap_or(&[])
                .iter()
                .filter(|(n, v)| self.spec(n).is_some_and(|s| !equal(v, &s["default"])))
                .cloned()
                .collect(),
        );
        Ok(Resolution {
            preset,
            task_class: task_class.map(str::to_owned),
            risk_tier: risk_tier.map(str::to_owned),
            knobs,
            sources,
            overrides,
            floored,
        })
    }
}

fn check_knob(name: &str, spec: &Json, tiers: &[String]) -> Result<()> {
    let where_ = format!("{REGISTRY} knobs.{name}");
    if spec.object().is_none() || keys(spec).iter().any(|k| !KNOB_FIELDS.contains(k)) {
        return Err(invalid(format!("{where_}: unknown fields")));
    }
    if !matches!(spec["type"].string(), Some("int" | "enum" | "bool"))
        || !matches!(spec["status"].string(), Some("wired" | "advisory"))
    {
        return Err(invalid(format!(
            "{where_}: type must be int, enum or bool and status wired or advisory"
        )));
    }
    if ["reader", "effect", "trade_off"]
        .iter()
        .any(|k| spec[k].string().is_none_or(str::is_empty))
    {
        return Err(invalid(format!(
            "{where_}: reader, effect and trade_off are required"
        )));
    }
    if spec["type"].string() == Some("int")
        && ["min", "max"].iter().any(|k| integer(&spec[k]).is_none())
    {
        return Err(invalid(format!("{where_}: an int knob needs min and max")));
    }
    if spec["type"].string() == Some("enum") && spec["values"].array().is_none_or(|a| a.is_empty())
    {
        return Err(invalid(format!(
            "{where_}: an enum knob needs values, least rigorous first"
        )));
    }
    if spec.get("floor").is_some_and(|f| {
        f.object().is_none() || keys(f).iter().any(|k| !tiers.iter().any(|t| t == k))
    }) {
        return Err(invalid(format!(
            "{where_}: floor keys must be risk tiers ({})",
            tiers.join(", ")
        )));
    }
    if spec["floor"].truth() && spec["nullable"].truth() {
        return Err(invalid(format!(
            "{where_}: a nullable knob cannot carry a floor"
        )));
    }
    if spec
        .get("project")
        .is_some_and(|v| v.string() != Some("strengthen") || spec["nullable"].truth())
    {
        return Err(invalid(format!(
            "{where_}: project takes only \"strengthen\", on a knob that is not nullable"
        )));
    }
    Ok(())
}
fn class_part<'a>(layer: &'a Json, task_class: Option<&str>) -> &'a Json {
    task_class
        .filter(|s| !s.is_empty())
        .and_then(|t| layer["task_classes"].get(t))
        .unwrap_or(&Json::Null)
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub origin: String,
    pub(crate) document: Json,
}
impl Layer {
    pub fn from_json(origin: &str, bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            origin: origin.into(),
            document: ordered::decode(bytes, "profile layer", Path::new(origin))?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Resolution {
    pub preset: String,
    pub task_class: Option<String>,
    pub risk_tier: Option<String>,
    pub(crate) knobs: Json,
    pub(crate) sources: Json,
    pub(crate) overrides: Json,
    pub floored: Vec<String>,
}
impl Resolution {
    pub fn knob(&self, name: &str) -> Option<Value> {
        self.knobs.get(name).map(Json::value)
    }
    pub fn source(&self, name: &str) -> Option<&str> {
        self.sources.get(name).and_then(Json::string)
    }
    pub fn overrides(&self) -> Value {
        self.overrides.value()
    }
    pub fn digest(&self) -> Result<String> {
        let compact = compact_sorted(&self.overrides)?;
        Ok(crate::pending::hex(&Sha256::digest(compact.as_bytes()))[..12].into())
    }
    pub(crate) fn stamp_json(&self) -> Result<Json> {
        let mut stamp = Json::Object(vec![
            ("preset".into(), Json::String(self.preset.clone())),
            ("digest".into(), Json::String(self.digest()?)),
            (
                "task_class".into(),
                self.task_class
                    .clone()
                    .map(Json::String)
                    .unwrap_or(Json::Null),
            ),
            (
                "risk_tier".into(),
                self.risk_tier
                    .clone()
                    .map(Json::String)
                    .unwrap_or(Json::Null),
            ),
            ("knobs".into(), self.knobs.clone()),
        ]);
        if !self.floored.is_empty() {
            stamp.set(
                "floored",
                Json::Array(self.floored.iter().cloned().map(Json::String).collect()),
            )?;
        }
        Ok(stamp)
    }
    pub fn stamp(&self) -> Result<Value> {
        Ok(self.stamp_json()?.value())
    }
}
impl Serialize for Resolution {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.stamp_json()
            .map_err(serde::ser::Error::custom)?
            .serialize(s)
    }
}

// Canonical digest uses separators=(",", ":"), not the display encoder's spaces.
fn compact_sorted(value: &Json) -> Result<String> {
    match value {
        Json::Object(items) => {
            let mut items: Vec<_> = items.iter().collect();
            items.sort_by(|a, b| a.0.cmp(&b.0));
            Ok(format!(
                "{{{}}}",
                items
                    .into_iter()
                    .map(|(k, v)| Ok(format!(
                        "{}:{}",
                        crate::pending::json_string(k)?,
                        compact_sorted(v)?
                    )))
                    .collect::<Result<Vec<_>>>()?
                    .join(",")
            ))
        }
        Json::Array(items) => Ok(format!(
            "[{}]",
            items
                .iter()
                .map(compact_sorted)
                .collect::<Result<Vec<_>>>()?
                .join(",")
        )),
        _ => ordered::dumps(value, false, true),
    }
}

// port: belongs in provenant-core
fn parse_int(text: &str) -> Option<Value> {
    let (negative, digits) = if let Some(rest) = text.strip_prefix('-') {
        (true, rest)
    } else {
        (false, text.strip_prefix('+').unwrap_or(text))
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let mut ascii = String::new();
    if negative {
        ascii.push('-');
    }
    // Python int accepts Unicode decimal digits and single underscores.
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let code = c as u32;
        let digit = DECIMAL_STARTS
            .iter()
            .find_map(|start| code.checked_sub(*start).filter(|v| *v < 10))?;
        ascii.push(char::from(b'0' + digit as u8));
    }
    if let Ok(v) = ascii.parse::<i64>() {
        Some(Value::from(v))
    } else {
        ascii.parse::<u64>().ok().map(Value::from)
    }
}
// Unicode 16 Nd starts, including the five mathematical digit sets.
// port: belongs in provenant-core
const DECIMAL_STARTS: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
    0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50,
    0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8,
    0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
];

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap()
    }
    fn registry() -> Registry {
        Registry::load(&root()).unwrap()
    }
    fn layer(origin: &str, value: Value) -> Layer {
        Layer::from_json(origin, &serde_json::to_vec(&value).unwrap()).unwrap()
    }
    #[test]
    fn balanced_defaults_digest_and_insertion_order_match_python() {
        let registry = registry();
        let resolution = registry.resolve(None, None, &[]).unwrap();
        assert_eq!(resolution.preset, "balanced");
        assert_eq!(resolution.overrides(), serde_json::json!({}));
        assert_eq!(resolution.digest().unwrap(), "44136fa355b3");
        let Some(output)=crate::oracle::python("skills/_shared/harness_profile.py","import json\nr=m.resolve(m.load_registry(p))\nprint(json.dumps([r.stamp(),list(r.knobs),r.sources]))",&root())else{return;};
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            serde_json::json!([
                resolution.stamp().unwrap(),
                registry.knob_names().collect::<Vec<_>>(),
                resolution.sources.value()
            ])
        );
    }
    #[test]
    fn precedence_task_class_project_guard_and_floors_match_python() {
        let registry = registry();
        let cases = serde_json::json!([
            [null,null,[]],
            ["review","crucial",[["env",{"preset":"economy"}],["dispatch",{"knobs":{"effort.shift":-1}}]]],
            ["legwork",null,[["user",{"knobs":{"council.junior_size":2},"task_classes":{"legwork":{"preset":"economy","knobs":{"council.junior_size":6}}}}],["project",{"knobs":{"council.junior_size":4}}]]],
            ["review",null,[["user",{"preset":"rigorous"}],["project",{"preset":"economy","knobs":{"review.cross_family":"prefer"}}]]],
            [null,"terminal",[["user",{"preset":"economy"}],["project",{"preset":"rigorous"}],["env",{"preset":"balanced"}]]]
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cases");
        fs::write(&path, serde_json::to_vec(&cases).unwrap()).unwrap();
        let body=format!("import json\nr=m.load_registry(Path({:?}))\nprint(json.dumps([[x.stamp(),x.sources,x.overrides] for t,k,l in json.loads(p.read_text()) for x in [m.resolve(r,task_class=t,risk_tier=k,layers=l)]]))",root().to_str().unwrap());
        let Some(output) = crate::oracle::python("skills/_shared/harness_profile.py", &body, &path)
        else {
            return;
        };
        let mut actual = vec![];
        for case in cases.as_array().unwrap() {
            let layers = case[2]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| layer(v[0].as_str().unwrap(), v[1].clone()))
                .collect::<Vec<_>>();
            let resolved = registry
                .resolve(case[0].as_str(), case[1].as_str(), &layers)
                .unwrap();
            actual.push(serde_json::json!([
                resolved.stamp().unwrap(),
                resolved.sources.value(),
                resolved.overrides()
            ]));
        }
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            serde_json::json!(actual)
        );
    }
    #[test]
    fn environment_parsing_and_failure_edges_match_python() {
        let registry = registry();
        let cases = serde_json::json!([
            ["effort.shift", "\u{1c}-1\u{1f}"],
            ["effort.shift", "-١"],
            ["lanes.max_concurrency", "+0_4"],
            ["repair.max_cycles", "DEFAULT"],
            ["review.cross_family", "require"],
            ["effort.shift", "1.0"],
            ["effort.shift", "1__0"],
            ["effort.shift", "99999999999999999999999999"],
            ["missing", "True"],
            ["effort.shift", "2"],
            ["effort.shift", "-0"]
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cases");
        fs::write(&path, serde_json::to_vec(&cases).unwrap()).unwrap();
        let body=format!("import json\nr=m.load_registry(Path({:?}))\nout=[]\nfor n,v in json.loads(p.read_text()):\n try: out.append(['ok',r.parse(n,v,'env')])\n except m.ProfileError as e: out.append(['error',str(e)])\nprint(json.dumps(out))",root().to_str().unwrap());
        let Some(output) = crate::oracle::python("skills/_shared/harness_profile.py", &body, &path)
        else {
            return;
        };
        let actual = cases
            .as_array()
            .unwrap()
            .iter()
            .map(
                |c| match registry.parse(c[0].as_str().unwrap(), c[1].as_str().unwrap(), "env") {
                    Ok(v) => serde_json::json!(["ok", v]),
                    Err(e) => serde_json::json!(["error", e.to_string()]),
                },
            )
            .collect::<Vec<_>>();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            serde_json::json!(actual)
        );
        assert!(registry
            .check("effort.shift", &Value::Bool(true), "value")
            .is_err());
        assert!(registry
            .check("effort.shift", &serde_json::json!(1.0), "value")
            .is_err());
    }
    #[test]
    fn malformed_layers_and_registries_refuse_without_panicking() {
        let registry = registry();
        for value in [
            serde_json::json!([]),
            serde_json::json!({"knobs":null}),
            serde_json::json!({"preset":1}),
            serde_json::json!({"task_classes":[]}),
            serde_json::json!({"task_classes":{"review":[]}}),
            serde_json::json!({"knobs":{"effort.shift":true}}),
            serde_json::json!({"unknown":null}),
        ] {
            assert!(registry
                .check_layer(&serde_json::to_vec(&value).unwrap(), "file", true)
                .is_err());
        }
        assert!(registry.resolve(None, Some("unknown"), &[]).is_err());
        let mut document: Value =
            serde_json::from_slice(&fs::read(root().join(REGISTRY)).unwrap()).unwrap();
        document["knobs"]["effort.shift"]
            .as_object_mut()
            .unwrap()
            .remove("trade_off");
        assert!(Registry::from_json(
            &serde_json::to_vec(&document).unwrap(),
            registry.tier_order,
            registry.task_classes
        )
        .unwrap_err()
        .to_string()
        .contains("trade_off"));
    }
    #[test]
    fn profile_paths_use_pinned_instance_and_nearest_git() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        fs::create_dir_all(project.join(".git")).unwrap();
        fs::create_dir_all(project.join("a/b")).unwrap();
        assert_eq!(
            project_path(&project.join("a/b")).unwrap(),
            project.canonicalize().unwrap().join(PROJECT_FILE)
        );
        let homes = provenant_core::paths::Homes {
            current: Some(dir.path().into()),
            named: BTreeMap::from([("other".into(), "/other".into())]),
        };
        let mut env = BTreeMap::from([
            ("AGENT_FABRIC_INSTANCE_ROOT".into(), "~/instance".into()),
            ("AGENTS_HOME".into(), "/ignored".into()),
        ]);
        assert_eq!(
            user_path(&env, &homes).unwrap(),
            dir.path().join("instance").join(USER_FILE)
        );
        env.insert(PROFILE_ROOT_ENV.into(), "~other/pinned".into());
        assert_eq!(
            user_path(&env, &homes).unwrap(),
            Path::new("/other/pinned").join(USER_FILE)
        );
    }
}
