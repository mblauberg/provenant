//! Shared, durable landing contract. Execution effects remain in the front-door owner.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewProof {
    #[default]
    Policy,
    Accepted {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandMember {
    pub pr: u64,
    pub head: Option<String>,
    pub base: Option<String>,
    #[serde(default)]
    pub review_proof: ReviewProof,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Observe,
    #[default]
    Policy,
    Chair,
}

/// Per-call patience and tier selection. Effective tier settings are in the plan
/// digest; changing a subscription's patience does not change its checked subject.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyChoice {
    pub tier: Option<String>,
    pub poll_seconds: Option<u64>,
    pub patience_seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DependencyEdge {
    /// The prerequisite PR must be incorporated before `dependent`.
    pub prerequisite: u64,
    pub dependent: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscriber {
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandRequest {
    pub members: Vec<LandMember>,
    pub authority: Authority,
    pub policy: PolicyChoice,
    /// Root branch/ref all members in this request land against.
    pub target: Option<String>,
    pub dependency_edges: Vec<DependencyEdge>,
    pub strategy: Option<Strategy>,
    pub compose: Compose,
    pub generation: u64,
    pub parent: Option<LandingId>,
    pub subscribers: Vec<Subscriber>,
}

/// A stable pinned-request key and a generation/plan-specific identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingId {
    pub request: provenant_core::Digest,
    pub digest: provenant_core::Digest,
}

impl LandingId {
    pub fn request_key(
        repository: &str,
        target: &str,
        request: &LandRequest,
    ) -> Result<provenant_core::Digest, serde_json::Error> {
        #[derive(Serialize)]
        struct MemberPin<'a> {
            pr: u64,
            head: &'a Option<String>,
            base: &'a Option<String>,
        }
        #[derive(Serialize)]
        struct Input<'a> {
            repository: &'a str,
            target: &'a str,
            members: Vec<MemberPin<'a>>,
            dependencies: &'a [DependencyEdge],
        }
        let input = Input {
            repository,
            target,
            members: request
                .members
                .iter()
                .map(|member| MemberPin {
                    pr: member.pr,
                    head: &member.head,
                    base: &member.base,
                })
                .collect(),
            dependencies: &request.dependency_edges,
        };
        crate::CanonicalV1::record_digest("landing_request", 2, &input)
            .map_err(<serde_json::Error as serde::ser::Error>::custom)
    }

    pub fn of(
        repository: &str,
        target: &str,
        input: &LandRequest,
        plan_digest: &str,
    ) -> Result<Self, serde_json::Error> {
        #[derive(Serialize)]
        struct Identity<'a> {
            request: provenant_core::Digest,
            generation: u64,
            parent: &'a Option<LandingId>,
            plan_digest: &'a str,
        }
        let request = Self::request_key(repository, target, input)?;
        let digest = crate::CanonicalV1::record_digest(
            "landing_identity",
            2,
            &Identity {
                request,
                generation: input.generation,
                parent: &input.parent,
                plan_digest,
            },
        )
        .map_err(<serde_json::Error as serde::ser::Error>::custom)?;
        Ok(Self { request, digest })
    }
}

impl std::fmt::Display for LandingId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.digest, formatter)
    }
}

/// Inputs that determine which independent approvals a landing requires.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewScope {
    pub project: Option<provenant_core::ProjectId>,
    pub exempt_patterns: Vec<String>,
}

/// Publication intent is stored before a forge submission can become ambiguous.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishIntent {
    pub context: LandingContext,
    pub repository: String,
    pub target: String,
    pub expected_base: String,
    pub candidate_commit: String,
    pub candidate_tree: String,
    pub members: Vec<MemberEvidence>,
}

