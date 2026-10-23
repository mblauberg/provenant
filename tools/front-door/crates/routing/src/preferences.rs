//! Hard-admissibility checks and stable preference ranking for route candidates.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::pending::python_string;

const AFFINITIES: [(&str, &str, &str); 3] = [
    ("family_affinity", "model_family", "unknown_family"),
    ("model_affinity", "model", "unknown_model"),
    ("adapter_affinity", "adapter", "unknown_adapter"),
];
const DEPRIORITIES: [(&str, &str, &str, &str); 3] = [
    (
        "families",
        "model_family",
        "family_deprioritise",
        "unknown_family",
    ),
    ("models", "model", "model_deprioritise", "unknown_model"),
    (
        "adapters",
        "adapter",
        "adapter_deprioritise",
        "unknown_adapter",
    ),
];
const TOP_LEVEL_FIELDS: [&str; 6] = [
    "schema_version",
    "task_classes",
    "roles",
    "deprioritise",
    "avoid",
    "spreading",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "observation")]
pub enum Availability {
    Observed { value: String },
    Unknown { reason: String },
    Unobserved,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub candidate_id: String,
    pub adapter: String,
    pub model_family: String,
    pub model: String,
    pub route: Value,
    pub availability: Availability,
    pub admissible: bool,
    pub selected: bool,
    pub disposition: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreferenceItem {
    pub kind: String,
    pub scope: String,
    pub value: String,
}

pub fn normalise_candidates(
    raw: &Value,
    task_class: &str,
    role: &str,
    policy: &BTreeMap<String, String>,
    alias_order: &BTreeMap<String, usize>,
    effort_order: &BTreeMap<String, usize>,
) -> Result<Vec<Candidate>, String> {
    if !python_one(raw.get("schema_version")) {
        return Err("candidate_set_invalid".into());
    }
    let Some(candidates) = raw.get("candidates").and_then(Value::as_array) else {
        return Err("candidate_set_invalid".into());
    };
    let mut identifiers = BTreeSet::new();
    let mut normalised = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let Some(candidate) = candidate.as_object() else {
            return Err("candidate_set_invalid".into());
        };
        let Some(candidate_id) = candidate
            .get("candidate_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return Err("candidate_set_invalid".into());
        };
        let Some(route) = candidate.get("route").and_then(Value::as_object) else {
            return Err("candidate_set_invalid".into());
        };
        if !identifiers.insert(candidate_id.to_owned()) {
            return Err("candidate_set_invalid".into());
        }
        let availability = parse_availability(candidate.get("availability"))
            .ok_or_else(|| "candidate_set_invalid".to_owned())?;
        let route_value = Value::Object(route.clone());
        let alias = route
            .get("alias")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let requested_effort = route
            .get("requested_effort")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let effort = route
            .get("effort")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let route_role = route
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let minimum_alias = policy
            .get("minimum_alias")
            .map(String::as_str)
            .unwrap_or_default();
        let minimum_effort = policy
            .get("minimum_effort")
            .map(String::as_str)
            .unwrap_or_default();
        let policy_role = policy.get("role").map(String::as_str).unwrap_or_default();
        let hard_policy_match = python_one(route.get("schema_version"))
            && route.get("status").and_then(Value::as_str) == Some("ok")
            && route.get("task_class").and_then(Value::as_str) == Some(task_class)
            && route.get("route_source").and_then(Value::as_str) == Some("task-class")
            && route_role == role
            && role == policy_role
            && matches!(alias_order.get(alias), Some(actual) if matches!(alias_order.get(minimum_alias), Some(floor) if actual >= floor))
            && matches!(effort_order.get(requested_effort), Some(actual) if matches!(effort_order.get(minimum_effort), Some(floor) if actual >= floor))
            && matches!(effort_order.get(effort), Some(actual) if matches!(effort_order.get(minimum_effort), Some(floor) if actual >= floor));
        let status = route.get("status").and_then(Value::as_str);
        let disposition = if status != Some("ok") {
            route
                .get("status")
                .filter(|value| crate::pending::value_truthy(value))
                .map(python_string)
                .unwrap_or_else(|| "hard_route_rejected".into())
        } else if !hard_policy_match {
            "hard_policy_mismatch".into()
        } else if matches!(availability, Availability::Observed { ref value } if value == "unavailable")
        {
            "observed_unavailable".into()
        } else {
            "admissible".into()
        };
        let adapter = route
            .get("adapter")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let model_family = route
            .get("model_family")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let model = route_model(route);
        normalised.push(Candidate {
            candidate_id: candidate_id.to_owned(),
            adapter,
            model_family,
            model,
            route: route_value,
            availability,
            admissible: disposition == "admissible",
            selected: false,
            disposition,
        });
    }
    Ok(normalised)
}

