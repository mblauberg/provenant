//! Typed TOML asset schema and price-table validation. The composition edge loads the asset.
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
    /// First day (YYYY-MM-DD) the rates apply.
    #[serde(default)]
    pub effective_from: Option<String>,
    /// Last day (YYYY-MM-DD) the rates apply; absent while current.
    #[serde(default)]
    pub effective_to: Option<String>,
    /// OpenRouter model id used by `telemetry prices refresh`.
    #[serde(default)]
    pub openrouter_id: Option<String>,
    /// LiteLLM model key used by `telemetry prices refresh`.
    #[serde(default)]
    pub litellm_id: Option<String>,
    pub rates: Rates,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredPrices {
    pub schema_version: u64,
    pub id: String,
    pub reviewed_at: String,
    pub units: String,
    /// Surface the sheet in `telemetry stats` once `reviewed_at` is older than this.
    #[serde(default)]
    pub max_review_age_days: Option<u32>,
    /// Refresh reports sources differing by more than this many basis points.
    #[serde(default)]
    pub discrepancy_threshold_bps: Option<u32>,
    pub models: Vec<ConfiguredModel>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PriceIdentity {
    pub id: String,
    pub reviewed_at: String,
}

/// How old the review is against the sheet's configured limit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Freshness {
    pub reviewed_at: String,
    pub age_days: i64,
    pub max_age_days: Option<u32>,
    pub stale: bool,
}

/// Validated table; callers cannot insert unchecked prices.
#[derive(Clone, Debug)]
pub struct PriceTable {
    pub(crate) identity: PriceIdentity,
    max_review_age_days: Option<u32>,
    models: BTreeMap<(String, String), (Rates, BillingKind)>,
}
impl PriceTable {
    /// Review age at `today`; stale only when the sheet configures a limit.
    pub fn freshness(&self, today: jiff::civil::Date) -> Freshness {
        let age_days = self
            .identity
            .reviewed_at
            .parse::<jiff::civil::Date>()
            .ok()
            .and_then(|reviewed| today.since(reviewed).ok())
            .map_or(0, |span| i64::from(span.get_days()));
        Freshness {
            reviewed_at: self.identity.reviewed_at.clone(),
            age_days,
            max_age_days: self.max_review_age_days,
            stale: self
                .max_review_age_days
                .is_some_and(|max| age_days > i64::from(max)),
        }
    }
    pub(crate) fn get(&self, provider: &str, model: &str) -> Option<&(Rates, BillingKind)> {
        self.models.get(&(provider.to_owned(), model.to_owned()))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PriceError {
    #[error("unsupported price schema version {0}")]
    UnsupportedVersion(u64),
    #[error("invalid price table: {message}")]
    Invalid {
        code: &'static str,
        message: &'static str,
    },
}

impl PriceError {
    /// Stable machine-readable refusal code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedVersion(_) => "unsupported_version",
            Self::Invalid { code, .. } => code,
        }
    }
}

pub fn validate_prices(config: &ConfiguredPrices) -> Result<PriceTable, PriceError> {
    if config.schema_version != 1 {
        return Err(PriceError::UnsupportedVersion(config.schema_version));
    }
    if config.units != "usd_microdollars_per_million_tokens" {
        return Err(PriceError::Invalid {
            code: "units",
            message: "units",
        });
    }
    if config.id.trim().is_empty()
        || config
            .reviewed_at
            .parse::<jiff::civil::Date>()
            .ok()
            .is_none_or(|d| d.to_string() != config.reviewed_at)
    {
        return Err(PriceError::Invalid {
            code: "identity",
            message: "identity or review date",
        });
    }
    let mut models = BTreeMap::new();
    for model in &config.models {
        if model.provider.trim().is_empty()
            || model.model.trim().is_empty()
            || model.sources.is_empty()
            || model.sources.iter().any(|s| s.trim().is_empty())
        {
            return Err(PriceError::Invalid {
                code: "model_identity",
                message: "model identity or source references",
            });
        }
        let rates = model.rates.values();
        if rates.contains(&Some(0)) && !model.zero_rate_verified {
            return Err(PriceError::Invalid {
                code: "unverified_zero_rate",
                message: "zero rate requires explicit verification",
            });
        }
        if model.billing == BillingKind::Free && rates != [Some(0); 4] {
            return Err(PriceError::Invalid {
                code: "invalid_free_rates",
                message: "free billing requires four verified zero rates",
            });
        }
        let date = |value: &Option<String>| match value {
            None => Ok(None),
            Some(text) => text
                .parse::<jiff::civil::Date>()
                .ok()
                .filter(|d| d.to_string() == *text)
                .map(Some)
                .ok_or(PriceError::Invalid {
                    code: "effective_date",
                    message: "effective date",
                }),
        };
        if let (Some(from), Some(to)) = (date(&model.effective_from)?, date(&model.effective_to)?)
            && from > to
        {
            return Err(PriceError::Invalid {
                code: "effective_range",
                message: "effective_from after effective_to",
            });
        }
        let key = (model.provider.clone(), model.model.clone());
        if models
            .insert(key, (model.rates.clone(), model.billing))
            .is_some()
        {
            return Err(PriceError::Invalid {
                code: "duplicate_model",
                message: "duplicate provider/model",
            });
        }
    }
    Ok(PriceTable {
        identity: PriceIdentity {
            id: config.id.clone(),
            reviewed_at: config.reviewed_at.clone(),
        },
        max_review_age_days: config.max_review_age_days,
        models,
    })
}
