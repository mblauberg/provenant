//! Fabric owns domain policy and complete compiled profile assets.
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, Profile, SectionError};
use provenant_core::HostId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricConfig {
    pub agent_label: String,
    pub client_label: String,
    /// Host-configured argv selected by adapter route name.
    #[serde(default)]
    pub commands: BTreeMap<String, CommandConfig>,
    /// Optional bounded host features, selected by a host-native route name.
    #[serde(default)]
    pub host_native: BTreeMap<String, HostNativeConfig>,
    #[serde(default, deserialize_with = "deserialize_ssh_executable")]
    pub ssh_executable: Option<std::path::PathBuf>,
    pub home_hosts: BTreeMap<String, HostId>,
    pub peers: BTreeMap<String, Peer>,
    pub limits: Limits,
    #[serde(default)]
    pub graph: GraphConfig,
    #[serde(default)]
    pub lanes: Lanes,
    /// The front door reads and validates pipeline experiments. The runtime only accepts the key.
    #[serde(default, skip_serializing)]
    pub pipelines: provenant_config::experiment::PipelineExperiments,
}

/// Named command policy supplied by host configuration, never by task prompts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfig {
    pub argv: Vec<String>,
    /// Absolute interpreter paths, granted execution only.
    #[serde(default)]
    pub runtimes: Vec<std::path::PathBuf>,
}

/// Only verified headless interfaces belong here. Each kind reuses its
/// provider's launch, account, confinement and output contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum HostNativeConfig {
    ClaudeAgent {
        model: String,
        #[serde(default)]
        effort: Option<String>,
        description: String,
        prompt: String,
    },
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
    /// What a graph `land` node does at each risk tier.
    pub land: GraphLand,
}

/// What a graph `land` node does once its PR is reviewed and green.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandPolicy {
    /// Merge through the repository's `land` workflow.
    Auto,
    /// Raise a node ask and merge only on a `land` answer.
    Ask,
}

/// One policy per risk tier. An unset tier falls through to the next layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LandTiers {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine: Option<LandPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub substantial: Option<LandPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crucial: Option<LandPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<LandPolicy>,
}

impl LandTiers {
    fn get(&self, tier: provenant_records::workflow::RiskTier) -> Option<LandPolicy> {
        use provenant_records::workflow::RiskTier;
        match tier {
            RiskTier::Routine => self.routine,
            RiskTier::Substantial => self.substantial,
            RiskTier::Crucial => self.crucial,
            RiskTier::Terminal => self.terminal,
        }
    }
}

/// Host-wide tier policy, with per-project overrides keyed by Fabric project ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphLand {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine: Option<LandPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub substantial: Option<LandPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crucial: Option<LandPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<LandPolicy>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<String, LandTiers>,
}

/// The effective policy for one tier and the layer it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandChoice {
    pub policy: LandPolicy,
    pub source: LandSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandSource {
    Default,
    Config,
    Project,
}

impl GraphLand {
    /// Routine and substantial land automatically; crucial and terminal ask.
    pub fn default_policy(tier: provenant_records::workflow::RiskTier) -> LandPolicy {
        use provenant_records::workflow::RiskTier;
        match tier {
            RiskTier::Routine | RiskTier::Substantial => LandPolicy::Auto,
            RiskTier::Crucial | RiskTier::Terminal => LandPolicy::Ask,
        }
    }

    /// Project entry, then host entry, then the compiled default.
    pub fn choose(
        &self,
        project: Option<&str>,
        tier: provenant_records::workflow::RiskTier,
    ) -> LandChoice {
        let host = LandTiers {
            routine: self.routine,
            substantial: self.substantial,
            crucial: self.crucial,
            terminal: self.terminal,
        };
        if let Some(policy) = project
            .and_then(|id| self.projects.get(id))
            .and_then(|tiers| tiers.get(tier))
        {
            LandChoice {
                policy,
                source: LandSource::Project,
            }
        } else if let Some(policy) = host.get(tier) {
            LandChoice {
                policy,
                source: LandSource::Config,
            }
        } else {
            LandChoice {
                policy: Self::default_policy(tier),
                source: LandSource::Default,
            }
        }
    }
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
    pub memory_pressure_threshold: provenant_config::MemoryPressureThreshold,
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
    #[serde(default, deserialize_with = "provenant_config::deserialize_auto")]
    pub lane_port_base: Option<u64>,
    #[serde(default, deserialize_with = "provenant_config::deserialize_auto")]
    pub lane_port_block: Option<u64>,
    #[serde(default, deserialize_with = "provenant_config::deserialize_auto")]
    pub lane_port_blocks: Option<u64>,
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
    /// What a settled lane tells the chair that dispatched it.
    pub settlement: Settlement,
}

