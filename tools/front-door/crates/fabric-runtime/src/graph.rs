//! Bounded DAG execution for Fabric graphs.
//!
//! `FileJournal` saves a complete canonical checkpoint with the OS layer's safe
//! filesystem API. `AttemptOwner` is implemented by composition over run. Its
//! key is idempotent: a repeated key observes the existing attempt instead of
//! launching a duplicate provider process.

use provenant_core::{CancelToken, Clock, DurationMicros, ProjectId, StateRoot, WaitBudget};
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode, PublishMode};
pub use provenant_records::graph::{
    AttemptKey, AttemptOutcome, AttemptState, GraphOutcome, GraphStatus, NodeState,
};
use provenant_records::pause::PauseCause;
use provenant_records::{CanonicalError, CanonicalV1, DecodeError, Limits};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use thiserror::Error;

mod asks;
mod ci;
pub use ci::{CiFailedCheck, CiTarget, ConflictEvidence, ConflictObservation, ConflictState};
mod budget;
mod candidate;
mod canonical;
pub use canonical::{canonicalise_route_models, canonicalise_route_nodes};
pub mod refusal;
pub use candidate::{AdmittedCandidate, CandidateError, admit_candidate, admit_running_candidate};
mod cancellation_intent;
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
mod prompt_text;
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
pub use prompt_text::normalise_prompt_text;
pub use validation::minimum_schema_version;
pub use when::WhenTerm;
#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod ask_link_tests;
#[cfg(test)]
mod cutover_followup_invariants;
#[cfg(test)]
mod cutover_g_tests;
#[cfg(test)]
mod fallback_invariants;
#[cfg(test)]
#[path = "graph/graph_host_typed_invariants.rs"]
mod graph_host_typed_invariants;
#[cfg(test)]
mod implement_invariants;
#[cfg(test)]
mod implementation_delivery_invariants;
#[cfg(test)]
mod next_step_tests;
#[cfg(test)]
mod stack_tests;
#[cfg(test)]
mod submission_implement_invariants;
#[cfg(test)]
#[path = "graph/typed_finding_tests.rs"]
mod typed_finding_tests;
#[cfg(test)]
mod typed_records_tests;
#[cfg(test)]
mod when_requirement_invariants;
#[cfg(test)]
mod when_tests;
use budget::*;
pub use journal::*;
pub use model::*;

/// Review identity follows the launched node, including runtime expansions.
/// `plan.review` frames the subject rather than reviewing it.
pub fn is_review_node(node: &GraphNode) -> bool {
    node.access == AccessMode::ReadOnly
        && (node.delivery_review.is_some()
            || node.role.as_deref().is_some_and(|role| {
                role == "lens" || role.starts_with("lens.") || role.starts_with("review.")
            })
            || matches!(
                node.review_behaviour,
                Some(
                    template::ReviewBehaviour::Lens
                        | template::ReviewBehaviour::Verify
                        | template::ReviewBehaviour::Synthesis
                )
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
    DecideTimeout, DecideTimeoutSource, UnansweredDisposition, close_resolved_asks, loop_ask,
};
pub use composites::CompositeSpec;
pub use land::{LandAuthority, LandObservation, LandRecord, LandRequest, LandSpec};
pub use loops::{InvalidLoopDecision, LoopDecision, LoopRound, LoopSpec};
pub use patch::{
    Admitted, LiveGraph, Patch, PatchContext, PatchError, PatchRecord, TemplateUse, admit_patch,
};
pub use plans::{Expansion, PlanSpec, RemainingBudgets, RetiredExpansion};
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
// 16 MiB plus the pending-steer reserve (#3303 session-ready steering), so a
// graph at the default node limit stays admitted.
const MAX_CHECKPOINT_BYTES: u64 = 17_825_792;
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
    /// Observe PR mergeability before the graph waits on CI for the same head.
    /// Hosts without this probe fail closed as unavailable.
    fn observe_conflict(&mut self, _target: &str) -> ConflictObservation {
        ConflictObservation {
            state: ConflictState::Unavailable,
            head_sha: None,
            summary: "mergeability observation unavailable".into(),
        }
    }
    fn observe_ci(
        &mut self,
        target: &str,
        checks: &[String],
        delivery_queue: bool,
        failed_checks_max_bytes: usize,
        max_failed_checks: usize,
    ) -> CiObservation;
}

/// The host work required by one persisted hygiene node. Identity and the
/// attempt's input head are sealed in the checkpoint before this call.
pub struct HygieneRequest<'a> {
    pub graph_id: &'a str,
    pub node_id: &'a str,
    pub operation_id: &'a str,
    pub source_node: &'a str,
    pub source_key: &'a AttemptKey,
    pub source_attempt: &'a str,
    pub worktree: &'a str,
    pub worktree_identity: Option<&'a provenant_records::run::SourceIdentity>,
    pub input_head: &'a str,
    pub expected_head: &'a str,
    pub handoff: Option<&'a str>,
}

/// Typed result plus a bounded operator diagnostic for an unsuccessful
/// formatter or commit operation.
pub struct HygieneExecution {
    pub outcome: provenant_records::graph::HygieneOutcome,
    pub branch: Option<String>,
    pub formatter_source: Option<String>,
    pub diagnostic: Option<String>,
}

/// Host publication of one clean hygiene head. `source_branch` is the sealed
/// graph integration branch; `expected_remote_head` is the prior receipt for
/// this stable graph-owned branch, or `None` when it must not yet exist.
pub struct PublicationRequest<'a> {
    pub graph_id: &'a str,
    pub node_id: &'a str,
    pub operation_id: &'a str,
    pub publication_identity: &'a str,
    pub source_node: &'a str,
    pub source_key: &'a AttemptKey,
    pub source_attempt: &'a str,
    pub worktree: &'a str,
    pub worktree_identity: Option<&'a provenant_records::run::SourceIdentity>,
    pub writer_branch: &'a str,
    pub source_branch: &'a str,
    pub expected_head: &'a str,
    pub expected_remote_head: Option<&'a str>,
    pub integration_base: &'a str,
    pub handoff: Option<&'a str>,
}

/// Persisted input to one parking transition. The host files an inbox request
/// with `operation_id`, whose immutable payload includes every field here.
pub struct DeliveryParkRequest<'a> {
    pub transition_id: &'a str,
    pub operation_id: &'a str,
    pub graph_id: &'a str,
    /// The queue lane supplies a sealed tracker item when it exists. Until
    /// then the inbox request identifies the graph separately.
    pub item: Option<&'a str>,
    pub node_id: &'a str,
    pub attempt_key: Option<&'a AttemptKey>,
    pub attempt_ref: Option<&'a str>,
    pub repository: Option<&'a str>,
    pub session: Option<&'a str>,
    pub reason: provenant_records::graph::ParkReason,
    pub handoff: &'a str,
    pub head: Option<&'a str>,
    pub evidence: &'a [String],
}

/// `observe_or_start` must be idempotent by `AttemptKey`. It starts the run
/// only when that key has no accepted operation, and otherwise observes it.
/// A `stored_ref` from the checkpoint names an accepted attempt: observe it as
/// is, never resubmit. Without one, the submission replays under `launch_actor`
/// (the actor recorded before the first submit) so a changed actor cannot
/// launch duplicate work. Provider launch and terminal publication remain run's responsibility.
pub trait AttemptOwner {
    type Error: Error + 'static;

    /// The worktree layout already resolved by this execution's project host.
    /// Graph planning is pure and never probes the filesystem or selects a layout.
    fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort;

    /// The host's effective routing catalogue, used to decide one spelling of
    /// every model a patch admits. Hosts without routing policy keep `None`.
    fn catalogue(&self) -> Option<provenant_routing::Catalogue> {
        None
    }

    /// Read the admitted successor of a terminal attempt, without replacing
    /// the predecessor's identity or submitting another operation.
    fn continuation(
        &mut self,
        _reference: &str,
    ) -> Result<Option<AttemptContinuation>, Self::Error> {
        Ok(None)
    }

    /// Whether a terminal attempt's queued steer was refused before its
    /// successor launched. The predecessor and its accepted text stay visible.
    fn continuation_refused(&mut self, _reference: &str) -> Result<bool, Self::Error> {
        Ok(false)
    }

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

    /// Effective host/project policy for graph admission.
    fn graph_admission_policy(&self) -> provenant_records::policy::Policy {
        provenant_records::policy::Policy::default()
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

    /// Effective configurable decide timeout, including where its value came from.
    fn decide_timeout(&self) -> Result<DecideTimeout, Self::Error> {
        Ok(DecideTimeout {
            seconds: crate::config::DEFAULT_DECIDE_TIMEOUT_SECONDS,
            source: DecideTimeoutSource::DomainDefault,
        })
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

    /// Run the configured formatter against the retained writer checkout.
    /// The caller persists `operation_id` and `input_head` before this effect;
    /// owners must reconcile that identity before retrying an uncertain commit.
    fn run_hygiene(
        &mut self,
        _request: HygieneRequest<'_>,
    ) -> Result<HygieneExecution, Self::Error> {
        Ok(HygieneExecution {
            outcome: provenant_records::graph::HygieneOutcome {
                kind: provenant_records::graph::HygieneOutcomeKind::Error,
                before_head: None,
                after_head: None,
                handoff: None,
            },
            branch: None,
            formatter_source: None,
            diagnostic: Some("host does not implement delivery hygiene".into()),
        })
    }

    /// Publish a clean queue head using a stable graph-owned ref. The host
    /// reconciles the expected remote head and PR identity before returning.
    fn publish_delivery(
        &mut self,
        _request: PublicationRequest<'_>,
    ) -> Result<Option<DeliveryPublication>, Self::Error> {
        Ok(None)
    }

    /// File one durable inbox request for a delivery transition. `None` means
    /// no receipt was established; the persisted intent remains pending and is
    /// replayed under the same operation identity.
    fn park_delivery(
        &mut self,
        _request: DeliveryParkRequest<'_>,
    ) -> Result<Option<provenant_core::HumanRequestId>, Self::Error> {
        Ok(None)
    }

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
        _owner: Option<provenant_core::Digest>,
    ) -> Result<Option<WriterWorktreeBinding>, Self::Error> {
        Ok(None)
    }

    /// Create a host-managed writer checkout from the checkpointed source.
    fn provision_child_worktree(
        &mut self,
        _path: &str,
        _source: &str,
        _owner: Option<provenant_core::Digest>,
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

    /// Write each requested upstream result, verified and displayed as
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
    fn placement_host_id(
        &self,
        _name: &str,
    ) -> Result<Option<provenant_core::HostId>, Self::Error> {
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

    /// Apply and durably acknowledge any stop request that arrived outside the
    /// graph lock. Implementations that have no independent control channel do
    /// nothing.
    fn apply_cancellation(
        &mut self,
        _document: &GraphDocument,
        _checkpoint: &mut GraphCheckpoint,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

fn apply_pending_cancellation<RunError, J: GraphJournal>(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    journal: &mut J,
    resume: &mut bool,
) -> Result<bool, GraphError<RunError, J::Error>>
where
    RunError: Error + 'static,
{
    let applied = journal
        .apply_cancellation(document, checkpoint)
        .map_err(GraphError::Journal)?;
    if applied {
        // A stop that arrived during a resume wins at this boundary. Do not
        // clear its durable fence as part of the same in-flight operation.
        *resume = false;
    }
    Ok(applied)
}

#[derive(Debug, Error)]
pub enum GraphError<RunError: Error + 'static, JournalError: Error + 'static> {
    #[error("cannot create graph checkpoint instance identity")]
    Instance(#[source] FsError),
    #[error("invalid graph: {0}")]
    Invalid(String),
    #[error("invalid graph: {0}")]
    Admission(AdmissionError),
    #[error("{0}")]
    Candidate(CandidateError),
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
            Self::Candidate(CandidateError::Extension(_)) => "invalid_extension",
            Self::Candidate(
                CandidateError::Checkpoint | CandidateError::CheckpointTopology { .. },
            ) => "bad_checkpoint",
            Self::Candidate(_) => "invalid_graph",
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
    #[serde(skip_serializing_if = "Option::is_none")]
    queue: Option<QueueKnobs>,
    schema_version: u64,
    graph_id: String,
    nodes: Vec<PlannedNode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct PlannedNode {
    // Derived from the persisted document; the warning is already retained there.
    #[serde(skip)]
    writer_dirty_preflight: bool,
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
    max_parallel: Option<usize>,
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
    // A project-relative fixture stays host-local; bytes already staged by a
    // peer cross the boundary. Both are forwarded to the launch as NodeInput.
    #[serde(skip_serializing_if = "Option::is_none")]
    fixture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fixture_bytes: Option<provenant_records::run::StagedFixture>,
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
    escalate: Option<EscalateSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_review: Option<DeliveryReviewSpec>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    after_settled: Vec<String>,
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
    // Host-owned node fields, copied from `GraphNode` at planning.
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_base: Option<String>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    capability_sources: provenant_records::run::CapabilitySources,
    #[serde(skip_serializing_if = "model::is_false")]
    repair_scope_gate: bool,
    #[serde(skip_serializing_if = "model::is_false")]
    detached: bool,
    #[serde(skip_serializing_if = "model::is_false")]
    provision_worktree: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_behaviour: Option<template::ReviewBehaviour>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    removed_requirements: Vec<RemovedRequirement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_lens: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repair_lens: Option<String>,
    #[serde(skip_serializing_if = "model::is_false")]
    repair_writer: bool,
    #[serde(skip_serializing_if = "model::is_false")]
    repair_triage: bool,
    #[serde(skip_serializing_if = "model::is_false")]
    repair_risk_assessment: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_generation: Option<u64>,
    #[serde(skip_serializing_if = "model::is_false")]
    container_retry: bool,
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
        delivery_queue_writer_adoption: false,
        extra: Default::default(),
        risk_tier: context
            .risk_tier
            .clone()
            .or_else(|| graph.risk_tier.clone()),
        task_kind: context
            .task_kind
            .clone()
            .or_else(|| graph.task_kind.clone()),
        pr: context.pr.or(graph.pr),
        lens: context.lens.clone().or_else(|| graph.lens.clone()),
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
        template: node.template.clone(),
        parent_node: node.parent_node.clone(),
        selection_source: context
            .selection_source
            .clone()
            .or_else(|| graph.selection_source.clone()),
    }
}

impl PlannedNode {
    fn dependencies(&self) -> impl Iterator<Item = &String> {
        self.depends_on
            .iter()
            .chain(self.after_settled.iter())
            .chain(self.after_expansion.iter().filter(|name| {
                !self.depends_on.contains(name) && !self.after_settled.contains(name)
            }))
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
        if current.kind == Some(NodeKind::Publish) {
            let writer = land::published_source_writer(plan, checkpoint, positions, source)?;
            let writer_index = *positions.get(writer)?;
            return checkpoint.nodes[writer_index]
                .writer_binding
                .as_ref()
                .map(|binding| binding.worktree.as_str());
        }
        if plans::writer(current) {
            return checkpoint.nodes[index]
                .writer_binding
                .as_ref()
                .map(|binding| binding.worktree.as_str());
        }
    }
    None
}

/// Stack size for every thread that runs graph execution, in production
/// workers and in test harness threads alike.
///
/// Reason: a debug build gives the scheduling and launch functions large
/// frames (opt-level 0 keeps every temporary in its own slot), and one graph
/// launch nests several of them. The 2 MiB default of a spawned thread cannot
/// hold that depth, so a launch overflows it (#3542). The largest phases are
/// split and boxed to keep each frame small, but the nesting is inherent to the
/// executor, so the stack is sized for it rather than left to the platform.
/// `tools/front-door/.cargo/config.toml` sets the same size as
/// `RUST_MIN_STACK` for test harness threads, which only that variable can
/// size; `stack_tests` pins the two together.
pub const GRAPH_STACK_BYTES: usize = 8 * 1024 * 1024;

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
        &[],
        None,
    )
}

/// Save submission refusals before any node is allowed to start.
pub fn advance_with_submission_refusals<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
    refusals: &[(String, String)],
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
        refusals,
        None,
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
        &[],
        None,
    )
}

/// Resume with the candidate admitted while holding the same journal lock.
pub fn resume_admitted<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
    extensions: &[String],
    candidate: AdmittedCandidate,
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
        &[],
        Some(candidate),
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
    worktree_layout: &provenant_config::WorktreeLayoutPort,
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
    let Some((index, next)) = next_step::settle_stop(
        document,
        plan,
        checkpoint,
        limits,
        worktree_layout,
        &mut ladder,
    ) else {
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
            node.subtree_cancel = Some(id.to_owned());
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
            && !node.detached_from_graph_cancel
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
        if node.state != NodeState::Running || node.detached_from_graph_cancel {
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

/// Control flow for the extracted scheduling pass (#3542).
enum ScheduleExit {
    /// The pass produced a final run for the caller to return.
    Done(Box<GraphRun>),
    /// Re-enter `advance_inner` for a decision extension or a cleared pause.
    Recurse {
        resume: bool,
        extensions: Vec<String>,
        observed_ci: std::collections::BTreeSet<String>,
    },
}

/// What the schedule does after a gate node was evaluated.
enum GateStep {
    /// Continue with the next node.
    Next,
    /// The checkpoint or plan was replaced; start the pass again.
    Reschedule,
}

/// Evaluate one gate node (repair-growth replan, CI poll or condition check).
/// Its own frame keeps the replan machinery off the scheduling loop (#3542).
#[allow(
    clippy::too_many_arguments,
    reason = "one admitted execution state follows internal rescheduling"
)]
fn graph_ancestors(
    node_index: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
) -> std::collections::BTreeSet<usize> {
    let mut seen = std::collections::BTreeSet::new();
    let mut pending: Vec<usize> = plan.nodes[node_index]
        .dependencies()
        .filter_map(|id| positions.get(id).copied())
        .collect();
    while let Some(index) = pending.pop() {
        if seen.insert(index) {
            pending.extend(
                plan.nodes[index]
                    .dependencies()
                    .filter_map(|id| positions.get(id).copied()),
            );
        }
    }
    seen
}

fn node_depends_on(
    node_index: usize,
    ancestor: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    graph_ancestors(node_index, plan, positions).contains(&ancestor)
}

fn same_expanded_round(left: Option<&loops::LoopRound>, right: Option<&loops::LoopRound>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.loop_id == right.loop_id && left.round == right.round,
        (None, None) => true,
        _ => false,
    }
}

fn recovery_container_members(
    root: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
) -> std::collections::BTreeSet<usize> {
    let mut members = std::collections::BTreeSet::new();
    let mut pending = vec![root];
    while let Some(parent) = pending.pop() {
        let node = &plan.nodes[parent];
        for member in &node.composite_members {
            if let Some(index) = positions.get(member).copied()
                && members.insert(index)
            {
                pending.push(index);
            }
        }
        if node.loop_body.is_some() {
            for (index, child) in plan.nodes.iter().enumerate() {
                if child
                    .round
                    .as_ref()
                    .is_some_and(|round| round.loop_id == node.id)
                    && members.insert(index)
                {
                    pending.push(index);
                }
            }
        }
    }
    members
}

fn prior_clear_conflict_gate(
    node_index: usize,
    published: &str,
    head: &str,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &GraphCheckpoint,
) -> Option<usize> {
    graph_ancestors(node_index, plan, positions)
        .into_iter()
        .rev()
        .find(|index| {
            same_expanded_round(
                plan.nodes[*index].round.as_ref(),
                plan.nodes[node_index].round.as_ref(),
            ) && matches!(
                &plan.nodes[*index].condition,
                Some(GateCondition::Conflict {
                    target: CiTarget::Node { node, .. },
                    ..
                }) if node == published
            ) && checkpoint.nodes[*index].state == NodeState::Succeeded
                && checkpoint.nodes[*index]
                    .gate
                    .as_ref()
                    .is_some_and(|gate| gate.passed)
                && checkpoint.nodes[*index]
                    .conflict
                    .as_ref()
                    .is_some_and(|evidence| {
                        evidence.state == ConflictState::Clear
                            && evidence.head_sha.as_deref() == Some(head)
                    })
        })
}

fn queue_conflict_recheck_already_observed(
    node_index: usize,
    condition: &GateCondition,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &GraphCheckpoint,
) -> bool {
    let GateCondition::Ci { target, .. } = condition else {
        return false;
    };
    let CiTarget::Node {
        node: published, ..
    } = target
    else {
        return false;
    };
    let Some(published_index) = positions.get(published).copied() else {
        return false;
    };
    if plan.nodes[published_index].kind != Some(NodeKind::Publish) {
        return false;
    }
    let Some(head) = ci::published_head(target, checkpoint, true) else {
        return false;
    };
    prior_clear_conflict_gate(node_index, published, &head, plan, positions, checkpoint)
        .and_then(|index| checkpoint.nodes[index].conflict.as_ref())
        .is_some_and(|evidence| evidence.ci_rechecks > 0)
}

fn reset_skipped_conflict_branch(
    index: usize,
    checkpoint: &mut GraphCheckpoint,
) -> Result<(), &'static str> {
    let progress = &mut checkpoint.nodes[index];
    if progress.state != NodeState::Skipped || progress.skip_reason.is_some() {
        return Ok(());
    }
    if !progress.attempts.is_empty()
        || !progress.previous_attempts.is_empty()
        || progress.writer_binding.is_some()
        || progress.outputs.is_some()
        || progress.gate.is_some()
        || progress.ci.is_some()
        || progress.conflict.is_some()
        || progress.delivery_park.is_some()
    {
        return Err("skipped conflict recovery branch has execution evidence");
    }
    progress.state = NodeState::Pending;
    progress.wait_reason = None;
    progress.skip_reason = None;
    Ok(())
}

