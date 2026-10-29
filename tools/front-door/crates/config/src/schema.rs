//! The editable field schema and the patch that edits it. Knob fields come
//! from the embedded registry; domain fields come from each registered
//! section's own default asset, and every edit is validated by the owning
//! domain when the edited configuration loads, so nothing here copies a rule.
use crate::{
    ConfigError, DomainSections, KnobEdit, Origin, ResolvedConfig,
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
fn leaves(value: &Value, path: String, out: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, value) in map {
                leaves(value, format!("{path}.{key}"), out);
            }
        }
        // Empty tables hold keyed entries (accounts); leave them to their commands.
        Value::Object(_) => {}
        _ => out.push((path, value.clone())),
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
    let domains = serde_json::to_value(&config.domains).unwrap_or(Value::Null);
    for entry in sections.entries {
        let Ok(defaults) = (entry.defaults)(config.profile)
            .parse::<DocumentMut>()
            .map_err(|_| ())
            .and_then(|doc| toml_edit::de::from_document::<Value>(doc).map_err(|_| ()))
        else {
            continue;
        };
        let mut found = Vec::new();
        leaves(&defaults, format!("domains.{}", entry.name), &mut found);
        for (path, default) in found {
            if fields.iter().any(|field| field.path == path) {
                continue;
            }
            let value = path
                .split('.')
                .skip(1)
                .try_fold(&domains, |node, key| node.get(key))
                .cloned()
                .unwrap_or_else(|| default.clone());
            fields.push(SchemaField {
                kind: kind_of(&default),
                min: None,
                max: None,
                values: Vec::new(),
                source: user(config.origins.get(&path)),
                restart_required: false,
                path,
                default,
                value,
            });
        }
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
    let registry = registry()?;
    for (path, value) in patch {
        if !known.contains_key(path.as_str()) {
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
