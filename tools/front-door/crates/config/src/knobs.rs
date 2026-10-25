//! Harness knobs: the registry embedded from `config/harness-knobs.json`, the
//! optional `[knobs]` override table and the edits `config set` and
//! `config unset` make to the user's `config.toml`.
use crate::{ConfigError, Profile};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::OnceLock};
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
#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Bool,
    StringList,
    Enum,
    Int,
}
#[derive(Deserialize)]
struct Spec {
    #[serde(rename = "type")]
    kind: Kind,
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
    #[serde(default)]
    task_classes: BTreeMap<String, ClassPreset>,
}
#[derive(Deserialize)]
struct ClassPreset {
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
    /// Resolve a task's final policy without changing the config file.
    pub fn dispatch_profile(
        &self,
        class: Option<&str>,
        overrides: &[String],
    ) -> Result<provenant_records::run::HarnessProfile, ConfigError> {
        let registry = registry()?;
        let mut knobs = self.knobs.clone();
        if let Some(class) = class {
            apply_class(registry, self.profile, &mut knobs, class);
        }
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
            task_class: class.map(str::to_owned),
            knobs: knobs
                .into_iter()
                .map(|(name, knob)| (name, knob.value))
                .collect(),
            overrides: overrides.to_vec(),
        })
    }

    /// Layer the profile preset's per-task-class knobs over the base preset.
    /// A user `[knobs]` override still wins. An unlisted class changes nothing.
    pub fn apply_task_class(&mut self, class: &str) -> Result<(), ConfigError> {
        let registry = registry()?;
        apply_class(registry, self.profile, &mut self.knobs, class);
        Ok(())
    }
}

fn apply_class(
    registry: &Registry,
    profile: Profile,
    knobs: &mut BTreeMap<String, KnobValue>,
    class: &str,
) {
    let preset = registry.presets.get(profile_name(profile));
    let layer = preset.and_then(|p| p.task_classes.get(class));
    for (name, value) in layer.into_iter().flat_map(|c| &c.knobs) {
        if let Some(knob) = knobs.get_mut(name)
            && knob.source != KnobSource::Override
        {
            *knob = KnobValue {
                value: value.clone(),
                source: KnobSource::Profile,
            };
        }
    }
}

/// Parsed once; the embedded text is immutable.
fn registry() -> Result<&'static Registry, ConfigError> {
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
        Value::Null if spec.nullable => Ok(()),
        Value::Null => Err("not_nullable"),
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
        toml_edit::Value::String(s) if s.value() == NULL => Some(Value::Null),
        toml_edit::Value::String(s) => Some(Value::from(s.value().as_str())),
        _ => None,
    }
}
fn parse_cli(spec: &Spec, text: &str) -> Result<Value, &'static str> {
    if text == NULL {
        return Ok(Value::Null);
    }
    if matches!(spec.kind, Kind::Bool | Kind::StringList) {
        return serde_json::from_str(text).map_err(|_| "type");
    }
    if spec.kind == Kind::Int {
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
            let spec = spec(registry, name)?;
            let value = parse_cli(spec, value).map_err(|rule| invalid(name, rule))?;
            check(spec, &value).map_err(|rule| invalid(name, rule))?;
            let item = match &value {
                Value::Null => toml_edit::value(NULL),
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
                Value::Object(_) => return Err(invalid(name, "type")),
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
    let state = match fs.open_anchor(roots.state.as_path()) {
        Ok(state) => state,
        Err(error) if error.is_not_found() => {
            // Create only the state directory itself, private. Its parent must
            // exist; a symlinked final component is refused by the anchor.
            let target = roots.state.as_path();
            let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
                return Err(error.into());
            };
            let parent = fs.open_anchor(parent)?;
            match parent.mkdir(name, 0o700) {
                Ok(()) => parent.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            fs.open_anchor(target)?
        }
        Err(error) => return Err(error.into()),
    };
    match state.atomic_write(
        std::ffi::OsStr::new("config.toml"),
        ".config-",
        updated.as_bytes(),
        0o600,
        provenant_supervisor::fs::PublishMode::Replace,
    )? {
        provenant_supervisor::fs::Publication::Durable => Ok(()),
        provenant_supervisor::fs::Publication::VisibleDurabilityUnconfirmed { sync_error } => {
            Err(sync_error.into())
        }
    }
}

/// Edit the owned TOML document, validate the complete configuration, publish
/// it atomically, then read it back through the same registered domains.
pub fn edit_config_document<T>(
    roots: &provenant_core::Roots,
    fs: &impl provenant_supervisor::fs::SafeFs,
    sections: &crate::DomainSections<T>,
    edit: impl FnOnce(&mut DocumentMut) -> Result<(), &'static str>,
) -> Result<crate::ResolvedConfig<T>, ConfigError>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    use provenant_supervisor::fs::{Publication, PublishMode};
    use std::{ffi::OsStr, io::Cursor};

    let path = roots.state.as_path().join("config.toml");
    let text = crate::loader::open(roots, fs)?
        .map(|reader| crate::loader::bounded_text(reader, &path))
        .transpose()?;
    crate::loader::load_with_domains(
        roots,
        text.as_deref().map(|source| Cursor::new(source.as_bytes())),
        sections,
    )?;
    let mut document: DocumentMut = text
        .as_deref()
        .unwrap_or("schema_version = 1\n")
        .parse()
        .map_err(|_| ConfigError::Edit {
            reason: "invalid TOML",
        })?;
    edit(&mut document).map_err(|reason| ConfigError::Edit { reason })?;
    let updated = document.to_string();
    crate::loader::load_with_domains(roots, Some(Cursor::new(updated.as_bytes())), sections)?;

    let state = match fs.open_anchor(roots.state.as_path()) {
        Ok(state) => state,
        Err(error) if error.is_not_found() => {
            let target = roots.state.as_path();
            let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
                return Err(error.into());
            };
            let parent = fs.open_anchor(parent)?;
            match parent.mkdir(name, 0o700) {
                Ok(()) => parent.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            fs.open_anchor(target)?
        }
        Err(error) => return Err(error.into()),
    };
    match state.atomic_write(
        OsStr::new("config.toml"),
        ".config-",
        updated.as_bytes(),
        0o600,
        PublishMode::Replace,
    )? {
        Publication::Durable => {}
        Publication::VisibleDurabilityUnconfirmed { sync_error } => return Err(sync_error.into()),
    }
    crate::loader::read_with_domains(roots, fs, sections)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod chair_tests {
    use super::*;
    #[test]
    fn chair_string_list_preserves_order_and_rejects_non_strings() {
        let text = apply(
            None,
            KnobEdit::Set {
                name: "chair.switch_accounts",
                value: "[\"second\",\"first\"]",
            },
        )
        .unwrap();
        let doc: DocumentMut = text.parse().unwrap();
        assert_eq!(
            from_toml(&doc["knobs"]["chair.switch_accounts"]),
            Some(serde_json::json!(["second", "first"]))
        );
        assert!(
            apply(
                None,
                KnobEdit::Set {
                    name: "chair.switch_accounts",
                    value: "[42]"
                }
            )
            .is_err()
        );
    }
}