fn preparation_exhausted_message(message: Option<String>) -> String {
    message.unwrap_or_else(|| "Preparation retries exhausted".into())
}

fn queue_conflict_recheck(
    node_index: usize,
    condition: &GateCondition,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    wall_now: Option<provenant_core::WallTime>,
) -> Result<(usize, usize), &'static str> {
    let GateCondition::Ci { target, .. } = condition else {
        return Err("queue CI conflict must target a published node");
    };
    let CiTarget::Node {
        node: published, ..
    } = target
    else {
        return Err("queue CI conflict must target a published node");
    };
    let Some(published_index) = positions.get(published).copied() else {
        return Err("queue CI conflict target is missing");
    };
    if plan.nodes[published_index].kind != Some(NodeKind::Publish) {
        return Err("queue CI conflict target is not a publication node");
    }
    let Some(head) = ci::published_head(target, checkpoint, true) else {
        return Err("queue CI conflict lost its published head");
    };
    let conflict_index =
        prior_clear_conflict_gate(node_index, published, &head, plan, positions, checkpoint)
            .ok_or("queue CI conflict has no prior clear conflict gate")?;
    let rebase = plan.nodes[conflict_index]
        .on_fail
        .as_ref()
        .and_then(|target| positions.get(target))
        .copied()
        .ok_or("queue conflict gate has no rebase writer")?;
    let recovery_container =
        plan.nodes[rebase].composite.is_some() || plan.nodes[rebase].loop_body.is_some();
    if (!plans::writer(&plan.nodes[rebase]) && !recovery_container)
        || !same_expanded_round(
            plan.nodes[rebase].round.as_ref(),
            plan.nodes[node_index].round.as_ref(),
        )
    {
        return Err("queue conflict recovery target is not a same-round writer or container");
    }

    reset_skipped_conflict_branch(rebase, checkpoint)?;
    let contained = if recovery_container {
        recovery_container_members(rebase, plan, positions)
    } else {
        std::collections::BTreeSet::new()
    };
    for candidate in 0..plan.nodes.len() {
        let contained_member = contained.contains(&candidate);
        let same_round_descendant = same_expanded_round(
            plan.nodes[candidate].round.as_ref(),
            plan.nodes[node_index].round.as_ref(),
        ) && node_depends_on(candidate, rebase, plan, positions);
        if candidate != rebase && (contained_member || same_round_descendant) {
            reset_skipped_conflict_branch(candidate, checkpoint)?;
        }
    }

    let conflict = &mut checkpoint.nodes[conflict_index];
    let prior_conflict = conflict.conflict.as_ref();
    let ci_rechecks = prior_conflict
        .filter(|evidence| evidence.head_sha.as_deref() == Some(head.as_str()))
        .map_or(0, |evidence| evidence.ci_rechecks)
        .saturating_add(1);
    let pending_observations = prior_conflict
        .map_or(0, |evidence| evidence.pending_observations)
        .saturating_add(1);
    conflict.state = NodeState::Running;
    conflict.wait_reason = None;
    conflict.gate = None;
    conflict.outputs = None;
    conflict.conflict = Some(ci::ConflictEvidence {
        state: ConflictState::Pending,
        head_sha: Some(head),
        summary: "rechecking mergeability after CI reported conflict".into(),
        wake_at: Some(wall_now.unwrap_or_else(|| {
            provenant_core::WallTime::from_micros(0).expect("epoch is a representable wall time")
        })),
        pending_observations,
        ci_rechecks,
    });

    let ci = &mut checkpoint.nodes[node_index];
    ci.state = NodeState::Pending;
    ci.wait_reason = None;
    ci.gate = None;
    ci.outputs = None;
    ci.ci = None;
    Ok((conflict_index, rebase))
}

