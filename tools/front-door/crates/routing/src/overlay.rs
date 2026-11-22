//! Host and project overlays for the model catalogue.
//!
//! Overlays reuse the config layers already used by admission and ports: host
//! `<state>/config.toml`, host-project `<state>/projects/<key>/config.toml` and
//! project `<project>/.provenant/config.toml`, in that order. Each entry
//! replaces the bundled entry with the same key or adds a new one; removal is
//! explicit with `remove = true`. An entry that cannot be decoded or that would
//! leave the catalogue invalid is skipped with a warning that names its layer.

use crate::{Capability, Catalogue, ModelEntry, ModelId, MonthlyAllowance, RoutePoolMember};
use provenant_config::{OverlayLayer, OverlaySource, OverlayWarning};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Source of each effective catalogue fact. Facts not named here come from the
/// bundled asset; an entry present here was supplied by the named layer.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CatalogueSources {
    /// `model:<adapter/model>` → source of the whole model entry.
    pub models: BTreeMap<String, String>,
    /// `<pool name>` → source of the pool's existence.
    pub pools: BTreeMap<String, String>,
    /// `<pool>/<adapter/model>` → source of pool membership.
    pub pool_members: BTreeMap<String, String>,
    /// `<pool>/<adapter/model>` → source of the member's weight.
    pub weights: BTreeMap<String, String>,
    /// `<route name>` → source of the capability-preference row.
    pub capability_preferences: BTreeMap<String, String>,
}

