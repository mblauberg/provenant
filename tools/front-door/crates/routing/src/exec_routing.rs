//! Fallback policy wire types and execution-facing route request contracts.

use crate::{
    ordered_json::{OrderedObject, OrderedValue},
    pending::python_strip,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

/// Adapter-specific fallback behavior is a table so routing policy does not grow
/// a central adapter-name match as providers are added.
pub trait AdapterRoutingPolicy {
    fn adapter(&self) -> &'static str;
    fn add_opencode_candidates(&self) -> bool;
}

#[derive(Clone, Copy)]
struct BuiltinAdapterPolicy {
    adapter: &'static str,
    add_opencode_candidates: bool,
}

impl AdapterRoutingPolicy for BuiltinAdapterPolicy {
    fn adapter(&self) -> &'static str {
        self.adapter
    }

    fn add_opencode_candidates(&self) -> bool {
        self.add_opencode_candidates
    }
}

static ADAPTER_POLICIES: [BuiltinAdapterPolicy; 7] = [
    BuiltinAdapterPolicy {
        adapter: "claude",
        add_opencode_candidates: true,
    },
    BuiltinAdapterPolicy {
        adapter: "codex",
        add_opencode_candidates: true,
    },
    BuiltinAdapterPolicy {
        adapter: "opencode",
        add_opencode_candidates: false,
    },
    BuiltinAdapterPolicy {
        adapter: "cursor",
        add_opencode_candidates: true,
    },
    BuiltinAdapterPolicy {
        adapter: "agy",
        add_opencode_candidates: true,
    },
    BuiltinAdapterPolicy {
        adapter: "kiro",
        add_opencode_candidates: true,
    },
    BuiltinAdapterPolicy {
        adapter: "copilot",
        add_opencode_candidates: true,
    },
];

fn adapter_policy(adapter: &str) -> Option<&'static dyn AdapterRoutingPolicy> {
    ADAPTER_POLICIES
        .iter()
        .find(|policy| policy.adapter() == adapter)
        .map(|policy| policy as &dyn AdapterRoutingPolicy)
}