fn advance_gate<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    worktree_layout: &provenant_config::WorktreeLayoutPort,
    effective_budget: &crate::config::GraphBudget,
    node_index: usize,
    condition: &GateCondition,
    checkpoint: &mut GraphCheckpoint,
    expanded: &mut GraphDocument,
    plan: &mut PlannedGraph,
    positions: &mut std::collections::BTreeMap<String, usize>,
    observed_ci: &mut std::collections::BTreeSet<String>,
) -> Result<GateStep, GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[node_index];
    let mut replan_error = None;
    let reported = repair::growth_in_round(node, &*plan, &*checkpoint, &*positions);
    let mut growth = reported;
    if repair::is_repair_gate(node, &*plan, &*positions) {
        if !reported && repair::risk_assessment_missing(node, &*plan, &*checkpoint) {
            replan_error = Some(
                "cannot assess repair growth: delta reviewer omitted a valid risk tier".into(),
            );
        }
        if let Some((before, after)) = repair::repair_commits(node, &*checkpoint)
            .map(|(before, after)| (before.to_owned(), after.to_owned()))
        {
            if let Some(value) =
                repair::cached_growth(&checkpoint.nodes[node_index], &before, &after)
            {
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
                        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
                        growth |= value;
                    }
                    Err(error) if !reported => {
                        replan_error = Some(format!("cannot assess repair growth: {error}"))
                    }
                    Err(_) => {}
                }
            }
        } else if document.schema_version >= 2 && !reported {
            replan_error = Some("cannot assess repair growth: pinned commits unavailable".into());
        }
    }
    if growth {
        if let Some((parent, commit)) = repair::replan_source(node, &*checkpoint)
            .map(|(parent, commit)| (parent.to_owned(), commit.to_owned()))
        {
            if repair::panel_expansion(&*checkpoint, &parent).is_none() {
                let result = (|| -> Result<_, String> {
                    commit
                        .parse::<provenant_core::GitOid>()
                        .map_err(|e| e.to_string())?;
                    let panel = owner.resolve_repair_panel(document, &commit)?;
                    let single = panel.slots.len() == 1;
                    // Repair input needs checked dispositions even for
                    // a single lens. Reuse the existing synthesis step.
                    let verifiers = if single { 0 } else { panel.verifiers };
                    let stages = when::review_stages(panel.frame && !single, verifiers > 0, true);
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
                        fields.insert("distinct_from".into(), "dependencies".into());
                        fields.insert("distinct_strength".into(), "prefer".into());
                        fields.insert("require_findings".into(), true.into());
                        fields.insert("required_verdict".into(), "VERDICT: PASS".into());
                        routes.push(
                            serde_json::to_string(&fields).map_err(|error| error.to_string())?,
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
                            vec![panel.advice.get(&lens).cloned().unwrap_or_default()],
                        );
                    }
                    let library = template::TemplateLibrary::bundled();
                    let context = patch::PatchContext {
                        catalogue: None,
                        library: &library,
                        resolvers: &resolvers,
                        journal: None,
                        limits,
                        worktree_layout: worktree_layout.clone(),
                    };
                    let mut prospective = checkpoint.clone();
                    let change =
                        repair::panel_patch(&parent, &commit, patch::revision(&prospective));
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
                        *checkpoint = prospective;
                        (*expanded, *plan, *positions) = (preview, new_plan, new_positions);
                        return Ok(GateStep::Reschedule);
                    }
                    Err(error) => {
                        replan_error = Some(format!("full panel replan required: {error}"))
                    }
                }
            } else if let Some(panel) = repair::panel_expansion(&*checkpoint, &parent) {
                if !repair::panel_reviews_commit(panel, &commit) {
                    replan_error = Some("full panel reviewed a different repaired commit; retry the repair container".into());
                } else if repair::panel_pending(&*checkpoint, panel) {
                    return Ok(GateStep::Next);
                } else {
                    replan_error = repair::panel_failure(&*checkpoint, panel);
                }
            }
        } else {
            replan_error = Some("full panel replan required: repaired commit unavailable".into());
        }
    }
    let gate_was_evaluated = !growth && replan_error.is_none();
    let reason = if growth || replan_error.is_some() {
        replan_error
    } else if matches!(
        condition,
        GateCondition::Ci { .. } | GateCondition::Conflict { .. }
    ) {
        if observed_ci.contains(&node.id) {
            return Ok(GateStep::Next);
        }
        let wall_now = owner.wall_now();
        let wake_at = if matches!(condition, GateCondition::Conflict { .. }) {
            checkpoint.nodes[node_index]
                .conflict
                .as_ref()
                .and_then(|conflict| conflict.wake_at)
        } else {
            checkpoint.nodes[node_index]
                .ci
                .as_ref()
                .and_then(|ci| ci.wake_at)
        };
        if owner.respect_ci_wake()
            && wake_at.is_some_and(|wake| wall_now.is_none_or(|now| now < wake))
        {
            return Ok(GateStep::Next);
        }
        let backoff = owner.ci_poll_backoff(&node.id);
        observed_ci.insert(node.id.clone());
        let mut evaluation = if matches!(condition, GateCondition::Conflict { .. }) {
            ci::evaluate_conflict(
                plan.queue.is_some(),
                node_index,
                node,
                &mut *checkpoint,
                owner,
                wall_now,
                backoff,
            )
        } else {
            ci::evaluate(
                plan.queue.is_some(),
                node_index,
                node,
                &mut *checkpoint,
                owner,
                wall_now,
                backoff,
            )
        }
        .map_err(GraphError::Refused)?;
        if plan.queue.is_some()
            && matches!(condition, GateCondition::Ci { .. })
            && checkpoint.nodes[node_index]
                .ci
                .as_ref()
                .is_some_and(|evidence| evidence.state == CiState::Conflict)
            && queue_conflict_recheck_already_observed(
                node_index, condition, plan, positions, checkpoint,
            )
        {
            let timeout_seconds = match condition {
                GateCondition::Ci {
                    timeout_seconds, ..
                } => *timeout_seconds,
                _ => unreachable!(),
            };
            let timed_out = checkpoint.nodes[node_index]
                .ci
                .as_ref()
                .is_some_and(|evidence| evidence.elapsed_seconds >= timeout_seconds);
            let evidence = checkpoint.nodes[node_index].ci.as_mut().ok_or_else(|| {
                GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "CI conflict recheck must retain its observation",
                }
            })?;
            evidence.state = CiState::Pending;
            evidence.summary = "CI still reports conflict after the exact published head was observed clear; waiting for CI to reconcile".into();
            if timed_out {
                evidence.wake_at = None;
                evaluation = ci::Evaluation::ParkUnavailable;
            } else {
                let now = wall_now.ok_or_else(|| {
                    GraphError::Refused(format!(
                        "node {}: no wall clock to set its CI conflict-reconciliation wake deadline",
                        node.id
                    ))
                })?;
                let delay_micros = backoff
                    .delay_seconds(evidence.pending_observations)
                    .saturating_mul(1_000_000);
                let wake_at = now
                    .checked_add(provenant_core::DurationMicros::from_micros(delay_micros))
                    .map_err(|_| {
                        GraphError::Refused(format!(
                            "node {}: CI conflict-reconciliation wake deadline exceeds representable time",
                            node.id
                        ))
                    })?;
                evidence.wake_at = Some(wake_at);
                evidence.pending_observations = evidence.pending_observations.saturating_add(1);
                checkpoint.nodes[node_index].state = NodeState::Running;
                evaluation = ci::Evaluation::Pending;
            }
        }
        match evaluation {
            ci::Evaluation::Pending => {
                asks::refresh_wake(&mut *checkpoint);
                journal.save(&*checkpoint).map_err(GraphError::Journal)?;
                return Ok(GateStep::Next);
            }
            ci::Evaluation::ParkUnavailable => {
                let ci = checkpoint.nodes[node_index]
                    .ci
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| GraphError::BadCheckpoint {
                        node: node.id.clone(),
                        invariant: "timed-out queue CI must retain its observation",
                    })?;
                let writer = loops::nearest_writer_ancestor(node_index, plan)
                    .and_then(|index| checkpoint.nodes.get(index));
                let attempt = writer.and_then(|progress| {
                    progress
                        .attempts
                        .iter()
                        .rev()
                        .find(|attempt| attempt.state == AttemptState::Terminal)
                        .cloned()
                });
                let handoff = writer
                    .and_then(|progress| progress.handoff.as_ref())
                    .map(|handoff| handoff.note.clone())
                    .unwrap_or_else(|| {
                        "required CI did not settle before its bounded timeout".into()
                    });
                let mut evidence = vec![ci.summary.clone()];
                evidence.push(format!("elapsed seconds: {}", ci.elapsed_seconds));
                evidence.extend(
                    ci.check_names
                        .iter()
                        .map(|name| format!("observed check: {name}")),
                );
                park_delivery_transition(
                    document,
                    node_index,
                    provenant_records::graph::ParkReason::CiUnavailable,
                    attempt.as_ref().map(|attempt| &attempt.key),
                    attempt
                        .as_ref()
                        .and_then(|attempt| attempt.attempt_ref.as_deref()),
                    &handoff,
                    ci.head_sha.as_deref(),
                    &evidence,
                    owner,
                    journal,
                    checkpoint,
                )?;
                asks::refresh_wake(&mut *checkpoint);
                return Ok(GateStep::Next);
            }
            ci::Evaluation::Settled(reason) => reason,
        }
    } else {
        gate_failure(
            condition,
            node,
            &*plan,
            &*checkpoint,
            &*positions,
            effective_budget,
        )
    };
    if plan.queue.is_some()
        && matches!(condition, GateCondition::Ci { .. })
        && checkpoint.nodes[node_index]
            .ci
            .as_ref()
            .is_some_and(|evidence| evidence.state == CiState::Conflict)
    {
        queue_conflict_recheck(
            node_index,
            condition,
            plan,
            positions,
            checkpoint,
            owner.wall_now(),
        )
        .map_err(|_| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "queue CI conflict must re-enter its exact published conflict gate",
        })?;
        asks::refresh_wake(checkpoint);
        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
        return Ok(GateStep::Reschedule);
    }
    let exact_queue_ci_red = gate_was_evaluated
        && plan.queue.is_some()
        && reason.is_some()
        && matches!(condition, GateCondition::Ci { .. })
        && match condition {
            GateCondition::Ci { target, .. } => checkpoint.nodes[node_index]
                .ci
                .as_ref()
                .is_some_and(|evidence| {
                    evidence.state == CiState::Failed
                        && !evidence.failed_checks.is_empty()
                        && ci::published_head(target, checkpoint, true).as_deref()
                            == evidence.head_sha.as_deref()
                }),
            _ => false,
        };
    let failed_delivery_review = gate_was_evaluated
        && plan.queue.is_some()
        && reason.is_some()
        && delivery_review_gate_failed(condition, node, plan, checkpoint, positions);
    let escalation_cause = if exact_queue_ci_red {
        Some(provenant_records::graph::EscalationCause::CiRed)
    } else if failed_delivery_review {
        Some(provenant_records::graph::EscalationCause::ReviewRepair)
    } else {
        None
    };
    let escalation = escalation_cause.and_then(|cause| {
        transitions::advance_delivery_escalation(node_index, plan, checkpoint, cause)
    });
    apply_gate_result(
        &mut checkpoint.nodes[node_index],
        reason,
        node.on_fail.clone(),
        matches!(condition, GateCondition::Ci { .. }),
        plan.queue.is_some(),
    );
    if let Some(conflict) = checkpoint.nodes[node_index].conflict.as_mut() {
        conflict.wake_at = None;
    }
    asks::refresh_wake(&mut *checkpoint);
    if matches!(
        escalation,
        Some(transitions::EscalationTransition::Exhausted { .. })
    ) {
        let (escalation_owner, _) =
            loops::escalation_owner(node_index, plan).ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "exhausted CI escalation must retain its owner policy",
            })?;
        let attempt_key = checkpoint.nodes[escalation_owner]
            .escalation
            .as_ref()
            .map(|state| state.transition_attempt.clone())
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "exhausted CI escalation must retain its repair identity",
            })?;
        let repair_progress = checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == attempt_key.node_id);
        let attempt_ref = repair_progress.and_then(|progress| {
            progress
                .attempts
                .iter()
                .find(|attempt| attempt.key == attempt_key)
                .and_then(|attempt| attempt.attempt_ref.clone())
        });
        let handoff = repair_progress
            .and_then(|progress| progress.handoff.as_ref())
            .map(|handoff| handoff.note.clone())
            .unwrap_or_else(|| "delivery repair ladder exhausted".into());
        let cause = escalation_cause.ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "exhausted escalation must retain its cause",
        })?;
        let (head, mut evidence) = match cause {
            provenant_records::graph::EscalationCause::CiRed => {
                let head = match condition {
                    GateCondition::Ci { target, .. } => {
                        ci::published_head(target, checkpoint, true)
                    }
                    _ => None,
                }
                .ok_or_else(|| GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "exhausted CI escalation must retain its published head",
                })?;
                let mut evidence = checkpoint.nodes[node_index]
                    .ci
                    .as_ref()
                    .map(|ci| vec![ci.summary.clone()])
                    .unwrap_or_default();
                if let Some(ci) = &checkpoint.nodes[node_index].ci {
                    evidence.extend(ci.failed_checks.iter().map(|check| {
                        format!(
                            "job {}: {} {} {}",
                            check.job_id.as_deref().unwrap_or("unknown"),
                            check.name,
                            check.url.as_deref().unwrap_or(""),
                            check.excerpt
                        )
                    }));
                }
                (Some(head), evidence)
            }
            provenant_records::graph::EscalationCause::ReviewRepair => {
                let head = node.inputs.iter().find_map(|input| {
                    checkpoint.nodes[positions[&input.node]]
                        .source_commit
                        .clone()
                });
                let mut evidence = vec!["delivery review did not approve the repair".into()];
                for input in &node.inputs {
                    if let Some(outputs) = checkpoint.nodes[positions[&input.node]].outputs.as_ref()
                    {
                        if let Some(verdict) = outputs.verdict.as_deref() {
                            evidence.push(format!("{} verdict: {verdict}", input.node));
                        }
                        evidence.extend(outputs.findings.iter().map(|finding| {
                            format!(
                                "{} finding {}: {} ({})",
                                input.node, finding.id, finding.summary, finding.severity
                            )
                        }));
                    }
                }
                (head, evidence)
            }
            _ => {
                return Err(GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "unsupported escalation cause reached ladder exhaustion",
                });
            }
        };
        evidence.push(format!("escalation cause: {cause:?}"));
        park_delivery_transition(
            document,
            node_index,
            provenant_records::graph::ParkReason::LadderExhausted,
            Some(&attempt_key),
            attempt_ref.as_deref(),
            &handoff,
            head.as_deref(),
            &evidence,
            owner,
            journal,
            checkpoint,
        )?;
        return Ok(GateStep::Next);
    }
    journal.save(&*checkpoint).map_err(GraphError::Journal)?;
    Ok(GateStep::Next)
}

/// How an attempt-loop phase leaves the node's attempt loop.
enum LoopStep {
    /// Carry on with the next phase of this attempt.
    Proceed,
    /// Leave the node's attempt loop and move to the next node.
    Break,
    /// The checkpoint was replaced; start the scheduling pass again.
    Reschedule,
}

/// Requests and materialised files staged for an attempt before it is recorded.
type StagedInputs = (Vec<InputRequest>, Option<MaterialisedInputs>);

/// Result of admitting a node's next attempt.
enum Staged {
    /// The node cannot launch now; leave its attempt loop.
    Wait,
    /// Admitted. Inputs are staged only when a new attempt record was begun.
    Ready(Option<StagedInputs>),
}

/// The attempt content the launch reads: its prompt and recorded identity.
struct AttemptInput {
    stored_ref: Option<String>,
    launch_actor: String,
    prompt: String,
    materialised: Option<MaterialisedInputs>,
}

/// Family separation facts for one attempt.
struct AttemptSeparation {
    distinct_attempts: Vec<String>,
    review_retry: bool,
    require_distinct: bool,
    family_peers: Vec<String>,
    prior_attempts: Vec<String>,
}

/// Settle a writer failure that happens before any attempt is recorded. A
/// transient contention gets the same bounded retry as a launch; every other
/// preparation failure fails the node now, so `wait` and `status` observe a
/// terminal node instead of one that looks live until the wait budget expires.
fn settle_writer_binding_failure<R: AttemptOwner, J: GraphJournal>(
    checkpoint: &mut GraphCheckpoint,
    journal: &mut J,
    node_index: usize,
    node_id: &str,
    owner: &R,
    error: &R::Error,
) -> Result<LoopStep, GraphError<R::Error, J::Error>> {
    if defer_launch_contention(
        checkpoint,
        node_index,
        error,
        owner.wall_now(),
        owner.admission_timeout_seconds().unwrap_or(3600),
        owner.launch_retry_policy(),
    )
    .map_err(GraphError::Refused)?
    {
        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
        return Ok(LoopStep::Break);
    }
    transitions::writer_binding_failed(
        &mut checkpoint.nodes[node_index],
        node_id,
        &next_attempt::writer_binding_reason(node_id, error),
    );
    checkpoint.nodes[node_index].advance_owner_code =
        Some(next_attempt::launch_diagnostic(error).code);
    journal.save(&*checkpoint).map_err(GraphError::Journal)?;
    Ok(LoopStep::Break)
}

/// Bind an expanded writer's worktree before its first attempt. Each phase of
/// the attempt loop has its own frame so a debug build does not pool their
/// temporaries into one large frame (#3542).
#[allow(
    clippy::too_many_arguments,
    reason = "one admitted execution state follows internal rescheduling"
)]
fn bind_expanded_writer<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    node_index: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    resume: &mut bool,
) -> Result<LoopStep, GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[node_index];
    // A row settled as skipped by the scheduler takes no writer effect. The
    // checkpoint state is authoritative: re-reading the plan predicate would
    // let a replaced, false-`when` writer launch unbound (#3899).
    if checkpoint.nodes[node_index].state == NodeState::Skipped {
        return Ok(LoopStep::Proceed);
    }
    // A pathless writer with peer provisioning intent is bound by the receiving
    // host, not by this scheduler's local worktree owner.
    if validation::peer_owned_writer(node.access, node.worktree.as_deref(), node.attempt.as_ref()) {
        return Ok(LoopStep::Proceed);
    }
    if plans::writer(node)
        && checkpoint.nodes[node_index].writer_binding.is_none()
        && checkpoint.nodes[node_index].attempts.is_empty()
    {
        if apply_pending_cancellation(document, &mut *checkpoint, journal, resume)? {
            return Ok(LoopStep::Reschedule);
        }
        let path = node
            .worktree
            .as_deref()
            .ok_or_else(|| GraphError::Invalid("writer worktree missing".into()))?;
        if checkpoint.nodes[node_index].provisioned_owner.is_none() {
            let inherited = checkpoint
                .nodes
                .iter()
                .filter(|row| row.worktree.as_deref() == Some(path))
                .find_map(|row| row.provisioned_owner);
            checkpoint.nodes[node_index].provisioned_owner = inherited;
        }
        if node.provision_worktree
            && (relay::pending_provisioning(&*checkpoint)
                .iter()
                .any(|(id, _, _)| id == &node.id)
                || !checkpoint.nodes.iter().any(|progress| {
                    progress.worktree.as_deref() == Some(path) && progress.writer_binding.is_some()
                }))
        {
            let pending = relay::pending_provisioning(&*checkpoint)
                .into_iter()
                .find(|(id, _, _)| id == &node.id);
            let source = if let Some((_, _, source)) = pending {
                source
            } else {
                let source = if let Some(source) = checkpoint.nodes[node_index]
                    .source_commit
                    .clone()
                    .or_else(|| checkpoint.nodes[node_index].provisioned_source.clone())
                {
                    Some(source)
                } else {
                    match &node.source {
                        Some(SourceBinding::Commit { commit }) => Some(commit.clone()),
                        Some(SourceBinding::Node { node: source }) => positions
                            .get(source)
                            .and_then(|index| checkpoint.nodes[*index].outputs.as_ref())
                            .and_then(|outputs| outputs.commit.clone()),
                        Some(SourceBinding::Head { node: writer }) => {
                            land::chain_head(plan, &*checkpoint, positions, &node.id, writer)
                        }
                        Some(SourceBinding::Integration) => {
                            owner.integration_head().map_err(GraphError::Run)?
                        }
                        None => {
                            let bound = checkpoint
                                .bases
                                .and_then(|bases| bases.integration)
                                .map(|base| base.to_string());
                            match bound {
                                Some(source) => Some(source),
                                None => {
                                    if apply_pending_cancellation(
                                        document,
                                        &mut *checkpoint,
                                        journal,
                                        resume,
                                    )? {
                                        return Ok(LoopStep::Reschedule);
                                    }
                                    owner
                                        .bind_writer_worktree(".")
                                        .map_err(GraphError::Run)?
                                        .map(|binding| binding.head)
                                }
                            }
                        }
                    }
                }
                .ok_or_else(|| {
                    GraphError::Invalid("writer provisioning source unavailable".into())
                })?;
                transitions::record_provisioning(node, &mut *checkpoint, node_index, path, &source);
                journal.save(&*checkpoint).map_err(GraphError::Journal)?;
                source
            };
            if apply_pending_cancellation(document, &mut *checkpoint, journal, resume)? {
                return Ok(LoopStep::Reschedule);
            }
            let creation_owner = checkpoint.nodes[node_index].provisioned_owner.or_else(|| {
                checkpoint
                    .nodes
                    .iter()
                    .filter(|row| row.worktree.as_deref() == Some(path))
                    .find_map(|row| row.provisioned_owner)
            });
            checkpoint.nodes[node_index].provisioned_owner = creation_owner;
            match owner.provision_child_worktree(path, &source, creation_owner) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(GraphError::Refused(
                        "host cannot provision writer worktrees".into(),
                    ));
                }
                Err(error) => {
                    if defer_launch_contention(
                        &mut *checkpoint,
                        node_index,
                        &error,
                        owner.wall_now(),
                        owner.admission_timeout_seconds().unwrap_or(3600),
                        owner.launch_retry_policy(),
                    )
                    .map_err(GraphError::Refused)?
                    {
                        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
                        return Ok(LoopStep::Break);
                    }
                    return Err(GraphError::Run(error));
                }
            }
        }
        if apply_pending_cancellation(document, &mut *checkpoint, journal, resume)? {
            return Ok(LoopStep::Reschedule);
        }
        let binding_result = if let Some(spec) = node
            .attempt
            .as_ref()
            .and_then(|spec| spec.new_worktree.as_ref())
        {
            // Retain the resolved base before any Git effect. Recovery
            // adopts only the clean worktree at this exact commit.
            let base = match checkpoint.nodes[node_index].new_worktree_base.as_deref() {
                Some(base) => base.to_owned(),
                None => {
                    // A failed base resolution used to `?` straight out of this
                    // function. The node then stayed live with no attempt row,
                    // so `wait` reported "still running at timeout" for a launch
                    // that had already failed to prepare (#3748). Settle it here.
                    let base = match owner.resolve_creation_base(
                        &spec.branch,
                        &spec.base,
                        spec.default_base,
                    ) {
                        Ok(Some(base)) if base.parse::<provenant_core::GitOid>().is_ok() => base,
                        Ok(_) => {
                            transitions::fail_with(
                                &mut checkpoint.nodes[node_index],
                                "writer_base_unresolvable",
                                format!("node {}: writer base could not be resolved", node.id),
                            );
                            journal.save(&*checkpoint).map_err(GraphError::Journal)?;
                            return Ok(LoopStep::Break);
                        }
                        Err(error) => {
                            return settle_writer_binding_failure(
                                &mut *checkpoint,
                                journal,
                                node_index,
                                &node.id,
                                owner,
                                &error,
                            );
                        }
                    };
                    transitions::record_creation_base(&mut checkpoint.nodes[node_index], &base);
                    journal.save(&*checkpoint).map_err(GraphError::Journal)?;
                    base
                }
            };
            let creation_owner = checkpoint.nodes[node_index].provisioned_owner.or_else(|| {
                checkpoint
                    .nodes
                    .iter()
                    .filter(|row| row.worktree.as_deref() == Some(path))
                    .find_map(|row| row.provisioned_owner)
            });
            checkpoint.nodes[node_index].provisioned_owner = creation_owner;
            owner.provision_writer_worktree(path, &spec.branch, &base, creation_owner)
        } else {
            owner.bind_writer_worktree(path)
        };
        let binding = match binding_result {
            Ok(binding) => binding,
            Err(error) => {
                return settle_writer_binding_failure(
                    &mut *checkpoint,
                    journal,
                    node_index,
                    &node.id,
                    owner,
                    &error,
                );
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
        relay::clear_provisioning(&mut *checkpoint, &node.id);
        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
    }
    Ok(LoopStep::Proceed)
}

