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
    documented_fields: Vec<String>,
    provider_updates: ProviderUpdates,
    pub(crate) presets: BTreeMap<String, Preset>,
}
#[derive(Deserialize)]
struct ProviderUpdates {
    provider_ids: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Bool,
    StringList,
    CensusIgnoreList,
    Enum,
    Int,
    Fraction,
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
    /// Grants or bounds capability or authority, so an invalid value takes
    /// `safe`, never a permissive reading of the entry.
    #[serde(default)]
    strict: bool,
    /// The restrictive value a strict knob takes when its entry is invalid.
    /// A strict knob with none has no safe reading, so an invalid entry
    /// refuses the file.
    #[serde(default)]
    safe: Option<Value>,
    #[serde(default)]
    host_only: bool,
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
    /// Whether to exclude the invoking process's ancestor chain during admission census.
    pub fn census_exclude_ancestors(&self) -> Result<bool, ConfigError> {
        let name = "confinement.census_exclude_ancestors";
        self.knobs
            .get(name)
            .and_then(|knob| knob.value.as_bool())
            .ok_or_else(|| invalid(name, "type"))
    }

    /// Host-owned process exceptions. Never read from a task/profile stamp.
    pub fn census_ignore_unreadable(
        &self,
    ) -> Result<Vec<provenant_os::pid::CensusIgnoreEntry>, ConfigError> {
        let name = "confinement.census_ignore_unreadable";
        let value = &self
            .knobs
            .get(name)
            .ok_or_else(|| invalid(name, "missing"))?
            .value;
        serde_json::from_value(value.clone()).map_err(|_| invalid(name, "type"))
    }

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
            if spec.host_only {
                return Err(invalid(name, "host_only"));
            }
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
        let launch_overrides: std::collections::BTreeSet<&str> = overrides
            .iter()
            .filter_map(|entry| entry.split_once('=').map(|(name, _)| name))
            .collect();
        // The layer that set each knob whose source a lane reports: its
        // limits, its CI scope and its gh budget.
        let knob_sources = SOURCED_KNOBS
            .into_iter()
            .filter_map(|name| {
                let knob = knobs.get(name)?;
                let source = match knob.source {
                    KnobSource::Override if launch_overrides.contains(name) => "launch",
                    KnobSource::Override => "host",
                    KnobSource::Default | KnobSource::Profile => "default",
                };
                Some((name.to_owned(), Value::String(source.into())))
            })
            .collect::<serde_json::Map<_, _>>();
        let mut extra = provenant_records::ExtraFields::default();
        extra.insert(KNOB_SOURCES.into(), Value::Object(knob_sources));
        Ok(provenant_records::run::HarnessProfile {
            name: profile_name(self.profile).into(),
            // Sealed records carry no floats, so a fraction travels as its
            // decimal text; readers parse it back.
            knobs: knobs
                .into_iter()
                .map(|(name, knob)| {
                    let value = match knob.value {
                        Value::Number(number)
                            if registry
                                .knobs
                                .get(&name)
                                .is_some_and(|spec| spec.kind == Kind::Fraction) =>
                        {
                            Value::String(number.to_string())
                        }
                        value => value,
                    };
                    (name, value)
                })
                .collect(),
            overrides: overrides.to_vec(),
            extra,
            ..Default::default()
        })
    }
}

/// Profile field naming the layer (`launch`, `host` or `default`) that set
/// each of [`SOURCED_KNOBS`].
pub const KNOB_SOURCES: &str = "knob_sources";