/// A validated candidate retains every raw field, including nulls and dictionary order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FallbackCandidate {
    pub fields: OrderedObject,
}
impl FallbackCandidate {
    pub fn adapter(&self) -> &str {
        self.fields
            .get("adapter")
            .and_then(OrderedValue::as_str)
            .unwrap_or_default()
    }
    pub fn model(&self) -> &str {
        self.fields
            .get("model")
            .and_then(OrderedValue::as_str)
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum FallbackPolicy {
    #[default]
    Default,
    Disabled,
    Enabled,
    Any,
    /// Python validates each entry, then returns the original list unchanged.
    Explicit(Vec<OrderedValue>),
}
impl Serialize for FallbackPolicy {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Default => serializer.serialize_none(),
            Self::Disabled => serializer.serialize_bool(false),
            Self::Enabled => serializer.serialize_bool(true),
            Self::Any => serializer.serialize_str("any"),
            Self::Explicit(entries) => entries.serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for FallbackPolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = OrderedValue::deserialize(deserializer)?;
        validate_policy(&raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum FallbackError {
    #[error("fallback entries must be adapter/model[@effort] or route objects")]
    InvalidEntry,
    #[error("fallback entry requires an adapter and non-empty model")]
    MissingModel,
    #[error("fallback effort must be a string")]
    InvalidEffort,
    #[error("fallback must be false, true, any or a JSON route list")]
    InvalidPolicy,
    #[error("JSON integer is outside the i64/u64 range")]
    NumberOutOfRange,
}

pub fn parse_candidate(
    raw: &OrderedValue,
    default_adapter: &str,
) -> Result<FallbackCandidate, FallbackError> {
    let mut object = match raw {
        OrderedValue::String(text) => {
            let (mut model, effort) = match text.split_once('@') {
                Some((model, effort)) => (model, Some(effort)),
                None => (text.as_str(), None),
            };
            let mut adapter = default_adapter;
            if let Some((prefix, rest)) = model.split_once('/') {
                if adapter_policy(prefix).is_some() {
                    adapter = prefix;
                    model = rest;
                }
            }
            let mut object = OrderedObject(vec![
                ("adapter".into(), OrderedValue::String(adapter.into())),
                ("model".into(), OrderedValue::String(model.into())),
            ]);
            if let Some(effort) = effort {
                object.insert("effort".into(), OrderedValue::String(effort.into()));
            }
            object
        }
        OrderedValue::Object(object) => object.clone(),
        _ => return Err(FallbackError::InvalidEntry),
    };
    let model = ["model", "resolved_model", "id"]
        .iter()
        .filter_map(|key| object.get(key))
        .find(|value| crate::pending::truthy(value))
        .and_then(OrderedValue::as_str)
        .filter(|model| !python_strip(model).is_empty())
        .ok_or(FallbackError::MissingModel)?
        .to_owned();
    let adapter = match object.get("adapter") {
        Some(OrderedValue::String(adapter)) => adapter.as_str(),
        Some(_) => return Err(FallbackError::MissingModel),
        None => default_adapter,
    };
    if adapter_policy(adapter).is_none() {
        return Err(FallbackError::MissingModel);
    }
    if !matches!(
        object.get("effort"),
        None | Some(OrderedValue::Null | OrderedValue::String(_))
    ) {
        return Err(FallbackError::InvalidEffort);
    }
    let adapter = adapter.to_owned();
    object.insert("adapter".into(), OrderedValue::String(adapter));
    object.insert("model".into(), OrderedValue::String(model));
    Ok(FallbackCandidate { fields: object })
}

pub fn validate_policy(raw: &OrderedValue) -> Result<FallbackPolicy, FallbackError> {
    if let OrderedValue::String(text) = raw {
        if text == "any" {
            return Ok(FallbackPolicy::Any);
        }
        let parsed = crate::pending::from_json(text).map_err(|error| match error {
            crate::pending::JsonError::NumberOutOfRange(_) => FallbackError::NumberOutOfRange,
            crate::pending::JsonError::Json(_) => FallbackError::InvalidPolicy,
        })?;
        return validate_policy_value(&parsed);
    }
    validate_policy_value(raw)
}
fn validate_policy_value(raw: &OrderedValue) -> Result<FallbackPolicy, FallbackError> {
    match raw {
        OrderedValue::String(text) if text == "any" => Ok(FallbackPolicy::Any),
        OrderedValue::Null => Ok(FallbackPolicy::Default),
        OrderedValue::Bool(false) => Ok(FallbackPolicy::Disabled),
        OrderedValue::Bool(true) => Ok(FallbackPolicy::Enabled),
        OrderedValue::Array(entries) => {
            for entry in entries {
                parse_candidate(entry, "codex")?;
            }
            Ok(FallbackPolicy::Explicit(entries.clone()))
        }
        _ => Err(FallbackError::InvalidPolicy),
    }
}

/// The route requests returned by Python candidates(), including an explicit null effort.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionCandidate {
    pub adapter: String,
    pub model: String,
    /// Python returns `effort_applied` verbatim when present, so this is any JSON value.
    pub effort: OrderedValue,
}

/// Input plan consumed by candidates(); confidentiality and policy are separate arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRoutePlan {
    #[serde(default)]
    pub requested_model: Option<OrderedValue>,
    pub route: OrderedObject,
    pub adapter: String,
    pub model: String,
    #[serde(default)]
    pub effort: Option<OrderedValue>,
    #[serde(flatten)]
    pub extra: OrderedObject,
}

/// External router process boundary. Implementations must enforce the supplied deadline.
pub trait RouterHost {
    fn product_root(&self) -> PathBuf;
    fn python_executable(&self) -> OsString;
    fn is_executable(&self, path: &Path) -> bool;
    fn run_bounded(&self, argv: &[OsString], timeout: Duration)
        -> Result<Vec<u8>, RouterHostError>;
    fn load_catalog(&self, product: &Path) -> Result<Value, RouterHostError>;
    fn host_id(&self) -> String;
    fn warning(&self, message: &str);
}

#[derive(Debug, Error)]
pub enum RouterHostError {
    #[error("router process is unavailable")]
    Process,
    #[error("router catalogue is unavailable")]
    Catalog,
}

/// Provider quota is advisory to route selection. The quota crate currently owns
/// quota records and pure display policy; this seam keeps account operations injected.
// port: belongs in provenant-quota (account routing/advice API is not on main).
pub trait ProviderQuota {
    fn claude_account_needs_reselection(
        &self,
        account: &str,
        now: jiff::Timestamp,
    ) -> Result<bool, QuotaUnavailable>;
    fn reselect_claude_account(
        &self,
        now: jiff::Timestamp,
    ) -> Result<Option<String>, QuotaUnavailable>;
    fn label_is_valid(&self, account: &str) -> bool;
    fn pinned_account_state(
        &self,
        adapter: &str,
        account: &str,
    ) -> Result<Option<String>, QuotaUnavailable>;
    fn apply_to_cooldowns(
        &self,
        records: &provenant_records::fabric_records::CooldownRows,
        selected_account: Option<(&str, &str)>,
        include_exhausted: bool,
        now: jiff::Timestamp,
    ) -> Result<provenant_records::fabric_records::CooldownRows, QuotaUnavailable>;
}

#[derive(Clone, Copy, Debug, Error)]
#[error("provider quota advice is unavailable")]
pub struct QuotaUnavailable;

/// The record owner performs scoped, lock-safe cooldown reads.
pub trait ExecutionCooldownStore {
    fn read_cooldowns(
        &self,
        scopes: Option<&BTreeMap<String, provenant_records::fabric_records::Scope>>,
    ) -> Result<
        provenant_records::fabric_records::CooldownRows,
        provenant_records::fabric_records::RecordError,
    >;
}

impl<T: provenant_records::fabric_records::FabricRecordStore> ExecutionCooldownStore for T {
    fn read_cooldowns(
        &self,
        scopes: Option<&BTreeMap<String, provenant_records::fabric_records::Scope>>,
    ) -> Result<
        provenant_records::fabric_records::CooldownRows,
        provenant_records::fabric_records::RecordError,
    > {
        provenant_records::fabric_records::FabricRecordStore::read_cooldowns(
            self, None, None, scopes,
        )
    }
}

#[derive(Debug, Error)]
pub enum ExecutionRoutingError {
    #[error(transparent)]
    Fallback(#[from] FallbackError),
    #[error("execution route plan is malformed: {0}")]
    InvalidPlan(&'static str),
    #[error("cooldown record has an invalid timestamp")]
    InvalidCoolingTimestamp,
    #[error("cooldown record is missing cooling_until")]
    MissingCoolingTimestamp,
    #[error("cannot compare timezone-aware and timezone-naive cooling timestamps")]
    IncompatibleCoolingTimestamps,
    #[error(transparent)]
    Store(#[from] provenant_records::fabric_records::RecordError),
}

/// Explicit runtime dependencies used by execution-facing routing policy.
pub struct ExecutionRouting<'a> {
    pub host: &'a dyn RouterHost,
    pub store: &'a dyn ExecutionCooldownStore,
    pub quota: Option<&'a dyn ProviderQuota>,
    pub now: jiff::Timestamp,
}

const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(15);

/// Build the same wrapper-or-interpreter command selected by the Python runtime.
pub fn route_command(host: &dyn RouterHost, product: &Path) -> Vec<OsString> {
    let wrapper = product.join("scripts/model-route");
    if host.is_executable(&wrapper) {
        vec![wrapper.into_os_string()]
    } else {
        vec![
            host.python_executable(),
            product.join("scripts/model_route.py").into_os_string(),
        ]
    }
}

/// Snapshot errors, malformed JSON, and a wrong schema all fall back to an empty catalogue.
pub fn snapshot(host: &dyn RouterHost) -> Value {
    let product = host.product_root();
    let mut argv = route_command(host, &product);
    argv.extend([OsString::from("snapshot"), OsString::from("--json")]);
    host.run_bounded(&argv, SNAPSHOT_TIMEOUT)
        .ok()
        .and_then(|bytes| provenant_records::checked_json::from_slice(&bytes).ok())
        .filter(|data| {
            data.is_object()
                && data.get("schema").and_then(Value::as_str) == Some("fabric.catalogue.v1")
        })
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Cooldown identity uses the router's registered id, without an effort selector.
pub fn registered_model(
    host: &dyn RouterHost,
    adapter: &str,
    model: &str,
    catalogue: Option<&Value>,
    warnings: Option<&mut Vec<String>>,
) -> String {
    let model = model.split_once('@').map_or(model, |(head, _)| head);
    if model == "*" {
        return model.to_owned();
    }
    let raw_catalogue = catalogue.cloned().unwrap_or_else(|| snapshot(host));
    let catalogue = match prepare_registered_catalogue(&raw_catalogue, adapter) {
        Ok(Some(catalogue)) => catalogue,
        Ok(None) => return model.to_owned(),
        Err(kind) => {
            report_registered_model_warning(host, adapter, model, warnings, kind);
            return model.to_owned();
        }
    };
    let cached = BTreeMap::new();
    let resolver = crate::matching::ModelCatalog {
        catalog: &catalogue,
        cached: &cached,
    };
    match resolver.registered_match(adapter, model) {
        Ok((Some(entry), _)) => entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(model)
            .to_owned(),
        Ok((None, _)) => model.to_owned(),
        Err(error) => {
            report_registered_model_warning(
                host,
                adapter,
                model,
                warnings,
                match_error_class(&error),
            );
            model.to_owned()
        }
    }
}

/// Return every configured catalogue-pattern family, preserving ambiguity and source order.
pub fn model_families(
    host: &dyn RouterHost,
    model: &str,
    catalogue: Option<&Value>,
) -> Vec<String> {
    let mut raw = catalogue
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(nested) = raw
        .get("catalogue")
        .filter(|value| value.is_object())
        .cloned()
    {
        raw = nested;
    }
    if raw.get("model_patterns").is_none() {
        let Ok(loaded) = host.load_catalog(&host.product_root()) else {
            return vec![];
        };
        raw = loaded;
    }
    crate::catalog::matching_model_families(model, &raw).unwrap_or_default()
}

/// Check the Python access/iteration shapes used by `_registered_match` and
/// normalize the few iterable forms accepted there but not by the Rust matcher.
fn prepare_registered_catalogue(
    catalogue: &Value,
    adapter: &str,
) -> Result<Option<Value>, &'static str> {
    let root = catalogue.as_object().ok_or("AttributeError")?;
    let Some(adapters) = root.get("adapters") else {
        return Ok(None);
    };
    let adapter_entry = match adapters {
        Value::Object(adapters) => {
            let Some(entry) = adapters.get(adapter) else {
                return Ok(None);
            };
            entry
        }
        // Python's initial `adapter in adapters` accepts lists and strings,
        // then `_registered_match` attempts to index them with a string.
        Value::Array(adapters) => {
            return if adapters.iter().any(|value| value.as_str() == Some(adapter)) {
                Err("TypeError")
            } else {
                Ok(None)
            };
        }
        Value::String(adapters) => {
            return if adapters.contains(adapter) {
                Err("TypeError")
            } else {
                Ok(None)
            };
        }
        _ => return Err("TypeError"),
    };
    let adapter_entry = adapter_entry.as_object().ok_or("AttributeError")?;
    let mut normalized = catalogue.clone();
    let normalized_adapter = normalized
        .get_mut("adapters")
        .and_then(|value| value.get_mut(adapter))
        .and_then(Value::as_object_mut)
        .ok_or("AttributeError")?;
    let Some(models) = adapter_entry.get("models") else {
        return Ok(Some(normalized));
    };
    match models {
        Value::Array(models) => {
            for model in models {
                let Some(fields) = model.as_object() else {
                    return Err("TypeError");
                };
                let Some(id) = fields.get("id") else {
                    return Err("KeyError");
                };
                if !id.is_string() {
                    return Err("AttributeError");
                }
            }
            let mut normalized_models = models.clone();
            for model in &mut normalized_models {
                let fields = model.as_object_mut().ok_or("TypeError")?;
                if let Some(names) = fields.get("names").cloned() {
                    let names = match names {
                        Value::Array(names) => {
                            if names.iter().any(|name| !name.is_string()) {
                                return Err("AttributeError");
                            }
                            names
                        }
                        Value::String(names) => names
                            .chars()
                            .map(|name| Value::String(name.to_string()))
                            .collect(),
                        Value::Object(names) => names.keys().cloned().map(Value::String).collect(),
                        _ => return Err("TypeError"),
                    };
                    fields.insert("names".into(), Value::Array(names));
                }
            }
            for model in &normalized_models {
                let fields = model.as_object().ok_or("TypeError")?;
                if fields.get("effort_transport").and_then(Value::as_str) == Some("model-suffix") {
                    match fields.get("suffix") {
                        None => {}
                        Some(Value::Object(suffixes)) => {
                            if suffixes.values().any(|suffix| !suffix.is_string()) {
                                return Err("TypeError");
                            }
                        }
                        Some(_) => return Err("AttributeError"),
                    }
                }
            }
            normalized_adapter.insert("models".into(), Value::Array(normalized_models));
        }
        Value::Object(models) if models.is_empty() => {
            normalized_adapter.insert("models".into(), Value::Array(vec![]));
        }
        Value::String(models) if models.is_empty() => {
            normalized_adapter.insert("models".into(), Value::Array(vec![]));
        }
        Value::Object(_) | Value::String(_) | Value::Null | Value::Number(_) | Value::Bool(_) => {
            return Err("TypeError");
        }
    }
    Ok(Some(normalized))
}

fn match_error_class(error: &crate::matching::MatchError) -> &'static str {
    match error {
        crate::matching::MatchError::Catalog("adapter") => "AttributeError",
        crate::matching::MatchError::Catalog("model list")
        | crate::matching::MatchError::Catalog("model entry") => "TypeError",
        crate::matching::MatchError::Catalog("model id") => "KeyError",
        crate::matching::MatchError::Catalog("model string list")
        | crate::matching::MatchError::Catalog("effort suffix map") => "AttributeError",
        crate::matching::MatchError::Catalog("effort suffix") => "TypeError",
        crate::matching::MatchError::Catalog(_) => "ValueError",
        crate::matching::MatchError::NumberOutOfRange => "ValueError",
        crate::matching::MatchError::Regex(_) => "error",
    }
}

fn report_registered_model_warning(
    host: &dyn RouterHost,
    adapter: &str,
    model: &str,
    warnings: Option<&mut Vec<String>>,
    kind: &str,
) {
    let warning = format!("catalogue malformed for {adapter}/{model}; using raw model ({kind})");
    if let Some(target) = warnings {
        target.push(warning);
    } else {
        host.warning(&warning);
    }
}

fn catalog_for_policy(host: &dyn RouterHost) -> Option<Value> {
    host.load_catalog(&host.product_root()).ok()
}

fn configured_traits(host: &dyn RouterHost, adapter: &str, model: &str) -> Vec<String> {
    let Some(catalogue) = catalog_for_policy(host) else {
        return vec![];
    };
    let cached = BTreeMap::new();
    let resolver = crate::matching::ModelCatalog {
        catalog: &catalogue,
        cached: &cached,
    };
    crate::pools::model_traits(&resolver, adapter, model).unwrap_or_default()
}

fn registered_training(
    host: &dyn RouterHost,
    adapter: &str,
    model: &str,
    meta: &OrderedObject,
) -> Option<bool> {
    if let Some(OrderedValue::Bool(value)) = meta.get("trains_on_prompts") {
        return Some(*value);
    }
    let catalogue = catalog_for_policy(host)?;
    let cached = BTreeMap::new();
    let resolver = crate::matching::ModelCatalog {
        catalog: &catalogue,
        cached: &cached,
    };
    let registered = resolver.registered_match(adapter, model).ok()?.0?;
    registered.get("trains_on_prompts").and_then(Value::as_bool)
}

fn py_equal_zero(value: Option<&OrderedValue>) -> bool {
    match value {
        Some(OrderedValue::Bool(false)) => true,
        Some(OrderedValue::Number(number)) if number.is_f64() => number.as_f64() == Some(0.0),
        Some(OrderedValue::Number(number)) => {
            number.as_i64() == Some(0) || number.as_u64() == Some(0)
        }
        _ => false,
    }
}

fn is_free_model(model: &str, meta: &OrderedObject) -> bool {
    py_equal_zero(meta.get("plan_cap_usd"))
        || model.ends_with("-free")
        || model.ends_with(":free")
        || model.starts_with("opencode/")
}

/// Why a route may retain or train on prompts, or `None` when safe.
pub fn disclosure_risk(
    host: &dyn RouterHost,
    adapter: &str,
    model: &str,
    meta: Option<&OrderedObject>,
) -> Option<&'static str> {
    let empty = OrderedObject::default();
    let meta = meta.unwrap_or(&empty);
    let free = is_free_model(model, meta);
    if meta.get("trains_on_prompts") == Some(&OrderedValue::Bool(true)) {
        return Some("trains on prompts");
    }
    if configured_traits(host, adapter, model)
        .iter()
        .any(|value| value == "trains-on-prompts")
    {
        return Some(if free {
            "is a free tier that may train on prompts"
        } else {
            "may train on prompts (model_traits)"
        });
    }
    if registered_training(host, adapter, model, meta) == Some(false) {
        return None;
    }
    free.then_some("is a free tier that may train on prompts")
}

/// Ordered safe members of an alias, excluding the current and cooling models.
pub fn confidential_alternatives(
    runtime: &ExecutionRouting<'_>,
    adapter: &str,
    model: &str,
    alias: Option<&str>,
    catalogue: Option<&Value>,
) -> Result<Vec<String>, ExecutionRoutingError> {
    let catalogue = catalogue
        .cloned()
        .or_else(|| catalog_for_policy(runtime.host));
    let Some(catalogue) = catalogue else {
        return Ok(vec![]);
    };
    let entry = catalogue
        .get("adapters")
        .and_then(|value| value.get(adapter));
    let mut aliases = entry.and_then(|value| value.get("aliases"));
    let aliases_empty = aliases.is_none_or(|value| !provenant_core::py::python_truth(value));
    if aliases_empty {
        if let Some(family) = entry
            .and_then(|value| value.get("fixed_model_family"))
            .and_then(Value::as_str)
        {
            aliases = catalogue
                .get("families")
                .and_then(|value| value.get(family))
                .and_then(|value| value.get("aliases"));
        }
    }
    let alias = alias.filter(|value| !value.is_empty()).unwrap_or("senior");
    let Some(members) = aliases
        .and_then(|value| value.get(alias))
        .and_then(Value::as_array)
    else {
        return Ok(vec![]);
    };
    let mut answer = Vec::new();
    for member in members.iter().filter_map(Value::as_str) {
        if member == model || disclosure_risk(runtime.host, adapter, member, None).is_some() {
            continue;
        }
        if runtime
            .cooling(adapter, member, None, None, false)?
            .is_some()
        {
            continue;
        }
        answer.push(member.to_owned());
    }
    Ok(answer)
}

fn adapter_name(entry: &Value) -> Option<&str> {
    let fields = entry.as_object()?;
    fields
        .get("name")
        .or_else(|| fields.get("adapter"))
        .or_else(|| fields.get("id"))
        .and_then(Value::as_str)
}

fn find_adapter<'a>(catalogue: &'a Value, name: &str) -> Option<&'a Value> {
    match catalogue.get("adapters")? {
        Value::Object(adapters) => adapters.get(name),
        // Python's `{item.get("name", item.get("adapter", item.get("id"))): item}`
        // keeps the final matching dictionary and ignores non-dictionary entries.
        Value::Array(adapters) => adapters
            .iter()
            .rev()
            .find(|entry| adapter_name(entry) == Some(name)),
        _ => None,
    }
}

