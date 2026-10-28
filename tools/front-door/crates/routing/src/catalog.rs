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
    /// Silence the advisory for Opus implementation lanes.
    pub suppress_opus_writer_warning: bool,
    pub routes: BTreeMap<String, NamedRoute>,
    pub pools: BTreeMap<String, Vec<RoutePoolMember>>,
    pub pacing: BTreeMap<String, PacingPolicy>,
    pub pacing_reserve: BTreeMap<String, PacingReserve>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PacingReserve {
    pub five_hour_percent: u8,
    pub seven_day_percent: u8,
}
impl Default for PacingReserve {
    fn default() -> Self {
        Self {
            five_hour_percent: 15,
            seven_day_percent: 10,
        }
    }
}
impl RoutingConfig {
    /// Suggest a cheaper route in the applied tier. Exact models and custom
    /// pools use the model's highest bundled implementation tier.
    pub fn opus_writer_advisory(
        &self,
        catalogue: &Catalogue,
        route: &crate::ResolvedRoute,
    ) -> Option<String> {
        if self.suppress_opus_writer_warning || !route.model.contains("opus") {
            return None;
        }
        let applied = format!("{}/{}", route.adapter, route.model);
        let pool = route
            .pool_selection
            .as_ref()
            .filter(|receipt| catalogue.has_pool(&receipt.pool))
            .map(|receipt| receipt.pool.as_str())
            .or_else(|| {
                [
                    "principal",
                    "senior",
                    "junior",
                    "design",
                    "writing",
                    "utility",
                ]
                .into_iter()
                .find(|name| {
                    catalogue
                        .pools
                        .get(*name)
                        .is_some_and(|members| members.iter().any(|m| m.model.as_str() == applied))
                })
            })?;
        let members = self.pools.get(pool).or_else(|| catalogue.pools.get(pool))?;
        let cheaper = members
            .iter()
            .chain(catalogue.pools.get(pool).into_iter().flatten())
            .find(|member| {
                member.weight > 0
                    && (member.model.model().contains("sonnet")
                        || member.model.adapter() == "codex")
            })?;
        let effort = cheaper.effort.as_deref().or_else(|| {
            route
                .effort
                .as_deref()
                .filter(|effort| cheaper.efforts.iter().any(|e| e == effort))
        });
        Some(format!(
            "advisory: Opus writer route {applied}; consider cheaper {pool} route {}{} (silence with domains.routing.suppress_opus_writer_warning = true)",
            cheaper.model.as_str(),
            effort
                .map(|effort| format!(" effort={effort}"))
                .unwrap_or_default(),
        ))
    }
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
            if catalogue
                .as_ref()
                .and_then(|value| value.model(&route.model))
                .is_none()
            {
                errors.push(SectionError {
                    field: format!("routes.{name}.model"),
                    code: "unknown_model",
                });
            }
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
        for (provider, reserve) in &self.pacing_reserve {
            if !provenant_providers::REGISTRY
                .iter()
                .any(|r| r.adapter.id() == provider)
            {
                errors.push(SectionError {
                    field: format!("pacing_reserve.{provider}"),
                    code: "unknown_provider",
                });
            }
            if reserve.five_hour_percent > 100 || reserve.seven_day_percent > 100 {
                errors.push(SectionError {
                    field: format!("pacing_reserve.{provider}"),
                    code: "range",
                });
            }
        }
        errors
    }
}
fn defaults(_: Profile) -> &'static str {
    "suppress_opus_writer_warning = false\n[routes]\n[pools]\n[pacing]\n"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccessContext, Fallback, RouteChoice, RouteRequest, RoutingContext, resolve};
    use provenant_providers::{AvailabilityRecord, REGISTRY};
    use provenant_records::{RouteEligibility, RouteReadiness};

    #[test]
    fn bundled_go_models_resolve_and_only_flash_has_provisional_pool_weights()
    -> Result<(), Box<dyn std::error::Error>> {
        let catalogue = Catalogue::bundled()?;
        let config = RoutingConfig::default();
        let model_names = ["mimo-v2.6-flash", "mimo-v2.6-pro", "glm-5.3"];
        let availability = [AvailabilityRecord {
            schema_version: 1,
            adapter: "opencode".into(),
            executable: "/fixture/opencode".into(),
            version: "fixture".into(),
            fixture_ref: "test".into(),
            catalogue_revision: catalogue.revision().into(),
            account_key: Some("go".into()),
            observed_models: model_names
                .iter()
                .map(|name| format!("opencode-go/{name}"))
                .collect(),
            observed_unix_seconds: 80,
            expires_unix_seconds: 120,
        }];
        let access = [AccessContext {
            adapter: "opencode",
            disclosure_ref: "policy",
            confinement_ref: "grant",
        }];
        let context = RoutingContext {
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability: &availability,
            quota: &[],
            access: &access,
            now: provenant_core::WallTime::from_micros(100_000_000)?,
            host: "test-host",
        };
        for name in model_names {
            let model = ModelId::parse(&format!("opencode/opencode-go/{name}"))?;
            let entry = catalogue.model(&model).ok_or("Go model missing")?;
            assert_eq!(entry.trains_on_prompts, Some(false));
            assert!(entry.efforts.is_empty());
            assert!(entry.evidence_ref.contains("opencode-go-models.md"));
            let route = resolve(
                &RouteRequest {
                    choice: RouteChoice::Model(model.clone()),
                    effort: None,
                    confidential: true,
                    allow_free: false,
                    fallback: Fallback::None,
                },
                &context,
            )?;
            assert_eq!(route.model, format!("opencode-go/{name}"));
            assert_eq!(route.eligibility, RouteEligibility::Eligible);
            assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
            let memberships: Vec<_> = catalogue
                .pools
                .iter()
                .filter_map(|(pool, members)| {
                    members
                        .iter()
                        .find(|member| member.model == model)
                        .map(|member| (pool.as_str(), member))
                })
                .collect();
            if name == "mimo-v2.6-flash" {
                assert_eq!(
                    memberships
                        .iter()
                        .map(|(pool, _)| *pool)
                        .collect::<Vec<_>>(),
                    ["junior", "senior"]
                );
                for (_, member) in memberships {
                    assert_eq!(member.weight, 1);
                    assert!(!member.free);
                    assert!(member.effort.is_none());
                    assert!(member.efforts.is_empty());
                }
            } else {
                assert!(memberships.is_empty(), "{name} must await a trial");
            }
        }
        assert_eq!(catalogue.pacing["opencode_paid"], PacingPolicy::Pace);
        assert_eq!(catalogue.pacing["opencode_free"], PacingPolicy::Max);
        Ok(())
    }
}
