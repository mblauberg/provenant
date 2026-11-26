//! The editable field schema and the patch that edits it. Knob fields come
//! from the embedded registry; typed settings and domain fields come from
//! their owners' defaults, typed values and published metadata. Every edit is
//! validated by the owning domain when the edited configuration loads, so
//! nothing here copies a rule.
use crate::{
    ConfigError, DomainSections, FieldMetadata, KnobEdit, Origin, ResolvedConfig,
    knobs::{Kind, registry},
};
use provenant_records::{Layer, Precedence};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use toml_edit::{DocumentMut, Item};

/// The editable field family's layers, lowest first. A user edit in the host
/// config file outranks a profile preset.
/// Spec: docs/specs/state.md#every-setting-reports-its-layer
pub const SCHEMA_PRECEDENCE: Precedence =
    Precedence(&[Layer::Default, Layer::Profile, Layer::Host]);

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
    FieldMetadata::new("idle", "string"),
    FieldMetadata::new("sweep_budget", "string"),
    FieldMetadata::new("stale_graph_age", "string"),
    FieldMetadata::int("graph_checkpoint_days", 1, 3650),
    FieldMetadata::new("discard_archive_retention", "string"),
    FieldMetadata::new("cargo_target_idle", "string"),
    FieldMetadata::new("free_floor", "string"),
    FieldMetadata::new("critical_free", "string"),
    FieldMetadata::new("alert_below", "string"),
    FieldMetadata::new("projects.{project}.regenerable", "array"),
    FieldMetadata::new("projects.{project}.keep", "array"),
    FieldMetadata::new("projects.{project}.integration_branches", "array"),
    FieldMetadata::new("projects.{project}.keep_paths", "array"),
    FieldMetadata::enumeration(
        "projects.{project}.remote_branches",
        &crate::RemoteBranches::NAMES,
    ),
    FieldMetadata::int("projects.{project}.remote_min_tip_age_days", 0, 3650),
    FieldMetadata::enumeration(
        "projects.{project}.foreign_worktrees",
        &crate::ForeignWorktrees::NAMES,
    ),
];
const WORKTREE_FIELDS: &[FieldMetadata] = &[
    FieldMetadata::int("timeout_seconds", 1, 3600),
    FieldMetadata::int("projects.{project}.timeout_seconds", 1, 3600),
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
    FieldMetadata::int("payload_cap_bytes", 1_048_576, 1_125_899_906_842_624),
];
const WARNINGS_FIELDS: &[FieldMetadata] = &[FieldMetadata::int("dedup_retention_days", 1, 3650)];
const INSTALL_FIELDS: &[FieldMetadata] = &[
    FieldMetadata::new("bin_dir", "string"),
    FieldMetadata::new("targets.{harness}.home", "string"),
    FieldMetadata::new("targets.{harness}.config", "string"),
];
const PROVIDER_FIELDS: &[FieldMetadata] = &[
    FieldMetadata::new("{provider}.models", "array"),
    FieldMetadata::new("{provider}.clean_config", "bool"),
    FieldMetadata::enumeration("{provider}.out_of_scope_pin", &["warn", "refuse", "allow"]),
    FieldMetadata::new("{provider}.update", "bool"),
    FieldMetadata::int("{provider}.update_interval_seconds", 1, i64::MAX),
    FieldMetadata::int("{provider}.update_timeout_seconds", 1, i64::MAX),
    FieldMetadata::int(
        "{provider}.runtime_deadline_seconds",
        1,
        provenant_core::MAX_LANE_RUNTIME_SECONDS as i64,
    ),
    FieldMetadata::int("{provider}.stall_after_seconds", 0, i64::MAX),
    FieldMetadata::enumeration(
        "{provider}.stall_policy",
        &["report", "steer", "retry", "fail"],
    ),
];

/// The config-owned sections and their field metadata, by table prefix. The
/// schema and [`declared_enums`] both read this list.
const CONFIG_FIELDS: &[(&str, &[FieldMetadata])] = &[
    ("gc", GC_FIELDS),
    ("worktrees", WORKTREE_FIELDS),
    ("retention", RETENTION_FIELDS),
    ("warnings", WARNINGS_FIELDS),
    ("install", INSTALL_FIELDS),
    ("providers", PROVIDER_FIELDS),
];

/// Deserialise one config-owned table through its runtime type.
fn owned(prefix: &str, document: DocumentMut) -> Result<Value, String> {
    fn typed<S: serde::de::DeserializeOwned + Serialize>(
        document: DocumentMut,
    ) -> Result<Value, String> {
        let value: S = toml_edit::de::from_document(document).map_err(|error| error.to_string())?;
        serde_json::to_value(value).map_err(|error| error.to_string())
    }
    match prefix {
        "gc" => typed::<crate::GcSettings>(document),
        "worktrees" => typed::<crate::WorktreeSettings>(document),
        "retention" => typed::<crate::RetentionSettings>(document),
        "warnings" => typed::<crate::WarningsSettings>(document),
        "install" => typed::<crate::InstallSettings>(document),
        "providers" => typed::<BTreeMap<String, crate::ProviderSettings>>(document),
        _ => Err(format!("{prefix} has no runtime type")),
    }
}