/// Admit the node's next attempt and stage its inputs.
#[allow(
    clippy::too_many_arguments,
    reason = "one admitted execution state follows internal rescheduling"
)]
fn stage_attempt<R: AttemptOwner, J: GraphJournal>(
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    effective_budget: &crate::config::GraphBudget,
    admission_policy: &provenant_records::policy::Policy,
    node_index: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    key: &AttemptKey,
    route: &GraphRoute,
) -> Result<Staged, GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[node_index];
    let cap = limits.node_prompt_bytes;
    match transitions::admit_attempt(
        node_index,
        plan,
        &mut *checkpoint,
        positions,
        limits,
        effective_budget,
        // Admission checks are graph-wide, so a node's dispatch
        // overrides never apply to them.
        admission_policy,
    ) {
        transitions::Admission::Ready => {}
        transitions::Admission::PolicyRefused { code, detail } => {
            // The refused node is terminal; it has no next attempt.
            transitions::fail_with(&mut checkpoint.nodes[node_index], code, detail);
            journal.save(&*checkpoint).map_err(GraphError::Journal)?;
            return Ok(Staged::Wait);
        }
        transitions::Admission::SaveAndWait => {
            journal.save(&*checkpoint).map_err(GraphError::Journal)?;
            return Ok(Staged::Wait);
        }
    }
    if node.source.is_some() && checkpoint.nodes[node_index].source_commit.is_none() {
        let commit = match node.source.as_ref() {
            Some(SourceBinding::Commit { commit }) => Some(commit.clone()),
            Some(SourceBinding::Node { node: source }) => positions
                .get(source)
                .and_then(|index| checkpoint.nodes[*index].outputs.as_ref())
                .and_then(|outputs| outputs.commit.clone()),
            Some(SourceBinding::Head { node: writer }) => {
                land::chain_head(plan, &*checkpoint, positions, &node.id, writer)
            }
            Some(SourceBinding::Integration) => {
                owner.integration_head().map_err(GraphError::Run)?
            }
            None => None,
        };
        let valid = transitions::apply_source_commit(node, &mut *checkpoint, node_index, commit);
        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
        if !valid {
            return Ok(Staged::Wait);
        }
    }
    if node.access == AccessMode::ReadOnly && template::review_node(node) {
        let progress = &checkpoint.nodes[node_index];
        let graph_review_base = checkpoint
            .bases
            .and_then(|bases| bases.review)
            .map(|base| base.to_string());
        let base = progress.review_base.as_ref().or(graph_review_base.as_ref());
        if base
            .zip(progress.source_commit.as_ref())
            .is_some_and(|(base, head)| base == head)
        {
            transitions::fail_with(
                &mut checkpoint.nodes[node_index],
                "review_empty_range",
                "review range is empty; select a subject with changes".into(),
            );
            journal.save(&*checkpoint).map_err(GraphError::Journal)?;
            return Ok(Staged::Wait);
        }
    }
    let mut integration_valid = true;
    for expected in transitions::integration_expectations(node_index, plan, &*checkpoint) {
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
        || !transitions::continuation_ready(node, node_index, &mut *checkpoint, positions)
    {
        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
        return Ok(Staged::Wait);
    }
    // File pointers are part of the prompt cap too. Stage them
    // before recording an attempt so a delivery that cannot fit
    // fails the consumer without launching it.
    let requests = upstream_results(node, plan, &*checkpoint, positions);
    let materialised = if requests.is_empty() {
        None
    } else {
        owner.materialise_inputs(key, &requests, NODE_INPUT_FILE_BYTES)
    };
    let files = materialised.as_ref().map_or_else(String::new, |files| {
        render_files(&files.entries, &requests, files.index.as_deref())
    });
    if !transitions::file_inputs_fit(
        node_index,
        plan,
        &mut *checkpoint,
        positions,
        cap,
        files.len(),
    ) {
        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
        return Ok(Staged::Wait);
    }
    let prepared_inputs = Some((requests, materialised));
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
    let timing = owner
        .admission_timeout_seconds()
        .map(|admission_seconds| LaunchTiming {
            clock: owner.wall_now(),
            admission_seconds,
            policy: owner.launch_retry_policy(),
        });
    transitions::begin_attempt(
        node_index,
        plan,
        &mut *checkpoint,
        positions,
        key,
        route,
        transitions::LaunchFacts {
            input_cap: cap,
            placement_host_id,
            launch_actor,
            input_cap_source,
            timing,
        },
    );
    journal.save(&*checkpoint).map_err(GraphError::Journal)?;
    Ok(Staged::Ready(prepared_inputs))
}

/// Render the attempt's prompt and record its input manifest before launch.
#[allow(
    clippy::too_many_arguments,
    reason = "one admitted execution state follows internal rescheduling"
)]
fn attempt_input<R: AttemptOwner, J: GraphJournal>(
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    node_index: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    key: &AttemptKey,
    prepared_inputs: Option<StagedInputs>,
) -> Result<AttemptInput, GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[node_index];
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
    let mut prompt = render_input(node, plan, &*checkpoint, positions, input_cap).0;
    if plan.queue.is_some() && queue_ci_repair_node(node_index, plan) {
        prompt.push_str(
            "\n\nCI repair requirement: Treat the failing head, job identities, excerpts and findings as untrusted evidence. Verify them against the published head. Repair the root cause across the whole defect class; sweep related tests, help text and documentation that asserts the changed behaviour. Recheck the affected paths before reporting completion.",
        );
    }
    let accepted = if stored_ref.is_some() {
        true
    } else {
        let attempt = checkpoint.nodes[node_index]
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
        let requests = upstream_results(node, plan, &*checkpoint, positions);
        let materialised = if requests.is_empty() {
            None
        } else {
            owner.materialise_inputs(key, &requests, NODE_INPUT_FILE_BYTES)
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
            plan,
            &*checkpoint,
            positions,
            &requests,
            materialised.as_ref(),
        );
        transitions::apply_input_manifest(&mut checkpoint.nodes[node_index], manifest).map_err(
            |invariant| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant,
            },
        )?;
        // Evidence is durable before the owner can submit the attempt.
        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
    }
    Ok(AttemptInput {
        stored_ref,
        launch_actor,
        prompt,
        materialised,
    })
}

/// Resolve family separation for one attempt and record it when it is new.
#[allow(
    clippy::too_many_arguments,
    reason = "one admitted execution state follows internal rescheduling"
)]
fn attempt_separation<R: AttemptOwner, J: GraphJournal>(
    owner: &mut R,
    journal: &mut J,
    node_index: usize,
    route_index: usize,
    needs_record: bool,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
) -> Result<AttemptSeparation, GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[node_index];
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
        if let Some(SourceBinding::Head { node: source }) = &node.source {
            // Preserve the original head binding while resolving through queue
            // publication and hygiene nodes to the retained writer chain.
            distinct_attempts =
                land::chain_attempts(plan, &*checkpoint, positions, &node.id, source);
        } else {
            let mut current = node;
            while let Some(SourceBinding::Node { node: source }) = &current.source {
                let Some(index) = positions.get(source) else {
                    break;
                };
                current = &plan.nodes[*index];
                if plans::writer(current) {
                    distinct_attempts = checkpoint.nodes[*index]
                        .attempts
                        .iter()
                        .filter_map(|attempt| attempt.attempt_ref.clone())
                        .collect();
                    break;
                }
            }
        }
    }
    let parent = plan
        .nodes
        .iter()
        .find(|candidate| candidate.composite_members.contains(&node.id))
        .map(|candidate| &candidate.id);
    let peer_refs: Vec<String> = node.family_group.as_ref().map_or_else(Vec::new, |group| {
        plan.nodes
            .iter()
            .enumerate()
            .filter(|(index, peer)| {
                let peer_parent = plan
                    .nodes
                    .iter()
                    .find(|candidate| candidate.composite_members.contains(&peer.id))
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
                .saturating_sub(checkpoint.nodes[node_index].quota_redraw_archived as usize),
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
        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
    }
    Ok(AttemptSeparation {
        distinct_attempts,
        review_retry,
        require_distinct,
        family_peers,
        prior_attempts,
    })
}

/// What recording an observed attempt leaves for the attempt loop.
enum Observed {
    /// A continuation attempt was recorded; observe it next.
    NextAttempt,
    /// The observation was applied to the checkpoint.
    Applied {
        settled_result: Option<Box<crate::result_capture::CapturedResult>>,
        should_continue: bool,
        quota_redraw: bool,
    },
}

/// Launch or observe the attempt through the owner. `None` means the launch
/// failed while starting and its disposition is saved; leave the attempt loop.
#[allow(
    clippy::too_many_arguments,
    reason = "one admitted execution state follows internal rescheduling"
)]
fn observe_attempt<R: AttemptOwner, J: GraphJournal>(
    owner: &mut R,
    journal: &mut J,
    node_index: usize,
    settlement_retries: Option<u8>,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    key: &AttemptKey,
    route: &GraphRoute,
    separation: &AttemptSeparation,
    input: &AttemptInput,
    continued: Option<&str>,
) -> Result<Option<AttemptObservation>, GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[node_index];
    let AttemptSeparation {
        distinct_attempts,
        require_distinct,
        family_peers,
        prior_attempts,
        ..
    } = separation;
    let AttemptInput {
        stored_ref,
        launch_actor,
        prompt,
        materialised,
    } = input;
    let require_distinct = *require_distinct;
    let mut launch_attempt = node.attempt.clone().unwrap_or_default();
    if !node.write_scope.is_empty() {
        launch_attempt.write_scope = node.write_scope.clone();
    }
    let observation = match owner.observe_or_start(
        key,
        NodeInput {
            writer_dirty_preflight: node.writer_dirty_preflight,
            review_launch: node.review_launch,
            require_skill_location: node.require_skill_location,
            settlement_retries,
            prior_attempts,
            family_peers,
            context: provenant_records::run::WorkContext {
                tier: route.tier.clone().or_else(|| route.alias.clone()),
                delivery_queue_writer_adoption: plan.queue.is_some()
                    && node.access == AccessMode::WorktreeWrite
                    && checkpoint.nodes[node_index].writer_binding.is_some(),
                // The work key's attempt: one-based launch order
                // within the node, never reused.
                attempt: Some(key.attempt),
                ..node_context(plan, node)
            },
            read_roots: &node.read_roots,
            attachments: &node.attachments,
            worktree: node.worktree.as_deref(),
            fixture: node.fixture.as_deref(),
            fixture_bytes: node.fixture_bytes.as_ref(),
            capabilities: node.capabilities.as_deref(),
            capability_sources: (!node.capability_sources.is_empty())
                .then_some(&node.capability_sources),
            attempt: Some(&launch_attempt),
            distinct_from_attempts: distinct_attempts,
            require_distinct,
            source_commit: checkpoint.nodes[node_index].source_commit.as_deref(),
            review_base: checkpoint.nodes[node_index].review_base.as_deref(),
            title: node.title.as_deref().unwrap_or(&node.id),
            prompt,
            required_verdict: node.required_verdict.as_deref(),
            deadline_seconds: node.deadline_seconds,
            inputs_dir: materialised
                .as_ref()
                .and_then(|inputs| inputs.directory.as_deref()),
        },
        route,
        node.access,
        if node.access == AccessMode::ReadOnly {
            source_worktree(node, plan, &*checkpoint, positions).or(node.worktree.as_deref())
        } else {
            checkpoint.nodes[node_index]
                .writer_binding
                .as_ref()
                .map(|binding| binding.worktree.as_str())
                .or(node.worktree.as_deref().filter(|path| !path.is_empty()))
        },
        Replay {
            delivery: land::delivery(node),
            branch: continued,
            fast_forward_only: node.round.is_some() || node.continues.is_some(),
            stored_ref: stored_ref.as_deref(),
            launch_actor,
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
                &mut *checkpoint,
                node_index,
                &error,
                LaunchTiming {
                    clock,
                    admission_seconds,
                    policy,
                },
                journal,
            )?;
            return Ok(None);
        }
        Err(error) => return Err(GraphError::Run(error)),
    };
    Ok(Some(observation))
}