/// Knobs whose effective source a lane's receipt, status and preamble show.
const SOURCED_KNOBS: [&str; 5] = [
    "lanes.token_budget",
    "lanes.budget_action",
    "lanes.budget_grace",
    "lanes.ci_scope",
    "lanes.gh_budget",
];

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
    ConfigError::UnknownKnob {
        name: name.into(),
        close: nearest(name, registry.knobs.keys().map(String::as_str)),
    }
}
/// The names closest to `name` by edit distance (or containment), nearest first.
pub(crate) fn nearest<'a>(name: &str, known: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut near: Vec<(usize, &str)> = known
        .filter(|known| known.contains(name) || name.contains(known) || distance(known, name) <= 3)
        .map(|known| (distance(known, name), known))
        .collect();
    near.sort();
    near.into_iter()
        .take(3)
        .map(|(_, known)| known.to_owned())
        .collect()
}
/// Registry knob names closest to `name`.
pub(crate) fn nearest_knobs(name: &str) -> Vec<String> {
    registry().map_or_else(
        |_| Vec::new(),
        |registry| nearest(name, registry.knobs.keys().map(String::as_str)),
    )
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
        Value::Array(items) if spec.kind == Kind::CensusIgnoreList => {
            let entries: Vec<provenant_os::pid::CensusIgnoreEntry> =
                serde_json::from_value(value.clone()).map_err(|_| "type")?;
            let mut paths = std::collections::BTreeSet::new();
            if items.len() <= 128
                && entries
                    .iter()
                    .all(|entry| entry.is_valid() && paths.insert(&entry.path))
            {
                Ok(())
            } else {
                Err("invalid_census_exception")
            }
        }
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
        Value::Number(number) if spec.kind == Kind::Fraction => {
            let value = number.as_f64().ok_or("type")?;
            if !value.is_finite()
                || spec.min.is_some_and(|min| value < min as f64)
                || spec.max.is_some_and(|max| value > max as f64)
            {
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
            .map(|value| from_toml(&Item::Value(value.clone())))
            .collect::<Option<Vec<_>>>()
            .map(Value::Array),
        toml_edit::Value::InlineTable(table) => table
            .iter()
            .map(|(key, value)| Some((key.to_owned(), from_toml(&Item::Value(value.clone()))?)))
            .collect::<Option<serde_json::Map<_, _>>>()
            .map(Value::Object),
        toml_edit::Value::Integer(n) => Some(Value::from(*n.value())),
        toml_edit::Value::Float(n) => serde_json::Number::from_f64(*n.value())
            .map(Value::Number)
            .or(Some(Value::Null)),
        toml_edit::Value::String(s) => Some(Value::from(s.value().as_str())),
        _ => None,
    }
}
fn parse_cli(spec: &Spec, text: &str) -> Result<Value, &'static str> {
    if matches!(
        spec.kind,
        Kind::Bool | Kind::StringList | Kind::CensusIgnoreList
    ) {
        return serde_json::from_str(text).map_err(|_| "type");
    }
    if spec.kind == Kind::Int {
        return text.parse::<i64>().map(Value::from).map_err(|_| "type");
    }
    if spec.kind == Kind::Fraction {
        let value = text.parse::<f64>().map_err(|_| "type")?;
        return serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or("type");
    }
    Ok(Value::from(text))
}

/// The restrictive value an invalid entry for the strict knob `name` takes.
pub(crate) fn safe_value(name: &str) -> Option<Value> {
    let spec = registry().ok()?.knobs.get(name)?;
    spec.strict.then(|| spec.safe.clone()).flatten()
}
/// Whether an invalid entry for `name` refuses the file: a strict knob with
/// no restrictive value, where any fallback could drop what the user set.
pub(crate) fn refuses_when_invalid(name: &str) -> bool {
    registry().is_ok_and(|registry| {
        registry
            .knobs
            .get(name)
            .is_some_and(|spec| spec.strict && spec.safe.is_none())
    })
}
/// Whether some strict knob has no restrictive value, so a malformed `[knobs]`
/// table cannot be ignored safely.
pub(crate) fn any_refuses_when_invalid() -> bool {
    registry().is_ok_and(|registry| {
        registry
            .knobs
            .values()
            .any(|spec| spec.strict && spec.safe.is_none())
    })
}
/// Every leaf under `[knobs]` by dotted name, so an unquoted `lanes.publish =
/// false`, which TOML reads as a nested table, is the knob `lanes.publish`.
fn flatten<'a>(table: &'a dyn TableLike, prefix: &str, out: &mut Vec<(String, &'a Item)>) {
    for (key, item) in table.iter() {
        let name = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        match item.as_table_like() {
            Some(nested) => flatten(nested, &name, out),
            None => out.push((name, item)),
        }
    }
}

/// One `[knobs]` entry the resolver ignored; the knob keeps a default.
pub(crate) struct KnobProblem {
    pub(crate) name: String,
    /// `unknown_key` for a name outside the registry, else the value rule.
    pub(crate) rule: &'static str,
}