fn parse_availability(value: Option<&Value>) -> Option<Availability> {
    match value.and_then(Value::as_object) {
        Some(object) => {
            match object.get("observation").and_then(Value::as_str) {
                Some("Observed") => match object.get("value").and_then(Value::as_str) {
                    Some(value @ ("available" | "unavailable")) => Some(Availability::Observed {
                        value: value.into(),
                    }),
                    _ => None,
                },
                Some("Unknown") => object.get("reason").and_then(Value::as_str).map(|reason| {
                    Availability::Unknown {
                        reason: reason.into(),
                    }
                }),
                Some("Unobserved") => Some(Availability::Unobserved),
                _ => None,
            }
        }
        None => Some(Availability::Unknown {
            reason: "AvailabilityNotObserved".into(),
        }),
    }
}

fn python_one(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) if number.is_f64() => matches!(number.as_f64(), Some(1.0)),
        Some(Value::Number(number)) => number.as_i64() == Some(1) || number.as_u64() == Some(1),
        _ => false,
    }
}

fn route_model(route: &Map<String, Value>) -> String {
    route
        .get("catalog_model")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| route.get("resolved_model").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

pub fn preference_items(preferences: &Value, task_class: &str, role: &str) -> Vec<PreferenceItem> {
    let mut items = Vec::new();
    for (scope, table_name, key) in [
        ("task_class", "task_classes", task_class),
        ("role", "roles", role),
    ] {
        if let Some(scoped) = preferences
            .get(table_name)
            .and_then(|table| table.get(key))
            .and_then(Value::as_object)
        {
            for (kind, _, _) in AFFINITIES {
                items.extend(string_list(scoped.get(kind)).into_iter().map(|value| {
                    PreferenceItem {
                        kind: kind.into(),
                        scope: scope.into(),
                        value,
                    }
                }));
            }
        }
    }
    for section_name in ["deprioritise", "avoid"] {
        if let Some(section) = preferences.get(section_name).and_then(Value::as_object) {
            for (field, _, kind, _) in DEPRIORITIES {
                items.extend(string_list(section.get(field)).into_iter().map(|value| {
                    PreferenceItem {
                        kind: kind.into(),
                        scope: section_name.into(),
                        value,
                    }
                }));
            }
        }
    }
    items
}

pub fn configuration_ignored(preferences: &Value, task_class: &str, role: &str) -> Vec<Value> {
    let mut ignored = Vec::new();
    if let Some(object) = preferences.as_object() {
        for key in object
            .keys()
            .filter(|key| !TOP_LEVEL_FIELDS.contains(&key.as_str()))
        {
            ignored.push(ignored_item(key, "top_level", key, "unknown_key"));
        }
    }
    for (scope, table_name, key) in [
        ("task_class", "task_classes", task_class),
        ("role", "roles", role),
    ] {
        if let Some(scoped) = preferences
            .get(table_name)
            .and_then(|table| table.get(key))
            .and_then(Value::as_object)
        {
            for item in scoped
                .keys()
                .filter(|item| !AFFINITIES.iter().any(|(field, _, _)| field == item))
            {
                ignored.push(ignored_item(item, scope, item, "unknown_key"));
            }
        }
    }
    for section_name in ["deprioritise", "avoid"] {
        if let Some(section) = preferences.get(section_name).and_then(Value::as_object) {
            for item in section
                .keys()
                .filter(|item| !DEPRIORITIES.iter().any(|(field, _, _, _)| field == item))
            {
                ignored.push(ignored_item(item, section_name, item, "unknown_key"));
            }
        }
    }
    if let Some(spreading) = preferences.get("spreading").and_then(Value::as_object) {
        for item in spreading.keys().filter(|item| item.as_str() != "policy") {
            ignored.push(ignored_item(item, "spreading", item, "unknown_key"));
        }
        if let Some(policy) = spreading
            .get("policy")
            .filter(|value| !value.is_null() && *value != "fair-round-robin")
        {
            let policy_string = policy
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| python_string(policy));
            ignored.push(ignored_item(
                "spreading_policy",
                "spreading",
                &policy_string,
                "unsupported_policy",
            ));
        }
    }
    ignored
}

