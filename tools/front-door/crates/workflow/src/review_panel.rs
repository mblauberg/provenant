//! Declarative review panel presets, validation and reducer output.

use crate::review_ladder::{REVIEW_PLAN_STATUSES, SKIPPED_STATUSES};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub const PANEL_AXES: &[&str] = &["membership", "distribution", "reduction", "degradation"];
pub const PANEL_SPEC_KEYS: &[&str] = &[
    "preset",
    "membership",
    "distribution",
    "reduction",
    "degradation",
];
pub const PANEL_RESULT_KEYS: &[&str] = &["shortfall", "output"];
pub const PANEL_RECORD_KEYS: &[&str] = &["id", "spec", "result"];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PanelError {
    #[error("unknown panel preset: {0}")]
    UnknownPreset(String),
    #[error("use one panel override form")]
    ConflictingOverrideForms,
    #[error("panel overrides must name only panel axes")]
    InvalidOverride,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    InvalidOutput(String),
    #[error("reducer is unavailable: {0}")]
    ReducerUnavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelPreset {
    pub distribution: String,
    pub reduction: String,
    pub minimum_members: u64,
}

/// Reducer seam; source-varying reducer names stay in the registry table.
pub trait PanelReducer: Send + Sync {
    fn reduce(&self, output: &Value) -> Result<Value, String>;
}
impl<F> PanelReducer for F
where F: Fn(&Value) -> Result<Value, String> + Send + Sync {
    fn reduce(&self, output: &Value) -> Result<Value, String> { self(output) }
}
type Reducer = Arc<dyn PanelReducer>;

/// An immutable-by-sharing set of presets and reducers.
///
/// Builder methods consume and return the registry. This permits isolated
/// extensions without a mutable global table or a copied test seam.
#[derive(Clone)]
pub struct PanelRegistry {
    presets: BTreeMap<String, PanelPreset>,
    reducers: BTreeMap<String, Reducer>,
}

impl Default for PanelRegistry {
    fn default() -> Self {
        let presets = BTreeMap::from([
            (
                "council".to_owned(),
                PanelPreset {
                    distribution: "shared".to_owned(),
                    reduction: "agreements-conflicts".to_owned(),
                    minimum_members: 3,
                },
            ),
            (
                "breadth".to_owned(),
                PanelPreset {
                    distribution: "split".to_owned(),
                    reduction: "union".to_owned(),
                    minimum_members: 1,
                },
            ),
        ]);
        let reducers: BTreeMap<String, Reducer> = BTreeMap::from([
            (
                "agreements-conflicts".to_owned(),
                Arc::new(agreements_conflicts) as Reducer,
            ),
            ("union".to_owned(), Arc::new(union) as Reducer),
        ]);
        Self { presets, reducers }
    }
}

impl PanelRegistry {
    pub fn with_preset(mut self, name: impl Into<String>, preset: PanelPreset) -> Self {
        self.presets.insert(name.into(), preset);
        self
    }

    pub fn with_reducer<F>(self, name: impl Into<String>, reducer: F) -> Self
    where F: Fn(&Value) -> Result<Value, String> + Send + Sync + 'static {
        self.with_reducer_impl(name, reducer)
    }
    pub fn with_reducer_impl<R: PanelReducer + 'static>(mut self, name: impl Into<String>, reducer: R) -> Self {

        self.reducers.insert(name.into(), Arc::new(reducer));
        self
    }

    pub fn known_distributions(&self) -> BTreeSet<String> {
        self.presets
            .values()
            .filter(|row| !row.distribution.is_empty())
            .map(|row| row.distribution.clone())
            .collect()
    }

    pub fn reduce(&self, name: &str, output: &Value) -> Result<Value, PanelError> {
        let reducer = self
            .reducers
            .get(name)
            .ok_or_else(|| PanelError::ReducerUnavailable(name.to_owned()))?;
        reducer.reduce(output).map_err(PanelError::InvalidOutput)
    }

    fn preset(&self, name: &str) -> Option<&PanelPreset> {
        self.presets.get(name)
    }

    fn has_reducer(&self, name: &str) -> bool {
        self.reducers.contains_key(name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelSpec {
    pub preset: String,
    pub membership: Value,
    pub distribution: Value,
    pub reduction: Value,
    pub degradation: Value,
}

impl PanelSpec {
    /// Raw panel input must be checked before converting it to JSON values.
    /// In particular, a Python-sized integer must never become a rounded float
    /// and then receive an ordinary "positive integer" validation diagnostic.
    pub fn from_json(raw: &[u8]) -> Result<Self, provenant_records::checked_json::DecodeError> {
        let checked = provenant_records::checked_json::from_slice(raw)?;
        Ok(serde_json::from_value(checked)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelShortfall {
    pub minimum_members: u64,
    pub completed_members: u64,
    pub missing_members: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelResult {
    #[serde(deserialize_with = "crate::pending::required_option")]
    pub shortfall: Option<PanelShortfall>,
    pub output: Value,
}

pub fn known_distributions() -> BTreeSet<String> {
    PanelRegistry::default().known_distributions()
}

pub fn resolve_panel(
    preset: &str,
    overrides: Option<&Value>,
    axis_overrides: &Map<String, Value>,
) -> Result<PanelSpec, PanelError> {
    resolve_panel_with(&PanelRegistry::default(), preset, overrides, axis_overrides)
}

pub fn resolve_panel_with(
    registry: &PanelRegistry,
    preset: &str,
    overrides: Option<&Value>,
    axis_overrides: &Map<String, Value>,
) -> Result<PanelSpec, PanelError> {
    let overrides = overrides.filter(|value| !value.is_null());
    let row = registry
        .preset(preset)
        .ok_or_else(|| PanelError::UnknownPreset(preset.to_owned()))?;

    if overrides.is_some() && !axis_overrides.is_empty() {
        return Err(PanelError::ConflictingOverrideForms);
    }
    let override_map = match overrides {
        None => None,
        Some(Value::Object(map)) if allowed_axes(map.keys()) => Some(map),
        Some(_) => return Err(PanelError::InvalidOverride),
    };
    if !allowed_axes(axis_overrides.keys()) {
        return Err(PanelError::InvalidOverride);
    }

    let mut spec = PanelSpec {
        preset: preset.to_owned(),
        membership: json!([]),
        distribution: json!(row.distribution),
        reduction: json!(row.reduction),
        degradation: json!({"minimum_members": row.minimum_members}),
    };
    if let Some(map) = override_map {
        apply_overrides(&mut spec, map);
    } else {
        apply_overrides(&mut spec, axis_overrides);
    }

    Ok(spec)
}

fn allowed_axes<'a>(keys: impl Iterator<Item = &'a String>) -> bool {
    keys.into_iter()
        .all(|key| PANEL_AXES.contains(&key.as_str()))
}

fn apply_overrides(spec: &mut PanelSpec, overrides: &Map<String, Value>) {
    for (axis, value) in overrides {
        match axis.as_str() {
            "membership" => spec.membership = value.clone(),
            "distribution" => spec.distribution = value.clone(),
            "reduction" => spec.reduction = value.clone(),
            "degradation" => spec.degradation = value.clone(),
            _ => {}
        }
    }
}

pub fn validate_panel(spec: &Value, members: &Value) -> Vec<String> {
    validate_panel_with(&PanelRegistry::default(), spec, members)
}

pub fn validate_panel_with(registry: &PanelRegistry, spec: &Value, members: &Value) -> Vec<String> {
    let Some(spec_object) = exact_object(spec, PANEL_SPEC_KEYS) else {
        return vec!["panel spec must use the closed panel spec schema".to_owned()];
    };

    let mut errors = Vec::new();

    match spec_object.get("preset").and_then(Value::as_str) {
        Some(preset) if registry.preset(preset).is_some() => {}
        _ => errors.push("panel spec.preset is unknown".to_owned()),
    }

    let membership = spec_object.get("membership");
    let valid_membership = membership.and_then(Value::as_array).is_some_and(|items| {
        items
            .iter()
            .all(|item| item.as_str().is_some_and(|s| !s.is_empty()))
    });
    let declared: Vec<&str> = if valid_membership {
        membership
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    } else {
        errors.push("panel spec.membership must be a list of non-empty review ids".to_owned());
        Vec::new()
    };
    if valid_membership {
        let distinct: BTreeSet<_> = declared.iter().copied().collect();
        if distinct.len() != declared.len() {
            errors.push("panel spec.membership review ids must be unique".to_owned());
        }
    }

    let distributions = registry.known_distributions();
    match spec_object.get("distribution").and_then(Value::as_str) {
        Some(distribution) if distributions.contains(distribution) => {}
        _ => errors.push("panel spec.distribution is unknown".to_owned()),
    }
    match spec_object.get("reduction").and_then(Value::as_str) {
        Some(reduction) if registry.has_reducer(reduction) => {}
        _ => errors.push("panel spec.reduction is unknown".to_owned()),
    }

    match spec_object
        .get("degradation")
        .and_then(|value| exact_object(value, &["minimum_members"]))
    {
        Some(degradation) => {
            let minimum = degradation.get("minimum_members");
            if !minimum.is_some_and(is_positive_integer) {
                errors.push(
                    "panel spec.degradation.minimum_members must be a positive integer".to_owned(),
                );
            }
        }
        None => errors.push("panel spec.degradation must contain minimum_members".to_owned()),
    }

    let Some(member_rows) = members.as_array() else {
        errors.push("panel members must be a list".to_owned());
        return errors;
    };

    let mut seen = BTreeSet::new();
    let mut actual = Vec::new();
    for (index, member) in member_rows.iter().enumerate() {
        let Some(row) = member.as_object() else {
            errors.push(format!(
                "panel members[{index}] must reference a review record"
            ));
            continue;
        };
        match row.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => {
                if seen.insert(id) {
                    actual.push(id);
                } else {
                    errors.push(format!("panel members[{index}].id must be unique"));
                }
            }
            _ => errors.push(format!(
                "panel members[{index}].id must be a non-empty review id"
            )),
        }

        match row.get("status").and_then(Value::as_str) {
            Some(status) if REVIEW_PLAN_STATUSES.contains(&status) => {}
            _ => errors.push(format!("panel members[{index}].status is invalid")),
        }
    }

    if valid_membership {
        let actual_set: BTreeSet<_> = actual.into_iter().collect();
        let declared_set: BTreeSet<_> = declared.into_iter().collect();
        if actual_set != declared_set {
            errors.push("panel members do not match spec.membership".to_owned());
        }
    }
    errors
}

pub fn panel_result(
    spec: &Value,
    members: &Value,
    output: &Value,
) -> Result<PanelResult, PanelError> {
    panel_result_with(&PanelRegistry::default(), spec, members, output)
}

pub fn panel_result_with(
    registry: &PanelRegistry,
    spec: &Value,
    members: &Value,
    output: &Value,
) -> Result<PanelResult, PanelError> {
    let errors = validate_panel_with(registry, spec, members);
    if !errors.is_empty() {
        return Err(PanelError::Invalid(errors.join("; ")));
    }
    let Some(spec_object) = spec.as_object() else {
        return Err(PanelError::Invalid(
            "panel spec must use the closed panel spec schema".to_owned(),
        ));
    };
    let Some(reduction) = spec_object.get("reduction").and_then(Value::as_str) else {
        return Err(PanelError::Invalid(
            "panel spec.reduction is unknown".to_owned(),
        ));
    };
    let normalized = registry.reduce(reduction, output)?;
    let shortfall = panel_shortfall(spec_object, members)?;
    Ok(PanelResult {
        shortfall,
        output: normalized,
    })
}

pub fn validate_panel_result(spec: &Value, members: &Value, result: &Value) -> Vec<String> {
    validate_panel_result_with(&PanelRegistry::default(), spec, members, result)
}

pub fn validate_panel_result_with(
    registry: &PanelRegistry,
    spec: &Value,
    members: &Value,
    result: &Value,
) -> Vec<String> {
    let mut errors = validate_panel_with(registry, spec, members);
    if !errors.is_empty() {
        return errors;
    }
    let Some(result_object) = exact_object(result, PANEL_RESULT_KEYS) else {
        return vec!["panel result must use the closed panel result schema".to_owned()];
    };
    let Some(spec_object) = spec.as_object() else {
        return vec!["panel spec must use the closed panel spec schema".to_owned()];
    };
    let expected = match panel_shortfall(spec_object, members) {
        Ok(value) => shortfall_value(value.as_ref()),
        Err(error) => {
            errors.push(error.to_string());
            return errors;
        }
    };
    let actual = result_object.get("shortfall").unwrap_or(&Value::Null);
    if !provenant_core::py::python_json_eq(actual, &expected) {
        errors.push("panel result.shortfall does not match completed membership".to_owned());
    }

    let Some(reduction) = spec_object.get("reduction").and_then(Value::as_str) else {
        return errors;
    };
    let Some(output) = result_object.get("output") else {
        return errors;
    };
    match registry.reduce(reduction, output) {
        Err(error) => errors.push(format!("panel result.output is invalid: {error}")),
        Ok(normalized) if !provenant_core::py::python_json_eq(&normalized, output) => {
            errors.push("panel result.output is not normalized by its declared reducer".to_owned())
        }
        Ok(_) => {}
    }
    errors
}

pub fn panel_shortfall(
    spec: &Map<String, Value>,
    members: &Value,
) -> Result<Option<PanelShortfall>, PanelError> {
    let degradation = spec
        .get("degradation")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            PanelError::Invalid("panel spec.degradation must contain minimum_members".to_owned())
        })?;
    let minimum = degradation
        .get("minimum_members")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            PanelError::Invalid(
                "panel spec.degradation.minimum_members must be a positive integer".to_owned(),
            )
        })?;
    let member_rows = members
        .as_array()
        .ok_or_else(|| PanelError::Invalid("panel members must be a list".to_owned()))?;
    let completed = member_rows
        .iter()
        .filter(|member| {
            let status = member
                .as_object()
                .and_then(|row| row.get("status"))
                .and_then(Value::as_str);
            !status.is_some_and(|value| SKIPPED_STATUSES.contains(&value))
        })
        .count() as u64;
    if completed >= minimum {
        return Ok(None);
    }
    Ok(Some(PanelShortfall {
        minimum_members: minimum,
        completed_members: completed,
        missing_members: minimum - completed,
    }))
}

