//! The binary edge registers the closed aggregate of landed domain owners.
use provenant_config::{DomainSection, DomainSections};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Domains {
    pub quota: provenant_quota::Settings,
    pub routing: provenant_routing::RoutingConfig,
    pub fabric: provenant_fabric_runtime::config::FabricConfig,
}
static ENTRIES: [DomainSection; 3] = [
    provenant_quota::SECTION,
    provenant_routing::SECTION,
    provenant_fabric_runtime::config::SECTION,
];
pub static SECTIONS: DomainSections<Domains> = DomainSections::new(&ENTRIES);