/// Registry default, then the profile preset, then the user's `[knobs]`.
/// An unknown name or an invalid value is ignored and reported; it never
/// refuses. A strict knob takes its restrictive value instead of the default,
/// and every strict knob does when the whole table is malformed.
pub(crate) fn resolve(
    profile: Profile,
    overrides: Option<&dyn TableLike>,
    malformed_table: bool,
) -> Result<(BTreeMap<String, KnobValue>, Vec<KnobProblem>), ConfigError> {
    let registry = registry()?;
    let preset = registry.presets.get(profile_name(profile));
    let mut knobs = BTreeMap::new();
    for (name, spec) in &registry.knobs {
        let from_preset = preset.and_then(|p| p.knobs.get(name));
        let value = match (from_preset, safe_value(name)) {
            (_, Some(safe)) if malformed_table => KnobValue {
                value: safe,
                source: KnobSource::Default,
            },
            (Some(value), _) => KnobValue {
                value: value.clone(),
                source: KnobSource::Profile,
            },
            (None, _) => KnobValue {
                value: spec.default.clone(),
                source: KnobSource::Default,
            },
        };
        knobs.insert(name.clone(), value);
    }
    let mut problems = Vec::new();
    let mut entries = Vec::new();
    if let Some(table) = overrides {
        flatten(table, "", &mut entries);
    }
    for (name, item) in entries {
        let name = name.as_str();
        let Some(spec) = registry.knobs.get(name) else {
            problems.push(KnobProblem {
                name: name.into(),
                rule: "unknown_key",
            });
            continue;
        };
        let checked = from_toml(item)
            .ok_or("type")
            .and_then(|value| check(spec, &value).map(|()| value));
        match checked {
            Ok(value) => {
                knobs.insert(
                    name.into(),
                    KnobValue {
                        value,
                        source: KnobSource::Override,
                    },
                );
            }
            Err(rule) => {
                if let Some(safe) = safe_value(name) {
                    knobs.insert(
                        name.into(),
                        KnobValue {
                            value: safe,
                            source: KnobSource::Default,
                        },
                    );
                }
                problems.push(KnobProblem {
                    name: name.into(),
                    rule,
                });
            }
        }
    }
    Ok((knobs, problems))
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
                        if let Some(text) = item.as_str() {
                            array.push(text);
                        } else if let Some(fields) = item.as_object() {
                            let mut table = toml_edit::InlineTable::new();
                            for (key, value) in fields {
                                table.insert(
                                    key,
                                    toml_edit::Value::from(
                                        value.as_str().ok_or_else(|| invalid(name, "type"))?,
                                    ),
                                );
                            }
                            array.push(toml_edit::Value::InlineTable(table));
                        } else {
                            return Err(invalid(name, "type"));
                        }
                    }
                    toml_edit::value(array)
                }
                Value::Number(n) if spec.kind == Kind::Fraction => {
                    toml_edit::value(n.as_f64().ok_or_else(|| invalid(name, "type"))?)
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
            let table = match doc.get_mut("knobs") {
                Some(item) => Some(
                    item.as_table_like_mut()
                        .ok_or_else(|| invalid("knobs", "type"))?,
                ),
                None => None,
            };
            // A misspelt entry already in the file can be removed by name,
            // which is how a warning about it is repaired.
            let present = table.as_ref().is_some_and(|knobs| knobs.contains_key(name));
            if !present {
                spec(registry, name)?;
            }
            if let Some(knobs) = table {
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

/// Whether a field belongs to the compact agent-facing configuration surface.
/// Provider placeholders cover the registry's documented adapters; internal
/// adapters remain available through the full schema and validation.
pub fn documented_field(path: &str) -> bool {
    registry().is_ok_and(|registry| {
        registry.documented_fields.iter().any(|field| {
            let mut expected = field.split('.');
            let mut actual = path.split('.');
            loop {
                match (expected.next(), actual.next()) {
                    (None, None) => return true,
                    (Some("{provider}"), Some(value))
                        if registry
                            .provider_updates
                            .provider_ids
                            .iter()
                            .any(|provider| provider == value) => {}
                    (Some(pattern), Some(value))
                        if pattern == value
                            || (pattern != "{provider}"
                                && pattern.starts_with('{')
                                && pattern.ends_with('}')) => {}
                    _ => return false,
                }
            }
        })
    })
}
