//! Harness knobs: the registry embedded from `config/harness-knobs.json`, the
//! optional `[knobs]` override table and the edits `config set` and
//! `config unset` make to the user's `config.toml`, plus the one config editor
//! that every command uses to rewrite that file.
use crate::{ConfigError, Profile};
use provenant_records::{Layer, Precedence, Source, Sourced};
use serde::Deserialize;
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
    String,
    StringList,
    CensusIgnoreList,
    Enum,
    Int,
    Fraction,
    CargoTarget,
    Size,
    Duration,
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
    pub(crate) host_only: bool,
}
#[derive(Deserialize)]
pub(crate) struct Preset {
    #[serde(default)]
    pub(crate) knobs: BTreeMap<String, Value>,
}

/// The knob family's layers, lowest first. The registry default is the floor,
/// a named preset overrides it, and an entry in the host's `[knobs]` table
/// overrides the preset. A launch flag is recorded separately in the sealed
/// profile and is not a configuration layer.
/// Spec: docs/specs/state.md#every-setting-reports-its-layer
pub const KNOB_PRECEDENCE: Precedence = Precedence(&[Layer::Default, Layer::Profile, Layer::Host]);

/// An effective knob value tagged with its layer.
fn sourced(value: Value, layer: Layer) -> Sourced<Value> {
    Sourced {
        value,
        source: Source {
            layer,
            origin: None,
        },
    }
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
                .is_some_and(|knob| knob.source.layer == Layer::Host)
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
            // Access is advisory: preserve invalid launch text so resolution warns and uses host.
            if name != "lanes.access" {
                check(spec, &value).map_err(|rule| invalid(name, rule))?;
            }
            // A dispatch override is a launch-time source, not a config layer.
            knobs.insert(name.into(), sourced(value, Layer::Launch));
        }
        // The layer that set every effective knob. A knob absent from the
        // map took its registry default, so the map stays small.
        let knob_sources = knobs
            .iter()
            .filter_map(|(name, knob)| {
                let layer = knob.source.layer;
                (layer != Layer::Default).then(|| (name.clone(), layer))
            })
            .collect();
        Ok(provenant_records::run::HarnessProfile {
            name: profile_name(self.profile).into(),
            // Sealed records carry no floats, so a fraction travels as its
            // decimal text; readers parse it back.
            knobs: knobs
                .into_iter()
                .map(|(name, knob)| {
                    let value = match knob.value {
                        Value::Number(number)
                            if matching_spec(registry, &name)
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
            knob_sources,
            ..Default::default()
        })
    }
}

/// The layer that set knob `name` for a sealed profile, as
/// [`crate::ResolvedConfig::dispatch_profile`] recorded it. Every limit,
/// receipt and status line reads its source here.
pub fn knob_source(profile: &provenant_records::run::HarnessProfile, name: &str) -> Layer {
    profile
        .knob_sources
        .get(name)
        .copied()
        .unwrap_or(Layer::Default)
}

/// Parsed once; the embedded text is immutable.
pub(crate) fn registry() -> Result<&'static Registry, ConfigError> {
    static PARSED: OnceLock<Option<Registry>> = OnceLock::new();
    PARSED
        .get_or_init(|| {
            let mut registry: Registry = serde_json::from_str(REGISTRY).ok()?;
            for rule in provenant_records::policy::table()
                .iter()
                .filter(|rule| rule.knob.is_some())
            {
                let values = rule
                    .strengths()
                    .into_iter()
                    .map(|strength| match strength {
                        provenant_records::policy::Strength::Refuse => "refuse".to_owned(),
                        provenant_records::policy::Strength::Warn => "warn".to_owned(),
                        provenant_records::policy::Strength::Off => "off".to_owned(),
                    })
                    .collect();
                registry.knobs.insert(
                    rule.knob.clone()?,
                    Spec {
                        kind: Kind::Enum,
                        min: None,
                        max: None,
                        values,
                        default: serde_json::to_value(rule.strength).ok()?,
                        strict: false,
                        safe: None,
                        host_only: false,
                    },
                );
            }
            Some(registry)
        })
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
    matching_spec(registry, name).ok_or_else(|| unknown(registry, name))
}

