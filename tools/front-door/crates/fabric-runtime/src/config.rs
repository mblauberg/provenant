//! Fabric owns domain policy and complete compiled profile assets.
use provenant_config::{DomainConfig, DomainSection, Profile, SectionError};
use provenant_core::HostId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub graph: GraphConfig,
    #[serde(default)]
    pub lanes: Lanes,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::manual_non_exhaustive)] // Consumes a retired config key without retaining its value.
pub struct GraphConfig {
    pub limits: GraphLimits,
    pub budget: GraphBudget,
    /// Backoff for pending CI observations; node overrides take precedence.
    pub ci_poll_backoff: crate::graph::CiPollBackoff,
    pub ci_poll_overrides: BTreeMap<String, crate::graph::CiPollBackoff>,
    #[serde(
        rename = "limit_ceiling",
        skip_serializing,
        deserialize_with = "ignore_limit_ceiling"
    )]
    _limit_ceiling: (),
    /// Absolute host directories graph nodes may read besides the project's own
    /// registered worktrees. Empty confines read roots to the project.
    pub allowed_read_roots: Vec<String>,
}

const LIMIT_CEILING_WARNING: &str = "warning: domains.fabric.graph.limit_ceiling is deprecated and ignored; host graph limits apply";

#[allow(clippy::print_stderr)]
fn ignore_limit_ceiling<'de, D>(deserializer: D) -> Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::de::IgnoredAny::deserialize(deserializer)?;
    // Validation and loading can deserialize the same configuration section.
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| eprintln!("{LIMIT_CEILING_WARNING}"));
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphLimits {
    pub continuations: usize,
    pub nodes: usize,
    pub edges: usize,
    pub routes_per_node: usize,
    pub node_prompt_bytes: usize,
    pub graph_prompt_bytes: usize,
    pub expansion_depth: usize,
    pub fan_out_width: usize,
    #[serde(
        deserialize_with = "deserialize_concurrency",
        serialize_with = "serialise_concurrency"
    )]
    pub concurrency: Option<usize>,
}
fn serialise_concurrency<S: serde::Serializer>(
    value: &Option<usize>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_u64(value.unwrap_or(0) as u64)
}
fn deserialize_concurrency<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<usize>::deserialize(deserializer)?.filter(|value| *value != 0))
}
impl Default for GraphLimits {
    fn default() -> Self {
        Self {
            continuations: 2,
            nodes: 512,
            edges: 4096,
            routes_per_node: 16,
            node_prompt_bytes: 256 * 1024,
            graph_prompt_bytes: 4 * 1024 * 1024,
            expansion_depth: 64,
            fan_out_width: 32,
            concurrency: None,
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Lanes {
    pub priority: LanePriority,
    pub cargo_seed: CargoSeed,
    #[serde(default, deserialize_with = "provenant_config::deserialize_auto")]
    pub max_concurrent_builds: Option<u64>,
    #[serde(default, deserialize_with = "provenant_config::deserialize_auto")]
    pub cargo_build_jobs: Option<u64>,
    #[serde(default, deserialize_with = "provenant_config::deserialize_auto")]
    pub max_writer_lanes: Option<u64>,
    #[serde(default, deserialize_with = "provenant_config::deserialize_auto")]
    pub max_project_writer_lanes: Option<u64>,
    #[serde(default, deserialize_with = "provenant_config::deserialize_auto")]
    pub build_wait_minutes: Option<u64>,
    /// `CARGO_INCREMENTAL` for lanes; off unless set.
    pub cargo_incremental: Option<bool>,
    /// `CARGO_PROFILE_DEV_DEBUG` for lanes; `line-tables-only` unless set.
    pub cargo_debug: Option<String>,
    /// Cap on the allocated bytes of all lane Cargo profile directories, idle
    /// and busy; see `provenant_config::LaneBuildSettings`.
    pub build_dir_max_bytes: Option<u64>,
    /// Seconds after its newest write during which a profile directory is
    /// never removed; 15 minutes unless set.
    pub build_dir_grace_seconds: Option<u64>,
    /// Seconds between settlement-triggered walks of the lane caches; 5
    /// minutes unless set.
    pub build_dir_survey_interval_seconds: Option<u64>,
}

impl Lanes {
    pub fn build_settings(&self) -> provenant_config::LaneBuildSettings {
        provenant_config::LaneBuildSettings {
            max_concurrent_builds: self.max_concurrent_builds,
            cargo_build_jobs: self.cargo_build_jobs,
            cargo_incremental: self.cargo_incremental,
            cargo_debug: self.cargo_debug.clone(),
            build_dir_max_bytes: self.build_dir_max_bytes,
            build_dir_grace_seconds: self.build_dir_grace_seconds,
            build_dir_survey_interval_seconds: self.build_dir_survey_interval_seconds,
            max_writer_lanes: self.max_writer_lanes,
            max_project_writer_lanes: self.max_project_writer_lanes,
            build_wait_minutes: self.build_wait_minutes,
        }
    }
}

/// Optional host Cargo seeds for writer lanes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CargoSeed {
    pub enabled: bool,
    /// Maximum number of published target generations to retain.
    pub max_generations: usize,
    /// Maximum total apparent bytes in published target generations.
    pub max_bytes: u64,
}

impl Default for CargoSeed {
    fn default() -> Self {
        Self {
            enabled: false,
            max_generations: 3,
            max_bytes: 20 * 1024 * 1024 * 1024,
        }
    }
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphBudget {
    pub unknown_usage: UnknownUsage,
    pub per_node: BudgetLimit,
    pub per_graph: BudgetLimit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownUsage {
    #[default]
    Warn,
    Pause,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BudgetLimit {
    pub tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub wall_time_seconds: Option<u64>,
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
        if self
            .graph
            .allowed_read_roots
            .iter()
            .any(|root| !std::path::Path::new(root).is_absolute())
        {
            reject("graph.allowed_read_roots".into());
        }
        let limits = &self.graph.limits;
        for (name, value) in [
            ("nodes", limits.nodes),
            ("edges", limits.edges),
            ("routes_per_node", limits.routes_per_node),
            ("node_prompt_bytes", limits.node_prompt_bytes),
            ("graph_prompt_bytes", limits.graph_prompt_bytes),
            ("expansion_depth", limits.expansion_depth),
            ("fan_out_width", limits.fan_out_width),
        ] {
            if value == 0 {
                reject(format!("graph.limits.{name}"));
            }
        }
        for (name, budget) in [
            ("per_node", &self.graph.budget.per_node),
            ("per_graph", &self.graph.budget.per_graph),
        ] {
            if budget.tokens == Some(0) {
                reject(format!("graph.budget.{name}.tokens"));
            }
            if budget.wall_time_seconds == Some(0) {
                reject(format!("graph.budget.{name}.wall_time_seconds"));
            }
            if budget
                .cost_usd
                .is_some_and(|cost| !cost.is_finite() || cost <= 0.0)
            {
                reject(format!("graph.budget.{name}.cost_usd"));
            }
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
        for key in self.lanes.build_settings().invalid_keys() {
            reject(format!("lanes.{key}"));
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
    fn lane_build_settings_validate_shared_bounds_and_auto() {
        let lanes: Lanes =
            serde_json::from_str(r#"{"max_concurrent_builds": -1, "cargo_build_jobs": -1}"#)
                .unwrap();
        assert_eq!(lanes.max_concurrent_builds, None);
        assert_eq!(lanes.cargo_build_jobs, None);
        let mut config = config();
        config.lanes.max_concurrent_builds = Some(0);
        config.lanes.cargo_build_jobs = Some(1025);
        config.lanes.max_writer_lanes = Some(65);
        config.lanes.max_project_writer_lanes = Some(0);
        config.lanes.build_wait_minutes = Some(0);
        let errors = config.validate();
        assert!(
            errors
                .iter()
                .any(|e| e.field == "lanes.max_concurrent_builds")
        );
        assert!(errors.iter().any(|e| e.field == "lanes.cargo_build_jobs"));
        for key in [
            "max_writer_lanes",
            "max_project_writer_lanes",
            "build_wait_minutes",
        ] {
            assert!(errors.iter().any(|e| e.field == format!("lanes.{key}")));
        }
    }

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
            graph: GraphConfig::default(),
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
        assert!(!config.lanes.cargo_seed.enabled);
        assert_eq!(config.lanes.cargo_seed.max_generations, 3);
        assert_eq!(config.lanes.cargo_seed.max_bytes, 20 * 1024 * 1024 * 1024);
        assert_eq!(config.lanes.priority.for_lane(true, false), 10);
        assert_eq!(config.lanes.priority.for_lane(false, false), 5);
        assert_eq!(config.lanes.priority.for_lane(true, true), 15);
        assert_eq!(config.lanes.priority.for_lane(false, true), 15);
        let mut value = serde_json::to_value(&config).unwrap();
        value["lanes"]["cargo_seed"]["enabled"] = true.into();
        value["lanes"]["cargo_seed"]["max_generations"] = 0.into();
        value["lanes"]["cargo_seed"]["max_bytes"] = 0.into();
        let limits = serde_json::from_value::<FabricConfig>(value.clone())
            .unwrap()
            .lanes
            .cargo_seed;
        assert_eq!((limits.max_generations, limits.max_bytes), (0, 0));
        assert!(
            serde_json::from_value::<FabricConfig>(value.clone())
                .unwrap()
                .lanes
                .cargo_seed
                .enabled
        );
        value.as_object_mut().unwrap().remove("lanes");
        assert!(
            !serde_json::from_value::<FabricConfig>(value.clone())
                .unwrap()
                .lanes
                .cargo_seed
                .enabled
        );
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

    #[test]
    fn retired_graph_ceiling_is_ignored_but_other_config_keys_remain_strict() {
        let mut value = serde_json::to_value(config()).unwrap();
        value["graph"]["limit_ceiling"] = serde_json::json!({"nodes": 1, "future": true});
        let loaded: FabricConfig = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(loaded.graph, GraphConfig::default());
        assert!(
            serde_json::to_value(&loaded).unwrap()["graph"]
                .get("limit_ceiling")
                .is_none()
        );
        assert_eq!(LIMIT_CEILING_WARNING.lines().count(), 1);
        assert!(LIMIT_CEILING_WARNING.contains("deprecated and ignored"));

        for pointer in ["graph", "limits", "peers"] {
            let mut unknown = value.clone();
            unknown[pointer]["unexpected"] = serde_json::json!({"nodes": 1});
            assert!(serde_json::from_value::<FabricConfig>(unknown).is_err());
        }
        value["graph"]["limits"]["unexpected"] = true.into();
        assert!(serde_json::from_value::<FabricConfig>(value).is_err());
    }

    #[test]
    fn graph_policy_defaults_and_validation() {
        let mut config = config();
        assert_eq!(config.graph.limits.expansion_depth, 64);
        assert_eq!(config.graph.limits.nodes, 512);
        assert_eq!(config.graph.limits.continuations, 2);
        config.graph.limits.continuations = 0;
        assert!(config.validate().is_empty());
        assert_eq!(config.graph.limits.graph_prompt_bytes, 4 * 1024 * 1024);
        assert!(config.graph.budget.per_graph.tokens.is_none());
        config.graph.limits.nodes = 513;
        assert!(config.validate().is_empty());
        config.graph.budget.per_node.cost_usd = Some(-1.0);
        assert!(
            config
                .validate()
                .iter()
                .any(|e| e.field == "graph.budget.per_node.cost_usd")
        );
    }

    #[test]
    fn configured_graph_limits_and_diagnostics_survive_config_layering() {
        #[derive(Serialize, Deserialize)]
        struct Domains {
            fabric: FabricConfig,
        }
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &std::path::Path,
            ) -> std::io::Result<PathBuf> {
                Ok(path.to_owned())
            }
        }
        let mut inputs =
            provenant_core::RootInputs::new("/fixture/home".into(), "/fixture/checkout".into());
        inputs.state_dir = Some("/fixture/state".into());
        let roots = inputs.resolve(&Resolver).unwrap();
        let sections = provenant_config::DomainSections::<Domains>::new(&[SECTION]);
        let text = b"schema_version = 1\n[domains.fabric.graph.limits]\nnodes = 10\nedges = 20\n[domains.fabric.graph.limit_ceiling]\nnodes = 1\n[domains.fabric.graph.ci_poll_backoff]\ninitial_seconds = 60\nmax_seconds = 300\n[domains.fabric.graph.ci_poll_overrides.build]\ninitial_seconds = 120\nmax_seconds = 600\n";
        let config =
            provenant_config::load_with_domains(&roots, Some(&text[..]), &sections).unwrap();
        assert_eq!(config.domains.fabric.graph.limits.nodes, 10);
        assert_eq!(config.domains.fabric.graph.limits.edges, 20);
        assert_eq!(
            config.domains.fabric.graph.ci_poll_backoff.initial_seconds,
            60
        );
        assert_eq!(config.domains.fabric.graph.ci_poll_backoff.max_seconds, 300);
        assert_eq!(
            config.domains.fabric.graph.ci_poll_overrides["build"].initial_seconds,
            120
        );
        assert!(matches!(
            config.origins["domains.fabric.graph.ci_poll_backoff.initial_seconds"],
            provenant_config::Origin::User { .. }
        ));
        assert!(matches!(
            config.origins["domains.fabric.graph.ci_poll_overrides.build.max_seconds"],
            provenant_config::Origin::User { .. }
        ));
        let defaults =
            provenant_config::load_with_domains(&roots, None::<&[u8]>, &sections).unwrap();
        assert!(matches!(
            defaults.origins["domains.fabric.graph.ci_poll_backoff.initial_seconds"],
            provenant_config::Origin::Compiled { .. }
        ));

        for (text, key, rejected, should_echo, rule) in [
            (
                "schema_version = 1\n[domains.fabric.graph.limits]\nnodes = 0\n",
                "graph.limits.nodes",
                "rejected value: 0",
                true,
                "range",
            ),
            (
                "schema_version = 1\n[domains.fabric.graph.limits]\nnodes = 'private-sentinel'\n",
                "graph.limits.nodes",
                "private-sentinel",
                false,
                "type",
            ),
            (
                "schema_version = 1\n[domains.fabric.graph.budget]\nunknown_usage = 'bad'\n",
                "graph.budget.unknown_usage",
                "bad",
                false,
                "range",
            ),
        ] {
            let error =
                provenant_config::load_with_domains(&roots, Some(text.as_bytes()), &sections)
                    .err()
                    .unwrap()
                    .to_string();
            assert!(error.contains(key), "{error}");
            assert_eq!(error.contains(rejected), should_echo, "{error}");
            assert!(error.contains(rule), "{error}");
        }
    }
}
