//! The binary edge registers the closed aggregate of landed domain owners.
use provenant_config::{DomainSection, DomainSections};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Domains {
    pub routing: provenant_routing::RoutingConfig,
    pub fabric: provenant_fabric_runtime::config::FabricConfig,
}
static ENTRIES: [DomainSection; 2] = [
    provenant_routing::SECTION,
    provenant_fabric_runtime::config::SECTION,
];
pub(crate) static SECTIONS: DomainSections<Domains> = DomainSections::new(&ENTRIES);