fn shortfall_value(shortfall: Option<&PanelShortfall>) -> Value {
    match shortfall {
        None => Value::Null,
        Some(value) => json!({
            "minimum_members": value.minimum_members,
            "completed_members": value.completed_members,
            "missing_members": value.missing_members,
        }),
    }
}

fn exact_object<'a>(value: &'a Value, required: &[&str]) -> Option<&'a Map<String, Value>> {
    let object = value.as_object()?;
    (object.len() == required.len() && required.iter().all(|key| object.contains_key(*key)))
        .then_some(object)
}

fn is_positive_integer(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.as_u64().is_some_and(|n| n > 0),
        _ => false,
    }
}

fn string_list(value: &Value, field: &str) -> Result<Vec<String>, String> {
    let Some(items) = value.as_array() else {
        return Err(format!("{field} must be a list of non-empty strings"));
    };
    let mut values = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str().filter(|text| !text.is_empty()) else {
            return Err(format!("{field} must be a list of non-empty strings"));
        };
        values.push(text.to_owned());
    }
    Ok(values)
}

fn agreements_conflicts(output: &Value) -> Result<Value, String> {
    let keys = ["agreements", "conflicts", "minority_views"];
    let Some(object) = exact_object(output, &keys) else {
        return Err(
            "agreements-conflicts output must contain agreements, conflicts, and minority_views"
                .to_owned(),
        );
    };
    let mut normalized = Map::new();
    for field in keys {
        let values = string_list(object.get(field).unwrap_or(&Value::Null), field)?;
        normalized.insert(field.to_owned(), json!(values));
    }
    Ok(Value::Object(normalized))
}