/// Fold an observed attempt into the checkpoint: usage, fallback evidence,
/// family separation and review range, then the pure transition.
#[allow(
    clippy::too_many_arguments,
    reason = "one admitted execution state follows internal rescheduling"
)]
fn record_observed_attempt<R: AttemptOwner, J: GraphJournal>(
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    node_index: usize,
    route_index: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    key: &AttemptKey,
    route: &GraphRoute,
    observation: AttemptObservation,
    facts: &AttemptSeparation,
) -> Result<Observed, GraphError<R::Error, J::Error>> {
    let node = &plan.nodes[node_index];
    let AttemptSeparation {
        distinct_attempts,
        review_retry,
        require_distinct,
        family_peers,
        ..
    } = facts;
    let (review_retry, require_distinct) = (*review_retry, *require_distinct);
    let observed_reference = match &observation {
        AttemptObservation::Pending { attempt_ref }
        | AttemptObservation::Unknown { attempt_ref }
        | AttemptObservation::Terminal { attempt_ref, .. } => attempt_ref.clone(),
    };
    if let Some(reference) = observed_reference
        && let Some(last) = checkpoint.nodes[node_index].attempts.last_mut()
        && last.key == *key
        && last.attempt_ref.as_deref() != Some(reference.as_str())
    {
        last.attempt_ref = Some(reference);
        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
    }
    let warnings = owner.take_launch_warnings();
    transitions::apply_launch_metadata(&mut *checkpoint, node_index, warnings);
    let applied_route = match &observation {
        AttemptObservation::Pending { attempt_ref }
        | AttemptObservation::Unknown { attempt_ref }
        | AttemptObservation::Terminal { attempt_ref, .. } => attempt_ref
            .as_ref()
            .and_then(|reference| owner.applied_route(reference).ok().flatten()),
    };
    let applied_route = applied_route.map(|mut applied| {
        if applied.adapter.is_none() {
            applied.adapter = route.adapter.clone();
        }
        if applied.model.is_none() {
            applied.model = route.model.clone();
        }
        if applied.effort.is_none() {
            applied.effort = route.effort.clone();
        }
        if applied.account.is_none() {
            applied.account = route.account.clone();
        }
        if applied.required_family.is_none() {
            applied.required_family = route.required_family.clone();
        }
        if applied.peer_source_head.is_none() {
            applied.peer_source_head = route.peer_source_head.clone();
        }
        if applied.peer_requested_route.is_none() {
            applied.peer_requested_route = route.peer_requested_route.clone();
        }
        if applied.peer_network.is_none() {
            applied.peer_network = route.peer_network;
        }
        if applied.peer_retry_limit.is_none() {
            applied.peer_retry_limit = route.peer_retry_limit;
        }
        if route.name.is_some() {
            applied.name = route.name.clone();
        }
        if route.alias.is_some() {
            applied.alias = route.alias.clone();
        }
        if route.tier.is_some() {
            applied.tier = route.tier.clone();
        }
        if applied.account_key.is_none() {
            applied.account_key = route.account_key.clone();
        }
        if applied.source.is_none() {
            applied.source = route.source.clone();
        }
        if applied.pool_selection.is_none() {
            applied.pool_selection = route.pool_selection.clone();
        }
        for (key, value) in &route.extra {
            applied
                .extra
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        applied
    });
    transitions::apply_route(&mut checkpoint.nodes[node_index], applied_route);
    let (fallback_evidence, evidence_unreadable) = match &observation {
        AttemptObservation::Terminal {
            attempt_ref: Some(reference),
            launched: false,
            ..
        } => match owner.preparation_fallback_evidence(key, reference) {
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
            && !live_fallback_allowed(*outcome, false, fallback_evidence) =>
        {
            owner
                .preparation_refusal_cause(key, reference)
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
            .preparation_skipped_accounts(key, reference)
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let fallback_evidence_retry = match &observation {
        AttemptObservation::Terminal {
            attempt_ref: Some(reference),
            outcome,
            launched: false,
            ..
        } if fallback_candidate(*outcome, node.access, false) => {
            let review_limit_reached = next_attempt::review_member(node)
                && checkpoint.nodes[node_index]
                    .attempts
                    .iter()
                    .filter(|attempt| attempt.review_substitution)
                    .count()
                    >= limits.review_substitutions;
            let waitable = next_attempt::custody_release_pending(
                node,
                route_index,
                *outcome,
                false,
                next_step::FallbackEvidence {
                    preparation: fallback_evidence,
                    unreadable: evidence_unreadable,
                    review_limit_reached,
                    cause: fallback_cause.as_ref(),
                    skipped_accounts: &skipped_accounts,
                },
            );
            if waitable {
                owner.wall_now().and_then(|now| {
                    next_attempt::initial_fallback_evidence_retry(key, reference, now)
                })
            } else {
                None
            }
        }
        _ => None,
    };
    let quota_redraw = match &observation {
        AttemptObservation::Terminal {
            attempt_ref: Some(reference),
            outcome: AttemptOutcome::UsageLimited,
            ..
        } if route.alias.is_some() && route.model.is_none() => {
            quota_redraw_budget(&checkpoint.nodes[node_index])
                && owner
                    .quota_alias_redraw(key, reference)
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
    let steer_refused = if let AttemptObservation::Terminal {
        attempt_ref: Some(reference),
        ..
    } = &observation
    {
        owner
            .continuation_refused(reference)
            .map_err(GraphError::Run)?
    } else {
        false
    };
    // A finished task has no follow-up: adopting a successor after success
    // would hide the successful attempt behind a second, never-launched task
    // and turn a preparation refusal into the graph's outcome (#3987).
    if let AttemptObservation::Terminal {
        attempt_ref: Some(reference),
        outcome,
        ..
    } = &observation
        && *outcome != AttemptOutcome::Succeeded
        && !steer_refused
        && let Some(successor) = owner.continuation(reference).map_err(GraphError::Run)?
    {
        transitions::continue_attempt(&mut *checkpoint, node_index, observation, successor, usage)
            .map_err(|invariant| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant,
            })?;
        journal.save(&*checkpoint).map_err(GraphError::Journal)?;
        return Ok(Observed::NextAttempt);
    }
    let recorded_separation = match (&observation, node.distinct_from, &node.family_group) {
        (AttemptObservation::Terminal { attempt_ref, .. }, distinct, group)
            if distinct.is_some() || group.is_some() || review_retry =>
        {
            let mut excluded = owner
                .attempt_families(distinct_attempts)
                .unwrap_or_default();
            let enforced = require_distinct && !excluded.is_empty();
            if !enforced {
                for family in family_peers {
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
                .review_base
                .as_deref()
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
        plan,
        &mut *checkpoint,
        positions,
        node_index,
        limits,
        observation,
        transitions::ObservationFacts {
            route_index,
            usage,
            fallback_evidence,
            evidence_unreadable,
            fallback_evidence_retry,
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
        &mut *checkpoint,
        node,
        plan,
        positions,
        transitions::ObservationRecord {
            node_index,
            retry_was_pending,
            recorded_separation,
            usage_error,
            cost_error,
        },
    );
    if steer_refused {
        let reason = format!(
            "node {}: steer successor preparation was refused; correct the cause and retry the node",
            node.id
        );
        checkpoint.nodes[node_index].state = NodeState::Blocked;
        checkpoint.nodes[node_index].advance_error_code = Some("steer_continuation_refused".into());
        checkpoint.nodes[node_index].advance_error = Some(reason.clone());
        checkpoint.set_pause(
            provenant_records::pause::PauseCause::Stop(
                provenant_records::StopCause::LaunchHandoffRefused,
            ),
            &reason,
        );
    }
    Ok(Observed::Applied {
        settled_result,
        should_continue: should_continue && !steer_refused,
        quota_redraw: quota_redraw && !steer_refused,
    })
}

/// Effect driver for one scheduling pass. Kept separate from admission and
/// final settlement so a debug build does not pool this loop's temporaries into
/// a single large frame on the graph launch path (#3542).
#[allow(
    clippy::too_many_arguments,
    reason = "one admitted execution state follows internal rescheduling"
)]
fn advance_schedule<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    worktree_layout: provenant_config::WorktreeLayoutPort,
    admission_policy: provenant_records::policy::Policy,
    catalogue: Option<&provenant_routing::Catalogue>,
    effective_budget: crate::config::GraphBudget,
    resume_in: bool,
    checkpoint_in: GraphCheckpoint,
    expanded_in: GraphDocument,
    plan_in: PlannedGraph,
    positions_in: std::collections::BTreeMap<String, usize>,
    observed_ci_in: std::collections::BTreeSet<String>,
) -> Result<ScheduleExit, GraphError<R::Error, J::Error>> {
    let mut resume = resume_in;
    let mut checkpoint = checkpoint_in;
    let mut expanded = expanded_in;
    let mut plan = plan_in;
    let mut positions = positions_in;
    let mut observed_ci = observed_ci_in;
    let mut stopped_children = std::collections::BTreeSet::new();
    'schedule: loop {
        for node_index in 0..checkpoint.nodes.len() {
            let Some(park) = checkpoint.nodes[node_index].delivery_park.clone() else {
                continue;
            };
            park_delivery_transition(
                document,
                node_index,
                park.reason,
                park.attempt_key.as_ref(),
                park.attempt_ref.as_deref(),
                &park.handoff,
                park.head.as_deref(),
                &park.evidence,
                owner,
                journal,
                &mut checkpoint,
            )?;
        }
        if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
            continue 'schedule;
        }
        // Save the running graph's pause before any recovery patch is planned
        // against today's tighter limits.
        let structural_pause =
            transitions::refresh_structure_pause(&mut checkpoint, &expanded, &plan, limits);
        if structural_pause.is_some() {
            // Persist the durable pause before observing attempts. A resume
            // against tighter limits still owes every launched attempt its
            // settlement: returning here would leave a finished node running
            // forever. A resume under the pause admits no new launch (see
            // the guard in the attempt loop) but never stops observing work the host already
            // accepted; it survives while any node is live and clears when
            // the graph fully settles or limits are raised (#3558).
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }
        if structural_pause.is_none() {
            if let Some(next) =
                review::correct_verdict(document, &plan, &checkpoint, limits, &worktree_layout)
                    .map_err(|error| GraphError::Refused(error.to_string()))?
            {
                let next_expanded =
                    plans::expand(document, &next.extensions, &next.expansions, limits)
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
                settle_stopped_writer::<R, J>(
                    document,
                    owner,
                    &plan,
                    &checkpoint,
                    limits,
                    &worktree_layout,
                )?
            {
                // A stop saved by an earlier host, or one the chair resumed.
                journal.save(&next).map_err(GraphError::Journal)?;
                checkpoint = next;
                (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                resume = false;
                continue 'schedule;
            }
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
                || checkpoint
                    .nodes
                    .iter()
                    .any(|progress| progress.id == node.id && progress.detached_from_graph_cancel))
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
                                && node.plan.is_some()))
                })
            {
                let before_settle = checkpoint.clone();
                let settled = plans::settle(
                    node,
                    document,
                    &expanded,
                    &mut checkpoint,
                    None,
                    limits,
                    &worktree_layout,
                    &admission_policy,
                    catalogue,
                );
                if let Some((next_expanded, next_plan, next_positions)) = settled {
                    validate_expanded_rows(&checkpoint, &next_plan)?;
                    owner
                        .validate_expansion(&next_expanded, &checkpoint)
                        .map_err(GraphError::Run)?;
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    (expanded, plan, positions) = (next_expanded, next_plan, next_positions);
                    resume = false;
                    continue 'schedule;
                }
                // A rejected retry reactivates its pause; persist it before
                // returning or considering other work.
                if checkpoint != before_settle {
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                }
            }
        }
        let structural_pause =
            transitions::refresh_structure_pause(&mut checkpoint, &expanded, &plan, limits);
        if structural_pause.is_some()
            || (checkpoint.pause_cause() == Some(PauseCause::StructuralLimit)
                && checkpoint.nodes.iter().any(|node| {
                    matches!(
                        node.expansion_cause,
                        Some(NodeDecisionCause::PlanExpansionPaused { active: true, .. })
                    )
                }))
        {
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
        }

        close_resolved_asks(&mut checkpoint, owner, journal)?;
        if checkpoint.cancelled {
            if !resume {
                settle_cancelled_nodes(&mut checkpoint, owner, journal)?;
                let detached_live = checkpoint
                    .nodes
                    .iter()
                    .any(|node| !node.state.settled() && node.detached_from_graph_cancel);
                if !detached_live {
                    return Ok(ScheduleExit::Done(Box::new(GraphRun {
                        status: GraphStatus::Cancelled,
                        outcome: None,
                        checkpoint,
                        advance_error: None,
                    })));
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
            if checkpoint.nodes[node_index].delivery_park.is_some() {
                continue;
            }
            if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
                continue 'schedule;
            }
            if checkpoint.cancelled && !checkpoint.nodes[node_index].detached_from_graph_cancel {
                continue;
            }
            if checkpoint.nodes[node_index].subtree_cancel.is_some() {
                continue;
            }
            // A parent earlier in this wave may just have settled.
            if checkpoint
                .nodes
                .iter()
                .any(|node| node.state.settled() && !node.children_cancelled)
            {
                relay::settle_subtrees(
                    document,
                    &mut checkpoint,
                    owner,
                    journal,
                    &mut stopped_children,
                )?;
                if checkpoint.nodes[node_index].subtree_cancel.is_some() {
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
                    if park_malformed_composite_delivery_review(
                        document,
                        node_index,
                        &plan,
                        &positions,
                        owner,
                        journal,
                        &mut checkpoint,
                    )? {
                        continue;
                    }
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
                            document,
                            node_index,
                            spec,
                            &plan,
                            &positions,
                            &mut checkpoint,
                            owner,
                            journal,
                        )?
                    {
                        if let Err(error) = loops::prepare_decision_extension(
                            document,
                            &checkpoint,
                            &extension,
                            limits,
                        ) {
                            match error {
                                loops::DecisionExtensionError::Structural(limit) => {
                                    checkpoint
                                        .set_pause(PauseCause::StructuralLimit, &limit.to_string());
                                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                                }
                                loops::DecisionExtensionError::Invalid(reason) => {
                                    loops::pause_invalid_decision(
                                        &mut checkpoint,
                                        &node.id,
                                        &reason,
                                        journal,
                                    )?;
                                }
                            }
                        } else {
                            return Ok(ScheduleExit::Recurse {
                                resume: true,
                                extensions: vec![extension],
                                observed_ci,
                            });
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
                    match advance_gate(
                        document,
                        owner,
                        journal,
                        limits,
                        &worktree_layout,
                        &effective_budget,
                        node_index,
                        &condition.clone(),
                        &mut checkpoint,
                        &mut expanded,
                        &mut plan,
                        &mut positions,
                        &mut observed_ci,
                    )? {
                        GateStep::Next => continue,
                        GateStep::Reschedule => continue 'schedule,
                    }
                }
                transitions::NodeAction::Hygiene => {
                    advance_hygiene(
                        document,
                        node_index,
                        &plan,
                        &positions,
                        &mut checkpoint,
                        owner,
                        journal,
                    )?;
                    continue;
                }
                transitions::NodeAction::Publish => {
                    advance_publish(
                        document,
                        node_index,
                        &plan,
                        &positions,
                        &mut checkpoint,
                        owner,
                        journal,
                    )?;
                    continue;
                }
                transitions::NodeAction::Attempt => {}
            }
            if checkpoint.nodes[node_index].retry.is_none()
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
                .then_some(node.review_base.as_deref())
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
                if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
                    continue 'schedule;
                }
                let (route_index, settlement_retries) = match decide_next_attempt(
                    node,
                    &checkpoint.nodes[node_index],
                    limits,
                    document
                        .queue
                        .as_ref()
                        .map(|queue| queue.prep_retries.value),
                    owner,
                    owner.wall_now(),
                )
                .map_err(GraphError::Run)?
                {
                    NextAttempt::WaitRetryWake => break,
                    NextAttempt::WaitFallbackEvidence(retry) => {
                        checkpoint.nodes[node_index].fallback_evidence_retry = Some(retry);
                        asks::refresh_wake(&mut checkpoint);
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    NextAttempt::LaunchRetryExhausted { reason } => {
                        transitions::retry_exhausted(&mut checkpoint.nodes[node_index], &reason);
                        asks::refresh_wake(&mut checkpoint);
                        journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        break;
                    }
                    NextAttempt::Fail { code, message } => {
                        if code.as_deref() == Some("prep_retries_exhausted") {
                            let attempt = checkpoint.nodes[node_index]
                                .attempts
                                .last()
                                .cloned()
                                .ok_or_else(|| GraphError::BadCheckpoint {
                                    node: node.id.clone(),
                                    invariant: "preparation retry exhaustion must retain its graph attempt key",
                                })?;
                            let message = preparation_exhausted_message(message);
                            let handoff = checkpoint.nodes[node_index]
                                .handoff
                                .as_ref()
                                .and_then(|handoff| serde_json::to_string(handoff).ok())
                                .unwrap_or_else(|| message.clone());
                            let head = checkpoint.nodes[node_index]
                                .outputs
                                .as_ref()
                                .and_then(|outputs| outputs.commit.as_deref())
                                .or(checkpoint.nodes[node_index].source_commit.as_deref())
                                .map(str::to_owned);
                            park_delivery_transition(
                                document,
                                node_index,
                                provenant_records::graph::ParkReason::OperationError,
                                Some(&attempt.key),
                                attempt.attempt_ref.as_deref(),
                                &handoff,
                                head.as_deref(),
                                &[message],
                                owner,
                                journal,
                                &mut checkpoint,
                            )?;
                        } else {
                            fail_node(
                                &mut checkpoint.nodes[node_index],
                                code.map(Into::into),
                                message,
                            );
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        }
                        checkpoint.nodes[node_index].fallback_evidence_retry = None;
                        asks::refresh_wake(&mut checkpoint);
                        break;
                    }
                    NextAttempt::None => break,
                    NextAttempt::Candidate {
                        route_index,
                        settlement_retries,
                    } => (route_index, settlement_retries),
                };
                let (route, key_route_index) = if let Some((route, _rung)) =
                    loops::delivery_route(node_index, &plan, &checkpoint)
                {
                    (route, 0)
                } else {
                    (
                        node.routes
                            .get(route_index)
                            .ok_or_else(|| GraphError::BadCheckpoint {
                                node: node.id.clone(),
                                invariant: "attempt route index must name a configured route",
                            })?
                            .clone(),
                        route_index,
                    )
                };
                let key = transitions::attempt_key(&plan, node_index, &checkpoint, key_route_index)
                    .map_err(|error| GraphError::Invalid(error.into()))?;

                // A resume under tighter limits admits no new launch: a saved
                // attempt with no host reference would be started by the
                // observation below, so it waits unless the host has already
                // accepted it. An attempt with a reference is only observed,
                // so finished work still settles (#3558). An ordinary advance
                // keeps its pinned behaviour of re-submitting a saved attempt.
                if resume
                    && checkpoint.pause_cause() == Some(PauseCause::StructuralLimit)
                    && let Some(last) = checkpoint.nodes[node_index].attempts.last()
                    && last.state != AttemptState::Terminal
                    && last.attempt_ref.is_none()
                    && !owner.launch_accepted(last).map_err(GraphError::Run)?
                {
                    break;
                }

                {
                    let prior = checkpoint.nodes[node_index]
                        .launch_retry
                        .as_ref()
                        .and_then(|retry| retry.deadline);
                    if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)?
                    {
                        continue 'schedule;
                    }
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
                match bind_expanded_writer(
                    document,
                    owner,
                    journal,
                    node_index,
                    &plan,
                    &positions,
                    &mut checkpoint,
                    &mut resume,
                )? {
                    LoopStep::Proceed => {}
                    LoopStep::Break => break,
                    LoopStep::Reschedule => continue 'schedule,
                }
                let needs_record = checkpoint.nodes[node_index]
                    .attempts
                    .last()
                    .is_none_or(|last| last.state == AttemptState::Terminal);
                let prepared_inputs = if needs_record {
                    match stage_attempt(
                        owner,
                        journal,
                        limits,
                        &effective_budget,
                        &admission_policy,
                        node_index,
                        &plan,
                        &positions,
                        &mut checkpoint,
                        &key,
                        &route,
                    )? {
                        Staged::Wait => break,
                        Staged::Ready(staged) => staged,
                    }
                } else {
                    None
                };
                let input = attempt_input(
                    owner,
                    journal,
                    limits,
                    node_index,
                    &plan,
                    &positions,
                    &mut checkpoint,
                    &key,
                    prepared_inputs,
                )?;
                // The writer is the nearest writing node along the source chain,
                // so a framing node between a lens and the writer is skipped.
                let separation = attempt_separation(
                    owner,
                    journal,
                    node_index,
                    route_index,
                    needs_record,
                    &plan,
                    &positions,
                    &mut checkpoint,
                )?;
                let continued = land::continued_branch(node, &checkpoint, &positions);
                if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
                    continue 'schedule;
                }
                let Some(observation) = observe_attempt(
                    owner,
                    journal,
                    node_index,
                    settlement_retries,
                    &plan,
                    &positions,
                    &mut checkpoint,
                    &key,
                    &route,
                    &separation,
                    &input,
                    continued.as_deref(),
                )?
                else {
                    break;
                };
                // The owner can be launching a process while a stop arrives.
                // Persist its reference before any later owner query can fail;
                // leave the intent pending until the full observation saves.
                let (settled_result, should_continue, quota_redraw) = match record_observed_attempt(
                    owner,
                    journal,
                    limits,
                    node_index,
                    route_index,
                    &plan,
                    &positions,
                    &mut checkpoint,
                    &key,
                    &route,
                    observation,
                    &separation,
                )? {
                    Observed::NextAttempt => continue,
                    Observed::Applied {
                        settled_result,
                        should_continue,
                        quota_redraw,
                    } => (settled_result, should_continue, quota_redraw),
                };
                let progress = &checkpoint.nodes[node_index];
                let malformed_delivery_review = plan.queue.is_some()
                    && node.delivery_review.is_some()
                    && progress.attempts.last().is_some_and(|attempt| {
                        attempt.key == key && attempt.state == AttemptState::Terminal
                    })
                    && review::malformed_delivery_evidence_error(
                        progress.advance_error_code.as_deref(),
                    );
                if malformed_delivery_review {
                    let progress = &checkpoint.nodes[node_index];
                    let attempt_ref = progress
                        .attempts
                        .last()
                        .filter(|attempt| attempt.key == key)
                        .and_then(|attempt| attempt.attempt_ref.clone());
                    let handoff = progress
                        .handoff
                        .as_ref()
                        .map(|handoff| handoff.note.clone())
                        .unwrap_or_else(|| {
                            "delivery review returned malformed or incomplete evidence".into()
                        });
                    let head = progress.source_commit.clone();
                    let mut evidence = progress
                        .advance_error_code
                        .iter()
                        .chain(progress.advance_error.iter())
                        .cloned()
                        .collect::<Vec<_>>();
                    if let Some(outputs) = &progress.outputs {
                        evidence.extend(outputs.warnings.iter().cloned());
                    }
                    park_delivery_transition(
                        document,
                        node_index,
                        provenant_records::graph::ParkReason::ReviewEvidence,
                        Some(&key),
                        attempt_ref.as_deref(),
                        &handoff,
                        head.as_deref(),
                        &evidence,
                        owner,
                        journal,
                        &mut checkpoint,
                    )?;
                    continue 'schedule;
                }
                if quota_redraw {
                    transitions::redraw_quota_alias(
                        document,
                        node,
                        &mut checkpoint,
                        node_index,
                        limits,
                        &worktree_layout,
                    )
                    .map_err(GraphError::Refused)?;
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    return Ok(ScheduleExit::Recurse {
                        resume: false,
                        extensions: Vec::new(),
                        observed_ci,
                    });
                }
                let appended = if settled_result.is_some() || node.plan.is_some() {
                    plans::settle(
                        node,
                        document,
                        &expanded,
                        &mut checkpoint,
                        settled_result.as_deref(),
                        limits,
                        &worktree_layout,
                        &admission_policy,
                        catalogue,
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
                    settle_stopped_writer::<R, J>(
                        document,
                        owner,
                        &plan,
                        &checkpoint,
                        limits,
                        &worktree_layout,
                    )?
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
                if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
                    continue 'schedule;
                }
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
                || checkpoint.nodes[node_index].detached_from_graph_cancel)
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
                    &worktree_layout,
                    &admission_policy,
                    catalogue,
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
            return Ok(ScheduleExit::Recurse {
                resume: false,
                extensions: Vec::new(),
                observed_ci,
            });
        }
        if document.delivery == Some(provenant_records::run::DeliveryMode::Pr)
            && !checkpoint.delivered
            && checkpoint.successful()
            && !plan
                .nodes
                .iter()
                .any(|node| node.kind == Some(NodeKind::Publish))
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
        return Ok(ScheduleExit::Done(Box::new(transitions::graph_run(
            checkpoint,
        ))));
    }
}

fn delivery_text(value: &str, max_bytes: usize) -> String {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn delivery_operation(
    domain: &str,
    prefix: &str,
    value: &impl Serialize,
) -> Result<String, CanonicalError> {
    let digest = CanonicalV1::record_digest(domain, 1, value)?;
    Ok(format!(
        "{prefix}-{}",
        provenant_core::hex(digest.as_bytes())
    ))
}

/// Persist the parking identity before opening the inbox, then replay the same
/// immutable operation until a receipt is recorded.
pub(super) fn park_delivery_transition<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    node_index: usize,
    reason: provenant_records::graph::ParkReason,
    attempt_key: Option<&AttemptKey>,
    attempt_ref: Option<&str>,
    handoff: &str,
    head: Option<&str>,
    evidence: &[String],
    owner: &mut R,
    journal: &mut J,
    checkpoint: &mut GraphCheckpoint,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let node_id = checkpoint
        .nodes
        .get(node_index)
        .map(|node| node.id.clone())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: document.graph_id.clone(),
            invariant: "delivery park node index must exist",
        })?;
    // TODO(#3972-queue): queue admission resolves tracker identities and seals
    // them in the graph context before launching item graphs.
    let item = document
        .context
        .as_ref()
        .and_then(|context| context.work_item.as_deref());
    let bounded_handoff = delivery_text(handoff, 1024);
    let bounded_evidence: Vec<String> = evidence
        .iter()
        .take(8)
        .map(|entry| delivery_text(entry, 512))
        .collect();
    let identity = serde_json::json!({
        "graph": document.graph_id,
        "item": item,
        "node": node_id,
        "reason": reason,
        "attempt_key": attempt_key,
        "attempt_ref": attempt_ref,
        "head": head,
        "handoff": bounded_handoff,
        "evidence": bounded_evidence,
    });
    let operation_id =
        delivery_operation("delivery-park", "park", &identity).map_err(GraphError::Canonical)?;
    let transition_id = operation_id.clone();

    if let Some(existing) = checkpoint.nodes[node_index].delivery_park.as_ref() {
        if existing.transition_id != transition_id {
            return Err(GraphError::BadCheckpoint {
                node: node_id,
                invariant: "one graph node cannot replace a persisted delivery park transition",
            });
        }
        if existing.request_id.is_some() {
            if checkpoint.nodes[node_index].state != NodeState::Blocked {
                checkpoint.nodes[node_index].state = NodeState::Blocked;
                journal.save(checkpoint).map_err(GraphError::Journal)?;
            }
            return Ok(());
        }
    } else {
        checkpoint.nodes[node_index].delivery_park = Some(DeliveryParkProgress {
            transition_id: transition_id.clone(),
            operation_id: operation_id.clone(),
            item: item.map(str::to_owned),
            attempt_key: attempt_key.cloned(),
            attempt_ref: attempt_ref.map(str::to_owned),
            reason,
            handoff: bounded_handoff.clone(),
            head: head.map(str::to_owned),
            evidence: bounded_evidence.clone(),
            request_id: None,
        });
        checkpoint.nodes[node_index].state = NodeState::Running;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }

    let request_id = owner
        .park_delivery(DeliveryParkRequest {
            transition_id: &transition_id,
            operation_id: &operation_id,
            graph_id: &document.graph_id,
            item,
            node_id: &node_id,
            attempt_key,
            attempt_ref,
            repository: None,
            session: document.decision_session.as_deref(),
            reason,
            handoff: &bounded_handoff,
            head,
            evidence: &bounded_evidence,
        })
        .map_err(GraphError::Run)?;
    if let Some(request_id) = request_id {
        let park = checkpoint.nodes[node_index]
            .delivery_park
            .as_mut()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node_id.clone(),
                invariant: "delivery park intent must remain persisted until receipt",
            })?;
        park.request_id = Some(request_id);
        checkpoint.nodes[node_index].state = NodeState::Blocked;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    Ok(())
}

