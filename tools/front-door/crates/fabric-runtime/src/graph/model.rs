use super::*;

pub(super) fn is_false(value: &bool) -> bool {
    !value
}

/// Host retry policy sealed with the attempt before a launch can fail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRetryPolicy {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub limit: u8,
    pub limit_source: String,
    pub first_seconds: u64,
    pub max_seconds: u64,
}

impl Default for LaunchRetryPolicy {
    fn default() -> Self {
        Self {
            extra: Default::default(),
            limit: 3,
            limit_source: "default".into(),
            first_seconds: 5,
            max_seconds: 600,
        }
    }
}

/// Persisted launch allowance, sealed before writer binding, with an optional
/// retry wake after a transient failure. The allowance is retained for the
/// node's current attempt, so a retry observes or relaunches that attempt
/// without renewing its deadline or launching a duplicate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRetry {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    /// Retry limit and timing fixed for this attempt.
    pub policy: LaunchRetryPolicy,
    /// Failed launches so far, counting the initial launch.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub failures: u8,
    /// Absolute end of this attempt's launch admission window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<provenant_core::WallTime>,
    /// Earliest next launch, for the host tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<provenant_core::WallTime>,
    /// Why the last launch failed, bounded, kept for status while waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Canonical encoding length, or `usize::MAX` when the value cannot be encoded.
pub(super) fn canonical_len<T: Serialize>(value: &T) -> usize {
    CanonicalV1::bytes(value).map_or(usize::MAX, |bytes| bytes.as_bytes().len())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<provenant_records::run::WorkContext>,
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_branch: Option<String>,
    /// A graph may open one PR after all its nodes succeed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<provenant_records::run::DeliveryMode>,
    /// Inbox session for pause asks and decide nodes without their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_session: Option<String>,
    pub schema_version: u64,
    pub graph_id: String,
    pub nodes: Vec<GraphNode>,
}

/// Selects `model`, then `name`, then `alias`. A named model-less entry
/// requires `adapter`, as does a model route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphRoute {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    /// A trusted configuration entry for a model-less adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// The selector used when launching a graph route.
pub enum GraphRouteSelector<'a> {
    Model(&'a str),
    Adapter(&'a str),
    Alias(Option<&'a str>),
}

impl GraphRoute {
    pub fn selector(&self) -> GraphRouteSelector<'_> {
        match (self.model.as_deref(), self.name.as_deref()) {
            (Some(model), _) => GraphRouteSelector::Model(model),
            (None, Some(name)) => GraphRouteSelector::Adapter(name),
            (None, None) => GraphRouteSelector::Alias(self.alias.as_deref()),
        }
    }
}

/// Host-only inputs retained for advancement after the submitting process exits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphExecution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_base: Option<provenant_core::GitOid>,
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    /// Integration head pinned before the graph's first node is admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_base: Option<provenant_core::GitOid>,
    pub directory: std::path::PathBuf,
    pub document: GraphDocument,
}

/// `route` remains the one-route form. A node may instead provide ordered
/// `routes`; supplying both is invalid.
/// A family constraint resolved at launch from the node's source writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistinctFrom {
    /// The nearest writing node along the source chain.
    Writer,
    /// Every upstream node this one depends on, transitively.
    Dependencies,
}

/// How firmly a node's family separation binds the router.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FamilyStrength {
    /// Substitute a route in an excluded family when none outside them is ready.
    #[default]
    Prefer,
    /// Fail the attempt rather than route inside an excluded family.
    Require,
}

/// How the router honoured a node's family separation, recorded at settlement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilySeparation {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub strength: FamilyStrength,
    /// Families that served the upstream attempts.
    pub excluded: Vec<String>,
    /// Sibling families avoided when possible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred: Vec<String>,
    /// Family of the attempt that settled, when it launched.
    pub applied: Option<String>,
    pub outcome: SeparationOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeparationOutcome {
    /// The attempt ran outside every excluded family.
    Separated,
    /// No route outside the excluded families was available.
    Unavailable,
    /// No upstream family was known, so nothing was excluded.
    Unconstrained,
}

// Wire presence is transient decoding metadata, not part of node identity.
#[derive(Clone, Debug, Default)]
pub(super) struct TopologyPresence(pub [bool; 2]);

impl PartialEq for TopologyPresence {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for TopologyPresence {}

/// The refusal for a graph, node or `--set` value that names cloud placement.
pub const CLOUD_PLACEMENT_REMOVED: &str = "invalid input field `attempt.cloud`: cloud placement was removed. Launch a cloud session with `provenant cloud launch --file BRIEF`";

/// Execution specification for an attempt, carrying what a plain lane expresses.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptSpec {
    /// Selected checkout for a live read-only child; omitted uses the graph directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    /// Peer name or host ID; omitted means local execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Require a confidential-safe route.
    #[serde(default, skip_serializing_if = "is_false")]
    pub confidential: bool,
    /// Provider execution timeout in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_timeout: Option<u64>,
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    /// A writer worktree branch created from base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_worktree: Option<crate::coordination::facade::NewWorktree>,
    /// Account label for routing and quota accounting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Explicit capabilities granted to this attempt.
    #[serde(default, alias = "capability", skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Use build-heavy scheduling priority.
    #[serde(default, alias = "build_weight", skip_serializing_if = "is_false")]
    pub build_heavy: bool,
    /// Explicit write roots outside the worktree.
    #[serde(default, alias = "write_root", skip_serializing_if = "Vec::is_empty")]
    pub write_roots: Vec<String>,
    /// Cargo commands this node's lane may run locally: `all`, `check` or
    /// `none`. Sets the `lanes.local_cargo` knob for this node alone; omitted
    /// inherits the run's and the host's setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_cargo: Option<String>,
    /// Bundled lane preset applied to this node: `light`. Fills the settings
    /// below that the node leaves unset; an explicit setting always wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// `shared` lets an unpinned read-only node read its bound checkout live;
    /// a node pinned to a commit always gets an immutable source view. Writers
    /// keep their one worktree, so `shared` is refused on a writer node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    /// `standard` (default) or `minimal` supervision of the lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervision: Option<String>,
    /// `delivery` (default) keeps the delivery receipt; `none` withholds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipts: Option<String>,
    /// `on` sandboxes this node's lane, `off` runs unsandboxed (overriding route,
    /// network and parent floors), and `auto` (default) runs unsandboxed for every
    /// lane kind. Sets the `lanes.sandbox` knob for this node alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    /// The node whose lane or plan added or replaced this one, set by patch
    /// admission. When that lane was sandboxed by distrust (its route, its
    /// parent or the knob), the graph owner sandboxes this node too. Admission
    /// overwrites any value an author supplies, and a value can only add a
    /// sandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_by: Option<String>,
    /// Set by the graph owner from `admitted_by` before launch, and carried to
    /// a peer. It only narrows: the lane is then sandboxed whatever `sandbox`
    /// says.
    #[serde(default, skip_serializing_if = "is_false")]
    pub parent_sandboxed: bool,
}

