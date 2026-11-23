//! Host and project price overlays.
//!
//! Prices keep the shipped `config/telemetry-prices.toml` as their bundled
//! layer. Owner entries live in the same host and project config files other
//! cross-cutting domains use, and follow the same precedence order. An overlay
//! entry replaces the bundled price for its `(provider, model)` key or adds a
//! new one; `remove = true` removes it. A bad entry is skipped with a warning
//! naming its layer, so the remaining sheet stays usable.

use crate::{
    BillingKind, ConfiguredModel, ConfiguredPrices, PriceError, PriceTable, Rates, validate_prices,
};
use provenant_config::{OverlaySource, OverlayWarning};
use serde::Deserialize;
use std::{
    io,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Effective price table together with the provenance of its overlaid entries.
#[derive(Clone, Debug)]
pub struct LayeredPrices {
    pub table: PriceTable,
}

#[derive(Debug, Error)]
pub enum PriceLoadError {
    #[error("{path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}: invalid price table: {message}")]
    Parse { path: PathBuf, message: String },
    #[error(transparent)]
    Invalid(#[from] PriceError),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PriceOverlay {
    provider: String,
    model: String,
    #[serde(default)]
    remove: bool,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    billing: Option<BillingKind>,
    #[serde(default)]
    zero_rate_verified: bool,
    #[serde(default)]
    effective_from: Option<String>,
    #[serde(default)]
    effective_to: Option<String>,
    #[serde(default)]
    openrouter_id: Option<String>,
    #[serde(default)]
    litellm_id: Option<String>,
    #[serde(default)]
    rates: Rates,
}

impl PriceOverlay {
    fn into_model(self) -> ConfiguredModel {
        ConfiguredModel {
            provider: self.provider,
            model: self.model,
            sources: self.sources,
            billing: self.billing.unwrap_or(BillingKind::Api),
            zero_rate_verified: self.zero_rate_verified,
            effective_from: self.effective_from,
            effective_to: self.effective_to,
            openrouter_id: self.openrouter_id,
            litellm_id: self.litellm_id,
            rates: self.rates,
        }
    }
}

fn decode(row: &toml_edit::Table) -> Result<PriceOverlay, String> {
    toml_edit::de::from_str::<PriceOverlay>(&row.to_string()).map_err(|error| error.to_string())
}

/// Load the shipped sheet, then apply host and project price overlays.
pub fn load(
    assets: &Path,
    state: &Path,
    project: Option<&Path>,
) -> Result<LayeredPrices, PriceLoadError> {
    let path = assets.join("config/telemetry-prices.toml");
    let text = std::fs::read_to_string(&path).map_err(|source| PriceLoadError::Read {
        path: path.clone(),
        source,
    })?;
    let mut config: ConfiguredPrices =
        toml_edit::de::from_str(&text).map_err(|error| PriceLoadError::Parse {
            path: path.clone(),
            message: error.to_string(),
        })?;
    let (layers, mut warnings) = provenant_config::overlays::read_layers(state, project, "prices");
    let mut sources = std::collections::BTreeMap::new();
    for layer in &layers {
        let document: toml_edit::DocumentMut = match layer.text.parse() {
            Ok(document) => document,
            Err(error) => {
                warnings.push(OverlayWarning::new(
                    layer.source,
                    "prices",
                    format!("invalid TOML: {error}"),
                ));
                continue;
            }
        };
        let Some(table) = document.get("prices").and_then(toml_edit::Item::as_table) else {
            continue;
        };
        let Some(rows) = table
            .get("models")
            .and_then(toml_edit::Item::as_array_of_tables)
        else {
            continue;
        };
        for row in rows.iter() {
            let key = format!(
                "{}/{}",
                row.get("provider")
                    .and_then(toml_edit::Item::as_str)
                    .unwrap_or("?"),
                row.get("model")
                    .and_then(toml_edit::Item::as_str)
                    .unwrap_or("?")
            );
            let before = config.clone();
            let before_sources = sources.clone();
            match decode(row) {
                Ok(entry) if entry.remove => {
                    config.models.retain(|model| {
                        model.provider != entry.provider || model.model != entry.model
                    });
                    sources.remove(&(entry.provider.clone(), entry.model.clone()));
                }
                Ok(entry) => {
                    let pair = (entry.provider.clone(), entry.model.clone());
                    let model = entry.into_model();
                    if let Some(existing) = config
                        .models
                        .iter_mut()
                        .find(|row| row.provider == model.provider && row.model == model.model)
                    {
                        *existing = model;
                    } else {
                        config.models.push(model);
                    }
                    if layer.source != OverlaySource::Bundled {
                        sources.insert(pair, layer.source.as_str().to_owned());
                    }
                }
                Err(reason) => {
                    warnings.push(OverlayWarning::new(
                        layer.source,
                        format!("models[{key}]"),
                        reason,
                    ));
                    continue;
                }
            }
            if validate_prices(&config).is_err() {
                config = before;
                sources = before_sources;
                warnings.push(OverlayWarning::new(
                    layer.source,
                    format!("models[{key}]"),
                    "entry would leave the price sheet invalid",
                ));
            }
        }
    }
    let mut table = validate_prices(&config)?;
    table.sources = sources;
    table.warnings = warnings;
    Ok(LayeredPrices { table })
}
