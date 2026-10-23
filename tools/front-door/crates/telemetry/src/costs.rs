//! Model attribution and summable API-equivalent costs. No rounding before finalisation.
use crate::{Numeric, Result, Usage, number};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

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
    pub dimensions: BTreeMap<String, Value>,
    pub rows: Numeric,
    #[serde(flatten)]
    pub tokens: TokenCounts,
    pub missing_token_fields: BTreeMap<String, Numeric>,
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
    dimensions: BTreeMap<String, Value>,
    prices: &Prices,
) -> Result<Group> {
    let entry = prices.entry(row)?;
    let cost = estimated_cost(row, &entry);
    let values = tokens(row);
    let known = values[..3].iter().all(Option::is_some);
    Ok(Group {
        dimensions,
        rows: Numeric::ONE,
        tokens: TokenCounts::from_array(values.map(|n| n.unwrap_or(Numeric::ZERO))),
        missing_token_fields: TOKEN_FIELDS
            .into_iter()
            .zip(values.map(|n| Numeric::from(i32::from(n.is_none()))))
            .map(|(k, v)| (k.into(), v))
            .collect(),
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
fn key(group: &Group, keys: &[&str]) -> Vec<Value> {
    keys.iter()
        .map(|k| group.dimensions.get(*k).cloned().unwrap_or(Value::Null))
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
            for (field, count) in group.missing_token_fields {
                let target = target.missing_token_fields.entry(field).or_default();
                *target = target.checked_add(count)?;
            }
            if let Some(cost) = group.reported_cost_usd {
                target.reported_cost_usd = Some(target.reported_cost_usd.unwrap_or(0.0) + cost);
            }
            target.model_raw.extend(group.model_raw);
            target.billing.extend(group.billing);
        } else {
            for (name, value) in keys.iter().zip(k) {
                group.dimensions.insert((*name).into(), value);
            }
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
    pub dimensions: BTreeMap<String, Value>,
    pub rows: Numeric,
    pub input_tokens: Option<Numeric>,
    pub cache_read_tokens: Option<Numeric>,
    pub cache_write_tokens: Option<Numeric>,
    pub output_tokens: Option<Numeric>,
    pub reasoning_tokens: Option<Numeric>,
    pub missing_token_rows: Numeric,
    pub missing_token_fields: BTreeMap<String, Numeric>,
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
                dimensions: keys
                    .iter()
                    .map(|k| {
                        (
                            (*k).into(),
                            g.dimensions.get(*k).cloned().unwrap_or(Value::Null),
                        )
                    })
                    .collect(),
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
        groups.push(unit(
            row,
            row.model_raw.as_deref(),
            keys.iter()
                .map(|k| ((*k).into(), value[*k].clone()))
                .collect(),
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
        let text = format!(
            "{{\"models\":{{}},\"padding\":\"{}\",\"adapters\":{{\"a\":{{\"models\":[{{\"id\":\"m\"}}]}}}}}}",
            "x".repeat(8 * 1024 * 1024)
        );
        std::fs::write(&path, text).unwrap();
        assert!(Prices::load(&path).is_err());
        assert!(catalogue(&path).is_empty());
    }
}