/// The `light` preset: no private source view, minimal supervision and no
/// delivery receipt. Secret redaction and object integrity are not settings,
/// so no preset can lower them.
pub const LIGHT_PRESET: &str = "light";

impl AttemptSpec {
    /// Whether the preset name is one this build bundles.
    pub fn preset_known(&self) -> bool {
        self.preset
            .as_deref()
            .is_none_or(|name| name == LIGHT_PRESET)
    }

    fn preset_default(&self, light: &'static str) -> Option<&str> {
        (self.preset.as_deref() == Some(LIGHT_PRESET)).then_some(light)
    }

    /// Effective isolation after the preset: `private` or `shared`.
    pub fn effective_isolation(&self) -> &str {
        self.isolation
            .as_deref()
            .or(self.preset_default("shared"))
            .unwrap_or("private")
    }

    /// Effective supervision after the preset: `standard` or `minimal`.
    pub fn effective_supervision(&self) -> &str {
        self.supervision
            .as_deref()
            .or(self.preset_default("minimal"))
            .unwrap_or("standard")
    }

    /// Effective receipts after the preset: `delivery` or `none`.
    pub fn effective_receipts(&self) -> &str {
        self.receipts
            .as_deref()
            .or(self.preset_default("none"))
            .unwrap_or("delivery")
    }

    /// `name=value` knob overrides for the settings this node sets or inherits
    /// from its preset.
    pub fn lane_knobs(&self) -> Vec<String> {
        let mut knobs = Vec::new();
        if self.effective_isolation() != "private" {
            knobs.push(format!("lanes.isolation={}", self.effective_isolation()));
        }
        if self.effective_supervision() != "standard" {
            knobs.push(format!(
                "lanes.supervision={}",
                self.effective_supervision()
            ));
        }
        if self.effective_receipts() != "delivery" {
            knobs.push(format!("lanes.receipts={}", self.effective_receipts()));
        }
        if let Some(sandbox) = &self.sandbox {
            knobs.push(format!("lanes.sandbox={sandbox}"));
        }
        knobs
    }

    /// Whether the spec still names the removed cloud placement. Such a node
    /// is refused rather than run locally.
    pub fn names_cloud_placement(&self) -> bool {
        ["cloud", "placement"]
            .iter()
            .any(|field| self.extra.contains_key(*field))
    }

    pub fn is_empty(&self) -> bool {
        self.host.is_none()
            && self.directory.is_none()
            && !self.confidential
            && self.provider_timeout.is_none()
            && self.new_worktree.is_none()
            && self.account.is_none()
            && self.capabilities.is_empty()
            && !self.build_heavy
            && self.write_roots.is_empty()
            && self.local_cargo.is_none()
            && self.preset.is_none()
            && self.isolation.is_none()
            && self.supervision.is_none()
            && self.receipts.is_none()
            && self.sandbox.is_none()
            && self.admitted_by.is_none()
            && !self.parent_sandboxed
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    /// The step to take after a stop with a given cause instead of the stop
    /// table's default (`docs/specs/run.md#what-follows-a-stop`).
    #[serde(default, skip_serializing_if = "provenant_records::OnStop::is_empty")]
    pub on_stop: provenant_records::OnStop,
    /// Attempt specification carrying placement, new worktree, account, capability, build weight and write roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<AttemptSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_group: Option<String>,
    /// Launch conditions resolved at submission; the node is skipped unless every term holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<WhenTerm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<provenant_records::run::WorkContext>,
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    #[serde(skip)]
    pub(super) composite_members: Vec<String>,
    // Expansion journal presence, in composite_members / after_expansion order.
    #[serde(skip)]
    pub(super) topology_present: TopologyPresence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composite: Option<CompositeSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Route this node away from the model families that served its source node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinct_from: Option<DistinctFrom>,
    /// `prefer` (the default) or `require`; applies when `distinct_from` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinct_strength: Option<FamilyStrength>,
    /// Dispositions each finding must carry at settlement, with a reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispositions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    /// Explicit node capability set; absent inherits the writer project default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceBinding>,
    /// Internal completion edges accept handled gates and inactive paths.
    #[serde(skip)]
    pub(super) after_expansion: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_scope: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_checks: Vec<String>,
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub loop_body: Option<LoopSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decide: Option<DecideSpec>,
    /// Continue this upstream writer's delivery branch in its worktree. The
    /// host fast-forward pushes the node's commits to that branch only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub land: Option<LandSpec>,
    #[serde(skip)]
    pub(super) round: Option<LoopRound>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<NodeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<GateCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_fail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<provenant_records::run::DeliveryMode>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_skill_location: bool,
    pub access: AccessMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Wall-clock limit for this node's lane, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_seconds: Option<u64>,
    #[serde(default, alias = "goal", alias = "brief")]
    pub prompt: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Allow dependants to proceed after this node fails or is blocked.
    #[serde(default, skip_serializing_if = "is_false")]
    pub advisory: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<GraphInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<GraphRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<Vec<GraphRoute>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Require this exact, case-sensitive line (ignoring surrounding whitespace)
    /// in a successful result. A missing line fails the node without fallback.
    pub required_verdict: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_findings: bool,
    /// On a CI gate's recovery node: skip when the gate failed with no
    /// findings. Unset, a node skips only if it takes the gate's findings as
    /// an input; `false` always runs it, `true` always skips it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_without_findings: Option<bool>,
}

impl GraphNode {
    /// Construct a composite boundary with the same defaults as a decoded node.
    pub fn composite(id: String, depends_on: Vec<String>, spec: CompositeSpec) -> Self {
        Self {
            id,
            kind: Some(NodeKind::Composite),
            access: AccessMode::ReadOnly,
            depends_on,
            composite: Some(spec),
            ..Self::default()
        }
    }

