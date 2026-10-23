//! Pure selection policy for the weighted global model pools.
//!
//! Filesystem, provider compatibility loading, installed-adapter discovery,
//! quota snapshots, rotation persistence and random draws are narrow injected
//! seams. The policy itself stays synchronous and has no process or clock
//! effects.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::ordered_json::{OrderedObject, OrderedValue};
use crate::{
    casefold::fold,
    matching::{MatchError, ModelCatalog},
    pools::{self, PoolError},
};
use serde_json::{Number, Value};
use thiserror::Error;

const MAX_COUNCIL: u64 = 8;
const DEFAULT_WEIGHT: &str = "normal";
const PRIVATE_UNSAFE_TRAIT: &str = "trains-on-prompts";

#[derive(Clone, Debug, PartialEq)]
pub enum Weight {
    Integer(u64),
    Float(f64),
}

impl Weight {
    fn from_number(number: &Number) -> Option<Self> {
        if number.is_f64() {
            let value = number.as_f64()?;
            (value >= 0.0 && value.is_finite()).then_some(Self::Float(value))
        } else {
            number.as_u64().map(Self::Integer)
        }
    }

    fn is_zero(&self) -> bool {
        match self {
            Self::Integer(value) => *value == 0,
            Self::Float(value) => *value <= 0.0,
        }
    }

    fn compare(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Integer(left), Self::Integer(right)) => left.cmp(right),
            (Self::Float(left), Self::Float(right)) => left.total_cmp(right),
            (Self::Integer(integer), Self::Float(float)) => compare_u64_float(*integer, *float),
            (Self::Float(float), Self::Integer(integer)) => {
                compare_u64_float(*integer, *float).reverse()
            }
        }
    }
}

/// Compare an exact JSON integer to a finite non-negative binary float.
fn compare_u64_float(integer: u64, float: f64) -> Ordering {
    if float >= 18_446_744_073_709_551_616.0 {
        return Ordering::Less;
    }
    let floor = float.floor() as u64;
    match integer.cmp(&floor) {
        Ordering::Equal if float.fract() != 0.0 => Ordering::Less,
        ordering => ordering,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RotationNumber {
    Integer(i128),
    Float(f64),
}

impl RotationNumber {
    fn from_ordered(value: &OrderedValue) -> Result<Self, PoolSelectionError> {
        let number = match value {
            OrderedValue::Bool(value) => f64::from(u8::from(*value)),
            OrderedValue::Number(number) => number
                .as_f64()
                .ok_or(PoolSelectionError::NumberOutOfRange)?,
            OrderedValue::String(text) => {
                provenant_core::py::python_float(text).ok_or_else(|| {
                    PoolSelectionError::MalformedRotationState(
                        "cursor entries must be finite numbers".into(),
                    )
                })?
            }
            _ => {
                return Err(PoolSelectionError::MalformedRotationState(
                    "cursor entries must be numbers".into(),
                ));
            }
        };
        if !number.is_finite() {
            return Err(PoolSelectionError::NumberOutOfRange);
        }
        Ok(Self::Float(number))
    }

    fn to_ordered(&self) -> Result<OrderedValue, PoolSelectionError> {
        let number = match self {
            Self::Integer(value) if *value >= 0 => Number::from(
                u64::try_from(*value).map_err(|_| PoolSelectionError::NumberOutOfRange)?,
            ),
            Self::Integer(value) => Number::from(
                i64::try_from(*value).map_err(|_| PoolSelectionError::NumberOutOfRange)?,
            ),
            Self::Float(value) => Number::from_f64(*value)
                .filter(|number| number.as_f64().is_some_and(f64::is_finite))
                .ok_or(PoolSelectionError::NumberOutOfRange)?,
        };
        Ok(OrderedValue::Number(number))
    }

    fn checked_add(&mut self, weight: &Weight) -> Result<(), PoolSelectionError> {
        *self = match (&*self, weight) {
            (Self::Integer(value), Weight::Integer(weight)) => Self::Integer(
                value
                    .checked_add(i128::from(*weight))
                    .ok_or(PoolSelectionError::NumberOutOfRange)?,
            ),
            (Self::Float(value), Weight::Integer(weight)) => Self::Float(*value + *weight as f64),
            (Self::Integer(value), Weight::Float(weight)) => Self::Float(*value as f64 + *weight),
            (Self::Float(value), Weight::Float(weight)) => Self::Float(*value + *weight),
        };
        if matches!(self, Self::Float(value) if !value.is_finite()) {
            return Err(PoolSelectionError::NumberOutOfRange);
        }
        Ok(())
    }

    fn checked_sub(&mut self, other: &Self) -> Result<(), PoolSelectionError> {
        *self = match (&*self, other) {
            (Self::Integer(left), Self::Integer(right)) => Self::Integer(
                left.checked_sub(*right)
                    .ok_or(PoolSelectionError::NumberOutOfRange)?,
            ),
            (Self::Float(left), Self::Integer(right)) => Self::Float(*left - *right as f64),
            (Self::Integer(left), Self::Float(right)) => Self::Float(*left as f64 - *right),
            (Self::Float(left), Self::Float(right)) => Self::Float(*left - *right),
        };
        if matches!(self, Self::Float(value) if !value.is_finite()) {
            return Err(PoolSelectionError::NumberOutOfRange);
        }
        Ok(())
    }

    fn compare(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Integer(left), Self::Integer(right)) => left.cmp(right),
            (Self::Float(left), Self::Float(right)) => left.total_cmp(right),
            (Self::Integer(left), Self::Float(right)) => compare_i128_float(*left, *right),
            (Self::Float(left), Self::Integer(right)) => {
                compare_i128_float(*right, *left).reverse()
            }
        }
    }
}

