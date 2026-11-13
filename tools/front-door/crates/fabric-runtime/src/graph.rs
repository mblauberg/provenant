//! Bounded DAG execution for Fabric graphs.
//!
//! `FileJournal` saves a complete canonical checkpoint with the OS layer's safe
//! filesystem API. `AttemptOwner` is implemented by composition over run. Its
//! key is idempotent: a repeated key observes the existing attempt instead of
//! launching a duplicate provider process.

use provenant_core::{CancelToken, Clock, DurationMicros, ProjectId, StateRoot, WaitBudget};
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode, PublishMode};
use provenant_records::pause::PauseCause;
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
#[path = "graph/checkpoint_tests.rs"]
mod checkpoint_tests;
#[cfg(test)]
#[path = "graph/ci_tests.rs"]
mod ci_tests;
mod composites;
mod envelope;
mod journal;
mod land;
#[cfg(test)]
#[path = "graph/land_tests.rs"]
mod land_tests;
mod loops;
mod model;
mod next_attempt;
mod next_step;
mod observation;
mod outputs;
pub mod patch;
mod plans;
mod prompt_store;
pub mod relay;
mod repair;
mod retire;
mod review;
pub mod template;
#[cfg(test)]
mod template_tests;
mod transitions;
mod validation;
mod wait_view;
pub mod when;
#[doc(hidden)]
pub use next_step::{DepletionFacts, DepletionRequest};
pub use plans::prompt as rendered_prompt;
pub use validation::minimum_schema_version;
pub use when::WhenTerm;
#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod ask_link_tests;
#[cfg(test)]
mod cutover_g_tests;
#[cfg(test)]
mod next_step_tests;
#[cfg(test)]
mod typed_records_tests;
#[cfg(test)]
mod when_tests;
use budget::*;
pub use journal::*;
pub use model::*;

/// Review identity follows the launched node, including runtime expansions.
/// `plan.review` frames the subject rather than reviewing it.
pub fn is_review_node(node: &GraphNode) -> bool {
    node.access == AccessMode::ReadOnly
        && (node.role.as_deref().is_some_and(|role| {
            role == "lens" || role.starts_with("lens.") || role.starts_with("review.")
        }) || matches!(
            node.extra
                .get("review_behaviour")
                .and_then(serde_json::Value::as_str),
            Some("lens" | "verify" | "synthesis")
        ))
}

pub(crate) use next_attempt::*;
use next_step::{FallbackEvidence, SettlementFallback};
pub use observation::*;
use outputs::*;
use transitions::{apply_gate_result, dependency_state};
#[cfg(test)]
use transitions::{apply_pending_observation, apply_unknown_observation};
pub(crate) use validation::*;
pub use wait_view::{GraphNodeWait, graph_waits};