/// An owner-declared enumeration: the one list the schema and the generated
/// reference publish, with the default its runtime type applies.
#[derive(Debug, Clone, Serialize)]
pub struct DeclaredEnum {
    pub path: String,
    pub values: &'static [&'static str],
    /// Absent where the value is inherited from another layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

/// Name each `{key}` segment with a probe entry.
fn concrete(pattern: &str) -> String {
    pattern
        .split('.')
        .map(|part| if is_placeholder(part) { "probe" } else { part })
        .collect::<Vec<_>>()
        .join(".")
}
fn leaf(value: &Value, path: &str) -> Option<Value> {
    path.split('.')
        .try_fold(value, |value, key| value.get(key))
        .filter(|value| !value.is_null())
        .cloned()
}

/// Every enumeration the config-owned sections and the registered domain
/// sections declare, under its full configuration path.
pub fn declared_enums<T>(sections: &DomainSections<T>) -> Vec<DeclaredEnum> {
    let mut out = Vec::new();
    for &(prefix, metadata) in CONFIG_FIELDS {
        for field in metadata.iter().filter(|field| !field.values.is_empty()) {
            let path = concrete(field.path);
            // A keyed entry with no keys set yields its runtime default.
            let mut document = DocumentMut::new();
            let shaped = path.rsplit_once('.').is_none_or(|(parent, _)| {
                set_path(&mut document, parent, &Value::Object(Default::default())).is_ok()
            });
            let default = shaped
                .then(|| owned(prefix, document).ok())
                .flatten()
                .and_then(|value| leaf(&value, &path));
            out.push(DeclaredEnum {
                path: format!("{prefix}.{}", field.path),
                values: field.values,
                default,
            });
        }
    }
    for entry in sections.entries {
        let defaults = (entry.defaults)(crate::Profile::default())
            .parse::<DocumentMut>()
            .ok()
            .and_then(|document| toml_edit::de::from_document::<Value>(document).ok());
        for field in entry.fields.iter().filter(|field| !field.values.is_empty()) {
            out.push(DeclaredEnum {
                path: format!("domains.{}.{}", entry.name, field.path),
                values: field.values,
                default: defaults.as_ref().and_then(|value| leaf(value, field.path)),
            });
        }
    }
    out
}

/// What the runtime refuses when one field is set to `value`, at a concrete
/// path such as `gc.projects.probe.foreign_worktrees`. Config-owned tables
/// deserialise through their type; a domain section validates its compiled
/// defaults with the field set.
pub fn probe_field<T>(sections: &DomainSections<T>, path: &str, value: &Value) -> Vec<String> {
    // A value the owning shape cannot hold is a schema refusal, as in a load.
    let refused = || vec![format!("{path}: schema")];
    if let Some((prefix, relative)) = path.split_once('.')
        && CONFIG_FIELDS.iter().any(|(owner, _)| *owner == prefix)
    {
        let mut document = DocumentMut::new();
        if set_path(&mut document, relative, value).is_err() {
            return refused();
        }
        return owned(prefix, document)
            .err()
            .map_or_else(Vec::new, |_| refused());
    }
    let Some((entry, relative)) = path.strip_prefix("domains.").and_then(|rest| {
        let (name, relative) = rest.split_once('.')?;
        Some((
            sections.entries.iter().find(|entry| entry.name == name)?,
            relative,
        ))
    }) else {
        return refused();
    };
    let Ok(mut document) = (entry.defaults)(crate::Profile::default()).parse::<DocumentMut>()
    else {
        return refused();
    };
    if set_path(&mut document, relative, value).is_err() {
        return refused();
    }
    (entry.validate)(document.as_table())
        .into_iter()
        .map(|error| format!("{}: {}", error.field, error.code))
        .collect()
}

