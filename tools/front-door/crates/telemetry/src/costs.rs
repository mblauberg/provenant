//! Model attribution and summable API-equivalent costs. No rounding before finalisation.
use crate::{number, Numeric, Result, Usage};
use regex::Regex;
use serde::{
    de::{MapAccess, Visitor},
    ser::SerializeMap,
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    ops::Index,
    path::Path,
};

/// A JSON object whose insertion order is observable at the Python boundary.
///
/// The ledger's grouping dimensions and token-missing counters are emitted as
/// dictionaries by the source implementation, so a sorted map changes their
/// JSON wire order. Keep this type local until a shared records owner provides
/// the same ordered map contract.
#[derive(Clone, Debug, PartialEq)]
pub struct OrderedFields<T>(Vec<(String, T)>);

impl<T> Default for OrderedFields<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> OrderedFields<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_keys(keys: &[&str], mut value: impl FnMut(&str) -> T) -> Self {
        keys.iter().map(|key| ((*key).into(), value(key))).collect()
    }

    pub fn get(&self, key: &str) -> Option<&T> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut T> {
        self.0
            .iter_mut()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn insert(&mut self, key: String, value: T) -> Option<T> {
        if let Some((_, previous)) = self.0.iter_mut().find(|(name, _)| name == &key) {
            return Some(std::mem::replace(previous, value));
        }
        self.0.push((key, value));
        None
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &T)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut T)> {
        self.0.iter_mut().map(|(key, value)| (key.as_str(), value))
    }
}

impl<T> FromIterator<(String, T)> for OrderedFields<T> {
    fn from_iter<I: IntoIterator<Item = (String, T)>>(iter: I) -> Self {
        let mut fields = Self::new();
        for (key, value) in iter {
            fields.insert(key, value);
        }
        fields
    }
}

impl<T> IntoIterator for OrderedFields<T> {
    type Item = (String, T);
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<T> From<BTreeMap<String, T>> for OrderedFields<T> {
    fn from(value: BTreeMap<String, T>) -> Self {
        Self(value.into_iter().collect())
    }
}

impl<T> Index<&str> for OrderedFields<T> {
    type Output = T;

    fn index(&self, index: &str) -> &Self::Output {
        self.get(index).expect("ordered field exists")
    }
}

impl<T: Serialize> Serialize for OrderedFields<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for OrderedFields<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct OrderedFieldsVisitor<T>(std::marker::PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for OrderedFieldsVisitor<T> {
            type Value = OrderedFields<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object with ordered fields")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut access: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut fields = OrderedFields::new();
                while let Some((key, value)) = access.next_entry()? {
                    fields.insert(key, value);
                }
                Ok(fields)
            }
        }

        deserializer.deserialize_map(OrderedFieldsVisitor(std::marker::PhantomData))
    }
}

