//! Harness knobs: the registry embedded from `config/harness-knobs.json`, the
//! optional `[knobs]` override table and the edits `config set` and
//! `config unset` make to the user's `config.toml`, plus the one config editor
//! that every command uses to rewrite that file.
use crate::{ConfigError, Profile};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::OnceLock};
use toml_edit::{DocumentMut, Item, TableLike};

const REGISTRY: &str = include_str!("../../../../../config/harness-knobs.json");

#[derive(Deserialize)]
pub(crate) struct Registry {
    pub(crate) knobs: BTreeMap<String, Spec>,
    pub(crate) presets: BTreeMap<String, Preset>,
}
#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Bool,
    StringList,
    Enum,
    Int,
}
#[derive(Deserialize)]
pub(crate) struct Spec {
    #[serde(rename = "type")]
    pub(crate) kind: Kind,
    pub(crate) min: Option<i64>,
    pub(crate) max: Option<i64>,
    #[serde(default)]
    pub(crate) values: Vec<String>,
    pub(crate) default: Value,
    /// Grants or bounds capability or authority, so an invalid value refuses
    /// the whole configuration rather than recovering to the default.
    #[serde(default)]
    strict: bool,
}
#[derive(Deserialize)]
pub(crate) struct Preset {
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

impl<D> crate::ResolvedConfig<D> {
    /// Whether the user chose `name`, in `[knobs]` or as a dispatch override,
    /// as opposed to the registry default or the profile preset.
    pub fn knob_is_set(&self, name: &str, overrides: &[String]) -> bool {
        overrides
            .iter()
            .any(|entry| entry.split_once('=').is_some_and(|(key, _)| key == name))
            || self
                .knobs
                .get(name)
                .is_some_and(|knob| knob.source == KnobSource::Override)
    }

    /// Resolve a task's final policy without changing the config file.
    pub fn dispatch_profile(
        &self,
        overrides: &[String],
    ) -> Result<provenant_records::run::HarnessProfile, ConfigError> {
        let registry = registry()?;
        let mut knobs = self.knobs.clone();
        for entry in overrides {
            let (name, text) = entry
                .split_once('=')
                .ok_or_else(|| invalid(entry, "expected_name_value"))?;
            let spec = spec(registry, name)?;
            let value = parse_cli(spec, text).map_err(|rule| invalid(name, rule))?;
            check(spec, &value).map_err(|rule| invalid(name, rule))?;
            knobs.insert(
                name.into(),
                KnobValue {
                    value,
                    source: KnobSource::Override,
                },
            );
        }
        Ok(provenant_records::run::HarnessProfile {
            name: profile_name(self.profile).into(),
            knobs: knobs
                .into_iter()
                .map(|(name, knob)| (name, knob.value))
                .collect(),
            overrides: overrides.to_vec(),
            ..Default::default()
        })
    }
}

/// Parsed once; the embedded text is immutable.
pub(crate) fn registry() -> Result<&'static Registry, ConfigError> {
    static PARSED: OnceLock<Option<Registry>> = OnceLock::new();
    PARSED
        .get_or_init(|| serde_json::from_str(REGISTRY).ok())
        .as_ref()
        .ok_or(ConfigError::KnobRegistry)
}
fn profile_name(profile: Profile) -> &'static str {
    profile.as_str()
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
        Value::Bool(_) if spec.kind == Kind::Bool => Ok(()),
        Value::Array(items) if spec.kind == Kind::StringList => {
            if items.len() <= 128
                && items.iter().all(|v| {
                    v.as_str().is_some_and(|s| {
                        !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
                    })
                })
            {
                Ok(())
            } else {
                Err("type")
            }
        }
        Value::String(text) if spec.kind == Kind::Enum => spec
            .values
            .contains(text)
            .then_some(())
            .ok_or("unknown_value"),
        Value::Number(number) if spec.kind == Kind::Int => {
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
        toml_edit::Value::Boolean(b) => Some(Value::Bool(*b.value())),
        toml_edit::Value::Array(items) => items
            .iter()
            .map(|v| v.as_str().map(Value::from))
            .collect::<Option<Vec<_>>>()
            .map(Value::Array),
        toml_edit::Value::Integer(n) => Some(Value::from(*n.value())),
        toml_edit::Value::String(s) => Some(Value::from(s.value().as_str())),
        _ => None,
    }
}
fn parse_cli(spec: &Spec, text: &str) -> Result<Value, &'static str> {
    if matches!(spec.kind, Kind::Bool | Kind::StringList) {
        return serde_json::from_str(text).map_err(|_| "type");
    }
    if spec.kind == Kind::Int {
        return text.parse::<i64>().map(Value::from).map_err(|_| "type");
    }
    Ok(Value::from(text))
}