fn is_placeholder_segment(segment: &str) -> bool {
    segment.len() > 2
        && ((segment.starts_with('<') && segment.ends_with('>'))
            || (segment.starts_with('{') && segment.ends_with('}')))
}

/// Resolve concrete keys covered by a registry family such as
/// `domains.fabric.graph.on_stop.<stop_cause>`.
fn matching_spec<'a>(registry: &'a Registry, name: &str) -> Option<&'a Spec> {
    fn matches(pattern: &str, name: &str) -> bool {
        let mut pattern = pattern.split('.');
        let mut name = name.split('.');
        loop {
            match (pattern.next(), name.next()) {
                (None, None) => return true,
                (Some(pattern), Some(value))
                    if pattern == value
                        || (is_placeholder_segment(pattern) && !value.is_empty()) => {}
                _ => return false,
            }
        }
    }

    registry.knobs.get(name).or_else(|| {
        registry
            .knobs
            .iter()
            .find_map(|(pattern, spec)| matches(pattern, name).then_some(spec))
    })
}
fn unknown(registry: &Registry, name: &str) -> ConfigError {
    // The refusal carries the close matches, so callers never compute them.
    let candidates = match provenant_core::resolve(&KnobCatalogue(registry), name) {
        Ok(_) => Vec::new(),
        Err(refusal) => refusal.candidates,
    };
    ConfigError::UnknownKnob {
        name: name.into(),
        close: candidates,
    }
}
/// The names closest to `name`, ranked by the shared ranker.
pub(crate) fn nearest<'a>(name: &str, known: impl Iterator<Item = &'a str>) -> Vec<String> {
    provenant_core::close_matches(name, known)
        .into_iter()
        .map(str::to_owned)
        .collect()
}
/// Knob names resolved through the shared name machinery.
struct KnobCatalogue<'a>(&'a Registry);

impl provenant_core::Catalogue for KnobCatalogue<'_> {
    type Item = ();
    const KIND: provenant_core::NameKind = provenant_core::NameKind("knob");

    fn names(&self) -> Vec<String> {
        self.0.knobs.keys().cloned().collect()
    }

    fn lookup(&self, canonical: &str) -> Option<Self::Item> {
        self.0.knobs.contains_key(canonical).then_some(())
    }
}