pub const TOKEN_FIELDS: [&str; 5] = [
    "input_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "output_tokens",
    "reasoning_tokens",
];
pub const SUMMED: [&str; 12] = [
    "rows",
    "input_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "output_tokens",
    "reasoning_tokens",
    "missing_token_rows",
    "cache_share_rows",
    "cache_share_input",
    "cache_share_read",
    "known_cost_usd",
    "missing_cost_rows",
];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    #[serde(default)]
    pub names: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Adapter {
    #[serde(default)]
    pub models: Vec<Model>,
}
pub type Catalogue = BTreeMap<String, Adapter>;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(from = "Value")]
pub struct Price {
    pub billing: Option<String>,
    pub price_status: Option<String>,
    #[serde(default)]
    pub usd_per_million: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
    #[serde(skip)]
    #[doc(hidden)]
    pub populated: bool,
}
impl From<Value> for Price {
    fn from(value: Value) -> Self {
        let mut fields: BTreeMap<String, Value> = value
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let populated = !fields.is_empty();
        Self {
            billing: fields
                .remove("billing")
                .and_then(|v| v.as_str().map(str::to_owned)),
            price_status: fields
                .remove("price_status")
                .and_then(|v| v.as_str().map(str::to_owned)),
            usd_per_million: fields
                .remove("usd_per_million")
                .and_then(|v| {
                    v.as_object()
                        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                })
                .unwrap_or_default(),
            extra: fields,
            populated,
        }
    }
}
impl Price {
    fn is_empty(&self) -> bool {
        !self.populated
            && self.billing.is_none()
            && self.price_status.is_none()
            && self.usd_per_million.is_empty()
            && self.extra.is_empty()
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Prices {
    #[serde(default)]
    pub models: BTreeMap<String, Price>,
    #[serde(default)]
    pub free_patterns: BTreeMap<String, Vec<String>>,
}
impl Prices {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = read_bounded(path)?;
        crate::decode_json(&bytes)
    }
    pub fn entry(&self, row: &Usage) -> Result<Price> {
        let (adapter, model) = (
            row.adapter.as_deref().unwrap_or("None"),
            row.model.as_deref().unwrap_or("None"),
        );
        let provider = row.provider.as_deref().unwrap_or("None");
        if let Some(entry) = self
            .models
            .get(&format!("{adapter}/{model}"))
            .filter(|p| !p.is_empty())
            .or_else(|| {
                self.models
                    .get(&format!("{adapter}/{provider}/{model}"))
                    .filter(|p| !p.is_empty())
            })
        {
            return Ok(entry.clone());
        }
        for pattern in self.free_patterns.get(adapter).into_iter().flatten() {
            let regex = Regex::new(pattern).map_err(|e| crate::Error::Invalid(e.to_string()))?;
            if regex.is_match(model) {
                return Ok(Price {
                    billing: Some("free".into()),
                    price_status: Some("free".into()),
                    ..Price::default()
                });
            }
        }
        Ok(Price::default())
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|source| crate::Error::Io {
        path: path.into(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take((crate::MAX_JSON_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| crate::Error::Io {
            path: path.into(),
            source,
        })?;
    if bytes.len() > crate::MAX_JSON_BYTES {
        return Err(crate::Error::InputTooLarge {
            limit: crate::MAX_JSON_BYTES,
        });
    }
    Ok(bytes)
}
pub fn catalogue(path: &Path) -> Catalogue {
    let Some(value) = read_bounded(path)
        .ok()
        .and_then(|b| crate::decode_json::<Value>(&b).ok())
    else {
        return Catalogue::new();
    };
    value["adapters"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, adapter)| {
            let models = adapter["models"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let id = entry["id"].as_str()?.to_owned();
                    let names = entry["names"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    Some(Model { id, names })
                })
                .collect();
            (name.clone(), Adapter { models })
        })
        .collect()
}
pub fn normalise_model(
    adapter: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
    adapters: &Catalogue,
) -> Option<String> {
    let mut model = crate::pending::python_strip(model?).to_owned();
    if model.is_empty() {
        return None;
    }
    let entries = adapters
        .get(adapter.unwrap_or(""))
        .map(|a| a.models.as_slice())
        .unwrap_or(&[]);
    let ids: BTreeSet<_> = entries.iter().map(|e| e.id.as_str()).collect();
    if adapter == Some("opencode") {
        if let Some(provider) = provider.filter(|p| !p.is_empty()) {
            let namespaces: BTreeSet<_> = ids
                .iter()
                .filter_map(|s| s.split('/').next())
                .chain([provider])
                .collect();
            if !namespaces.contains(model.split('/').next().unwrap_or("")) {
                model = format!("{provider}/{model}");
            }
        }
    }
    if ids.contains(model.as_str()) {
        return Some(model);
    }
    let mut aliases = BTreeMap::new();
    for entry in entries {
        for name in &entry.names {
            if crate::pending::python_alpha(name) {
                aliases.insert(name.to_lowercase(), entry.id.clone());
            }
        }
    }
    if let Some(id) = aliases.get(&model.to_lowercase()) {
        return Some(id.clone());
    }
    let mut candidate = String::new();
    let mut dash = false;
    for c in model.to_lowercase().chars() {
        if crate::pending::python_whitespace(c) || c == '-' {
            if !dash {
                candidate.push('-');
            }
            dash = true;
        } else {
            candidate.push(c);
            dash = false;
        }
    }
    if ids.contains(candidate.as_str()) {
        return Some(candidate);
    }
    // The Python substitutions are cumulative, in this exact order.
    for kind in [0, 1, 2] {
        if let Some((prefix, suffix)) = candidate.rsplit_once('-') {
            let remove = match kind {
                0 => {
                    suffix.chars().count() == 8
                        && suffix
                            .chars()
                            .all(|c| crate::pending::decimal_digit(c).is_some())
                }
                1 => matches!(
                    suffix,
                    "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                ),
                _ => suffix.strip_suffix('k').is_some_and(|n| {
                    !n.is_empty()
                        && n.chars()
                            .all(|c| crate::pending::decimal_digit(c).is_some())
                }),
            };
            if remove {
                candidate = prefix.into();
            }
        }
        if ids.contains(candidate.as_str()) {
            return Some(candidate);
        }
    }
    Some(model)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exclusion {
    TestFixture,
    Unattributed,
}
pub fn classify(row: &Usage, raw: Option<&str>) -> Option<Exclusion> {
    let tail = raw.unwrap_or("").rsplit('/').next().unwrap_or("");
    if tail == "fixture" || tail.contains("nonexistent") {
        Some(Exclusion::TestFixture)
    } else if row.adapter.as_deref().is_none_or(str::is_empty)
        || row
            .model
            .as_deref()
            .is_none_or(|s| s.is_empty() || s == "<synthetic>")
    {
        Some(Exclusion::Unattributed)
    } else {
        None
    }
}
pub fn tokens(row: &Usage) -> [Option<Numeric>; 5] {
    [
        row.input_tokens,
        row.cache_read_tokens,
        row.cache_write_tokens,
        row.output_tokens,
        row.reasoning_tokens,
    ]
}
pub fn estimated_cost(row: &Usage, entry: &Price) -> Option<f64> {
    if entry.billing.as_deref() == Some("free") {
        return Some(0.0);
    }
    let mut total = 0.0;
    for (index, (count, rate)) in tokens(row)
        .into_iter()
        .zip(["input", "cached_input", "cache_write", "output", "output"])
        .enumerate()
    {
        if count.is_none() && index == 4 {
            continue;
        }
        let count = count?.as_f64();
        let price = entry
            .usd_per_million
            .get(rate)
            .and_then(number)
            .map(Numeric::as_f64);
        if count > 0.0 && price.is_none() {
            return None;
        }
        total += count * price.unwrap_or(0.0) / 1_000_000.0;
    }
    Some(total)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TokenCounts {
    pub input_tokens: Numeric,
    pub cache_read_tokens: Numeric,
    pub cache_write_tokens: Numeric,
    pub output_tokens: Numeric,
    pub reasoning_tokens: Numeric,
}
impl TokenCounts {
    pub fn array(&self) -> [Numeric; 5] {
        [
            self.input_tokens,
            self.cache_read_tokens,
            self.cache_write_tokens,
            self.output_tokens,
            self.reasoning_tokens,
        ]
    }
    pub fn from_array(a: [Numeric; 5]) -> Self {
        Self {
            input_tokens: a[0],
            cache_read_tokens: a[1],
            cache_write_tokens: a[2],
            output_tokens: a[3],
            reasoning_tokens: a[4],
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Group {
    #[serde(flatten)]
    pub dimensions: OrderedFields<Value>,
    pub rows: Numeric,
    #[serde(flatten)]
    pub tokens: TokenCounts,
    pub missing_token_fields: OrderedFields<Numeric>,
    pub missing_token_rows: Numeric,
    pub cache_share_rows: Numeric,
    pub cache_share_input: Numeric,
    pub cache_share_read: Numeric,
    pub known_cost_usd: f64,
    pub missing_cost_rows: Numeric,
    pub reported_cost_usd: Option<f64>,
    pub model_raw: Vec<String>,
    pub billing: Vec<String>,
}
pub fn unit(
    row: &Usage,
    raw: Option<&str>,
    dimensions: impl Into<OrderedFields<Value>>,
    prices: &Prices,
) -> Result<Group> {
    let entry = prices.entry(row)?;
    let cost = estimated_cost(row, &entry);
    let values = tokens(row);
    let known = values[..3].iter().all(Option::is_some);
    Ok(Group {
        dimensions: dimensions.into(),
        rows: Numeric::ONE,
        tokens: TokenCounts::from_array(values.map(|n| n.unwrap_or(Numeric::ZERO))),
        missing_token_fields: OrderedFields::from_iter(
            TOKEN_FIELDS
                .into_iter()
                .zip(values.map(|n| Numeric::from(i32::from(n.is_none()))))
                .map(|(k, v)| (k.into(), v)),
        ),
        missing_token_rows: Numeric::from(i32::from(values.iter().any(Option::is_none))),
        cache_share_rows: Numeric::from(i32::from(known)),
        cache_share_input: if known {
            values[..3]
                .iter()
                .flatten()
                .try_fold(Numeric::ZERO, |a, b| a.checked_add(*b))?
        } else {
            Numeric::ZERO
        },
        cache_share_read: if known {
            row.cache_read_tokens.unwrap_or(Numeric::ZERO)
        } else {
            Numeric::ZERO
        },
        known_cost_usd: cost.unwrap_or(0.0),
        missing_cost_rows: Numeric::from(i32::from(cost.is_none())),
        reported_cost_usd: row.reported_cost,
        model_raw: raw
            .filter(|s| !s.is_empty())
            .into_iter()
            .map(str::to_owned)
            .collect(),
        billing: entry
            .billing
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect(),
    })
}

/// Build a summable row group with output dimensions in the caller's key order.
/// The plain `unit` entry point remains convenient for callers without an
/// explicit report key list.
pub fn unit_ordered(
    row: &Usage,
    raw: Option<&str>,
    dimensions: impl Into<OrderedFields<Value>>,
    keys: &[&str],
    prices: &Prices,
) -> Result<Group> {
    let dimensions = dimensions.into();
    let requested = keys.iter().copied().collect::<BTreeSet<_>>();
    let mut ordered = OrderedFields::from_keys(keys, |key| {
        dimensions.get(key).cloned().unwrap_or(Value::Null)
    });
    for (key, value) in dimensions.into_iter() {
        if !requested.contains(key.as_str()) {
            ordered.insert(key, value);
        }
    }
    unit(row, raw, ordered, prices)
}
fn key(group: &Group, keys: &[&str]) -> Vec<Value> {
    keys.iter()
        .map(|k| group.dimensions.get(k).cloned().unwrap_or(Value::Null))
        .collect()
}
fn sort_key(group: &Group, keys: &[&str]) -> Vec<String> {
    key(group, keys)
        .iter()
        .map(|v| {
            if crate::pending::python_truthy(v) {
                crate::pending::python_str(v)
            } else {
                String::new()
            }
        })
        .collect()
}
pub fn combine(groups: impl IntoIterator<Item = Group>, keys: &[&str]) -> Result<Vec<Group>> {
    // Vec preserves first insertion for tied display keys, including None versus empty string.
    let mut merged: Vec<Group> = Vec::new();
    for mut group in groups {
        let k = key(&group, keys);
        if let Some(target) = merged.iter_mut().find(|g| {
            key(g, keys)
                .iter()
                .zip(&k)
                .all(|(a, b)| crate::pending::python_equal(a, b))
        }) {
            target.rows = target.rows.checked_add(group.rows)?;
            let mut sums = [Numeric::ZERO; 5];
            for (i, sum) in sums.iter_mut().enumerate() {
                *sum = target.tokens.array()[i].checked_add(group.tokens.array()[i])?;
            }
            target.tokens = TokenCounts::from_array(sums);
            target.missing_token_rows = target
                .missing_token_rows
                .checked_add(group.missing_token_rows)?;
            target.cache_share_rows = target
                .cache_share_rows
                .checked_add(group.cache_share_rows)?;
            target.cache_share_input = target
                .cache_share_input
                .checked_add(group.cache_share_input)?;
            target.cache_share_read = target
                .cache_share_read
                .checked_add(group.cache_share_read)?;
            target.known_cost_usd += group.known_cost_usd;
            target.missing_cost_rows = target
                .missing_cost_rows
                .checked_add(group.missing_cost_rows)?;
            for (field, count) in group.missing_token_fields.into_iter() {
                if let Some(target) = target.missing_token_fields.get_mut(&field) {
                    *target = target.checked_add(count)?;
                } else {
                    target.missing_token_fields.insert(field, count);
                }
            }
            if let Some(cost) = group.reported_cost_usd {
                target.reported_cost_usd = Some(target.reported_cost_usd.unwrap_or(0.0) + cost);
            }
            target.model_raw.extend(group.model_raw);
            target.billing.extend(group.billing);
        } else {
            let requested = keys.iter().copied().collect::<BTreeSet<_>>();
            group.dimensions = OrderedFields::from_iter(
                keys.iter()
                    .zip(k)
                    .map(|(name, value)| ((*name).into(), value))
                    .chain(
                        group
                            .dimensions
                            .into_iter()
                            .filter(|(name, _)| !requested.contains(name.as_str())),
                    ),
            );
            merged.push(group);
        }
    }
    for group in &mut merged {
        group.model_raw.sort();
        group.model_raw.dedup();
        group.billing.sort();
        group.billing.dedup();
    }
    merged.sort_by_key(|g| sort_key(g, keys));
    Ok(merged)
}
/// Round at the output edge using Python's decimal rounding of the binary float.
pub fn round(value: f64, digits: usize) -> f64 {
    format!("{value:.digits$}").parse().unwrap_or(value)
}
pub fn micro(value: Option<f64>) -> Option<f64> {
    value.map(|n| round(n, 6))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FinalGroup {
    #[serde(flatten)]
    pub dimensions: OrderedFields<Value>,
    pub rows: Numeric,
    pub input_tokens: Option<Numeric>,
    pub cache_read_tokens: Option<Numeric>,
    pub cache_write_tokens: Option<Numeric>,
    pub output_tokens: Option<Numeric>,
    pub reasoning_tokens: Option<Numeric>,
    pub missing_token_rows: Numeric,
    pub missing_token_fields: OrderedFields<Numeric>,
    pub cache_read_share: Option<f64>,
    pub cache_share_rows: Numeric,
    pub estimated_cost_usd: Option<f64>,
    pub missing_cost_rows: Numeric,
    pub reported_cost_usd: Option<f64>,
    pub cost_basis: String,
    pub billing: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_raw: Option<Vec<String>>,
}

/// One non-receipt SQL group. Token sums stand for every row in the group;
/// null and positive flags retain the per-row facts needed by `cell_cost`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageCell {
    pub dimensions: BTreeMap<String, Value>,
    pub usage: Usage,
    pub rows: Numeric,
    pub null_tokens: [bool; 5],
    pub positive_tokens: [bool; 5],
    pub first_row: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NativeUsage {
    pub adapter: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub session: Option<String>,
    pub timestamp: Option<String>,
}

/// SQL and named parameters passed to the ledger adapter. The adapter owns the
/// connection and translates SQLite values into the JSON value representation.
#[derive(Clone, Debug, PartialEq)]
pub struct SqlCellQuery {
    pub sql: String,
    pub parameters: BTreeMap<String, Value>,
}

pub type SqlCellRow = BTreeMap<String, Value>;

/// Store seam for usage-cell aggregation. It keeps SQL selection in this
/// policy owner while the ledger owner controls connection lifetime and errors.
pub trait UsageCellStore {
    fn query_rows(&mut self, query: &SqlCellQuery) -> Result<Vec<SqlCellRow>>;
}

fn sql_parameter(value: &Value) -> Result<rusqlite::types::Value> {
    use rusqlite::types::Value as SqlValue;
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                SqlValue::Integer(integer)
            } else if let Some(integer) = value.as_u64() {
                SqlValue::Integer(
                    i64::try_from(integer).map_err(|_| crate::Error::NumberOutOfRange)?,
                )
            } else {
                SqlValue::Real(value.as_f64().ok_or_else(|| {
                    crate::Error::Invalid("invalid numeric usage query parameter".into())
                })?)
            }
        }
        Value::String(value) => SqlValue::Text(value.clone()),
        Value::Array(_) | Value::Object(_) => {
            return Err(crate::Error::Invalid(
                "usage query parameter must be a scalar".into(),
            ));
        }
    })
}

impl UsageCellStore for rusqlite::Connection {
    fn query_rows(&mut self, query: &SqlCellQuery) -> Result<Vec<SqlCellRow>> {
        use rusqlite::types::ValueRef;

        let mut statement = self.prepare(&query.sql)?;
        let mut bound = BTreeSet::new();
        for (name, value) in &query.parameters {
            let name = format!(":{name}");
            if let Some(index) = statement.parameter_index(&name)? {
                statement.raw_bind_parameter(index, sql_parameter(value)?)?;
                bound.insert(index);
            }
        }
        if bound.len() != statement.parameter_count() {
            return Err(crate::Error::Invalid(
                "usage query has an unbound or positional parameter".into(),
            ));
        }
        let names = statement
            .column_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        let mut rows = statement.raw_query();
        let mut records = Vec::new();
        while let Some(row) = rows.next()? {
            let mut record = SqlCellRow::new();
            for (index, name) in names.iter().enumerate() {
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => Value::from(value),
                    ValueRef::Real(value) => {
                        Value::Number(serde_json::Number::from_f64(value).ok_or_else(|| {
                            crate::Error::Invalid("SQLite returned a non-finite number".into())
                        })?)
                    }
                    ValueRef::Text(value) => Value::String(
                        std::str::from_utf8(value)
                            .map_err(|_| {
                                crate::Error::Invalid("SQLite returned non-UTF-8 text".into())
                            })?
                            .into(),
                    ),
                    ValueRef::Blob(_) => {
                        return Err(crate::Error::Invalid(
                            "SQLite returned a blob for a usage cell".into(),
                        ));
                    }
                };
                record.insert(name.clone(), value);
            }
            records.push(record);
        }
        Ok(records)
    }
}

fn cell_field<'a>(row: &'a SqlCellRow, name: &str) -> Result<&'a Value> {
    row.get(name)
        .ok_or_else(|| crate::Error::Invalid(format!("usage cell missing {name}")))
}

fn cell_text(row: &SqlCellRow, name: &str) -> Result<Option<String>> {
    match cell_field(row, name)? {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value.clone())),
        _ => Err(crate::Error::Invalid(format!(
            "usage cell {name} is not text"
        ))),
    }
}

fn cell_integer(row: &SqlCellRow, name: &str) -> Result<Numeric> {
    let value = cell_field(row, name)?;
    let Some(number) = value.as_number() else {
        return Err(crate::Error::Invalid(format!(
            "usage cell {name} is not an integer"
        )));
    };
    if let Some(integer) = number.as_i64() {
        Ok(Numeric::from(integer))
    } else if let Some(integer) = number.as_u64() {
        Ok(Numeric::from(integer))
    } else {
        Err(crate::Error::Invalid(format!(
            "usage cell {name} is not an integer"
        )))
    }
}

fn cell_flag(row: &SqlCellRow, name: &str) -> Result<bool> {
    match cell_integer(row, name)? {
        Numeric::Integer(0) => Ok(false),
        Numeric::Integer(1) => Ok(true),
        _ => Err(crate::Error::Invalid(format!(
            "usage cell {name} is not a boolean flag"
        ))),
    }
}

pub fn cell_cost(cell: &UsageCell, entry: &Price) -> Option<f64> {
    if entry.billing.as_deref() == Some("free") {
        return Some(0.0);
    }
    let rates = &entry.usd_per_million;
    let mut total = 0.0;
    let values = tokens(&cell.usage);
    for (index, rate) in ["input", "cached_input", "cache_write", "output", "output"]
        .into_iter()
        .enumerate()
    {
        if cell.null_tokens[index] {
            if index == 4 {
                continue;
            }
            return None;
        }
        let count = values[index].unwrap_or(Numeric::ZERO).as_f64();
        let price = rates.get(rate).and_then(number).map(Numeric::as_f64);
        if cell.positive_tokens[index] && price.is_none() {
            return None;
        }
        total += count * price.unwrap_or(0.0) / 1_000_000.0;
    }
    Some(total)
}

/// Build the summable group represented by one SQL cell.
pub fn cell_unit(cell: &UsageCell, keys: &[&str], prices: &Prices) -> Result<Group> {
    let entry = prices.entry(&cell.usage)?;
    let cost = cell_cost(cell, &entry);
    let count = cell.rows;
    let known = !cell.null_tokens[..3].iter().any(|value| *value);
    let values = tokens(&cell.usage);
    let cache_share_input = if known {
        values[..3]
            .iter()
            .flatten()
            .try_fold(Numeric::ZERO, |sum, value| sum.checked_add(*value))?
    } else {
        Numeric::ZERO
    };
    let missing = |index: usize| {
        if cell.null_tokens[index] {
            count
        } else {
            Numeric::ZERO
        }
    };
    Ok(Group {
        dimensions: OrderedFields::from_keys(keys, |key| {
            cell.dimensions.get(key).cloned().unwrap_or(Value::Null)
        }),
        rows: count,
        tokens: TokenCounts::from_array(values.map(|value| value.unwrap_or(Numeric::ZERO))),
        missing_token_fields: OrderedFields::from_iter(
            TOKEN_FIELDS
                .into_iter()
                .enumerate()
                .map(|(index, field)| (field.into(), missing(index))),
        ),
        missing_token_rows: if cell.null_tokens.iter().any(|value| *value) {
            count
        } else {
            Numeric::ZERO
        },
        cache_share_rows: if known { count } else { Numeric::ZERO },
        cache_share_input,
        cache_share_read: if known {
            values[1].unwrap_or(Numeric::ZERO)
        } else {
            Numeric::ZERO
        },
        known_cost_usd: cost.unwrap_or(0.0),
        missing_cost_rows: if cost.is_none() { count } else { Numeric::ZERO },
        reported_cost_usd: cell.usage.reported_cost,
        model_raw: cell
            .usage
            .model_raw
            .as_ref()
            .filter(|model| !model.is_empty())
            .cloned()
            .into_iter()
            .collect(),
        billing: entry
            .billing
            .filter(|billing| !billing.is_empty())
            .into_iter()
            .collect(),
    })
}

fn utc_day(moment: jiff::Timestamp) -> Result<String> {
    let zoned = moment.to_zoned(jiff::tz::TimeZone::UTC);
    Ok(zoned.strftime("%Y-%m-%d").to_string())
}

fn cell_from_row(row: &SqlCellRow, summary: bool) -> Result<UsageCell> {
    let mut dimensions = BTreeMap::new();
    for name in [
        "source", "adapter", "provider", "model", "account", "status",
    ] {
        let value = cell_text(row, name)?;
        dimensions.insert(name.into(), value.map(Value::String).unwrap_or(Value::Null));
    }
    if summary {
        for name in ["host", "session"] {
            let value = cell_text(row, name)?;
            dimensions.insert(name.into(), value.map(Value::String).unwrap_or(Value::Null));
        }
    }
    let timestamp = cell_text(row, "timestamp")?;
    dimensions.insert(
        "timestamp".into(),
        timestamp.clone().map(Value::String).unwrap_or(Value::Null),
    );
    let source = cell_text(row, "source")?
        .ok_or_else(|| crate::Error::Invalid("usage cell source is null".into()))?;
    let adapter = cell_text(row, "adapter")?;
    let provider = cell_text(row, "provider")?;
    let model = cell_text(row, "model")?;
    let account = cell_text(row, "account")?;
    let status = cell_text(row, "status")?;
    let host = if summary {
        cell_text(row, "host")?
            .ok_or_else(|| crate::Error::Invalid("usage cell host is null".into()))?
    } else {
        String::new()
    };
    let session = if summary {
        cell_text(row, "session")?
    } else {
        None
    };
    let mut sums = [Numeric::ZERO; 5];
    let mut null_tokens = [false; 5];
    let mut positive_tokens = [false; 5];
    for (index, field) in TOKEN_FIELDS.iter().enumerate() {
        sums[index] = cell_integer(row, field)?;
        null_tokens[index] = cell_flag(row, &format!("null_{field}"))?;
        positive_tokens[index] = cell_flag(row, &format!("positive_{field}"))?;
    }
    let reported_cost = match cell_field(row, "reported_cost")? {
        Value::Null => None,
        value => Some(value.as_f64().ok_or_else(|| {
            crate::Error::Invalid("usage cell reported_cost is not numeric".into())
        })?),
    };
    let rows = cell_integer(row, "rows")?;
    let first_row = cell_text(row, "first_row")?
        .ok_or_else(|| crate::Error::Invalid("usage cell first_row is null".into()))?;
    let usage = Usage {
        source,
        host,
        session,
        adapter,
        provider,
        model_raw: model.clone(),
        model,
        account,
        timestamp,
        input_tokens: Some(sums[0]),
        cache_read_tokens: Some(sums[1]),
        cache_write_tokens: Some(sums[2]),
        output_tokens: Some(sums[3]),
        reasoning_tokens: Some(sums[4]),
        reported_cost,
        status,
        ..Usage::default()
    };
    Ok(UsageCell {
        dimensions,
        usage,
        rows,
        null_tokens,
        positive_tokens,
        first_row,
    })
}

fn native_from_row(row: &SqlCellRow) -> Result<NativeUsage> {
    Ok(NativeUsage {
        adapter: cell_text(row, "adapter")?,
        provider: cell_text(row, "provider")?,
        model: cell_text(row, "model")?,
        session: cell_text(row, "session")?,
        timestamp: cell_text(row, "timestamp")?,
    })
}

/// Aggregate non-receipt usage in SQLite without materialising individual ledger
/// rows. SQL values and result decoding cross the store boundary through typed
/// records; source order remains the caller-provided `order` expression.
#[allow(clippy::too_many_arguments)]
pub fn usage_cells<S: UsageCellStore>(
    store: &mut S,
    where_clause: &str,
    parameters: &BTreeMap<String, Value>,
    summary: bool,
    boundary: Option<jiff::Timestamp>,
    end: Option<jiff::Timestamp>,
    need_native: bool,
    order: &str,
    current: &str,
    aliases: &BTreeSet<String>,
) -> Result<(Vec<UsageCell>, Vec<NativeUsage>)> {
    let mut parameters = parameters.clone();
    parameters.insert(
        "host_identity_current".into(),
        Value::String(current.into()),
    );
    let mut alias_parameters = Vec::new();
    for (index, alias) in aliases.iter().enumerate() {
        let name = format!("host_identity_alias_{index}");
        parameters.insert(name.clone(), Value::String(alias.clone()));
        alias_parameters.push(format!(":{name}"));
    }
    let host_condition = format!("host IN ({})", alias_parameters.join(", "));
    let host_expression =
        format!("CASE WHEN {host_condition} THEN :host_identity_current ELSE host END");
    let mut simple = "timestamp GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T*' AND (timestamp LIKE '%Z' OR timestamp LIKE '%+00:00')".to_owned();
    for (name, moment, operator) in [("low", boundary, ">"), ("high", end, "<")] {
        if let Some(moment) = moment {
            simple.push_str(&format!(" AND substr(timestamp,1,10) {operator} :{name}"));
            parameters.insert(name.into(), Value::String(utc_day(moment)?));
        }
    }
    let stamp = format!("CASE WHEN {simple} THEN substr(timestamp,1,10) ELSE timestamp END");
    let filtered_where = format!(" WHERE source!='fabric'{where_clause}");
    let mut columns = vec![
        "source", "adapter", "provider", "model", "account", "status",
    ];
    let mut group_columns = columns.clone();
    if summary {
        columns.extend(["local_host AS host", "session"]);
        group_columns.extend(["local_host", "session"]);
    }
    let flags = TOKEN_FIELDS
        .iter()
        .map(|field| {
            format!("{field} IS NULL AS null_{field}, COALESCE({field}>0,0) AS positive_{field}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let sums = TOKEN_FIELDS
        .iter()
        .map(|field| format!("SUM(COALESCE({field},0)) AS {field}"))
        .collect::<Vec<_>>()
        .join(", ");
    let ranked = format!(
        "WITH ranked_usage AS (SELECT usage.*, rowid AS ledger_rowid, {host_expression} AS local_host, ROW_NUMBER() OVER (PARTITION BY source, event_id, {host_expression} ORDER BY CASE WHEN host=:host_identity_current THEN 0 ELSE 1 END, rowid) AS alias_rank FROM usage{filtered_where}) "
    );
    let group_by = group_columns.join(", ");
    let null_groups = TOKEN_FIELDS
        .iter()
        .flat_map(|field| [format!("null_{field}"), format!("positive_{field}")])
        .collect::<Vec<_>>()
        .join(", ");
    let query = SqlCellQuery {
        sql: format!(
            "{ranked}SELECT {}, {stamp} AS timestamp, {flags}, {sums}, SUM(reported_cost) AS reported_cost, COUNT(*) AS rows, MIN({order}) AS first_row FROM ranked_usage WHERE alias_rank=1 GROUP BY {group_by}, {stamp}, {null_groups}",
            columns.join(", ")
        ),
        parameters: parameters.clone(),
    };
    let cells = store
        .query_rows(&query)?
        .iter()
        .map(|row| cell_from_row(row, summary))
        .collect::<Result<Vec<_>>>()?;
    let native = if need_native {
        let query = SqlCellQuery {
            sql: format!(
                "{ranked}SELECT DISTINCT adapter, provider, model, session, {stamp} AS timestamp FROM ranked_usage WHERE alias_rank=1 AND session IS NOT NULL AND session!=''"
            ),
            parameters,
        };
        store
            .query_rows(&query)?
            .iter()
            .map(native_from_row)
            .collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    Ok((cells, native))
}

pub fn finalise(groups: Vec<Group>, keys: &[&str]) -> Vec<FinalGroup> {
    groups
        .into_iter()
        .map(|g| {
            let a = g.tokens.array();
            let token = |i| {
                if g.missing_token_fields
                    .get(TOKEN_FIELDS[i])
                    .copied()
                    .unwrap_or(Numeric::ZERO)
                    == g.rows
                {
                    None
                } else {
                    Some(a[i])
                }
            };
            FinalGroup {
                dimensions: OrderedFields::from_keys(keys, |key| {
                    g.dimensions.get(key).cloned().unwrap_or(Value::Null)
                }),
                rows: g.rows,
                input_tokens: token(0),
                cache_read_tokens: token(1),
                cache_write_tokens: token(2),
                output_tokens: token(3),
                reasoning_tokens: token(4),
                missing_token_rows: g.missing_token_rows,
                missing_token_fields: g.missing_token_fields,
                cache_read_share: if !g.cache_share_input.is_zero() {
                    Some(g.cache_share_read.as_f64() / g.cache_share_input.as_f64())
                } else {
                    None
                },
                cache_share_rows: g.cache_share_rows,
                estimated_cost_usd: if g.missing_cost_rows.is_zero() {
                    micro(Some(g.known_cost_usd))
                } else {
                    None
                },
                missing_cost_rows: g.missing_cost_rows,
                reported_cost_usd: micro(g.reported_cost_usd),
                cost_basis: "api_equivalent_estimate".into(),
                billing: if g.billing.len() == 1 {
                    g.billing.first().cloned()
                } else if g.billing.is_empty() {
                    None
                } else {
                    Some("mixed".into())
                },
                model_raw: if keys.contains(&"model") {
                    Some(g.model_raw)
                } else {
                    None
                },
            }
        })
        .collect()
}
pub fn aggregate(rows: &[Usage], keys: &[&str], prices: &Prices) -> Result<Vec<FinalGroup>> {
    let mut groups = Vec::new();
    for row in rows {
        let value = serde_json::to_value(row)?;
        groups.push(unit_ordered(
            row,
            row.model_raw.as_deref(),
            OrderedFields::from_keys(keys, |key| value.get(key).cloned().unwrap_or(Value::Null)),
            keys,
            prices,
        )?);
    }
    Ok(finalise(combine(groups, keys)?, keys))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missingness_reasoning_and_free_routes() {
        let mut row = Usage {
            adapter: Some("a".into()),
            model: Some("m".into()),
            input_tokens: Some(100.into()),
            cache_read_tokens: Some(0.into()),
            cache_write_tokens: Some(0.into()),
            output_tokens: Some(10.into()),
            ..Usage::default()
        };
        let price = Price {
            usd_per_million: BTreeMap::from([
                ("input".into(), serde_json::json!(2)),
                ("output".into(), serde_json::json!(4)),
            ]),
            ..Price::default()
        };
        assert_eq!(estimated_cost(&row, &price), Some(0.00024));
        row.reasoning_tokens = Some(5.into());
        assert_eq!(estimated_cost(&row, &price), Some(0.00026000000000000003));
        row.cache_read_tokens = None;
        assert_eq!(estimated_cost(&row, &price), None);
        assert_eq!(
            estimated_cost(
                &row,
                &Price {
                    billing: Some("free".into()),
                    ..Price::default()
                }
            ),
            Some(0.0)
        );
    }
    #[test]
    fn combine_keeps_unknown_fields_null_and_rounds_once() {
        let row = Usage::default();
        let group = unit(&row, None, BTreeMap::new(), &Prices::default()).unwrap();
        let finalised = finalise(combine([group.clone(), group], &[]).unwrap(), &[]);
        assert_eq!(finalised[0].input_tokens, None);
        assert_eq!(finalised[0].missing_token_rows, Numeric::from(2));
        assert_eq!(finalised[0].estimated_cost_usd, None);
        let row = Usage {
            input_tokens: Some(1.into()),
            cache_read_tokens: Some(0.into()),
            cache_write_tokens: Some(0.into()),
            output_tokens: Some(0.into()),
            reported_cost: Some(0.0000004),
            ..Usage::default()
        };
        let price = Price {
            usd_per_million: BTreeMap::from([("input".into(), serde_json::json!(0.4))]),
            ..Price::default()
        };
        let mut a = unit(
            &row,
            Some("Z"),
            BTreeMap::from([("model".into(), serde_json::json!("b"))]),
            &Prices::default(),
        )
        .unwrap();
        a.known_cost_usd = estimated_cost(&row, &price).unwrap();
        a.missing_cost_rows = Numeric::ZERO;
        let mut b = a.clone();
        b.model_raw = vec!["A".into()];
        let mut c = a.clone();
        c.dimensions.insert("model".into(), serde_json::json!("a"));
        let groups = finalise(combine([a, b, c], &["model"]).unwrap(), &["model"]);
        assert_eq!(groups[0].dimensions["model"], "a");
        assert_eq!(groups[1].dimensions["model"], "b");
        assert_eq!(groups[1].model_raw.as_ref().unwrap(), &["A", "Z"]);
        assert_eq!(groups[1].estimated_cost_usd, Some(0.000001));
        assert_eq!(groups[1].reported_cost_usd, Some(0.000001));
    }

    #[test]
    fn final_group_json_bytes_keep_requested_and_token_field_order() {
        let row = Usage {
            source: "ledger".into(),
            event_id: "e1".into(),
            host: "host-z".into(),
            adapter: Some("adapter-a".into()),
            model: Some("model-a".into()),
            input_tokens: Some(3.into()),
            output_tokens: Some(1.into()),
            ..Usage::default()
        };
        let keys = ["host", "model", "adapter"];
        let prices = Prices::default();
        let actual = serde_json::to_string_pretty(
            &aggregate(std::slice::from_ref(&row), &keys, &prices).unwrap(),
        )
        .unwrap();
        let input = serde_json::json!({
            "rows": [serde_json::to_value(&row).unwrap()],
            "keys": keys,
            "prices": prices.models,
        });
        let source = format!(
            "{}\nrows=costs.aggregate(t,input['rows'],input['keys'],input['prices'])\nprint(json.dumps(json.dumps(rows,indent=2)))",
            crate::differential::LOAD
        );
        let Some(expected) = crate::differential::python(&source, &input) else {
            return;
        };
        assert_eq!(actual, expected.as_str().unwrap());
    }
    #[test]
    fn normalisation_preserves_versioned_models() {
        let catalogue = BTreeMap::from([(
            "claude".into(),
            Adapter {
                models: vec![Model {
                    id: "claude-opus-4".into(),
                    names: vec!["Opus".into()],
                }],
            },
        )]);
        assert_eq!(
            normalise_model(Some("claude"), None, Some("opus"), &catalogue),
            Some("claude-opus-4".into())
        );
        assert_eq!(
            normalise_model(
                Some("claude"),
                None,
                Some("CLAUDE OPUS 4-high-20260101"),
                &catalogue
            ),
            Some("claude-opus-4".into())
        );
        assert_eq!(
            normalise_model(Some("claude"), None, Some("claude-opus-4.1"), &catalogue),
            Some("claude-opus-4.1".into())
        );
        assert_eq!(
            normalise_model(
                Some("opencode"),
                Some("provider"),
                Some("model"),
                &catalogue
            ),
            Some("provider/model".into())
        );
    }
    #[test]
    fn cache_share_only_counts_complete_input_rows() {
        let row = Usage {
            input_tokens: Some(100.into()),
            cache_read_tokens: Some(10.into()),
            ..Usage::default()
        };
        let g = unit(&row, None, BTreeMap::new(), &Prices::default()).unwrap();
        assert_eq!(g.cache_share_rows, Numeric::ZERO);
        assert_eq!(g.cache_share_input, Numeric::ZERO);
    }
    #[test]
    fn review_regression_empty_prices_fall_through_and_null_rates_work() {
        let prices: Prices = serde_json::from_value(serde_json::json!({"models":{"a/m":{},"a/p/m":{"billing":"free","usd_per_million":null}}})).unwrap();
        let row = Usage {
            adapter: Some("a".into()),
            provider: Some("p".into()),
            model: Some("m".into()),
            ..Usage::default()
        };
        assert_eq!(prices.entry(&row).unwrap().billing.as_deref(), Some("free"));
        let prices: Prices = serde_json::from_value(
            serde_json::json!({"models":{"a/m":{},"a/p/m":{}},"free_patterns":{"a":["^m$"]}}),
        )
        .unwrap();
        assert_eq!(prices.entry(&row).unwrap().billing.as_deref(), Some("free"));
        let prices: Prices = serde_json::from_value(
            serde_json::json!({"models":{"a/m":{"billing":null},"a/p/m":{"billing":"free"}}}),
        )
        .unwrap();
        assert_eq!(prices.entry(&row).unwrap().billing, None);
    }
    #[test]
    fn review_regression_bad_catalogue_entries_keep_usable_models() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalogue.json");
        std::fs::write(&path, r#"{"adapters":{"a":{"models":[{}, {"id":"m","names":[null,3,"Alias"]}]},"b":{"models":[{"id":"other"}]}}}"#).unwrap();
        let table = catalogue(&path);
        assert_eq!(
            normalise_model(Some("a"), None, Some("Alias"), &table).as_deref(),
            Some("m")
        );
        assert_eq!(table["b"].models[0].id, "other");
    }
    #[test]
    fn review_regression_aggregate_keeps_pre_normalisation_model() {
        let row: Usage = serde_json::from_value(serde_json::json!({"source":"s","event_id":"e","host":"h","adapter":"a","model":"normalised","model_raw":"Original Alias"})).unwrap();
        let groups = aggregate(&[row], &["model"], &Prices::default()).unwrap();
        assert_eq!(groups[0].model_raw.as_ref().unwrap(), &["Original Alias"]);
    }

    #[test]
    fn review_regression_catalogue_and_price_reads_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("table.json");
        let text = format!("{{\"models\":{{}},\"padding\":\"{}\",\"adapters\":{{\"a\":{{\"models\":[{{\"id\":\"m\"}}]}}}}}}", "x".repeat(8 * 1024 * 1024));
        std::fs::write(&path, text).unwrap();
        assert!(Prices::load(&path).is_err());
        assert!(catalogue(&path).is_empty());
    }

    fn fixture_cell() -> UsageCell {
        UsageCell {
            dimensions: BTreeMap::from([
                ("adapter".into(), serde_json::json!("a")),
                ("host".into(), serde_json::json!("current")),
                ("model".into(), serde_json::json!("m")),
            ]),
            usage: Usage {
                source: "native".into(),
                host: "current".into(),
                adapter: Some("a".into()),
                provider: Some("p".into()),
                model: Some("m".into()),
                model_raw: Some("Original alias".into()),
                input_tokens: Some(130.into()),
                cache_read_tokens: Some(26.into()),
                cache_write_tokens: Some(13.into()),
                output_tokens: Some(65.into()),
                reasoning_tokens: Some(13.into()),
                reported_cost: Some(0.25),
                ..Usage::default()
            },
            rows: 13.into(),
            null_tokens: [false; 5],
            positive_tokens: [true; 5],
            first_row: "2026-10-21\u{1}00000000000000000001".into(),
        }
    }

    fn fixture_prices() -> Prices {
        Prices {
            models: BTreeMap::from([(
                "a/m".into(),
                Price {
                    billing: Some("api".into()),
                    usd_per_million: BTreeMap::from([
                        ("input".into(), serde_json::json!(2)),
                        ("cached_input".into(), serde_json::json!(1)),
                        ("cache_write".into(), serde_json::json!(4)),
                        ("output".into(), serde_json::json!(3)),
                    ]),
                    ..Price::default()
                },
            )]),
            ..Prices::default()
        }
    }

    fn python_cell(cell: &UsageCell) -> Value {
        let values = tokens(&cell.usage);
        let mut value = serde_json::to_value(&cell.dimensions).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert("adapter".into(), serde_json::json!(cell.usage.adapter));
        object.insert("provider".into(), serde_json::json!(cell.usage.provider));
        object.insert("model".into(), serde_json::json!(cell.usage.model));
        object.insert("model_raw".into(), serde_json::json!(cell.usage.model_raw));
        object.insert("rows".into(), serde_json::json!(cell.rows));
        object.insert(
            "reported_cost".into(),
            serde_json::json!(cell.usage.reported_cost),
        );
        for (index, field) in TOKEN_FIELDS.iter().enumerate() {
            object.insert((*field).into(), serde_json::json!(values[index]));
            object.insert(
                format!("null_{field}"),
                serde_json::json!(cell.null_tokens[index]),
            );
            object.insert(
                format!("positive_{field}"),
                serde_json::json!(cell.positive_tokens[index]),
            );
        }
        value
    }

    #[test]
    fn cell_cost_preserves_null_reasoning_zero_rates_and_free_billing() {
        let mut cell = fixture_cell();
        let price = fixture_prices().models["a/m"].clone();
        assert_eq!(
            cell_cost(&cell, &price),
            estimated_cost(&cell.usage, &price)
        );

        cell.null_tokens[4] = true;
        cell.usage.reasoning_tokens = None;
        assert_eq!(
            cell_cost(&cell, &price),
            estimated_cost(&cell.usage, &price)
        );
        cell.null_tokens[0] = true;
        assert_eq!(cell_cost(&cell, &price), None);

        cell.null_tokens = [false; 5];
        let no_output_price = Price {
            usd_per_million: BTreeMap::from([("input".into(), serde_json::json!(2))]),
            ..Price::default()
        };
        assert_eq!(cell_cost(&cell, &no_output_price), None);
        assert_eq!(
            cell_cost(
                &cell,
                &Price {
                    billing: Some("free".into()),
                    ..Price::default()
                }
            ),
            Some(0.0)
        );
    }

    #[test]
    fn cell_unit_matches_python_cell_and_cost_policy() {
        let keys = ["adapter", "model", "host"];
        let mut unlabelled = fixture_cell();
        unlabelled.dimensions.insert("adapter".into(), Value::Null);
        unlabelled
            .dimensions
            .insert("model".into(), serde_json::json!(""));
        unlabelled
            .dimensions
            .insert("host".into(), serde_json::json!(""));
        unlabelled.usage.adapter = None;
        unlabelled.usage.model = Some(String::new());
        unlabelled.usage.model_raw = Some(String::new());
        unlabelled.null_tokens = [true; 5];
        unlabelled.positive_tokens = [false; 5];

        let mut free = fixture_cell();
        free.usage.model = Some("free-model".into());
        free.usage.model_raw = Some("free-model".into());
        free.dimensions
            .insert("model".into(), serde_json::json!("free-model"));
        free.null_tokens = [true; 5];
        free.positive_tokens = [false; 5];
        let free_prices = Prices {
            free_patterns: BTreeMap::from([("a".into(), vec!["^free-model$".into()])]),
            ..Prices::default()
        };

        let mut zero_tokens = fixture_cell();
        zero_tokens.usage.input_tokens = Some(0.into());
        zero_tokens.usage.cache_read_tokens = Some(0.into());
        zero_tokens.usage.cache_write_tokens = Some(0.into());
        zero_tokens.usage.output_tokens = Some(0.into());
        zero_tokens.usage.reasoning_tokens = Some(0.into());
        zero_tokens.positive_tokens = [false; 5];

        for (cell, prices) in [
            (fixture_cell(), fixture_prices()),
            (unlabelled, fixture_prices()),
            (free, free_prices),
            (zero_tokens, Prices::default()),
        ] {
            let input = serde_json::json!({
                "cell": python_cell(&cell),
                "keys": keys,
                "prices": prices.models,
                "free_patterns": prices.free_patterns,
            });
            let source = format!(
                "{}\nprint(json.dumps(costs.cell_unit(t,input['cell'],input['keys'],input['prices'],input['free_patterns'])))",
                crate::differential::LOAD
            );
            let Some(expected) = crate::differential::python(&source, &input) else {
                return;
            };
            let actual = serde_json::to_value(cell_unit(&cell, &keys, &prices).unwrap()).unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[derive(Default)]
    struct QueueStore {
        queries: Vec<SqlCellQuery>,
        responses: std::collections::VecDeque<Vec<SqlCellRow>>,
    }

    impl UsageCellStore for QueueStore {
        fn query_rows(&mut self, query: &SqlCellQuery) -> Result<Vec<SqlCellRow>> {
            self.queries.push(query.clone());
            self.responses
                .pop_front()
                .ok_or_else(|| crate::Error::Invalid("unexpected fixture query".into()))
        }
    }

    fn sql_cell_row() -> SqlCellRow {
        let mut row = BTreeMap::from([
            ("source".into(), serde_json::json!("native")),
            ("adapter".into(), serde_json::json!("a")),
            ("provider".into(), serde_json::json!("p")),
            ("model".into(), serde_json::json!("m")),
            ("account".into(), Value::Null),
            ("status".into(), Value::Null),
            ("host".into(), serde_json::json!("current")),
            ("session".into(), serde_json::json!("s")),
            (
                "timestamp".into(),
                serde_json::json!("2026-10-21T00:00:00Z"),
            ),
            ("reported_cost".into(), Value::Null),
            ("rows".into(), serde_json::json!(13)),
            ("first_row".into(), serde_json::json!("0001")),
        ]);
        for field in TOKEN_FIELDS {
            row.insert(field.into(), serde_json::json!(13));
            row.insert(format!("null_{field}"), serde_json::json!(0));
            row.insert(format!("positive_{field}"), serde_json::json!(1));
        }
        row
    }

    #[test]
    fn usage_cells_builds_bounded_alias_queries_and_decodes_groups() {
        let mut store = QueueStore {
            responses: std::collections::VecDeque::from([
                vec![sql_cell_row()],
                vec![BTreeMap::from([
                    ("adapter".into(), serde_json::json!("a")),
                    ("provider".into(), serde_json::json!("p")),
                    ("model".into(), serde_json::json!("m")),
                    ("session".into(), serde_json::json!("s")),
                    (
                        "timestamp".into(),
                        serde_json::json!("2026-10-21T00:00:00Z"),
                    ),
                ])],
            ]),
            ..QueueStore::default()
        };
        let aliases = BTreeSet::from(["current".into(), "old".into()]);
        let boundary = crate::parse_time("2026-10-21T01:00:00+01:00").unwrap();
        let end = crate::parse_time("2026-10-22").unwrap();
        let parameters = BTreeMap::from([("start".into(), serde_json::json!("2026-10-20"))]);
        let (cells, native) = usage_cells(
            &mut store,
            " AND timestamp >= :start",
            &parameters,
            true,
            Some(boundary),
            Some(end),
            true,
            "printf('%020d', ledger_rowid)",
            "current",
            &aliases,
        )
        .unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].rows, Numeric::from(13));
        assert_eq!(cells[0].usage.model_raw.as_deref(), Some("m"));
        assert_eq!(cells[0].dimensions["host"], "current");
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].session.as_deref(), Some("s"));
        assert_eq!(store.queries.len(), 2);
        let sql = &store.queries[0].sql;
        assert!(sql.contains("ROW_NUMBER() OVER"));
        assert!(sql.contains("PARTITION BY source, event_id"));
        assert!(sql.contains("MIN(printf('%020d', ledger_rowid)) AS first_row"));
        assert!(store.queries[0].parameters["host_identity_alias_0"] == "current");
        assert!(store.queries[0].parameters["low"] == "2026-10-21");
        assert!(store.queries[0].parameters["high"] == "2026-10-22");
        assert!(store.queries[1]
            .sql
            .contains("SELECT DISTINCT adapter, provider, model, session"));
    }

    #[test]
    fn usage_cells_sql_matches_python_with_many_rows_and_alias_deduplication() {
        let fields = [
            "source",
            "event_id",
            "host",
            "session",
            "adapter",
            "provider",
            "model",
            "account",
            "status",
            "timestamp",
            "input_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "output_tokens",
            "reasoning_tokens",
            "reported_cost",
        ];
        let mut fixtures = Vec::new();
        for index in 0..16 {
            let timestamp = if index % 2 == 0 {
                "2026-10-20T00:00:00Z"
            } else {
                "2026-10-21T12:00:00+00:00"
            };
            let mut row = serde_json::json!({
                "source": "claude",
                "event_id": format!("e{index}"),
                "host": if index % 2 == 0 { "old" } else { "current" },
                "session": if index % 4 == 0 { "" } else { "s1" },
                "adapter": if index % 5 == 0 { Value::Null } else if index % 5 == 1 { serde_json::json!("") } else { serde_json::json!("a") },
                "provider": "p",
                "model": if index % 7 == 0 { Value::Null } else if index % 7 == 1 { serde_json::json!("") } else { serde_json::json!("m") },
                "account": if index % 3 == 0 { Value::Null } else { serde_json::json!("acct") },
                "status": if index % 3 == 0 { Value::Null } else { serde_json::json!("ok") },
                "timestamp": timestamp,
                "input_tokens": if index % 3 == 0 { Value::Null } else { serde_json::json!(index + 10) },
                "cache_read_tokens": if index % 4 == 0 { Value::Null } else { serde_json::json!(index + 2) },
                "cache_write_tokens": if index % 5 == 0 { Value::Null } else { serde_json::json!(index + 1) },
                "output_tokens": serde_json::json!(index * 2),
                "reasoning_tokens": if index % 6 == 0 { Value::Null } else { serde_json::json!(index) },
                "reported_cost": if index % 2 == 0 { Value::Null } else { serde_json::json!(0.25) },
            });
            if index == 0 {
                row["event_id"] = serde_json::json!("duplicate");
                fixtures.push(row.clone());
                row["host"] = serde_json::json!("current");
                row["input_tokens"] = serde_json::json!(777);
            }
            fixtures.push(row);
        }
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE usage (
                source TEXT, event_id TEXT, host TEXT, session TEXT, adapter TEXT,
                provider TEXT, model TEXT, account TEXT, status TEXT, timestamp TEXT,
                input_tokens INTEGER, cache_read_tokens INTEGER, cache_write_tokens INTEGER,
                output_tokens INTEGER, reasoning_tokens INTEGER, reported_cost REAL,
                PRIMARY KEY(source,event_id,host)
            );",
        )
        .unwrap();
        let insert = format!(
            "INSERT INTO usage ({}) VALUES ({})",
            fields.join(", "),
            (1..=fields.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for row in &fixtures {
            let values = fields
                .iter()
                .map(|field| sql_parameter(&row[*field]).unwrap())
                .collect::<Vec<_>>();
            db.execute(&insert, rusqlite::params_from_iter(values.iter()))
                .unwrap();
        }

        let aliases = BTreeSet::from(["current".into(), "old".into()]);
        let order = "printf('%s' || char(1) || '%020d', COALESCE(timestamp,''), ledger_rowid)";
        let where_clause = " AND timestamp >= :start";
        let parameters = BTreeMap::from([("start".into(), serde_json::json!("2026-10-19"))]);
        let boundary = crate::parse_time("2026-10-20").unwrap();
        let end = crate::parse_time("2026-10-22").unwrap();
        let source = format!(
            "{}\nimport sqlite3\nfields={fields:?}\ndb=sqlite3.connect(':memory:')\ndb.row_factory=sqlite3.Row\ndb.execute('CREATE TABLE usage (source TEXT,event_id TEXT,host TEXT,session TEXT,adapter TEXT,provider TEXT,model TEXT,account TEXT,status TEXT,timestamp TEXT,input_tokens INTEGER,cache_read_tokens INTEGER,cache_write_tokens INTEGER,output_tokens INTEGER,reasoning_tokens INTEGER,reported_cost REAL)')\ndb.executemany('INSERT INTO usage ('+','.join(fields)+') VALUES ('+','.join('?' for _ in fields)+')',[tuple(row.get(name) for name in fields) for row in input['rows']])\nboundary=t.since_time(input['boundary'])\nend=t.since_time(input['end'])\ncells,native=costs.usage_cells(db,input['where'],input['parameters'],input['summary'],boundary,end,input['need_native'],input['order'],input['current'],set(input['aliases']))\nprint(json.dumps({{'cells':cells,'native':native}}))",
            crate::differential::LOAD,
            fields = fields
        );
        for (summary, need_native) in [(true, true), (false, false)] {
            let input = serde_json::json!({
                "rows": fixtures,
                "where": where_clause,
                "parameters": parameters,
                "summary": summary,
                "boundary": "2026-10-20",
                "end": "2026-10-22",
                "need_native": need_native,
                "order": order,
                "current": "current",
                "aliases": aliases,
            });
            let Some(expected) = crate::differential::python(&source, &input) else {
                return;
            };
            let (cells, native) = usage_cells(
                &mut db,
                where_clause,
                &parameters,
                summary,
                Some(boundary),
                Some(end),
                need_native,
                order,
                "current",
                &aliases,
            )
            .unwrap();
            let cells = cells
                .into_iter()
                .map(|cell| {
                    let mut value = serde_json::Map::new();
                    value.extend(cell.dimensions);
                    for (index, field) in TOKEN_FIELDS.iter().enumerate() {
                        value.insert(
                            (*field).into(),
                            serde_json::to_value(
                                tokens(&cell.usage)[index].unwrap_or(Numeric::ZERO),
                            )
                            .unwrap(),
                        );
                        value.insert(
                            format!("null_{field}"),
                            serde_json::json!(i32::from(cell.null_tokens[index])),
                        );
                        value.insert(
                            format!("positive_{field}"),
                            serde_json::json!(i32::from(cell.positive_tokens[index])),
                        );
                    }
                    value.insert(
                        "reported_cost".into(),
                        serde_json::json!(cell.usage.reported_cost),
                    );
                    value.insert("rows".into(), serde_json::to_value(cell.rows).unwrap());
                    value.insert("first_row".into(), serde_json::json!(cell.first_row));
                    Value::Object(value)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                serde_json::json!({"cells": cells, "native": native}),
                expected
            );
        }
    }
}