fn compare_i128_float(integer: i128, float: f64) -> Ordering {
    const TWO_127: f64 = 170_141_183_460_469_231_731_687_303_715_884_105_728.0;
    if float >= TWO_127 {
        return Ordering::Less;
    }
    if float < -TWO_127 {
        return Ordering::Greater;
    }
    let floor = float.floor() as i128;
    match integer.cmp(&floor) {
        Ordering::Equal if float.fract() != 0.0 => Ordering::Less,
        ordering => ordering,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffortOrder {
    ordered: Vec<String>,
    ranks: BTreeMap<String, usize>,
}

impl EffortOrder {
    /// The input order is retained for Python-compatible error messages.
    pub fn new(entries: impl IntoIterator<Item = (String, usize)>) -> Self {
        let mut ordered = Vec::new();
        let mut ranks = BTreeMap::new();
        for (name, rank) in entries {
            if !ranks.contains_key(&name) {
                ordered.push(name.clone());
            }
            ranks.insert(name, rank);
        }
        Self { ordered, ranks }
    }

    pub fn names(&self) -> &[String] {
        &self.ordered
    }

    fn as_map(&self) -> &BTreeMap<String, usize> {
        &self.ranks
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelIdentity {
    pub adapter: String,
    pub model: String,
    pub registered: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolMode {
    Top,
    Rotate,
}

/// Adapter for the catalogue/matching owner. A future provider-specific host
/// can implement the same seam without adding branches to the selector.
pub trait PoolModelHost {
    fn resolve(&self, text: &str) -> Result<ModelIdentity, PoolSelectionError>;
    fn is_registered(&self, adapter: &str, model: &str) -> Result<bool, PoolSelectionError>;
    fn family(&self, model: &str) -> Result<String, PoolSelectionError>;
    fn compatibility_family(&self, model: &str) -> Result<Option<String>, PoolSelectionError>;
    fn traits(&self, adapter: &str, model: &str) -> Result<Vec<String>, PoolSelectionError>;
    fn suffix_effort(&self, adapter: &str, model: &str) -> Result<String, PoolSelectionError>;
    fn default_mode(&self) -> PoolMode;
}

pub struct CatalogueModelHost {
    catalog: Value,
    ordered_catalog: OrderedValue,
    cached_models: BTreeMap<String, Vec<String>>,
    default_mode: PoolMode,
}

impl CatalogueModelHost {
    pub fn new(catalog: &OrderedValue, cached_models: BTreeMap<String, Vec<String>>) -> Self {
        Self::with_mode(catalog, cached_models, PoolMode::Top)
    }

    pub fn with_mode(
        catalog: &OrderedValue,
        cached_models: BTreeMap<String, Vec<String>>,
        default_mode: PoolMode,
    ) -> Self {
        Self {
            catalog: catalog.to_value(),
            ordered_catalog: catalog.clone(),
            cached_models,
            default_mode,
        }
    }

    fn resolver(&self) -> ModelCatalog<'_> {
        ModelCatalog {
            catalog: &self.catalog,
            cached: &self.cached_models,
        }
    }
}

impl PoolModelHost for CatalogueModelHost {
    fn resolve(&self, text: &str) -> Result<ModelIdentity, PoolSelectionError> {
        let (adapter, model, registered) = pools::resolve_model(&self.resolver(), text)?;
        Ok(ModelIdentity {
            adapter,
            model,
            registered,
        })
    }

    fn is_registered(&self, adapter: &str, model: &str) -> Result<bool, PoolSelectionError> {
        Ok(self
            .resolver()
            .registered_match(adapter, model)?
            .0
            .is_some())
    }

    fn family(&self, model: &str) -> Result<String, PoolSelectionError> {
        Ok(pools::spread_family(model, &self.catalog)?)
    }

    fn compatibility_family(&self, model: &str) -> Result<Option<String>, PoolSelectionError> {
        Ok(crate::catalog::infer_family(model, &self.catalog)?)
    }

    fn traits(&self, adapter: &str, model: &str) -> Result<Vec<String>, PoolSelectionError> {
        Ok(pools::model_traits(&self.resolver(), adapter, model)?)
    }

    fn suffix_effort(&self, adapter: &str, model: &str) -> Result<String, PoolSelectionError> {
        Ok(self
            .resolver()
            .suffix_effort_ordered(adapter, model, &self.ordered_catalog)?)
    }

    fn default_mode(&self) -> PoolMode {
        self.default_mode
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompatibilityPolicy {
    pub enabled: bool,
    pub disabled_reason: String,
    pub allowed_families: Vec<String>,
    pub allowed_model_patterns: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompatibilityLoad {
    pub policy: Option<CompatibilityPolicy>,
    /// An empty status means loading succeeded or this adapter has no record.
    pub status: String,
}

pub trait InstalledAdapterSource {
    fn installed(&self, adapter: &str) -> bool;
}

pub trait CompatibilitySource {
    fn load(&mut self, adapter: &str) -> Result<CompatibilityLoad, String>;
}

pub trait CooldownSource {
    fn cooling_until(&self, adapter: &str, model: &str) -> Result<Option<String>, String>;
}

pub trait QuotaStateSource {
    fn states(&self) -> Result<OrderedObject, String>;
}

/// Called under one store-owned bounded transaction. The callback contains
/// the smooth weighted round-robin policy and returns the selected entry index.
pub trait RotationStateStore {
    /// Invoke the update under one bounded, exclusive transaction. Pass the
    /// parsed JSON state, or `Null` when the file is missing, unreadable, or
    /// invalid JSON. Persist the mutated document atomically on success using
    /// the source format (two-space indentation, sorted keys, trailing newline).
    fn update_state(
        &mut self,
        update: &mut dyn FnMut(&mut OrderedValue) -> Result<usize, PoolSelectionError>,
    ) -> Result<usize, String>;
}

/// The host owns the random source and mirrors Python `random.choices` odds.
pub trait WeightedDraw {
    fn choose(&mut self, weights: &[Weight]) -> Result<usize, String>;
}

#[derive(Debug, Error)]
pub enum PoolSelectionError {
    #[error(transparent)]
    Pool(#[from] PoolError),
    #[error(transparent)]
    Matching(#[from] MatchError),
    #[error(transparent)]
    Catalogue(#[from] crate::catalog::CatalogError),
    #[error("availability source failed: {0}")]
    Availability(String),
    #[error("rotation state store failed: {0}")]
    Rotation(String),
    #[error("random draw failed: {0}")]
    Random(String),
    #[error("malformed quota state for adapter {0}")]
    MalformedQuota(String),
    #[error("malformed rotation state: {0}")]
    MalformedRotationState(String),
    #[error("cannot use '{kind}' as a dict key (unhashable type: '{kind}')")]
    UnhashableEffort { kind: &'static str },
    #[error("numeric value is outside the supported exact range")]
    NumberOutOfRange,
    #[error("random source returned index {index} for {count} candidates")]
    DrawIndex { index: usize, count: usize },
    #[error("rotation store returned index {index} for {count} candidates")]
    RotationIndex { index: usize, count: usize },
}

pub struct Availability<'a> {
    catalog: &'a OrderedValue,
    installed: &'a dyn InstalledAdapterSource,
    compatibility: &'a mut dyn CompatibilitySource,
    cooldowns: &'a dyn CooldownSource,
    quota: &'a dyn QuotaStateSource,
    loaded_compatibility: BTreeMap<String, CompatibilityLoad>,
    quota_states: Option<OrderedObject>,
}

impl<'a> Availability<'a> {
    pub fn new(
        catalog: &'a OrderedValue,
        installed: &'a dyn InstalledAdapterSource,
        compatibility: &'a mut dyn CompatibilitySource,
        cooldowns: &'a dyn CooldownSource,
        quota: &'a dyn QuotaStateSource,
    ) -> Self {
        Self {
            catalog,
            installed,
            compatibility,
            cooldowns,
            quota,
            loaded_compatibility: BTreeMap::new(),
            quota_states: None,
        }
    }

    fn compatibility(&mut self, adapter: &str) -> Result<&CompatibilityLoad, PoolSelectionError> {
        if !self.loaded_compatibility.contains_key(adapter) {
            let record = self
                .compatibility
                .load(adapter)
                .map_err(PoolSelectionError::Availability)?;
            self.loaded_compatibility.insert(adapter.to_owned(), record);
        }
        self.loaded_compatibility
            .get(adapter)
            .ok_or_else(|| PoolSelectionError::Availability("compatibility cache missing".into()))
    }

    fn disabled_reason(&mut self, adapter: &str) -> Result<String, PoolSelectionError> {
        let loaded = self.compatibility(adapter)?;
        if !loaded.status.is_empty() {
            return Ok(loaded.status.clone());
        }
        Ok(loaded
            .policy
            .as_ref()
            .filter(|policy| !policy.enabled)
            .map(|_| "disabled".to_owned())
            .unwrap_or_default())
    }

    fn incompatible_reason(
        &mut self,
        adapter: &str,
        model: &str,
        host: &dyn PoolModelHost,
    ) -> Result<String, PoolSelectionError> {
        let policy = self.compatibility(adapter)?.policy.clone();
        let Some(policy) = policy else {
            return Ok(String::new());
        };
        let family = host
            .compatibility_family(model)?
            .filter(|family| !family.is_empty())
            .unwrap_or_else(|| "generic-open".to_owned());
        let lowered_model = model.to_lowercase();
        let pattern_match = policy.allowed_model_patterns.is_empty()
            || policy
                .allowed_model_patterns
                .iter()
                .any(|pattern| glob_match(&pattern.to_lowercase(), &lowered_model));
        let compatible_family = policy
            .allowed_families
            .iter()
            .any(|allowed| allowed == &family)
            || (!policy.allowed_model_patterns.is_empty()
                && policy
                    .allowed_families
                    .iter()
                    .any(|allowed| allowed == "open-weight")
                && pattern_match);
        if !compatible_family {
            return Ok(format!("{adapter} cannot run the {family} family"));
        }
        if !pattern_match {
            return Ok(format!("{adapter} does not allow {model}"));
        }
        Ok(String::new())
    }

    pub fn reason(
        &mut self,
        adapter: &str,
        model: &str,
        weight: &Weight,
        host: &dyn PoolModelHost,
    ) -> Result<String, PoolSelectionError> {
        if weight.is_zero() {
            return Ok("off".into());
        }
        if self
            .catalog
            .get("adapters")
            .and_then(OrderedValue::as_object)
            .is_none_or(|adapters| adapters.get(adapter).is_none())
        {
            return Ok("unknown adapter".into());
        }
        let disabled = self.disabled_reason(adapter)?;
        if !disabled.is_empty() {
            return Ok(format!("adapter {disabled}"));
        }
        let incompatible = self.incompatible_reason(adapter, model, host)?;
        if !incompatible.is_empty() {
            return Ok(incompatible);
        }
        if !self.installed.installed(adapter) {
            return Ok("not installed".into());
        }
        self.cooldowns
            .cooling_until(adapter, model)
            .map(|cooling| {
                cooling
                    .map(|until| format!("cooling until {until}"))
                    .unwrap_or_default()
            })
            .map_err(PoolSelectionError::Availability)
    }

    fn quota_states(&mut self) -> Result<&OrderedObject, PoolSelectionError> {
        if self.quota_states.is_none() {
            self.quota_states = Some(
                self.quota
                    .states()
                    .map_err(PoolSelectionError::Availability)?,
            );
        }
        self.quota_states
            .as_ref()
            .ok_or_else(|| PoolSelectionError::Availability("quota cache missing".to_owned()))
    }

    pub fn last_resort(&mut self, adapter: &str) -> Result<bool, PoolSelectionError> {
        Ok(self
            .quota_states()?
            .get(adapter)
            .map(|record| {
                let object = record
                    .as_object()
                    .ok_or_else(|| PoolSelectionError::MalformedQuota(adapter.to_owned()))?;
                Ok::<bool, PoolSelectionError>(
                    object
                        .get("last_resort")
                        .map(|value| provenant_core::py::python_truth(&value.to_value()))
                        .unwrap_or(false),
                )
            })
            .transpose()?
            .unwrap_or(false))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectionEntry {
    pub key: String,
    pub adapter: String,
    pub model: String,
    pub confidential: bool,
    pub weight: Weight,
    pub weight_label: OrderedValue,
    pub effort: Option<Vec<String>>,
    pub family: String,
    pub traits: Vec<String>,
}

pub fn pool(
    host: &dyn PoolModelHost,
    catalog: &OrderedValue,
    name: &str,
    effort_order: &EffortOrder,
) -> Result<Vec<SelectionEntry>, PoolSelectionError> {
    let Some(OrderedValue::Array(raw_entries)) =
        catalog.get("routes").and_then(|routes| routes.get(name))
    else {
        return Ok(Vec::new());
    };
    let mut combined: Vec<(String, OrderedObject)> = Vec::new();
    for raw in raw_entries {
        if !pools::entry_is_valid(&raw.to_value(), effort_order.as_map()) {
            continue;
        }
        if raw
            .get("confidential")
            .is_some_and(|value| as_bool(value).is_none())
        {
            return Err(PoolError::new(
                "confidential_invalid",
                &format!("{name} pool confidential must be a boolean"),
            )
            .into());
        }
        let model = raw
            .get("model")
            .and_then(OrderedValue::as_str)
            .ok_or_else(|| PoolError::new("models_invalid", "pool entry model must be a string"))?;
        let identity = host.resolve(model)?;
        let key = format!("{}/{}", identity.adapter, identity.model);
        let raw_object = raw
            .as_object()
            .ok_or_else(|| PoolError::new("models_invalid", "pool entry must be an object"))?;
        if let Some((_, previous)) = combined.iter_mut().find(|(existing, _)| existing == &key) {
            let mut merged = raw_object.clone();
            for (field, value) in &previous.0 {
                merged.insert(field.clone(), value.clone());
            }
            *previous = merged;
        } else {
            combined.push((key, raw_object.clone()));
        }
    }

    combined
        .into_iter()
        .map(|(key, raw)| {
            let (adapter, model) = key.split_once('/').ok_or_else(|| {
                PoolError::new("models_invalid", "canonical pool entry has no adapter")
            })?;
            let weight_label = raw
                .get("weight")
                .cloned()
                .unwrap_or_else(|| OrderedValue::String(DEFAULT_WEIGHT.into()));
            let weight_value = pools::weight_value(&weight_label.to_value()).ok_or_else(|| {
                PoolError::new("weight_invalid", "pool entry has an invalid weight")
            })?;
            let weight =
                Weight::from_number(&weight_value).ok_or(PoolSelectionError::NumberOutOfRange)?;
            let effort = match raw.get("effort") {
                Some(value) => Some(
                    pools::effort_band(&value.to_value(), effort_order.as_map()).ok_or_else(
                        || {
                            PoolError::new(
                                "effort_invalid",
                                "pool entry has an invalid effort band",
                            )
                        },
                    )?,
                ),
                None => None,
            };
            let adapter = adapter.to_owned();
            let model = model.to_owned();
            let family = host.family(&model)?;
            let traits = host.traits(&adapter, &model)?;
            Ok(SelectionEntry {
                key,
                adapter,
                model,
                confidential: raw.get("confidential").and_then(as_bool).unwrap_or(true),
                weight,
                weight_label,
                effort,
                family,
                traits,
            })
        })
        .collect()
}

fn route_name(catalog: &OrderedValue, name: &str) -> Result<String, PoolError> {
    let routes = catalog.get("routes").and_then(OrderedValue::as_object);
    let key = fold(provenant_core::py::python_strip(name));
    if routes.is_some_and(|routes| routes.get(&key).is_some()) {
        return Ok(key);
    }
    if let Some(message) = pools::renamed_message("route", provenant_core::py::python_strip(name)) {
        return Err(PoolError::new("route_renamed", &message));
    }
    if let Some(target) = catalog
        .get("route_synonyms")
        .and_then(|synonyms| synonyms.get(&key))
        .and_then(OrderedValue::as_str)
    {
        if routes.is_some_and(|routes| routes.get(target).is_some()) {
            return Ok(target.to_owned());
        }
    }
    let mut names = routes
        .map(|routes| {
            routes
                .0
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    names.sort();
    Err(PoolError::new(
        "route_invalid",
        &format!("Pass route {}.", names.join(", ")),
    ))
}

fn ordered_keys(value: Option<&OrderedValue>) -> Vec<String> {
    value
        .and_then(OrderedValue::as_object)
        .map(|object| object.0.iter().map(|(key, _)| key.clone()).collect())
        .unwrap_or_default()
}

fn push_warning(warnings: &mut Vec<String>, value: String) {
    warnings.push(value);
}

fn py_truth(value: Option<&OrderedValue>) -> bool {
    value.is_some_and(|value| provenant_core::py::python_truth(&value.to_value()))
}

fn as_bool(value: &OrderedValue) -> Option<bool> {
    if let OrderedValue::Bool(value) = value {
        Some(*value)
    } else {
        None
    }
}

fn py_string(value: &OrderedValue) -> String {
    provenant_core::py::python_str(&value.to_value())
}

fn json_string(value: impl Into<String>) -> OrderedValue {
    OrderedValue::String(value.into())
}

fn json_bool(value: bool) -> OrderedValue {
    OrderedValue::Bool(value)
}

fn selection_error(code: &str, fix: &str) -> PoolSelectionError {
    PoolError::new(code, fix).into()
}

fn requested_council(request: &OrderedValue) -> Result<Option<usize>, PoolSelectionError> {
    let Some(value) = request.get("council") else {
        return Ok(None);
    };
    if matches!(value, OrderedValue::Null) {
        return Ok(None);
    }
    let count = match value {
        OrderedValue::Number(number) if !number.is_f64() => {
            number.as_u64().filter(|number| *number <= MAX_COUNCIL)
        }
        _ => None,
    };
    let Some(count) = count.filter(|count| (1..=MAX_COUNCIL).contains(count)) else {
        return Err(selection_error(
            "council_invalid",
            &format!("Pass council as an integer from 1 to {MAX_COUNCIL}."),
        ));
    };
    usize::try_from(count)
        .map(Some)
        .map_err(|_| PoolSelectionError::NumberOutOfRange)
}

fn request_bool(
    request: &OrderedValue,
    field: &str,
    default: bool,
    error_message: &str,
) -> Result<bool, PoolSelectionError> {
    match request.get(field) {
        Some(OrderedValue::Bool(value)) => Ok(*value),
        Some(_) => Err(selection_error(
            if field == "confidential" {
                "confidential_invalid"
            } else {
                "allow_free_invalid"
            },
            error_message,
        )),
        None => Ok(default),
    }
}

fn effort_for(
    entry: &SelectionEntry,
    requested: Option<&OrderedValue>,
    effort_order: &EffortOrder,
    label: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<OrderedValue>, PoolSelectionError> {
    let Some(band) = &entry.effort else {
        return Ok(requested.cloned());
    };
    let Some(requested) = requested else {
        return Ok(band.first().cloned().map(OrderedValue::String));
    };
    let requested = match requested {
        OrderedValue::String(requested) => Some(requested.as_str()),
        OrderedValue::Array(_) => {
            return Err(PoolSelectionError::UnhashableEffort { kind: "list" });
        }
        OrderedValue::Object(_) => {
            return Err(PoolSelectionError::UnhashableEffort { kind: "dict" });
        }
        _ => None,
    };
    let Some(requested) = requested else {
        return Ok(band.first().cloned().map(OrderedValue::String));
    };
    let Some(requested_rank) = effort_order.ranks.get(requested) else {
        return Ok(band.first().cloned().map(OrderedValue::String));
    };
    let first = band.first().and_then(|name| effort_order.ranks.get(name));
    let last = band.last().and_then(|name| effort_order.ranks.get(name));
    if first.is_some_and(|first| requested_rank < first) {
        let Some(effort) = band.first().cloned() else {
            return Ok(None);
        };
        push_warning(
            warnings,
            format!(
                "effort {requested} raised to {effort} by the {label} band for {}",
                entry.key
            ),
        );
        Ok(Some(OrderedValue::String(effort)))
    } else if last.is_some_and(|last| requested_rank > last) {
        let Some(effort) = band.last().cloned() else {
            return Ok(None);
        };
        push_warning(
            warnings,
            format!(
                "effort {requested} lowered to {effort} by the {label} band for {}",
                entry.key
            ),
        );
        Ok(Some(OrderedValue::String(effort)))
    } else {
        Ok(Some(OrderedValue::String(requested.to_owned())))
    }
}

fn choose_council(
    entries: &[SelectionEntry],
    count: usize,
    draw: &mut dyn WeightedDraw,
) -> Result<Vec<SelectionEntry>, PoolSelectionError> {
    let mut picks = Vec::with_capacity(count);
    while picks.len() < count {
        let mut remaining = entries.to_vec();
        let mut families = Vec::<String>::new();
        while !remaining.is_empty() && picks.len() < count {
            let mut fresh: Vec<usize> = if families.is_empty() {
                (0..remaining.len()).collect()
            } else {
                remaining
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| {
                        !families.contains(&entry.family)
                            && !entry.traits.iter().any(|item| item == PRIVATE_UNSAFE_TRAIT)
                    })
                    .map(|(index, _)| index)
                    .collect()
            };
            if fresh.is_empty() {
                fresh = (0..remaining.len()).collect();
            }
            let weights: Vec<_> = fresh
                .iter()
                .map(|index| remaining[*index].weight.clone())
                .collect();
            let selected = draw.choose(&weights).map_err(PoolSelectionError::Random)?;
            if selected >= fresh.len() {
                return Err(PoolSelectionError::DrawIndex {
                    index: selected,
                    count: fresh.len(),
                });
            }
            let entry_index = fresh[selected];
            let entry = remaining.remove(entry_index);
            if !families.contains(&entry.family) {
                families.push(entry.family.clone());
            }
            picks.push(entry);
        }
    }
    Ok(picks)
}

fn object_field_mut<'a>(
    object: &'a mut OrderedObject,
    name: &str,
) -> Result<&'a mut OrderedObject, PoolSelectionError> {
    if object.get(name).is_none() {
        object.insert(
            name.to_owned(),
            OrderedValue::Object(OrderedObject::default()),
        );
    }
    match object.0.iter_mut().find(|(key, _)| key == name) {
        Some((_, OrderedValue::Object(child))) => Ok(child),
        Some(_) => Err(PoolSelectionError::MalformedRotationState(format!(
            "{name} must be an object"
        ))),
        None => Err(PoolSelectionError::MalformedRotationState(format!(
            "{name} is missing"
        ))),
    }
}

fn rotate_document(
    document: &mut OrderedValue,
    entries: &[SelectionEntry],
    project: &str,
    route: &str,
) -> Result<usize, PoolSelectionError> {
    let schema_version = document.get("schema_version");
    let valid_schema = schema_version.is_some_and(|version| {
        provenant_core::py::python_json_eq(&version.to_value(), &Value::Number(Number::from(1)))
    });
    if !valid_schema {
        let mut root = OrderedObject::default();
        root.insert(
            "schema_version".into(),
            OrderedValue::Number(Number::from(1)),
        );
        root.insert(
            "projects".into(),
            OrderedValue::Object(OrderedObject::default()),
        );
        *document = OrderedValue::Object(root);
    }
    let OrderedValue::Object(root) = document else {
        return Err(PoolSelectionError::MalformedRotationState(
            "state must be an object".into(),
        ));
    };
    let projects = object_field_mut(root, "projects")?;
    let project_state = object_field_mut(projects, project)?;
    let route_state = object_field_mut(project_state, route)?;

    let mut total = RotationNumber::Integer(0);
    let mut scores = Vec::with_capacity(entries.len());
    for entry in entries {
        total.checked_add(&entry.weight)?;
        let mut score = route_state
            .get(&entry.key)
            .map(RotationNumber::from_ordered)
            .transpose()?
            .unwrap_or(RotationNumber::Float(0.0));
        score.checked_add(&entry.weight)?;
        route_state.insert(entry.key.clone(), score.to_ordered()?);
        scores.push(score);
    }
    let mut chosen = 0;
    for index in 1..scores.len() {
        if scores[index].compare(&scores[chosen]) == Ordering::Greater {
            chosen = index;
        }
    }
    let mut chosen_score = scores
        .get(chosen)
        .cloned()
        .ok_or(PoolSelectionError::NumberOutOfRange)?;
    chosen_score.checked_sub(&total)?;
    route_state.insert(entries[chosen].key.clone(), chosen_score.to_ordered()?);
    Ok(chosen)
}

fn rotate(
    entries: &[SelectionEntry],
    project: &str,
    route: &str,
    store: &mut dyn RotationStateStore,
) -> Result<SelectionEntry, PoolSelectionError> {
    let mut update =
        |document: &mut OrderedValue| rotate_document(document, entries, project, route);
    let index = store
        .update_state(&mut update)
        .map_err(PoolSelectionError::Rotation)?;
    entries
        .get(index)
        .cloned()
        .ok_or(PoolSelectionError::RotationIndex {
            index,
            count: entries.len(),
        })
}

fn spawn(native: &str, entries: &[SelectionEntry], count: usize) -> String {
    let mut names = Vec::<String>::new();
    for entry in entries {
        if !names.contains(&entry.model) {
            names.push(entry.model.clone());
        }
    }
    format!(
        "spawn {count} native {native} member{} ({})",
        if count == 1 { "" } else { "s" },
        names.join(", ")
    )
}

pub fn pick(
    host: &dyn PoolModelHost,
    catalog: &OrderedValue,
    request: &OrderedValue,
    availability: &mut Availability<'_>,
    rotation: &mut dyn RotationStateStore,
    draw: &mut dyn WeightedDraw,
    effort_order: &EffortOrder,
) -> Result<OrderedValue, PoolSelectionError> {
    let request_object = request
        .as_object()
        .ok_or_else(|| selection_error("invalid_input", "Pass each pick request as an object."))?;
    let council = requested_council(request)?;
    let mut native = request
        .get("native")
        .and_then(OrderedValue::as_str)
        .filter(|native| !native.is_empty())
        .map(str::to_owned);
    let models = request
        .get("models")
        .filter(|models| !matches!(models, OrderedValue::Null));
    if request_object
        .0
        .iter()
        .any(|(key, _)| key == "confidential")
        && request.get("confidential").and_then(as_bool).is_none()
    {
        return Err(selection_error(
            "confidential_invalid",
            "confidential must be a boolean",
        ));
    }
    let mut confidential = request
        .get("confidential")
        .and_then(as_bool)
        .unwrap_or(false);
    let mut allow_free = request_bool(request, "allow_free", true, "allow_free must be a boolean")?;
    let requested_effort = request
        .get("effort")
        .filter(|value| provenant_core::py::python_truth(&value.to_value()));
    if requested_effort
        .and_then(OrderedValue::as_str)
        .is_some_and(|effort| fold(provenant_core::py::python_strip(effort)) == "max")
    {
        return Err(selection_error(
            "effort_invalid",
            "effort 'max' was retired; fix: use xhigh",
        ));
    }
    let mut warnings = Vec::new();
    let adapters = catalog.get("adapters");

    if let Some(council) = council {
        if py_truth(request.get("rotate")) {
            push_warning(
                &mut warnings,
                "rotate ignored: council already spreads picks".into(),
            );
        }
        let _ = council;
    }

    let (entries, label, mode) = if let Some(models) = models {
        let items = match models {
            OrderedValue::Array(items)
                if (1..=MAX_COUNCIL as usize).contains(&items.len())
                    && items.iter().all(|item| {
                        item.as_str()
                            .is_some_and(|text| !provenant_core::py::python_strip(text).is_empty())
                    }) =>
            {
                items
            }
            _ => {
                return Err(selection_error(
                    "models_invalid",
                    &format!("Pass models as 1-{MAX_COUNCIL} adapter/model[@effort] strings."),
                ));
            }
        };
        if py_truth(request.get("route")) {
            push_warning(
                &mut warnings,
                format!(
                    "route {} ignored: models bypasses routes",
                    request.get("route").map(py_string).unwrap_or_default()
                ),
            );
        }
        if py_truth(request.get("adapter")) {
            push_warning(
                &mut warnings,
                format!(
                    "adapter {} ignored: each models entry names its adapter",
                    request.get("adapter").map(py_string).unwrap_or_default()
                ),
            );
        }
        let mut entries = Vec::with_capacity(items.len());
        for item in items {
            let item = item.as_str().ok_or_else(|| {
                selection_error("models_invalid", "model entries must be strings")
            })?;
            let text = provenant_core::py::python_strip(item);
            let (model_text, item_effort) = match text.split_once('@') {
                Some((head, effort)) => (head, effort),
                None => (text, ""),
            };
            let (head, has_separator) = model_text
                .split_once('/')
                .map(|(head, _)| (head, true))
                .unwrap_or((model_text, false));
            let identity = host.resolve(model_text)?;
            let known_adapter = adapters
                .and_then(OrderedValue::as_object)
                .is_some_and(|items| items.get(head).is_some());
            if has_separator
                && !known_adapter
                && !identity.registered
                && !pools::known_provider_path(model_text)
            {
                let mut adapter_names = ordered_keys(adapters);
                adapter_names.sort();
                return Err(selection_error(
                    "models_invalid",
                    &format!(
                        "{item} names no known adapter; fix: prefix it with one of {}, e.g. codex/gpt-6-luna@low.",
                        adapter_names.join(", ")
                    ),
                ));
            }
            if !item_effort.is_empty() && !effort_order.ranks.contains_key(item_effort) {
                return Err(selection_error(
                    "effort_invalid",
                    &format!(
                        "Pass a supported effort after @ in {item}: {}.",
                        effort_order.ordered.join(", ")
                    ),
                ));
            }
            if !identity.registered {
                push_warning(
                    &mut warnings,
                    format!(
                        "{model_text} is not in the catalogue; passing it to {} as given",
                        identity.adapter
                    ),
                );
            }
            let suffix_model = if has_separator && known_adapter {
                model_text
                    .split_once('/')
                    .map(|(_, model)| model)
                    .unwrap_or(model_text)
            } else {
                model_text
            };
            let effort = if !item_effort.is_empty() {
                item_effort.to_owned()
            } else if adapters
                .and_then(OrderedValue::as_object)
                .is_some_and(|items| items.get(&identity.adapter).is_some())
            {
                host.suffix_effort(&identity.adapter, suffix_model)?
            } else {
                String::new()
            };
            entries.push(SelectionEntry {
                key: format!("{}/{}", identity.adapter, identity.model),
                family: host.family(&identity.model)?,
                traits: host.traits(&identity.adapter, &identity.model)?,
                adapter: identity.adapter,
                model: identity.model,
                confidential: true,
                weight: Weight::Float(1.0),
                weight_label: OrderedValue::Null,
                effort: (!effort.is_empty()).then_some(vec![effort]),
            });
        }
        (entries, "ad-hoc".to_owned(), "council".to_owned())
    } else {
        if !py_truth(request.get("route")) {
            let route_names = catalog
                .get("routes")
                .and_then(OrderedValue::as_object)
                .map(|routes| {
                    routes
                        .0
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            return Err(selection_error(
                "route_required",
                &format!("Pass route ({route_names}) or models with rotate or council."),
            ));
        }
        let route_value = request.get("route").unwrap_or(&OrderedValue::Null);
        let label = route_name(catalog, &py_string(route_value))?;
        let defaults = catalog
            .get("route_defaults")
            .and_then(|defaults| defaults.get(&label))
            .filter(|defaults| defaults.as_object().is_some())
            .cloned()
            .unwrap_or(OrderedValue::Null);
        if defaults
            .as_object()
            .is_some_and(|defaults| defaults.get("confidential").is_some())
            && defaults.get("confidential").and_then(as_bool).is_none()
        {
            return Err(selection_error(
                "confidential_invalid",
                &format!("{label} default confidential must be a boolean"),
            ));
        }
        if defaults
            .as_object()
            .is_some_and(|defaults| defaults.get("allow_free").is_some())
            && defaults.get("allow_free").and_then(as_bool).is_none()
        {
            return Err(selection_error(
                "allow_free_invalid",
                &format!("{label} default allow_free must be a boolean"),
            ));
        }
        let default_confidential = defaults
            .get("confidential")
            .and_then(as_bool)
            .unwrap_or(label == "utility");
        confidential = request_bool(
            request,
            "confidential",
            default_confidential,
            "confidential must be a boolean",
        )?;
        allow_free = request_bool(
            request,
            "allow_free",
            defaults
                .get("allow_free")
                .and_then(as_bool)
                .unwrap_or(label != "utility"),
            "allow_free must be a boolean",
        )?;
        let mut entries = pool(host, catalog, &label, effort_order)?;
        let rotate_value = request
            .get("rotate")
            .filter(|value| !matches!(value, OrderedValue::Null));
        let rotate = rotate_value
            .map(|value| provenant_core::py::python_truth(&value.to_value()))
            .unwrap_or_else(|| host.default_mode() == PoolMode::Rotate);
        let mode = if council.is_some() {
            "council"
        } else if rotate {
            "rotate"
        } else {
            "top"
        };
        if py_truth(request.get("adapter")) {
            let adapter = request.get("adapter").map(py_string).unwrap_or_default();
            let adapter_value = request.get("adapter").and_then(OrderedValue::as_str);
            entries.retain(|entry| adapter_value == Some(entry.adapter.as_str()));
            if entries.is_empty() {
                let containing: Vec<_> = ordered_keys(catalog.get("routes"))
                    .into_iter()
                    .map(|candidate| {
                        let entries = pool(host, catalog, &candidate, effort_order)?;
                        Ok::<_, PoolSelectionError>(
                            entries
                                .iter()
                                .any(|entry| entry.adapter == adapter)
                                .then_some(candidate),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect();
                let containing_text = if containing.is_empty() {
                    "none".to_owned()
                } else {
                    containing.join(", ")
                };
                return Err(selection_error(
                    "route_adapter_empty",
                    &format!(
                        "{label} has no {adapter} entry; pools containing {adapter}: {}; fix: pick one of those routes, omit adapter, or pass model: {adapter}/<model-id> instead of route.",
                        containing_text
                    ),
                ));
            }
            // Explicitly naming the native adapter sends it through Fabric.
            native = None;
        }
        (entries, label, mode.to_owned())
    };

    let mut usable = Vec::new();
    for entry in entries {
        let mut why = availability.reason(&entry.adapter, &entry.model, &entry.weight, host)?;
        if why.is_empty()
            && confidential
            && entry
                .traits
                .iter()
                .any(|trait_| trait_ == PRIVATE_UNSAFE_TRAIT)
        {
            why = "trains on prompts; task is confidential".into();
        }
        if why.is_empty() && confidential && !entry.confidential {
            why = "public-only pool member; task is confidential".into();
        }
        if why.is_empty()
            && confidential
            && !allow_free
            && entry.traits.iter().any(|trait_| trait_ == "free")
        {
            why = "free tier; confidential route requires a paid model".into();
        }
        if !why.is_empty() {
            if why != "off" {
                push_warning(&mut warnings, format!("skipped {}: {why}", entry.key));
            }
            continue;
        }
        usable.push(entry);
    }

    if models.is_none() && !py_truth(request.get("adapter")) {
        let mut allowance = Vec::new();
        let mut spare = Vec::new();
        for entry in usable {
            if availability.last_resort(&entry.adapter)? {
                spare.push(entry);
            } else {
                allowance.push(entry);
            }
        }
        if !allowance.is_empty() {
            for entry in spare {
                push_warning(
                    &mut warnings,
                    format!(
                        "skipped {}: drawing on credits while others have allowance",
                        entry.key
                    ),
                );
            }
            usable = allowance;
        } else {
            for entry in &spare {
                push_warning(
                    &mut warnings,
                    format!(
                        "{} draws on credits: nothing with allowance is available",
                        entry.key
                    ),
                );
            }
            usable = spare;
        }
    }

    if usable.is_empty() {
        return Err(selection_error(
            "route_unavailable",
            &format!(
                "No available model in {label}{}; fix: pass another route or models; `provenant routes` shows what is available.",
                if warnings.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", warnings.join("; "))
                }
            ),
        ));
    }

    if native.as_deref().is_some_and(|_| mode != "council") && models.is_none() {
        let native_name = native.as_deref().unwrap_or_default();
        let own: Vec<_> = usable
            .iter()
            .filter(|entry| entry.adapter == native_name)
            .cloned()
            .collect();
        usable.retain(|entry| entry.adapter != native_name);
        for entry in &own {
            push_warning(
                &mut warnings,
                format!("{} skipped: use a native subagent", entry.key),
            );
        }
        if usable.is_empty() {
            let mut top = own
                .first()
                .ok_or_else(|| selection_error("route_unavailable", "no usable pool members"))?;
            for entry in own.iter().skip(1) {
                if entry.weight.compare(&top.weight) == Ordering::Greater {
                    top = entry;
                }
            }
            let top = top.clone();
            return Err(selection_error(
                "route_native_only",
                &format!(
                    "{}; nothing else in {label} is available; pass adapter {native_name} to run it through Fabric anyway.",
                    spawn(native_name, &[top], 1)
                ),
            ));
        }
    }

    let chosen = if mode == "top" {
        let mut top = 0usize;
        for index in 1..usable.len() {
            if usable[index].weight.compare(&usable[top].weight) == Ordering::Greater {
                top = index;
            }
        }
        vec![usable[top].clone()]
    } else if mode == "rotate" {
        let project = request
            .get("project")
            .filter(|value| provenant_core::py::python_truth(&value.to_value()))
            .map(py_string)
            .unwrap_or_default();
        vec![rotate(&usable, &project, &label, rotation)?]
    } else if models.is_some() {
        usable.clone()
    } else {
        let count = council.unwrap_or(1);
        let mut chosen = choose_council(&usable, count, draw)?;
        if count > usable.len() {
            push_warning(
                &mut warnings,
                format!(
                    "council {count} exceeds {} available models; some repeat",
                    usable.len()
                ),
            );
        }
        if let Some(native) = native.as_deref() {
            let own: Vec<_> = chosen
                .iter()
                .filter(|entry| entry.adapter == native)
                .cloned()
                .collect();
            chosen.retain(|entry| entry.adapter != native);
            if !own.is_empty() && chosen.is_empty() {
                return Err(selection_error(
                    "route_native_only",
                    &format!(
                        "{}; Fabric has no other member to run; pass adapter {native} to run them anyway.",
                        spawn(native, &own, own.len())
                    ),
                ));
            }
            if !own.is_empty() {
                push_warning(
                    &mut warnings,
                    format!(
                        "{}; Fabric runs {} of {count}",
                        spawn(native, &own, own.len()),
                        chosen.len()
                    ),
                );
            }
        }
        chosen
    };

    let mut picks = Vec::new();
    for (index, entry) in chosen.iter().enumerate() {
        if entry
            .traits
            .iter()
            .any(|trait_| trait_ == PRIVATE_UNSAFE_TRAIT)
        {
            push_warning(
                &mut warnings,
                format!(
                    "{} is a free tier that may train on prompts; pass confidential: true to skip it",
                    entry.key
                ),
            );
        }
        let reason = if mode == "council" {
            format!("{label} {mode} {}/{}", index + 1, chosen.len())
        } else {
            format!("{label} {mode}")
        };
        let effort = effort_for(entry, requested_effort, effort_order, &label, &mut warnings)?;
        let mut picked = OrderedObject::default();
        picked.insert("adapter".into(), json_string(entry.adapter.clone()));
        picked.insert("model".into(), json_string(entry.model.clone()));
        picked.insert("family".into(), json_string(entry.family.clone()));
        if let Some(effort) =
            effort.filter(|effort| provenant_core::py::python_truth(&effort.to_value()))
        {
            picked.insert("effort".into(), effort);
        }
        picked.insert("reason".into(), json_string(reason));
        picks.push(OrderedValue::Object(picked));
    }
    let mut result = OrderedObject::default();
    result.insert("status".into(), json_string("ok"));
    result.insert("route".into(), json_string(label));
    result.insert("mode".into(), json_string(mode));
    result.insert("picks".into(), OrderedValue::Array(picks));
    result.insert("confidential".into(), json_bool(confidential));
    let mut unique_warnings = Vec::new();
    for warning in warnings {
        if !unique_warnings.contains(&warning) {
            unique_warnings.push(warning);
        }
    }
    result.insert(
        "warnings".into(),
        OrderedValue::Array(
            unique_warnings
                .into_iter()
                .map(OrderedValue::String)
                .collect(),
        ),
    );
    Ok(OrderedValue::Object(result))
}

pub fn pick_many(
    host: &dyn PoolModelHost,
    catalog: &OrderedValue,
    requests: &[OrderedValue],
    availability: &mut Availability<'_>,
    rotation: &mut dyn RotationStateStore,
    draw: &mut dyn WeightedDraw,
    effort_order: &EffortOrder,
) -> Result<Vec<OrderedValue>, PoolSelectionError> {
    let mut results = Vec::with_capacity(requests.len());
    for request in requests {
        if request.as_object().is_none() {
            results.push(rejected(
                "invalid_input",
                "Pass each pick request as an object.",
            ));
            continue;
        }
        match pick(
            host,
            catalog,
            request,
            availability,
            rotation,
            draw,
            effort_order,
        ) {
            Ok(result) => results.push(result),
            Err(PoolSelectionError::Pool(error)) => {
                results.push(rejected(&error.code, &error.fix));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(results)
}

fn rejected(error: &str, fix: &str) -> OrderedValue {
    let mut result = OrderedObject::default();
    result.insert("status".into(), json_string("rejected"));
    result.insert("error".into(), json_string(error));
    result.insert("fix".into(), json_string(fix));
    OrderedValue::Object(result)
}

pub fn describe(
    host: &dyn PoolModelHost,
    catalog: &OrderedValue,
    availability: &mut Availability<'_>,
    effort_order: &EffortOrder,
) -> Result<OrderedValue, PoolSelectionError> {
    let quota = availability.quota_states()?.clone();
    let mut routes = OrderedObject::default();
    for name in ordered_keys(catalog.get("routes")) {
        let entries = pool(host, catalog, &name, effort_order)?;
        let mut described = Vec::with_capacity(entries.len());
        for entry in entries {
            let mut record = OrderedObject::default();
            record.insert("model".into(), json_string(entry.key.clone()));
            record.insert("weight".into(), entry.weight_label.clone());
            record.insert("family".into(), json_string(entry.family.clone()));
            if let Some(effort) = entry.effort.as_ref().filter(|items| !items.is_empty()) {
                record.insert("effort".into(), json_string(effort.join("-")));
            }
            if !entry.traits.is_empty() {
                record.insert(
                    "traits".into(),
                    OrderedValue::Array(
                        entry
                            .traits
                            .iter()
                            .cloned()
                            .map(OrderedValue::String)
                            .collect(),
                    ),
                );
            }
            let reason = availability.reason(&entry.adapter, &entry.model, &entry.weight, host)?;
            record.insert(
                "availability".into(),
                json_string(if reason.is_empty() {
                    "available"
                } else {
                    &reason
                }),
            );
            if let Some(quota_record) = quota.get(&entry.adapter) {
                let Some(quota_object) = quota_record.as_object() else {
                    return Err(PoolSelectionError::MalformedQuota(entry.adapter));
                };
                let mut fields = OrderedObject::default();
                for key in ["account", "state", "summary", "observed_at"] {
                    let Some(value) = quota_object.get(key) else {
                        return Err(PoolSelectionError::MalformedQuota(entry.adapter.clone()));
                    };
                    fields.insert(key.into(), value.clone());
                }
                record.insert("quota".into(), OrderedValue::Object(fields));
            }
            described.push(OrderedValue::Object(record));
        }
        routes.insert(name, OrderedValue::Array(described));
    }
    let mut synonyms = OrderedObject::default();
    let mut aliases = catalog
        .get("route_synonyms")
        .and_then(OrderedValue::as_object)
        .map(|synonyms| synonyms.0.clone())
        .unwrap_or_default();
    aliases.sort_by(|left, right| left.0.cmp(&right.0));
    for (alias, target) in aliases {
        synonyms.insert(alias, target);
    }
    let mut result = OrderedObject::default();
    result.insert("schema".into(), json_string("fabric.routes.v1"));
    result.insert("routes".into(), OrderedValue::Object(routes));
    result.insert("synonyms".into(), OrderedValue::Object(synonyms));
    Ok(OrderedValue::Object(result))
}

fn glob_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let mut memo = BTreeMap::new();

    fn class_match(pattern: &[char], start: usize, value: char) -> Option<(usize, bool)> {
        let mut cursor = start + 1;
        let negated = pattern.get(cursor) == Some(&'!');
        if negated {
            cursor += 1;
        }
        let mut members = Vec::new();
        if pattern.get(cursor) == Some(&']') {
            members.push(']');
            cursor += 1;
        }
        let body_start = cursor;
        while cursor < pattern.len() && pattern[cursor] != ']' {
            cursor += 1;
        }
        if cursor == pattern.len() || (cursor == body_start && members.is_empty()) {
            return None;
        }
        members.extend_from_slice(&pattern[body_start..cursor]);

        let mut matched = false;
        let mut index = 0;
        while index < members.len() {
            if index + 2 < members.len() && members[index + 1] == '-' {
                matched |= members[index] <= value && value <= members[index + 2];
                index += 3;
            } else {
                matched |= members[index] == value;
                index += 1;
            }
        }
        Some((cursor + 1, matched != negated))
    }

    fn matches(
        pattern: &[char],
        value: &[char],
        pi: usize,
        vi: usize,
        memo: &mut BTreeMap<(usize, usize), bool>,
    ) -> bool {
        if let Some(result) = memo.get(&(pi, vi)) {
            return *result;
        }
        let result = if pi == pattern.len() {
            vi == value.len()
        } else if pattern[pi] == '*' {
            matches(pattern, value, pi + 1, vi, memo)
                || (vi < value.len() && matches(pattern, value, pi, vi + 1, memo))
        } else if pattern[pi] == '[' {
            if vi >= value.len() {
                false
            } else if let Some((next, is_match)) = class_match(pattern, pi, value[vi]) {
                is_match && matches(pattern, value, next, vi + 1, memo)
            } else {
                value[vi] == '[' && matches(pattern, value, pi + 1, vi + 1, memo)
            }
        } else if vi < value.len() && (pattern[pi] == '?' || pattern[pi] == value[vi]) {
            matches(pattern, value, pi + 1, vi + 1, memo)
        } else {
            false
        };
        memo.insert((pi, vi), result);
        result
    }
    matches(&pattern, &value, 0, 0, &mut memo)
}

#[cfg(test)]
#[path = "pool_selection/tests.rs"]
mod tests;