/// Shared context copied into pending and settled receipts before dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingContext {
    pub id: LandingId,
    pub generation: u64,
    pub request: LandRequest,
    pub plan: ResolvedPlan,
    pub operation: Option<OperationRef>,
    pub expected_revision: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dependencies {
    #[default]
    Independent,
    Stack,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Single,
    #[default]
    Batch,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Schedule {
    #[default]
    Immediate,
    Ordered,
    ForgeQueue,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Publication {
    #[default]
    ExactCandidate,
    Forge,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completion {
    #[default]
    Event,
    BoundedSubscription,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Plan {
    pub dependencies: Dependencies,
    pub candidate: CandidateKind,
    pub schedule: Schedule,
    pub publication: Publication,
    pub completion: Completion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgeCapability {
    MergeQueue,
    AutoMerge,
    AsyncMerge,
    Restack,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Available,
    Unavailable,
    #[default]
    Unknown,
}

/// Capabilities are an optional, repository-scoped probe. Missing entries mean unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Capabilities {
    pub repository: Option<String>,
    pub values: BTreeMap<ForgeCapability, CapabilityState>,
}

impl Capabilities {
    pub fn state(&self, capability: ForgeCapability) -> CapabilityState {
        self.values
            .get(&capability)
            .copied()
            .unwrap_or(CapabilityState::Unknown)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityPath {
    Native,
    Observer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDecision {
    pub capability: ForgeCapability,
    pub state: CapabilityState,
    pub selected: CapabilityPath,
}

/// The execution port selected by the strategy declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Execution {
    Builtin,
    Command,
    Unavailable,
}

/// Repository publisher. The full pending intent is supplied as JSON on stdin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCommand {
    pub argv: Vec<String>,
    pub unknown_exit_code: i64,
    pub timeout_seconds: u64,
}
impl RepositoryCommand {
    pub fn valid(&self) -> bool {
        !self.argv.is_empty()
            && !self.argv[0].is_empty()
            && self.unknown_exit_code > 0
            && self.unknown_exit_code <= 255
            && self.timeout_seconds > 0
    }
}

#[derive(Clone, Debug)]
pub struct StrategyDeclaration {
    pub strategy: Strategy,
    pub name: &'static str,
    pub defaults: Plan,
    /// Optional native features; each has an observer implementation.
    pub requirements: &'static [ForgeCapability],
    pub resolver: fn(Plan, &Compose, &Capabilities) -> Plan,
    pub execution: Execution,
}

macro_rules! declare_strategies {
    ($( $(#[$attr:meta])* $variant:ident => ($name:literal, $plan:expr, $requirements:expr, $resolver:ident, $execution:ident); )+) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Strategy { $( $(#[$attr])* #[serde(rename = $name)] $variant, )+ }

        pub const STRATEGY_CHOICES: &[&str] = &[$($name),+];

        pub static STRATEGY_DECLARATIONS: &[StrategyDeclaration] = &[
            $(StrategyDeclaration {
                strategy: Strategy::$variant,
                name: $name,
                defaults: $plan,
                requirements: $requirements,
                resolver: $resolver,
                execution: Execution::$execution,
            }),+
        ];
    }
}

declare_strategies! {
    Direct => ("direct", Plan { dependencies: Dependencies::Independent, candidate: CandidateKind::Single, schedule: Schedule::Immediate, publication: Publication::ExactCandidate, completion: Completion::Event }, &[], resolve_preset, Builtin);
    #[default]
    Batch => ("batch", Plan { dependencies: Dependencies::Independent, candidate: CandidateKind::Batch, schedule: Schedule::Immediate, publication: Publication::ExactCandidate, completion: Completion::Event }, &[], resolve_preset, Builtin);
    Stack => ("stack", Plan { dependencies: Dependencies::Stack, candidate: CandidateKind::Batch, schedule: Schedule::Ordered, publication: Publication::ExactCandidate, completion: Completion::Event }, &[ForgeCapability::Restack], resolve_preset, Unavailable);
    Queue => ("queue", Plan { dependencies: Dependencies::Independent, candidate: CandidateKind::Single, schedule: Schedule::Ordered, publication: Publication::ExactCandidate, completion: Completion::Event }, &[ForgeCapability::MergeQueue], resolve_queue, Unavailable);
    Async => ("async", Plan { dependencies: Dependencies::Independent, candidate: CandidateKind::Single, schedule: Schedule::Immediate, publication: Publication::ExactCandidate, completion: Completion::Event }, &[ForgeCapability::AutoMerge, ForgeCapability::AsyncMerge], resolve_preset, Unavailable);
    Command => ("command", Plan { dependencies: Dependencies::Independent, candidate: CandidateKind::Batch, schedule: Schedule::Immediate, publication: Publication::ExactCandidate, completion: Completion::Event }, &[], resolve_preset, Command);
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Compose {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Dependencies>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<CandidateKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<Publication>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<Completion>,
}

impl Compose {
    pub fn apply_to(&self, plan: Plan) -> Plan {
        apply_compose(plan, self)
    }
}

impl Strategy {
    pub fn choices() -> &'static [&'static str] {
        STRATEGY_CHOICES
    }

    pub fn declaration(self) -> &'static StrategyDeclaration {
        // Enum discriminants and table positions are generated by the same declaration.
        &STRATEGY_DECLARATIONS[self as usize]
    }

    pub fn plan(self, compose: &Compose, capabilities: &Capabilities) -> Plan {
        (self.declaration().resolver)(self.declaration().defaults, compose, capabilities)
    }

    pub fn capability_decisions(
        self,
        plan: &Plan,
        capabilities: &Capabilities,
    ) -> Vec<CapabilityDecision> {
        self.declaration()
            .requirements
            .iter()
            .map(|capability| {
                let state = capabilities.state(*capability);
                let selected = if state == CapabilityState::Available
                    && (*capability != ForgeCapability::MergeQueue
                        || plan.schedule == Schedule::ForgeQueue)
                {
                    CapabilityPath::Native
                } else {
                    CapabilityPath::Observer
                };
                CapabilityDecision {
                    capability: *capability,
                    state,
                    selected,
                }
            })
            .collect()
    }

    pub fn ensure_executable(self, plan: &Plan) -> Result<(), Unavailable> {
        let supported = (self.declaration().execution != Execution::Unavailable)
            && plan.dependencies == Dependencies::Independent
            && plan.schedule == Schedule::Immediate
            && plan.publication == Publication::ExactCandidate;
        if supported {
            Ok(())
        } else {
            Err(Unavailable {
                requested: self,
                alternatives: STRATEGY_DECLARATIONS
                    .iter()
                    .filter(|entry| entry.execution != Execution::Unavailable)
                    .map(|entry| entry.strategy)
                    .collect(),
                note: if self.declaration().execution != Execution::Unavailable {
                    "This composition is not implemented yet; execution follows in 2.1.".into()
                } else {
                    "Execution is planned for 2.1; this strategy has been resolved without switching it.".into()
                },
            })
        }
    }
}

fn apply_compose(mut plan: Plan, compose: &Compose) -> Plan {
    plan.dependencies = compose.dependencies.unwrap_or(plan.dependencies);
    plan.candidate = compose.candidate.unwrap_or(plan.candidate);
    plan.schedule = compose.schedule.unwrap_or(plan.schedule);
    plan.publication = compose.publication.unwrap_or(plan.publication);
    plan.completion = compose.completion.unwrap_or(plan.completion);
    plan
}

fn resolve_preset(defaults: Plan, compose: &Compose, _capabilities: &Capabilities) -> Plan {
    apply_compose(defaults, compose)
}
fn resolve_queue(defaults: Plan, compose: &Compose, capabilities: &Capabilities) -> Plan {
    let mut plan = apply_compose(defaults, compose);
    let queue_units_fit =
        plan.dependencies == Dependencies::Independent && plan.candidate == CandidateKind::Single;
    if plan.schedule == Schedule::ForgeQueue || compose.schedule.is_none() {
        plan.schedule = if queue_units_fit
            && capabilities.state(ForgeCapability::MergeQueue) == CapabilityState::Available
        {
            Schedule::ForgeQueue
        } else {
            Schedule::Ordered
        };
    }
    plan
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unavailable {
    pub requested: Strategy,
    pub alternatives: Vec<Strategy>,
    pub note: String,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.note)
    }
}
impl std::error::Error for Unavailable {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPlan {
    pub strategy: Strategy,
    pub plan: Plan,
    /// Per-field layer, key, matching rule and other resolver provenance.
    pub provenance: BTreeMap<String, serde_json::Value>,
    pub capabilities: Capabilities,
    pub capability_decisions: Vec<CapabilityDecision>,
    pub digest: String,
}

impl ResolvedPlan {
    pub fn new(
        strategy: Strategy,
        compose: &Compose,
        capabilities: Capabilities,
        mut provenance: BTreeMap<String, serde_json::Value>,
    ) -> Result<Self, serde_json::Error> {
        let plan = strategy.plan(compose, &capabilities);
        let capability_decisions = strategy.capability_decisions(&plan, &capabilities);
        let declaration = strategy.declaration();
        for (field, overridden) in [
            ("dependencies", compose.dependencies.is_some()),
            ("candidate", compose.candidate.is_some()),
            ("schedule", compose.schedule.is_some()),
            ("publication", compose.publication.is_some()),
            ("completion", compose.completion.is_some()),
        ] {
            let source = if overridden {
                "compose"
            } else if field == "schedule"
                && strategy == Strategy::Queue
                && plan.schedule == Schedule::ForgeQueue
            {
                "capability"
            } else {
                "strategy_preset"
            };
            provenance.entry(field.into()).or_insert_with(|| {
                serde_json::json!({
                    "source": source,
                    "strategy": declaration.name,
                })
            });
        }
        let mut resolved = Self {
            strategy,
            plan,
            provenance,
            capabilities,
            capability_decisions,
            digest: String::new(),
        };
        resolved.digest = resolved.computed_digest()?;
        Ok(resolved)
    }

    /// Recompute the seal from saved values, without rerunning strategy resolution.
    pub fn computed_digest(&self) -> Result<String, serde_json::Error> {
        let mut value = serde_json::to_value(self)?;
        if let serde_json::Value::Object(fields) = &mut value {
            fields.remove("digest");
        }
        crate::CanonicalV1::record_digest("landing_plan", 2, &value)
            .map(|digest| digest.to_string())
            .map_err(<serde_json::Error as serde::ser::Error>::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MemberOutcome {
    Pending { reason: String },
    Landed { commit: Option<String> },
    Unknown { reason: String },
    Refused { reason: String },
}

impl MemberOutcome {
    pub fn landed(&self) -> bool {
        matches!(self, Self::Landed { .. })
    }
}

/// Transport progress is distinct from the final business outcome above.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MemberProgress {
    Pending,
    Submitted { operation: OperationRef },
    Queued { operation: OperationRef },
    Deferred { reason: String },
    Unknown { reason: String },
    Failed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationRef {
    ForgeAsync {
        provider: String,
        id: String,
        pr: u64,
        head: String,
    },
    MergeQueue {
        provider: String,
        id: String,
        pr: u64,
        head: String,
    },
    AutoMerge {
        provider: String,
        pr: u64,
        head: String,
    },
    Observer {
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DispatchKey(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberEvidence {
    pub pr: u64,
    pub head: String,
    pub base: String,
    pub base_commit: String,
    pub gate_definition_digest: String,
    pub candidate_commit: String,
    pub candidate_tree: String,
    pub checks: Vec<CheckEvidence>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    #[default]
    Checks,
    Commands,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateDefinition {
    pub kind: GateKind,
    pub digest: String,
    pub checks: Vec<String>,
    pub commands: Vec<String>,
    pub issuer: Option<String>,
    pub executor: Option<String>,
    pub coverage: Vec<String>,
    pub timeout_seconds: u64,
    pub pool: Option<String>,
    pub env_passthrough: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateResult {
    Passed,
    Failed,
    Pending,
    Missing,
    Skipped,
    Neutral,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckEvidence {
    pub name: String,
    pub issuer: Option<String>,
    pub result: GateResult,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeEvidence {
    pub candidate_commit: Option<String>,
    pub candidate_tree: Option<String>,
    pub target_base: Option<String>,
    pub member_commits: BTreeMap<u64, String>,
    pub members: Vec<MemberEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Submission {
    Merged { evidence: MergeEvidence },
    Accepted { operation: OperationRef },
    Unknown { dispatch_key: DispatchKey },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingState {
    Submitted,
    AwaitingChecks,
    Queued,
    Processing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ForgeObservation {
    Pending { pending: PendingState },
    Merged { evidence: MergeEvidence },
    Rejected { reason: String },
    Unknown { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObservationKey(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub id: LandingId,
    pub generation: u64,
    pub operation: OperationRef,
    pub expected_revision: u64,
    pub key: ObservationKey,
    pub result: ForgeObservation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationGuarantee {
    None,
    ExactCandidate,
    OrderedMembers,
    ForgeManaged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupDebtItem {
    pub step: String,
    pub reason: String,
    pub attempts: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupDebt {
    pub items: Vec<CleanupDebtItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestackMapping {
    pub pr: u64,
    pub previous_head: String,
    pub new_head: String,
    pub previous_base: String,
    pub new_base: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingChanged {
    pub id: LandingId,
    pub generation: u64,
    pub revision: u64,
    pub operation: Option<OperationRef>,
    pub outcomes: BTreeMap<u64, MemberOutcome>,
    pub evidence: Option<MergeEvidence>,
    pub deduplication_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub event: LandingChanged,
    pub delivered_to: BTreeSet<Subscriber>,
    pub attempts: u32,
}

/// Run/graph snapshots retain this reference and cursor without owning a second queue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingSubscription {
    pub id: LandingId,
    pub generation: u64,
    pub subscriber: Subscriber,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumerCursor {
    pub subscriber: Subscriber,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingRecord {
    pub schema_version: u32,
    pub id: LandingId,
    pub repository: String,
    pub target: String,
    pub request: LandRequest,
    pub generation: u64,
    pub revision: u64,
    pub resolved_plan: ResolvedPlan,
    pub gate_definition: GateDefinition,
    pub outcomes: BTreeMap<u64, MemberOutcome>,
    pub progress: BTreeMap<u64, MemberProgress>,
    pub operation: Option<OperationRef>,
    pub submission: Option<Submission>,
    pub unavailable: Option<Unavailable>,
    pub evidence: Option<MergeEvidence>,
    pub publication_guarantee: PublicationGuarantee,
    pub cleanup_debt: CleanupDebt,
    pub restack_mappings: Vec<RestackMapping>,
    pub subscribers: Vec<Subscriber>,
    pub outbox: Vec<OutboxEntry>,
    /// Highest revision applied by each subscribed consumer.
    pub consumer_cursors: Vec<ConsumerCursor>,
    pub applied_observations: BTreeSet<ObservationKey>,
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Number of bounded owner passes; identity/revision are separate.
    pub passes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetirementEndpoint {
    Local,
    Remote { remote: String, push_url: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetirementOrigin {
    Created,
    Adopted,
}

/// Value-shape mirror of confinement::worktrees::RefOwnership; deliberately independent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementRef {
    pub repository: String,
    pub endpoint: RetirementEndpoint,
    pub reference: String,
    pub generation: u64,
    pub origin: RetirementOrigin,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetirementAction {
    #[default]
    Preserve,
    Discard,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementDecision {
    pub authority: String,
    pub reason: String,
    pub action: RetirementAction,
    pub decided_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RetirementStepState {
    Pending,
    Completed { detail: Option<String> },
    Refused { reason: String },
    Unknown { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementStepOutcome {
    pub step: String,
    pub state: RetirementStepState,
}

/// Durable intent and per-step outcome retained independently of the ref ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementRecord {
    pub schema_version: u32,
    pub id: String,
    pub reference: RetirementRef,
    pub expected_tip: String,
    pub decision: RetirementDecision,
    pub recovery_reference: Option<String>,
    pub revision: u64,
    pub outcomes: Vec<RetirementStepOutcome>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategies_share_one_choice_declaration_and_observer_capabilities()
    -> Result<(), serde_json::Error> {
        for declaration in STRATEGY_DECLARATIONS {
            let decoded: Strategy = serde_json::from_value(serde_json::json!(declaration.name))?;
            assert_eq!(decoded, declaration.strategy);
            let plan = declaration
                .strategy
                .plan(&Compose::default(), &Capabilities::default());
            let decisions = declaration
                .strategy
                .capability_decisions(&plan, &Capabilities::default());
            assert!(
                decisions
                    .iter()
                    .all(|decision| decision.selected == CapabilityPath::Observer)
            );
        }
        assert_eq!(
            Strategy::Batch
                .plan(&Compose::default(), &Capabilities::default())
                .candidate,
            CandidateKind::Batch
        );
        Ok(())
    }

    #[test]
    fn native_queue_is_selected_only_from_a_positive_capability_probe() {
        let unknown = Capabilities::default();
        assert_eq!(
            Strategy::Queue.plan(&Compose::default(), &unknown).schedule,
            Schedule::Ordered
        );
        let available = Capabilities {
            repository: Some("repo".into()),
            values: [(ForgeCapability::MergeQueue, CapabilityState::Available)].into(),
        };
        assert_eq!(
            Strategy::Queue
                .plan(&Compose::default(), &available)
                .schedule,
            Schedule::ForgeQueue
        );
        let stack_units = Compose {
            dependencies: Some(Dependencies::Stack),
            ..Compose::default()
        };
        assert_eq!(
            Strategy::Queue.plan(&stack_units, &available).schedule,
            Schedule::Ordered
        );
        let batch_units = Compose {
            candidate: Some(CandidateKind::Batch),
            ..Compose::default()
        };
        assert_eq!(
            Strategy::Queue.plan(&batch_units, &available).schedule,
            Schedule::Ordered
        );
    }

    #[test]
    fn landing_identity_binds_order_dependencies_generation_and_plan()
    -> Result<(), serde_json::Error> {
        let request = |members, generation| LandRequest {
            members,
            dependency_edges: vec![DependencyEdge {
                prerequisite: 1,
                dependent: 2,
            }],
            generation,
            ..LandRequest::default()
        };
        let members = vec![
            LandMember {
                pr: 1,
                head: Some("a".into()),
                base: Some("main".into()),
                review_proof: ReviewProof::Policy,
            },
            LandMember {
                pr: 2,
                head: Some("b".into()),
                base: Some("a".into()),
                review_proof: ReviewProof::Policy,
            },
        ];
        let first = request(members.clone(), 0);
        let id = LandingId::of("repo", "main", &first, "plan-a")?;
        assert_ne!(
            id,
            LandingId::of(
                "repo",
                "main",
                &request(members.iter().cloned().rev().collect(), 0),
                "plan-a"
            )?
        );
        assert_ne!(
            id,
            LandingId::of("repo", "main", &request(members.clone(), 1), "plan-a")?
        );
        let changed_plan = LandingId::of("repo", "main", &first, "plan-b")?;
        assert_ne!(id, changed_plan);
        assert_eq!(id.request, changed_plan.request);
        let mut changed = first.clone();
        changed.dependency_edges.clear();
        assert_ne!(id, LandingId::of("repo", "main", &changed, "plan-a")?);
        changed = first.clone();
        changed.members[0].head = Some("new-head".into());
        assert_ne!(id, LandingId::of("repo", "main", &changed, "plan-a")?);
        assert_ne!(id, LandingId::of("repo", "release", &first, "plan-a")?);
        Ok(())
    }

    #[test]
    fn retirement_and_pending_observation_roundtrip() -> Result<(), serde_json::Error> {
        let record = RetirementRecord {
            schema_version: 2,
            id: "retirement".into(),
            reference: RetirementRef {
                repository: "repo".into(),
                endpoint: RetirementEndpoint::Remote {
                    remote: "origin".into(),
                    push_url: "https://example.invalid/repo".into(),
                },
                reference: "refs/heads/work".into(),
                generation: 7,
                origin: RetirementOrigin::Created,
            },
            expected_tip: "tip".into(),
            decision: RetirementDecision {
                authority: "owner-decision".into(),
                reason: "finished".into(),
                action: RetirementAction::default(),
                decided_at: "2026-11-14T00:00:00Z".into(),
            },
            recovery_reference: None,
            revision: 1,
            outcomes: vec![RetirementStepOutcome {
                step: "worktree".into(),
                state: RetirementStepState::Pending,
            }],
        };
        let decoded: RetirementRecord = serde_json::from_slice(&serde_json::to_vec(&record)?)?;
        assert_eq!(decoded, record);
        let observation = ForgeObservation::Pending {
            pending: PendingState::AwaitingChecks,
        };
        let decoded: ForgeObservation = serde_json::from_slice(&serde_json::to_vec(&observation)?)?;
        assert_eq!(decoded, observation);
        Ok(())
    }

    #[test]
    fn resolved_plan_digest_includes_sources_and_capability_decisions()
    -> Result<(), serde_json::Error> {
        let caps = Capabilities::default();
        let a = ResolvedPlan::new(
            Strategy::Batch,
            &Compose::default(),
            caps.clone(),
            BTreeMap::new(),
        )?;
        let b = ResolvedPlan::new(
            Strategy::Batch,
            &Compose::default(),
            caps,
            [("strategy".into(), serde_json::json!({"layer":"project"}))].into(),
        )?;
        assert_ne!(a.digest, b.digest);
        assert!(
            Strategy::Async
                .ensure_executable(
                    &Strategy::Async.plan(&Compose::default(), &Capabilities::default())
                )
                .is_err()
        );
        Ok(())
    }
}
