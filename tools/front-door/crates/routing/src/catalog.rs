use crate::{
    ModelId, RoutingError,
    request::{effort_valid, identifier},
};
use provenant_config::{DomainConfig, DomainSection, Profile, SectionError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PacingPolicy {
    Pace,
    Spend,
    Max,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct RoutingConfig {
    pub routes: BTreeMap<String, NamedRoute>,
    pub pools: BTreeMap<String, Vec<RoutePoolMember>>,
    pub pacing: BTreeMap<String, PacingPolicy>,
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
        let catalogue = Catalogue::bundled().ok();
        for (name, route) in &self.routes {
            let collides_with_pool = catalogue.as_ref().is_some_and(|value| value.has_pool(name));
            if !identifier(name)
                || collides_with_pool
                || route.effort.as_deref().is_some_and(|e| !effort_valid(e))
            {
                errors.push(SectionError {
                    field: "routes".into(),
                    code: if collides_with_pool {
                        "collision"
                    } else {
                        "selector"
                    },
                });
            }
        }
        if self.pools.len() > 32 {
            errors.push(SectionError {
                field: "pools".into(),
                code: "limit",
            });
        }
        for (name, members) in &self.pools {
            if !identifier(name) || self.routes.contains_key(name) {
                errors.push(SectionError {
                    field: format!("pools.{name}"),
                    code: if self.routes.contains_key(name) {
                        "collision"
                    } else {
                        "selector"
                    },
                });
            }
            if members.len() > 16 {
                errors.push(SectionError {
                    field: format!("pools.{name}"),
                    code: "limit",
                });
            }
            let mut seen = BTreeSet::new();
            for (index, member) in members.iter().enumerate() {
                let prefix = format!("pools.{name}[{index}]");
                if !seen.insert(&member.model) {
                    errors.push(SectionError {
                        field: format!("{prefix}.model"),
                        code: "duplicate",
                    });
                }
                if member.weight > 100 {
                    errors.push(SectionError {
                        field: format!("{prefix}.weight"),
                        code: "range",
                    });
                }
                let model = catalogue
                    .as_ref()
                    .and_then(|value| value.model(&member.model));
                if model.is_none() {
                    errors.push(SectionError {
                        field: format!("{prefix}.model"),
                        code: "unknown_model",
                    });
                }
                if !provenant_providers::REGISTRY
                    .iter()
                    .any(|provider| provider.adapter.id() == member.model.adapter())
                {
                    errors.push(SectionError {
                        field: format!("{prefix}.model"),
                        code: "unknown_provider",
                    });
                }
                let mut seen_efforts = BTreeSet::new();
                if member.efforts.len() > 16
                    || member.efforts.iter().any(|effort| {
                        !effort_valid(effort)
                            || !seen_efforts.insert(effort)
                            || model.is_some_and(|entry| {
                                !entry.efforts.iter().any(|supported| supported == effort)
                            })
                    })
                {
                    errors.push(SectionError {
                        field: format!("{prefix}.efforts"),
                        code: "unsupported_effort",
                    });
                }
                if member.effort.as_deref().is_some_and(|effort| {
                    !effort_valid(effort) || !member.efforts.iter().any(|allowed| allowed == effort)
                }) {
                    errors.push(SectionError {
                        field: format!("{prefix}.effort"),
                        code: "unsupported_effort",
                    });
                }
            }
        }
        for provider in self.pacing.keys() {
            if !provenant_providers::REGISTRY
                .iter()
                .any(|registration| registration.adapter.id() == provider)
            {
                errors.push(SectionError {
                    field: format!("pacing.{provider}"),
                    code: "unknown_provider",
                });
            }
        }
        errors
    }
}
fn defaults(_: Profile) -> &'static str {
    "[routes]\n[pools]\n[pacing]\n"
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
    /// Unknown is treated as training for confidential selection.
    #[serde(default)]
    pub trains_on_prompts: Option<bool>,
}
/// Pool candidate; list order is used for weight-zero fallback selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutePoolMember {
    pub model: ModelId,
    #[serde(default = "default_weight")]
    pub weight: u8,
    pub effort: Option<String>,
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default)]
    pub free: bool,
}
fn default_weight() -> u8 {
    1
}
/// Only the bundled asset constructs a catalogue. A caller cannot deserialize or
/// change family/effort rows while retaining an existing revision.
#[derive(Debug, Clone, Serialize)]
pub struct Catalogue {
    pub(crate) schema_version: u64,
    pub(crate) revision: String,
    pub(crate) models: Vec<ModelEntry>,
    pub(crate) pools: BTreeMap<String, Vec<RoutePoolMember>>,
    pub(crate) pacing: BTreeMap<String, PacingPolicy>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueAsset {
    schema_version: u64,
    revision: String,
    models: Vec<ModelEntry>,
    #[serde(default)]
    pools: BTreeMap<String, Vec<RoutePoolMember>>,
    #[serde(default)]
    pacing: BTreeMap<String, PacingPolicy>,
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
            pacing: asset.pacing,
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
            if !identifier(name) || members.is_empty() || members.len() > 16 {
                return Err(RoutingError::Catalogue);
            }
            let mut pool_models = BTreeSet::new();
            for member in members {
                let Some(model) = self.model(&member.model) else {
                    return Err(RoutingError::Catalogue);
                };
                if !pool_models.insert(&member.model)
                    || member.weight > 100
                    || member
                        .efforts
                        .iter()
                        .any(|effort| !model.efforts.iter().any(|supported| supported == effort))
                    || member.effort.as_deref().is_some_and(|effort| {
                        !member.efforts.iter().any(|allowed| allowed == effort)
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
    pub fn has_pool(&self, name: &str) -> bool {
        self.pools.contains_key(name)
    }
    /// Whether the bundled pool evidence marks this exact model as free.
    pub fn is_free_model(&self, id: &ModelId) -> bool {
        self.pools
            .values()
            .flatten()
            .any(|member| member.free && member.model == *id)
    }
}