fn advance_hygiene<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    node_index: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let node = plan
        .nodes
        .get(node_index)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: document.graph_id.clone(),
            invariant: "hygiene node index must exist",
        })?;
    if let Some(park) = checkpoint.nodes[node_index].delivery_park.clone() {
        return park_delivery_transition(
            document,
            node_index,
            park.reason,
            park.attempt_key.as_ref(),
            park.attempt_ref.as_deref(),
            &park.handoff,
            park.head.as_deref(),
            &park.evidence,
            owner,
            journal,
            checkpoint,
        );
    }
    let source_id = match &node.source {
        Some(SourceBinding::Node { node }) => node,
        _ => {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "hygiene requires its validated direct writer source",
            });
        }
    };
    let source_index = *positions
        .get(source_id)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene writer source must exist",
        })?;
    let source_progress = &checkpoint.nodes[source_index];
    let source_attempt = source_progress
        .recorded_attempts()
        .filter(|attempt| attempt.state == AttemptState::Terminal)
        .next_back()
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene requires a terminal writer attempt",
        })?;
    let input_head = source_attempt
        .input_manifest
        .as_ref()
        .and_then(|manifest| manifest.source_commit.as_deref())
        .or(source_progress.source_commit.as_deref())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene writer attempt must retain its input head",
        })?;
    let source_outputs =
        source_progress
            .outputs
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "hygiene writer must retain its outputs",
            })?;
    let expected_head = source_outputs
        .commit
        .as_deref()
        .or(source_progress.source_commit.as_deref())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene writer must retain its output head",
        })?;
    let writer_binding =
        source_progress
            .writer_binding
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "hygiene writer must retain its verified worktree binding",
            })?;
    let worktree = writer_binding.worktree.clone();
    let worktree_identity = writer_binding.source_directory_identity.clone();
    let source_key = source_attempt.key.clone();
    let source_attempt_ref =
        source_attempt
            .attempt_ref
            .clone()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "hygiene requires the accepted writer attempt reference",
            })?;
    let input_head = input_head.to_owned();
    let expected_head = expected_head.to_owned();
    let source_branch = source_outputs.branch.clone();
    let handoff = source_progress
        .handoff
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene handoff must be serializable",
        })?;

    let operation_id = delivery_operation(
        "delivery-hygiene",
        "hygiene",
        &serde_json::json!({
            "graph": document.graph_id,
            "node": node.id,
            "source_attempt": source_key,
            "input_head": input_head,
            "expected_head": expected_head,
        }),
    )
    .map_err(GraphError::Canonical)?;
    if checkpoint.nodes[node_index].delivery_hygiene.is_none() {
        checkpoint.nodes[node_index].delivery_hygiene = Some(DeliveryHygieneProgress {
            operation_id: operation_id.clone(),
            input_head: input_head.clone(),
            handoff: handoff.clone(),
            outcome: None,
        });
        checkpoint.nodes[node_index].state = NodeState::Running;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }

    let intent = checkpoint.nodes[node_index]
        .delivery_hygiene
        .as_ref()
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene intent must be saved before execution",
        })?;
    if intent.operation_id != operation_id
        || intent.input_head != input_head
        || intent.handoff != handoff
    {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "hygiene replay must retain its original operation, writer input and handoff",
        });
    }
    let intent_handoff = intent.handoff.clone();
    let intent_input_head = intent.input_head.clone();
    if let Some(outcome) = intent.outcome.clone() {
        if outcome.kind != provenant_records::graph::HygieneOutcomeKind::Error {
            return Ok(());
        }
        let evidence = checkpoint.nodes[node_index]
            .advance_error
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        return park_delivery_transition(
            document,
            node_index,
            provenant_records::graph::ParkReason::OperationError,
            Some(&source_key),
            Some(&source_attempt_ref),
            intent_handoff.as_deref().unwrap_or(""),
            outcome.after_head.as_deref(),
            &evidence,
            owner,
            journal,
            checkpoint,
        );
    }

    let mut execution = owner
        .run_hygiene(HygieneRequest {
            graph_id: &document.graph_id,
            node_id: &node.id,
            operation_id: &operation_id,
            source_node: source_id,
            source_key: &source_key,
            source_attempt: &source_attempt_ref,
            worktree: &worktree,
            worktree_identity: worktree_identity.as_ref(),
            input_head: &intent_input_head,
            expected_head: &expected_head,
            handoff: intent_handoff.as_deref(),
        })
        .map_err(GraphError::Run)?;
    execution.outcome.handoff = intent_handoff.clone();
    if execution.outcome.kind == provenant_records::graph::HygieneOutcomeKind::Clean
        && execution.outcome.after_head.is_none()
    {
        execution.outcome.kind = provenant_records::graph::HygieneOutcomeKind::Error;
        execution.diagnostic = Some("hygiene completed without a verified output head".into());
    }
    let outcome = execution.outcome;
    let formatter_warnings = execution
        .formatter_source
        .iter()
        .map(|source| format!("delivery hygiene formatter source: {source}"))
        .collect();
    {
        let progress = &mut checkpoint.nodes[node_index];
        progress
            .delivery_hygiene
            .as_mut()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "hygiene intent must remain persisted during execution",
            })?
            .outcome = Some(outcome.clone());
        progress.outputs = Some(NodeOutputs {
            schema_version: 2,
            hygiene: Some(outcome.clone()),
            branch: execution.branch.or(source_branch),
            commit: outcome.after_head.clone(),
            warnings: formatter_warnings,
            ..Default::default()
        });
    }
    match outcome.kind {
        provenant_records::graph::HygieneOutcomeKind::Clean => {
            let progress = &mut checkpoint.nodes[node_index];
            progress.state = NodeState::Succeeded;
            progress.advance_error = None;
            progress.advance_error_code = None;
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
        kind @ (provenant_records::graph::HygieneOutcomeKind::NoCommit
        | provenant_records::graph::HygieneOutcomeKind::UncommittedEdits) => {
            let cause = if kind == provenant_records::graph::HygieneOutcomeKind::NoCommit {
                provenant_records::graph::EscalationCause::NoCommit
            } else {
                provenant_records::graph::EscalationCause::UncommittedEdits
            };
            let (error_code, error_message) =
                if kind == provenant_records::graph::HygieneOutcomeKind::NoCommit {
                    (
                        "hygiene_no_commit",
                        "writer produced no commit beyond its input head",
                    )
                } else {
                    (
                        "hygiene_uncommitted_edits",
                        "writer worktree still contains tracked or untracked edits",
                    )
                };
            {
                let progress = &mut checkpoint.nodes[node_index];
                progress.state = NodeState::Failed;
                progress.advance_error_code = Some(error_code.into());
                progress.advance_error = Some(error_message.into());
            }
            let escalation =
                transitions::advance_delivery_escalation(node_index, plan, checkpoint, cause);
            if matches!(
                escalation,
                Some(transitions::EscalationTransition::Exhausted { .. })
            ) {
                let park_node = loops::escalation_owner(node_index, plan)
                    .map_or(node_index, |(owner_index, _)| owner_index);
                let evidence = vec![error_message.to_owned()];
                park_delivery_transition(
                    document,
                    park_node,
                    provenant_records::graph::ParkReason::LadderExhausted,
                    Some(&source_key),
                    Some(&source_attempt_ref),
                    handoff.as_deref().unwrap_or(""),
                    outcome.after_head.as_deref(),
                    &evidence,
                    owner,
                    journal,
                    checkpoint,
                )?;
            } else {
                journal.save(checkpoint).map_err(GraphError::Journal)?;
            }
        }
        provenant_records::graph::HygieneOutcomeKind::Error => {
            let evidence = {
                let progress = &mut checkpoint.nodes[node_index];
                progress.state = NodeState::Running;
                progress.advance_error_code = Some("hygiene_operation_error".into());
                progress.advance_error = execution
                    .diagnostic
                    .as_deref()
                    .map(|diagnostic| delivery_text(diagnostic, 1024))
                    .or_else(|| Some("formatter or formatter commit failed".into()));
                progress.advance_error.iter().cloned().collect::<Vec<_>>()
            };
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            park_delivery_transition(
                document,
                node_index,
                provenant_records::graph::ParkReason::OperationError,
                Some(&source_key),
                Some(&source_attempt_ref),
                intent_handoff.as_deref().unwrap_or(""),
                outcome.after_head.as_deref(),
                &evidence,
                owner,
                journal,
                checkpoint,
            )?;
        }
    }
    Ok(())
}

