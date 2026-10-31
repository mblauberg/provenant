//! The editable field schema and the patch that edits it. Knob fields come
//! from the embedded registry; typed settings and domain fields come from
//! their owners' defaults, typed values and published metadata. Every edit is
//! validated by the owning domain when the edited configuration loads, so
//! nothing here copies a rule.
use crate::{
    ConfigError, DomainSections, FieldMetadata, KnobEdit, Origin, ResolvedConfig,
    knobs::{Kind, KnobSource, registry},
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use toml_edit::{DocumentMut, Item};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Default,
    Profile,
    User,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchemaField {
    pub path: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    pub default: Value,
    pub value: Value,
    pub source: Layer,
    pub restart_required: bool,
}

#[derive(Debug, Serialize)]
pub struct Schema {
    pub revision: String,
    pub fields: Vec<SchemaField>,
}

const GC_FIELDS: &[FieldMetadata] = &[
    FieldMetadata::new("schedule", "bool"),
    FieldMetadata::int("idle_days", 1, 3650),
    FieldMetadata::new("free_floor", "string"),
    FieldMetadata::new("alert_below", "string"),
    FieldMetadata::enumeration(
        "projects.{project}.foreign_worktrees",
        &["keep", "merged-clean"],
    ),
];
const RETENTION_FIELDS: &[FieldMetadata] = &[
    FieldMetadata::int("provider_state_days", 1, 3650),
    FieldMetadata::int("logs_days", 1, 3650),
    FieldMetadata::int("session_revisions", 1, 64),
    FieldMetadata::int("projects.{project}.checkpoint_soft_bytes", 1, 262_144),
    FieldMetadata::int("chair_log_days", 1, 3650),
    FieldMetadata::int("chair_log_replay_days", 7, 3650),
    FieldMetadata::int("chair_log_entries", 1, 100_000),
    FieldMetadata::int("chair_log_bytes", 65_536, 1_073_741_824),
];
const INSTALL_FIELDS: &[FieldMetadata] = &[
    FieldMetadata::new("bin_dir", "string"),
    FieldMetadata::new("targets.{harness}.home", "string"),
    FieldMetadata::new("targets.{harness}.config", "string"),
];
const PROVIDER_FIELDS: &[FieldMetadata] = &[
    FieldMetadata::new("{provider}.update", "bool"),
    FieldMetadata::int("{provider}.update_interval_seconds", 1, i64::MAX),
    FieldMetadata::int("{provider}.update_timeout_seconds", 1, i64::MAX),
    FieldMetadata::int(
        "{provider}.runtime_deadline_seconds",
        1,
        provenant_core::MAX_LANE_RUNTIME_SECONDS as i64,
    ),
];

fn user(origin: Option<&Origin>) -> Layer {
    match origin {
        Some(Origin::User { .. }) => Layer::User,
        _ => Layer::Default,
    }
}
fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        _ => "other",
    }
}
fn leaves(value: &Value, path: String, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, value) in map {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                leaves(value, child, out);
            }
        }
        // Empty tables hold keyed entries; metadata names their shape.
        Value::Object(_) => {}
        _ => {
            out.insert(path, value.clone());
        }
    }
}
fn is_placeholder(part: &str) -> bool {
    part.starts_with('{') && part.ends_with('}')
}
/// Match a concrete path against an owner pattern whose `{name}` segments
/// stand for one keyed map entry.
fn matches(pattern: &str, path: &str) -> bool {
    let mut pattern = pattern.split('.');
    let mut path = path.split('.');
    loop {
        match (pattern.next(), path.next()) {
            (None, None) => return true,
            (Some(expected), Some(actual)) if is_placeholder(expected) || expected == actual => {}
            _ => return false,
        }
    }
}

