//! Fabric owns domain policy and complete compiled profile assets.
use provenant_config::{DomainConfig, DomainSection, Profile, SectionError};
use provenant_core::HostId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricConfig {
    pub agent_label: String,
    pub client_label: String,
    pub home_hosts: BTreeMap<String, HostId>,
    pub peers: BTreeMap<String, Peer>,
    pub limits: Limits,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub host: HostId,
    pub destination: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub body_bytes: u64,
    pub input_frame_bytes: u64,
    pub json_depth: u64,
    pub page_rows: u64,
    pub output_lines: u64,
    pub output_bytes: u64,
    pub result_bytes: u64,
    pub lease_seconds: u64,
    pub peer_seconds: u64,
    #[serde(default = "default_graph_timeout_ms")]
    pub graph_timeout_ms: u64,
    pub bundle_bytes: u64,
}
fn default_graph_timeout_ms() -> u64 {
    14_400_000
}

impl DomainConfig for FabricConfig {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors = Vec::new();
        let mut reject = |field: String| {
            errors.push(SectionError {
                field,
                code: "range",
            })
        };
        for (name, label) in [
            (
                "agent_label",
                if self.agent_label.is_empty() {
                    None
                } else {
                    Some(self.agent_label.as_str())
                },
            ),
            ("client_label", Some(self.client_label.as_str())),
        ] {
            if label.is_some_and(|value| crate::identity::validate_label(value).is_err()) {
                reject(name.into());
            }
        }
        for (name, peer) in &self.peers {
            if crate::identity::validate_label(name).is_err()
                || peer.destination.is_empty()
                || peer.destination.len() > 255
                || peer.destination.starts_with('-')
                || !peer
                    .destination
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b))
            {
                reject(format!("peers.{name}"));
            }
        }
        for key in self.home_hosts.keys() {
            if key.len() != 64
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                reject("home_hosts".into());
            }
        }
        let l = &self.limits;
        for (field, value, cap) in [
            ("body_bytes", l.body_bytes, 65536),
            ("input_frame_bytes", l.input_frame_bytes, 1048576),
            ("json_depth", l.json_depth, 64),
            ("page_rows", l.page_rows, 100),
            ("output_lines", l.output_lines, 500),
            ("output_bytes", l.output_bytes, 65536),
            ("result_bytes", l.result_bytes, 1048576),
            ("lease_seconds", l.lease_seconds, 3600),
            ("peer_seconds", l.peer_seconds, 30),
            ("bundle_bytes", l.bundle_bytes, 67108864),
        ] {
            if value == 0 || value > cap {
                reject(format!("limits.{field}"));
            }
        }
        if l.graph_timeout_ms < 1000 || l.graph_timeout_ms > 14_400_000 {
            reject("limits.graph_timeout_ms".into());
        }
        errors
    }
}
fn defaults(profile: Profile) -> &'static str {
    match profile {
        Profile::Balanced => include_str!("../assets/balanced.toml"),
        Profile::Economy => include_str!("../assets/economy.toml"),
        Profile::Rigorous => include_str!("../assets/rigorous.toml"),
    }
}
pub const SECTION: DomainSection = DomainSection::new::<FabricConfig>("fabric", defaults);