/// Reactions to a lane settling, which run once per attempt. Each is a switch
/// with a default; none changes the outcome or any lane's authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settlement {
    /// Post one terse notice to the dispatching chair's inbox.
    pub notice: bool,
    /// Also notify for read-only lanes, which are probes by default.
    pub notice_read_only: bool,
    /// Append an entry to the dispatching chair's session log.
    pub chair_log: bool,
}
impl Default for Settlement {
    fn default() -> Self {
        Self {
            notice: true,
            notice_read_only: false,
            chair_log: true,
        }
    }
}

impl Lanes {
    pub fn build_settings(&self) -> provenant_config::LaneBuildSettings {
        provenant_config::LaneBuildSettings {
            memory_pressure_threshold: self.memory_pressure_threshold,
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
            lane_port_base: self.lane_port_base,
            lane_port_block: self.lane_port_block,
            lane_port_blocks: self.lane_port_blocks,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// Milliseconds a store waits for a contended lock; read through
    /// `provenant_config::StoreSettings` when a command starts.
    #[serde(default = "default_store_busy_timeout_ms")]
    pub store_busy_timeout_ms: u64,
}
fn default_graph_timeout_seconds() -> u64 {
    14_400
}
fn default_store_busy_timeout_ms() -> u64 {
    30_000
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitsConfig {
    body_bytes: u64,
    input_frame_bytes: u64,
    json_depth: u64,
    page_rows: u64,
    output_lines: u64,
    output_bytes: u64,
    result_bytes: u64,
    lease_seconds: u64,
    peer_seconds: u64,
    #[serde(default = "default_graph_timeout_seconds")]
    graph_timeout_seconds: u64,
    #[serde(default = "default_store_busy_timeout_ms")]
    store_busy_timeout_ms: u64,
    #[serde(
        rename = "bundle_bytes",
        default,
        deserialize_with = "ignore_bundle_bytes"
    )]
    _bundle_bytes: Option<()>,
}

const BUNDLE_BYTES_WARNING: &str = "warning: domains.fabric.limits.bundle_bytes is deprecated and ignored; Git transport no longer uses bundle files";

#[allow(clippy::print_stderr)]
fn ignore_bundle_bytes<'de, D>(deserializer: D) -> Result<Option<()>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::de::IgnoredAny::deserialize(deserializer)?;
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| eprintln!("{BUNDLE_BYTES_WARNING}"));
    Ok(Some(()))
}

