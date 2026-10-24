//! Harness knobs: the registry embedded from `config/harness-knobs.json`, the
//! optional `[knobs]` override table and the edits `config set` and
//! `config unset` make to the user's `config.toml`.
use crate::{ConfigError, Profile};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use toml_edit::{DocumentMut, Item, TableLike};

const REGISTRY: &str = include_str!("../../../../../config/harness-knobs.json");
/// TOML has no null, so an override that nulls a nullable knob is the string
/// `"null"`, which no integer knob could otherwise hold.
const NULL: &str = "null";

#[derive(Deserialize)]
struct Registry {
    knobs: BTreeMap<String, Spec>,
    presets: BTreeMap<String, Preset>,
}
#[derive(Deserialize)]
struct Spec {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    nullable: bool,
    min: Option<i64>,
    max: Option<i64>,
    #[serde(default)]
    values: Vec<String>,
    default: Value,
}
#[derive(Deserialize)]
struct Preset {
    #[serde(default)]
    knobs: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnobSource {
    Default,
    Profile,
    Override,
}
/// An effective knob value and the layer that supplied it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KnobValue {
    pub value: Value,
    pub source: KnobSource,
}

/// One change to the user's config file.
#[derive(Debug, Clone, Copy)]
pub enum KnobEdit<'a> {
    Profile(&'a str),
    Set { name: &'a str, value: &'a str },
    Unset(&'a str),
}

fn registry() -> Result<Registry, ConfigError> {
    serde_json::from_str(REGISTRY).map_err(|_| ConfigError::KnobRegistry)
}
fn profile_name(profile: Profile) -> &'static str {
    match profile {
        Profile::Balanced => "balanced",
        Profile::Economy => "economy",
        Profile::Rigorous => "rigorous",
    }
}
fn invalid(name: &str, rule: &'static str) -> ConfigError {
    ConfigError::KnobValue {
        name: name.into(),
        rule,
    }
}
fn spec<'a>(registry: &'a Registry, name: &str) -> Result<&'a Spec, ConfigError> {
    registry
        .knobs
        .get(name)
        .ok_or_else(|| unknown(registry, name))
}
fn unknown(registry: &Registry, name: &str) -> ConfigError {
    let near = |known: &&String| {
        known.contains(name) || name.contains(known.as_str()) || distance(known, name) <= 3
    };
    ConfigError::UnknownKnob {
        name: name.into(),
        close: registry.knobs.keys().filter(near).cloned().collect(),
    }
}
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(x != *y));
            diagonal = above;
        }
    }
    row[b.len()]
}

/// Check a value against the knob's type, range, enum values and nullability.
fn check(spec: &Spec, value: &Value) -> Result<(), &'static str> {
    match value {
        Value::Null if spec.nullable => Ok(()),
        Value::Null => Err("not_nullable"),
        Value::String(text) if spec.kind == "enum" => spec
            .values
            .contains(text)
            .then_some(())
            .ok_or("unknown_value"),
        Value::Number(number) if spec.kind == "int" => {
            let n = number.as_i64().ok_or("type")?;
            if spec.min.is_some_and(|min| n < min) || spec.max.is_some_and(|max| n > max) {
                Err("outside_allowed_range")
            } else {
                Ok(())
            }
        }
        _ => Err("type"),
    }
}
fn from_toml(item: &Item) -> Option<Value> {
    match item.as_value()? {
        toml_edit::Value::Integer(n) => Some(Value::from(*n.value())),
        toml_edit::Value::String(s) if s.value() == NULL => Some(Value::Null),
        toml_edit::Value::String(s) => Some(Value::from(s.value().as_str())),
        _ => None,
    }
}
fn parse_cli(spec: &Spec, text: &str) -> Result<Value, &'static str> {
    if text == NULL {
        return Ok(Value::Null);
    }
    if spec.kind == "int" {
        return text.parse::<i64>().map(Value::from).map_err(|_| "type");
    }
    Ok(Value::from(text))
}