/// Check a value against the knob's type, range, enum values and nullability.
fn check(spec: &Spec, value: &Value) -> Result<(), &'static str> {
    match value {
        Value::Array(items) if spec.kind == Kind::CensusIgnoreList => {
            let entries: Vec<provenant_os::pid::CensusIgnoreEntry> =
                serde_json::from_value(value.clone()).map_err(|_| "type")?;
            let mut paths = std::collections::BTreeSet::new();
            if items.len() <= 128
                && entries.iter().all(|entry| {
                    entry.is_valid()
                        && matches!(
                            entry.started_before,
                            None | Some(provenant_os::pid::StartedBefore::Root)
                        )
                        && paths.insert(&entry.path)
                })
            {
                Ok(())
            } else {
                Err("invalid_census_exception")
            }
        }
        Value::Bool(_) if spec.kind == Kind::Bool => Ok(()),
        Value::String(text) if spec.kind == Kind::String => (!text.contains('\0')
            && text.len() <= 4096)
            .then_some(())
            .ok_or("type"),
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
        Value::String(text) if spec.kind == Kind::CargoTarget => {
            if text == "project"
                || text == "private"
                || (std::path::Path::new(text).is_absolute() && !text.contains('\0'))
            {
                Ok(())
            } else {
                Err("unknown_value")
            }
        }
        Value::String(text) if spec.kind == Kind::Size => {
            crate::cache_budget::is_valid_setting(text)
                .then_some(())
                .ok_or("invalid_size")
        }
        Value::Number(number) if spec.kind == Kind::Size => {
            number.as_u64().map(|_| ()).ok_or("invalid_size")
        }
        Value::String(text) if spec.kind == Kind::Duration => crate::parse_gc_idle_duration(text)
            .is_ok()
            .then_some(())
            .ok_or("invalid_duration"),
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
    let spec = matching_spec(registry().ok()?, name)?;
    spec.strict.then(|| spec.safe.clone()).flatten()
}
/// Whether an invalid entry for `name` refuses the file: a strict knob with
/// no restrictive value, where any fallback could drop what the user set.
pub(crate) fn refuses_when_invalid(name: &str) -> bool {
    registry().is_ok_and(|registry| {
        matching_spec(registry, name).is_some_and(|spec| spec.strict && spec.safe.is_none())
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

/// Each knob with the layer that set it, plus the problems found on the way.
type ResolvedKnobs = (BTreeMap<String, Sourced<Value>>, Vec<KnobProblem>);

/// Registry default, then the profile preset, then the user's `[knobs]`.
/// An unknown name or an invalid value is ignored and reported; it never
/// refuses. A strict knob takes its restrictive value instead of the default,
/// and every strict knob does when the whole table is malformed.
pub(crate) fn resolve(
    profile: Profile,
    overrides: Option<&dyn TableLike>,
    malformed_table: bool,
) -> Result<ResolvedKnobs, ConfigError> {
    let registry = registry()?;
    let preset = registry.presets.get(profile_name(profile));
    let mut by_name: BTreeMap<String, Vec<Sourced<Value>>> = BTreeMap::new();
    for (name, spec) in &registry.knobs {
        if name.split('.').any(is_placeholder_segment) {
            continue;
        }
        let mut candidates = Vec::new();
        let safe = safe_value(name);
        if malformed_table {
            // A malformed table narrows every strict knob to its safe value;
            // other knobs keep the preset or default they would otherwise use.
            if let Some(safe) = safe {
                candidates.push(sourced(safe, Layer::Default));
            } else if let Some(value) = preset.and_then(|preset| preset.knobs.get(name)) {
                candidates.push(sourced(value.clone(), Layer::Profile));
            } else {
                candidates.push(sourced(spec.default.clone(), Layer::Default));
            }
        } else {
            candidates.push(sourced(spec.default.clone(), Layer::Default));
            if let Some(value) = preset.and_then(|preset| preset.knobs.get(name)) {
                candidates.push(sourced(value.clone(), Layer::Profile));
            }
        }
        by_name.insert(name.clone(), candidates);
    }
    let mut problems = Vec::new();
    let mut entries = Vec::new();
    if let Some(table) = overrides {
        flatten(table, "", &mut entries);
    }
    for (name, item) in entries {
        let name = name.as_str();
        let Some(spec) = matching_spec(&registry, name) else {
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
                by_name
                    .entry(name.into())
                    .or_default()
                    .push(sourced(value, Layer::Host));
            }
            Err(rule) => {
                if let Some(safe) = safe_value(name) {
                    let candidates = by_name.entry(name.into()).or_default();
                    candidates.clear();
                    candidates.push(sourced(safe, Layer::Default));
                }
                problems.push(KnobProblem {
                    name: name.into(),
                    rule,
                });
            }
        }
    }
    let mut knobs = BTreeMap::new();
    for (name, candidates) in by_name {
        let merge = KNOB_PRECEDENCE.merge(candidates);
        provenant_records::warn_ignored(&merge.ignored, &name);
        if let Some(winner) = merge.winner {
            knobs.insert(name, winner);
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
    fn registry_families_accept_concrete_stop_default_keys() {
        let config = "schema_version = 1\n[knobs]\n\
            \"domains.fabric.graph.on_stop.stopped_no_work\" = \"chair\"\n\
            \"domains.fabric.graph.projects.project-a.on_stop.blocked\" = \"new_route\"\n"
            .parse::<DocumentMut>()
            .unwrap();
        let (resolved, problems) = resolve(
            Profile::Balanced,
            config.get("knobs").and_then(Item::as_table_like),
            false,
        )
        .unwrap();

        assert!(problems.is_empty());
        assert_eq!(
            resolved["domains.fabric.graph.on_stop.stopped_no_work"].value,
            serde_json::json!("chair")
        );
        assert_eq!(
            resolved["domains.fabric.graph.projects.project-a.on_stop.blocked"].value,
            serde_json::json!("new_route")
        );
        assert!(!resolved.contains_key("domains.fabric.graph.on_stop.<stop_cause>"));
    }

    #[test]
    fn lane_access_is_documented_and_defaults_to_host() {
        let registry = registry().unwrap();
        let spec = &registry.knobs["lanes.access"];
        assert!(documented_field("lanes.access"));
        assert_eq!(spec.default, "host");
        assert_eq!(spec.values, ["host", "isolated"]);
    }

    #[test]
    fn lane_history_family_is_documented_with_its_defaults() {
        let registry = registry().unwrap();
        for (name, values, default) in [
            ("lanes.history.rebase", &["allow", "refuse"][..], "allow"),
            (
                "lanes.history.unrooted_tip",
                &["warn", "off", "refuse"][..],
                "warn",
            ),
            (
                "lanes.history.conflict_markers",
                &["warn", "off", "refuse"][..],
                "warn",
            ),
            (
                "lanes.history.default_branch_rewrite",
                &["refuse", "allow"][..],
                "refuse",
            ),
        ] {
            // Every knob is documented in the advanced reference, which is
            // generated from the registry itself. The compact page is capped,
            // so host-only keys stay out of it.
            assert!(registry.knobs.contains_key(name), "{name}");
            let spec = &registry.knobs[name];
            assert_eq!(spec.default, default, "{name}");
            // Flexible defaults that warn: an unknown value keeps the
            // documented default and `lane_history` reports it with its source.
            // A typo must not refuse the file, and the family is never strict.
            assert!(!spec.strict, "{name}");
            assert_eq!(
                spec.values,
                values
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>(),
                "{name}"
            );
        }
        for name in ["lanes.history.rebase", "lanes.history.unrooted_tip"] {
            assert!(documented_field(name), "{name}");
        }
        for name in [
            "lanes.history.conflict_markers",
            "lanes.history.default_branch_rewrite",
        ] {
            assert!(
                !documented_field(name),
                "{name} is host-only and documented in the advanced reference"
            );
        }
        // The host pre-push hook reads the conflict-marker setting, and the
        // default-branch rewrite is the owner's gate: no dispatch override.
        assert!(registry.knobs["lanes.history.conflict_markers"].host_only);
        assert!(registry.knobs["lanes.history.default_branch_rewrite"].host_only);
    }

    #[test]
    fn registry_runtime_deadline_maximum_is_the_lane_ceiling() {
        let registry: Value = serde_json::from_str(REGISTRY).unwrap();
        assert_eq!(
            registry["provider_updates"]["runtime_deadline_seconds"]["max"],
            provenant_core::MAX_LANE_RUNTIME_SECONDS
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod policy_action_tests {
    use super::*;

    #[test]
    fn generated_policy_knobs_offer_only_supported_strengths() {
        let registry = registry().unwrap();
        for rule in provenant_records::policy::table()
            .iter()
            .filter(|rule| rule.knob.is_some())
        {
            let name = rule.knob.as_deref().unwrap();
            let expected: std::collections::BTreeSet<_> = rule
                .strengths()
                .into_iter()
                .map(|strength| match strength {
                    provenant_records::policy::Strength::Refuse => "refuse",
                    provenant_records::policy::Strength::Warn => "warn",
                    provenant_records::policy::Strength::Off => "off",
                })
                .collect();
            let actual: std::collections::BTreeSet<_> = registry.knobs[name]
                .values
                .iter()
                .map(String::as_str)
                .collect();
            assert_eq!(actual, expected, "{}", rule.code);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod name_tests {
    use super::*;

    #[test]
    // Spec: docs/specs/cli.md#every-name-refusal-lists-close-matches
    fn a_one_edit_knob_typo_ranks_first_and_carries_candidates() {
        let registry = registry().unwrap();
        let matches = nearest(
            "chair.switch_account",
            registry.knobs.keys().map(String::as_str),
        );
        assert_eq!(
            matches.first().map(String::as_str),
            Some("chair.switch_accounts")
        );
        let ConfigError::UnknownKnob { close, .. } = unknown(registry, "chair.switch_account")
        else {
            panic!("unknown knob");
        };
        assert!(close.iter().any(|name| name == "chair.switch_accounts"));
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod precedence_tests {
    use super::*;

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn every_knob_layer_set_picks_the_host_and_shadows_the_rest() {
        let candidates = KNOB_PRECEDENCE
            .0
            .iter()
            .copied()
            .map(|layer| sourced(Value::Null, layer));
        let merge = KNOB_PRECEDENCE.merge(candidates);
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