impl<'de> Deserialize<'de> for Limits {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let limits = LimitsConfig::deserialize(deserializer)?;
        Ok(Self {
            body_bytes: limits.body_bytes,
            input_frame_bytes: limits.input_frame_bytes,
            json_depth: limits.json_depth,
            page_rows: limits.page_rows,
            output_lines: limits.output_lines,
            output_bytes: limits.output_bytes,
            result_bytes: limits.result_bytes,
            lease_seconds: limits.lease_seconds,
            peer_seconds: limits.peer_seconds,
            graph_timeout_seconds: limits.graph_timeout_seconds,
            store_busy_timeout_ms: limits.store_busy_timeout_ms,
        })
    }
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
        for (name, command) in &self.commands {
            let argv = &command.argv;
            if name.is_empty()
                || name.contains('\0')
                || argv.is_empty()
                || argv[0].is_empty()
                || argv.iter().any(|arg| arg.contains('\0'))
                || command.runtimes.iter().any(|path| {
                    !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0)
                })
            {
                reject(format!("commands.{name}"));
            }
        }
        for (name, native) in &self.host_native {
            let HostNativeConfig::ClaudeAgent {
                model,
                effort,
                description,
                prompt,
            } = native;
            let text =
                |value: &str, max| !value.is_empty() && value.len() <= max && !value.contains('\0');
            if !crate::graph::valid_identifier(name)
                || !model
                    .strip_prefix("claude/")
                    .is_some_and(|id| text(id, 256))
                || effort.as_deref().is_some_and(|value| !text(value, 64))
                || !text(description, 1024)
                || !text(prompt, 65_536)
            {
                reject(format!("host_native.{name}"));
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
        ] {
            if value == 0 || value > cap {
                reject(format!("limits.{field}"));
            }
        }
        if l.graph_timeout_seconds == 0 || l.graph_timeout_seconds > 14_400 {
            reject("limits.graph_timeout_seconds".into());
        }
        if l.store_busy_timeout_ms > provenant_config::STORE_BUSY_TIMEOUT_CEILING_MS {
            reject("limits.store_busy_timeout_ms".into());
        }
        if self
            .graph
            .allowed_read_roots
            .iter()
            .any(|root| !std::path::Path::new(root).is_absolute())
        {
            reject("graph.allowed_read_roots".into());
        }
        for project in self.graph.land.projects.keys() {
            if !crate::graph::valid_identifier(project) {
                reject(format!("graph.land.projects.{project}"));
            }
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
/// Protocol caps, graph limits and budgets, read roots and executables are a
/// boundary: recovery could raise a cap, lift a budget or swap a binary. Labels,
/// peers' addresses and lane tuning recover to defaults.
pub const FIELD_METADATA: &[FieldMetadata] = &[
    FieldMetadata::enumeration(
        "lanes.memory_pressure_threshold",
        &["warning", "critical", "never"],
    ),
    FieldMetadata::int("lanes.max_concurrent_builds", 1, 64),
    FieldMetadata::int("lanes.cargo_build_jobs", 1, 1024),
    FieldMetadata::int("lanes.max_writer_lanes", 1, 64),
    FieldMetadata::int("lanes.max_project_writer_lanes", 1, 32),
    FieldMetadata::int("lanes.build_wait_minutes", 1, 1440),
    FieldMetadata::int("lanes.lane_port_base", 1024, 65535),
    FieldMetadata::int("lanes.lane_port_block", 1, 1024),
    FieldMetadata::int("lanes.lane_port_blocks", 0, 1024),
    FieldMetadata::int("peers.{peer}.connect_timeout_seconds", 1, 30),
    FieldMetadata::new("commands.{name}.argv", "array"),
    FieldMetadata::new("home_hosts.{host}", "string"),
];
pub const SECTION: DomainSection = DomainSection::new::<FabricConfig>("fabric", defaults)
    .with_boundary_check(boundary)
    .with_fields(FIELD_METADATA);

fn boundary(error: &SectionError) -> bool {
    let field = error.field.as_str();
    let under = |path: &str| {
        field == path
            || field.starts_with(&format!("{path}."))
            // A malformed ancestor would drop the boundary value with it.
            || path.starts_with(&format!("{field}."))
            || field.is_empty()
    };
    [
        "limits",
        "graph.limits",
        "graph.budget",
        "graph.allowed_read_roots",
        "ssh_executable",
        "home_hosts",
    ]
    .into_iter()
    .any(under)
        || field
            .strip_prefix("peers.")
            .and_then(|rest| rest.split_once('.'))
            .is_some_and(|(_, leaf)| leaf == "remote_executable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn configured_commands_keep_literal_argv_and_validate_executable_and_nuls() {
        let mut config = config();
        config.commands.insert(
            "fmt-check".into(),
            CommandConfig {
                argv: vec!["cargo".into(), "fmt".into(), "a b;$(literal)".into()],
                runtimes: vec!["/opt/check/bin/python3".into()],
            },
        );
        assert!(config.validate().is_empty());
        assert_eq!(
            serde_json::from_value::<FabricConfig>(serde_json::to_value(&config).unwrap()).unwrap(),
            config
        );
        for argv in [
            vec![],
            vec![String::new()],
            vec!["cargo\0".into()],
            vec!["cargo".into(), "arg\0".into()],
        ] {
            config.commands.get_mut("fmt-check").unwrap().argv = argv;
            assert!(
                config
                    .validate()
                    .iter()
                    .any(|error| error.field == "commands.fmt-check")
            );
        }
    }

    #[test]
    fn configured_command_runtimes_require_absolute_paths() {
        let mut config = config();
        let command: CommandConfig = serde_json::from_value(serde_json::json!({
            "argv": ["./check.py"],
            "runtimes": ["/opt/check/bin/python3", "/opt/check/bin/ruby"]
        }))
        .unwrap();
        config.commands.insert("check".into(), command);
        assert!(config.validate().is_empty());
        let defaulted: CommandConfig = serde_json::from_value(serde_json::json!({
            "argv": ["cargo", "fmt"]
        }))
        .unwrap();
        assert!(defaulted.runtimes.is_empty());
        for path in ["python3", "./bin/python3", "/opt/check/bin/python3\0"] {
            config.commands.get_mut("check").unwrap().runtimes = vec![path.into()];
            assert!(
                config
                    .validate()
                    .iter()
                    .any(|error| error.field == "commands.check")
            );
        }
        assert!(
            serde_json::from_value::<CommandConfig>(serde_json::json!(["cargo", "fmt"])).is_err()
        );
        assert!(
            serde_json::from_value::<CommandConfig>(serde_json::json!({
                "argv": ["cargo"], "runtime": ["/opt/check/bin/python3"]
            }))
            .is_err()
        );
    }

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

    #[test]
    fn host_native_entries_are_opt_in_and_validate_their_declared_kind() {
        let mut value = serde_json::to_value(config()).unwrap();
        value["host_native"] = serde_json::json!({"ci-fixer": {
            "kind": "claude-agent", "model": "claude/claude-sonnet-5-5",
            "effort": "medium", "description": "Repair CI", "prompt": "Fix the failing checks."
        }});
        let parsed: FabricConfig = serde_json::from_value(value.clone()).unwrap();
        let errors = parsed.validate();
        assert!(errors.is_empty(), "{errors:?}");
        for (field, invalid) in [
            ("model", "codex/model"),
            ("effort", ""),
            ("description", ""),
            ("prompt", "bad\0prompt"),
        ] {
            let mut invalid_value = value.clone();
            invalid_value["host_native"]["ci-fixer"][field] = invalid.into();
            let parsed: FabricConfig = serde_json::from_value(invalid_value).unwrap();
            assert!(
                parsed
                    .validate()
                    .iter()
                    .any(|error| error.field == "host_native.ci-fixer"),
                "{field}"
            );
        }
        value["host_native"]["ci-fixer"]["kind"] = "unverified-fixer".into();
        assert!(serde_json::from_value::<FabricConfig>(value).is_err());
    }

    fn config() -> FabricConfig {
        let host = HostId::from_bytes([1; 16]);
        FabricConfig {
            agent_label: "agent".into(),
            client_label: "agent".into(),
            commands: BTreeMap::new(),
            host_native: BTreeMap::new(),
            pipelines: Default::default(),
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
                store_busy_timeout_ms: 30_000,
            },
        }
    }

    #[test]
    fn retired_bundle_bytes_config_still_loads_and_is_omitted() {
        let mut value = serde_json::to_value(config()).unwrap();
        value["limits"]["bundle_bytes"] = 67_108_864.into();
        let parsed: FabricConfig = serde_json::from_value(value).unwrap();
        assert!(
            !serde_json::to_value(parsed).unwrap()["limits"]
                .as_object()
                .unwrap()
                .contains_key("bundle_bytes")
        );
        assert!(BUNDLE_BYTES_WARNING.contains("deprecated and ignored"));
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
    fn land_policy_layers_project_then_host_then_default() {
        use provenant_records::workflow::RiskTier;
        let mut land = GraphLand::default();
        let choose = |land: &GraphLand, project, tier| land.choose(project, tier);
        assert_eq!(
            choose(&land, None, RiskTier::Routine),
            LandChoice {
                policy: LandPolicy::Auto,
                source: LandSource::Default
            }
        );
        assert_eq!(
            choose(&land, None, RiskTier::Substantial).policy,
            LandPolicy::Auto
        );
        assert_eq!(
            choose(&land, None, RiskTier::Crucial).policy,
            LandPolicy::Ask
        );
        assert_eq!(
            choose(&land, None, RiskTier::Terminal).policy,
            LandPolicy::Ask
        );
        land =
            serde_json::from_str(r#"{"crucial":"auto","projects":{"p1":{"substantial":"ask"}}}"#)
                .unwrap();
        assert_eq!(
            choose(&land, Some("p1"), RiskTier::Substantial),
            LandChoice {
                policy: LandPolicy::Ask,
                source: LandSource::Project
            }
        );
        assert_eq!(
            choose(&land, Some("p2"), RiskTier::Substantial).source,
            LandSource::Default
        );
        assert_eq!(
            choose(&land, Some("p1"), RiskTier::Crucial),
            LandChoice {
                policy: LandPolicy::Auto,
                source: LandSource::Config
            }
        );
        assert!(serde_json::from_str::<GraphLand>(r#"{"crucial":"never"}"#).is_err());
        assert!(serde_json::from_str::<GraphLand>(r#"{"urgent":"ask"}"#).is_err());
    }

    #[test]
    fn graph_policy_defaults_and_validation() {
        let mut config = config();
        assert_eq!(config.limits.store_busy_timeout_ms, 30_000);
        config.limits.store_busy_timeout_ms = 0;
        assert!(config.validate().is_empty());
        config.limits.store_busy_timeout_ms = 600_001;
        assert!(
            config
                .validate()
                .iter()
                .any(|e| e.field == "limits.store_busy_timeout_ms")
        );
        config.limits.store_busy_timeout_ms = 30_000;
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
            // A zero budget must not recover to unlimited.
            (
                "schema_version = 1\n[domains.fabric.graph.budget.per_graph]\ntokens = 0\n",
                "graph.budget.per_graph.tokens",
                "rejected value: 0",
                true,
                "range",
            ),
            (
                "schema_version = 1\n[domains.fabric.graph.budget.per_node]\nwall_time_seconds = 0\n",
                "graph.budget.per_node.wall_time_seconds",
                "rejected value: 0",
                true,
                "range",
            ),
            // Configuration may only lower a protocol cap.
            (
                "schema_version = 1\n[domains.fabric.limits]\nlease_seconds = 3601\n",
                "limits.lease_seconds",
                "3601",
                false,
                "range",
            ),
        ] {
            let error =
                provenant_config::load_with_domains(&roots, Some(text.as_bytes()), &sections)
                    .err()
                    .unwrap();
            let provenant_config::ConfigError::Domain { source, .. } = &error else {
                panic!("unexpected refusal: {error:?}");
            };
            assert_eq!(source.field, format!("domains.fabric.{key}"), "{error}");
            assert_eq!(source.code, rule, "{error}");
            assert_eq!(error.to_string().contains(rejected), should_echo, "{error}");
        }

        // Lane tuning is not a boundary: it recovers with a warning that names
        // the field, keeping a valid budget beside it.
        let config = provenant_config::load_with_domains(
            &roots,
            Some(&b"schema_version = 1\n[domains.fabric.lanes.priority]\nwriter = 40\n[domains.fabric.graph.budget.per_graph]\ntokens = 1000\n"[..]),
            &sections,
        )
        .unwrap();
        assert_eq!(config.domains.fabric.lanes.priority.writer, 10);
        assert_eq!(
            config.domains.fabric.graph.budget.per_graph.tokens,
            Some(1000)
        );
        assert!(
            config.warnings[0]
                .message
                .starts_with("domains.fabric.lanes.priority.writer:")
        );
    }
}
