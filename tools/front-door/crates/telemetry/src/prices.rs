//! Typed TOML asset schema. State owns deserialisation and bounded asset loading.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The same five disjoint categories as records' `Usage`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Categories<T> {
    pub uncached_input: T,
    pub cache_read: T,
    pub cache_write: T,
    pub output_excluding_reasoning: T,
    pub reasoning: T,
}
impl<T> Categories<T> {
    pub(crate) fn values_mut(&mut self) -> [&mut T; 5] {
        [
            &mut self.uncached_input,
            &mut self.cache_read,
            &mut self.cache_write,
            &mut self.output_excluding_reasoning,
            &mut self.reasoning,
        ]
    }
}
/// Four independent prices; reasoning tokens use the output rate.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rates {
    pub uncached_input: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub output_excluding_reasoning: Option<u64>,
}
impl Rates {
    pub(crate) fn values(&self) -> [Option<u64>; 4] {
        [
            self.uncached_input,
            self.cache_read,
            self.cache_write,
            self.output_excluding_reasoning,
        ]
    }
    /// Rates in `Categories` order; reasoning reuses the output rate.
    pub(crate) fn usage_rates(&self) -> [Option<u64>; 5] {
        let [uncached_input, cache_read, cache_write, output] = self.values();
        [uncached_input, cache_read, cache_write, output, output]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingKind {
    Api,
    Subscription,
    Free,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredModel {
    pub provider: String,
    pub model: String,
    pub sources: Vec<String>,
    pub billing: BillingKind,
    #[serde(default)]
    pub zero_rate_verified: bool,
    pub rates: Rates,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredPrices {
    pub schema_version: u64,
    pub id: String,
    pub reviewed_at: String,
    pub units: String,
    pub models: Vec<ConfiguredModel>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PriceIdentity {
    pub id: String,
    pub reviewed_at: String,
}

/// Validated table; callers cannot insert unchecked prices.
#[derive(Clone, Debug)]
pub struct PriceTable {
    pub(crate) identity: PriceIdentity,
    models: BTreeMap<(String, String), Rates>,
}
impl PriceTable {
    pub(crate) fn get(&self, provider: &str, model: &str) -> Option<&Rates> {
        self.models.get(&(provider.to_owned(), model.to_owned()))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PriceError {
    #[error("unsupported price schema version {0}")]
    UnsupportedVersion(u64),
    #[error("invalid price table: {0}")]
    Invalid(&'static str),
}

pub fn validate_prices(config: &ConfiguredPrices) -> Result<PriceTable, PriceError> {
    if config.schema_version != 1 {
        return Err(PriceError::UnsupportedVersion(config.schema_version));
    }
    if config.units != "usd_microdollars_per_million_tokens" {
        return Err(PriceError::Invalid("units"));
    }
    if config.id.trim().is_empty()
        || config
            .reviewed_at
            .parse::<jiff::civil::Date>()
            .ok()
            .is_none_or(|d| d.to_string() != config.reviewed_at)
    {
        return Err(PriceError::Invalid("identity or review date"));
    }
    let mut models = BTreeMap::new();
    for model in &config.models {
        if model.provider.trim().is_empty()
            || model.model.trim().is_empty()
            || model.sources.is_empty()
            || model.sources.iter().any(|s| s.trim().is_empty())
        {
            return Err(PriceError::Invalid("model identity or source references"));
        }
        let rates = model.rates.values();
        if rates.contains(&Some(0)) && !model.zero_rate_verified {
            return Err(PriceError::Invalid(
                "zero rate requires explicit verification",
            ));
        }
        if model.billing == BillingKind::Free && rates != [Some(0); 4] {
            return Err(PriceError::Invalid(
                "free billing requires four verified zero rates",
            ));
        }
        let key = (model.provider.clone(), model.model.clone());
        if models.insert(key, model.rates.clone()).is_some() {
            return Err(PriceError::Invalid("duplicate provider/model"));
        }
    }
    Ok(PriceTable {
        identity: PriceIdentity {
            id: config.id.clone(),
            reviewed_at: config.reviewed_at.clone(),
        },
        models,
    })
}