fn origin_layer(origin: Option<&Origin>) -> Layer {
    match origin {
        Some(Origin::User { .. }) => Layer::Host,
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
            source: origin_layer(origins.get(path)),
            restart_required,
        });
    }
    // Keyed patterns remain editable independently of materialised rows.
    for (index, field) in metadata.iter().enumerate() {
        if used[index] && !field.path.split('.').any(is_placeholder) {
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
        source: origin_layer(config.origins.get("profile")),
        restart_required: false,
    }];
    for (name, spec) in &registry.knobs {
        let knob = config.knobs.get(name);
        fields.push(SchemaField {
            path: name.clone(),
            kind: match spec.kind {
                Kind::Bool => "bool",
                Kind::String => "string",
                Kind::StringList => "string_list",
                Kind::CensusIgnoreList => "census_ignore_list",
                Kind::Enum => "enum",
                Kind::Int => "int",
                Kind::Fraction => "fraction",
                Kind::CargoTarget => "cargo_target",
                Kind::Size => "size",
                Kind::Duration => "duration",
            },
            min: spec.min,
            max: spec.max,
            values: spec.values.clone(),
            default: spec.default.clone(),
            value: knob.map_or(spec.default.clone(), |k| k.value.clone()),
            source: knob.map_or(Layer::Default, |k| k.source.layer),
            restart_required: false,
        });
    }
    for &(prefix, metadata) in CONFIG_FIELDS {
        let (defaults, effective) = match prefix {
            "gc" => (
                serde_json::to_value(crate::GcSettings::default()),
                serde_json::to_value(&config.gc),
            ),
            "worktrees" => (
                serde_json::to_value(crate::WorktreeSettings::default()),
                serde_json::to_value(&config.worktrees),
            ),
            "retention" => (
                serde_json::to_value(crate::RetentionSettings::default()),
                serde_json::to_value(&config.retention),
            ),
            "warnings" => (
                serde_json::to_value(crate::WarningsSettings::default()),
                serde_json::to_value(&config.warning_dedup),
            ),
            "install" => (
                serde_json::to_value(crate::InstallSettings::default()),
                serde_json::to_value(&config.install),
            ),
            "providers" => (
                serde_json::to_value(crate::providers::defaults()),
                serde_json::to_value(&config.providers),
            ),
            _ => (Ok(Value::Null), Ok(Value::Null)),
        };
        let (defaults, effective) = (
            defaults.unwrap_or(Value::Null),
            effective.unwrap_or(Value::Null),
        );
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

/// Key prefix to dedicated command hints.
pub const DEDICATED_COMMANDS: &[(&str, &str)] = &[
    ("routing.model_exclusions", "provenant route exclude add"),
    (
        "domains.routing.model_exclusions",
        "provenant route exclude add",
    ),
];

/// Dedicated command to use instead of editing a key directly through `config apply`.
pub fn dedicated_command(path: &str) -> Option<&'static str> {
    for &(prefix, command) in DEDICATED_COMMANDS {
        if path == prefix
            || path.starts_with(&format!("{prefix}."))
            || path.starts_with(&format!("{prefix}["))
        {
            return Some(command);
        }
    }
    None
}

/// Apply the same editable schema to a single CLI setting. Registry knobs keep
/// their existing parser; typed fields are decoded according to their owner.
pub fn apply_edit<T: Serialize>(
    edit: KnobEdit<'_>,
    document: &mut DocumentMut,
    config: &ResolvedConfig<T>,
    sections: &DomainSections<T>,
) -> Result<(), ConfigError> {
    let (name, text) = match edit {
        KnobEdit::Profile(_) => return edit.apply(document),
        KnobEdit::Set { name, value } => (name, Some(value)),
        KnobEdit::Unset(name) => (name, None),
    };
    if registry()?.knobs.contains_key(name) {
        return edit.apply(document);
    }
    let editable = schema(config, sections, String::new())?;
    let field = editable.fields.iter().find(|f| matches(&f.path, name));
    let Some(field) = field else {
        return edit.apply(document);
    };
    let value = match text {
        None => Value::Null,
        Some(text)
            if matches!(
                field.kind,
                "string" | "enum" | "duration" | "size" | "cargo_target"
            ) =>
        {
            Value::String(text.to_owned())
        }
        Some(text) => serde_json::from_str(text)
            .map_err(|_| ConfigError::Refused(format!("{name}: invalid {} value", field.kind)))?,
    };
    let patch = Value::Object([(name.to_owned(), value)].into_iter().collect());
    apply_patch(&patch, document, config, sections)
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
            if let Some(cmd) = dedicated_command(path) {
                return Err(ConfigError::Refused(format!(
                    "{path} is not an editable field; use {cmd}"
                )));
            }
            let near = crate::knobs::nearest(path, known.keys().copied());
            return Err(ConfigError::Refused(if near.is_empty() {
                format!("{path} is not an editable field")
            } else {
                format!(
                    "{path} is not an editable field; did you mean {}?",
                    near.join(", ")
                )
            }));
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

#[cfg(test)]
mod precedence_tests {
    use super::*;
    use provenant_records::{Source, Sourced};

    fn field(layer: Layer) -> Sourced<&'static str> {
        Sourced {
            value: layer.as_str(),
            source: Source {
                layer,
                origin: None,
            },
        }
    }

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn every_field_layer_set_picks_the_host_and_shadows_the_rest() {
        let candidates = SCHEMA_PRECEDENCE.0.iter().copied().map(field);
        let merge = SCHEMA_PRECEDENCE.merge(candidates);
        assert_eq!(
            merge
                .winner
                .as_ref()
                .map(|candidate| candidate.source.layer),
            Some(Layer::Host)
        );
        assert_eq!(
            merge
                .shadowed
                .iter()
                .map(|candidate| candidate.source.layer)
                .collect::<Vec<_>>(),
            [Layer::Default, Layer::Profile]
        );
        assert!(merge.ignored.is_empty());
    }
}
