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
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GraphConfig {
    pub limits: GraphLimits,
    pub limit_ceiling: GraphLimits,
    pub budget: GraphBudget,
}

// Omitted ceiling fields inherit the corresponding resolved host limit. The
// compiled asset leaves the ceiling absent so overrides precede this fallback.
impl<'de> Deserialize<'de> for GraphConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct RawGraphConfig {
            limits: GraphLimits,
            limit_ceiling: crate::graph::GraphLimitOverrides,
            budget: GraphBudget,
        }
        let raw = RawGraphConfig::deserialize(deserializer)?;
        let mut ceiling = raw.limits.clone();
        macro_rules! inherit {
            ($($field:ident),+ $(,)?) => {$(
                if let Some(value) = raw.limit_ceiling.$field {
                    ceiling.$field = value;
                }
            )+};
        }
        inherit!(
            nodes,
            edges,
            routes_per_node,
            node_prompt_bytes,
            graph_prompt_bytes,
            expansion_depth,
            fan_out_width
        );
        if let Some(value) = raw.limit_ceiling.concurrency {
            ceiling.concurrency = (value != 0).then_some(value);
        }
        Ok(Self {
            limits: raw.limits,
            limit_ceiling: ceiling,
            budget: raw.budget,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphLimits {
    pub nodes: usize,
    pub edges: usize,
    pub routes_per_node: usize,
    pub node_prompt_bytes: usize,
    pub graph_prompt_bytes: usize,
    pub expansion_depth: usize,
    pub fan_out_width: usize,
    #[serde(
        deserialize_with = "deserialize_concurrency",
        serialize_with = "serialize_concurrency"
    )]
    pub concurrency: Option<usize>,
}
fn serialize_concurrency<S: serde::Serializer>(
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
            nodes: 512,
            edges: 4096,
            routes_per_node: 16,
            node_prompt_bytes: 256 * 1024,
            graph_prompt_bytes: 4 * 1024 * 1024,
            expansion_depth: 4,
            fan_out_width: 32,
            concurrency: None,
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
        let limits = &self.graph.limits;
        let ceiling = &self.graph.limit_ceiling;
        for (name, value, maximum) in [
            ("nodes", limits.nodes, ceiling.nodes),
            ("edges", limits.edges, ceiling.edges),
            (
                "routes_per_node",
                limits.routes_per_node,
                ceiling.routes_per_node,
            ),
            (
                "node_prompt_bytes",
                limits.node_prompt_bytes,
                ceiling.node_prompt_bytes,
            ),
            (
                "graph_prompt_bytes",
                limits.graph_prompt_bytes,
                ceiling.graph_prompt_bytes,
            ),
            (
                "expansion_depth",
                limits.expansion_depth,
                ceiling.expansion_depth,
            ),
            ("fan_out_width", limits.fan_out_width, ceiling.fan_out_width),
        ] {
            if maximum == 0 && value != 0 {
                reject(format!("graph.limit_ceiling.{name}"));
            }
            if value == 0 || value > maximum {
                reject(format!("graph.limits.{name}"));
            }
        }
        if matches!((limits.concurrency, ceiling.concurrency), (Some(value), Some(maximum)) if value > maximum)
        {
            reject("graph.limits.concurrency".into());
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
    fn graph_policy_defaults_and_validation() {
        let mut config = config();
        assert_eq!(config.graph.limits.nodes, 512);
        assert_eq!(config.graph.limits.graph_prompt_bytes, 4 * 1024 * 1024);
        assert!(config.graph.budget.per_graph.tokens.is_none());
        config.graph.limits.nodes = 513;
        assert!(
            config
                .validate()
                .iter()
                .any(|e| e.field == "graph.limits.nodes")
        );
        config.graph.limit_ceiling.nodes = 1024;
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
    fn graph_ceilings_inherit_each_configured_limit() {
        let graph: GraphConfig = serde_json::from_value(serde_json::json!({
            "limits": {"nodes": 10, "edges": 20, "concurrency": 3},
            "limit_ceiling": {"edges": 30}
        }))
        .unwrap();
        assert_eq!(graph.limit_ceiling.nodes, 10);
        assert_eq!(graph.limit_ceiling.edges, 30);
        assert_eq!(graph.limit_ceiling.concurrency, Some(3));
        assert_eq!(graph.budget.unknown_usage, UnknownUsage::Warn);

        let unlimited: GraphConfig = serde_json::from_value(serde_json::json!({
            "limits": {"concurrency": 3},
            "limit_ceiling": {"concurrency": 0},
            "budget": {"unknown_usage": "pause"}
        }))
        .unwrap();
        assert_eq!(unlimited.limit_ceiling.concurrency, None);
        assert_eq!(unlimited.budget.unknown_usage, UnknownUsage::Pause);
        let round_trip: GraphConfig =
            serde_json::from_value(serde_json::to_value(&unlimited).unwrap()).unwrap();
        assert_eq!(round_trip, unlimited);
    }

    #[test]
    fn configured_graph_ceilings_and_diagnostics_survive_config_layering() {
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
        let text = b"schema_version = 1\n[domains.fabric.graph.limits]\nnodes = 10\nedges = 20\n[domains.fabric.graph.limit_ceiling]\nedges = 30\n";
        let config =
            provenant_config::load_with_domains(&roots, Some(&text[..]), &sections).unwrap();
        assert_eq!(config.domains.fabric.graph.limit_ceiling.nodes, 10);
        assert_eq!(config.domains.fabric.graph.limit_ceiling.edges, 30);

        let text = b"schema_version = 1\n[domains.fabric.graph.limits]\nnodes = 11\n[domains.fabric.graph.limit_ceiling]\nnodes = 10\n";
        let error = provenant_config::load_with_domains(&roots, Some(&text[..]), &sections)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("graph.limits.nodes"), "{error}");
        assert!(error.contains("11"), "{error}");

        for (text, key, rejected) in [
            (
                "schema_version = 1\n[domains.fabric.graph.limits]\nnodes = 0\n",
                "graph.limits.nodes",
                "0",
            ),
            (
                "schema_version = 1\n[domains.fabric.graph.limits]\nnodes = 'wrong'\n",
                "graph.limits.nodes",
                "wrong",
            ),
            (
                "schema_version = 1\n[domains.fabric.graph.budget]\nunknown_usage = 'bad'\n",
                "graph.budget.unknown_usage",
                "bad",
            ),
        ] {
            let error =
                provenant_config::load_with_domains(&roots, Some(text.as_bytes()), &sections)
                    .err()
                    .unwrap()
                    .to_string();
            assert!(error.contains(key), "{error}");
            assert!(error.contains(rejected), "{error}");
        }
    }
}