fn push_fields(
    fields: &mut Vec<SchemaField>,
    prefix: &str,
    defaults: &BTreeMap<String, Value>,
    effective: &BTreeMap<String, Value>,
    metadata: &'static [FieldMetadata],
    origins: &BTreeMap<String, Origin>,
) {
    let mut used = vec![false; metadata.len()];
    for path in defaults
        .keys()
        .chain(effective.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if fields.iter().any(|field| field.path == *path) {
            continue;
        }
        let relative = path
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('.'))
            .unwrap_or(path);
        let found = metadata
            .iter()
            .position(|field| matches(field.path, relative));
        if let Some(index) = found {
            used[index] = true;
        }
        let default = defaults
            .get(path)
            .or_else(|| effective.get(path))
            .cloned()
            .unwrap_or(Value::Null);
        let value = effective
            .get(path)
            .cloned()
            .unwrap_or_else(|| default.clone());
        let (kind, min, max, values, restart_required) = found.map_or_else(
            || (kind_of(&default), None, None, Vec::new(), false),
            |index| {
                let field = metadata[index];
                (
                    field.kind,
                    field.min,
                    field.max,
                    field.values.iter().map(|v| (*v).to_owned()).collect(),
                    field.restart_required,
                )
            },
        );
        fields.push(SchemaField {
            path: path.clone(),
            kind,
            min,
            max,
            values,
            default,
            value,
            source: user(origins.get(path)),
            restart_required,
        });
    }
    // A keyed shape with no configured row still needs its type and bounds.
    for (index, field) in metadata.iter().enumerate() {
        if used[index] {
            continue;
        }
        fields.push(SchemaField {
            path: format!("{prefix}.{}", field.path),
            kind: field.kind,
            min: field.min,
            max: field.max,
            values: field.values.iter().map(|v| (*v).to_owned()).collect(),
            default: Value::Null,
            value: Value::Null,
            source: Layer::Default,
            restart_required: field.restart_required,
        });
    }
}

/// Every editable field with its bounds, default, effective value and layer.
pub fn schema<T: Serialize>(
    config: &ResolvedConfig<T>,
    sections: &DomainSections<T>,
    revision: String,
) -> Result<Schema, ConfigError> {
    let registry = registry()?;
    let mut fields = vec![SchemaField {
        path: "profile".into(),
        kind: "enum",
        min: None,
        max: None,
        values: registry.presets.keys().cloned().collect(),
        default: Value::from(crate::Profile::default().as_str()),
        value: Value::from(config.profile.as_str()),
        source: user(config.origins.get("profile")),
        restart_required: false,
    }];
    for (name, spec) in &registry.knobs {
        let knob = config.knobs.get(name);
        fields.push(SchemaField {
            path: name.clone(),
            kind: match spec.kind {
                Kind::Bool => "bool",
                Kind::StringList => "string_list",
                Kind::CensusIgnoreList => "census_ignore_list",
                Kind::Enum => "enum",
                Kind::Int => "int",
            },
            min: spec.min,
            max: spec.max,
            values: spec.values.clone(),
            default: spec.default.clone(),
            value: knob.map_or(spec.default.clone(), |k| k.value.clone()),
            source: match knob.map(|k| k.source) {
                Some(KnobSource::Override) => Layer::User,
                Some(KnobSource::Profile) => Layer::Profile,
                _ => Layer::Default,
            },
            restart_required: false,
        });
    }
    for (prefix, defaults, effective, metadata) in [
        (
            "gc",
            serde_json::to_value(crate::GcSettings::default()).unwrap_or(Value::Null),
            serde_json::to_value(&config.gc).unwrap_or(Value::Null),
            GC_FIELDS,
        ),
        (
            "retention",
            serde_json::to_value(crate::RetentionSettings::default()).unwrap_or(Value::Null),
            serde_json::to_value(&config.retention).unwrap_or(Value::Null),
            RETENTION_FIELDS,
        ),
        (
            "install",
            serde_json::to_value(crate::InstallSettings::default()).unwrap_or(Value::Null),
            serde_json::to_value(&config.install).unwrap_or(Value::Null),
            INSTALL_FIELDS,
        ),
        (
            "providers",
            serde_json::to_value(crate::providers::defaults()).unwrap_or(Value::Null),
            serde_json::to_value(&config.providers).unwrap_or(Value::Null),
            PROVIDER_FIELDS,
        ),
    ] {
        let mut default_leaves = BTreeMap::new();
        let mut effective_leaves = BTreeMap::new();
        leaves(&defaults, prefix.into(), &mut default_leaves);
        leaves(&effective, prefix.into(), &mut effective_leaves);
        push_fields(
            &mut fields,
            prefix,
            &default_leaves,
            &effective_leaves,
            metadata,
            &config.origins,
        );
    }
    let domains = serde_json::to_value(&config.domains).unwrap_or(Value::Null);
    for entry in sections.entries {
        let prefix = format!("domains.{}", entry.name);
        let mut defaults = BTreeMap::new();
        if let Ok(document) = (entry.defaults)(config.profile).parse::<DocumentMut>()
            && let Ok(value) = toml_edit::de::from_document::<Value>(document)
        {
            leaves(&value, prefix.clone(), &mut defaults);
        }
        let mut effective = BTreeMap::new();
        if let Some(value) = domains.get(entry.name) {
            leaves(value, prefix.clone(), &mut effective);
        }
        push_fields(
            &mut fields,
            &prefix,
            &defaults,
            &effective,
            entry.fields,
            &config.origins,
        );
    }
    Ok(Schema { revision, fields })
}

