//! Fair selection and receipts over hard-admissible routes. Persistence is a
//! locked transaction supplied by the composing owner, never a second FS owner.
use crate::{
    ordered_json::OrderedValue,
    preferences::{self, Candidate, CandidateError, PreferenceItem, PreferenceStatus},
};
use provenant_core::py::python_json_eq;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SelectionError {
    #[error(transparent)]
    Candidates(#[from] CandidateError),
    #[error(transparent)]
    Json(#[from] provenant_records::DecodeError),
    #[error("selection integer is outside i64/u64 range")]
    NumberOutOfRange,
    #[error("selection {operation} failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("selection input at {0} is not UTF-8")]
    Encoding(PathBuf),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpreadState {
    pub schema_version: u64,
    pub assignments: BTreeMap<String, u64>,
    // Python accepts a bool or integral float equal to the integer sum, retaining
    // that input kind in state_before; a float counter stays float on increment.
    pub selection_count: Value,
}
impl Default for SpreadState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            assignments: BTreeMap::new(),
            selection_count: json!(0),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StateStatus {
    Initialised,
    Loaded,
    Invalid,
}

pub fn load_spread_state(
    raw: Option<&str>,
) -> Result<(Option<SpreadState>, StateStatus), SelectionError> {
    let Some(raw) = raw else {
        return Ok((Some(SpreadState::default()), StateStatus::Initialised));
    };
    let value = match provenant_records::checked_json::from_str(raw) {
        Ok(v) => v,
        Err(e @ provenant_records::DecodeError::NumberOutOfRange(_)) => return Err(e.into()),
        Err(_) => return Ok((None, StateStatus::Invalid)),
    };
    let invalid = || Ok((None, StateStatus::Invalid));
    let Some(assignments) = value.get("assignments").and_then(Value::as_object) else {
        return invalid();
    };
    if !value
        .get("schema_version")
        .is_some_and(|v| python_json_eq(v, &json!(1)))
    {
        return invalid();
    }
    let mut counts = BTreeMap::new();
    let mut total = 0u64;
    for (family, count) in assignments {
        let Some(count) = count.as_u64().filter(|_| !count.is_f64()) else {
            return invalid();
        };
        if family.is_empty() {
            return invalid();
        }
        total = total
            .checked_add(count)
            .ok_or(SelectionError::NumberOutOfRange)?;
        counts.insert(family.clone(), count);
    }
    let Some(selection_count) = value
        .get("selection_count")
        .filter(|v| python_json_eq(v, &json!(total)))
    else {
        return invalid();
    };
    Ok((
        Some(SpreadState {
            schema_version: 1,
            assignments: counts,
            selection_count: selection_count.clone(),
        }),
        StateStatus::Loaded,
    ))
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct PreferenceReceipt {
    pub file_status: PreferenceStatus,
    pub applied: Vec<PreferenceItem>,
    pub not_honoured: Vec<Value>,
    pub ignored: Vec<Value>,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SpreadingReceipt {
    pub policy: &'static str,
    pub state_status: StateStatus,
    pub state_before: SpreadState,
    pub state_after: SpreadState,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SelectionRecord {
    pub schema_version: u64,
    pub status: &'static str,
    pub task_class: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen_candidate_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen_route: Option<OrderedValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<Candidate>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preference: Option<PreferenceReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spreading: Option<SpreadingReceipt>,
}
impl SelectionRecord {
    fn base(task: &str, role: &str, status: &'static str) -> Self {
        Self {
            schema_version: 1,
            status,
            task_class: task.into(),
            role: role.into(),
            selection_source: Some("hard-admissible-preference"),
            chosen_candidate_id: None,
            chosen_route: None,
            candidates: None,
            preference: None,
            spreading: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct SelectionOutput {
    pub code: i32,
    pub record: SelectionRecord,
}
impl SelectionOutput {
    pub fn stdout(&self) -> Result<String, serde_json::Error> {
        let raw = serde_json::to_value(&self.record)?;
        Ok(format!(
            "{}\n",
            crate::pending::json_dumps(&raw.into(), true, None)
        ))
    }
}
fn attribute<'a>(candidate: &'a Candidate, key: &str) -> &'a str {
    match key {
        "model_family" => &candidate.model_family,
        "adapter" => &candidate.adapter,
        _ => &candidate.model,
    }
}
pub fn preference_receipt(
    items: &[PreferenceItem],
    candidates: &[Candidate],
    chosen: &Candidate,
    least_used: &[usize],
) -> (Vec<PreferenceItem>, Vec<Value>, Vec<Value>) {
    let mut applied = vec![];
    let mut not_honoured = vec![];
    let mut ignored = vec![];
    for item in items {
        let (attr, unknown, deprioritise) = match item.kind.as_str() {
            "family_affinity" => ("model_family", "unknown_family", false),
            "model_affinity" => ("model", "unknown_model", false),
            "adapter_affinity" => ("adapter", "unknown_adapter", false),
            "family_deprioritise" => ("model_family", "unknown_family", true),
            "model_deprioritise" => ("model", "unknown_model", true),
            _ => ("adapter", "unknown_adapter", true),
        };
        let matching: Vec<_> = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| attribute(c, attr) == item.value)
            .collect();
        let with_reason = |reason: &str| json!({"kind":item.kind,"scope":item.scope,"value":item.value,"reason":reason});
        if matching.is_empty() {
            ignored.push(with_reason(unknown));
        } else if !matching.iter().any(|(_, c)| c.admissible) {
            if deprioritise {
                applied.push(item.clone());
                continue;
            }
            let reason = if item.kind == "adapter_affinity"
                && matching
                    .iter()
                    .any(|(_, c)| c.disposition == "adapter_disabled")
            {
                "adapter_disabled"
            } else if attr == "model_family" {
                "no_admissible_candidate"
            } else {
                "outside_admissible_set"
            };
            not_honoured.push(with_reason(reason));
        } else if deprioritise != (attribute(chosen, attr) == item.value) {
            applied.push(item.clone());
        } else {
            let reason = if deprioritise && candidates.iter().filter(|c| c.admissible).count() == 1
            {
                "sole_admissible_candidate"
            } else if candidates.iter().enumerate().any(|(i, c)| {
                c.admissible
                    && (deprioritise != (attribute(c, attr) == item.value))
                    && least_used.contains(&i)
            }) {
                "lost_to_higher_preference"
            } else {
                "fair_spreading"
            };
            not_honoured.push(with_reason(reason));
        }
    }
    (applied, not_honoured, ignored)
}

pub struct SelectionContext<'a> {
    pub task_class: &'a str,
    pub role: &'a str,
    pub policy_table: &'a BTreeMap<String, BTreeMap<String, String>>,
    pub aliases: &'a BTreeMap<String, usize>,
    pub efforts: &'a BTreeMap<String, usize>,
}
pub fn select_documents(
    context: &SelectionContext<'_>,
    raw_candidates: &OrderedValue,
    preferences: &OrderedValue,
    preferences_status: PreferenceStatus,
    state: Option<SpreadState>,
    state_status: StateStatus,
) -> Result<SelectionOutput, SelectionError> {
    let mut record = SelectionRecord::base(context.task_class, context.role, "unknown_task_class");
    let Some(policy) = context.policy_table.get(context.task_class) else {
        record.selection_source = None;
        return Ok(SelectionOutput { code: 2, record });
    };
    let mut candidates = match preferences::normalise_candidates_ordered(
        raw_candidates,
        context.task_class,
        context.role,
        policy,
        context.aliases,
        context.efforts,
    ) {
        Ok(c) => c,
        Err(CandidateError::InvalidSet) => {
            record.status = "candidate_set_invalid";
            return Ok(SelectionOutput { code: 2, record });
        }
        Err(e) => return Err(e.into()),
    };
    let Some(state) = state.filter(|_| state_status != StateStatus::Invalid) else {
        record.status = "spread_state_invalid";
        return Ok(SelectionOutput { code: 2, record });
    };
    let admissible: Vec<_> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.admissible.then_some(i))
        .collect();
    let Some(minimum) = admissible
        .iter()
        .map(|&i| {
            state
                .assignments
                .get(&candidates[i].model_family)
                .copied()
                .unwrap_or(0)
        })
        .min()
    else {
        record.status = "no_admissible_candidate";
        record.candidates = Some(candidates);
        return Ok(SelectionOutput { code: 1, record });
    };
    let least_used: Vec<_> = admissible
        .iter()
        .copied()
        .filter(|&i| {
            state
                .assignments
                .get(&candidates[i].model_family)
                .copied()
                .unwrap_or(0)
                == minimum
        })
        .collect();
    let pref_value = preferences.to_value();
    let items = preferences::preference_items(&pref_value, context.task_class, context.role);
    let ranks = preferences::rank_maps(&items);
    let deprioritised = preferences::deprioritised(&pref_value);
    let Some(&chosen) = least_used
        .iter()
        .min_by_key(|&&i| preferences::preference_rank(&candidates[i], &ranks, &deprioritised))
    else {
        return Err(CandidateError::InvalidSet.into());
    };
    for &i in &admissible {
        candidates[i].selected = i == chosen;
        candidates[i].disposition = if i == chosen {
            "selected"
        } else {
            "passed_over"
        }
        .into();
    }
    let mut after = state.clone();
    let family = &candidates[chosen].model_family;
    after.assignments.insert(
        family.clone(),
        state
            .assignments
            .get(family)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(SelectionError::NumberOutOfRange)?,
    );
    let total = after
        .assignments
        .values()
        .try_fold(0u64, |sum, n| sum.checked_add(*n))
        .ok_or(SelectionError::NumberOutOfRange)?;
    after.selection_count = if state.selection_count.is_f64() {
        json!(
            state
                .selection_count
                .as_f64()
                .ok_or(SelectionError::NumberOutOfRange)?
                + 1.0
        )
    } else {
        json!(total)
    };
    let (applied, not_honoured, mut ignored) =
        preference_receipt(&items, &candidates, &candidates[chosen], &least_used);
    ignored.extend(preferences::configuration_ignored_ordered(
        preferences,
        context.task_class,
        context.role,
    ));
    record.status = "ok";
    record.chosen_candidate_id = Some(candidates[chosen].candidate_id.clone());
    record.chosen_route = Some(candidates[chosen].route.clone());
    record.candidates = Some(candidates);
    record.preference = Some(PreferenceReceipt {
        file_status: preferences_status,
        applied,
        not_honoured,
        ignored,
    });
    record.spreading = Some(SpreadingReceipt {
        policy: "fair-round-robin",
        state_status,
        state_before: state,
        state_after: after,
    });
    Ok(SelectionOutput { code: 0, record })
}

/// begin creates the parent and holds the per-state exclusive lock through
/// publication. A transaction must use the supervisor safe FS API for atomic
/// replacement; failed writes propagate and never produce a success receipt.
pub trait SelectionStore {
    fn begin(
        &self,
        state_path: &Path,
    ) -> Result<Box<dyn SelectionTransaction + '_>, SelectionError>;
}
pub trait SelectionTransaction {
    fn read(&mut self, path: &Path) -> Result<Option<String>, SelectionError>;
    fn write_atomic(&mut self, path: &Path, bytes: &[u8]) -> Result<(), SelectionError>;
}
pub struct SelectionPaths<'a> {
    pub candidates: &'a Path,
    pub preferences: &'a Path,
    pub state: &'a Path,
}
pub fn select(
    store: &impl SelectionStore,
    paths: &SelectionPaths<'_>,
    context: &SelectionContext<'_>,
) -> Result<SelectionOutput, SelectionError> {
    let mut transaction = store.begin(paths.state)?;
    if !context.policy_table.contains_key(context.task_class) {
        return select_documents(
            context,
            &OrderedValue::Null,
            &OrderedValue::Null,
            PreferenceStatus::Unavailable,
            None,
            StateStatus::Invalid,
        );
    }
    fn read_json(
        tx: &mut dyn SelectionTransaction,
        path: &Path,
    ) -> Result<Option<OrderedValue>, SelectionError> {
        match tx.read(path) {
            Ok(Some(raw)) => match crate::pending::from_json(&raw) {
                Ok(value) => Ok(Some(value)),
                Err(e @ provenant_records::DecodeError::NumberOutOfRange(_)) => Err(e.into()),
                Err(_) => Ok(None),
            },
            Ok(None) => Ok(None),
            Err(e @ SelectionError::Encoding(_)) => Err(e),
            Err(SelectionError::Io { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }
    let candidates = read_json(&mut *transaction, paths.candidates)?.unwrap_or(OrderedValue::Null);
    let (preferences, status) = match transaction.read(paths.preferences) {
        Ok(Some(raw)) => match crate::pending::from_json(&raw) {
            Ok(v @ OrderedValue::Object(_)) => (v, PreferenceStatus::Loaded),
            Ok(_) => (OrderedValue::Null, PreferenceStatus::Invalid),
            Err(e @ provenant_records::DecodeError::NumberOutOfRange(_)) => return Err(e.into()),
            Err(_) => (OrderedValue::Null, PreferenceStatus::Corrupt),
        },
        Ok(None) | Err(SelectionError::Io { .. }) => {
            (OrderedValue::Null, PreferenceStatus::Unavailable)
        }
        Err(e) => return Err(e),
    };
    let (state, state_status) = match transaction.read(paths.state) {
        Ok(raw) => load_spread_state(raw.as_deref())?,
        Err(SelectionError::Io { .. }) => (None, StateStatus::Invalid),
        Err(e) => return Err(e),
    };
    let output = select_documents(
        context,
        &candidates,
        &preferences,
        status,
        state,
        state_status,
    )?;
    if let Some(spreading) = &output.record.spreading {
        let raw = serde_json::to_value(&spreading.state_after)
            .map_err(provenant_records::DecodeError::from)?;
        let encoded = format!(
            "{}\n",
            crate::pending::json_dumps(&raw.into(), true, Some(2))
        );
        transaction.write_atomic(paths.state, encoded.as_bytes())?;
    }
    Ok(output)
}
#[cfg(test)]
mod tests;
