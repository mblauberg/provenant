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
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalogue {
    pub schema_version: u64,
    pub revision: String,
    pub models: Vec<ModelEntry>,
}
impl Catalogue {
    pub fn bundled() -> Result<Self, RoutingError> {
        let value: Self = toml_edit::de::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/catalogue.toml"
        )))
        .map_err(|_| RoutingError::Catalogue)?;
        value.validate()?;
        Ok(value)
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
        Ok(())
    }
    pub fn model(&self, id: &ModelId) -> Option<&ModelEntry> {
        self.models.iter().find(|row| row.model == *id)
    }
}