fn advance_publish<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    node_index: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let node = plan
        .nodes
        .get(node_index)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: document.graph_id.clone(),
            invariant: "publish node index must exist",
        })?;
    if let Some(park) = checkpoint.nodes[node_index].delivery_park.clone() {
        return park_delivery_transition(
            document,
            node_index,
            park.reason,
            park.attempt_key.as_ref(),
            park.attempt_ref.as_deref(),
            &park.handoff,
            park.head.as_deref(),
            &park.evidence,
            owner,
            journal,
            checkpoint,
        );
    }
    let hygiene_id = match &node.source {
        Some(SourceBinding::Node { node }) => node.clone(),
        _ => {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish requires its validated direct hygiene source",
            });
        }
    };
    let hygiene_index = *positions
        .get(&hygiene_id)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish hygiene source must exist",
        })?;
    let hygiene_node = &plan.nodes[hygiene_index];
    if hygiene_node.kind != Some(NodeKind::Hygiene) {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish source must be a hygiene node",
        });
    }
    let hygiene_progress = &checkpoint.nodes[hygiene_index];
    let hygiene_outputs =
        hygiene_progress
            .outputs
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish hygiene source must retain outputs",
            })?;
    let hygiene_outcome =
        hygiene_outputs
            .hygiene
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish hygiene source must retain its typed outcome",
            })?;
    if hygiene_outcome.kind != provenant_records::graph::HygieneOutcomeKind::Clean {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish requires a clean hygiene outcome",
        });
    }
    let expected_head =
        hygiene_outcome
            .after_head
            .clone()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish hygiene outcome must retain after_head",
            })?;
    let writer_id = match &hygiene_node.source {
        Some(SourceBinding::Node { node }) => node.clone(),
        _ => {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish hygiene source must retain its direct writer binding",
            });
        }
    };
    let writer_index = *positions
        .get(&writer_id)
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish writer source must exist",
        })?;
    let writer_progress = &checkpoint.nodes[writer_index];
    let writer_binding =
        writer_progress
            .writer_binding
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish writer must retain its verified worktree binding",
            })?;
    let writer_worktree = writer_binding.worktree.clone();
    let writer_identity = writer_binding.source_directory_identity.clone();
    let writer_attempt = writer_progress
        .recorded_attempts()
        .filter(|attempt| attempt.state == AttemptState::Terminal)
        .next_back()
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish writer must retain a terminal attempt",
        })?;
    let writer_key = writer_attempt.key.clone();
    let writer_attempt_ref =
        writer_attempt
            .attempt_ref
            .clone()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publish writer must retain the accepted attempt reference",
            })?;
    let writer_branch = hygiene_outputs
        .branch
        .clone()
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish hygiene output must retain its writer branch",
        })?;
    let source_branch = document
        .integration_branch
        .clone()
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish requires the sealed integration branch",
        })?;
    let integration_base = checkpoint
        .bases
        .and_then(|bases| bases.integration)
        .map(|base| base.to_string())
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publish requires the integration base pinned at admission",
        })?;
    let handoff = hygiene_outcome.handoff.clone();

    let mut expected_remote_head = None;
    for prior_index in 0..node_index {
        if plan.nodes[prior_index].kind != Some(NodeKind::Publish) {
            continue;
        }
        let prior_state = checkpoint.nodes[prior_index].state;
        if !prior_state.settled() {
            checkpoint.nodes[node_index].wait_reason = Some("after".into());
            checkpoint.nodes[node_index].state = NodeState::Pending;
            journal.save(checkpoint).map_err(GraphError::Journal)?;
            return Ok(());
        }
        if prior_state == NodeState::Failed || prior_state == NodeState::Blocked {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "a later publication cannot bypass a failed or parked publication",
            });
        }
        let prior_publication = checkpoint.nodes[prior_index]
            .delivery_publication
            .as_ref()
            .map(|intent| (intent.source_branch.clone(), intent.receipt.clone()));
        let prior_parked = checkpoint.nodes[prior_index].delivery_park.is_some();
        if let Some((prior_source_branch, receipt)) = prior_publication {
            if prior_source_branch != source_branch {
                continue;
            }
            if prior_parked {
                checkpoint.nodes[node_index].wait_reason = Some("after".into());
                checkpoint.nodes[node_index].state = NodeState::Pending;
                journal.save(checkpoint).map_err(GraphError::Journal)?;
                return Ok(());
            }
            let receipt = receipt.ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "prior graph publication must retain its receipt",
            })?;
            expected_remote_head = Some(receipt.head);
        }
    }

    // TODO(#3972-queue): bind the stable queue item identity once the queue
    // owner supplies it; graph_id is the current durable publication scope.
    let publication_identity = document.graph_id.as_str();
    let intent_value = serde_json::json!({
        "graph": document.graph_id,
        "node": node.id,
        "publication_identity": publication_identity,
        "source_node": writer_id,
        "source_key": writer_key,
        "source_attempt": writer_attempt_ref,
        "writer_branch": writer_branch,
        "source_branch": source_branch,
        "head": expected_head,
        "integration_base": integration_base,
        "expected_remote_head": expected_remote_head,
    });
    let operation_id = delivery_operation("delivery-publication", "publish", &intent_value)
        .map_err(GraphError::Canonical)?;
    if checkpoint.nodes[node_index].delivery_publication.is_none() {
        checkpoint.nodes[node_index].delivery_publication = Some(DeliveryPublicationProgress {
            operation_id: operation_id.clone(),
            source_node: writer_id.clone(),
            source_key: writer_key.clone(),
            source_attempt: writer_attempt_ref.clone(),
            writer_branch: writer_branch.clone(),
            input_head: expected_head.clone(),
            source_branch: source_branch.clone(),
            integration_base: integration_base.clone(),
            expected_remote_head: expected_remote_head.clone(),
            receipt: None,
            error: None,
        });
        checkpoint.nodes[node_index].state = NodeState::Running;
        checkpoint.nodes[node_index].wait_reason = None;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    let intent = checkpoint.nodes[node_index]
        .delivery_publication
        .as_ref()
        .ok_or_else(|| GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publication intent must be saved before execution",
        })?;
    if intent.operation_id != operation_id
        || intent.source_node != writer_id
        || intent.source_key != writer_key
        || intent.source_attempt != writer_attempt_ref
        || intent.writer_branch != writer_branch
        || intent.input_head != expected_head
        || intent.source_branch != source_branch
        || intent.integration_base != integration_base
        || intent.expected_remote_head != expected_remote_head
    {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "publication replay must retain its original source and remote lease",
        });
    }
    if let Some(receipt) = intent.receipt.clone() {
        let outputs = checkpoint.nodes[node_index]
            .outputs
            .as_ref()
            .ok_or_else(|| GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "published node receipt must retain matching outputs",
            })?;
        if receipt.head != expected_head
            || outputs.publication.as_ref() != Some(&receipt)
            || outputs.branch.as_deref() != Some(&receipt.branch)
            || outputs.commit.as_deref() != Some(&receipt.head)
        {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "publication receipt and outputs must match the sealed head",
            });
        }
        return Ok(());
    }
    let operation_id = intent.operation_id.clone();
    let expected_remote_head = intent.expected_remote_head.clone();
    let input_head = intent.input_head.clone();
    let source_branch = intent.source_branch.clone();
    let integration_base = intent.integration_base.clone();
    let writer_branch = intent.writer_branch.clone();
    let handoff = handoff.clone();
    let mut failure = None;
    let receipt = match owner.publish_delivery(PublicationRequest {
        graph_id: &document.graph_id,
        node_id: &node.id,
        operation_id: &operation_id,
        publication_identity,
        source_node: &writer_id,
        source_key: &writer_key,
        source_attempt: &writer_attempt_ref,
        worktree: &writer_worktree,
        worktree_identity: writer_identity.as_ref(),
        writer_branch: &writer_branch,
        source_branch: &source_branch,
        expected_head: &input_head,
        expected_remote_head: expected_remote_head.as_deref(),
        integration_base: &integration_base,
        handoff: handoff.as_deref(),
    }) {
        Ok(Some(receipt))
            if !receipt.branch.trim().is_empty()
                && receipt.branch.len() <= outputs::OUTPUT_FIELD_BYTES
                && receipt
                    .branch
                    .strip_prefix("provenant/graph/")
                    .is_some_and(|suffix| {
                        suffix.len() == 64 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                && !receipt.branch.chars().any(char::is_whitespace)
                && receipt.head == input_head
                && receipt.head.parse::<provenant_core::GitOid>().is_ok()
                && receipt.pull_request.starts_with("https://")
                && receipt.pull_request.len() <= outputs::OUTPUT_FIELD_BYTES
                && !receipt.pull_request.chars().any(char::is_whitespace)
                && !receipt.pull_request.chars().any(char::is_control)
                && receipt.pull_request.lines().count() == 1 =>
        {
            Some(receipt)
        }
        Ok(Some(_)) => {
            failure = Some("host publication receipt did not match the sealed head".to_owned());
            None
        }
        Ok(None) => {
            failure = Some("host does not provide delivery publication".to_owned());
            None
        }
        Err(error) => {
            failure = Some(delivery_text(&error.to_string(), 1024));
            None
        }
    };
    if let Some(receipt) = receipt {
        let progress = &mut checkpoint.nodes[node_index];
        let intent =
            progress
                .delivery_publication
                .as_mut()
                .ok_or_else(|| GraphError::BadCheckpoint {
                    node: node.id.clone(),
                    invariant: "publication intent must remain persisted during execution",
                })?;
        intent.receipt = Some(receipt.clone());
        intent.error = None;
        progress.outputs = Some(NodeOutputs {
            schema_version: 2,
            publication: Some(receipt.clone()),
            branch: Some(receipt.branch),
            commit: Some(receipt.head),
            ..Default::default()
        });
        progress.state = NodeState::Succeeded;
        progress.advance_error = None;
        progress.advance_error_code = None;
        progress.wait_reason = None;
        if checkpoint.delivered
            && checkpoint.pull_request.as_deref() != Some(receipt.pull_request.as_str())
        {
            return Err(GraphError::BadCheckpoint {
                node: node.id.clone(),
                invariant: "queue publication must retain one stable graph PR identity",
            });
        }
        transitions::record_delivery(checkpoint, receipt.pull_request.clone())
            .map_err(GraphError::Refused)?;
        journal.save(checkpoint).map_err(GraphError::Journal)?;
        return Ok(());
    }

    let failure = failure.unwrap_or_else(|| "publication failed without a diagnostic".into());
    let evidence = vec![failure.clone()];
    if let Some(intent) = checkpoint.nodes[node_index].delivery_publication.as_mut() {
        intent.error = Some(failure.clone());
    }
    checkpoint.nodes[node_index].state = NodeState::Running;
    checkpoint.nodes[node_index].advance_error_code = Some("publication_operation_error".into());
    checkpoint.nodes[node_index].advance_error = Some(failure);
    journal.save(checkpoint).map_err(GraphError::Journal)?;
    park_delivery_transition(
        document,
        node_index,
        provenant_records::graph::ParkReason::OperationError,
        Some(&writer_key),
        Some(&writer_attempt_ref),
        handoff.as_deref().unwrap_or(""),
        Some(&input_head),
        &evidence,
        owner,
        journal,
        checkpoint,
    )
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
    submission_refusals: &[(String, String)],
    admitted_candidate: Option<AdmittedCandidate>,
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    let worktree_layout = owner.worktree_layout();
    // The host owns the effective catalogue; patch admission reads it from the
    // graph context rather than resolving a bundled fallback of its own.
    let catalogue = owner.catalogue();
    // Resolve once for this graph operation, before replay and expansion.
    let admission_policy = owner.graph_admission_policy();
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
    let (base, _) = if stored.is_some() {
        validate_replay_plan(document, &unbounded)
            .map_err(|error| GraphError::Invalid(error.to_string()))?
    } else {
        validate_plan_with_limits(document, &unbounded).map_err(GraphError::Admission)?
    };
    let admitted = match admitted_candidate {
        Some(candidate) => {
            if candidate.source != *document
                || candidate.checkpoint != stored
                || candidate.extensions != extensions
            {
                return Err(GraphError::Invalid(
                    "admitted candidate does not match the execution state".into(),
                ));
            }
            candidate
        }
        None => match stored.as_ref() {
            Some(checkpoint) => {
                admit_running_candidate(document, checkpoint, extensions, limits, budget, None)
            }
            None => admit_candidate(document, None, extensions, limits, budget, None),
        }
        .map_err(|error| match error {
            CandidateError::CheckpointTopology { node, invariant } => GraphError::BadCheckpoint {
                node: node.to_string(),
                invariant,
            },
            other => GraphError::Candidate(other),
        })?,
    };
    let stored_extensions = stored.as_ref().map_or(&[][..], |c| c.extensions.as_slice());
    let expanded = plans::expand(
        document,
        stored_extensions,
        stored.as_ref().map_or(&[], |c| c.expansions.as_slice()),
        &unbounded,
    )
    .map_err(|reason| GraphError::Invalid(reason.to_string()))?;
    let (plan, positions) = validate_flat_plan(&expanded, &unbounded)
        .map_err(|reason| GraphError::Invalid(reason.to_string()))?;
    let plan_digest = plan_digest(&base)
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
            let (review, integration) = owner.bound_graph_bases();
            let bases = GraphBaseBinding::new(review, integration);
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
                bases,
                instance_id,
                ask_actor,
            );
            if let Some(submission) = document.submission_patch.as_ref() {
                let library = template::TemplateLibrary::bundled();
                let resolvers = Default::default();
                patch::admit_patch(
                    &mut patch::LiveGraph {
                        document,
                        checkpoint: &mut checkpoint,
                    },
                    None,
                    submission,
                    &patch::PatchContext {
                        catalogue: catalogue.as_ref(),
                        library: &library,
                        resolvers: &resolvers,
                        journal: None,
                        limits,
                        worktree_layout: worktree_layout.clone(),
                    },
                )
                .map_err(|error| GraphError::Invalid(error.to_string()))?;
            }
            // Publish the generation before the first eager bind. A stop can
            // then write an instance-bound request while a host owns the graph
            // lock and is resolving or creating a worktree.
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
            if !submission_refusals.is_empty() {
                for progress in &mut checkpoint.nodes {
                    if progress.state == NodeState::Pending {
                        let reason = submission_refusals
                            .iter()
                            .find(|(id, _)| id == &progress.id)
                            .map_or("graph submission refused before launch", |(_, reason)| {
                                reason.as_str()
                            });
                        transitions::fail_with(
                            progress,
                            "writer_preflight_refused",
                            reason.to_owned(),
                        );
                    }
                }
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                return Ok(transitions::graph_run(checkpoint));
            }
            for index in 0..plan.nodes.len() {
                if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
                    settle_cancelled_nodes(&mut checkpoint, owner, journal)?;
                    return Ok(transitions::graph_run(checkpoint));
                }
                let node = &plan.nodes[index];
                // A loop round's worktree is advanced by earlier rounds; its
                // starting head becomes known only at that round. A row the
                // scheduler settled as skipped never runs and takes no writer
                // binding (#3899).
                if checkpoint.nodes[index].state != NodeState::Skipped
                    && plans::writer(node)
                    && !node.provision_worktree
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
                    match owner.bind_writer_worktree(worktree) {
                        Ok(binding) => {
                            transitions::apply_writer_binding(
                                node,
                                &mut checkpoint.nodes[index],
                                binding,
                                document.schema_version >= 2,
                            )
                            .map_err(GraphError::Refused)?;
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        }
                        // Contention waits in the launch loop, which owns the
                        // persisted deadline; only a refusal fails the node here.
                        Err(error) if next_attempt::launch_contention(&error) => {}
                        Err(error) => {
                            transitions::writer_binding_failed(
                                &mut checkpoint.nodes[index],
                                &node.id,
                                &next_attempt::writer_binding_reason(&node.id, &error),
                            );
                            journal.save(&checkpoint).map_err(GraphError::Journal)?;
                        }
                    }
                    // Save a successful binding before acknowledging the stop;
                    // otherwise the physical bind could outlive its checkpoint.
                    if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)?
                    {
                        settle_cancelled_nodes(&mut checkpoint, owner, journal)?;
                        return Ok(transitions::graph_run(checkpoint));
                    }
                }
            }
            // Pin named review bases while the admitted source locations are
            // still the ones selected for this execution.
            for index in 0..plan.nodes.len() {
                if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
                    settle_cancelled_nodes(&mut checkpoint, owner, journal)?;
                    return Ok(transitions::graph_run(checkpoint));
                }
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
                    .review_base
                    .as_deref()
                    .ok_or_else(|| GraphError::Invalid("node review base missing".into()))?;
                let base = owner.resolve_review_base(reference, location).map_err(GraphError::Run)?
                    .filter(|base| base.parse::<provenant_core::GitOid>().is_ok())
                    .ok_or_else(|| GraphError::Refused(format!("node {}: review base could not be bound; correct the reference and rerun", node.id)))?;
                transitions::apply_review_base(&mut checkpoint.nodes[index], base);
                journal.save(&checkpoint).map_err(GraphError::Journal)?;
                if apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
                    settle_cancelled_nodes(&mut checkpoint, owner, journal)?;
                    return Ok(transitions::graph_run(checkpoint));
                }
            }
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
            checkpoint
        }
    };

    if let Some(limit) = admitted.growth_limit {
        // A refused resume cannot reopen or re-pause a cancelled graph.
        if checkpoint.cancelled {
            return Ok(GraphRun {
                status: GraphStatus::Cancelled,
                outcome: None,
                checkpoint,
                advance_error: None,
            });
        }
        checkpoint.set_pause(PauseCause::StructuralLimit, &limit.to_string());
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
        return Ok(transitions::graph_run(checkpoint));
    }

    if recover_minima(&mut checkpoint, &plan) {
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }

    if may_ask(&plan) && (checkpoint.instance_id.is_none() || checkpoint.ask_actor.is_none()) {
        asks::identity::<R, J::Error>(&mut checkpoint, owner)?;
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }
    let effective_budget = admitted.effective_budget;
    if transitions::refresh_limit_notes(&mut checkpoint, document) {
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }
    if resume && apply_pending_cancellation(document, &mut checkpoint, journal, &mut resume)? {
        settle_cancelled_nodes(&mut checkpoint, owner, journal)?;
        return Ok(transitions::graph_run(checkpoint));
    }
    if resume {
        let mut resume_view = checkpoint.clone();
        restore_cancelled_nodes(&mut resume_view)
            .map_err(|(node, invariant)| GraphError::BadCheckpoint { node, invariant })?;
        // Keep the cancellation fence and its displayed states durable until
        // every extension is accepted. Reopen in the same save as those states.
        if checkpoint.cancelled {
            close_resolved_asks(&mut checkpoint, owner, journal)?;
        }
        if journal
            .apply_cancellation(document, &mut checkpoint)
            .map_err(GraphError::Journal)?
        {
            settle_cancelled_nodes(&mut checkpoint, owner, journal)?;
            return Ok(transitions::graph_run(checkpoint));
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
                &[],
                None,
            );
        }
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }

    match advance_schedule(
        document,
        owner,
        journal,
        limits,
        worktree_layout,
        admission_policy,
        catalogue.as_ref(),
        effective_budget,
        resume,
        checkpoint,
        expanded,
        plan,
        positions,
        std::mem::take(observed_ci),
    )? {
        ScheduleExit::Done(run) => Ok(*run),
        ScheduleExit::Recurse {
            resume,
            extensions,
            mut observed_ci,
        } => advance_inner(
            document,
            owner,
            journal,
            limits,
            budget,
            resume,
            &extensions,
            &mut observed_ci,
            &[],
            None,
        ),
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
        repair_design: None,
        partial: false,
        risk_tier: None,
        repair_items: None,
        spec_concerns: None,
        spec_concern_count: None,
        spec_defect_count: None,
        artifact_findings: None,
        full_artifact_findings: None,
        compact_disposition_findings: None,
        full_disposition_findings: None,
        verdict_error: None,
        hygiene: None,
        publication: None,
        schema_version: 2,
        decide_outcome: None,
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

fn delivery_review_gate_failed(
    condition: &GateCondition,
    gate: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    use provenant_records::workflow::ReviewVerdict;
    let GateCondition::Verdict { expected, .. } = condition else {
        return false;
    };
    if ReviewVerdict::from_token(expected) != Some(ReviewVerdict::Approve) || gate.inputs.is_empty()
    {
        return false;
    }
    let mut failed_review = false;
    for input in &gate.inputs {
        let Some(&index) = positions.get(&input.node) else {
            return false;
        };
        let Some(policy) = plan.nodes[index].delivery_review.as_ref() else {
            return false;
        };
        let progress = &checkpoint.nodes[index];
        if progress.state != NodeState::Succeeded {
            return false;
        }
        let Some(outputs) = progress.outputs.as_ref() else {
            return false;
        };
        let Some(verdict) = outputs
            .verdict
            .as_deref()
            .and_then(ReviewVerdict::from_token)
        else {
            return false;
        };
        if outputs.partial
            || outputs.findings_available != Some(true)
            || outputs.verdict_error.is_some()
        {
            return false;
        }
        if !review::delivery_matches(policy, outputs, expected) {
            if verdict == ReviewVerdict::Approve {
                return false;
            }
            failed_review = true;
        }
    }
    failed_review
}

fn queue_ci_repair_node(node_index: usize, plan: &PlannedGraph) -> bool {
    let Some(node) = plan.nodes.get(node_index) else {
        return false;
    };
    plan.nodes.iter().any(|gate| {
        matches!(gate.condition, Some(GateCondition::Ci { .. }))
            && gate.on_fail.as_deref().is_some_and(|target| {
                target == node.id
                    || node
                        .round
                        .as_ref()
                        .is_some_and(|round| target == round.loop_id)
            })
    })
}

fn park_malformed_composite_delivery_review<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    node_index: usize,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    owner: &mut R,
    journal: &mut J,
    checkpoint: &mut GraphCheckpoint,
) -> Result<bool, GraphError<R::Error, J::Error>> {
    let Some(node) = plan.nodes.get(node_index) else {
        return Err(GraphError::BadCheckpoint {
            node: document.graph_id.clone(),
            invariant: "composite delivery review index must match the planned graph",
        });
    };
    let Some(progress) = checkpoint.nodes.get(node_index) else {
        return Err(GraphError::BadCheckpoint {
            node: node.id.clone(),
            invariant: "composite delivery review requires its checkpoint row",
        });
    };
    if document.queue.is_none() || !review::malformed_composite_delivery_evidence(node, progress) {
        return Ok(false);
    }

    let writer = loops::nearest_writer_ancestor(node_index, plan)
        .and_then(|index| checkpoint.nodes.get(index));
    let attempt = writer.and_then(|progress| {
        progress
            .attempts
            .iter()
            .rev()
            .find(|attempt| attempt.state == AttemptState::Terminal)
            .cloned()
    });
    let head = writer
        .and_then(|progress| {
            progress
                .outputs
                .as_ref()
                .and_then(|outputs| outputs.commit.as_deref())
                .or(progress.source_commit.as_deref())
        })
        .or_else(|| {
            checkpoint.nodes[node_index]
                .source_commit
                .as_deref()
                .or_else(|| {
                    checkpoint.nodes[node_index]
                        .outputs
                        .as_ref()
                        .and_then(|outputs| outputs.commit.as_deref())
                })
        })
        .map(str::to_owned);
    let handoff = writer
        .and_then(|progress| progress.handoff.as_ref())
        .map(|handoff| handoff.note.clone())
        .unwrap_or_else(|| {
            "delivery review composite returned malformed or partial evidence".into()
        });
    let progress = &checkpoint.nodes[node_index];
    let mut evidence = vec![format!(
        "delivery review boundary {} settled {:?}",
        node.id, progress.state
    )];
    if let Some(code) = &progress.advance_error_code {
        evidence.push(format!("boundary error code: {code}"));
    }
    if let Some(error) = &progress.advance_error {
        evidence.push(format!("boundary error: {error}"));
    }
    for member_id in &node.composite_members {
        let Some(&member_index) = positions.get(member_id) else {
            continue;
        };
        let member = &checkpoint.nodes[member_index];
        let partial = member
            .outputs
            .as_ref()
            .is_some_and(|outputs| outputs.partial || outputs.findings_available != Some(true));
        if member.state != NodeState::Succeeded || partial {
            let detail = member
                .advance_error
                .as_deref()
                .unwrap_or("incomplete panel member evidence");
            evidence.push(format!("member {member_id} {:?}: {detail}", member.state));
        }
    }
    if let Some(outputs) = &progress.outputs {
        if let Some(verdict) = &outputs.verdict {
            evidence.push(format!("aggregate verdict: {verdict}"));
        }
        evidence.extend(outputs.warnings.iter().cloned());
    }
    park_delivery_transition(
        document,
        node_index,
        provenant_records::graph::ParkReason::ReviewEvidence,
        attempt.as_ref().map(|attempt| &attempt.key),
        attempt
            .as_ref()
            .and_then(|attempt| attempt.attempt_ref.as_deref()),
        &handoff,
        head.as_deref(),
        &evidence,
        owner,
        journal,
        checkpoint,
    )?;
    Ok(true)
}

fn gate_failure(
    condition: &GateCondition,
    node: &PlannedNode,
    plan: &PlannedGraph,
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
                    let index = positions[&input.node];
                    let outputs = checkpoint.nodes[index].outputs.as_ref();
                    if let Some(policy) = &plan.nodes[index].delivery_review {
                        return outputs.is_some_and(|outputs| {
                            review::delivery_matches(policy, outputs, expected)
                        });
                    }
                    outputs
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
        GateCondition::Ci { .. } | GateCondition::Conflict { .. } => {
            unreachable!("CI and conflict gates are observed by the host")
        }
        GateCondition::Budget { .. } => {
            // An explicit check must fail closed even if admission only warns.
            let mut budget = budget.clone();
            budget.unknown_usage = crate::config::UnknownUsage::Pause;
            budget_reason(checkpoint, &budget)
        }
    }
}