/// Whether an invalid value of the knob must refuse the configuration.
pub(crate) fn strict(name: &str) -> bool {
    registry().is_ok_and(|registry| registry.knobs.get(name).is_some_and(|spec| spec.strict))
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

impl KnobEdit<'_> {
    /// Apply the edit to the config document, keeping every other byte,
    /// comment and key order.
    pub fn apply(self, doc: &mut DocumentMut) -> Result<(), ConfigError> {
        apply(doc, self)
    }
}
fn apply(doc: &mut DocumentMut, edit: KnobEdit) -> Result<(), ConfigError> {
    let registry = registry()?;
    match edit {
        KnobEdit::Profile(name) => {
            if !registry.presets.contains_key(name) {
                return Err(invalid("profile", "unknown_value"));
            }
            doc["profile"] = toml_edit::value(name);
        }
        KnobEdit::Set { name, value } => {
            let spec = spec(registry, name)?;
            let value = parse_cli(spec, value).map_err(|rule| invalid(name, rule))?;
            check(spec, &value).map_err(|rule| invalid(name, rule))?;
            let item = match &value {
                Value::Bool(b) => toml_edit::value(*b),
                Value::Array(items) => {
                    let mut array = toml_edit::Array::new();
                    for item in items {
                        array.push(item.as_str().ok_or_else(|| invalid(name, "type"))?);
                    }
                    toml_edit::value(array)
                }
                Value::Number(n) => toml_edit::value(n.as_i64().unwrap_or_default()),
                Value::String(s) => toml_edit::value(s.as_str()),
                Value::Null | Value::Object(_) => return Err(invalid(name, "type")),
            };
            let knobs = doc
                .entry("knobs")
                .or_insert_with(toml_edit::table)
                .as_table_like_mut()
                .ok_or_else(|| invalid("knobs", "type"))?;
            match knobs.get_mut(name) {
                // Keep the key and the value's own comments and spacing.
                Some(slot) => {
                    let decor = slot.as_value().map(|old| old.decor().clone());
                    *slot = item;
                    if let (Some(decor), Some(new)) = (decor, slot.as_value_mut()) {
                        *new.decor_mut() = decor;
                    }
                }
                None => {
                    knobs.insert(name, item);
                }
            }
        }
        KnobEdit::Unset(name) => {
            spec(registry, name)?;
            if let Some(item) = doc.get_mut("knobs") {
                let knobs = item
                    .as_table_like_mut()
                    .ok_or_else(|| invalid("knobs", "type"))?;
                knobs.remove(name);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod chair_tests {
    use super::*;
    #[test]
    fn chair_string_list_preserves_order_and_rejects_non_strings() {
        let mut doc = DocumentMut::new();
        KnobEdit::Set {
            name: "chair.switch_accounts",
            value: "[\"second\",\"first\"]",
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            from_toml(&doc["knobs"]["chair.switch_accounts"]),
            Some(serde_json::json!(["second", "first"]))
        );
        assert!(
            KnobEdit::Set {
                name: "chair.switch_accounts",
                value: "[42]"
            }
            .apply(&mut DocumentMut::new())
            .is_err()
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod ceiling_tests {
    use super::*;
    #[test]
    fn registry_runtime_deadline_maximum_is_the_lane_ceiling() {
        let registry: Value = serde_json::from_str(REGISTRY).unwrap();
        assert_eq!(
            registry["provider_updates"]["runtime_deadline_seconds"]["max"],
            provenant_core::MAX_LANE_RUNTIME_SECONDS
        );
    }
}