    pub(super) fn dependencies(&self) -> impl Iterator<Item = &String> {
        self.depends_on.iter().chain(
            self.after_expansion
                .iter()
                .filter(|name| !self.depends_on.contains(name)),
        )
    }
    pub(super) fn content_bytes(&self) -> usize {
        plans::prompt(self)
            .len()
            .saturating_add(match &self.condition {
                Some(GateCondition::Verdict { expected, .. }) => expected.len(),
                _ => 0,
            })
            .saturating_add(self.decide.as_ref().map_or(0, |spec| spec.question.len()))
    }
}

pub(super) fn expansion_safety_limits(
    limits: &crate::config::GraphLimits,
) -> crate::config::GraphLimits {
    crate::config::GraphLimits {
        nodes: limits.composite_expansion_safety_nodes(),
        // Composite nesting is itself governed by the host's configured
        // expansion depth. Only the construction guard uses a safety floor.
        expansion_depth: limits.expansion_depth,
        edges: usize::MAX,
        routes_per_node: u16::MAX as usize,
        node_prompt_bytes: usize::MAX,
        graph_prompt_bytes: usize::MAX,
        ..limits.clone()
    }
}

/// A host graph limit whose value can be safely reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphLimitKey {
    Nodes,
    Edges,
    RoutesPerNode,
    NodePromptBytes,
    GraphPromptBytes,
    FanOutWidth,
    ExpansionDepth,
}
impl GraphLimitKey {
    fn name(self) -> &'static str {
        match self {
            Self::Nodes => "nodes",
            Self::Edges => "edges",
            Self::RoutesPerNode => "routes_per_node",
            Self::NodePromptBytes => "node_prompt_bytes",
            Self::GraphPromptBytes => "graph_prompt_bytes",
            Self::FanOutWidth => "fan_out_width",
            Self::ExpansionDepth => "expansion_depth",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphStructuralLimit {
    pub key: GraphLimitKey,
    pub actual: usize,
    pub max: usize,
}
impl std::fmt::Display for GraphStructuralLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "limits.{}={} reached ({}); set by domains.fabric.graph.limits",
            self.key.name(),
            self.max,
            self.actual
        )
    }
}

/// Worst-case checkpoint size that exceeded the fixed graph envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEnvelopeLimit {
    pub bound: usize,
}

impl std::fmt::Display for GraphEnvelopeLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "graph checkpoint could reach {} bytes, over its {}-byte limit; split the graph into smaller batches, or use fewer nodes, routes, loop rounds or extensions, or smaller prompts",
            self.bound, MAX_CHECKPOINT_BYTES
        )
    }
}

/// The static admission check that refused a graph. The contained text is the
/// existing operator-facing reason; callers branch on the variant, not text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphOutputField {
    Input,
    CompositeExport,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("{0}")]
    Schema(String),
    #[error("{0}")]
    Composite(String),
    #[error("{0}")]
    Plan(String),
    #[error("{0}")]
    StructuralLimit(GraphStructuralLimit),
    #[error("{0}")]
    Envelope(GraphEnvelopeLimit),
    #[error("{reason}")]
    OutputKind {
        field: GraphOutputField,
        reason: String,
    },
}

/// The `GraphDocument::extra` key holding a submission's replay digest.
pub const REPLAY_DIGEST_FIELD: &str = "replay_digest";

impl GraphDocument {
    /// The host digest of the input that created this graph. A named `run` whose
    /// input has the same digest finds this graph instead of starting another.
    pub fn replay_digest(&self) -> Option<&str> {
        self.extra.get(REPLAY_DIGEST_FIELD)?.as_str()
    }

    pub fn set_replay_digest(&mut self, digest: &str) {
        self.extra
            .insert(REPLAY_DIGEST_FIELD.into(), serde_json::json!(digest));
    }

    /// Check nested composite structure with the same expansion safety bounds
    /// used by graph admission, without starting a graph.
    pub fn check_composites(&self, limits: &crate::config::GraphLimits) -> Result<(), String> {
        let expansion_limits = expansion_safety_limits(limits);
        let expanded =
            composites::expand(self, &expansion_limits).map_err(|error| error.to_string())?;
        loops::check_expanded_ids(&expanded, &[])?;
        let count = loops::expanded_node_count(&expanded, &[])?;
        if let Some(reason) = limits.nodes_bound().refusal(count) {
            return Err(reason);
        }
        Ok(())
    }

    /// Host preflight sees the same bounded static composite/loop expansion as
    /// the scheduler. Planner additions are checked separately before admission.
    /// A plan whose worst-case checkpoint would not fit is refused here, naming
    /// the size, rather than stopping mid-run on an oversized checkpoint.
    pub fn expanded(&self, limits: &crate::config::GraphLimits) -> Result<Self, String> {
        self.admit(limits).map_err(|error| error.to_string())?;
        loops::expand(self, &[], limits).map_err(|error| error.to_string())
    }

    /// The checks admission applies to a submitted document, without running
    /// it: the schema version against the fields it carries, then the
    /// expanded plan against `limits`. A document that passes here is not
    /// refused as invalid when it is submitted.
    pub fn validate(&self, limits: &crate::config::GraphLimits) -> Result<(), String> {
        self.admit(limits).map_err(|error| error.to_string())
    }