fn route_values(value: Option<&OrderedValue>) -> Result<Vec<OrderedValue>, ExecutionRoutingError> {
    if value.is_some_and(|value| !crate::pending::truthy(value)) {
        return Ok(vec![]);
    }
    match value {
        None => Ok(vec![]),
        Some(OrderedValue::Array(values)) => Ok(values.clone()),
        Some(OrderedValue::Object(object)) => Ok(object
            .0
            .iter()
            .map(|(key, _)| OrderedValue::String(key.clone()))
            .collect()),
        Some(OrderedValue::String(text)) => Ok(text
            .chars()
            .map(|ch| OrderedValue::String(ch.to_string()))
            .collect()),
        Some(_) => Err(ExecutionRoutingError::InvalidPlan(
            "route candidates must be iterable",
        )),
    }
}

fn route_json_values(value: Option<&Value>) -> Result<Vec<OrderedValue>, ExecutionRoutingError> {
    let Some(value) = value else {
        return Ok(vec![]);
    };
    route_values(Some(&OrderedValue::from(value.clone())))
}

fn catalog_models(catalogue: &Value, model: &str) -> OrderedObject {
    match catalogue.get("models") {
        Some(Value::Object(models)) => models
            .get(model)
            .and_then(Value::as_object)
            .map(|value| {
                OrderedObject(
                    value
                        .iter()
                        .map(|(key, value)| (key.clone(), OrderedValue::from(value.clone())))
                        .collect(),
                )
            })
            .unwrap_or_default(),
        Some(Value::Array(models)) => models
            .iter()
            .rfind(|entry| entry.get("id").and_then(Value::as_str) == Some(model))
            .and_then(Value::as_object)
            .map(|value| {
                OrderedObject(
                    value
                        .iter()
                        .map(|(key, value)| (key.clone(), OrderedValue::from(value.clone())))
                        .collect(),
                )
            })
            .unwrap_or_default(),
        _ => OrderedObject::default(),
    }
}