fn ignored_item(kind: &str, scope: &str, value: &str, reason: &str) -> Value {
    serde_json::json!({"kind":kind,"scope":scope,"value":value,"reason":reason})
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn deprioritised(preferences: &Value) -> BTreeMap<String, BTreeSet<String>> {
    let mut result = BTreeMap::from([
        ("model_family".into(), BTreeSet::new()),
        ("model".into(), BTreeSet::new()),
        ("adapter".into(), BTreeSet::new()),
    ]);
    for section_name in ["deprioritise", "avoid"] {
        if let Some(section) = preferences.get(section_name) {
            for (field, attribute, _, _) in DEPRIORITIES {
                if let Some(values) = result.get_mut(attribute) {
                    values.extend(string_list(section.get(field)));
                }
            }
        }
    }
    result
}

pub fn rank_maps(items: &[PreferenceItem]) -> BTreeMap<(String, String), BTreeMap<String, usize>> {
    let mut ranks = BTreeMap::new();
    for item in items {
        if !AFFINITIES.iter().any(|(kind, _, _)| *kind == item.kind) {
            continue;
        }
        let key = if item.kind == "family_affinity" {
            "model_family"
        } else if item.kind == "adapter_affinity" {
            "adapter"
        } else {
            "model"
        };
        let values: &mut BTreeMap<String, usize> =
            ranks.entry((item.scope.clone(), key.into())).or_default();
        if !values.contains_key(&item.value) {
            let rank = values.len();
            values.insert(item.value.clone(), rank);
        }
    }
    ranks
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PreferenceRank {
    affinity: [usize; 6],
    avoided: [bool; 3],
    family: String,
    adapter: String,
    model: String,
    candidate_id: String,
}

pub fn preference_rank(
    candidate: &Candidate,
    ranks: &BTreeMap<(String, String), BTreeMap<String, usize>>,
    deprioritised: &BTreeMap<String, BTreeSet<String>>,
) -> PreferenceRank {
    const FALLBACK: usize = 1_000_000;
    let attrs = ["model_family", "adapter", "model"];
    let values = [
        &candidate.model_family,
        &candidate.adapter,
        &candidate.model,
    ];
    let mut affinity = [FALLBACK; 6];
    for (scope_index, scope) in ["task_class", "role"].into_iter().enumerate() {
        for (attribute_index, (attribute, value)) in attrs.iter().zip(values).enumerate() {
            affinity[scope_index * 3 + attribute_index] = ranks
                .get(&(scope.into(), (*attribute).into()))
                .and_then(|map| map.get(value))
                .copied()
                .unwrap_or(FALLBACK);
        }
    }
    let avoided = std::array::from_fn(|index| {
        let attr = attrs[index];
        deprioritised
            .get(attr)
            .is_some_and(|values| values.contains(values_for_candidate(candidate, attr)))
    });
    PreferenceRank {
        affinity,
        avoided,
        family: candidate.model_family.clone(),
        adapter: candidate.adapter.clone(),
        model: candidate.model.clone(),
        candidate_id: candidate.candidate_id.clone(),
    }
}

fn values_for_candidate<'a>(candidate: &'a Candidate, attribute: &str) -> &'a str {
    match attribute {
        "model_family" => &candidate.model_family,
        "adapter" => &candidate.adapter,
        _ => &candidate.model,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreferenceStatus {
    Loaded,
    Corrupt,
    Unavailable,
    Invalid,
}

#[derive(Debug, thiserror::Error)]
pub enum PreferenceReadError {
    #[error("cannot read preferences at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("preferences at {path} are not UTF-8: {source}")]
    Encoding {
        path: std::path::PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error(transparent)]
    Json(#[from] crate::pending::JsonError),
}
pub fn read_json(
    path: &std::path::Path,
) -> Result<crate::ordered_json::OrderedValue, PreferenceReadError> {
    let bytes = std::fs::read(path).map_err(|source| PreferenceReadError::Io {
        path: path.into(),
        source,
    })?;
    let text = String::from_utf8(bytes).map_err(|source| PreferenceReadError::Encoding {
        path: path.into(),
        source,
    })?;
    Ok(crate::pending::from_json(&text)?)
}
pub fn load_preferences(
    path: &std::path::Path,
) -> Result<(crate::ordered_json::OrderedObject, PreferenceStatus), PreferenceReadError> {
    use crate::ordered_json::{OrderedObject, OrderedValue};
    let (preferences, status) = match read_json(path) {
        Ok(OrderedValue::Object(value)) => (value, PreferenceStatus::Loaded),
        Ok(_) => (OrderedObject::default(), PreferenceStatus::Invalid),
        Err(PreferenceReadError::Io { .. }) => {
            (OrderedObject::default(), PreferenceStatus::Unavailable)
        }
        Err(PreferenceReadError::Json(crate::pending::JsonError::Invalid(_))) => {
            (OrderedObject::default(), PreferenceStatus::Corrupt)
        }
        Err(error) => return Err(error),
    };
    Ok((preferences, status))
}

/// Preserve dictionary order in Python's string representation of unsupported policies.
pub fn configuration_ignored_ordered(
    preferences: &crate::ordered_json::OrderedValue,
    task_class: &str,
    role: &str,
) -> Vec<Value> {
    let mut ignored = configuration_ignored(&preferences.to_value(), task_class, role);
    if let Some(policy) = preferences.get("spreading").and_then(|s| s.get("policy")) {
        if let Some(item) = ignored
            .iter_mut()
            .find(|item| item.get("kind").and_then(Value::as_str) == Some("spreading_policy"))
        {
            item["value"] = Value::String(crate::pending::ordered_string(policy));
        }
    }
    ignored
}

#[cfg(test)]
mod differential_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn preferences_rank_only_admissible_routes_and_keep_python_status_spelling() {
        let policy = BTreeMap::from([
            ("role".into(), "worker".into()),
            ("minimum_alias".into(), "senior".into()),
            ("minimum_effort".into(), "high".into()),
        ]);
        let aliases = BTreeMap::from([
            ("junior".into(), 0),
            ("senior".into(), 1),
            ("principal".into(), 2),
        ]);
        let efforts = BTreeMap::from([("low".into(), 0), ("high".into(), 1), ("ultra".into(), 2)]);
        let good = json!({"schema_version":true,"status":"ok","task_class":"work","route_source":"task-class","role":"worker","alias":"senior","requested_effort":"high","effort":"high","adapter":"a","model_family":"f","resolved_model":"m"});
        let mut cases = vec![];
        for status in [
            json!(null),
            json!(false),
            json!(true),
            json!(2),
            json!(["x"]),
            json!("ok"),
        ] {
            let mut route = good.clone();
            route["status"] = status;
            cases.push(
                json!({"schema_version":1,"candidates":[{"candidate_id":"x","route":route}]}),
            );
        }
        for availability in [
            json!(null),
            json!({"observation":"Observed","value":"unavailable"}),
            json!({"observation":"Unobserved"}),
            json!({"observation":"Unknown","reason":""}),
            json!({"observation":"Observed","value":"maybe"}),
        ] {
            cases.push(json!({"schema_version":1,"candidates":[{"candidate_id":"x","route":good,"availability":availability}]}));
        }
        cases.extend([json!({"schema_version":1,"candidates":[{"candidate_id":"x","route":good},{"candidate_id":"x","route":good}]}),json!([]),json!({"schema_version":1,"candidates":null})]);
        let Some(expected): Option<Value> = crate::oracle::python(
            r#"
import importlib.util,json,pathlib,sys
s=importlib.util.spec_from_file_location('preferences',pathlib.Path('scripts/model_route_preferences.py'));m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
d=json.load(sys.stdin);print(json.dumps([m._normalise_candidates(c,'work','worker',d['policy'],d['aliases'],d['efforts']) for c in d['cases']]))
"#,
            &json!({"cases":cases,"policy":policy,"aliases":aliases,"efforts":efforts}),
        ) else {
            return;
        };
        let actual: Vec<_> = cases
            .iter()
            .map(|raw| {
                match normalise_candidates(raw, "work", "worker", &policy, &aliases, &efforts) {
                    Ok(c) => json!([c, ""]),
                    Err(e) => json!([[], e]),
                }
            })
            .collect();
        assert_eq!(json!(actual), expected);
    }
    #[test]
    fn unknown_preferences_and_ordered_policy_repr_match_python() {
        let raw = r#"{"z":true,"a":{},"task_classes":{"work":{"model_affinity":["m","m",3],"family_affinity":["f"],"zzz":false}},"roles":{"worker":{"adapter_affinity":["a"]}},"deprioritise":{"families":["f"],"models":["m"],"extra":true},"spreading":{"policy":{"z":true,"a":null}}}"#;
        let preferences: crate::ordered_json::OrderedValue =
            crate::pending::from_json(raw).unwrap();
        let Some(expected): Option<Value> = crate::oracle::python(
            r#"
import importlib.util,json,pathlib,sys
s=importlib.util.spec_from_file_location('preferences',pathlib.Path('scripts/model_route_preferences.py'));m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
p=json.loads(json.load(sys.stdin));print(json.dumps([m._preference_items(p,'work','worker'),m._configuration_ignored(p,'work','worker'),{k:sorted(v) for k,v in m._deprioritised(p).items()}]))
"#,
            &json!(raw),
        ) else {
            return;
        };
        let value = preferences.to_value();
        assert_eq!(
            json!([
                preference_items(&value, "work", "worker"),
                configuration_ignored_ordered(&preferences, "work", "worker"),
                deprioritised(&value)
            ]),
            expected
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prefs.json");
        assert_eq!(
            load_preferences(&path).unwrap().1,
            PreferenceStatus::Unavailable
        );
        for (raw, status) in [
            ("{", PreferenceStatus::Corrupt),
            ("[]", PreferenceStatus::Invalid),
            ("{}", PreferenceStatus::Loaded),
        ] {
            std::fs::write(&path, raw).unwrap();
            assert_eq!(load_preferences(&path).unwrap().1, status);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{
        configuration_ignored, normalise_candidates, preference_items, preference_rank,
        Availability,
    };

    #[test]
    fn forged_routes_below_the_hard_floor_are_not_admissible() {
        let raw = json!({"schema_version":1,"candidates":[{
            "candidate_id":"weak","route":{"schema_version":1,"status":"ok","task_class":"critical_review","route_source":"task-class","role":"reviewer","alias":"junior","requested_effort":"low","effort":"low"}
        }]});
        let candidates = normalise_candidates(
            &raw,
            "critical_review",
            "reviewer",
            &BTreeMap::from([
                ("minimum_alias".into(), "senior".into()),
                ("minimum_effort".into(), "medium".into()),
                ("role".into(), "reviewer".into()),
            ]),
            &BTreeMap::from([("junior".into(), 0), ("senior".into(), 1)]),
            &BTreeMap::from([("low".into(), 0), ("medium".into(), 1)]),
        )
        .unwrap();
        assert!(!candidates[0].admissible);
        assert_eq!(candidates[0].disposition, "hard_policy_mismatch");
    }

    #[test]
    fn absent_availability_is_unknown_and_preference_order_is_stable() {
        let raw = json!({"schema_version":1,"candidates":[{"candidate_id":"b","route":{"status":"ok"}}, {"candidate_id":"a","route":{"status":"ok"}}]});
        let candidates = normalise_candidates(
            &raw,
            "work",
            "worker",
            &BTreeMap::from([
                ("minimum_alias".into(), "junior".into()),
                ("minimum_effort".into(), "low".into()),
                ("role".into(), "worker".into()),
            ]),
            &BTreeMap::from([("junior".into(), 0)]),
            &BTreeMap::from([("low".into(), 0)]),
        )
        .unwrap();
        assert_eq!(
            candidates[0].availability,
            Availability::Unknown {
                reason: "AvailabilityNotObserved".into()
            }
        );
        let items = preference_items(
            &json!({"task_classes":{"work":{"model_affinity":["m"]}}}),
            "work",
            "worker",
        );
        let ranks = super::rank_maps(&items);
        assert!(
            preference_rank(&candidates[0], &ranks, &Default::default())
                > preference_rank(&candidates[1], &ranks, &Default::default())
        );
    }

    #[test]
    fn integer_like_python_schema_versions_and_unknown_policy_values_match_the_oracle() {
        let raw = json!({"schema_version":1.0,"candidates":[]});
        assert!(normalise_candidates(
            &raw,
            "work",
            "worker",
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new()
        )
        .is_ok());
        let ignored = configuration_ignored(
            &json!({"spreading":{"policy":"weighted"}}),
            "work",
            "worker",
        );
        assert_eq!(ignored[0]["value"], "weighted");
        assert_eq!(ignored[0]["reason"], "unsupported_policy");
        let ignored =
            configuration_ignored(&json!({"spreading":{"policy":true}}), "work", "worker");
        assert_eq!(ignored[0]["value"], "True");
        let ignored =
            configuration_ignored(&json!({"spreading":{"policy":["x"]}}), "work", "worker");
        assert_eq!(ignored[0]["value"], "['x']");
        let ignored =
            configuration_ignored(&json!({"spreading":{"policy":["a\nb"]}}), "work", "worker");
        assert_eq!(ignored[0]["value"], "['a\\nb']");
        let ignored =
            configuration_ignored(&json!({"spreading":{"policy":["it's"]}}), "work", "worker");
        assert_eq!(ignored[0]["value"], "[\"it's\"]");
        let ignored =
            configuration_ignored(&json!({"spreading":{"policy":["\u{0}"]}}), "work", "worker");
        assert_eq!(ignored[0]["value"], "['\\x00']");
        let ignored = configuration_ignored(
            &json!({"spreading":{"policy":["\u{a0}"]}}),
            "work",
            "worker",
        );
        assert_eq!(ignored[0]["value"], "['\\xa0']");
        let ignored = configuration_ignored(
            &json!({"spreading":{"policy":["\u{200d}"]}}),
            "work",
            "worker",
        );
        assert_eq!(ignored[0]["value"], "['\\u200d']");
        let ignored =
            configuration_ignored(&json!({"spreading":{"policy":1e-7}}), "work", "worker");
        assert_eq!(ignored[0]["value"], "1e-07");
        let ignored =
            configuration_ignored(&json!({"spreading":{"policy":1e-5}}), "work", "worker");
        assert_eq!(ignored[0]["value"], "1e-05");
    }
}
