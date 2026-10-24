use crate::{
    ModelId, RoutingError,
    request::{effort_valid, identifier},
};
use provenant_config::{DomainConfig, DomainSection, Profile, SectionError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingConfig {
    pub routes: BTreeMap<String, NamedRoute>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedRoute {
    pub model: ModelId,
    pub effort: Option<String>,
}
impl DomainConfig for RoutingConfig {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors = Vec::new();
        if self.routes.len() > 128 {
            errors.push(SectionError {
                field: "routes".into(),
                code: "limit",
            });
        }
        for (name, route) in &self.routes {
            if !identifier(name) || route.effort.as_deref().is_some_and(|e| !effort_valid(e)) {
                errors.push(SectionError {
                    field: "routes".into(),
                    code: "selector",
                });
            }
        }
        errors
    }
}
fn defaults(_: Profile) -> &'static str {
    "[routes]\n"
}
pub const SECTION: DomainSection = DomainSection::new::<RoutingConfig>("routing", defaults);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub model: ModelId,
    /// Only exact controls with recording evidence. No guessed effort default.
    pub efforts: Vec<String>,
    pub evidence_ref: String,
    pub family: Option<String>,
    /// Unknown is treated as training for confidential pool selection.
    #[serde(default)]
    pub trains_on_prompts: Option<bool>,
}
/// Ordered candidate in a bundled route pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutePoolMember {
    pub model: ModelId,
    pub effort: Option<String>,
}
/// Only the bundled asset constructs a catalogue. A caller cannot deserialize or
/// change family/effort rows while retaining an existing revision.
#[derive(Debug, Clone, Serialize)]
pub struct Catalogue {
    pub(crate) schema_version: u64,
    pub(crate) revision: String,
    pub(crate) models: Vec<ModelEntry>,
    pub(crate) pools: BTreeMap<String, Vec<RoutePoolMember>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueAsset {
    schema_version: u64,
    revision: String,
    models: Vec<ModelEntry>,
    #[serde(default)]
    pools: BTreeMap<String, Vec<RoutePoolMember>>,
}
impl Catalogue {
    pub fn bundled() -> Result<Self, RoutingError> {
        let asset: CatalogueAsset = toml_edit::de::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/catalogue.toml"
        )))
        .map_err(|_| RoutingError::Catalogue)?;
        let value = Self {
            schema_version: asset.schema_version,
            revision: asset.revision,
            models: asset.models,
            pools: asset.pools,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub fn models(&self) -> &[ModelEntry] {
        &self.models
    }
    pub fn validate(&self) -> Result<(), RoutingError> {
        let mut seen = BTreeSet::new();
        if self.schema_version != 1 || !identifier(&self.revision) || self.models.len() > 256 {
            return Err(RoutingError::Catalogue);
        }
        for row in &self.models {
            if !seen.insert(&row.model)
                || row.evidence_ref.is_empty()
                || row.evidence_ref.len() > 512
                || row.efforts.len() > 16
                || row.efforts.iter().any(|e| !effort_valid(e))
                || row.family.as_deref().is_some_and(|f| !identifier(f))
            {
                return Err(RoutingError::Catalogue);
            }
        }
        if self.pools.len() > 32 {
            return Err(RoutingError::Catalogue);
        }
        for (name, members) in &self.pools {
            if !identifier(name) || members.is_empty() || members.len() > 32 {
                return Err(RoutingError::Catalogue);
            }
            let mut pool_models = BTreeSet::new();
            for member in members {
                let Some(model) = self.model(&member.model) else {
                    return Err(RoutingError::Catalogue);
                };
                if !pool_models.insert(&member.model)
                    || member.effort.as_deref().is_some_and(|effort| {
                        !model.efforts.iter().any(|supported| supported == effort)
                    })
                {
                    return Err(RoutingError::Catalogue);
                }
            }
        }
        Ok(())
    }
    pub fn model(&self, id: &ModelId) -> Option<&ModelEntry> {
        self.models.iter().find(|row| row.model == *id)
    }
}