    /// The single static admission decision for a submitted graph. The reason
    /// is suitable for bounded display and typed projection by the front door.
    pub fn admit(&self, limits: &crate::config::GraphLimits) -> Result<(), AdmissionError> {
        validate_schema(self)?;
        let expansion_limits = expansion_safety_limits(limits);
        let expanded = loops::expand(self, &[], &expansion_limits)?;
        // Refused at admission only. A graph persisted before the removal
        // still replays, and its node launch blocks with the same refusal.
        if expanded.nodes.iter().any(|node| {
            node.attempt
                .as_ref()
                .is_some_and(AttemptSpec::names_cloud_placement)
        }) {
            return Err(AdmissionError::Plan(CLOUD_PLACEMENT_REMOVED.into()));
        }
        let (plan, _) = validate_flat_plan(&expanded, &expansion_limits)?;
        if let Some(reason) = structural_limit(&expanded, &plan, limits) {
            return Err(AdmissionError::StructuralLimit(reason));
        }
        envelope::admit(self, &[], &[], &plan)?;
        validate_output_kinds(&expanded)
    }

    pub fn inspect(&self, limits: &crate::config::GraphLimits) -> Result<GraphInspection, String> {
        validate_schema(self).map_err(|error| error.to_string())?;
        let inspect_limits = expansion_safety_limits(limits);
        let expanded =
            loops::expand(self, &[], &inspect_limits).map_err(|error| error.to_string())?;
        let generous = crate::config::GraphLimits {
            nodes: usize::MAX,
            edges: usize::MAX,
            routes_per_node: u16::MAX as usize,
            node_prompt_bytes: usize::MAX,
            graph_prompt_bytes: usize::MAX,
            ..limits.clone()
        };
        let (plan, _) =
            validate_flat_plan(&expanded, &generous).map_err(|error| error.to_string())?;
        let mut depths = std::collections::BTreeMap::<&str, usize>::new();
        let mut dependency_depth = 0;
        for node in &plan.nodes {
            let depth = node
                .dependencies()
                .map(|parent| depths[parent.as_str()] + 1)
                .max()
                .unwrap_or(0);
            dependency_depth = dependency_depth.max(depth);
            depths.insert(&node.id, depth);
        }
        Ok(GraphInspection {
            nodes: plan.nodes.len(),
            edges: plan
                .nodes
                .iter()
                .map(|node| node.dependencies().count())
                .sum(),
            dependency_depth,
            max_attempts: plan.nodes.iter().map(|node| node.routes.len()).sum(),
            prompt_bytes: plan.nodes.iter().map(|node| node.prompt.len()).sum(),
            limit_reason: structural_limit_reason(&expanded, &plan, limits),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphInspection {
    pub nodes: usize,
    pub edges: usize,
    pub dependency_depth: usize,
    /// Static route attempts, including all declared loop rounds. Planner
    /// fragments and honest-stop continuations consume separate host budgets.
    pub max_attempts: usize,
    pub prompt_bytes: usize,
    pub limit_reason: Option<String>,
}

/// Node purpose. Omitted kinds retain the original task wire format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Composite,
    Task,
    Synthesise,
    Gate,
    Loop,
    Decide,
    Land,
}

/// Immutable review subject or the commit a later writer must start from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceBinding {
    Commit {
        commit: String,
    },
    Node {
        node: String,
    },
    /// The newest head delivered by an upstream writer and the writers that
    /// continue it, among this node's own upstream nodes.
    Head {
        node: String,
    },
    Integration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GateCondition {
    Verdict {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        expected: String,
        threshold: VerdictThreshold,
    },
    /// Check the effective host graph budget, including saved extensions.
    Budget {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
    Ci {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        target: CiTarget,
        #[serde(default)]
        checks: Vec<String>,
        #[serde(default = "default_ci_timeout")]
        timeout_seconds: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failed_checks_max_bytes: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_failed_checks: Option<usize>,
    },
}

pub(super) fn default_ci_timeout() -> u64 {
    7200
}

pub(super) fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

pub(super) fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

pub(super) fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiState {
    Passed,
    Failed,
    Pending,
    Unavailable,
    /// The pull request conflicts with its base, so CI never starts.
    Conflict,
    /// No check has registered for the head, so CI was never scheduled.
    NotScheduled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiObservation {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub state: CiState,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_checks: Vec<CiFailedCheck>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_names: Vec<String>,
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiHostStamp {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub boot_identity: String,
    pub host_monotonic_micros: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiEvidence {
    /// Next host poll, persisted independently for every running CI gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<provenant_core::WallTime>,
    /// Pending observations already made, which selects the next backoff.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub pending_observations: u64,
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_observed_stamp: Option<CiHostStamp>,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub clock_reanchors: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_reanchor_reason: Option<String>,
    pub state: CiState,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_checks: Vec<CiFailedCheck>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_names: Vec<String>,
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub elapsed_seconds: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictThreshold {
    All,
    Any,
    Majority,
    Count(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateResult {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub passed: bool,
    pub reason: Option<String>,
    pub on_fail: Option<String>,
}

/// Select outputs from a direct dependency for inclusion in this node's brief.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphInput {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub node: String,
    pub outputs: Vec<OutputKind>,
    /// Selected machine-readable outputs that must be available before launch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_outputs: Vec<OutputKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    Result,
    Verdict,
    Findings,
    Branch,
    Commit,
    /// The `remaining[]` items of the node's handoff block.
    Remaining,
}

/// Compact, versioned settlement record. No provider transcript is retained.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeOutputs {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_error: Option<String>,
    pub schema_version: u64,
    pub result: Option<ResultOutput>,
    pub verdict: Option<String>,
    pub findings: Vec<Finding>,
    /// Whether the full findings selection was observed and parsed. Missing
    /// evidence in older checkpoints remains unknown on replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings_available: Option<bool>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub warnings: Vec<String>,
    /// Why a planner used its static fallback stage, when one was configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanOutput>,
    /// The `remaining[]` items of the handoff block the report declared, in
    /// any status, so unfinished work survives a clipped result excerpt; none
    /// when no handoff was declared or it lists no items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanOutput {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub fallback_reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultOutput {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub digest: String,
    pub excerpt: String,
    pub truncated: bool,
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub id: String,
    pub severity: String,
    pub file: String,
    pub line: Option<u64>,
    pub summary: String,
    pub status: String,
    /// The settlement decision on this finding, from the node's declared list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    /// One line of why that disposition was chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Evidence checked by verification or synthesis before a repair writer starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingTrigger {
    pub sequence: String,
    pub reach: FindingReach,
    pub observed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingReach {
    DefaultPath,
    OperatorMistake,
    AttackerOrUnreachable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingTriage {
    pub decision: String,
    pub reason: String,
    pub worth_fixing: bool,
    pub root_cause: String,
    pub blocking_class: String,
    #[serde(default)]
    pub shared_flaw: bool,
}

impl Finding {
    pub fn trigger(&self) -> Option<FindingTrigger> {
        serde_json::from_value(self.extra.get("trigger")?.clone()).ok()
    }

    pub fn triage(&self) -> Option<FindingTriage> {
        serde_json::from_value(self.extra.get("triage")?.clone()).ok()
    }

    pub fn boundary_or_data_loss(&self) -> bool {
        self.triage().is_some_and(|triage| {
            matches!(triage.blocking_class.as_str(), "boundary" | "data_loss")
        }) || self
            .extra
            .get("class")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|class| matches!(class, "boundary" | "data_loss"))
    }

    pub fn default_path(&self) -> bool {
        self.trigger().is_some_and(|trigger| {
            trigger.reach == FindingReach::DefaultPath && !trigger.sequence.trim().is_empty()
        })
    }

    pub fn effective_severity(&self) -> &str {
        if self.boundary_or_data_loss() || self.default_path() {
            &self.severity
        } else {
            "note"
        }
    }

    pub fn falsified_with_evidence(&self) -> bool {
        self.extra
            .get("verification")
            .and_then(serde_json::Value::as_str)
            == Some("falsified")
            && self
                .extra
                .get("evidence")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|evidence| !evidence.trim().is_empty())
    }

    pub fn rejected_boundary(&self) -> bool {
        self.boundary_or_data_loss()
            && (self.disposition.as_deref() == Some("rejected")
                || self
                    .triage()
                    .is_some_and(|triage| triage.decision == "rejected"))
    }

    pub fn spec_defect(&self) -> bool {
        self.extra.get("kind").and_then(serde_json::Value::as_str) == Some("spec_defect")
    }
}

/// Fallback after provider launch is safe only for a confined read-only node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    #[default]
    ReadOnly,
    WorktreeWrite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// Bump STATE_FORMAT_VERSION for changes an older or newer binary cannot read.
pub struct GraphCheckpoint {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expansions: Vec<Expansion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<RemainingBudgets>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub delivery_required: bool,
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub delivered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<String>,
    /// Graph-wide review and integration bases fixed at admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_review_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_integration_base: Option<String>,
    /// Integration head pinned when the first integration source was resolved.
    /// Synthesis and writers that start from that commit stop if it moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_pin: Option<String>,
    /// Durable admission fence. Resume observes the same attempt keys.
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
    /// Why the graph waits for the chair, with the cause typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<provenant_records::pause::Pause>,
    /// Usage fields unavailable for settled attempts, including partial
    /// telemetry. Only the newest lines are kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_usage: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub unknown_usage_omitted: usize,
    /// Durable, deduplicated observations of unavailable attempt telemetry,
    /// newest last. Older events beyond the cap are counted, not kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_events: Vec<UnknownUsageEvent>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub usage_events_omitted: u64,
    /// Attempts settled so far, which orders each attempt's `settled`.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub settlements: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// Links to the inbox requests raised by loops, decide nodes, lands and
    /// pauses, by ask ID. The inbox holds the question and the answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asks: Vec<AskLink>,
    /// Actor every ask is raised under, recorded before the first ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_actor: Option<String>,
    /// Earliest deadline of an open ask or running CI gate, for the host tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<provenant_core::WallTime>,
    /// Pause asks raised so far, which numbers the next one.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub pause_asks: u64,
    /// Warnings about ignored document limit overrides.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limit_notes: Vec<String>,
    pub schema_version: u64,
    pub graph_id: String,
    /// Digest of the validated, normalised plan, including prompt bytes.
    pub plan_digest: String,
    /// Nodes are stored in topological order.
    pub nodes: Vec<NodeProgress>,
}

pub(super) fn not_delivered(delivered: &bool) -> bool {
    !delivered
}

/// Approved planner failure facts, without captured fragment text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "limit", rename_all = "snake_case")]
pub enum PlanFailureFact {
    MissingBlock,
    MissingResult,
    InvalidBlock,
    InvalidFragment,
    InvalidContinuation,
    Schema,
    InvalidGraph,
    OutputKind,
    Structural(GraphStructuralLimit),
    Envelope(GraphEnvelopeLimit),
}

impl std::fmt::Display for PlanFailureFact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBlock => f.write_str("fragment block missing"),
            Self::MissingResult => f.write_str("planner result missing"),
            Self::InvalidBlock => f.write_str("invalid fragment block; check its format and size"),
            Self::InvalidFragment => {
                f.write_str("invalid fragment; check the declared graph constraints")
            }
            Self::InvalidContinuation => {
                f.write_str("invalid writer continuation; inspect the handoff")
            }
            Self::Schema => f.write_str("schema version"),
            Self::InvalidGraph => {
                f.write_str("expanded graph is invalid; check the declared graph constraints")
            }
            Self::OutputKind => f.write_str("expanded graph selects an unavailable output kind"),
            Self::Structural(limit) => write!(f, "{limit}"),
            Self::Envelope(limit) => write!(f, "{limit}"),
        }
    }
}

/// Scheduler-owned causes whose decisions must not depend on rendered prose.
/// Paused or truncated checkpoints require a finish-first drain before this
/// checkpoint meaning is installed; no legacy cause reader is provided.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeDecisionCause {
    LoopPaused,
    LoopAskClosed,
    InvalidLoopDecision,
    PlanExpansionPaused {
        active: bool,
        fallback: bool,
        fragment_error: Option<PlanFailureFact>,
    },
    CompositeFindingsTruncated {
        warning_indices: Vec<usize>,
        notice_lines: usize,
    },
}

impl NodeDecisionCause {
    pub(super) fn from_pause(cause: PauseCause) -> Option<Self> {
        match cause {
            PauseCause::LoopPaused => Some(Self::LoopPaused),
            PauseCause::LoopAskClosed => Some(Self::LoopAskClosed),
            PauseCause::InvalidLoopDecision => Some(Self::InvalidLoopDecision),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeProgress {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    /// Direct children of a composite, whose individual evidence stays available.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<String>,
    /// Declared source selection, including upstream sources still pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_binding: Option<SourceBinding>,
    /// Minimum declared by this composite, retained for outcome reporting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_successful: Option<usize>,
    /// Resolved once before launch and retained across retries and replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    /// Canonical verified writer worktree and its head at binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer_binding: Option<WriterWorktreeBinding>,
    /// An expanded writer whose worktree has not yet become available to bind.
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub writer_binding_pending: bool,
    /// Node-specific review base resolved before the first attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_base: Option<String>,
    /// Review base awaiting a source location that has not yet been bound.
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub review_base_pending: bool,
    /// Bounded planner JSON retained until admission succeeds, including across resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment: Option<String>,
    /// The writer's unfinished handoff, kept for its continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<provenant_records::handoff::Handoff>,
    /// The stopped writer's handoff says `blocked`, read once at settlement.
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub blocked: bool,
    /// The decisions a review node made on its findings, each bound to the
    /// head and base it reviewed. A decision without a base carries no
    /// approval over, but is stored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<provenant_records::FindingDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion_pause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion_cause: Option<NodeDecisionCause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_pause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_pause_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_cause: Option<NodeDecisionCause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation_cause: Option<NodeDecisionCause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<LoopRound>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateResult>,
    /// Which family separation applied, when the node asked for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separation: Option<FamilySeparation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiEvidence>,
    /// A land node's pinned subject, tier policy and authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub land: Option<LandRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Retain the writer checkout for chair inspection after a failed launch.
    pub worktree: Option<String>,
    pub id: String,
    pub state: NodeState,
    /// State to restore when a cancelled graph is resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_from: Option<NodeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    /// An actionable admission or settlement failure, retained for status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advance_error: Option<String>,
    /// Stable cause of an admission or settlement failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advance_error_code: Option<String>,
    /// Code from the owner whose launch refusal produced the disposition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advance_owner_code: Option<String>,
    /// Launch retry wait for the current attempt after a transient launch
    /// error. Cleared when the attempt is observed again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_retry: Option<LaunchRetry>,
    /// Final retry budget and cause retained when launch retries are exhausted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_launch_retry: Option<LaunchRetry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<NodeOutputs>,
    /// Immutable evidence from earlier node redos, included in budget accounting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_attempts: Vec<GraphAttempt>,
    /// One durable row per route actually tried, in route order.
    pub attempts: Vec<GraphAttempt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterWorktreeBinding {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub worktree: String,
    pub head: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphAttempt {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub launched: bool,
    /// This attempt replaces a settled failed review member on its next route.
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub review_substitution: bool,
    pub key: AttemptKey,
    pub route: GraphRoute,
    pub state: AttemptState,
    pub attempt_ref: Option<String>,
    pub outcome: Option<AttemptOutcome>,
    /// Actor that first submitted this attempt. Run-store operation replay is
    /// actor-scoped, so a resume submits under it.
    pub launch_actor: String,
    /// Byte cap fixed at admission so operation replay renders the same input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cap: Option<usize>,
    /// The effective graph limits supplied to admission own this cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cap_source: Option<InputCapSource>,
    /// Bounded identifiers describing what was selected and delivered before launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_manifest: Option<AttemptInputManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AttemptUsage>,
    /// When the attempt settled, as a graph-wide order from 1; zero while it runs.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub settled: u64,
    /// The stop cause the run sealed for this attempt, read at settlement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<provenant_records::StopCause>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputCapSource {
    EffectiveGraphLimits,
    #[serde(rename = "default")]
    Default,
    Configured,
}

/// Evidence contains identifiers and digests only, never input text or host paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptInputManifest {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub schema_version: u64,
    pub source_commit: Option<String>,
    /// Producer of an upstream source, absent when it cannot be identified.
    #[serde(default)]
    pub source_producer: Option<AttemptKey>,
    /// Writer worktree head observed when its location was bound.
    #[serde(default)]
    pub writer_head: Option<String>,
    pub review_base: Option<String>,
    /// Graph-wide bases are distinct from a node's review base and source.
    #[serde(default)]
    pub graph_review_base: Option<String>,
    pub integration_base: Option<String>,
    /// The integration head selected as an upstream source, if any.
    #[serde(default)]
    pub integration_source: Option<String>,
    pub applied_route: Option<GraphRoute>,
    /// Ordering edges, separate from the outputs selected as context.
    #[serde(default)]
    pub dependencies: Option<Vec<String>>,
    #[serde(default)]
    pub dependencies_omitted: usize,
    pub inputs: Vec<AttemptInputEvidence>,
    pub omitted: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptInputEvidence {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub producer_node: String,
    pub producer_attempt: Option<AttemptKey>,
    pub kind: OutputKind,
    pub required: bool,
    pub producer_result_digest: Option<String>,
    pub commit: Option<String>,
    pub delivery: InputDelivery,
    pub file_delivery: InputDelivery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputDelivery {
    Delivered,
    Capped,
    Omitted,
    Unavailable,
    Pending,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptUsage {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub tokens: Option<u64>,
    pub cost_micro_usd: Option<u64>,
    pub wall_time_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub tokens_partial: bool,
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub cost_partial: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_unknown_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownUsageEvent {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub node_id: String,
    pub route_index: u16,
    pub metric: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptKey {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub graph_id: String,
    pub node_id: String,
    /// Zero-based position in that node's ordered route list.
    pub route_index: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Blocked,
    Skipped,
}

impl NodeState {
    pub(super) fn settled(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }
}

impl GraphCheckpoint {
    /// Pause the graph for the chair. The reason is cut to its bound.
    pub fn set_pause(&mut self, cause: provenant_records::pause::PauseCause, reason: &str) {
        let reason = bounded(reason, provenant_records::pause::PAUSE_REASON_BYTES);
        let reason = if reason.trim().is_empty() {
            "paused".into()
        } else {
            reason
        };
        self.pause = provenant_records::pause::Pause::new(cause, None, reason).ok();
    }

    pub fn pause_cause(&self) -> Option<provenant_records::pause::PauseCause> {
        self.pause
            .as_ref()
            .map(provenant_records::pause::Pause::cause)
    }

    pub fn pause_reason(&self) -> Option<&str> {
        self.pause
            .as_ref()
            .map(provenant_records::pause::Pause::reason)
    }

    pub(super) fn deactivate_expansion_pauses(&mut self) {
        for node in &mut self.nodes {
            if let Some(NodeDecisionCause::PlanExpansionPaused { active, .. }) =
                &mut node.expansion_cause
            {
                *active = false;
                if node.expansion_pause.is_none() {
                    node.expansion_cause = None;
                }
            }
        }
    }

    pub(super) fn activate_expansion_pause(&mut self, index: usize) {
        self.deactivate_expansion_pauses();
        if let Some(NodeDecisionCause::PlanExpansionPaused { active, .. }) =
            &mut self.nodes[index].expansion_cause
        {
            *active = true;
        }
    }

    /// Classify settled work without relaxing the strict delivery gate.
    pub fn outcome(&self) -> GraphOutcome {
        if self.successful() {
            return GraphOutcome::Succeeded;
        }
        if self
            .nodes
            .iter()
            .any(|node| node.min_successful.is_some() && !self.node_successful(node))
        {
            return GraphOutcome::Failed;
        }
        let mut covered = std::collections::BTreeSet::new();
        let mut pending: Vec<_> = self
            .nodes
            .iter()
            .filter(|node| node.min_successful.is_some() && node.state == NodeState::Succeeded)
            .flat_map(|node| node.children.iter())
            .collect();
        while let Some(id) = pending.pop() {
            if covered.insert(id)
                && let Some(node) = self.nodes.iter().find(|node| &node.id == id)
            {
                pending.extend(node.children.iter());
            }
        }
        if self
            .nodes
            .iter()
            .any(|node| !self.node_successful(node) && !covered.contains(&node.id))
        {
            GraphOutcome::Failed
        } else {
            GraphOutcome::Partial
        }
    }

    /// Failed gates are handled only when their recovery path succeeds.
    pub fn successful(&self) -> bool {
        if self.nodes.iter().any(|n| n.expansion_pause.is_some()) {
            return false;
        }
        self.nodes.iter().all(|node| self.node_successful(node))
    }

    pub(super) fn node_successful(&self, node: &NodeProgress) -> bool {
        if loops::handled_gate(self, node) || repair::handled_panel(self, node) {
            return true;
        }
        let mut current = node;
        for depth in 0..self.nodes.len() {
            match current.state {
                NodeState::Succeeded => return true,
                NodeState::Skipped => return depth == 0,
                NodeState::Failed => {
                    let Some(target) = current
                        .gate
                        .as_ref()
                        .filter(|gate| !gate.passed)
                        .and_then(|gate| gate.on_fail.as_ref())
                        .and_then(|target| self.nodes.iter().find(|node| &node.id == target))
                    else {
                        return false;
                    };
                    current = target;
                }
                _ => return false,
            }
        }
        false
    }
}

impl NodeProgress {
    /// All attempt evidence in launch order, including previous redos.
    pub fn recorded_attempts(&self) -> impl DoubleEndedIterator<Item = &GraphAttempt> {
        self.previous_attempts.iter().chain(&self.attempts)
    }

    pub(super) fn truncation(&self) -> Option<(&[usize], usize)> {
        match self.truncation_cause.as_ref()? {
            NodeDecisionCause::CompositeFindingsTruncated {
                warning_indices,
                notice_lines,
            } => Some((warning_indices, *notice_lines)),
            _ => None,
        }
    }

    pub(super) fn truncation_warnings(&self) -> Vec<&String> {
        let Some((indices, _)) = self.truncation() else {
            return Vec::new();
        };
        let Some(outputs) = &self.outputs else {
            return Vec::new();
        };
        indices
            .iter()
            .filter_map(|index| outputs.warnings.get(*index))
            .collect()
    }

    pub(super) fn truncation_notices(&self) -> Vec<&str> {
        let Some((_, lines)) = self.truncation() else {
            return Vec::new();
        };
        self.outputs
            .as_ref()
            .and_then(|outputs| outputs.result.as_ref())
            .map_or_else(Vec::new, |result| {
                result.excerpt.lines().take(lines).collect()
            })
    }

    /// Attempt-less evidence for final graph delivery.
    pub fn deterministic_summary(&self) -> Option<String> {
        if let Some(reason) = self
            .outputs
            .as_ref()
            .and_then(|outputs| outputs.plan.as_ref())
            .map(|plan| plan.fallback_reason.as_str())
        {
            return Some(format!(
                "Node {}: planner fallback\nReason: {reason}\n\n",
                self.id
            ));
        }
        let decision = self
            .outputs
            .as_ref()
            .and_then(|outputs| outputs.result.as_ref())
            .filter(|_| self.attempts.is_empty() && self.children.is_empty());
        if self.state == NodeState::Skipped {
            Some(format!("Node {}: skipped\n\n", self.id))
        } else if let Some(decision) = decision {
            Some(format!("Node {}: {}\n\n", self.id, decision.excerpt))
        } else if self.attempts.is_empty() && self.outputs.is_some() && self.gate.is_none() {
            Some(format!(
                "Node {}: {} {:?}\n\n",
                self.id,
                if self.children.is_empty() {
                    "loop"
                } else {
                    "composite"
                },
                self.state
            ))
        } else {
            self.gate.as_ref().map(|gate| {
                format!(
                    "Node {}: gate {}, reason: {}, on_fail: {}\n\n",
                    self.id,
                    if gate.passed { "PASS" } else { "FAIL" },
                    gate.reason.as_deref().unwrap_or("condition satisfied"),
                    gate.on_fail.as_deref().unwrap_or("none")
                )
            })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphOutcome {
    Succeeded,
    Partial,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphStatus {
    Pending,
    Complete,
    Cancelled,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRun {
    pub status: GraphStatus,
    pub outcome: Option<GraphOutcome>,
    pub checkpoint: GraphCheckpoint,
    /// Why the last automatic advance failed, as recorded for `status`.
    pub advance_error: Option<String>,
}

impl Serialize for GraphRun {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let fields =
            3 + usize::from(self.outcome.is_some()) + usize::from(self.advance_error.is_some());
        let mut state = serializer.serialize_struct("GraphRun", fields)?;
        state.serialize_field("status", &self.status)?;
        if let Some(outcome) = &self.outcome {
            state.serialize_field("outcome", outcome)?;
        }
        state.serialize_field("checkpoint", &self.checkpoint)?;
        if let Some(error) = &self.advance_error {
            state.serialize_field("advance_error", error)?;
        }
        state.serialize_field("waits", &super::graph_waits(&self.checkpoint))?;
        state.end()
    }
}

/// What the checkpoint already knows about an attempt.
#[derive(Clone, Copy, Debug)]
pub struct Replay<'a> {
    pub delivery: Option<&'a provenant_records::run::DeliveryMode>,
    /// The delivery branch a continuing writer must push to. The host refuses
    /// a worktree on any other branch.
    pub branch: Option<&'a str>,
    /// An integration may only fast-forward its target onto the round's head.
    /// It never merges, so a target that moved refuses the delivery.
    pub fast_forward_only: bool,
    /// Reference to the accepted attempt, once saved.
    pub stored_ref: Option<&'a str>,
    /// Actor to submit under when no reference was saved.
    pub launch_actor: &'a str,
}

/// User-facing content for a graph node's submitted task.
pub struct NodeInput<'a> {
    /// Review identity of the expanded node being launched.
    pub review_launch: bool,
    pub require_skill_location: bool,
    /// Remaining portion of the graph's single infrastructure allowance.
    pub settlement_retries: Option<u8>,
    /// Models selected for earlier attempts of this node.
    pub prior_attempts: &'a [String],
    /// Sibling families to avoid where possible, never a routing requirement.
    pub family_peers: &'a [String],
    pub context: provenant_records::run::WorkContext,
    pub read_roots: &'a [String],
    pub attachments: &'a [String],
    pub capabilities: Option<&'a [String]>,
    pub capability_sources: Option<&'a serde_json::Value>,
    /// The node's attempt spec: an account for routes that name none, and
    /// build weight. Capabilities are already folded into `capabilities`.
    pub attempt: Option<&'a AttemptSpec>,
    /// Attempts of the source writer whose model families must be avoided.
    pub distinct_from_attempts: &'a [String],
    /// Refuse a route inside distinct_from_attempts' families. family_peers
    /// remain a preference even when writer separation is enforced.
    pub require_distinct: bool,
    pub source_commit: Option<&'a str>,
    pub review_base: Option<&'a str>,
    pub title: &'a str,
    pub prompt: &'a str,
    pub required_verdict: Option<&'a str>,
    pub deadline_seconds: Option<u64>,
    /// Host directory holding this attempt's materialised upstream results.
    /// The lane receives read access to it and nothing else of the host's
    /// private state.
    pub inputs_dir: Option<&'a std::path::Path>,
}

/// One upstream result a node's prompt points at as a file. `attempt_ref` is
/// the host-qualified attempt that produced it, absent when none was recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputRequest {
    pub label: String,
    pub attempt_ref: Option<String>,
    /// Findings from a non-attempt member, such as a CI gate with no result file.
    pub inline_findings: Option<Vec<Finding>>,
    /// Composite qualification to apply to IDs in the source result.
    pub finding_prefix: Option<String>,
}

/// What became of one requested upstream result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaterialisedInput {
    /// Written in full, or cut at the per-node cap when `omitted` is non-zero.
    File {
        label: String,
        path: std::path::PathBuf,
        bytes: u64,
        omitted: u64,
    },
    /// Not written because the per-node cap was already reached.
    Capped { label: String },
    /// Not available here; the reason is shown to the node.
    Unavailable { label: String, reason: String },
}

/// The host's answer: one entry per request, in request order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaterialisedInputs {
    pub directory: Option<std::path::PathBuf>,
    /// Optional index of all inputs when the prompt cannot list every source.
    pub index: Option<std::path::PathBuf>,
    pub entries: Vec<MaterialisedInput>,
}

/// Host polling policy. Defaults observe pending gates once per tick; hosts
/// can select a longer exponential backoff independently for each gate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CiPollBackoff {
    pub initial_seconds: u64,
    pub max_seconds: u64,
}

impl CiPollBackoff {
    pub(super) fn delay_seconds(self, pending_observations: u64) -> u64 {
        provenant_core::retry::Backoff::new(
            std::time::Duration::from_secs(self.initial_seconds),
            2,
            std::time::Duration::from_secs(self.max_seconds),
        )
        .delay(pending_observations)
        .as_secs()
    }
}