/// Return ordered retry route requests with Python's policy and truthiness rules.
pub fn candidates(
    runtime: &ExecutionRouting<'_>,
    plan: &ExecutionRoutePlan,
    policy: Option<&OrderedValue>,
    catalogue: Option<&Value>,
    confidential: bool,
) -> Result<Vec<ExecutionCandidate>, ExecutionRoutingError> {
    let mut policy = policy.map(validate_policy).transpose()?;
    if policy.is_none() {
        policy = Some(
            if plan
                .requested_model
                .as_ref()
                .is_some_and(crate::pending::truthy)
            {
                FallbackPolicy::Disabled
            } else {
                FallbackPolicy::Enabled
            },
        );
    }
    let policy = policy.unwrap_or_default();
    if policy == FallbackPolicy::Disabled {
        return Ok(vec![]);
    }
    let catalogue = catalogue.cloned().unwrap_or_else(|| snapshot(runtime.host));
    let route = &plan.route;
    let alias_value = route
        .get("alias")
        .filter(|value| crate::pending::truthy(value));
    let alias = alias_value.and_then(OrderedValue::as_str);
    let adapter_alias = if alias_value.is_some() {
        alias
    } else {
        Some("senior")
    };
    let explicit = matches!(policy, FallbackPolicy::Explicit(_));
    let mut raw = match &policy {
        FallbackPolicy::Explicit(items) => items.clone(),
        _ if route.get("fallback_candidates").is_some() => match route.get("fallback_candidates") {
            Some(OrderedValue::Array(items)) => items.clone(),
            _ => vec![],
        },
        _ => {
            let mut items = route_values(route.get("candidates"))?;
            if let Some(value) = route
                .get("fallback_model")
                .filter(|value| crate::pending::truthy(value))
            {
                items.push(value.clone());
            }
            let adapter_config = find_adapter(&catalogue, &plan.adapter);
            let alias_items = adapter_alias.and_then(|alias| {
                adapter_config
                    .and_then(|entry| entry.get("aliases"))
                    .and_then(|aliases| aliases.get(alias))
            });
            items.extend(route_json_values(alias_items)?);
            if adapter_policy(&plan.adapter).is_some_and(|policy| policy.add_opencode_candidates())
            {
                if let Some(cross) = find_adapter(&catalogue, "opencode") {
                    let aliases = cross.get("aliases");
                    let cross_members = alias
                        .and_then(|alias| aliases.and_then(|value| value.get(alias)))
                        .filter(|value| crate::pending::value_truthy(value))
                        .or_else(|| {
                            aliases
                                .and_then(|value| value.get("senior"))
                                .filter(|value| crate::pending::value_truthy(value))
                        });
                    let members = match cross_members {
                        Some(value) => route_values(Some(&OrderedValue::from(value.clone())))?,
                        None => cross
                            .get("default_model")
                            .filter(|value| !value.is_null())
                            .map(|value| vec![OrderedValue::from(value.clone())])
                            .unwrap_or_default(),
                    };
                    for member in members.into_iter().filter(crate::pending::truthy) {
                        let mut object = OrderedObject::default();
                        object.insert("adapter".into(), OrderedValue::String("opencode".into()));
                        object.insert("model".into(), member);
                        items.push(OrderedValue::Object(object));
                    }
                }
            }
            items
        }
    };
    let mut seen = vec![(plan.adapter.as_str().to_owned(), plan.model.clone())];
    let mut answer = Vec::new();
    for value in raw.drain(..) {
        let candidate = match parse_candidate(&value, &plan.adapter) {
            Ok(candidate) => candidate,
            Err(error) if explicit => return Err(error.into()),
            Err(_) => continue,
        };
        let adapter = candidate.adapter().to_owned();
        let model = candidate.model().to_owned();
        if model.is_empty()
            || adapter_policy(&adapter).is_none()
            || seen.iter().any(|(a, m)| a == &adapter && m == &model)
        {
            continue;
        }
        seen.push((adapter.clone(), model.clone()));
        let meta = catalog_models(&catalogue, &model);
        let training = candidate
            .fields
            .get("trains_on_prompts")
            .or_else(|| meta.get("trains_on_prompts"));
        let mut merged = meta.clone();
        for (key, value) in &candidate.fields.0 {
            merged.insert(key.clone(), value.clone());
        }
        let free = py_equal_zero(
            candidate
                .fields
                .get("plan_cap_usd")
                .or_else(|| meta.get("plan_cap_usd")),
        ) || model.ends_with("-free")
            || model.ends_with(":free")
            || model.starts_with("opencode/");
        if confidential {
            if disclosure_risk(runtime.host, &adapter, &model, Some(&merged)).is_some() {
                continue;
            }
        } else if !explicit
            && policy != FallbackPolicy::Any
            && (training == Some(&OrderedValue::Bool(true))
                || (free
                    && registered_training(runtime.host, &adapter, &model, &merged) != Some(false)))
        {
            continue;
        }
        let effort = candidate
            .fields
            .get("effort_applied")
            .or_else(|| candidate.fields.get("effort"))
            .or(plan.effort.as_ref())
            .cloned()
            .unwrap_or(OrderedValue::Null);
        answer.push(ExecutionCandidate {
            adapter,
            model,
            effort,
        });
    }
    Ok(answer)
}