fn union(output: &Value) -> Result<Value, String> {
    let Some(object) = exact_object(output, &["findings"]) else {
        return Err("union output must contain findings".to_owned());
    };
    let values = string_list(object.get("findings").unwrap_or(&Value::Null), "findings")?;
    let mut seen = BTreeSet::new();
    let deduplicated = values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect::<Vec<_>>();
    Ok(json!({"findings": deduplicated}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn council_spec() -> Value {
        json!({
            "preset":"council",
            "membership":["design","ux","product","challenge"],
            "distribution":"shared",
            "reduction":"agreements-conflicts",
            "degradation":{"minimum_members":3}
        })
    }

    fn members() -> Value {
        json!([
            {"id":"design","status":"complete"},
            {"id":"ux","status":"complete"},
            {"id":"product","status":"complete"},
            {"id":"challenge","status":"complete"}
        ])
    }

    fn axis_overrides(values: Value) -> Map<String, Value> {
        values.as_object().cloned().unwrap_or_default()
    }

    fn python_outcome(case: &Value) -> Value {
        let result = match case.get("op").and_then(Value::as_str) {
            Some("resolve") => {
                let overrides = case.get("overrides");
                let axes = case
                    .get("axis_overrides")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                resolve_panel(
                    case.get("preset").and_then(Value::as_str).unwrap_or(""),
                    overrides,
                    &axes,
                )
                .map(|value| json!(value))
            }
            Some("validate") => Ok(json!(validate_panel(
                case.get("spec").unwrap_or(&Value::Null),
                case.get("members").unwrap_or(&Value::Null),
            ))),
            Some("result") => panel_result(
                case.get("spec").unwrap_or(&Value::Null),
                case.get("members").unwrap_or(&Value::Null),
                case.get("output").unwrap_or(&Value::Null),
            )
            .map(|value| json!(value)),
            Some("validate_result") => Ok(json!(validate_panel_result(
                case.get("spec").unwrap_or(&Value::Null),
                case.get("members").unwrap_or(&Value::Null),
                case.get("result").unwrap_or(&Value::Null),
            ))),
            Some("reduce") => {
                let name = case.get("name").and_then(Value::as_str).unwrap_or("");
                PanelRegistry::default()
                    .reduce(name, case.get("output").unwrap_or(&Value::Null))
                    .map_err(|error| match error {
                        PanelError::InvalidOutput(message) => PanelError::Invalid(message),
                        other => other,
                    })
            }
            _ => Err(PanelError::Invalid(
                "unknown differential operation".to_owned(),
            )),
        };
        match result {
            Ok(value) => value,
            Err(error) => json!({"error":error.to_string()}),
        }
    }

    #[test]
    fn oversized_minimum_is_a_typed_boundary_refusal_and_python_accepts_it() {
        let raw = r#"{"preset":"breadth","membership":[],"distribution":"split","reduction":"union","degradation":{"minimum_members":1000000000000000000000000000000}}"#;
        assert!(matches!(PanelSpec::from_json(raw.as_bytes()), Err(provenant_records::checked_json::DecodeError::NumberOutOfRange(_))));
        if let Some(outputs) = crate::test_support::oracle("review_panel", &json!([{"op":"raw_shortfall","raw":raw}])) {
            assert!(outputs[0]["errors"].as_array().unwrap().is_empty());
            assert!(outputs[0]["wire"].as_str().unwrap().contains("1000000000000000000000000000000"));
        }
        let spec = json!({"degradation":{"minimum_members":u64::MAX}});
        assert_eq!(panel_shortfall(spec.as_object().unwrap(), &json!([])).unwrap().unwrap().minimum_members, u64::MAX);
    }

    #[test]
    fn resolves_defaults_and_overrides_without_mutating_registry() {
        let before = PanelRegistry::default();
        let overrides = json!({
            "membership":["a","b"],
            "distribution":"split",
            "reduction":"union",
            "degradation":{"minimum_members":1}
        });
        let spec = resolve_panel("council", Some(&overrides), &Map::new()).unwrap();
        assert_eq!(
            json!(spec),
            json!({
                "preset":"council",
                "membership":["a","b"],
                "distribution":"split",
                "reduction":"union",
                "degradation":{"minimum_members":1}
            })
        );
        assert_eq!(before.known_distributions(), known_distributions());
    }

    #[test]
    fn validates_policy_and_preserves_nonblocking_shortfall() {
        let spec = council_spec();
        let rows = json!([
            {"id":"design","status":"complete"},
            {"id":"ux","status":"complete"},
            {"id":"product","status":"failed"},
            {"id":"challenge","status":"omitted"}
        ]);
        assert!(validate_panel(&spec, &rows).is_empty());
        let result = panel_result(
            &spec,
            &rows,
            &json!({
                "agreements":["A"],
                "conflicts":["B"],
                "minority_views":["C"]
            }),
        )
        .unwrap();
        assert_eq!(
            json!(result.shortfall),
            json!({
                "minimum_members":3,
                "completed_members":2,
                "missing_members":1
            })
        );
        assert!(validate_panel_result(&spec, &rows, &json!(result)).is_empty());

        for skipped_status in SKIPPED_STATUSES {
            let mut rows = rows.clone();
            if let Some(challenge) = rows.get_mut(3) {
                challenge["status"] = json!(skipped_status);
            }
            assert!(validate_panel(&spec, &rows).is_empty());
            let result = panel_result(
                &spec,
                &rows,
                &json!({
                    "agreements": [],
                    "conflicts": [],
                    "minority_views": []
                }),
            )
            .unwrap();
            assert_eq!(
                result
                    .shortfall
                    .as_ref()
                    .map(|value| value.completed_members),
                Some(2)
            );
        }
    }

    #[test]
    fn reducers_normalize_and_deduplicate_in_source_order() {
        assert_eq!(
            PanelRegistry::default()
                .reduce(
                    "agreements-conflicts",
                    &json!({
                        "agreements":["A"],
                        "conflicts":["B"],
                        "minority_views":["C"]
                    })
                )
                .unwrap(),
            json!({
                "agreements":["A"],
                "conflicts":["B"],
                "minority_views":["C"]
            })
        );
        assert_eq!(
            PanelRegistry::default()
                .reduce("union", &json!({"findings":["A","B","A"]}))
                .unwrap(),
            json!({"findings":["A","B"]})
        );
    }

    #[test]
    fn immutable_registry_supports_one_row_and_one_reducer_extension() {
        let registry = PanelRegistry::default()
            .with_preset(
                "risk-scan",
                PanelPreset {
                    distribution: "sequenced".to_owned(),
                    reduction: "risks".to_owned(),
                    minimum_members: 2,
                },
            )
            .with_reducer("risks", |output| {
                let Some(object) = exact_object(output, &["risks"]) else {
                    return Err("risks output must contain risks".to_owned());
                };
                let values = string_list(object.get("risks").unwrap_or(&Value::Null), "risks")?;
                let mut seen = BTreeSet::new();
                Ok(json!({
                    "risks": values.into_iter()
                        .filter(|value| seen.insert(value.clone()))
                        .collect::<Vec<_>>()
                }))
            });
        let spec = json!({
            "preset":"risk-scan",
            "membership":["a","b"],
            "distribution":"sequenced",
            "reduction":"risks",
            "degradation":{"minimum_members":2}
        });
        let member_rows = json!([{"id":"a","status":"complete"},{"id":"b","status":"complete"}]);
        let result =
            panel_result_with(&registry, &spec, &member_rows, &json!({"risks":["A","A"]})).unwrap();
        assert!(
            validate_panel_result_with(&registry, &spec, &member_rows, &json!(result)).is_empty()
        );
        assert_eq!(result.output, json!({"risks":["A"]}));
        assert_eq!(
            PanelRegistry::default().known_distributions(),
            known_distributions()
        );
    }

    #[test]
    fn malformed_values_return_errors_without_panicking() {
        let cases = [
            (Value::Null, Value::Null),
            (json!({"preset":"council"}), json!([])),
            (
                json!({"preset":"council","membership":"review-a","distribution":"shared","reduction":"agreements-conflicts","degradation":{"minimum_members":3}}),
                json!([]),
            ),
            (council_spec(), json!("not-members")),
        ];
        assert!(cases
            .iter()
            .all(|(spec, rows)| !validate_panel(spec, rows).is_empty()));
        assert!(resolve_panel("missing", None, &Map::new()).is_err());
        assert_eq!(
            resolve_panel(
                "council",
                Some(&json!({"distribution":"split"})),
                &axis_overrides(json!({"distribution":"shared"}))
            )
            .unwrap_err()
            .to_string(),
            "use one panel override form"
        );
    }

    #[test]
    fn python_policy_cases_match_live_oracle() {
        let spec = council_spec();
        let complete = members();
        let skipped = json!([
            {"id":"design","status":"complete"},
            {"id":"ux","status":"complete"},
            {"id":"product","status":"failed"},
            {"id":"challenge","status":"omitted"}
        ]);
        let cases = json!([
            {"op":"resolve","preset":"council","overrides":{
                "membership":["a","b"],"distribution":"split","reduction":"union",
                "degradation":{"minimum_members":1}
            }},
            {"op":"resolve","preset":"unknown"},
            {"op":"resolve","preset":"council","overrides":{"unknown":true}},
            {"op":"resolve","preset":"council","overrides":{"distribution":"split"},
                "axis_overrides":{"distribution":"shared"}},
            {"op":"validate","spec":null,"members":null},
            {"op":"validate","spec":spec,"members":skipped},
            {"op":"validate","spec":{
                "preset":"council","membership":["a","a"],"distribution":"typo",
                "reduction":"missing","degradation":{"minimum_members":true}
            },"members":[{"id":"a","status":[]}]},
            {"op":"validate","spec":{
                "preset":"council","membership":[],"distribution":"shared",
                "reduction":"agreements-conflicts","degradation":{"minimum_members":1.0}
            },"members":[null,{"id":"a","status":"future"},{"id":"a","status":"complete"}]},
            {"op":"result","spec":spec,"members":skipped,"output":{
                "agreements":["A"],"conflicts":["B"],"minority_views":["C"]
            }},
            {"op":"result","spec":spec,"members":complete,"output":{
                "agreements":["A"],"conflicts":["B"],"minority_views":["C"],"extra":"no"
            }},
            {"op":"validate_result","spec":spec,"members":complete,"result":{
                "shortfall":null,"output":{"agreements":[],"conflicts":[],"minority_views":[]}
            }},
            {"op":"validate_result","spec":spec,"members":complete,"result":{
                "shortfall":{"minimum_members":3,"completed_members":true,"missing_members":0},
                "output":{"agreements":[],"conflicts":[],"minority_views":[]}
            }},
            {"op":"validate_result","spec":{
                "preset":"breadth","membership":[],"distribution":"split",
                "reduction":"union","degradation":{"minimum_members":1}
            },"members":[],"result":{
                "shortfall":{"minimum_members":1,"completed_members":0,"missing_members":true},
                "output":{"findings":[]}
            }},
            {"op":"reduce","name":"union","output":{"findings":["A","B","A"]}},
            {"op":"reduce","name":"agreements-conflicts","output":{
                "agreements":["A"],"conflicts":["B"],"minority_views":["C"]
            }}
        ]);
        let actual: Vec<_> = cases
            .as_array()
            .into_iter()
            .flatten()
            .map(python_outcome)
            .collect();
        if let Some(expected) = crate::test_support::oracle("review_panel", &cases) {
            assert_eq!(json!(actual), expected);
        }
    }
}