/// Apply a flat patch of `{"field": value}` to the document; a null value
/// clears the field. Profile and knobs go through the knob editor; any other
/// field must be one the schema lists, and the owning domain validates it when
/// the edited configuration loads.
pub fn apply_patch<T: Serialize>(
    patch: &Value,
    document: &mut DocumentMut,
    config: &ResolvedConfig<T>,
    sections: &DomainSections<T>,
) -> Result<(), ConfigError> {
    let Value::Object(patch) = patch else {
        return Err(ConfigError::Refused("patch must be a JSON object".into()));
    };
    if patch.is_empty() {
        return Err(ConfigError::Refused("patch is empty".into()));
    }
    let editable = schema(config, sections, String::new())?;
    let known: BTreeMap<&str, &SchemaField> = editable
        .fields
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();
    let editable_path = |path: &str| {
        known.contains_key(path)
            || editable
                .fields
                .iter()
                .any(|field| matches(&field.path, path))
    };
    let registry = registry()?;
    for (path, value) in patch {
        if !editable_path(path) {
            return Err(ConfigError::Refused(format!(
                "{path} is not an editable field"
            )));
        }
        if path == "profile" || registry.knobs.contains_key(path) {
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            let edit = match (path.as_str(), value) {
                ("profile", Value::Null) => {
                    return Err(ConfigError::Refused("profile cannot be cleared".into()));
                }
                ("profile", _) => KnobEdit::Profile(&text),
                (_, Value::Null) => KnobEdit::Unset(path),
                _ => KnobEdit::Set {
                    name: path,
                    value: &text,
                },
            };
            edit.apply(document)?;
        } else {
            set_path(document, path, value)?;
        }
    }
    Ok(())
}

fn set_path(document: &mut DocumentMut, path: &str, value: &Value) -> Result<(), ConfigError> {
    let refuse = |why: &str| ConfigError::Refused(format!("{path}: {why}"));
    let mut keys: Vec<&str> = path.split('.').collect();
    let leaf = keys.pop().ok_or_else(|| refuse("empty path"))?;
    let mut table = document.as_table_mut() as &mut dyn toml_edit::TableLike;
    for key in keys {
        let item = table.entry(key).or_insert(toml_edit::table());
        table = item
            .as_table_like_mut()
            .ok_or_else(|| refuse("parent is not a table"))?;
    }
    if value.is_null() {
        table.remove(leaf);
    } else {
        table.insert(
            leaf,
            Item::Value(to_toml(value).ok_or_else(|| refuse("unsupported value"))?),
        );
    }
    Ok(())
}
fn to_toml(value: &Value) -> Option<toml_edit::Value> {
    Some(match value {
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64()?.into(),
        },
        Value::String(s) => s.as_str().into(),
        Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(to_toml(item)?);
            }
            array.into()
        }
        Value::Object(map) => {
            let mut table = toml_edit::InlineTable::new();
            for (key, item) in map {
                table.insert(key, to_toml(item)?);
            }
            table.into()
        }
        Value::Null => return None,
    })
}