fn cooldown_timestamp(
    value: &str,
) -> Result<provenant_core::py::PythonDateTime, ExecutionRoutingError> {
    let normalized = value
        .strip_suffix('Z')
        .map(|head| format!("{head}+00:00"))
        .unwrap_or_else(|| value.to_owned());
    provenant_core::py::python_fromisoformat(&normalized)
        .map_err(|_| ExecutionRoutingError::InvalidCoolingTimestamp)
}

fn compare_cooling_timestamps(
    left: provenant_core::py::PythonDateTime,
    right: provenant_core::py::PythonDateTime,
) -> Result<std::cmp::Ordering, ExecutionRoutingError> {
    match (left.offset_microseconds, right.offset_microseconds) {
        (None, None) => Ok(left.datetime.cmp(&right.datetime)),
        (Some(left_offset), Some(right_offset)) => {
            let epoch = jiff::civil::Date::new(1970, 1, 1)
                .map_err(|_| ExecutionRoutingError::InvalidCoolingTimestamp)?
                .at(0, 0, 0, 0);
            let left_utc =
                left.datetime.duration_since(epoch).as_micros() - i128::from(left_offset);
            let right_utc =
                right.datetime.duration_since(epoch).as_micros() - i128::from(right_offset);
            Ok(left_utc.cmp(&right_utc))
        }
        _ => Err(ExecutionRoutingError::IncompatibleCoolingTimestamps),
    }
}

