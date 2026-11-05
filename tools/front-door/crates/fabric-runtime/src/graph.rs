//! Bounded DAG execution for Fabric graphs.
//!
//! `FileJournal` saves a complete canonical checkpoint with the OS layer's safe
//! filesystem API. `AttemptOwner` is implemented by composition over run. Its
//! key is idempotent: a repeated key observes the existing attempt instead of
//! launching a duplicate provider process.

use provenant_core::{CancelToken, Clock, DurationMicros, ProjectId, StateRoot, WaitBudget};
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode, PublishMode};
use provenant_records::{CanonicalError, CanonicalV1, DecodeError, Limits, decode_v1};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use thiserror::Error;

mod asks;
mod ci;
pub use ci::{CiFailedCheck, CiTarget};
mod budget;
#[cfg(test)]
#[path = "graph/ci_tests.rs"]
mod ci_tests;
mod composites;
mod envelope;
#[cfg(test)]
#[path = "graph/forward_compatibility.rs"]
mod forward_compatibility;
mod journal;
mod land;
#[cfg(test)]
#[path = "graph/land_tests.rs"]
mod land_tests;
mod loops;
mod model;
mod observation;
mod outputs;
mod pattern_plan;
mod plans;
mod validation;
#[doc(hidden)]
pub use plans::prompt as rendered_prompt;
pub use validation::minimum_schema_version;
#[cfg(test)]
mod admission_tests;
use budget::*;
pub use journal::*;
pub use model::*;
pub use observation::*;
use outputs::*;
pub(crate) use validation::*;
#[cfg(test)]
#[path = "graph/journal_tests.rs"]
mod file_tests;
#[cfg(test)]
#[path = "graph/graph_tests.rs"]
mod tests;
pub use asks::{
    AnswerSource, AskAnswer, AskKind, AskOption, AskReply, AskScope, DecideSpec, NodeAsk,
    close_resolved_asks, loop_ask,
};
pub use composites::CompositeSpec;
pub use land::{LandAuthority, LandObservation, LandRecord, LandRequest, LandSpec};
pub use loops::{
    InvalidLoopDecision, LegacyLoopAsk, LoopDecision, LoopRound, LoopSpec, legacy_loop_asks,
};
pub use plans::{
    Expansion, PatternInstantiation, PatternParameter, PatternParameterKind, PatternSpec, PlanSpec,
    RemainingBudgets,
};

fn recover_minima(checkpoint: &mut GraphCheckpoint, plan: &PlannedGraph) -> bool {
    let mut recovered = false;
    for (progress, planned) in checkpoint.nodes.iter_mut().zip(&plan.nodes) {
        if progress.min_successful.is_none()
            && let Some(minimum) = planned
                .composite
                .as_ref()
                .and_then(|spec| spec.min_successful)
        {
            progress.min_successful = Some(minimum);
            recovered = true;
        }
    }
    recovered
}

fn recover_status_minima(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
) -> Result<(), ()> {
    let limits = crate::config::GraphLimits {
        fan_out_width: usize::MAX,
        expansion_depth: usize::MAX,
        ..plans::generous()
    };
    plans::validate_persisted_topology(
        document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &limits,
    )
    .map_err(|_| ())?;
    let expanded = plans::expand(
        document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &limits,
    )
    .map_err(|_| ())?;
    let (plan, _) = validate_flat_plan(&expanded, &limits).map_err(|_| ())?;
    if checkpoint.nodes.len() != plan.nodes.len()
        || checkpoint
            .nodes
            .iter()
            .zip(&plan.nodes)
            .any(|(progress, planned)| progress.id != planned.id)
    {
        return Err(());
    }
    recover_minima(checkpoint, &plan);
    Ok(())
}

// Reserve the existing family-separation envelope for sibling groups too.
fn bounded_envelope_plan(plan: &PlannedGraph) -> PlannedGraph {
    let mut bounded = plan.clone();
    for node in &mut bounded.nodes {
        if node.family_group.is_some() && node.distinct_from.is_none() {
            node.distinct_from = Some(DistinctFrom::Dependencies);
        }
    }
    bounded
}

fn graph_envelope_reason(
    document: &GraphDocument,
    expansions: &[Expansion],
    extensions: &[String],
    plan: &PlannedGraph,
) -> Option<String> {
    envelope::reason(
        document,
        expansions,
        extensions,
        &bounded_envelope_plan(plan),
    )
}

fn graph_envelope_bound(
    document: &GraphDocument,
    expansions: &[Expansion],
    extensions: &[String],
    plan: &PlannedGraph,
) -> usize {
    envelope::checkpoint_bound(
        document,
        expansions,
        extensions,
        &bounded_envelope_plan(plan),
    )
}

pub const MAX_CANCEL_REASON_BYTES: usize = 256;
const MAX_CHECKPOINT_BYTES: u64 = 16_777_216;
/// Every JSON member costs at least two bytes, so the byte limit binds first.
const MAX_CHECKPOINT_MEMBERS: usize = MAX_CHECKPOINT_BYTES as usize / 2;
const MAX_ADVANCE_ERROR_BYTES: usize = 2_048;
// Canonical-byte shares for runtime text stored in a checkpoint. The envelope
// bound in `graph/envelope.rs` adds them up for each plan before admission.
const RESULT_EXCERPT_BYTES: usize = 4_096;
/// Canonical bytes of a declared handoff's remaining items carried in a node's
/// outputs; the full block stays with the stopped-early continuation.
const REMAINING_BYTES: usize = 4_096;
const COMPOSITE_EXCERPT_BYTES: usize = 65_536;
/// The raw findings block is at most 8 KiB of JSON; canonical form adds at
/// most a null `line` per finding.
const FINDINGS_BYTES: usize = 9_216;
// Oversized disposition projections retain their bounded findings block and
// reconstruct inherited copies from upstream checkpoint inputs.
const DISPOSITION_FINDINGS_BYTES: usize = 128 * 1024;
const WARNING_BYTES: usize = 512;
/// Output parsing, writer handoff and an empty review range can each add warnings.
const MAX_WARNINGS: usize = 5;
/// Usage failure and unknown-cost reasons.
const NOTE_BYTES: usize = 512;
const PAUSE_REASON_BYTES: usize = 4_096;
/// Host-issued attempt references, actors and boot identities.
const HOST_ID_BYTES: usize = 256;
const MAX_UNKNOWN_USAGE: usize = 64;
const MAX_USAGE_EVENTS: usize = 128;

/// Bounded retries for transient launch failures.
#[derive(Debug, Clone, Copy)]
pub struct LaunchRetryPolicy {
    pub limit: u32,
    pub first_seconds: u64,
    pub max_seconds: u64,
}
impl Default for LaunchRetryPolicy {
    fn default() -> Self {
        Self {
            limit: 3,
            first_seconds: 5,
            max_seconds: 600,
        }
    }
}

/// Entries a prompt lists before referring to the complete input index.
pub const UPSTREAM_INPUT_LIST_LIMIT: usize = 64;

/// Narrow read-only host interface for CI checks.
pub trait CiStatus {
    fn host_stamp(&self) -> Option<CiHostStamp>;
    fn observe_ci(
        &mut self,
        target: &str,
        checks: &[String],
        failed_checks_max_bytes: usize,
        max_failed_checks: usize,
    ) -> CiObservation;
}

/// `observe_or_start` must be idempotent by `AttemptKey`. It starts the run
/// only when that key has no accepted operation, and otherwise observes it.
/// A `stored_ref` from the checkpoint names an accepted attempt: observe it as
/// is, never resubmit. Without one, the submission replays under `launch_actor`
/// (the actor recorded before the first submit) so a changed actor cannot
/// launch duplicate work. Provider launch and terminal publication remain run's responsibility.
#[derive(Debug, Error)]
pub enum ReviewResolveError {
    #[error("review resolution is unsupported by this host")]
    Unsupported,
    #[error("review resolution failed: {0}")]
    Failed(String),
}

pub trait AttemptOwner {
    type Error: Error + 'static;

    /// Resolve the existing infrastructure allowance once, before admission.
    /// Hosts without routing policy retain the legacy explicit-route behavior.
    fn retry_allowance(
        &self,
        _node: &provenant_records::ExtraFields,
        _graph: &provenant_records::ExtraFields,
    ) -> Result<Option<(u8, String)>, Self::Error> {
        Ok(None)
    }

    /// Infrastructure successors already published in this run lineage.
    fn retry_successors(&mut self, _attempt_ref: &str) -> Result<u64, Self::Error> {
        Ok(0)
    }

    /// Maximum time to admit a launch, measured from its first submission.
    fn admission_timeout_seconds(&self) -> Option<u64> {
        None
    }

    /// Effective launch retry policy for this graph advance.
    fn launch_retry_policy(&self) -> Result<LaunchRetryPolicy, Self::Error> {
        Ok(LaunchRetryPolicy::default())
    }

    /// Re-read host-owned proof of a pre-custody preparation refusal and
    /// released custody. Missing or unverifiable proof never authorizes a route.
    fn preparation_fallback_evidence(
        &mut self,
        _key: &AttemptKey,
        _reference: &str,
    ) -> Result<Option<PreparationFallbackEvidence>, Self::Error> {
        Ok(None)
    }

    /// Resolve a deferred review; hosts without support fail closed.
    fn resolve_review(
        &mut self,
        _node: &GraphNode,
        _subject: &str,
    ) -> Result<Expansion, ReviewResolveError> {
        Err(ReviewResolveError::Unsupported)
    }

    /// Pin the integration branch at the instant this node is first admitted.
    fn integration_head(&mut self) -> Result<Option<String>, Self::Error> {
        Ok(None)
    }

    /// A host without CI support fails closed.
    fn ci_status(&mut self) -> Option<&mut dyn CiStatus> {
        None
    }

    /// Effective host policy for this CI gate.
    fn ci_poll_backoff(&self, _node_id: &str) -> CiPollBackoff {
        CiPollBackoff::default()
    }

    /// Scheduled ticks honour per-gate deadlines. Explicit advances can probe
    /// immediately without waiting for the scheduled poll.
    fn respect_ci_wake(&self) -> bool {
        false
    }

    /// The actor this owner submits as, recorded before the first submit.
    fn actor(&self) -> String;

    /// Raise the ask if it is new, idempotently by its ask ID, and return its
    /// answer once there is one. A host without an inbox never answers.
    fn node_ask(&mut self, _scope: AskScope<'_>) -> Result<Option<AskReply>, Self::Error> {
        Ok(None)
    }
    /// Withdraw a resolved ask's inbox request. Closing never creates one.
    fn close_node_ask(&mut self, _scope: AskScope<'_>) -> Result<(), Self::Error> {
        Ok(())
    }
    /// Wall clock for ask deadlines. Without one, a decide node with a timeout
    /// refuses to ask, and a stored deadline does not fire.
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        None
    }
    fn deliver_graph(
        &mut self,
        _document: &GraphDocument,
        _checkpoint: &GraphCheckpoint,
    ) -> Result<Option<String>, Self::Error> {
        Ok(None)
    }

    /// Check newly expanded targets against host evidence before publishing them.
    fn validate_expansion(
        &mut self,
        _document: &GraphDocument,
        _checkpoint: &GraphCheckpoint,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn usage(&mut self, _attempt_ref: &str) -> Result<AttemptUsage, Self::Error> {
        Ok(AttemptUsage::default())
    }

    fn cost_unknown_reason(&mut self) -> Option<String> {
        None
    }

    /// Model families that served these attempts. Unknown attempts are skipped.
    fn attempt_families(&mut self, _attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        Ok(Vec::new())
    }

    /// Models that served the referenced attempts, in attempt order.
    fn attempt_models(&mut self, _attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        Ok(Vec::new())
    }

    /// The effective land policy for a risk tier and the layer it came from.
    fn land_policy(
        &self,
        tier: provenant_records::workflow::RiskTier,
    ) -> crate::config::LandChoice {
        crate::config::GraphLand::default().choose(None, tier)
    }

    /// Merge an authorised land node's pull request through the repository's
    /// own workflow, pinned to `request.head`. Idempotent: a PR already merged
    /// at that head reports `Merged`. A host without one refuses.
    fn land(&mut self, _request: LandRequest<'_>) -> Result<LandObservation, Self::Error> {
        Ok(LandObservation::Refused {
            reason: "this host cannot land pull requests".into(),
        })
    }

    /// Write each requested upstream result, verified and redacted as
    /// `provenant output --full` reads it, into a directory only this attempt
    /// may read, within `cap` bytes in total. Idempotent by `key`. A host that
    /// cannot materialise returns `None` and the prompt keeps its excerpts.
    fn materialise_inputs(
        &mut self,
        _key: &AttemptKey,
        _requests: &[InputRequest],
        _cap: usize,
    ) -> Option<MaterialisedInputs> {
        None
    }

    /// Reissue an idempotent stop if a policy successor appeared during cancellation.
    fn stop_cancelled_attempt(&mut self, _attempt: &GraphAttempt) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Read-only check: never starts or replays an attempt.
    fn cancelled_attempt_settled(&mut self, _attempt: &GraphAttempt) -> Result<bool, Self::Error> {
        Ok(false)
    }

    /// Whether a launch already crossed run's acceptance boundary. This is
    /// read-only and must not submit a new operation.
    fn launch_accepted(&mut self, _attempt: &GraphAttempt) -> Result<bool, Self::Error> {
        Ok(false)
    }

    /// Whether the immutable task reviewed an empty commit range.
    fn equal_review_range(&mut self, _attempt_ref: &str) -> Result<bool, Self::Error> {
        Ok(false)
    }

    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        node: NodeInput<'_>,
        route: &GraphRoute,
        access: AccessMode,
        worktree: Option<&str>,
        replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error>;
}

