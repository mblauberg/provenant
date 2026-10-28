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
    #[serde(default, deserialize_with = "deserialize_ssh_executable")]
    pub ssh_executable: Option<std::path::PathBuf>,
    pub home_hosts: BTreeMap<String, HostId>,
    pub peers: BTreeMap<String, Peer>,
    pub limits: Limits,
    #[serde(default)]
    pub lanes: Lanes,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Lanes {
    pub priority: LanePriority,
}

/// Nice values applied by the host before entering lane confinement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct LanePriority {
    pub writer: u8,
    pub read_only: u8,
    pub build_heavy: u8,
}

impl Default for LanePriority {
    fn default() -> Self {
        Self {
            writer: 10,
            read_only: 5,
            build_heavy: 15,
        }
    }
}

impl LanePriority {
    pub fn for_lane(&self, writer: bool, build_heavy: bool) -> u8 {
        if build_heavy {
            self.build_heavy
        } else if writer {
            self.writer
        } else {
            self.read_only
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub host: HostId,
    pub destination: String,
    #[serde(default)]
    pub remote_executable: Option<std::path::PathBuf>,
    #[serde(default = "default_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
}
fn default_connect_timeout_seconds() -> u64 {
    10
}
fn deserialize_ssh_executable<'de, D>(
    deserializer: D,
) -> Result<Option<std::path::PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<std::path::PathBuf>::deserialize(deserializer)?
        .filter(|path| !path.as_os_str().is_empty()))
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
    #[serde(default = "default_graph_timeout_seconds")]
    pub graph_timeout_seconds: u64,
    pub bundle_bytes: u64,
}
fn default_graph_timeout_seconds() -> u64 {
    14_400
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
            if peer
                .remote_executable
                .as_deref()
                .is_some_and(|path| !valid_executable_override(path))
            {
                reject(format!("peers.{name}.remote_executable"));
            }
            if !(1..=30).contains(&peer.connect_timeout_seconds) {
                reject(format!("peers.{name}.connect_timeout_seconds"));
            }
        }
        if self
            .ssh_executable
            .as_deref()
            .is_some_and(|path| !valid_executable_override(path))
        {
            reject("ssh_executable".into());
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
        if l.graph_timeout_seconds == 0 || l.graph_timeout_seconds > 14_400 {
            reject("limits.graph_timeout_seconds".into());
        }
        for (name, value) in [
            ("writer", self.lanes.priority.writer),
            ("read-only", self.lanes.priority.read_only),
            ("build-heavy", self.lanes.priority.build_heavy),
        ] {
            if value > 19 {
                reject(format!("lanes.priority.{name}"));
            }
        }
        errors
    }
}
pub(crate) fn valid_executable_override(path: &std::path::Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|value| {
            value.len() <= 4096
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
        })
}
fn defaults(_profile: Profile) -> &'static str {
    include_str!("../assets/fabric.toml")
}
pub const SECTION: DomainSection = DomainSection::new::<FabricConfig>("fabric", defaults);

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn inbox_identity_uses_agent_then_harness_then_client_defaults() {
        use crate::identity::{Address, inbox_address};
        use std::ffi::OsStr;
        let host = HostId::from_bytes([1; 16]);
        let mut config = config();
        config.agent_label = "configured-chair".into();
        let address = |config: &FabricConfig, agent, client| {
            inbox_address(host, config, agent, client).unwrap()
        };
        assert_eq!(
            address(&config, Some(OsStr::new("explicit-chair")), Some("codex")),
            Address::new(host, "explicit-chair").unwrap()
        );
        assert_eq!(
            address(&config, Some(OsStr::new("")), Some("codex")),
            Address::new(host, "configured-chair").unwrap()
        );
        config.agent_label.clear();
        assert_eq!(
            address(&config, None, Some("codex")),
            Address::new(host, "codex").unwrap()
        );
        assert_eq!(
            address(&config, None, None),
            Address::new(host, "agent").unwrap()
        );
    }

    fn config() -> FabricConfig {
        let host = HostId::from_bytes([1; 16]);
        FabricConfig {
            agent_label: "agent".into(),
            client_label: "agent".into(),
            home_hosts: BTreeMap::new(),
            peers: BTreeMap::from([(
                "worker".into(),
                Peer {
                    host,
                    destination: "worker".into(),
                    remote_executable: Some(PathBuf::from("/opt/provenant/bin/provenant")),
                    connect_timeout_seconds: 10,
                },
            )]),
            ssh_executable: Some(PathBuf::from("/opt/homebrew/bin/ssh")),
            lanes: Lanes::default(),
            limits: Limits {
                body_bytes: 65536,
                input_frame_bytes: 1048576,
                json_depth: 64,
                page_rows: 100,
                output_lines: 500,
                output_bytes: 65536,
                result_bytes: 1048576,
                lease_seconds: 3600,
                peer_seconds: 30,
                graph_timeout_seconds: 14400,
                bundle_bytes: 67108864,
            },
        }
    }

    #[test]
    fn lane_priority_defaults_and_bounds() {
        let mut config = config();
        assert_eq!(config.lanes.priority.for_lane(true, false), 10);
        assert_eq!(config.lanes.priority.for_lane(false, false), 5);
        assert_eq!(config.lanes.priority.for_lane(true, true), 15);
        assert_eq!(config.lanes.priority.for_lane(false, true), 15);
        let mut value = serde_json::to_value(&config).unwrap();
        value.as_object_mut().unwrap().remove("lanes");
        assert_eq!(
            serde_json::from_value::<FabricConfig>(value)
                .unwrap()
                .lanes
                .priority,
            LanePriority {
                writer: 10,
                read_only: 5,
                build_heavy: 15
            }
        );
        config.lanes.priority.writer = 20;
        assert!(
            config
                .validate()
                .iter()
                .any(|e| e.field == "lanes.priority.writer")
        );
        config.lanes.priority.writer = 0;
        config.lanes.priority.read_only = 19;
        assert!(config.validate().is_empty());
    }

    #[test]
    fn ssh_host_overrides_require_absolute_safe_paths_and_bounded_timeout() {
        let mut config = config();
        assert!(config.validate().is_empty());

        let mut value = serde_json::to_value(&config).unwrap();
        value["ssh_executable"] = serde_json::Value::String(String::new());
        assert_eq!(
            serde_json::from_value::<FabricConfig>(value)
                .unwrap()
                .ssh_executable,
            None
        );
        config.ssh_executable = None;
        let encoded = serde_json::to_vec(&config).unwrap();
        assert_eq!(
            serde_json::from_slice::<FabricConfig>(&encoded).unwrap(),
            config
        );
        config.ssh_executable = Some(PathBuf::from("/opt/homebrew/bin/ssh"));

        config.ssh_executable = Some(PathBuf::from("ssh"));
        assert!(!config.validate().is_empty());
        config.ssh_executable = Some(PathBuf::from("/opt/custom dir/ssh"));
        assert!(!config.validate().is_empty());
        config.ssh_executable = Some(PathBuf::from("/opt/homebrew/bin/ssh"));

        config.peers.get_mut("worker").unwrap().remote_executable =
            Some(PathBuf::from("relative/provenant"));
        assert!(!config.validate().is_empty());
        config.peers.get_mut("worker").unwrap().remote_executable =
            Some(PathBuf::from("/opt/provenant path/provenant"));
        assert!(!config.validate().is_empty());
        {
            let peer = config.peers.get_mut("worker").unwrap();
            peer.remote_executable = Some(PathBuf::from("/opt/provenant/bin/provenant"));
            peer.connect_timeout_seconds = 31;
        }
        assert!(!config.validate().is_empty());
    }
}