impl ExecutionRouting<'_> {
    pub fn claude_account_needs_reselection(&self, account: Option<&str>) -> bool {
        let (Some(quota), Some(account)) = (self.quota, account) else {
            return false;
        };
        quota
            .claude_account_needs_reselection(account, self.now)
            .unwrap_or(true)
    }

    pub fn reselect_claude_account(&self) -> Option<String> {
        self.quota?.reselect_claude_account(self.now).ok().flatten()
    }

    pub fn pinned_account_warning(&self, adapter: &str, account: Option<&str>) -> Option<String> {
        let quota = self.quota?;
        let account = account?;
        if !quota.label_is_valid(account) {
            return None;
        }
        let state = quota
            .pinned_account_state(adapter, account)
            .ok()
            .flatten()?;
        if state == "credit_only" || state == "exhausted" {
            Some(format!(
                "account {account} is out of quota ({state}): launching because it was pinned explicitly"
            ))
        } else {
            None
        }
    }

    pub fn cooling(
        &self,
        adapter: &str,
        model: &str,
        records: Option<&provenant_records::fabric_records::CooldownRows>,
        account: Option<&str>,
        explicit_account: bool,
    ) -> Result<Option<provenant_records::fabric_records::RecordRow>, ExecutionRoutingError> {
        use provenant_records::fabric_records::{Field, Scope};
        let mut rows = if let Some(records) = records {
            records.clone()
        } else {
            let scopes = account.map(|account| {
                let mut scopes = BTreeMap::new();
                scopes.insert(
                    adapter.to_owned(),
                    Scope {
                        account: Field::Value(account.to_owned()),
                        host: Field::Value(self.host.host_id()),
                    },
                );
                scopes
            });
            self.store.read_cooldowns(scopes.as_ref())?
        };
        if records.is_none() {
            if explicit_account {
                rows.retain(|_, row| {
                    row.get("source").and_then(Value::as_str) != Some("provider quota")
                });
            }
            if let Some(quota) = self.quota {
                if let Ok(updated) = quota.apply_to_cooldowns(
                    &rows,
                    account.map(|account| (adapter, account)),
                    !explicit_account,
                    self.now,
                ) {
                    rows = updated;
                }
            }
        }
        let catalog = None;
        let canonical = registered_model(self.host, adapter, model, catalog, None);
        let exact = format!("{adapter}/{canonical}");
        let wildcard = format!("{adapter}/*");
        let keys = [exact, wildcard];
        let entries: Vec<_> = keys
            .iter()
            .filter_map(|key| {
                rows.get(key)
                    .filter(|row| !row.is_empty())
                    .map(|row| (key.as_str(), row))
            })
            .collect();
        let mut best: Option<(
            &str,
            &provenant_records::fabric_records::RecordRow,
            provenant_core::py::PythonDateTime,
        )> = None;
        for (key, row) in entries {
            let stamp = row
                .get("cooling_until")
                .and_then(Value::as_str)
                .ok_or(ExecutionRoutingError::MissingCoolingTimestamp)?;
            let parsed = cooldown_timestamp(stamp)?;
            if let Some((_, _, old)) = best {
                if compare_cooling_timestamps(parsed, old)? == std::cmp::Ordering::Greater {
                    best = Some((key, row, parsed));
                }
            } else {
                best = Some((key, row, parsed));
            }
        }
        Ok(best.map(|(_, row, _)| row.clone()))
    }
}