/// The file journal persists every state transition atomically. Its held lock
/// fences concurrent writers for this graph. The caller must hold exclusive
/// access to the journal for the whole advance.
pub trait GraphJournal {
    type Error: Error + 'static;

    fn load(&mut self, graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error>;
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error>;
}

#[derive(Debug, Error)]
pub enum GraphError<RunError: Error + 'static, JournalError: Error + 'static> {
    #[error("cannot create graph checkpoint instance identity")]
    Instance(#[source] FsError),
    #[error("invalid graph: {0}")]
    Invalid(String),
    #[error("invalid extension: {0}")]
    Extension(String),
    /// The graph cannot advance as it stands; the text names the next step.
    #[error("{0}")]
    Refused(String),
    #[error("graph checkpoint does not match the submitted plan")]
    PlanChanged,
    #[error("graph checkpoint node {node}: {invariant}")]
    BadCheckpoint {
        node: String,
        invariant: &'static str,
    },
    #[error("cannot canonically encode the validated graph plan")]
    Canonical(#[source] CanonicalError),
    #[error("run attempt owner failed: {0}")]
    Run(#[source] RunError),
    #[error("graph journal failed: {0}")]
    Journal(#[source] JournalError),
}

impl<R: Error + 'static, J: Error + 'static> GraphError<R, J> {
    /// Stable identifier independent of the displayed explanation.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Instance(_) => "instance",
            Self::Invalid(_) => "invalid_graph",
            Self::Extension(_) => "invalid_extension",
            Self::Refused(_) => "graph_refused",
            Self::PlanChanged => "plan_changed",
            Self::BadCheckpoint { .. } => "bad_checkpoint",
            Self::Canonical(_) => "canonical",
            Self::Run(_) => "run",
            Self::Journal(_) => "journal",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct PlannedGraph {
    #[serde(skip_serializing_if = "Option::is_none")]
    review_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<provenant_records::run::WorkContext>,
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    #[serde(skip_serializing_if = "Option::is_none")]
    integration_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<provenant_records::run::DeliveryMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_session: Option<String>,
    schema_version: u64,
    graph_id: String,
    nodes: Vec<PlannedNode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct PlannedNode {
    #[serde(skip_serializing_if = "Option::is_none")]
    review: Option<ReviewSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    family_group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<provenant_records::run::WorkContext>,
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    composite_members: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    composite: Option<CompositeSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    distinct_from: Option<DistinctFrom>,
    #[serde(skip_serializing_if = "Option::is_none")]
    distinct_strength: Option<FamilyStrength>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dispositions: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    read_roots: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<SourceBinding>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    after_expansion: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<PlanSpec>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    write_scope: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    acceptance_checks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    loop_body: Option<LoopSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decide: Option<DecideSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    continues: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    land: Option<LandSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    round: Option<LoopRound>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<NodeKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    condition: Option<GateCondition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_fail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<provenant_records::run::DeliveryMode>,
    #[serde(default, skip_serializing_if = "model::is_false")]
    require_skill_location: bool,
    access: AccessMode,
    worktree: Option<String>,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deadline_seconds: Option<u64>,
    prompt: String,
    depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "model::is_false")]
    advisory: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    inputs: Vec<GraphInput>,
    routes: Vec<GraphRoute>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_verdict: Option<String>,
    #[serde(skip_serializing_if = "model::is_false")]
    require_findings: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    skip_without_findings: Option<bool>,
}

/// Node labels override graph defaults. Scheduler identity comes from the plan.
fn node_context(plan: &PlannedGraph, node: &PlannedNode) -> provenant_records::run::WorkContext {
    let defaults = provenant_records::run::WorkContext::default();
    let graph = plan.context.as_ref().unwrap_or(&defaults);
    let context = node.context.as_ref().unwrap_or(&defaults);
    provenant_records::run::WorkContext {
        extra: Default::default(),
        graph_id: Some(plan.graph_id.clone()),
        node_id: Some(node.id.clone()),
        role: context
            .role
            .clone()
            .or_else(|| node.role.clone())
            .or_else(|| graph.role.clone()),
        work_item: context
            .work_item
            .clone()
            .or_else(|| graph.work_item.clone()),
        preset: context.preset.clone().or_else(|| graph.preset.clone()),
        pattern: context.pattern.clone().or_else(|| graph.pattern.clone()),
        role_digest: context
            .role_digest
            .clone()
            .or_else(|| graph.role_digest.clone()),
        experiment_arm: context
            .experiment_arm
            .clone()
            .or_else(|| graph.experiment_arm.clone()),
        shadow_of: context
            .shadow_of
            .clone()
            .or_else(|| graph.shadow_of.clone()),
        selection_source: context
            .selection_source
            .clone()
            .or_else(|| graph.selection_source.clone()),
    }
}

impl PlannedNode {
    fn dependencies(&self) -> impl Iterator<Item = &String> {
        self.depends_on.iter().chain(
            self.after_expansion
                .iter()
                .filter(|name| !self.depends_on.contains(name)),
        )
    }
}

/// The verifier belongs to the same panel when it follows the deferred lens
/// group in the same composite scope. Lens members belong directly.
fn review_panel_contains(
    expansion: &Expansion,
    node: &PlannedNode,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    if expansion.review.is_none() {
        return false;
    }
    if expansion.nodes.iter().any(|member| member.id == node.id) {
        return true;
    }
    if node.role.as_deref() != Some("review.verify")
        || node.id.rsplit_once("--").map(|(scope, _)| scope)
            != expansion.source.rsplit_once("--").map(|(scope, _)| scope)
    {
        return false;
    }
    let mut pending: Vec<&String> = node.dependencies().collect();
    let mut seen = std::collections::BTreeSet::new();
    while let Some(id) = pending.pop() {
        if id == &expansion.source {
            return true;
        }
        if let Some(index) = positions.get(id)
            && seen.insert(*index)
        {
            pending.extend(plan.nodes[*index].dependencies());
        }
    }
    false
}

/// Advance all currently ready nodes once. Independent nodes can progress
/// while a dependency is still running. Stable keys make calls safe to repeat.
#[cfg(test)]
pub fn advance<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    advance_with_budget(
        document,
        owner,
        journal,
        &crate::config::GraphLimits::default(),
        &crate::config::GraphBudget::default(),
    )
}

/// Explicitly reopen graph admission without changing any attempt identity.
#[cfg(test)]
pub fn resume<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    resume_with_budget(
        document,
        owner,
        journal,
        &crate::config::GraphLimits::default(),
        &crate::config::GraphBudget::default(),
        &[],
    )
}

pub fn advance_with_budget<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    advance_inner(
        document,
        owner,
        journal,
        limits,
        budget,
        false,
        &[],
        &mut Default::default(),
    )
}

pub fn resume_with_budget<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
    extensions: &[String],
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    advance_inner(
        document,
        owner,
        journal,
        limits,
        budget,
        true,
        extensions,
        &mut Default::default(),
    )
}

/// Whether a failed launch is worth a bounded retry, decided only from the
/// error's typed kind, never from its prose. Host capacity, store
/// contention, wait expiry, an unconfirmed launch handoff and filesystem I/O
/// are transient; access, confinement, invalid input and every untyped error
/// block the node. The launch itself stays idempotent by attempt key, so a
/// retry observes an already accepted attempt instead of launching twice.
fn launch_retry_kind(error: &(dyn Error + 'static)) -> bool {
    use crate::coordination::facade::FacadeError;
    use provenant_core::retry::{RetryCause, is_transient};
    let cause = match error.downcast_ref::<FacadeError>() {
        Some(FacadeError::Busy | FacadeError::WriterLockBusy | FacadeError::WriterUnknown) => {
            RetryCause::Contention
        }
        Some(FacadeError::Timeout) => RetryCause::Timeout,
        Some(
            FacadeError::HomeUnavailable
            | FacadeError::Io { .. }
            | FacadeError::DispatchUnobserved { .. }
            | FacadeError::HostDeliveryStage { .. }
            | FacadeError::HostDeliveryLimit { .. }
            | FacadeError::LaunchUnknown { .. },
        ) => RetryCause::Transport,
        _ => RetryCause::Permanent,
    };
    is_transient(cause)
}

fn launch_diagnostic(error: &(dyn Error + 'static)) -> provenant_records::diagnostic::Diagnostic {
    error
        .downcast_ref::<crate::coordination::facade::FacadeError>()
        .map(crate::coordination::facade::FacadeError::diagnostic)
        .unwrap_or_else(|| provenant_records::diagnostic::Diagnostic::new("launch_failed"))
}

/// Apply a persisted disposition to a node whose launch errored while its
/// attempt was still starting: a bounded retry with backoff for a transient
/// kind, a blocked node with reason and remedy otherwise and after the
/// admission deadline. Saves the checkpoint either way.
fn launch_disposition<R: AttemptOwner, J: GraphJournal>(
    checkpoint: &mut GraphCheckpoint,
    index: usize,
    error: &R::Error,
    clock: Option<provenant_core::WallTime>,
    admission_seconds: u64,
    policy: LaunchRetryPolicy,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let retrying = launch_retry_kind(error);
    if retrying {
        let Some(now) = clock else {
            return Err(GraphError::Refused(format!(
                "node {}: no wall clock to set its launch retry deadline; retry the advance",
                checkpoint.nodes[index].id
            )));
        };
        let deadline = match checkpoint.nodes[index]
            .launch_retry
            .as_ref()
            .and_then(|retry| retry.deadline)
        {
            Some(deadline) => deadline,
            None => DurationMicros::from_seconds(admission_seconds)
                .ok()
                .and_then(|duration| now.checked_add(duration).ok())
                .ok_or_else(|| {
                    GraphError::Refused(format!(
                        "node {}: launch retry deadline is unavailable; retry the advance",
                        checkpoint.nodes[index].id
                    ))
                })?,
        };
        let retry = checkpoint.nodes[index]
            .launch_retry
            .get_or_insert_with(|| LaunchRetry {
                extra: Default::default(),
                failures: 0,
                deadline: None,
                wake_at: None,
                reason: None,
            });
        retry.failures = retry.failures.saturating_add(1);
        retry.deadline = Some(deadline);
        if now < deadline && u32::from(retry.failures) <= policy.limit {
            let delay = provenant_core::retry::Backoff::new(
                std::time::Duration::from_secs(policy.first_seconds),
                2,
                std::time::Duration::from_secs(policy.max_seconds.max(policy.first_seconds)),
            )
            .delay(u64::from(retry.failures) - 1);
            retry.wake_at = Some(
                DurationMicros::from_seconds(delay.as_secs())
                    .ok()
                    .and_then(|duration| now.checked_add(duration).ok())
                    .map_or(deadline, |wake| wake.min(deadline)),
            );
            let projected = launch_diagnostic(error);
            retry.reason = Some(bounded(
                projected.detail.as_deref().unwrap_or(&projected.code),
                MAX_ADVANCE_ERROR_BYTES,
            ));
            asks::refresh_wake(checkpoint);
            return journal.save(checkpoint).map_err(GraphError::Journal);
        }
    }
    apply_launch_block(
        &checkpoint.graph_id,
        &mut checkpoint.nodes[index],
        error,
        retrying,
    );
    asks::refresh_wake(checkpoint);
    journal.save(checkpoint).map_err(GraphError::Journal)
}

/// Apply a launch refusal to one node without accessing a journal or owner.
fn apply_launch_block(
    graph_id: &str,
    progress: &mut NodeProgress,
    error: &(dyn Error + 'static),
    retrying: bool,
) {
    // A permanent refusal, or a retry budget spent: block the node with the
    // reason and a remedy, and keep the refusal visible in graph status.
    let id = &progress.id;
    let code = if retrying {
        "launch_retry_exhausted"
    } else {
        "launch_refused"
    };
    progress.advance_error_code = Some(code.into());
    let projected = launch_diagnostic(error);
    let detail = projected.detail.as_deref().unwrap_or(&projected.code);
    let action = projected
        .next
        .as_deref()
        .unwrap_or("inspect the node's inputs and state before retrying");
    let recovery = if error
        .downcast_ref::<crate::coordination::facade::FacadeError>()
        .is_some_and(|e| {
            matches!(
                e,
                crate::coordination::facade::FacadeError::LaunchUnknown { .. }
            )
        }) {
        String::new()
    } else if graph_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        format!(
            "; after correcting the cause, cancel with `provenant fabric graph cancel {graph_id}` and rerun with a new graph_id"
        )
    } else {
        "; inspect and cancel the graph before rerunning with a new graph_id".into()
    };
    progress.advance_error = Some(bounded(
        &format!(
            "node {}: the launch failed [{}]: {detail}; {action}{recovery}",
            provenant_records::diagnostic::safe_text(id),
            projected.code
        ),
        MAX_ADVANCE_ERROR_BYTES,
    ));
    progress.state = NodeState::Blocked;
    // Keep the attempt row even when launch handoff is uncertain. A guardian
    // may already exist, and cancellation needs its identity and custody.
    progress.launch_retry = None;
}

/// Revert cancellation-only state changes without altering attempt records.
fn restore_cancelled_nodes(checkpoint: &mut GraphCheckpoint) -> Result<(), (String, &'static str)> {
    let saved_wake = checkpoint.wake_at;
    asks::refresh_wake(checkpoint);
    if checkpoint.wake_at != saved_wake {
        return Err((
            String::new(),
            "graph wake time disagrees with its pending asks",
        ));
    }
    for node in &mut checkpoint.nodes {
        match node.cancelled_from.take() {
            Some(state @ (NodeState::Pending | NodeState::Running))
                if checkpoint.cancelled
                    && node.state == NodeState::Skipped
                    && node.skip_reason.as_deref() == Some("graph cancelled") =>
            {
                node.state = state;
                node.skip_reason = None;
            }
            None => {}
            _ => {
                return Err((
                    node.id.clone(),
                    "cancelled node can only be restored from pending or running",
                ));
            }
        }
    }
    asks::refresh_wake(checkpoint);
    Ok(())
}

/// Mark nodes that have no live attempts, as part of the durable cancellation fence.
pub fn mark_cancelled_nodes_without_live_attempts(checkpoint: &mut GraphCheckpoint) -> bool {
    if !checkpoint.cancelled {
        return false;
    }
    let mut changed = false;
    for node in &mut checkpoint.nodes {
        if matches!(node.state, NodeState::Pending | NodeState::Running)
            && node
                .attempts
                .iter()
                .all(|attempt| attempt.state == AttemptState::Terminal)
        {
            node.cancelled_from = Some(node.state);
            node.state = NodeState::Skipped;
            node.skip_reason = Some("graph cancelled".into());
            changed = true;
        }
    }
    if changed {
        asks::refresh_wake(checkpoint);
    }
    changed
}

/// Settle cancellation without admitting work; retain original states for resume.
pub fn settle_cancelled_nodes<R: AttemptOwner, J: GraphJournal>(
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    if !checkpoint.cancelled {
        return Ok(());
    }
    if mark_cancelled_nodes_without_live_attempts(checkpoint) {
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    let mut first_error = None;
    for index in 0..checkpoint.nodes.len() {
        let node = &checkpoint.nodes[index];
        if node.state != NodeState::Running {
            continue;
        }
        let mut settled = true;
        for attempt in &node.attempts {
            if attempt.state != AttemptState::Terminal {
                match owner.cancelled_attempt_settled(attempt) {
                    Ok(true) => {}
                    Ok(false) => {
                        settled = false;
                        if let Err(error) = owner.stop_cancelled_attempt(attempt) {
                            first_error.get_or_insert(error);
                        }
                    }
                    Err(error) => {
                        settled = false;
                        first_error.get_or_insert(error);
                        if let Err(error) = owner.stop_cancelled_attempt(attempt) {
                            first_error.get_or_insert(error);
                        }
                    }
                }
            }
        }
        if settled {
            let node = &mut checkpoint.nodes[index];
            node.cancelled_from = Some(node.state);
            node.state = NodeState::Skipped;
            node.skip_reason = Some("graph cancelled".into());
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
    }
    match first_error {
        Some(error) => Err(GraphError::Run(error)),
        None => Ok(()),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one observation set follows internal rescheduling for this advance"
)]
fn advance_inner<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
    mut resume: bool,
    extensions: &[String],
    observed_ci: &mut std::collections::BTreeSet<String>,
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    let unbounded = crate::config::GraphLimits {
        nodes: usize::MAX,
        edges: usize::MAX,
        routes_per_node: u16::MAX as usize,
        node_prompt_bytes: usize::MAX,
        graph_prompt_bytes: usize::MAX,
        fan_out_width: usize::MAX,
        expansion_depth: usize::MAX,
        ..limits.clone()
    };
    let stored = journal
        .load(&document.graph_id)
        .map_err(GraphError::Journal)?;
    if let Some(checkpoint) = &stored {
        plans::validate_persisted_topology(
            document,
            &checkpoint.extensions,
            &checkpoint.expansions,
            &unbounded,
        )
        .map_err(|(node, invariant)| GraphError::BadCheckpoint { node, invariant })?;
    }
    let stored_extensions = stored.as_ref().map_or(&[][..], |c| c.extensions.as_slice());
    let (base, _) = if stored.is_some() {
        validate_replay_plan(document, &unbounded)
    } else {
        validate_plan_with_limits(document, &unbounded)
    }
    .map_err(GraphError::Invalid)?;
    let mut expanded = plans::expand(
        document,
        stored_extensions,
        stored.as_ref().map_or(&[], |c| c.expansions.as_slice()),
        &unbounded,
    )
    .map_err(|reason| GraphError::Invalid(reason.to_string()))?;
    let (mut plan, mut positions) = validate_flat_plan(&expanded, &unbounded)
        .map_err(|reason| GraphError::Invalid(reason.to_string()))?;
    let plan_digest = CanonicalV1::record_digest("fabric-graph-plan", 1, &base)
        .map_err(GraphError::Canonical)?
        .to_string();

    let mut checkpoint = match stored {
        Some(checkpoint) => {
            let mut validation = checkpoint.clone();
            restore_cancelled_nodes(&mut validation)
                .map_err(|(node, invariant)| GraphError::BadCheckpoint { node, invariant })?;
            validate_checkpoint(document, &validation, &plan, &plan_digest)?;
            checkpoint
        }
        None => {
            if let Some(reason) = graph_envelope_reason(document, &[], &[], &plan) {
                return Err(GraphError::Invalid(reason));
            }
            let asks = may_ask(&plan);
            let checkpoint = GraphCheckpoint {
                extra: Default::default(),
                expansions: Vec::new(),
                remaining: None,
                instance_id: if asks {
                    Some(provenant_core::hex(
                        &provenant_os::fs::entropy_128().map_err(GraphError::Instance)?,
                    ))
                } else {
                    None
                },
                asks: Vec::new(),
                ask_actor: asks.then(|| owner.actor()),
                wake_at: None,
                pause_asks: 0,
                delivery_required: document.delivery
                    == Some(provenant_records::run::DeliveryMode::Pr),
                delivered: false,
                pull_request: None,
                schema_version: 1,
                cancelled: false,
                cancel_reason: None,
                integration_pin: None,
                pause_reason: None,
                pause_code: None,
                unknown_usage: Vec::new(),
                unknown_usage_omitted: 0,
                usage_events: Vec::new(),
                usage_events_omitted: 0,
                settlements: 0,
                extensions: Vec::new(),
                limit_notes: Vec::new(),
                graph_id: plan.graph_id.clone(),
                plan_digest,
                nodes: plan
                    .nodes
                    .iter()
                    .map(|node| NodeProgress {
                        extra: Default::default(),
                        children: node.composite_members.clone(),
                        min_successful: node
                            .composite
                            .as_ref()
                            .and_then(|spec| spec.min_successful),
                        source_commit: None,
                        fragment: None,
                        handoff: None,
                        expansion_pause: None,
                        loop_pause: None,
                        loop_pause_code: None,
                        round: node.round.clone(),
                        gate: None,
                        separation: None,
                        ci: None,
                        land: None,
                        id: node.id.clone(),
                        title: Some(node.title.as_deref().unwrap_or(&node.id).to_owned()),
                        worktree: node.worktree.clone(),
                        state: NodeState::Pending,
                        cancelled_from: None,
                        skip_reason: None,
                        advance_error: None,
                        advance_error_code: None,
                        launch_retry: None,
                        outputs: None,
                        attempts: Vec::new(),
                    })
                    .collect(),
            };
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
            checkpoint
        }
    };

    if recover_minima(&mut checkpoint, &plan) {
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }

    if may_ask(&plan) && (checkpoint.instance_id.is_none() || checkpoint.ask_actor.is_none()) {
        asks::identity::<R, J::Error>(&mut checkpoint, owner)?;
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }
    // Extensions have no count or length limit of their own; they only have
    // to fit in the checkpoint envelope with everything else.
    if extensions
        .iter()
        .any(|e| !checkpoint.extensions.contains(e))
    {
        let mut all = checkpoint.extensions.clone();
        all.extend(
            extensions
                .iter()
                .filter(|e| !checkpoint.extensions.contains(e))
                .cloned(),
        );
        if let Some(reason) = graph_envelope_reason(document, &checkpoint.expansions, &all, &plan) {
            return Err(GraphError::Extension(reason));
        }
    }
    let mut effective_budget = budget.clone();
    for (extension, replay) in
        checkpoint
            .extensions
            .iter()
            .map(|e| (e, true))
            .chain(extensions.iter().map(|e| {
                (
                    e,
                    e.starts_with("limits.") && checkpoint.extensions.contains(e),
                )
            }))
    {
        if extension.starts_with("loop.") {
            loops::validate_extension(document, extension, &unbounded)
                .map_err(GraphError::Extension)?;
        } else if extension.starts_with("limits.") {
            if !replay {
                return Err(GraphError::Extension(format!(
                    "{extension}: graph limits are configured by the host"
                )));
            }
        } else {
            apply_extension(&mut effective_budget, extension, replay)
                .map_err(|error| GraphError::Extension(error.to_string()))?;
        }
    }
    let limit_notes = document_limit_notes(document);
    if checkpoint.limit_notes != limit_notes {
        checkpoint.limit_notes = limit_notes;
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }
    if resume {
        let mut resume_view = checkpoint.clone();
        restore_cancelled_nodes(&mut resume_view)
            .map_err(|(node, invariant)| GraphError::BadCheckpoint { node, invariant })?;
        if extensions.iter().any(|e| e.starts_with("loop.")) {
            let mut prospective = resume_view.clone();
            for extension in extensions.iter().filter(|e| e.starts_with("loop.")) {
                loops::validate_resume_extension(document, &resume_view, extension, &unbounded)
                    .map_err(GraphError::Extension)?;
                loops::validate_resume_extension(document, &prospective, extension, &unbounded)
                    .map_err(GraphError::Extension)?;
                if !prospective.extensions.contains(extension) {
                    prospective.extensions.push(extension.clone());
                }
            }
        }
        if extensions.iter().any(|e| {
            e.starts_with("loop.")
                && e.split_once('=')
                    .is_some_and(|(key, _)| key.ends_with(".rounds"))
        }) {
            let mut prospective = checkpoint.clone();
            for extension in extensions {
                if !prospective.extensions.contains(extension) {
                    prospective.extensions.push(extension.clone());
                }
            }
            let preview = plans::expand(
                document,
                &prospective.extensions,
                &checkpoint.expansions,
                &unbounded,
            )
            .map_err(|reason| GraphError::Invalid(reason.to_string()))?;
            let (preview_plan, _) = validate_flat_plan(&preview, &unbounded)
                .map_err(|reason| GraphError::Invalid(reason.to_string()))?;
            if let Some(reason) =
                structural_limit_reason(&preview, &preview_plan, limits).or_else(|| {
                    graph_envelope_reason(
                        document,
                        &checkpoint.expansions,
                        &prospective.extensions,
                        &preview_plan,
                    )
                })
            {
                for extension in extensions.iter().filter(|e| e.starts_with("loop.")) {
                    let name = extension
                        .strip_prefix("loop.")
                        .and_then(|e| e.split_once('=').map(|(key, _)| key))
                        .and_then(|key| key.rsplit_once('.').map(|(name, _)| name))
                        .ok_or(GraphError::Invalid(String::from("loop extension")))?;
                    loops::pause_invalid_decision(&mut checkpoint, name, &reason, journal)?;
                }
                return Ok(GraphRun {
                    status: if checkpoint.cancelled {
                        GraphStatus::Cancelled
                    } else {
                        GraphStatus::Paused
                    },
                    outcome: None,
                    checkpoint,
                    advance_error: None,
                });
            }
        }
        // Keep the cancellation fence and its displayed states durable until
        // every extension is accepted. Reopen in the same save as those states.
        if checkpoint.cancelled {
            close_resolved_asks(&mut checkpoint, owner, journal)?;
            restore_cancelled_nodes(&mut checkpoint)
                .map_err(|(node, invariant)| GraphError::BadCheckpoint { node, invariant })?;
            checkpoint.cancelled = false;
        }
        for extension in extensions {
            if !checkpoint.extensions.contains(extension) {
                checkpoint.extensions.push(extension.clone());
            }
        }
        checkpoint.pause_reason = None;
        checkpoint.pause_code = None;
        if extensions.iter().any(|e| e.starts_with("loop.")) {
            let expanded = plans::expand(
                document,
                &checkpoint.extensions,
                &checkpoint.expansions,
                &unbounded,
            )
            .map_err(|reason| GraphError::Invalid(reason.to_string()))?;
            let (extended, _) = validate_flat_plan(&expanded, &unbounded)
                .map_err(|reason| GraphError::Invalid(reason.to_string()))?;
            for extension in extensions.iter().filter(|e| e.starts_with("loop.")) {
                let name = extension
                    .strip_prefix("loop.")
                    .and_then(|e| e.split_once('=').map(|(key, _)| key))
                    .and_then(|key| key.rsplit_once('.').map(|(name, _)| name))
                    .ok_or(GraphError::Invalid(String::from("loop extension")))?;
                if let Some(node) = checkpoint.nodes.iter_mut().find(|n| n.id == name) {
                    node.loop_pause = None;
                    node.loop_pause_code = None;
                }
            }
            loops::reconcile(&mut checkpoint, &extended);
            checkpoint.cancelled = false;
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
            return advance_inner(
                document,
                owner,
                journal,
                limits,
                budget,
                false,
                &[],
                observed_ci,
            );
        }
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }

    'schedule: loop {
        plans::remaining(&mut checkpoint, &expanded, limits);
        for node in &plan.nodes {
            if (!checkpoint.cancelled || resume)
                && checkpoint.nodes.iter().any(|n| {
                    n.id == node.id
                        && (n.handoff.is_some()
                            || n.fragment.is_some()
                            || (n.state == NodeState::Failed
                                && node
                                    .plan
                                    .as_ref()
                                    .is_some_and(|spec| spec.fallback.is_some()))
                            || (n.state == NodeState::Succeeded
                                && n.expansion_pause.is_some()
                                && node
                                    .plan
                                    .as_ref()
                                    .is_some_and(|spec| spec.fallback.is_some())))
                })
                && let Some((next_expanded, next_plan, next_positions)) =
                    plans::settle(node, document, &expanded, &mut checkpoint, None, limits)
            {
                validate_expanded_rows(&checkpoint, &next_plan)?;
                owner
                    .validate_expansion(&next_expanded, &checkpoint)
                    .map_err(GraphError::Run)?;
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                resume = false;
                continue 'schedule;
            }
        }
        if let Some(reason) = checkpoint.nodes.iter().find_map(|node| {
            node.expansion_pause
                .as_deref()
                .map(plans::pause_reason_for_display)
        }) {
            checkpoint.pause_reason = Some(reason);
            checkpoint.pause_code = Some("invalid_plan_expansion".into());
        }
        let structural_pause = if checkpoint.nodes.iter().all(|node| node.state.settled()) {
            None
        } else {
            structural_limit_reason(&expanded, &plan, limits).or_else(|| {
                let reserved = plan.nodes.iter().filter(|node| !checkpoint.expansions.iter().any(|expansion| expansion.source == node.id))
                    .filter_map(|node| node.review.as_ref()).fold(expanded.nodes.len(), |total, review| total.saturating_add(review.max_nodes));
                (reserved > limits.nodes).then(|| format!("limits.nodes={} reached ({reserved} including deferred review reservations); set by domains.fabric.graph.limits", limits.nodes))
            })
        };
        if let Some(reason) = &structural_pause {
            checkpoint.pause_reason = Some(bounded(reason, PAUSE_REASON_BYTES));
            checkpoint.pause_code = Some("structural_limit".into());
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }

        close_resolved_asks(&mut checkpoint, owner, journal)?;
        if checkpoint.cancelled {
            if !resume {
                settle_cancelled_nodes(&mut checkpoint, owner, journal)?;
                return Ok(GraphRun {
                    status: GraphStatus::Cancelled,
                    outcome: None,
                    checkpoint,
                    advance_error: None,
                });
            }
            checkpoint.cancelled = false;
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }

        for node_index in 0..plan.nodes.len() {
            if checkpoint.nodes[node_index].state.settled() {
                if checkpoint.nodes[node_index].state == NodeState::Blocked
                    && dependency_state(&plan.nodes[node_index], &plan, &checkpoint, &positions)
                        == Some(NodeState::Blocked)
                    && attribute_blocker(
                        node_index,
                        &plan.nodes[node_index],
                        &mut checkpoint,
                        &positions,
                    )
                {
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                }
                continue;
            }

            let node = &plan.nodes[node_index];
            if loops::inactive_round(node, &checkpoint, &positions) {
                checkpoint.nodes[node_index].state = NodeState::Skipped;
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                continue;
            }
            if node.composite.is_some() {
                composites::settle::<R, J>(
                    node_index,
                    &plan,
                    &positions,
                    &mut checkpoint,
                    journal,
                )?;
                continue;
            }
            if let Some(spec) = &node.loop_body {
                let terminal_decision = checkpoint.extensions.iter().any(|e| {
                    e == &format!("loop.{}.decision=stop", node.id)
                        || e == &format!("loop.{}.decision=accept", node.id)
                });
                let body_settled = checkpoint
                    .nodes
                    .iter()
                    .filter(|n| n.round.as_ref().is_some_and(|r| r.loop_id == node.id))
                    .all(|n| n.state.settled());
                if (structural_pause.is_none() || terminal_decision || body_settled)
                    && let Some(extension) = loops::settle(
                        node_index,
                        spec,
                        &plan,
                        &positions,
                        &mut checkpoint,
                        owner,
                        journal,
                    )?
                {
                    if let Err(reason) =
                        loops::prepare_decision_extension(document, &checkpoint, &extension, limits)
                    {
                        for ask in &mut checkpoint.asks {
                            if ask.node.as_deref() == Some(node.id.as_str()) {
                                ask.resolved = true;
                                if let Some(answer) = ask.answer.as_mut() {
                                    answer
                                        .rejection_code
                                        .get_or_insert_with(|| "invalid_loop_decision".into());
                                    answer.rejected.get_or_insert_with(|| {
                                        bounded(&reason, MAX_ADVANCE_ERROR_BYTES)
                                    });
                                }
                            }
                        }
                        loops::pause_invalid_decision(&mut checkpoint, &node.id, &reason, journal)?;
                    } else {
                        return advance_inner(
                            document,
                            owner,
                            journal,
                            limits,
                            budget,
                            true,
                            &[extension],
                            observed_ci,
                        );
                    }
                }
                continue;
            }
            let dependency = dependency_state(node, &plan, &checkpoint, &positions);
            if let Some(state) = dependency {
                let mut changed = checkpoint.nodes[node_index].state != state;
                if state == NodeState::Blocked {
                    changed |= attribute_blocker(node_index, node, &mut checkpoint, &positions);
                } else if changed
                    && checkpoint.nodes[node_index].advance_error_code.as_deref()
                        == Some("upstream_blocked")
                {
                    checkpoint.nodes[node_index].advance_error = None;
                    checkpoint.nodes[node_index].advance_error_code = None;
                }
                if changed {
                    checkpoint.nodes[node_index].state = state;
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                }
                continue;
            }
            if let Some(spec) = &node.decide {
                asks::decide(
                    node_index,
                    node,
                    spec,
                    &plan,
                    &mut checkpoint,
                    owner,
                    journal,
                )?;
                continue;
            }
            if let Some(spec) = &node.land {
                let wall_now = owner.wall_now();
                let waiting = owner.respect_ci_wake()
                    && checkpoint.nodes[node_index]
                        .land
                        .as_ref()
                        .and_then(|record| record.wake_at)
                        .is_some_and(|wake| wall_now.is_none_or(|now| now < wake));
                if structural_pause.is_none() && checkpoint.pause_reason.is_none() && !waiting {
                    land::advance(
                        node_index,
                        node,
                        spec,
                        &plan,
                        &positions,
                        &mut checkpoint,
                        owner,
                        journal,
                    )?;
                }
                continue;
            }
            if let Some(condition) = &node.condition {
                if structural_pause.is_some() || checkpoint.pause_reason.is_some() {
                    continue;
                }
                let reason = if matches!(condition, GateCondition::Ci { .. }) {
                    if observed_ci.contains(&node.id) {
                        continue;
                    }
                    let wall_now = owner.wall_now();
                    if owner.respect_ci_wake()
                        && checkpoint.nodes[node_index]
                            .ci
                            .as_ref()
                            .and_then(|ci| ci.wake_at)
                            .is_some_and(|wake| wall_now.is_none_or(|now| now < wake))
                    {
                        continue;
                    }
                    let backoff = owner.ci_poll_backoff(&node.id);
                    observed_ci.insert(node.id.clone());
                    match ci::evaluate(node_index, node, &mut checkpoint, owner, wall_now, backoff)
                        .map_err(GraphError::Refused)?
                    {
                        ci::Evaluation::Pending => {
                            asks::refresh_wake(&mut checkpoint);
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                            continue;
                        }
                        ci::Evaluation::Settled(reason) => reason,
                    }
                } else {
                    gate_failure(condition, node, &checkpoint, &positions, &effective_budget)
                };
                let progress = &mut checkpoint.nodes[node_index];
                progress.state = if reason.is_none() {
                    NodeState::Succeeded
                } else {
                    NodeState::Failed
                };
                progress.gate = Some(GateResult {
                    extra: Default::default(),
                    passed: reason.is_none(),
                    reason: reason.map(|reason| bounded(&reason, OUTPUT_FIELD_BYTES)),
                    on_fail: node.on_fail.clone(),
                });
                progress.outputs = Some(ci::outputs(
                    progress.state == NodeState::Succeeded,
                    progress.ci.as_ref(),
                ));
                if let Some(ci) = progress.ci.as_mut() {
                    ci.wake_at = None;
                }
                asks::refresh_wake(&mut checkpoint);
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                continue;
            }
            if !checkpoint.nodes[node_index].extra.contains_key("retry")
                && let Some((retries, origin)) = owner
                    .retry_allowance(&node.extra, &plan.extra)
                    .map_err(GraphError::Run)?
            {
                checkpoint.nodes[node_index].extra.insert(
                    "retry".into(),
                    serde_json::json!({"retries":retries,"origin":origin}),
                );
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
            }
            if node.review.is_some() {
                if structural_pause.is_some() || checkpoint.pause_reason.is_some() {
                    continue;
                }
                if checkpoint.nodes[node_index].source_commit.is_none() {
                    let commit = match node.source.as_ref() {
                        Some(SourceBinding::Commit { commit }) => Some(commit.clone()),
                        Some(SourceBinding::Node { node: source }) => positions
                            .get(source)
                            .and_then(|index| checkpoint.nodes[*index].outputs.as_ref())
                            .and_then(|outputs| outputs.commit.clone()),
                        Some(SourceBinding::Head { node: writer }) => {
                            land::chain_head(&plan, &checkpoint, &positions, &node.id, writer)
                        }
                        Some(SourceBinding::Integration) => {
                            owner.integration_head().map_err(GraphError::Run)?
                        }
                        None => None,
                    };
                    if commit
                        .as_ref()
                        .is_none_or(|value| value.parse::<provenant_core::GitOid>().is_err())
                    {
                        let progress = &mut checkpoint.nodes[node_index];
                        progress.state = NodeState::Failed;
                        progress.advance_error_code = Some("source_commit_unavailable".into());
                        progress.advance_error = Some(format!(
                            "node {}: source commit unavailable or invalid",
                            node.id
                        ));
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        continue;
                    }
                    if matches!(node.source, Some(SourceBinding::Integration))
                        && checkpoint.integration_pin.is_none()
                    {
                        checkpoint.integration_pin = commit.clone();
                    }
                    checkpoint.nodes[node_index].source_commit = commit;
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                }
                let subject = checkpoint.nodes[node_index]
                    .source_commit
                    .as_deref()
                    .ok_or_else(|| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "deferred review node must have a source commit",
                    })?;
                let original = expanded
                    .nodes
                    .iter()
                    .find(|candidate| candidate.id == node.id)
                    .ok_or_else(|| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "deferred review node must exist in the expanded plan",
                    })?;
                let mut resolution_error_code = "review_resolution_failed";
                let result = owner
                    .resolve_review(original, subject)
                    .map_err(|error| {
                        if matches!(error, ReviewResolveError::Unsupported) {
                            resolution_error_code = "review_resolution_unsupported";
                        }
                        error.to_string()
                    })
                    .and_then(|mut addition| {
                        if let Some(review) = &mut addition.review {
                            if !review.substitutions.is_empty() {
                                return Err("new review resolution cannot contain substitutions".into());
                            }
                            let source = bounded(&review.source, MAX_ADVANCE_ERROR_BYTES - 32);
                            if source.len() < review.source.len() {
                                review.source = format!("{source} [truncated]");
                            }
                        }
                        for child in &mut addition.nodes {
                            if child.composite.is_none() {
                                child.source = Some(SourceBinding::Commit {
                                    commit: subject.to_owned(),
                                });
                            }
                        }
                        if checkpoint
                            .expansions
                            .iter()
                            .filter_map(|item| item.review.as_ref())
                            .any(|review| {
                                addition
                                    .review
                                    .as_ref()
                                    .is_some_and(|next| review.base != next.base)
                            })
                        {
                            return Err("review expansions disagree on the base".into());
                        }
                        plans::validate_review_addition(&expanded, &addition)?;
                        if addition.source != node.id
                            || addition
                                .review
                                .as_ref()
                                .is_none_or(|review| review.subject.to_string() != subject)
                        {
                            return Err("review expansion subject or source mismatch".into());
                        }
                        // Flat validation otherwise reports only `node id`.
                        let mut ids: std::collections::BTreeSet<&str> =
                            expanded.nodes.iter().map(|item| item.id.as_str()).collect();
                        for child in &addition.nodes {
                            if !valid_identifier(&child.id) {
                                return Err(format!(
                                    "expanded review node id {:?} is invalid; use at most 64 ASCII letters, digits, dots, underscores or hyphens; shorten the composite or lens name",
                                    bounded(&child.id, 256)
                                ));
                            }
                            if !ids.insert(child.id.as_str()) {
                                return Err(format!(
                                    "expanded review node id {:?} is duplicated; use distinct composite and lens names",
                                    bounded(&child.id, 256)
                                ));
                            }
                        }
                        let mut preview = expanded.clone();
                        plans::append(&mut preview, &addition);
                        let (next_plan, next_positions) =
                            validate_flat_plan(&preview, &plans::generous())
                                .map_err(str::to_owned)?;
                        if let Some(reason) = structural_limit_reason(&preview, &next_plan, limits)
                        {
                            return Err(reason);
                        }
                        let mut additions = checkpoint.expansions.clone();
                        additions.push(addition.clone());
                        if let Some(reason) = graph_envelope_reason(
                            document,
                            &additions,
                            &checkpoint.extensions,
                            &next_plan,
                        ) {
                            return Err(reason);
                        }
                        let before = graph_envelope_bound(document, &checkpoint.expansions, &checkpoint.extensions, &plan);
                        let after = graph_envelope_bound(document, &additions, &checkpoint.extensions, &next_plan);
                        if after > before {
                            return Err(format!(
                                "review {} would project {after} checkpoint bytes, exceeding its reserved allowance of {before} bytes; split the graph into smaller batches or use a smaller review",
                                node.id
                            ));
                        }
                        Ok((addition, preview, next_plan, next_positions))
                    });
                match result {
                    Ok((addition, preview, next_plan, next_positions)) => {
                        let mut prospective = checkpoint.clone();
                        prospective.nodes[node_index].state = NodeState::Succeeded;
                        prospective.expansions.push(addition);
                        loops::reconcile(&mut prospective, &next_plan);
                        for (progress, planned) in
                            prospective.nodes.iter_mut().zip(&next_plan.nodes)
                        {
                            progress.children.clone_from(&planned.composite_members);
                        }
                        plans::remaining(&mut prospective, &preview, limits);
                        validate_expanded_rows(&prospective, &next_plan)?;
                        owner
                            .validate_expansion(&preview, &prospective)
                            .map_err(GraphError::Run)?;
                        journal.save(&prospective).map_err(GraphError::Journal)?;
                        checkpoint = prospective;
                        (expanded, plan, positions) = (preview, next_plan, next_positions);
                        continue 'schedule;
                    }
                    Err(reason) => {
                        let progress = &mut checkpoint.nodes[node_index];
                        progress.state = NodeState::Failed;
                        progress.advance_error_code = Some(resolution_error_code.into());
                        progress.advance_error = Some(bounded(
                            &format!("node {}: {reason}", node.id),
                            MAX_ADVANCE_ERROR_BYTES,
                        ));
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        continue;
                    }
                }
            }
            loop {
                // A persisted launch wait defers this node until its own wake
                // on every advance, tick or manual, so a failing host that
                // cannot launch is not hammered with new submissions.
                let launch_clock = owner.wall_now();
                if checkpoint.nodes[node_index]
                    .launch_retry
                    .as_ref()
                    .and_then(|retry| retry.wake_at)
                    .is_some_and(|wake| launch_clock.is_some_and(|now| now < wake))
                {
                    break;
                }
                if let Some(retry) = &checkpoint.nodes[node_index].launch_retry
                    && retry
                        .deadline
                        .zip(launch_clock)
                        .is_some_and(|(end, now)| now >= end)
                {
                    let accepted = match checkpoint.nodes[node_index].attempts.last() {
                        Some(attempt) if attempt.attempt_ref.is_some() => true,
                        Some(attempt) => owner.launch_accepted(attempt).map_err(GraphError::Run)?,
                        None => false,
                    };
                    if !accepted {
                        let reason = retry.reason.clone().unwrap_or_default();
                        let progress = &mut checkpoint.nodes[node_index];
                        progress.advance_error_code = Some("launch_retry_exhausted".into());
                        progress.advance_error = Some(format!(
                            "node {}: the launch failed: {}; {}",
                            progress.id,
                            reason,
                            launch_diagnostic(&crate::coordination::facade::FacadeError::Busy)
                                .next
                                .as_deref()
                                .unwrap_or("retry the graph advance")
                        ));
                        progress.state = NodeState::Blocked;
                        progress.launch_retry = None;
                        asks::refresh_wake(&mut checkpoint);
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                }
                let progress = &checkpoint.nodes[node_index];
                let route_index = match progress.attempts.last() {
                    None => 0,
                    Some(last) if last.state != AttemptState::Terminal => {
                        usize::from(last.key.route_index)
                    }
                    Some(last)
                        if review_substitution(node, last)
                            && progress
                                .attempts
                                .iter()
                                .filter(|attempt| attempt.review_substitution)
                                .count()
                                < limits.review_substitutions =>
                    {
                        progress.attempts.len()
                    }
                    Some(last) if review_substitution(node, last) => {
                        // A lowered limit can withdraw a retry that was saved
                        // but had not yet started when the graph stopped.
                        checkpoint.nodes[node_index].state = NodeState::Failed;
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    Some(last)
                        if last.outcome.is_some_and(|outcome| {
                            fallback_candidate(outcome, node.access, last.launched)
                        }) =>
                    {
                        let Some(outcome) = last.outcome else { break };
                        let (evidence, unreadable) = match last.attempt_ref.as_deref() {
                            Some(reference) if !last.launched => {
                                match owner.preparation_fallback_evidence(&last.key, reference) {
                                    Ok(evidence) => (evidence, false),
                                    Err(_) => (None, true),
                                }
                            }
                            _ => (None, false),
                        };
                        if !live_fallback_allowed(outcome, last.launched, node.access, evidence) {
                            let (code, message) =
                                fallback_denial(last.launched, node.access, unreadable);
                            let progress = &mut checkpoint.nodes[node_index];
                            progress.state = NodeState::Failed;
                            progress.advance_error_code = Some(code.into());
                            progress.advance_error = Some(message.into());
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                            break;
                        }
                        if review_preparation_substitution(node, last)
                            && progress
                                .attempts
                                .iter()
                                .filter(|attempt| attempt.review_substitution)
                                .count()
                                >= limits.review_substitutions
                        {
                            let progress = &mut checkpoint.nodes[node_index];
                            progress.state = NodeState::Failed;
                            progress.advance_error_code = Some("review_substitution_limit".into());
                            progress.advance_error =
                                Some("review substitution limit reached".into());
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                            break;
                        }
                        progress.attempts.len()
                    }
                    Some(_) => break,
                };
                if route_index > 0
                    && progress.attempts.last().is_some_and(|attempt| {
                        attempt.launched && attempt.state == AttemptState::Terminal
                    })
                    && node
                        .routes
                        .get(route_index - 1)
                        .and_then(|route| route.account.as_ref())
                        .is_some_and(|account| {
                            node.routes
                                .get(route_index)
                                .is_some_and(|next| next.account.as_ref() != Some(account))
                        })
                {
                    let progress = &mut checkpoint.nodes[node_index];
                    progress.state = NodeState::Failed;
                    progress.advance_error_code = Some("fallback_account_pinned".into());
                    progress.advance_error =
                        Some("Route fallback cannot change the pinned account.".into());
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    break;
                }
                let allowance = progress
                    .extra
                    .get("retry")
                    .and_then(|value| value["retries"].as_u64());
                // Preparation refusals have not launched a provider and do not
                // consume the settlement retry allowance.
                let mut spent = 0u64;
                for reference in progress
                    .attempts
                    .iter()
                    .filter_map(|attempt| attempt.attempt_ref.as_deref())
                {
                    spent = spent.saturating_add(
                        owner.retry_successors(reference).map_err(GraphError::Run)?,
                    );
                }
                // Each launched attempt that needs another route consumes one
                // infrastructure retry, in addition to successors within a run.
                let launched_switches = progress
                    .attempts
                    .iter()
                    .filter(|attempt| attempt.launched && attempt.state == AttemptState::Terminal)
                    .count() as u64;
                spent = spent.saturating_add(launched_switches);
                if route_index > 0
                    && progress.attempts.last().is_some_and(|attempt| {
                        attempt.launched && attempt.state == AttemptState::Terminal
                    })
                    && allowance.is_some_and(|limit| spent > limit)
                {
                    let progress = &mut checkpoint.nodes[node_index];
                    progress.state = NodeState::Failed;
                    progress.advance_error_code = Some("fallback_budget_exhausted".into());
                    progress.advance_error = Some("Route fallback retry budget exhausted.".into());
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    break;
                }
                let settlement_retries = allowance.map(|limit| limit.saturating_sub(spent) as u8);
                let route = node
                    .routes
                    .get(route_index)
                    .ok_or_else(|| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "attempt route index must name a configured route",
                    })?
                    .clone();
                let key = AttemptKey {
                    extra: Default::default(),
                    graph_id: plan.graph_id.clone(),
                    node_id: node.id.clone(),
                    route_index: u16::try_from(route_index)
                        .map_err(|_| GraphError::Invalid(String::from("route count")))?,
                };

                let needs_record = checkpoint.nodes[node_index]
                    .attempts
                    .last()
                    .is_none_or(|last| last.state == AttemptState::Terminal);
                if needs_record {
                    let cap = limits.node_prompt_bytes;
                    if node.access == AccessMode::WorktreeWrite
                        && checkpoint.nodes.iter().enumerate().any(|(index, other)| {
                            index != node_index
                                && other.state == NodeState::Running
                                && plan.nodes.iter().any(|peer| {
                                    peer.id == other.id && plans::writers_conflict(node, peer)
                                })
                        })
                    {
                        break;
                    }
                    if limits.concurrency.is_some_and(|cap| {
                        checkpoint
                            .nodes
                            .iter()
                            .enumerate()
                            .filter(|(index, node)| {
                                *index != node_index
                                    && node.state == NodeState::Running
                                    && node.ci.is_none()
                                    && plan.nodes[*index].composite.is_none()
                                    && plan.nodes[*index].decide.is_none()
                                    && plan.nodes[*index].land.is_none()
                            })
                            .count()
                            >= cap
                    }) {
                        break;
                    }
                    update_usage_warnings(&mut checkpoint);
                    if let Some(reason) = checkpoint
                        .pause_reason
                        .clone()
                        .or_else(|| budget_reason(&checkpoint, &effective_budget))
                    {
                        checkpoint.pause_reason = Some(bounded(&reason, PAUSE_REASON_BYTES));
                        checkpoint
                            .pause_code
                            .get_or_insert_with(|| "budget_exceeded".into());
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    if !node.inputs.is_empty()
                        && render_input(node, &plan, &checkpoint, &positions, cap).1
                    {
                        let (code, reason) = if node
                            .prompt
                            .len()
                            .saturating_add(INPUT_OPEN.len())
                            .saturating_add(INPUT_CLOSE.len())
                            > cap
                        {
                            (
                                "input_prompt_too_large",
                                "node prompt leaves no room for upstream data",
                            )
                        } else {
                            (
                                "upstream_outputs_too_large",
                                "selected upstream outputs do not fit",
                            )
                        };
                        let progress = &mut checkpoint.nodes[node_index];
                        progress.advance_error_code = Some(code.into());
                        progress.advance_error = Some(format!(
                            "node {}: {reason}; increase node_prompt_bytes and restart the graph",
                            node.id
                        ));
                        progress.state = NodeState::Failed;
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    if node.source.is_some() && checkpoint.nodes[node_index].source_commit.is_none()
                    {
                        let commit = match node.source.as_ref() {
                            Some(SourceBinding::Commit { commit }) => Some(commit.clone()),
                            Some(SourceBinding::Node { node: source }) => positions
                                .get(source)
                                .and_then(|index| checkpoint.nodes[*index].outputs.as_ref())
                                .and_then(|outputs| outputs.commit.clone()),
                            Some(SourceBinding::Head { node: writer }) => {
                                land::chain_head(&plan, &checkpoint, &positions, &node.id, writer)
                            }
                            Some(SourceBinding::Integration) => {
                                owner.integration_head().map_err(GraphError::Run)?
                            }
                            None => None,
                        };
                        let progress = &mut checkpoint.nodes[node_index];
                        if commit
                            .as_ref()
                            .is_none_or(|commit| commit.parse::<provenant_core::GitOid>().is_err())
                        {
                            progress.state = NodeState::Failed;
                            progress.advance_error_code = Some("source_commit_unavailable".into());
                            progress.advance_error = Some(format!(
                                "node {}: source commit unavailable or invalid",
                                node.id
                            ));
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                            break;
                        }
                        if matches!(node.source, Some(SourceBinding::Integration))
                            && checkpoint.integration_pin.is_none()
                        {
                            checkpoint.integration_pin = commit.clone();
                        }
                        checkpoint.nodes[node_index].source_commit = commit;
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    }
                    if (node.kind == Some(NodeKind::Synthesise) || plans::writer(node))
                        && let Some(pin) = checkpoint.integration_pin.clone()
                        && checkpoint.nodes[node_index].source_commit.as_ref() == Some(&pin)
                        && checkpoint.nodes[node_index].attempts.is_empty()
                        && let Some(head) = owner.integration_head().map_err(GraphError::Run)?
                        && head != pin
                    {
                        let progress = &mut checkpoint.nodes[node_index];
                        progress.state = NodeState::Failed;
                        progress.advance_error_code = Some("integration_head_moved".into());
                        progress.advance_error = Some(format!(
                            "node {}: the integration branch moved from the pinned {pin} to {head} after the review subject was pinned; rerun the graph to review the new head",
                            node.id
                        ));
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    // A loop round integrates by fast-forward from the head the
                    // previous writer in its worktree delivered, so a target
                    // that moved since then stops the loop before the round.
                    if node.round.is_some()
                        && plans::writer(node)
                        && matches!(
                            node.delivery,
                            Some(provenant_records::run::DeliveryMode::Integrate(_))
                        )
                        && checkpoint.nodes[node_index].attempts.is_empty()
                        && let Some(delivered) = plan.nodes[..node_index]
                            .iter()
                            .enumerate()
                            .rev()
                            .filter(|(_, earlier)| {
                                plans::writer(earlier)
                                    && earlier.worktree == node.worktree
                                    && matches!(
                                        earlier.delivery,
                                        Some(provenant_records::run::DeliveryMode::Integrate(_))
                                    )
                            })
                            .find_map(|(index, _)| {
                                checkpoint.nodes[index]
                                    .outputs
                                    .as_ref()
                                    .and_then(|outputs| outputs.commit.clone())
                            })
                        && let Some(head) = owner.integration_head().map_err(GraphError::Run)?
                        && head != delivered
                    {
                        let progress = &mut checkpoint.nodes[node_index];
                        progress.state = NodeState::Failed;
                        progress.advance_error_code = Some("integration_head_moved".into());
                        progress.advance_error = Some(format!(
                            "node {}: the integration branch moved from the delivered {delivered} to {head}; a loop round only fast-forwards it, so rerun the graph",
                            node.id
                        ));
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    if node.continues.is_some()
                        && land::continued_branch(node, &checkpoint, &positions).is_none()
                    {
                        let progress = &mut checkpoint.nodes[node_index];
                        progress.state = NodeState::Failed;
                        progress.advance_error_code = Some("continuation_delivery_missing".into());
                        progress.advance_error = Some(format!(
                            "node {}: the writer it continues recorded no delivery branch",
                            node.id
                        ));
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    let review_substitution = checkpoint.nodes[node_index]
                        .attempts
                        .last()
                        .is_some_and(|attempt| {
                            review_substitution(node, attempt)
                                || review_preparation_substitution(node, attempt)
                        });
                    if review_substitution {
                        for expansion in &mut checkpoint.expansions {
                            if review_panel_contains(expansion, node, &plan, &positions)
                                && let Some(review) = &mut expansion.review
                            {
                                review.substitutions.push(ReviewSubstitution {
                                    member: node.id.clone(),
                                    route_index: key.route_index,
                                });
                                review.substitutions.sort();
                            }
                        }
                    }
                    checkpoint.nodes[node_index].attempts.push(GraphAttempt {
                        extra: Default::default(),
                        review_substitution,
                        key: key.clone(),
                        route: route.clone(),
                        state: AttemptState::Starting,
                        attempt_ref: None,
                        outcome: None,
                        launched: true,
                        launch_actor: owner.actor(),
                        settled: 0,
                        input_cap: (!node.inputs.is_empty()).then_some(cap),
                        usage: None,
                    });
                    checkpoint.nodes[node_index].state = NodeState::Running;
                    if let Some(seconds) = owner.admission_timeout_seconds() {
                        let deadline = owner.wall_now().and_then(|now| {
                            DurationMicros::from_seconds(seconds)
                                .ok()
                                .and_then(|duration| now.checked_add(duration).ok())
                        });
                        checkpoint.nodes[node_index].launch_retry = Some(LaunchRetry {
                            extra: Default::default(),
                            failures: 0,
                            deadline,
                            wake_at: None,
                            reason: deadline.is_none().then(|| {
                                "wall clock unavailable; launch retry timing is unavailable".into()
                            }),
                        });
                    }
                    // A retry must not replay the preceding attempt's family snapshot.
                    checkpoint.nodes[node_index].separation = None;
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                }

                let stored_ref = checkpoint.nodes[node_index]
                    .attempts
                    .last()
                    .and_then(|last| last.attempt_ref.clone());
                let launch_actor = checkpoint.nodes[node_index]
                    .attempts
                    .last()
                    .map_or_else(|| owner.actor(), |last| last.launch_actor.clone());
                let input_cap = checkpoint.nodes[node_index]
                    .attempts
                    .last()
                    .and_then(|attempt| attempt.input_cap)
                    .unwrap_or(limits.node_prompt_bytes);
                let mut prompt = render_input(node, &plan, &checkpoint, &positions, input_cap).0;
                // An accepted attempt is only observed, so its files exist
                // already and its prompt is not sent again.
                let requests = if stored_ref.is_some() {
                    Vec::new()
                } else {
                    upstream_results(node, &plan, &checkpoint, &positions)
                };
                let materialised = if requests.is_empty() {
                    None
                } else {
                    owner.materialise_inputs(&key, &requests, NODE_INPUT_FILE_BYTES)
                };
                if let Some(materialised) = &materialised {
                    prompt.push_str(&render_files(
                        &materialised.entries,
                        &requests,
                        materialised.index.as_deref(),
                    ));
                }
                // The writer is the nearest writing node along the source chain,
                // so a framing node between a lens and the writer is skipped.
                let mut distinct_attempts: Vec<String> = Vec::new();
                if node.distinct_from == Some(DistinctFrom::Dependencies) {
                    // Composite members are dependencies through after_expansion.
                    let mut pending: Vec<&String> = node.dependencies().collect();
                    let mut seen = std::collections::BTreeSet::new();
                    while let Some(id) = pending.pop() {
                        let Some(index) = positions.get(id) else {
                            continue;
                        };
                        if !seen.insert(*index) {
                            continue;
                        }
                        distinct_attempts.extend(
                            checkpoint.nodes[*index]
                                .attempts
                                .iter()
                                .filter_map(|attempt| attempt.attempt_ref.clone()),
                        );
                        pending.extend(plan.nodes[*index].dependencies());
                    }
                    distinct_attempts.sort();
                    distinct_attempts.dedup();
                } else if node.distinct_from == Some(DistinctFrom::Writer) {
                    let mut current = node;
                    // A resolved review lens is pinned to a commit, so its writer
                    // is found through the deferred node it was expanded from.
                    if matches!(node.source, Some(SourceBinding::Commit { .. }))
                        && let Some(origin) = checkpoint
                            .expansions
                            .iter()
                            .find(|expansion| {
                                expansion.review.is_some()
                                    && expansion.nodes.iter().any(|n| n.id == node.id)
                            })
                            .and_then(|expansion| positions.get(&expansion.source))
                    {
                        current = &plan.nodes[*origin];
                    }
                    while let Some(
                        binding @ (SourceBinding::Node { node: source }
                        | SourceBinding::Head { node: source }),
                    ) = &current.source
                    {
                        let Some(index) = positions.get(source) else {
                            break;
                        };
                        let holder = &current.id;
                        current = &plan.nodes[*index];
                        if plans::writer(current) {
                            // A head binding pins the newest commit of the
                            // chain, so every repair that continues the writer
                            // authored the subject too.
                            distinct_attempts = if matches!(binding, SourceBinding::Head { .. }) {
                                land::chain_attempts(&plan, &checkpoint, &positions, holder, source)
                            } else {
                                checkpoint.nodes[*index]
                                    .attempts
                                    .iter()
                                    .filter_map(|attempt| attempt.attempt_ref.clone())
                                    .collect()
                            };
                            break;
                        }
                    }
                }
                let parent = plan
                    .nodes
                    .iter()
                    .find(|candidate| candidate.composite_members.contains(&node.id))
                    .map(|candidate| &candidate.id);
                let peer_refs: Vec<String> =
                    node.family_group.as_ref().map_or_else(Vec::new, |group| {
                        plan.nodes
                            .iter()
                            .enumerate()
                            .filter(|(index, peer)| {
                                let peer_parent = plan
                                    .nodes
                                    .iter()
                                    .find(|candidate| {
                                        candidate.composite_members.contains(&peer.id)
                                    })
                                    .map(|candidate| &candidate.id);
                                *index != node_index
                                    && peer.family_group.as_ref() == Some(group)
                                    && peer_parent == parent
                            })
                            .flat_map(|(index, _)| checkpoint.nodes[index].attempts.iter())
                            .filter_map(|attempt| attempt.attempt_ref.clone())
                            .collect()
                    });
                let review_retry = checkpoint.nodes[node_index]
                    .attempts
                    .last()
                    .is_some_and(|attempt| attempt.review_substitution);
                let mut family_peers = if (node.family_group.is_some() || review_retry)
                    && !needs_record
                    && checkpoint.nodes[node_index].separation.is_some()
                {
                    checkpoint.nodes[node_index]
                        .separation
                        .as_ref()
                        .map(|value| {
                            if value.strength == FamilyStrength::Require {
                                value.preferred.clone()
                            } else {
                                value.excluded.clone()
                            }
                        })
                        .unwrap_or_default()
                } else {
                    owner.attempt_families(&peer_refs).unwrap_or_default()
                };
                if review_retry && needs_record {
                    let previous: Vec<_> = checkpoint.nodes[node_index]
                        .attempts
                        .iter()
                        .take(route_index)
                        .filter_map(|attempt| attempt.attempt_ref.clone())
                        .collect();
                    for family in owner.attempt_families(&previous).unwrap_or_default() {
                        if !family_peers.contains(&family) {
                            family_peers.push(family);
                        }
                    }
                }
                let mut prior_attempts = Vec::new();
                for attempt in checkpoint.nodes[node_index]
                    .attempts
                    .iter()
                    .take(route_index)
                    .filter(|_| !review_retry)
                {
                    let resolved = if let Some(reference) = &attempt.attempt_ref {
                        owner
                            .attempt_models(std::slice::from_ref(reference))
                            .map_err(GraphError::Run)?
                            .into_iter()
                            .next()
                    } else {
                        None
                    };
                    if let Some(model) = resolved.or_else(|| attempt.route.model.clone()) {
                        prior_attempts.push(model);
                    }
                }
                let require_distinct = node.distinct_strength == Some(FamilyStrength::Require)
                    && (node.family_group.is_none() || node.distinct_from.is_some());
                if (needs_record || checkpoint.nodes[node_index].separation.is_none())
                    && (node.distinct_from.is_some() || node.family_group.is_some() || review_retry)
                {
                    let mut excluded = owner
                        .attempt_families(&distinct_attempts)
                        .unwrap_or_default();
                    let enforced = require_distinct && !excluded.is_empty();
                    let mut record = if enforced {
                        let mut record = separation(FamilyStrength::Require, excluded, None);
                        record.preferred = family_peers
                            .iter()
                            .take(SEPARATION_FAMILIES)
                            .map(|family| bounded(family, FAMILY_BYTES))
                            .collect();
                        record
                    } else {
                        for family in &family_peers {
                            if !excluded.contains(family) {
                                excluded.push(family.clone());
                            }
                        }
                        let strength = if node.family_group.is_some() {
                            FamilyStrength::Prefer
                        } else {
                            node.distinct_strength.unwrap_or_default()
                        };
                        separation(strength, excluded, None)
                    };
                    record.applied = None;
                    checkpoint.nodes[node_index].separation = Some(record);
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                }
                let continued = land::continued_branch(node, &checkpoint, &positions);
                let observation = match owner.observe_or_start(
                    &key,
                    NodeInput {
                        require_skill_location: node.require_skill_location,
                        settlement_retries,
                        prior_attempts: &prior_attempts,
                        family_peers: &family_peers,
                        context: node_context(&plan, node),
                        read_roots: &node.read_roots,
                        capabilities: node.capabilities.as_deref(),
                        capability_sources: node.extra.get("capability_sources"),
                        distinct_from_attempts: &distinct_attempts,
                        require_distinct,
                        source_commit: checkpoint.nodes[node_index].source_commit.as_deref(),
                        review_base: node
                            .extra
                            .get("review_base")
                            .and_then(serde_json::Value::as_str),
                        title: node.title.as_deref().unwrap_or(&node.id),
                        prompt: &prompt,
                        required_verdict: node.required_verdict.as_deref(),
                        deadline_seconds: node.deadline_seconds,
                        inputs_dir: materialised
                            .as_ref()
                            .and_then(|inputs| inputs.directory.as_deref()),
                    },
                    &route,
                    node.access,
                    node.worktree.as_deref(),
                    Replay {
                        delivery: land::delivery(node),
                        branch: continued.as_deref(),
                        fast_forward_only: node.round.is_some(),
                        stored_ref: stored_ref.as_deref(),
                        launch_actor: &launch_actor,
                    },
                ) {
                    Ok(observation) => observation,
                    // A launch that fails while its attempt is still starting
                    // leaves the node stopped for good unless a disposition is
                    // persisted here: a bounded retry wake or a blocked node.
                    Err(error)
                        if checkpoint.nodes[node_index]
                            .attempts
                            .last()
                            .is_some_and(|last| {
                                last.state == AttemptState::Starting && last.attempt_ref.is_none()
                            }) =>
                    {
                        launch_disposition::<R, J>(
                            &mut checkpoint,
                            node_index,
                            &error,
                            owner.wall_now(),
                            owner.admission_timeout_seconds().unwrap_or(3600),
                            owner.launch_retry_policy().map_err(GraphError::Run)?,
                            journal,
                        )?;
                        break;
                    }
                    Err(error) => return Err(GraphError::Run(error)),
                };
                let (fallback_evidence, evidence_unreadable) = match &observation {
                    AttemptObservation::Terminal {
                        attempt_ref: Some(reference),
                        launched: false,
                        ..
                    } => match owner.preparation_fallback_evidence(&key, reference) {
                        Ok(evidence) => (evidence, false),
                        Err(_) => (None, true),
                    },
                    _ => (None, false),
                };
                let mut usage_error = None;
                let mut cost_error = None;
                let usage = if let AttemptObservation::Terminal {
                    attempt_ref: Some(reference),
                    ..
                } = &observation
                {
                    Some(match owner.usage(reference) {
                        Ok(usage) => usage,
                        Err(error) => {
                            usage_error = Some(bounded(&error.to_string(), NOTE_BYTES));
                            cost_error = owner
                                .cost_unknown_reason()
                                .map(|reason| bounded(&reason, NOTE_BYTES));
                            AttemptUsage::default()
                        }
                    })
                    .map(|mut usage| {
                        usage.cost_unknown_reason = usage
                            .cost_unknown_reason
                            .map(|reason| bounded(&reason, NOTE_BYTES));
                        usage
                    })
                } else {
                    if matches!(
                        &observation,
                        AttemptObservation::Terminal { launched: true, .. }
                    ) {
                        cost_error = owner
                            .cost_unknown_reason()
                            .map(|reason| bounded(&reason, NOTE_BYTES));
                    }
                    None
                };
                let recorded_separation =
                    match (&observation, node.distinct_from, &node.family_group) {
                        (AttemptObservation::Terminal { attempt_ref, .. }, distinct, group)
                            if distinct.is_some() || group.is_some() || review_retry =>
                        {
                            let mut excluded = owner
                                .attempt_families(&distinct_attempts)
                                .unwrap_or_default();
                            let enforced = require_distinct && !excluded.is_empty();
                            if !enforced {
                                for family in &family_peers {
                                    if !excluded.contains(family) {
                                        excluded.push(family.clone());
                                    }
                                }
                            }
                            let applied = attempt_ref.as_ref().and_then(|reference| {
                                owner
                                    .attempt_families(std::slice::from_ref(reference))
                                    .ok()
                                    .and_then(|families| families.into_iter().next())
                            });
                            let mut record = separation(
                                if enforced {
                                    FamilyStrength::Require
                                } else if node.family_group.is_some() {
                                    FamilyStrength::Prefer
                                } else {
                                    node.distinct_strength.unwrap_or_default()
                                },
                                excluded,
                                applied,
                            );
                            if enforced {
                                record.preferred = family_peers
                                    .iter()
                                    .take(SEPARATION_FAMILIES)
                                    .map(|family| bounded(family, FAMILY_BYTES))
                                    .collect();
                            }
                            Some(record)
                        }
                        _ => None,
                    };
                // Every selected upstream finding identity needs a disposition.
                let upstream_findings: Vec<Finding> = if node.dispositions.is_empty() {
                    Default::default()
                } else {
                    selected_findings(node, &plan, &checkpoint, &positions)
                        .into_iter()
                        .map(|(source, mut finding)| {
                            finding.extra.insert(
                                "propagates".into(),
                                format!("{source}:{}", finding.id).into(),
                            );
                            finding
                        })
                        .collect()
                };
                let proof_dispositions = if node.dispositions.is_empty()
                    && node.role.as_deref() == Some("review.verify")
                {
                    vec![
                        "upheld".to_owned(),
                        "falsified".to_owned(),
                        "unverifiable".to_owned(),
                    ]
                } else {
                    node.dispositions.clone()
                };
                let overflow_ids = if proof_dispositions.is_empty() {
                    Vec::new()
                } else {
                    truncated_finding_ids(node, &plan, &checkpoint, &positions)
                };
                let marker_inputs = propagation_inputs(node, &plan, &checkpoint, &positions);
                let mut settled_result = None;
                let retry_was_pending;
                let should_continue = {
                    let progress = &mut checkpoint.nodes[node_index];
                    let attempt =
                        progress
                            .attempts
                            .last_mut()
                            .ok_or_else(|| GraphError::BadCheckpoint {
                                node: node.id.clone(),
                                invariant: "an observed attempt must have a checkpoint row",
                            })?;
                    // Any observation answers the stopped launch, so its wait
                    // is over and the checkpoint wake no longer owes it.
                    retry_was_pending = progress.launch_retry.take().is_some();
                    match observation {
                        AttemptObservation::Pending { attempt_ref } => {
                            attempt.state = AttemptState::Running;
                            attempt.attempt_ref = attempt_ref;
                            progress.state = NodeState::Running;
                            false
                        }
                        AttemptObservation::Unknown { attempt_ref } => {
                            attempt.state = AttemptState::Unresolved;
                            attempt.attempt_ref = attempt_ref;
                            progress.state = NodeState::Running;
                            false
                        }
                        AttemptObservation::Terminal {
                            attempt_ref,
                            outcome,
                            launched,
                            result,
                            result_warning,
                            result_path,
                            branch,
                            commit,
                        } => {
                            settled_result = result.clone();
                            attempt.usage = usage;
                            attempt.launched = launched;
                            attempt.state = AttemptState::Terminal;
                            attempt.attempt_ref = attempt_ref;
                            // Provider success alone does not satisfy an explicit result contract.
                            let warned = result_warning.is_some();
                            let mut outputs = make_outputs_with_upstream(
                                result.as_deref(),
                                result_warning,
                                result_path,
                                branch,
                                commit,
                                &upstream_findings,
                            );
                            if node.access == AccessMode::ReadOnly
                                && let Some(lens) = node
                                    .role
                                    .as_deref()
                                    .and_then(|role| role.strip_prefix("lens."))
                            {
                                for finding in &mut outputs.findings {
                                    if !upstream_findings
                                        .iter()
                                        .any(|source| source.id == finding.id)
                                    {
                                        finding.extra.insert("lens".into(), lens.into());
                                    }
                                }
                            }
                            let marker_warnings = propagation_warnings(&outputs, &marker_inputs);
                            if !marker_warnings.is_empty() {
                                outputs.warnings.truncate(MAX_WARNINGS.saturating_sub(2));
                                outputs.warnings.extend(marker_warnings);
                            }
                            if node.access == AccessMode::ReadOnly
                                && node.role.as_deref().is_some_and(|role| {
                                    role == "lens"
                                        || role.starts_with("lens.")
                                        || role.starts_with("review.")
                                })
                                && let Some(reference) = attempt.attempt_ref.as_deref()
                                && owner
                                    .equal_review_range(reference)
                                    .map_err(GraphError::Run)?
                            {
                                outputs
                                    .warnings
                                    .push("equal_base_and_head: review range is empty".into());
                            }
                            outputs.warnings.truncate(MAX_WARNINGS);
                            if outcome == AttemptOutcome::UsageLimited {
                                outputs
                                    .warnings
                                    .retain(|warning| !warning.starts_with(FINDINGS_INVALID));
                            }
                            let disposition_error = if node.require_findings
                                && (outputs.result.is_none()
                                    || outputs
                                        .warnings
                                        .iter()
                                        .any(|warning| warning.starts_with(FINDINGS_INVALID)))
                            {
                                Some((
                                    "findings_required",
                                    outputs
                                        .warnings
                                        .iter()
                                        .find(|warning| warning.starts_with(FINDINGS_INVALID))
                                        .cloned()
                                        .unwrap_or_else(|| {
                                            "the findings block is missing or invalid".to_owned()
                                        }),
                                ))
                            } else {
                                // Overflow proof covers all findings from truncated composites;
                                // their structured subset may differ across selected inputs.
                                let required_findings: Vec<_> = upstream_findings
                                    .iter()
                                    .filter(|finding| {
                                        !overflow_ids.iter().any(|(id, _)| id == &finding.id)
                                    })
                                    .cloned()
                                    .collect();
                                disposition_error(&node.dispositions, &required_findings, &outputs)
                                    .or_else(|| {
                                        (!overflow_ids.is_empty() && result.as_ref().is_none_or(|capture| !capture.disposition_proof.matches(&overflow_ids, &proof_dispositions))).then(|| (
                                            "disposition_overflow_incomplete",
                                            format!("all {} findings from truncated composites need exactly one DISPOSITION line with an allowed disposition and non-empty reason", overflow_ids.len()),
                                        ))
                                    })
                                    .or_else(|| {
                                        let proof = &result.as_ref()?.disposition_proof;
                                        let verifier = node.dispositions.is_empty() && node.role.as_deref() == Some("review.verify");
                                        let mismatch = outputs.findings.iter().any(|finding| {
                                            if !overflow_ids.iter().any(|(id, _)| id == &finding.id) { return false; }
                                            let status = if verifier { finding.extra.get("verification").and_then(serde_json::Value::as_str) } else { finding.disposition.as_deref() };
                                            status.is_none_or(|status| !proof.agrees(&finding.id, status, &overflow_ids))
                                        });
                                        mismatch.then(|| ("disposition_overflow_inconsistent", "structured findings disagree with their DISPOSITION lines".to_owned()))
                                    })
                            };
                            let review_result = node.access == AccessMode::ReadOnly
                                && node.role.as_deref().is_some_and(|role| {
                                    role.starts_with("lens.") || role.starts_with("review.")
                                })
                                && outputs.verdict.is_some()
                                && outputs.verdict_error.is_none()
                                && outputs.result.is_some()
                                && !outputs
                                    .warnings
                                    .iter()
                                    .any(|warning| warning.starts_with(FINDINGS_INVALID))
                                && disposition_error.is_none();
                            let outcome = if outcome == AttemptOutcome::DeclaredStop
                                && review_result
                                && !warned
                            {
                                AttemptOutcome::Succeeded
                            } else if warned || outcome == AttemptOutcome::DeclaredStop {
                                AttemptOutcome::OtherFailure
                            } else if outcome == AttemptOutcome::Succeeded
                                && (!review_result
                                    && node.required_verdict.is_some()
                                    && !(plans::writer(node)
                                        && result.as_ref().is_some_and(|capture| {
                                            plans::handoff(capture).ok().flatten().is_some()
                                        }))
                                    && !result
                                        .as_ref()
                                        .is_some_and(|capture| capture.matched_required)
                                    || disposition_error.is_some())
                            {
                                AttemptOutcome::TaskFailed
                            } else {
                                outcome
                            };
                            attempt.outcome = Some(outcome);
                            if outcome == AttemptOutcome::TaskFailed
                                && let Some((code, reason)) = disposition_error
                            {
                                progress.advance_error_code = Some(code.into());
                                progress.advance_error =
                                    Some(bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
                            }
                            compact_findings(&mut outputs, result.as_deref(), &upstream_findings);
                            progress.outputs = Some(outputs);
                            match outcome {
                                AttemptOutcome::Succeeded => {
                                    progress.state = NodeState::Succeeded;
                                    false
                                }
                                AttemptOutcome::AuthRequired
                                | AttemptOutcome::UsageLimited
                                | AttemptOutcome::AdapterUnavailable => {
                                    let review_limit_reached =
                                        review_preparation_substitution(node, attempt)
                                            && progress
                                                .attempts
                                                .iter()
                                                .filter(|attempt| attempt.review_substitution)
                                                .count()
                                                >= limits.review_substitutions;
                                    if !live_fallback_allowed(
                                        outcome,
                                        launched,
                                        node.access,
                                        fallback_evidence,
                                    ) || route_index + 1 == node.routes.len()
                                        || review_limit_reached
                                    {
                                        if outcome == AttemptOutcome::UsageLimited {
                                            progress.advance_error_code =
                                                Some("usage_limited".into());
                                            // Name the pool so the user knows which allowance ran out.
                                            let pool = route
                                                .alias
                                                .as_deref()
                                                .map_or_else(String::new, |alias| {
                                                    format!(" for pool {alias}")
                                                });
                                            progress.advance_error = Some(bounded(
                                                &format!(
                                                    "Provider quota or rate limit reached{pool}; wait for the allowance to reset or choose an eligible route."
                                                ),
                                                MAX_ADVANCE_ERROR_BYTES,
                                            ));
                                        }
                                        if route_index + 1 == node.routes.len()
                                            && launched
                                            && node.access == AccessMode::ReadOnly
                                            && (route.account.is_some() || route.model.is_some())
                                        {
                                            progress.advance_error_code =
                                                Some("fallback_route_pinned".into());
                                            progress.advance_error = Some(
                                                "The selected route is pinned and has no fallback."
                                                    .into(),
                                            );
                                        }
                                        if !live_fallback_allowed(
                                            outcome,
                                            launched,
                                            node.access,
                                            fallback_evidence,
                                        ) && route_index + 1 < node.routes.len()
                                        {
                                            let (code, message) = fallback_denial(
                                                launched,
                                                node.access,
                                                evidence_unreadable,
                                            );
                                            progress.advance_error_code = Some(code.into());
                                            progress.advance_error = Some(message.into());
                                        }
                                        if review_limit_reached
                                            && route_index + 1 < node.routes.len()
                                            && live_fallback_allowed(
                                                outcome,
                                                launched,
                                                node.access,
                                                fallback_evidence,
                                            )
                                        {
                                            progress.advance_error_code =
                                                Some("review_substitution_limit".into());
                                            progress.advance_error =
                                                Some("review substitution limit reached".into());
                                        }
                                        progress.state = NodeState::Failed;
                                        false
                                    } else {
                                        true
                                    }
                                }
                                AttemptOutcome::DeadlineExceeded => {
                                    progress.advance_error_code = Some("deadline_exceeded".into());
                                    progress.advance_error =
                                        Some("node deadline exceeded; lane was cancelled".into());
                                    progress.state = NodeState::Failed;
                                    false
                                }
                                AttemptOutcome::TaskFailed
                                    if review_substitution(node, attempt)
                                        && route_index + 1 < node.routes.len()
                                        && progress
                                            .attempts
                                            .iter()
                                            .filter(|attempt| attempt.review_substitution)
                                            .count()
                                            < limits.review_substitutions =>
                                {
                                    progress.outputs = None;
                                    progress.advance_error = None;
                                    progress.advance_error_code = None;
                                    progress.state = NodeState::Running;
                                    true
                                }
                                AttemptOutcome::TaskFailed
                                | AttemptOutcome::OtherFailure
                                | AttemptOutcome::ReviewRequired
                                | AttemptOutcome::DeclaredStop => {
                                    progress.state = NodeState::Failed;
                                    false
                                }
                            }
                        }
                    }
                };
                if retry_was_pending {
                    asks::refresh_wake(&mut checkpoint);
                }
                if recorded_separation.is_some() {
                    checkpoint.nodes[node_index].separation = recorded_separation;
                }
                if checkpoint.nodes[node_index]
                    .attempts
                    .last()
                    .is_some_and(|attempt| attempt.state == AttemptState::Terminal)
                {
                    stamp_settlement(&mut checkpoint, node_index);
                    record_usage_events(&mut checkpoint, node_index, usage_error, cost_error);
                }
                update_usage_warnings(&mut checkpoint);
                let appended = if settled_result.is_some() || node.plan.is_some() {
                    plans::settle(
                        node,
                        document,
                        &expanded,
                        &mut checkpoint,
                        settled_result.as_deref(),
                        limits,
                    )
                } else {
                    None
                };
                if let Some((next_expanded, next_plan, _)) = &appended {
                    validate_expanded_rows(&checkpoint, next_plan)?;
                    owner
                        .validate_expansion(next_expanded, &checkpoint)
                        .map_err(GraphError::Run)?;
                }
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                if let Some((next_expanded, next_plan, next_positions)) = appended {
                    (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                    resume = false;
                    continue 'schedule;
                }
                if !should_continue {
                    break;
                }
            }
            if (!checkpoint.cancelled || resume)
                && checkpoint.nodes.iter().any(|progress| {
                    progress.id == plan.nodes[node_index].id
                        && progress.state == NodeState::Failed
                        && plan.nodes[node_index]
                            .plan
                            .as_ref()
                            .is_some_and(|spec| spec.fallback.is_some())
                })
                && let Some((next_expanded, next_plan, next_positions)) = plans::settle(
                    &plan.nodes[node_index],
                    document,
                    &expanded,
                    &mut checkpoint,
                    None,
                    limits,
                )
            {
                validate_expanded_rows(&checkpoint, &next_plan)?;
                owner
                    .validate_expansion(&next_expanded, &checkpoint)
                    .map_err(GraphError::Run)?;
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                resume = false;
                continue 'schedule;
            }
        }

        loops::refresh_pause(&mut checkpoint);
        let all_settled = checkpoint.nodes.iter().all(|node| node.state.settled());
        if all_settled
            && checkpoint.nodes.iter().all(|n| n.expansion_pause.is_none())
            && checkpoint.pause_reason.take().is_some()
        {
            checkpoint.pause_code = None;
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }
        // Close asks only after an obsolete pause is cleared, so its ask closes too.
        close_resolved_asks(&mut checkpoint, owner, journal)?;
        if asks::pause(
            document,
            &plan,
            &mut checkpoint,
            owner,
            journal,
            &effective_budget,
        )? {
            return advance_inner(
                document,
                owner,
                journal,
                limits,
                budget,
                false,
                &[],
                observed_ci,
            );
        }
        if document.delivery == Some(provenant_records::run::DeliveryMode::Pr)
            && !checkpoint.delivered
            && checkpoint.successful()
        {
            checkpoint.pull_request = owner
                .deliver_graph(&expanded, &checkpoint)
                .map_err(GraphError::Run)?
                .map(|url| bounded(&url, OUTPUT_FIELD_BYTES));
            checkpoint.delivered = true;
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }
        let status = if checkpoint.pause_reason.is_some() {
            GraphStatus::Paused
        } else if all_settled {
            GraphStatus::Complete
        } else {
            GraphStatus::Pending
        };
        let advance_error = checkpoint
            .nodes
            .iter()
            .find_map(|node| node.advance_error.clone());
        return Ok(GraphRun {
            status,
            outcome: (status == GraphStatus::Complete).then(|| checkpoint.outcome()),
            checkpoint,
            advance_error,
        });
    }
}

/// Whether this plan can raise an inbox ask, which needs a durable identity.
fn may_ask(plan: &PlannedGraph) -> bool {
    plan.decision_session.is_some()
        || plan
            .nodes
            .iter()
            .any(|n| n.loop_body.is_some() || n.decide.is_some() || n.land.is_some())
}

impl PlannedNode {
    /// Whether this recovery node is skipped when `gate` failed with no
    /// findings: its own setting, else whether it takes the gate's findings.
    fn skips_without_findings(&self, gate: &str) -> bool {
        self.skip_without_findings.unwrap_or_else(|| {
            self.inputs
                .iter()
                .any(|input| input.node == gate && input.outputs.contains(&OutputKind::Findings))
        })
    }
}

/// Explain an unexplained Blocked non-gate node by naming its first failed or blocked parent.
/// Returns whether the checkpoint changed.
fn attribute_blocker(
    node_index: usize,
    node: &PlannedNode,
    checkpoint: &mut GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    // These nodes have their own settlement and checkpoint evidence rules.
    if node.condition.is_some()
        || node.composite.is_some()
        || node.land.is_some()
        || checkpoint.nodes[node_index].advance_error.is_some()
    {
        return false;
    }
    let blocker = node
        .after_expansion
        .iter()
        .chain(&node.depends_on)
        .map(|id| &checkpoint.nodes[positions[id]])
        .find(|parent| matches!(parent.state, NodeState::Failed | NodeState::Blocked));
    if let Some(parent) = blocker {
        let reason = format!("blocked by {} ({:?})", parent.id, parent.state);
        checkpoint.nodes[node_index].advance_error =
            Some(bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
        checkpoint.nodes[node_index].advance_error_code = Some("upstream_blocked".into());
        true
    } else {
        false
    }
}

/// A failure target must declare its gate as a dependency. Other dependants
/// follow the success edge; inactive paths propagate a skipped state.
fn dependency_state(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> Option<NodeState> {
    let mut waiting = false;
    let mut skipped = false;
    if node
        .after_expansion
        .iter()
        .any(|parent| !checkpoint.nodes[positions[parent]].state.settled())
    {
        return Some(NodeState::Pending);
    }
    for parent in &node.after_expansion {
        let progress = &checkpoint.nodes[positions[parent]];
        if !checkpoint.node_successful(progress)
            && !(plan.nodes[positions[parent]].advisory
                && matches!(progress.state, NodeState::Failed | NodeState::Blocked))
        {
            return Some(NodeState::Blocked);
        }
    }
    for parent in &node.depends_on {
        let index = positions[parent];
        if loops::round_failure_edge(node, &plan.nodes[index])
            && checkpoint.nodes[index].state == NodeState::Failed
        {
            continue;
        }
        let recovery = plan.nodes[index].on_fail.as_ref().is_some_and(|target| {
            target == &node.id
                || node
                    .round
                    .as_ref()
                    .is_some_and(|round| &round.loop_id == target)
        });
        match checkpoint.nodes[index].state {
            NodeState::Failed | NodeState::Blocked if plan.nodes[index].advisory => {}
            // A failed CI gate that reported no failing check leaves a node
            // that consumes its findings nothing to act on, so it is skipped,
            // not launched. Other recovery nodes (notify, diagnose) still run.
            NodeState::Failed
                if recovery
                    && node.skips_without_findings(&plan.nodes[index].id)
                    && matches!(plan.nodes[index].condition, Some(GateCondition::Ci { .. }))
                    && checkpoint.nodes[index]
                        .outputs
                        .as_ref()
                        .is_some_and(|outputs| outputs.findings.is_empty()) =>
            {
                skipped = true
            }
            NodeState::Failed if recovery => {}
            NodeState::Failed if plan.nodes[index].on_fail.is_some() => skipped = true,
            NodeState::Failed | NodeState::Blocked => return Some(NodeState::Blocked),
            NodeState::Succeeded if recovery => skipped = true,
            NodeState::Skipped => skipped = true,
            NodeState::Pending | NodeState::Running => waiting = true,
            NodeState::Succeeded => {}
        }
    }
    if waiting {
        Some(NodeState::Pending)
    } else if skipped {
        Some(NodeState::Skipped)
    } else {
        None
    }
}

fn gate_outputs(passed: bool) -> NodeOutputs {
    NodeOutputs {
        extra: Default::default(),
        verdict_error: None,
        schema_version: 2,
        result: None,
        verdict: Some(if passed { "PASS" } else { "FAIL" }.to_owned()),
        findings: Vec::new(),
        branch: None,
        commit: None,
        warnings: Vec::new(),
        plan: None,
        remaining: None,
    }
}

fn gate_failure(
    condition: &GateCondition,
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
    budget: &crate::config::GraphBudget,
) -> Option<String> {
    match condition {
        GateCondition::Verdict {
            expected,
            threshold,
            ..
        } => {
            for input in &node.inputs {
                if let Some(error) = checkpoint.nodes[positions[&input.node]]
                    .outputs
                    .as_ref()
                    .and_then(|outputs| outputs.verdict_error.as_ref())
                {
                    return Some(format!("input {}: {error}", input.node));
                }
            }
            let matched = node
                .inputs
                .iter()
                .filter(|input| {
                    checkpoint.nodes[positions[&input.node]]
                        .outputs
                        .as_ref()
                        .and_then(|outputs| outputs.verdict.as_ref())
                        .is_some_and(|actual| {
                            use provenant_records::workflow::ReviewVerdict;
                            match (
                                ReviewVerdict::from_token(actual),
                                ReviewVerdict::from_token(expected),
                            ) {
                                (Some(actual), Some(expected)) => actual == expected,
                                _ => actual == expected,
                            }
                        })
                })
                .count();
            let total = node.inputs.len();
            let passed = match threshold {
                VerdictThreshold::All => matched == total,
                VerdictThreshold::Any => matched > 0,
                VerdictThreshold::Majority => matched > total / 2,
                VerdictThreshold::Count(count) => matched >= *count,
            };
            (!passed).then(|| format!("verdict threshold not met: {matched}/{total} matched"))
        }
        GateCondition::Ci { .. } => unreachable!("CI is observed by the host"),
        GateCondition::Budget { .. } => {
            // An explicit check must fail closed even if admission only warns.
            let mut budget = budget.clone();
            budget.unknown_usage = crate::config::UnknownUsage::Pause;
            budget_reason(checkpoint, &budget)
        }
    }
}