/// Registry default, then the profile preset, then the user's `[knobs]`.
/// An error names the offending knob key and rule.
pub(crate) fn resolve(
    profile: Profile,
    overrides: Option<&dyn TableLike>,
) -> Result<BTreeMap<String, KnobValue>, (String, &'static str)> {
    let registry = registry().map_err(|_| (String::new(), "registry"))?;
    let preset = registry.presets.get(profile_name(profile));
    let mut knobs = BTreeMap::new();
    for (name, spec) in &registry.knobs {
        let from_preset = preset.and_then(|p| p.knobs.get(name));
        knobs.insert(
            name.clone(),
            match from_preset {
                Some(value) => KnobValue {
                    value: value.clone(),
                    source: KnobSource::Profile,
                },
                None => KnobValue {
                    value: spec.default.clone(),
                    source: KnobSource::Default,
                },
            },
        );
    }
    for (name, item) in overrides.into_iter().flat_map(|table| table.iter()) {
        let key = format!("knobs.{name}");
        let spec = registry
            .knobs
            .get(name)
            .ok_or((key.clone(), "unknown_key"))?;
        let value = from_toml(item).ok_or((key.clone(), "type"))?;
        check(spec, &value).map_err(|rule| (key, rule))?;
        knobs.insert(
            name.into(),
            KnobValue {
                value,
                source: KnobSource::Override,
            },
        );
    }
    Ok(knobs)
}

/// Apply `edit` to the config text, keeping every other byte, comment and
/// key order. An absent file starts from the minimal valid header.
fn apply(text: Option<&str>, edit: KnobEdit) -> Result<String, ConfigError> {
    let registry = registry()?;
    let mut doc: DocumentMut = text
        .unwrap_or("schema_version = 1\n")
        .parse()
        .map_err(|_| ConfigError::KnobRegistry)?;
    match edit {
        KnobEdit::Profile(name) => {
            if !registry.presets.contains_key(name) {
                return Err(invalid("profile", "unknown_value"));
            }
            doc["profile"] = toml_edit::value(name);
        }
        KnobEdit::Set { name, value } => {
            let spec = spec(&registry, name)?;
            let value = parse_cli(spec, value).map_err(|rule| invalid(name, rule))?;
            check(spec, &value).map_err(|rule| invalid(name, rule))?;
            let item = match &value {
                Value::Null => toml_edit::value(NULL),
                Value::Number(n) => toml_edit::value(n.as_i64().unwrap_or_default()),
                Value::String(s) => toml_edit::value(s.as_str()),
                _ => return Err(invalid(name, "type")),
            };
            let knobs = doc
                .entry("knobs")
                .or_insert_with(toml_edit::table)
                .as_table_mut()
                .ok_or_else(|| invalid("knobs", "type"))?;
            knobs.insert(name, item);
        }
        KnobEdit::Unset(name) => {
            spec(&registry, name)?;
            if let Some(knobs) = doc.get_mut("knobs").and_then(Item::as_table_mut) {
                knobs.remove(name);
            }
        }
    }
    Ok(doc.to_string())
}

/// Read, edit and atomically replace `<state>/config.toml` (mode 0600). The
/// result is validated by the caller re-reading the file through `config_show`.
pub fn edit_config(
    roots: &provenant_core::Roots,
    fs: &impl provenant_supervisor::fs::SafeFs,
    edit: KnobEdit,
) -> Result<(), ConfigError> {
    let path = roots.state.as_path().join("config.toml");
    let text = crate::loader::open(roots, fs)?
        .map(|reader| crate::loader::bounded_text(reader, &path))
        .transpose()?;
    let updated = apply(text.as_deref(), edit)?;
    let state = fs.open_anchor(roots.state.as_path())?;
    state.atomic_write(
        std::ffi::OsStr::new("config.toml"),
        ".config-",
        updated.as_bytes(),
        0o600,
        provenant_supervisor::fs::PublishMode::Replace,
    )?;
    Ok(())
}