/// Persist a writer's contention wait independently of the transport retry
/// limit. Provisioning callers retain their own effect fence until resolution.
/// Returns false for a settled or cancelled node, or a non-contention error.
/// The caller saves the result.
pub fn defer_launch_contention(
    checkpoint: &mut GraphCheckpoint,
    index: usize,
    error: &(dyn Error + 'static),
    clock: Option<provenant_core::WallTime>,
    admission_seconds: u64,
    policy: LaunchRetryPolicy,
) -> Result<bool, String> {
    if !next_attempt::launch_contention(error) {
        return Ok(false);
    }
    let progress = checkpoint
        .nodes
        .get_mut(index)
        .ok_or("launch contention node is unavailable")?;
    if !matches!(progress.state, NodeState::Pending | NodeState::Running)
        || progress.cancelled_from.is_some()
    {
        return Ok(false);
    }
    if clock.is_none() {
        return Err("wall clock unavailable; cannot record a launch retry wake".into());
    }
    // Binding and provisioning precede the first attempt record. Running marks
    // the persisted launch wait as eligible for the scheduler's next wake.
    progress.state = NodeState::Running;
    next_attempt::apply_launch_disposition(
        checkpoint,
        index,
        error,
        clock,
        admission_seconds,
        policy,
    )?;
    Ok(true)
}
#[cfg(test)]
#[path = "graph/journal_tests.rs"]
mod file_tests;
#[cfg(test)]
#[path = "graph/graph_tests.rs"]
mod tests;
pub use asks::{
    AskFiled, AskFiling, AskKind, AskLink, AskOption, AskReply, AskRequest, AskScope, DecideSpec,
    close_resolved_asks, loop_ask,
};
pub use composites::CompositeSpec;
pub use land::{LandAuthority, LandObservation, LandRecord, LandRequest, LandSpec};
pub use loops::{InvalidLoopDecision, LoopDecision, LoopRound, LoopSpec};
pub use patch::{
    Admitted, LiveGraph, Patch, PatchContext, PatchError, PatchRecord, TemplateUse, admit_patch,
};
pub use plans::{Expansion, PlanSpec, RemainingBudgets};
pub use template::{Compiler, Layer, Template, TemplateLibrary, TemplateParameter};

/// Review range resolved for an immutable attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewRange {
    Unknown,
    NoBase,
    Resolved { base: String, head: String },
}

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

    /// Origin of the effective node prompt limit, sealed with the attempt.
    fn input_cap_source(&self) -> InputCapSource {
        InputCapSource::EffectiveGraphLimits
    }

    /// Effective launch retry budget and timing, sealed before launch.
    fn launch_retry_policy(&self) -> LaunchRetryPolicy {
        LaunchRetryPolicy::default()
    }

    /// Restore a writer's sealed deadline, or start its launch allowance before binding.
    fn begin_launch(
        &mut self,
        _access: AccessMode,
        deadline: Option<provenant_core::WallTime>,
    ) -> Result<Option<provenant_core::WallTime>, Self::Error> {
        Ok(deadline)
    }

    /// Maximum time to admit a launch, measured from its first submission.
    fn admission_timeout_seconds(&self) -> Option<u64> {
        None
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

    /// Read a typed preparation cause for reporting, without granting fallback.
    fn preparation_refusal_cause(
        &mut self,
        _key: &AttemptKey,
        _reference: &str,
    ) -> Result<Option<provenant_records::RouteExclusionReason>, Self::Error> {
        Ok(None)
    }

    /// The accounts, by registered label, that an unlaunched attempt's route
    /// passed over with each one's reason, when no account was ready.
    fn preparation_skipped_accounts(
        &mut self,
        _key: &AttemptKey,
        _reference: &str,
    ) -> Result<Vec<provenant_records::SkippedAccount>, Self::Error> {
        Ok(Vec::new())
    }

    /// Host proof that a terminal quota refusal belongs to the same unpinned
    /// alias and has released custody. Partial writer work is retained for the
    /// successor; missing identity or custody evidence denies redraw.
    fn quota_alias_redraw(
        &mut self,
        _key: &AttemptKey,
        _reference: &str,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }

    /// The routing facts the depletion ladder reads for a writer stopped by a
    /// spent quota, a missing model or a lost login. `None` means this host
    /// keeps no routing facts, and the node's own declared routes stand in.
    fn depletion_facts(
        &mut self,
        _request: DepletionRequest<'_>,
    ) -> Result<Option<DepletionFacts>, Self::Error> {
        Ok(None)
    }

    /// Resolve a fresh panel for a repair whose risk or scope has grown.
    fn resolve_repair_panel(
        &mut self,
        _document: &GraphDocument,
        _subject: &str,
    ) -> Result<RepairPanelResolution, String> {
        Err("host cannot resolve @review-panel for the repaired head".into())
    }

    /// Compare a repair's pinned before and after commits against its review base.
    fn detect_repair_growth(&mut self, _before: &str, _after: &str) -> Result<bool, String> {
        Err("host cannot assess repair scope and risk".into())
    }

    /// Resolve a node-specific review base in its bound execution context.
    /// A host without Git may only accept an already pinned object ID.
    fn resolve_review_base(
        &mut self,
        reference: &str,
        _worktree: Option<&str>,
    ) -> Result<Option<String>, Self::Error> {
        Ok(reference
            .parse::<provenant_core::GitOid>()
            .ok()
            .map(|oid| oid.to_string()))
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

    /// File the ask as an open inbox request, idempotently by the graph
    /// instance and ask ID: a repeat returns the request it filed first, with
    /// the deadline that request carries. A host without an inbox names the ask
    /// itself and never has an answer.
    fn file_ask(&mut self, filing: AskFiling<'_>) -> Result<AskFiled, Self::Error> {
        Ok(AskFiled {
            request: filing.ask.ask_id.clone(),
            deadline: filing.ask.wake_at,
            closed: false,
        })
    }
    /// Read the answer to a filed ask, once there is one. A host without an
    /// inbox never answers.
    fn node_ask(&mut self, _scope: AskScope<'_>) -> Result<Option<AskReply>, Self::Error> {
        Ok(None)
    }
    /// Send a durable non-blocking owner notification, idempotent by ask ID.
    /// False means the host has no notification transport yet.
    fn notify_owner(&mut self, _filing: AskFiling<'_>) -> Result<bool, Self::Error> {
        Ok(false)
    }
    /// Withdraw an unanswered ask's inbox request. Closing never creates one,
    /// and an answered request stays answered.
    fn close_node_ask(&mut self, _scope: AskScope<'_>) -> Result<(), Self::Error> {
        Ok(())
    }
    /// Wall clock for ask deadlines. Without one, a decide node with a timeout
    /// refuses to ask, and a stored deadline does not fire.
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        None
    }
    /// Reconcile any earlier delivery before creating a PR. A retry after a
    /// checkpoint write failure must return the existing identity.
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

    /// Verify a registered writer and bind its current source before admission.
    fn bind_writer_worktree(
        &mut self,
        _worktree: &str,
    ) -> Result<Option<WriterWorktreeBinding>, Self::Error> {
        Ok(None)
    }

    /// Submit-time warnings retained in graph status for every transport.
    fn take_launch_warnings(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// Resolve a new writer's base before checkpointing any Git effects.
    fn resolve_creation_base(
        &mut self,
        _branch: &str,
        reference: &str,
        _default_base: bool,
    ) -> Result<Option<String>, Self::Error> {
        self.resolve_review_base(reference, None)
    }

    /// Create or recover a graph-owned writer after its base is checkpointed.
    fn provision_writer_worktree(
        &mut self,
        _worktree: &str,
        _branch: &str,
        _base: &str,
    ) -> Result<Option<WriterWorktreeBinding>, Self::Error> {
        Ok(None)
    }

    /// Create a host-managed writer checkout from the checkpointed source.
    fn provision_child_worktree(
        &mut self,
        _path: &str,
        _source: &str,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }

    /// Graph-wide review and integration bases bound by the execution owner.
    fn bound_graph_bases(
        &self,
    ) -> (
        Option<provenant_core::GitOid>,
        Option<provenant_core::GitOid>,
    ) {
        (None, None)
    }

    /// The route recorded by the host for an accepted attempt, when known.
    fn applied_route(&mut self, _attempt: &str) -> Result<Option<GraphRoute>, Self::Error> {
        Ok(None)
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

    /// Prove whether a review of `reviewed` covers the land `head` of
    /// `branch` that moved after it, against a fetch of the branch its pull
    /// request merges into. `None` means this host cannot prove carry-over,
    /// so only the reviewed head itself may land.
    fn review_coverage(
        &mut self,
        _branch: &str,
        _reviewed: provenant_core::review_cover::Reviewed<'_>,
        _head: &str,
    ) -> Result<Option<provenant_core::review_cover::Coverage>, Self::Error> {
        Ok(None)
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

    /// Resolve a placement before persisting an uncertain launch.
    fn placement_host_id(&self, _name: &str) -> Result<Option<String>, Self::Error> {
        Ok(None)
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

    /// Positively prove that a failed submission never crossed acceptance.
    /// Owners without a durable acceptance lookup must leave recovery fenced.
    fn launch_refused_before_acceptance(
        &mut self,
        _attempt: &GraphAttempt,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }

    /// Resolve the range of an immutable review attempt, without launching it.
    fn review_range(
        &mut self,
        _attempt_ref: &str,
        _explicit_base: bool,
    ) -> Result<ReviewRange, Self::Error> {
        Ok(ReviewRange::Unknown)
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

/// Full host-selected review topology and per-lens routing for a repair replan.
#[derive(Clone, Debug)]
pub struct RepairPanelResolution {
    pub slots: Vec<RepairPanelSlot>,
    pub frame: bool,
    pub verifiers: usize,
    pub synthesis: bool,
    pub advice: std::collections::BTreeMap<String, String>,
}

/// Host-selected lens and routing fields for a repair review slot.
#[derive(Clone, Debug)]
pub struct RepairPanelSlot {
    pub lens: String,
    pub route: serde_json::Value,
    pub family_group: Option<String>,
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
    #[error("invalid graph: {0}")]
    Admission(AdmissionError),
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
            Self::Invalid(_) | Self::Admission(_) => "invalid_graph",
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
    #[serde(skip)]
    review_launch: bool,
    #[serde(skip_serializing_if = "provenant_records::OnStop::is_empty")]
    on_stop: provenant_records::OnStop,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempt: Option<AttemptSpec>,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    when: Vec<WhenTerm>,
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
        budget_sources: context.budget_sources.clone(),
        graph_template_launch: false,
        writer_predecessor: None,
        extra: Default::default(),
        risk_tier: context
            .risk_tier
            .clone()
            .or_else(|| graph.risk_tier.clone()),
        task_kind: context
            .task_kind
            .clone()
            .or_else(|| graph.task_kind.clone()),
        graph_id: Some(plan.graph_id.clone()),
        node_id: Some(node.id.clone()),
        attempt: None,
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
        // The requested tier is set by the launch, from the route it asks for.
        tier: None,
        template: node
            .extra
            .get(template::TEMPLATE_FIELD)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        parent_node: node
            .extra
            .get(patch::PARENT_NODE_FIELD)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
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

/// The verified writer location on a reader's source chain, when known.
fn source_worktree<'a>(
    node: &'a PlannedNode,
    plan: &'a PlannedGraph,
    checkpoint: &'a GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> Option<&'a str> {
    let mut current = node;
    for _ in 0..plan.nodes.len() {
        let source = match &current.source {
            Some(SourceBinding::Node { node } | SourceBinding::Head { node }) => node,
            _ => return None,
        };
        let index = *positions.get(source)?;
        current = &plan.nodes[index];
        if plans::writer(current) {
            return checkpoint.nodes[index]
                .writer_binding
                .as_ref()
                .map(|binding| binding.worktree.as_str());
        }
    }
    None
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
            Some(state @ (NodeState::Pending | NodeState::Running))
                if node.state == NodeState::Skipped
                    && node
                        .skip_reason
                        .as_deref()
                        .is_some_and(|reason| reason.starts_with("parent ")) =>
            {
                // A resumed graph must not resurrect a cancelled subtree.
                node.cancelled_from = Some(state);
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

/// Fence one node and its attached descendants before stopping their attempts.
/// The checkpoint that settles a stopped writer, with its plan: the restart
/// patch, or a chair pause when admission, planning or the owner refuses the
/// restart. Never an error, so a refused restart cannot leave the stop owing.
#[allow(clippy::type_complexity)]
fn settle_stopped_writer<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
) -> Result<
    Option<(
        GraphCheckpoint,
        GraphDocument,
        PlannedGraph,
        std::collections::BTreeMap<String, usize>,
    )>,
    GraphError<R::Error, J::Error>,
> {
    let mut ladder = |request: DepletionRequest<'_>| {
        owner
            .depletion_facts(request)
            .map_err(|error| error.to_string())
    };
    let Some((index, next)) =
        next_step::settle_stop(document, plan, checkpoint, limits, &mut ladder)
    else {
        return Ok(None);
    };
    let planned =
        plans::expand(document, &next.extensions, &next.expansions, limits).and_then(|expanded| {
            let (plan, positions) =
                validate_flat_plan(&expanded, limits).map_err(|error| error.to_string())?;
            owner
                .validate_expansion(&expanded, &next)
                .map_err(|error| error.to_string())?;
            Ok((expanded, plan, positions))
        });
    let (next, (expanded, plan, positions)) = match planned {
        Ok(planned) => (next, planned),
        Err(reason) => {
            // The pause keeps the saved expansions, which already planned.
            let paused = next_step::refused(plan, checkpoint, index, &reason);
            let expanded = plans::expand(document, &paused.extensions, &paused.expansions, limits)
                .map_err(GraphError::Invalid)?;
            let (plan, positions) = validate_flat_plan(&expanded, limits)
                .map_err(|error| GraphError::Invalid(error.to_string()))?;
            (paused, (expanded, plan, positions))
        }
    };
    Ok(Some((next, expanded, plan, positions)))
}

pub fn cancel_node_subtree(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    id: &str,
    reason: Option<&str>,
) -> Result<relay::SubtreeCancel, String> {
    let index = checkpoint
        .nodes
        .iter()
        .position(|node| node.id == id)
        .ok_or_else(|| format!("unknown node {id}"))?;
    let mut effect = relay::cancel_subtree(document, checkpoint, id)?;
    let node = &mut checkpoint.nodes[index];
    next_step::fence_stopped(node);
    if !node.state.settled() {
        if node
            .attempts
            .iter()
            .any(|a| a.state != AttemptState::Terminal)
        {
            node.extra.insert("subtree_cancel".into(), id.into());
            effect.running.push(id.to_owned());
        } else {
            node.cancelled_from = Some(node.state);
            node.state = NodeState::Skipped;
            effect.skipped.push(id.to_owned());
        }
        node.skip_reason = Some(format!(
            "parent {id} cancelled: {}",
            reason.unwrap_or("stop requested")
        ));
    }
    asks::refresh_wake(checkpoint);
    Ok(effect)
}

/// Reconcile subtree cancellation without admitting or launching more work.
pub fn settle_stopped_subtrees<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    relay::settle_subtrees(
        document,
        checkpoint,
        owner,
        journal,
        &mut Default::default(),
    )?;
    close_resolved_asks(checkpoint, owner, journal)
}

/// Mark nodes that have no live attempts, as part of the durable cancellation fence.
pub fn mark_cancelled_nodes_without_live_attempts(checkpoint: &mut GraphCheckpoint) -> bool {
    if !checkpoint.cancelled {
        return false;
    }
    let mut changed = false;
    for node in &mut checkpoint.nodes {
        if matches!(node.state, NodeState::Pending | NodeState::Running)
            && node.extra.get(relay::DETACHED_FROM_GRAPH_CANCEL)
                != Some(&serde_json::Value::Bool(true))
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
    close_resolved_asks(checkpoint, owner, journal)?;
    let mut first_error = None;
    for index in 0..checkpoint.nodes.len() {
        let node = &checkpoint.nodes[index];
        if node.state != NodeState::Running
            || node.extra.get(relay::DETACHED_FROM_GRAPH_CANCEL)
                == Some(&serde_json::Value::Bool(true))
        {
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

/// Effect driver: load the checkpoint, collect host facts, apply the pure
/// transition and save before the next effect. Node and attempt decisions live
/// in `transitions` and `next_attempt`; host failures leave the previous saved
/// checkpoint (including its launch and custody fences) authoritative.
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
            .map_err(|error| GraphError::Invalid(error.to_string()))?
    } else {
        validate_plan_with_limits(document, &unbounded).map_err(GraphError::Admission)?
    };
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
            // Cancellation retains attempt custody evidence. Validate the
            // original state without reopening the actual admission fence.
            for node in &mut validation.nodes {
                if node
                    .skip_reason
                    .as_deref()
                    .is_some_and(|reason| reason.starts_with("parent "))
                    && let Some(state) = node.cancelled_from.take()
                {
                    node.state = state;
                    node.skip_reason = None;
                }
            }
            asks::refresh_wake(&mut validation);
            validate_checkpoint(document, &validation, &plan, &plan_digest)?;
            checkpoint
        }
        None => {
            if let Some(reason) = graph_envelope_reason(document, &[], &[], &plan) {
                return Err(GraphError::Invalid(reason));
            }
            let asks = may_ask(&plan);
            let (bound_review_base, bound_integration_base) = owner.bound_graph_bases();
            let instance_id = if asks {
                Some(provenant_core::hex(
                    &provenant_os::fs::entropy_128().map_err(GraphError::Instance)?,
                ))
            } else {
                None
            };
            let ask_actor = asks.then(|| owner.actor());
            let mut checkpoint = transitions::initial_checkpoint(
                document,
                &plan,
                plan_digest,
                bound_review_base.map(|base| base.to_string()),
                bound_integration_base.map(|base| base.to_string()),
                instance_id,
                ask_actor,
            );
            if let Some(raw) = document.extra.get("submission_patch") {
                let patch: patch::Patch = serde_json::from_value(raw.clone())
                    .map_err(|error| GraphError::Invalid(error.to_string()))?;
                let library = template::TemplateLibrary::bundled();
                let resolvers = Default::default();
                patch::admit_patch(
                    &mut patch::LiveGraph {
                        document,
                        checkpoint: &mut checkpoint,
                    },
                    None,
                    &patch,
                    &patch::PatchContext {
                        library: &library,
                        resolvers: &resolvers,
                        scan: &patch::scan_for_secrets,
                        journal: None,
                        limits,
                    },
                )
                .map_err(|error| GraphError::Invalid(error.to_string()))?;
            }
            {
                for (node, progress) in plan.nodes.iter().zip(&mut checkpoint.nodes) {
                    // A loop round's worktree is advanced by earlier rounds;
                    // its starting head becomes known only at that round.
                    if plans::writer(node)
                        && node
                            .extra
                            .get("provision_worktree")
                            .and_then(serde_json::Value::as_bool)
                            != Some(true)
                        && node.round.is_none()
                        && node
                            .attempt
                            .as_ref()
                            .and_then(|spec| spec.new_worktree.as_ref())
                            .is_none()
                    {
                        let worktree = node
                            .worktree
                            .as_deref()
                            .ok_or_else(|| GraphError::Invalid("writer worktree missing".into()))?;
                        let binding = match owner.bind_writer_worktree(worktree) {
                            Ok(binding) => binding,
                            // Contention waits in the launch loop, which owns the
                            // persisted deadline; only a refusal fails the node here.
                            Err(error) if next_attempt::launch_contention(&error) => continue,
                            Err(error) => {
                                transitions::writer_binding_failed(
                                    progress,
                                    &node.id,
                                    &next_attempt::writer_binding_reason(&node.id, &error),
                                );
                                continue;
                            }
                        };
                        transitions::apply_writer_binding(
                            node,
                            progress,
                            binding,
                            document.schema_version >= 2,
                        )
                        .map_err(GraphError::Refused)?;
                    }
                }
            }
            // Pin named review bases while the admitted source locations are
            // still the ones selected for this execution.
            for index in 0..plan.nodes.len() {
                if !checkpoint.nodes[index].review_base_pending {
                    continue;
                }
                let node = &plan.nodes[index];
                let location = source_worktree(node, &plan, &checkpoint, &positions);
                if matches!(
                    node.source,
                    Some(SourceBinding::Node { .. } | SourceBinding::Head { .. })
                ) && location.is_none()
                {
                    continue;
                }
                let reference = node
                    .extra
                    .get("review_base")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| GraphError::Invalid("node review base missing".into()))?;
                let base = owner.resolve_review_base(reference, location).map_err(GraphError::Run)?
                    .filter(|base| base.parse::<provenant_core::GitOid>().is_ok())
                    .ok_or_else(|| GraphError::Refused(format!("node {}: review base could not be bound; correct the reference and rerun", node.id)))?;
                transitions::apply_review_base(&mut checkpoint.nodes[index], base);
            }
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
    if transitions::refresh_limit_notes(&mut checkpoint, document) {
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
        }
        transitions::apply_resume(&mut checkpoint, extensions)
            .map_err(|(node, invariant)| GraphError::BadCheckpoint { node, invariant })?;
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
            transitions::apply_loop_extensions(&mut checkpoint, extensions, &extended)
                .map_err(|error| GraphError::Invalid(error.into()))?;
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

    let mut stopped_children = std::collections::BTreeSet::new();
    'schedule: loop {
        if let Some(next) = review::correct_verdict(document, &plan, &checkpoint, limits)
            .map_err(|error| GraphError::Refused(error.to_string()))?
        {
            let next_expanded = plans::expand(document, &next.extensions, &next.expansions, limits)
                .map_err(|error| GraphError::Invalid(error.to_string()))?;
            let (next_plan, next_positions) = validate_flat_plan(&next_expanded, limits)
                .map_err(|error| GraphError::Invalid(error.to_string()))?;
            owner
                .validate_expansion(&next_expanded, &next)
                .map_err(GraphError::Run)?;
            journal.save(&next).map_err(GraphError::Journal)?;
            checkpoint = next;
            (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
            resume = false;
            continue 'schedule;
        }
        if let Some((next, next_expanded, next_plan, next_positions)) =
            settle_stopped_writer::<R, J>(document, owner, &plan, &checkpoint, limits)?
        {
            // A stop saved by an earlier host, or one the chair resumed.
            journal.save(&next).map_err(GraphError::Journal)?;
            checkpoint = next;
            (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
            resume = false;
            continue 'schedule;
        }
        plans::remaining(&mut checkpoint, &expanded, limits);
        if checkpoint.cancelled {
            relay::fence_detached_cancellation(document, &mut checkpoint)
                .map_err(GraphError::Refused)?;
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }
        for node in &plan.nodes {
            if (!checkpoint.cancelled
                || resume
                || checkpoint.nodes.iter().any(|progress| {
                    progress.id == node.id
                        && progress.extra.get(relay::DETACHED_FROM_GRAPH_CANCEL)
                            == Some(&serde_json::Value::Bool(true))
                }))
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
        let structural_pause =
            transitions::refresh_structure_pause(&mut checkpoint, &expanded, &plan, limits);
        if structural_pause.is_some() {
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }

        close_resolved_asks(&mut checkpoint, owner, journal)?;
        if checkpoint.cancelled {
            if !resume {
                settle_cancelled_nodes(&mut checkpoint, owner, journal)?;
                let detached_live = checkpoint.nodes.iter().any(|node| {
                    !node.state.settled()
                        && node.extra.get(relay::DETACHED_FROM_GRAPH_CANCEL)
                            == Some(&serde_json::Value::Bool(true))
                });
                if !detached_live {
                    return Ok(GraphRun {
                        status: GraphStatus::Cancelled,
                        outcome: None,
                        checkpoint,
                        advance_error: None,
                    });
                }
            } else {
                transitions::clear_cancellation(&mut checkpoint);
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
            }
        }

        relay::settle_subtrees(
            document,
            &mut checkpoint,
            owner,
            journal,
            &mut stopped_children,
        )?;
        for node_index in 0..plan.nodes.len() {
            if checkpoint.cancelled
                && checkpoint.nodes[node_index]
                    .extra
                    .get(relay::DETACHED_FROM_GRAPH_CANCEL)
                    != Some(&serde_json::Value::Bool(true))
            {
                continue;
            }
            if checkpoint.nodes[node_index]
                .extra
                .contains_key(relay::SUBTREE_CANCEL)
            {
                continue;
            }
            // A parent earlier in this wave may just have settled.
            if checkpoint.nodes.iter().any(|node| {
                node.state.settled()
                    && node.extra.get("children_cancelled") != Some(&serde_json::Value::Bool(true))
            }) {
                relay::settle_subtrees(
                    document,
                    &mut checkpoint,
                    owner,
                    journal,
                    &mut stopped_children,
                )?;
                if checkpoint.nodes[node_index]
                    .extra
                    .contains_key(relay::SUBTREE_CANCEL)
                {
                    continue;
                }
            }
            let node = &plan.nodes[node_index];
            match transitions::node_action(node_index, &plan, &mut checkpoint, &positions) {
                transitions::NodeAction::Wait { changed } => {
                    if changed {
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    }
                    continue;
                }
                transitions::NodeAction::Composite => {
                    composites::settle::<R, J>(
                        node_index,
                        &plan,
                        &positions,
                        &mut checkpoint,
                        journal,
                    )?;
                    continue;
                }
                transitions::NodeAction::Loop(spec) => {
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
                        if let Err(reason) = loops::prepare_decision_extension(
                            document,
                            &checkpoint,
                            &extension,
                            limits,
                        ) {
                            loops::pause_invalid_decision(
                                &mut checkpoint,
                                &node.id,
                                &reason,
                                journal,
                            )?;
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
                transitions::NodeAction::Decide(spec) => {
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
                transitions::NodeAction::Land(spec) => {
                    let wall_now = owner.wall_now();
                    let waiting = owner.respect_ci_wake()
                        && checkpoint.nodes[node_index]
                            .land
                            .as_ref()
                            .and_then(|record| record.wake_at)
                            .is_some_and(|wake| wall_now.is_none_or(|now| now < wake));
                    if structural_pause.is_none() && checkpoint.pause.is_none() && !waiting {
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
                transitions::NodeAction::Gate(condition) => {
                    if structural_pause.is_some() || checkpoint.pause.is_some() {
                        continue;
                    }
                    let mut replan_error = None;
                    let reported = repair::growth_in_round(node, &plan, &checkpoint, &positions);
                    let mut growth = reported;
                    if repair::is_repair_gate(node, &plan, &positions) {
                        if !reported && repair::risk_assessment_missing(node, &plan, &checkpoint) {
                            replan_error = Some(
                            "cannot assess repair growth: delta reviewer omitted a valid risk tier"
                                .into(),
                        );
                        }
                        if let Some((before, after)) = repair::repair_commits(node, &checkpoint)
                            .map(|(before, after)| (before.to_owned(), after.to_owned()))
                        {
                            if let Some(value) = repair::cached_growth(
                                &checkpoint.nodes[node_index],
                                &before,
                                &after,
                            ) {
                                growth |= value;
                            } else {
                                match owner.detect_repair_growth(&before, &after) {
                                    Ok(value) => {
                                        transitions::record_repair_growth(
                                            &mut checkpoint.nodes[node_index],
                                            &before,
                                            &after,
                                            value,
                                        );
                                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                                        growth |= value;
                                    }
                                    Err(error) if !reported => {
                                        replan_error =
                                            Some(format!("cannot assess repair growth: {error}"))
                                    }
                                    Err(_) => {}
                                }
                            }
                        } else if document.schema_version >= 2 && !reported {
                            replan_error = Some(
                                "cannot assess repair growth: pinned commits unavailable".into(),
                            );
                        }
                    }
                    if growth {
                        if let Some((parent, commit)) = repair::replan_source(node, &checkpoint)
                            .map(|(parent, commit)| (parent.to_owned(), commit.to_owned()))
                        {
                            if repair::panel_expansion(&checkpoint, &parent).is_none() {
                                let result = (|| -> Result<_, String> {
                                    commit
                                        .parse::<provenant_core::GitOid>()
                                        .map_err(|e| e.to_string())?;
                                    let panel = owner.resolve_repair_panel(document, &commit)?;
                                    let single = panel.slots.len() == 1;
                                    // Repair input needs checked dispositions even for
                                    // a single lens. Reuse the existing synthesis step.
                                    let verifiers = if single { 0 } else { panel.verifiers };
                                    let stages = when::review_stages(
                                        panel.frame && !single,
                                        verifiers > 0,
                                        true,
                                    );
                                    let verifier_ids: Vec<String> = (1..=verifiers.max(1))
                                        .map(|index| index.to_string())
                                        .collect();
                                    let slots = panel.slots;
                                    let mut routes = Vec::with_capacity(slots.len());
                                    let mut lenses = Vec::with_capacity(slots.len());
                                    for slot in slots {
                                        lenses.push(slot.lens.clone());
                                        let mut fields = slot
                                            .route
                                            .as_object()
                                            .cloned()
                                            .ok_or("invalid panel route")?;
                                        if document.schema_version >= 2
                                            && let Some(group) = slot.family_group
                                        {
                                            fields.insert("family_group".into(), group.into());
                                        }
                                        fields
                                            .insert("distinct_from".into(), "dependencies".into());
                                        fields.insert("distinct_strength".into(), "prefer".into());
                                        fields.insert("require_findings".into(), true.into());
                                        fields.insert(
                                            "required_verdict".into(),
                                            "VERDICT: PASS".into(),
                                        );
                                        routes.push(
                                            serde_json::to_string(&fields)
                                                .map_err(|error| error.to_string())?,
                                        );
                                    }
                                    let mut resolvers = std::collections::BTreeMap::from([
                                        ("review-panel".into(), lenses.clone()),
                                        ("review-panel-routes".into(), routes),
                                        ("review-verifiers".into(), verifier_ids),
                                    ]);
                                    resolvers.extend(stages);
                                    for lens in lenses {
                                        resolvers.insert(
                                            format!("review-advice-{lens}"),
                                            vec![
                                                panel
                                                    .advice
                                                    .get(&lens)
                                                    .cloned()
                                                    .unwrap_or_default(),
                                            ],
                                        );
                                    }
                                    let library = template::TemplateLibrary::bundled();
                                    let context = patch::PatchContext {
                                        library: &library,
                                        resolvers: &resolvers,
                                        scan: &patch::scan_for_secrets,
                                        journal: None,
                                        limits,
                                    };
                                    let mut prospective = checkpoint.clone();
                                    let change = repair::panel_patch(
                                        &parent,
                                        &commit,
                                        patch::revision(&prospective),
                                    );
                                    patch::admit_patch(
                                        &mut patch::LiveGraph {
                                            document,
                                            checkpoint: &mut prospective,
                                        },
                                        Some(&parent),
                                        &change,
                                        &context,
                                    )
                                    .map_err(|error| error.message)?;
                                    let preview = plans::expand(
                                        document,
                                        &prospective.extensions,
                                        &prospective.expansions,
                                        limits,
                                    )
                                    .map_err(|error| error.to_string())?;
                                    let (new_plan, new_positions) =
                                        validate_flat_plan(&preview, &plans::generous())
                                            .map_err(|error| error.to_string())?;
                                    owner
                                        .validate_expansion(&preview, &prospective)
                                        .map_err(|error| error.to_string())?;
                                    Ok((prospective, preview, new_plan, new_positions))
                                })();
                                match result {
                                    Ok((prospective, preview, new_plan, new_positions)) => {
                                        journal.save(&prospective).map_err(GraphError::Journal)?;
                                        checkpoint = prospective;
                                        (expanded, plan, positions) =
                                            (preview, new_plan, new_positions);
                                        continue 'schedule;
                                    }
                                    Err(error) => {
                                        replan_error =
                                            Some(format!("full panel replan required: {error}"))
                                    }
                                }
                            } else if let Some(panel) =
                                repair::panel_expansion(&checkpoint, &parent)
                            {
                                if !repair::panel_reviews_commit(panel, &commit) {
                                    replan_error = Some("full panel reviewed a different repaired commit; retry the repair container".into());
                                } else if repair::panel_pending(&checkpoint, panel) {
                                    continue;
                                } else {
                                    replan_error = repair::panel_failure(&checkpoint, panel);
                                }
                            }
                        } else {
                            replan_error = Some(
                                "full panel replan required: repaired commit unavailable".into(),
                            );
                        }
                    }
                    let reason = if growth || replan_error.is_some() {
                        replan_error
                    } else if matches!(condition, GateCondition::Ci { .. }) {
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
                        match ci::evaluate(
                            node_index,
                            node,
                            &mut checkpoint,
                            owner,
                            wall_now,
                            backoff,
                        )
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
                    apply_gate_result(
                        &mut checkpoint.nodes[node_index],
                        reason,
                        node.on_fail.clone(),
                        matches!(condition, GateCondition::Ci { .. }),
                    );
                    asks::refresh_wake(&mut checkpoint);
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    continue;
                }
                transitions::NodeAction::Attempt => {}
            }
            if !checkpoint.nodes[node_index].extra.contains_key("retry")
                && let Some((retries, origin)) = owner
                    .retry_allowance(&node.extra, &plan.extra)
                    .map_err(GraphError::Run)?
            {
                transitions::record_retry_allowance(
                    &mut checkpoint.nodes[node_index],
                    retries,
                    &origin,
                );
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
            }
            let requested_review_base = (node.access == AccessMode::ReadOnly)
                .then(|| {
                    node.extra
                        .get("review_base")
                        .and_then(serde_json::Value::as_str)
                })
                .flatten()
                .filter(|base| !base.is_empty());
            if let Some(reference) = requested_review_base {
                let progress = &checkpoint.nodes[node_index];
                if progress.review_base.is_none() && !progress.attempts.is_empty() {
                    return Err(GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "node review base must be bound before its first attempt",
                    });
                }
                if let Some(base) = &progress.review_base {
                    if base.parse::<provenant_core::GitOid>().is_err() {
                        return Err(GraphError::BadCheckpoint {
                            node: node.id.clone(),
                            invariant: "node review base must be a full object ID",
                        });
                    }
                } else {
                    let base = owner
                        .resolve_review_base(
                            reference,
                            source_worktree(node, &plan, &checkpoint, &positions)
                                .or(node.worktree.as_deref()),
                        )
                        .map_err(GraphError::Run)?;
                    let Some(base) =
                        base.filter(|base| base.parse::<provenant_core::GitOid>().is_ok())
                    else {
                        return Err(GraphError::Refused(format!(
                            "node {}: review base could not be bound; correct the reference and rerun",
                            node.id
                        )));
                    };
                    transitions::apply_review_base(&mut checkpoint.nodes[node_index], base);
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                }
            }
            loop {
                let (route_index, settlement_retries) = match decide_next_attempt(
                    node,
                    &checkpoint.nodes[node_index],
                    limits,
                    owner,
                    owner.wall_now(),
                )
                .map_err(GraphError::Run)?
                {
                    NextAttempt::WaitRetryWake => break,
                    NextAttempt::LaunchRetryExhausted { reason } => {
                        transitions::retry_exhausted(&mut checkpoint.nodes[node_index], &reason);
                        asks::refresh_wake(&mut checkpoint);
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    NextAttempt::Fail { code, message } => {
                        fail_node(
                            &mut checkpoint.nodes[node_index],
                            code.map(Into::into),
                            message,
                        );
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    NextAttempt::None => break,
                    NextAttempt::Candidate {
                        route_index,
                        settlement_retries,
                    } => (route_index, settlement_retries),
                };
                let route = node
                    .routes
                    .get(route_index)
                    .ok_or_else(|| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "attempt route index must name a configured route",
                    })?
                    .clone();
                let key = transitions::attempt_key(&plan, node_index, &checkpoint, route_index)
                    .map_err(|error| GraphError::Invalid(error.into()))?;

                {
                    let prior = checkpoint.nodes[node_index]
                        .launch_retry
                        .as_ref()
                        .and_then(|retry| retry.deadline);
                    if let Some(deadline) = owner
                        .begin_launch(node.access, prior)
                        .map_err(GraphError::Run)?
                        && prior.is_none()
                    {
                        checkpoint.nodes[node_index].launch_retry = Some(LaunchRetry {
                            extra: Default::default(),
                            policy: owner.launch_retry_policy(),
                            failures: 0,
                            deadline: Some(deadline),
                            wake_at: None,
                            reason: None,
                        });
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    }
                    // Relay provisioning seals the deadline before the host owner
                    // is available to resolve the configured retry policy.
                    if checkpoint.nodes[node_index].attempts.is_empty()
                        && let Some(retry) = checkpoint.nodes[node_index].launch_retry.as_mut()
                    {
                        retry.policy = owner.launch_retry_policy();
                    }
                }

                // Expanded writers acquire their binding as soon as they exist.
                if plans::writer(node)
                    && checkpoint.nodes[node_index].writer_binding.is_none()
                    && checkpoint.nodes[node_index].attempts.is_empty()
                {
                    let path = node
                        .worktree
                        .as_deref()
                        .ok_or_else(|| GraphError::Invalid("writer worktree missing".into()))?;
                    if node
                        .extra
                        .get("provision_worktree")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                        && (relay::pending_provisioning(&checkpoint)
                            .iter()
                            .any(|(id, _, _)| id == &node.id)
                            || !checkpoint.nodes.iter().any(|progress| {
                                progress.worktree.as_deref() == Some(path)
                                    && progress.writer_binding.is_some()
                            }))
                    {
                        let pending = relay::pending_provisioning(&checkpoint)
                            .into_iter()
                            .find(|(id, _, _)| id == &node.id);
                        let source = if let Some((_, _, source)) = pending {
                            source
                        } else {
                            let source = if let Some(source) = checkpoint.nodes[node_index]
                                .source_commit
                                .clone()
                                .or_else(|| {
                                    checkpoint.nodes[node_index]
                                        .extra
                                        .get("provisioned_source")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_owned)
                                }) {
                                Some(source)
                            } else {
                                match &node.source {
                                    Some(SourceBinding::Commit { commit }) => Some(commit.clone()),
                                    Some(SourceBinding::Node { node: source }) => positions
                                        .get(source)
                                        .and_then(|index| checkpoint.nodes[*index].outputs.as_ref())
                                        .and_then(|outputs| outputs.commit.clone()),
                                    Some(SourceBinding::Head { node: writer }) => land::chain_head(
                                        &plan,
                                        &checkpoint,
                                        &positions,
                                        &node.id,
                                        writer,
                                    ),
                                    Some(SourceBinding::Integration) => {
                                        owner.integration_head().map_err(GraphError::Run)?
                                    }
                                    None => {
                                        let bound = checkpoint.bound_integration_base.clone();
                                        match bound {
                                            Some(source) => Some(source),
                                            None => owner
                                                .bind_writer_worktree(".")
                                                .map_err(GraphError::Run)?
                                                .map(|binding| binding.head),
                                        }
                                    }
                                }
                            }
                            .ok_or_else(|| {
                                GraphError::Invalid("writer provisioning source unavailable".into())
                            })?;
                            transitions::record_provisioning(
                                node,
                                &mut checkpoint,
                                node_index,
                                path,
                                &source,
                            );
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                            source
                        };
                        match owner.provision_child_worktree(path, &source) {
                            Ok(true) => {}
                            Ok(false) => {
                                return Err(GraphError::Refused(
                                    "host cannot provision writer worktrees".into(),
                                ));
                            }
                            Err(error) => {
                                if defer_launch_contention(
                                    &mut checkpoint,
                                    node_index,
                                    &error,
                                    owner.wall_now(),
                                    owner.admission_timeout_seconds().unwrap_or(3600),
                                    owner.launch_retry_policy(),
                                )
                                .map_err(GraphError::Refused)?
                                {
                                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                                    break;
                                }
                                return Err(GraphError::Run(error));
                            }
                        }
                    }
                    let binding_result = if let Some(spec) = node
                        .attempt
                        .as_ref()
                        .and_then(|spec| spec.new_worktree.as_ref())
                    {
                        // Retain the resolved base before any Git effect. Recovery
                        // adopts only the clean worktree at this exact commit.
                        let base = match checkpoint.nodes[node_index]
                            .extra
                            .get("new_worktree_base")
                            .and_then(serde_json::Value::as_str)
                        {
                            Some(base) => base.to_owned(),
                            None => {
                                let base = owner
                                    .resolve_creation_base(
                                        &spec.branch,
                                        &spec.base,
                                        spec.extra
                                            .get("default_base")
                                            .and_then(serde_json::Value::as_bool)
                                            .unwrap_or(false),
                                    )
                                    .map_err(GraphError::Run)?
                                    .filter(|base| base.parse::<provenant_core::GitOid>().is_ok())
                                    .ok_or_else(|| {
                                        GraphError::Refused(format!(
                                            "node {}: writer base could not be resolved",
                                            node.id
                                        ))
                                    })?;
                                transitions::record_creation_base(
                                    &mut checkpoint.nodes[node_index],
                                    &base,
                                );
                                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                                base
                            }
                        };
                        owner.provision_writer_worktree(path, &spec.branch, &base)
                    } else {
                        owner.bind_writer_worktree(path)
                    };
                    let binding = match binding_result {
                        Ok(binding) => binding,
                        Err(error) => {
                            if defer_launch_contention(
                                &mut checkpoint,
                                node_index,
                                &error,
                                owner.wall_now(),
                                owner.admission_timeout_seconds().unwrap_or(3600),
                                owner.launch_retry_policy(),
                            )
                            .map_err(GraphError::Refused)?
                            {
                                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                                break;
                            }
                            transitions::writer_binding_failed(
                                &mut checkpoint.nodes[node_index],
                                &node.id,
                                &next_attempt::writer_binding_reason(&node.id, &error),
                            );
                            checkpoint.nodes[node_index].advance_owner_code =
                                Some(next_attempt::launch_diagnostic(&error).code);
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                            break;
                        }
                    };
                    transitions::apply_writer_binding(
                        node,
                        &mut checkpoint.nodes[node_index],
                        binding,
                        document.schema_version >= 2
                            || node
                                .attempt
                                .as_ref()
                                .is_some_and(|spec| spec.new_worktree.is_some()),
                    )
                    .map_err(GraphError::Refused)?;
                    relay::clear_provisioning(&mut checkpoint, &node.id);
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                }
                let needs_record = checkpoint.nodes[node_index]
                    .attempts
                    .last()
                    .is_none_or(|last| last.state == AttemptState::Terminal);
                let mut prepared_inputs = None;
                if needs_record {
                    let cap = limits.node_prompt_bytes;
                    match transitions::admit_attempt(
                        node_index,
                        &plan,
                        &mut checkpoint,
                        &positions,
                        limits,
                        &effective_budget,
                    ) {
                        transitions::Admission::Ready => {}
                        transitions::Admission::WriterBusy
                        | transitions::Admission::SlotWait { changed: false } => break,
                        transitions::Admission::SlotWait { changed: true }
                        | transitions::Admission::SaveAndWait => {
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                            break;
                        }
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
                        let valid = transitions::apply_source_commit(
                            node,
                            &mut checkpoint,
                            node_index,
                            commit,
                        );
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        if !valid {
                            break;
                        }
                    }
                    if node.access == AccessMode::ReadOnly && template::review_node(node) {
                        let progress = &checkpoint.nodes[node_index];
                        let base = progress
                            .review_base
                            .as_ref()
                            .or(checkpoint.bound_review_base.as_ref());
                        if base
                            .zip(progress.source_commit.as_ref())
                            .is_some_and(|(base, head)| base == head)
                        {
                            transitions::fail_with(
                                &mut checkpoint.nodes[node_index],
                                "review_empty_range",
                                "review range is empty; select a subject with changes".into(),
                            );
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                            break;
                        }
                    }
                    let mut integration_valid = true;
                    for expected in
                        transitions::integration_expectations(node_index, &plan, &checkpoint)
                    {
                        let head = owner.integration_head().map_err(GraphError::Run)?;
                        if !transitions::apply_integration_observation(
                            node,
                            &mut checkpoint.nodes[node_index],
                            &expected,
                            head.as_deref(),
                        ) {
                            integration_valid = false;
                            break;
                        }
                    }
                    if !integration_valid
                        || !transitions::continuation_ready(
                            node,
                            node_index,
                            &mut checkpoint,
                            &positions,
                        )
                    {
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    // File pointers are part of the prompt cap too. Stage them
                    // before recording an attempt so a delivery that cannot fit
                    // fails the consumer without launching it.
                    let requests = upstream_results(node, &plan, &checkpoint, &positions);
                    let materialised = if requests.is_empty() {
                        None
                    } else {
                        owner.materialise_inputs(&key, &requests, NODE_INPUT_FILE_BYTES)
                    };
                    let files = materialised.as_ref().map_or_else(String::new, |files| {
                        render_files(&files.entries, &requests, files.index.as_deref())
                    });
                    if !transitions::file_inputs_fit(
                        node_index,
                        &plan,
                        &mut checkpoint,
                        &positions,
                        cap,
                        files.len(),
                    ) {
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    prepared_inputs = Some((requests, materialised));
                    let placement_host_id = node
                        .attempt
                        .as_ref()
                        .and_then(|spec| spec.host.as_ref())
                        .map(|host| owner.placement_host_id(host))
                        .transpose()
                        .map_err(GraphError::Run)?
                        .flatten();
                    let launch_actor = owner.actor();
                    let input_cap_source = owner.input_cap_source();
                    let timing =
                        owner
                            .admission_timeout_seconds()
                            .map(|admission_seconds| LaunchTiming {
                                clock: owner.wall_now(),
                                admission_seconds,
                                policy: owner.launch_retry_policy(),
                            });
                    transitions::begin_attempt(
                        node_index,
                        &plan,
                        &mut checkpoint,
                        &positions,
                        &key,
                        &route,
                        transitions::LaunchFacts {
                            input_cap: cap,
                            placement_host_id,
                            launch_actor,
                            input_cap_source,
                            timing,
                        },
                    );
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
                let accepted = if stored_ref.is_some() {
                    true
                } else {
                    let attempt =
                        checkpoint.nodes[node_index]
                            .attempts
                            .last()
                            .ok_or_else(|| GraphError::BadCheckpoint {
                                node: node.id.clone(),
                                invariant: "an observed attempt must have a checkpoint row",
                            })?;
                    owner.launch_accepted(attempt).map_err(GraphError::Run)?
                };
                // Accepted attempts are observed with their immutable run input.
                let (requests, materialised) = if stored_ref.is_some() {
                    (Vec::new(), None)
                } else if let Some(prepared) = prepared_inputs {
                    prepared
                } else {
                    let requests = upstream_results(node, &plan, &checkpoint, &positions);
                    let materialised = if requests.is_empty() {
                        None
                    } else {
                        owner.materialise_inputs(&key, &requests, NODE_INPUT_FILE_BYTES)
                    };
                    (requests, materialised)
                };
                if let Some(materialised) = &materialised {
                    prompt.push_str(&render_files(
                        &materialised.entries,
                        &requests,
                        materialised.index.as_deref(),
                    ));
                }
                if !accepted && !node.inputs.is_empty() && prompt.len() > input_cap {
                    return Err(GraphError::Refused(format!(
                        "node {}: selected upstream file notices no longer fit the saved node_prompt_bytes; restore the original delivery or restart with a larger limit",
                        node.id
                    )));
                }
                if !accepted {
                    let manifest = attempt_input_manifest(
                        node,
                        &plan,
                        &checkpoint,
                        &positions,
                        &requests,
                        materialised.as_ref(),
                    );
                    transitions::apply_input_manifest(&mut checkpoint.nodes[node_index], manifest)
                        .map_err(|invariant| GraphError::BadCheckpoint {
                            node: node.id.clone(),
                            invariant,
                        })?;
                    // Evidence is durable before the owner can submit the attempt.
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
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
                // A restart is in place: its archived attempts below carry the
                // models the alias draw excludes.
                let mut prior_attempts = Vec::new();
                for attempt in checkpoint.nodes[node_index]
                    .previous_attempts
                    .iter()
                    .skip(
                        checkpoint.nodes[node_index]
                            .previous_attempts
                            .len()
                            .saturating_sub(
                                checkpoint.nodes[node_index]
                                    .extra
                                    .get("quota_redraw_archived")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(0) as usize,
                            ),
                    )
                    .chain(
                        checkpoint.nodes[node_index]
                            .attempts
                            .iter()
                            .take(route_index),
                    )
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
                    transitions::record_separation(&mut checkpoint.nodes[node_index], record);
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                }
                let continued = land::continued_branch(node, &checkpoint, &positions);
                let observation = match owner.observe_or_start(
                    &key,
                    NodeInput {
                        review_launch: node.review_launch,
                        require_skill_location: node.require_skill_location,
                        settlement_retries,
                        prior_attempts: &prior_attempts,
                        family_peers: &family_peers,
                        context: provenant_records::run::WorkContext {
                            tier: route.tier.clone().or_else(|| route.alias.clone()),
                            // The work key's attempt: one-based launch order
                            // within the node, never reused.
                            attempt: u32::try_from(
                                checkpoint.nodes[node_index].recorded_attempts().count(),
                            )
                            .ok(),
                            ..node_context(&plan, node)
                        },
                        read_roots: &node.read_roots,
                        attachments: &node.attachments,
                        capabilities: node.capabilities.as_deref(),
                        capability_sources: node.extra.get("capability_sources"),
                        attempt: node.attempt.as_ref(),
                        distinct_from_attempts: &distinct_attempts,
                        require_distinct,
                        source_commit: checkpoint.nodes[node_index].source_commit.as_deref(),
                        review_base: checkpoint.nodes[node_index].review_base.as_deref(),
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
                    if node.access == AccessMode::ReadOnly {
                        source_worktree(node, &plan, &checkpoint, &positions)
                            .or(node.worktree.as_deref())
                    } else {
                        checkpoint.nodes[node_index]
                            .writer_binding
                            .as_ref()
                            .map(|binding| binding.worktree.as_str())
                            .or(node.worktree.as_deref())
                    },
                    Replay {
                        delivery: land::delivery(node),
                        branch: continued.as_deref(),
                        fast_forward_only: node.round.is_some() || node.continues.is_some(),
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
                        let clock = owner.wall_now();
                        let admission_seconds = owner.admission_timeout_seconds().unwrap_or(3600);
                        let policy = owner.launch_retry_policy();
                        launch_disposition::<R, J>(
                            owner,
                            &mut checkpoint,
                            node_index,
                            &error,
                            LaunchTiming {
                                clock,
                                admission_seconds,
                                policy,
                            },
                            journal,
                        )?;
                        break;
                    }
                    Err(error) => return Err(GraphError::Run(error)),
                };
                let warnings = owner.take_launch_warnings();
                transitions::apply_launch_metadata(&mut checkpoint, node_index, warnings);
                let applied_route = match &observation {
                    AttemptObservation::Pending { attempt_ref }
                    | AttemptObservation::Unknown { attempt_ref }
                    | AttemptObservation::Terminal { attempt_ref, .. } => attempt_ref
                        .as_ref()
                        .and_then(|reference| owner.applied_route(reference).ok().flatten()),
                };
                transitions::apply_route(&mut checkpoint.nodes[node_index], applied_route);
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
                let fallback_cause = match &observation {
                    AttemptObservation::Terminal {
                        attempt_ref: Some(reference),
                        launched: false,
                        outcome,
                        ..
                    } if route_index + 1 < node.routes.len()
                        && fallback_candidate(*outcome, node.access, false)
                        && !live_fallback_allowed(
                            *outcome,
                            false,
                            node.access,
                            fallback_evidence,
                        ) =>
                    {
                        owner
                            .preparation_refusal_cause(&key, reference)
                            .ok()
                            .flatten()
                    }
                    _ => None,
                };
                let skipped_accounts = match &observation {
                    AttemptObservation::Terminal {
                        attempt_ref: Some(reference),
                        launched: false,
                        outcome: AttemptOutcome::UsageLimited,
                        ..
                    } => owner
                        .preparation_skipped_accounts(&key, reference)
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                let quota_redraw = match &observation {
                    AttemptObservation::Terminal {
                        attempt_ref: Some(reference),
                        outcome: AttemptOutcome::UsageLimited,
                        ..
                    } if route.alias.is_some() && route.model.is_none() => {
                        quota_redraw_budget(&checkpoint.nodes[node_index])
                            && owner
                                .quota_alias_redraw(&key, reference)
                                .map_err(GraphError::Run)?
                    }
                    _ => false,
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
                                if record
                                    .applied
                                    .as_ref()
                                    .is_some_and(|family| family_peers.contains(family))
                                {
                                    record.outcome = SeparationOutcome::Unavailable;
                                }
                            }
                            Some(record)
                        }
                        _ => None,
                    };
                let review_range = match &observation {
                    AttemptObservation::Terminal {
                        attempt_ref: Some(reference),
                        ..
                    } if node.access == AccessMode::ReadOnly && template::review_node(node) => {
                        let explicit_base = node
                            .extra
                            .get("review_base")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|base| !base.is_empty());
                        owner
                            .review_range(reference, explicit_base)
                            .map_err(GraphError::Run)?
                    }
                    _ => ReviewRange::Unknown,
                };
                let transitions::AppliedObservation {
                    settled_result,
                    retry_was_pending,
                    should_continue,
                } = transitions::apply_attempt_observation(
                    &plan,
                    &mut checkpoint,
                    &positions,
                    node_index,
                    limits,
                    observation,
                    transitions::ObservationFacts {
                        route_index,
                        usage,
                        fallback_evidence,
                        evidence_unreadable,
                        fallback_cause,
                        skipped_accounts,
                        review_range,
                    },
                )
                .map_err(|invariant| GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant,
                })?;
                transitions::record_observation(
                    &mut checkpoint,
                    node,
                    &plan,
                    &positions,
                    transitions::ObservationRecord {
                        node_index,
                        retry_was_pending,
                        recorded_separation,
                        usage_error,
                        cost_error,
                    },
                );
                if quota_redraw {
                    transitions::redraw_quota_alias(
                        document,
                        node,
                        &mut checkpoint,
                        node_index,
                        limits,
                    )
                    .map_err(GraphError::Refused)?;
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
                // Save a writer's stop in the same transition as its restart
                // or chair pause: a stop saved alone reads as settled work.
                let stopped = if appended.is_none() {
                    settle_stopped_writer::<R, J>(document, owner, &plan, &checkpoint, limits)?
                } else {
                    None
                };
                if let Some((next, next_expanded, next_plan, next_positions)) = stopped {
                    journal.save(&next).map_err(GraphError::Journal)?;
                    checkpoint = next;
                    (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                    resume = false;
                    continue 'schedule;
                }
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                if let Some((next_expanded, next_plan, next_positions)) = appended {
                    (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                    resume = false;
                    continue 'schedule;
                }
                if checkpoint.nodes[node_index].advance_error_code.as_deref()
                    == Some("review_verdict_invalid")
                {
                    resume = false;
                    continue 'schedule;
                }
                if !should_continue {
                    break;
                }
            }
            if (!checkpoint.cancelled
                || resume
                || checkpoint.nodes[node_index]
                    .extra
                    .get(relay::DETACHED_FROM_GRAPH_CANCEL)
                    == Some(&serde_json::Value::Bool(true)))
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

        relay::settle_subtrees(
            document,
            &mut checkpoint,
            owner,
            journal,
            &mut stopped_children,
        )?;
        loops::refresh_pause(&mut checkpoint);
        if transitions::clear_settled_pause(&mut checkpoint) {
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
            let url = owner
                .deliver_graph(&expanded, &checkpoint)
                .map_err(GraphError::Run)?
                .ok_or_else(|| {
                    GraphError::Refused(
                        "PR delivery returned no identity; reconcile delivery before retrying"
                            .into(),
                    )
                })?;
            transitions::record_delivery(&mut checkpoint, url).map_err(GraphError::Refused)?;
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }
        return Ok(transitions::graph_run(checkpoint));
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
fn gate_outputs(passed: bool) -> NodeOutputs {
    NodeOutputs {
        extra: Default::default(),
        verdict_error: None,
        schema_version: 2,
        result: None,
        verdict: Some(if passed { "PASS" } else { "FAIL" }.to_owned()),
        findings: Vec::new(),
        findings_available: Some(true),
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
