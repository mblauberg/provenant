//! The binary edge registers the closed aggregate of landed domain owners.
use provenant_config::{DomainSection, DomainSections};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Domains {
    pub accounts: provenant_providers::accounts::Settings,
    pub quota: provenant_quota::Settings,
    pub routing: provenant_routing::RoutingConfig,
    pub fabric: provenant_fabric_runtime::config::FabricConfig,
    pub guard: provenant_confinement::guard::GuardConfig,
    #[serde(default)]
    pub run: provenant_confinement::run_config::RunConfig,
}
static ENTRIES: [DomainSection; 6] = [
    provenant_providers::accounts::SECTION,
    provenant_quota::SECTION,
    provenant_routing::SECTION,
    provenant_fabric_runtime::config::SECTION,
    provenant_confinement::guard::SECTION,
    provenant_confinement::run_config::SECTION,
];
pub static SECTIONS: DomainSections<Domains> = DomainSections::new(&ENTRIES);