#[cfg(test)]
#[path = "exec_routing/tests.rs"]
mod runtime_tests;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{FallbackCandidate, FallbackError, FallbackPolicy};
    fn parse_candidate(
        raw: &serde_json::Value,
        adapter: &str,
    ) -> Result<FallbackCandidate, FallbackError> {
        super::parse_candidate(&raw.clone().into(), adapter)
    }
    fn validate_policy(raw: &serde_json::Value) -> Result<FallbackPolicy, FallbackError> {
        super::validate_policy(&raw.clone().into())
    }

    #[test]
    fn fallback_json_string_preserves_typed_integer_range_errors() {
        let raw = crate::ordered_json::OrderedValue::String(
            r#"[{"model":"x","meta":18446744073709551616}]"#.into(),
        );
        assert_eq!(
            super::validate_policy(&raw),
            Err(FallbackError::NumberOutOfRange)
        );
    }

    #[test]
    fn fallback_wire_and_candidate_dicts_match_the_python_source() {
        let wires = vec![
            r#"null"#,
            r#"true"#,
            r#"false"#,
            r#""any""#,
            r#"["opencode/vendor/model@high",{"z":0,"effort":null,"id":"x","a":2}]"#,
            r#"{"effort":null,"id":"x","extra":{"z":1,"a":2}}"#,
            r#"{"model":"x","adapter":"claude","effort":""}"#,
            r#"{"model":[],"resolved_model":"x"}"#,
            r#"{"model":true,"id":"x"}"#,
            r#"{"adapter":null,"model":"x"}"#,
            r#"{"model":"x","effort":1}"#,
            r#""opencode/openrouter/vendor/model@high""#,
            r#""\u001c\u001f""#,
        ];
        let Some(expected): Option<Vec<Vec<String>>> = crate::oracle::python(
            "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve())); import exec_routing as m\nanswer=[]\nfor wire in json.load(sys.stdin):\n row=[]\n for fn in (m.validate_policy,lambda value:m.parse_candidate(value,'codex')):\n  try: row.append(json.dumps(fn(json.loads(wire)),ensure_ascii=False,separators=(',',':')))\n  except ValueError as error: row.append('error:'+str(error))\n answer.append(row)\nprint(json.dumps(answer))",
            &serde_json::json!(wires),
        ) else {
            return;
        };
        let actual: Vec<Vec<String>> = wires
            .iter()
            .map(|wire| {
                let raw = crate::pending::from_json(wire).unwrap();
                vec![
                    super::validate_policy(&raw)
                        .map(|value| serde_json::to_string(&value).unwrap())
                        .unwrap_or_else(|error| format!("error:{error}")),
                    super::parse_candidate(&raw, "codex")
                        .map(|value| serde_json::to_string(&value).unwrap())
                        .unwrap_or_else(|error| format!("error:{error}")),
                ]
            })
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn policies_round_trip_python_wire_forms_without_normalizing_the_raw_list() {
        for wire in [
            "null",
            "true",
            "false",
            r#""any""#,
            r#"["opencode/vendor/model@high",{"z":1,"effort":null,"id":"x","meta":{"z":0,"a":1}}]"#,
        ] {
            let policy: FallbackPolicy = crate::pending::from_json(wire).unwrap();
            assert_eq!(serde_json::to_string(&policy).unwrap(), wire);
            let raw = crate::pending::from_json(wire).unwrap();
            assert_eq!(super::validate_policy(&raw).unwrap(), policy);
        }
        assert!(serde_json::from_str::<FallbackPolicy>(r#"{"explicit":[]}"#).is_err());
    }

    #[test]
    fn parsed_candidates_keep_null_and_insertion_order() {
        let raw = crate::pending::from_json(
            r#"{"z":0,"effort":null,"resolved_model":"x","adapter":"claude","a":2}"#,
        )
        .unwrap();
        let candidate = super::parse_candidate(&raw, "codex").unwrap();
        assert_eq!(
            serde_json::to_string(&candidate).unwrap(),
            r#"{"z":0,"effort":null,"resolved_model":"x","adapter":"claude","a":2,"model":"x"}"#
        );
        let raw =
            crate::pending::from_json(r#"{"model":"x","effort":null,"adapter":"claude"}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&super::parse_candidate(&raw, "codex").unwrap()).unwrap(),
            r#"{"model":"x","effort":null,"adapter":"claude"}"#
        );
        assert!(super::parse_candidate(
            &crate::ordered_json::OrderedValue::String("\u{1c}\u{1f}".into()),
            "codex"
        )
        .is_err());
    }

    #[test]
    fn execution_interfaces_match_plan_inputs_and_route_request_outputs() {
        let wire = r#"{"requested_model":"gpt-6.1-sol","route":{"alias":"senior","fallback_candidates":["claude/sonnet"]},"adapter":"codex","model":"gpt-6.1-sol","effort":"high","task":"work"}"#;
        let plan: super::ExecutionRoutePlan = crate::pending::from_json(wire).unwrap();
        assert_eq!(
            plan.requested_model
                .as_ref()
                .and_then(crate::ordered_json::OrderedValue::as_str),
            Some("gpt-6.1-sol")
        );
        assert_eq!(
            plan.route
                .get("alias")
                .and_then(crate::ordered_json::OrderedValue::as_str),
            Some("senior")
        );
        assert_eq!(serde_json::to_string(&plan).unwrap(), wire);
        let candidate: super::ExecutionCandidate =
            serde_json::from_str(r#"{"adapter":"claude","model":"sonnet","effort":null}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&candidate).unwrap(),
            r#"{"adapter":"claude","model":"sonnet","effort":null}"#
        );
    }

    #[test]
    fn fallback_policy_wire_null_matches_python() {
        assert_eq!(
            serde_json::to_string(&FallbackPolicy::Default).unwrap(),
            "null"
        );
    }

    #[test]
    fn fallback_candidate_strings_preserve_nested_model_paths_and_effort() {
        let parsed =
            parse_candidate(&json!("opencode/openrouter/vendor/model@high"), "codex").unwrap();
        assert_eq!(parsed.adapter(), "opencode");
        assert_eq!(parsed.model(), "openrouter/vendor/model");
        assert_eq!(
            parsed
                .fields
                .get("effort")
                .and_then(crate::ordered_json::OrderedValue::as_str),
            Some("high")
        );
    }

    #[test]
    fn fallback_policy_accepts_only_the_supported_shapes() {
        assert_eq!(
            validate_policy(&json!(false)).unwrap(),
            FallbackPolicy::Disabled
        );
        assert_eq!(validate_policy(&json!("any")).unwrap(), FallbackPolicy::Any);
        assert_eq!(
            validate_policy(&json!(r#""any""#)).unwrap(),
            FallbackPolicy::Any
        );
        assert_eq!(
            validate_policy(&json!("false")).unwrap(),
            FallbackPolicy::Disabled
        );
        assert_eq!(
            validate_policy(&json!(r#""false""#)),
            Err(super::FallbackError::InvalidPolicy)
        );
        assert!(validate_policy(&json!([{"adapter":"bogus","model":"x"}])).is_err());
        assert!(parse_candidate(&json!({"adapter":"codex","model":"  "}), "codex").is_err());
    }

    #[test]
    fn fallback_object_fields_match_python_truthiness_and_presence() {
        assert!(parse_candidate(&json!({"adapter":null,"model":"x"}), "codex").is_err());
        assert!(parse_candidate(&json!({"model":42,"id":"x"}), "codex").is_err());
        assert!(parse_candidate(&json!({"model":" ","id":"x"}), "codex").is_err());
        assert_eq!(
            parse_candidate(&json!({"model":[],"id":"x"}), "codex")
                .unwrap()
                .model(),
            "x"
        );
        assert_eq!(
            parse_candidate(&json!({"model":"x"}), "codex")
                .unwrap()
                .adapter(),
            "codex"
        );
    }
}