impl CatalogueSources {
    fn record(models: &mut BTreeMap<String, String>, key: String, source: OverlaySource) {
        if source != OverlaySource::Bundled {
            models.insert(key, source.as_str().to_owned());
        }
    }
    pub fn model_source(&self, model: &ModelId) -> Option<&str> {
        self.models
            .get(&format!("model:{}", model.as_str()))
            .map(String::as_str)
    }
    pub fn pool_source(&self, pool: &str) -> Option<&str> {
        self.pools.get(pool).map(String::as_str)
    }
    pub fn membership_source(&self, pool: &str, model: &ModelId) -> Option<&str> {
        self.pool_members
            .get(&format!("{pool}/{}", model.as_str()))
            .map(String::as_str)
    }
    pub fn weight_source(&self, pool: &str, model: &ModelId) -> Option<&str> {
        self.weights
            .get(&format!("{pool}/{}", model.as_str()))
            .map(String::as_str)
    }
    pub fn capability_source(&self, route: &str) -> Option<&str> {
        self.capability_preferences.get(route).map(String::as_str)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelOverlay {
    model: ModelId,
    #[serde(default)]
    remove: bool,
    #[serde(default)]
    capability: Capability,
    #[serde(default)]
    free: bool,
    #[serde(default)]
    efforts: Vec<String>,
    #[serde(default)]
    effort_suffix: bool,
    #[serde(default)]
    evidence_ref: String,
    #[serde(default)]
    service_tiers: Vec<String>,
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    trains_on_prompts: Option<bool>,
    #[serde(default)]
    monthly_allowance_usd: Option<MonthlyAllowance>,
    #[serde(default)]
    quota_pool: Option<String>,
    #[serde(default)]
    superseded_by: Vec<ModelId>,
}

impl ModelOverlay {
    fn into_entry(self, source: OverlaySource) -> ModelEntry {
        ModelEntry {
            model: self.model,
            capability: self.capability,
            free: self.free,
            efforts: self.efforts,
            effort_suffix: self.effort_suffix,
            evidence_ref: self.evidence_ref,
            source: source.as_str().to_owned(),
            service_tiers: self.service_tiers,
            // Per-model policy rules remain owned by `domains.routing.model_policy`;
            // an overlay changes catalogue facts, not policy authority.
            policy_rules: Vec::new(),
            family: self.family,
            trains_on_prompts: self.trains_on_prompts,
            monthly_allowance_usd: self.monthly_allowance_usd,
            quota_pool: self.quota_pool,
            superseded_by: self.superseded_by,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PoolMemberOverlay {
    pool: String,
    model: ModelId,
    #[serde(default)]
    remove: bool,
    #[serde(default = "default_weight")]
    weight: u8,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    efforts: Vec<String>,
    #[serde(default)]
    free: bool,
}

fn default_weight() -> u8 {
    1
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityPreferenceOverlay {
    #[serde(default)]
    remove: bool,
    #[serde(default)]
    general: Option<u16>,
    #[serde(default)]
    strong: Option<u16>,
    #[serde(default)]
    frontier: Option<u16>,
}

/// Decode one array-of-tables row on its own. The row's rendered table is a
/// valid standalone TOML document, so one malformed row cannot reject its
/// siblings or the rest of the layer.
fn decode<T: serde::de::DeserializeOwned>(row: &toml_edit::Table) -> Result<T, String> {
    toml_edit::de::from_str::<T>(&row.to_string()).map_err(|error| error.to_string())
}

fn rows<'a>(table: &'a toml_edit::Table, key: &str) -> Vec<&'a toml_edit::Table> {
    table
        .get(key)
        .and_then(toml_edit::Item::as_array_of_tables)
        .map_or_else(Vec::new, |rows| rows.iter().collect())
}

/// Apply every layer in order to a bundled (or discovered) catalogue.
pub(crate) fn apply(
    catalogue: &mut Catalogue,
    layers: &[OverlayLayer],
    warnings: Vec<OverlayWarning>,
) {
    catalogue.overlay_warnings = warnings;
    let mut changed = false;
    for layer in layers {
        let document: toml_edit::DocumentMut = match layer.text.parse() {
            Ok(document) => document,
            Err(error) => {
                catalogue.overlay_warnings.push(OverlayWarning::new(
                    layer.source,
                    "catalogue",
                    format!("invalid TOML: {error}"),
                ));
                continue;
            }
        };
        let Some(table) = document
            .get("catalogue")
            .and_then(toml_edit::Item::as_table)
        else {
            continue;
        };
        for row in rows(table, "models") {
            let key = row
                .get("model")
                .and_then(toml_edit::Item::as_str)
                .unwrap_or("<unreadable model>")
                .to_owned();
            let before = catalogue.clone();
            match decode::<ModelOverlay>(row) {
                Ok(entry) if entry.remove => {
                    remove_model(catalogue, &entry.model);
                    catalogue
                        .overlay_sources
                        .models
                        .remove(&format!("model:{}", entry.model.as_str()));
                }
                Ok(entry) => {
                    let model = entry.model.clone();
                    let source = layer.source;
                    if let Some(existing) =
                        catalogue.models.iter_mut().find(|row| row.model == model)
                    {
                        *existing = entry.into_entry(source);
                    } else {
                        catalogue.models.push(entry.into_entry(source));
                    }
                    CatalogueSources::record(
                        &mut catalogue.overlay_sources.models,
                        format!("model:{}", model.as_str()),
                        source,
                    );
                }
                Err(reason) => {
                    catalogue.overlay_warnings.push(OverlayWarning::new(
                        layer.source,
                        format!("models[{key}]"),
                        reason,
                    ));
                    continue;
                }
            }
            if catalogue.validate().is_err() {
                *catalogue = before;
                catalogue.overlay_warnings.push(OverlayWarning::new(
                    layer.source,
                    format!("models[{key}]"),
                    "entry would leave the catalogue invalid",
                ));
            } else {
                changed = true;
            }
        }
        for row in rows(table, "pool_members") {
            let key = format!(
                "{}/{}",
                row.get("pool")
                    .and_then(toml_edit::Item::as_str)
                    .unwrap_or("?"),
                row.get("model")
                    .and_then(toml_edit::Item::as_str)
                    .unwrap_or("?")
            );
            let before = catalogue.clone();
            match decode::<PoolMemberOverlay>(row) {
                Ok(entry) => apply_pool_member(catalogue, entry, layer.source),
                Err(reason) => {
                    catalogue.overlay_warnings.push(OverlayWarning::new(
                        layer.source,
                        format!("pool_members[{key}]"),
                        reason,
                    ));
                    continue;
                }
            }
            if catalogue.validate().is_err() {
                *catalogue = before;
                catalogue.overlay_warnings.push(OverlayWarning::new(
                    layer.source,
                    format!("pool_members[{key}]"),
                    "entry would leave the catalogue invalid",
                ));
            } else {
                changed = true;
            }
        }
        if let Some(preferences) = table
            .get("capability_preferences")
            .and_then(toml_edit::Item::as_table)
        {
            for (name, item) in preferences.iter() {
                let Some(row) = item.as_table() else {
                    catalogue.overlay_warnings.push(OverlayWarning::new(
                        layer.source,
                        format!("capability_preferences[{name}]"),
                        "expected a table",
                    ));
                    continue;
                };
                let before = catalogue.clone();
                match decode::<CapabilityPreferenceOverlay>(row) {
                    Ok(entry) if entry.remove => {
                        catalogue.capability_preferences.remove(name);
                        catalogue
                            .overlay_sources
                            .capability_preferences
                            .remove(name);
                    }
                    Ok(entry) => {
                        let mut values = BTreeMap::new();
                        for (capability, weight) in [
                            (Capability::General, entry.general),
                            (Capability::Strong, entry.strong),
                            (Capability::Frontier, entry.frontier),
                        ] {
                            if let Some(weight) = weight {
                                values.insert(capability, weight);
                            }
                        }
                        catalogue
                            .capability_preferences
                            .insert(name.to_owned(), values);
                        CatalogueSources::record(
                            &mut catalogue.overlay_sources.capability_preferences,
                            name.to_owned(),
                            layer.source,
                        );
                    }
                    Err(reason) => {
                        catalogue.overlay_warnings.push(OverlayWarning::new(
                            layer.source,
                            format!("capability_preferences[{name}]"),
                            reason,
                        ));
                        continue;
                    }
                }
                if catalogue.validate().is_err() {
                    *catalogue = before;
                    catalogue.overlay_warnings.push(OverlayWarning::new(
                        layer.source,
                        format!("capability_preferences[{name}]"),
                        "entry would leave the catalogue invalid",
                    ));
                } else {
                    changed = true;
                }
            }
        }
    }
    if changed {
        use sha2::{Digest as _, Sha256};
        let contents: Vec<_> = layers
            .iter()
            .map(|layer| (layer.source.as_str(), layer.text.as_str()))
            .collect();
        let bytes =
            serde_json::to_vec(&(catalogue.revision.as_str(), contents)).unwrap_or_default();
        let digest = Sha256::digest(bytes);
        catalogue.revision = format!(
            "overlay-{}",
            digest
                .iter()
                .take(24)
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
    }
}

fn remove_model(catalogue: &mut Catalogue, model: &ModelId) {
    catalogue.models.retain(|row| row.model != *model);
    for (name, members) in &mut catalogue.pools {
        let selectors = catalogue.pool_selectors.entry(name.clone()).or_default();
        let mut kept_models = Vec::new();
        let mut kept_selectors = Vec::new();
        for (member, selector) in members.iter().zip(selectors.iter()) {
            if member.model != *model {
                kept_models.push(member.clone());
                kept_selectors.push(selector.clone());
            }
        }
        *members = kept_models;
        *selectors = kept_selectors;
    }
    catalogue.pools.retain(|_, members| !members.is_empty());
    catalogue
        .pool_selectors
        .retain(|name, _| catalogue.pools.contains_key(name));
    catalogue
        .writer_advisory
        .warn_models
        .retain(|id| id != model);
    catalogue
        .writer_advisory
        .cheaper_models
        .retain(|id| id != model);
    for row in &mut catalogue.models {
        row.superseded_by.retain(|id| id != model);
    }
}

fn apply_pool_member(catalogue: &mut Catalogue, entry: PoolMemberOverlay, source: OverlaySource) {
    let pool = entry.pool;
    let model = entry.model;
    let member = RoutePoolMember {
        model: model.clone(),
        weight: entry.weight,
        effort: entry.effort,
        efforts: entry.efforts,
        free: entry.free,
    };
    let index = catalogue
        .pools
        .get(&pool)
        .and_then(|members| members.iter().position(|row| row.model == model));
    if entry.remove {
        if let Some(index) = index {
            if let Some(members) = catalogue.pools.get_mut(&pool) {
                members.remove(index);
            }
            if let Some(selectors) = catalogue.pool_selectors.get_mut(&pool)
                && index < selectors.len()
            {
                selectors.remove(index);
            }
            if catalogue
                .pools
                .get(&pool)
                .is_some_and(|members| members.is_empty())
            {
                catalogue.pools.remove(&pool);
                catalogue.pool_selectors.remove(&pool);
            }
        }
    } else if let Some(index) = index {
        if let Some(members) = catalogue.pools.get_mut(&pool) {
            members[index] = member;
        }
    } else {
        catalogue
            .pools
            .entry(pool.clone())
            .or_default()
            .push(member);
        catalogue
            .pool_selectors
            .entry(pool.clone())
            .or_default()
            .push(String::new());
    }
    let key = format!("{pool}/{}", model.as_str());
    if entry.remove {
        catalogue.overlay_sources.pools.remove(&pool);
        catalogue.overlay_sources.pool_members.remove(&key);
        catalogue.overlay_sources.weights.remove(&key);
    } else {
        CatalogueSources::record(&mut catalogue.overlay_sources.pools, pool, source);
        CatalogueSources::record(
            &mut catalogue.overlay_sources.pool_members,
            key.clone(),
            source,
        );
        CatalogueSources::record(&mut catalogue.overlay_sources.weights, key, source);
    }
}
