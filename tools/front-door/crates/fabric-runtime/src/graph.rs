//! Bounded DAG execution for Fabric graphs.
//!
//! `FileJournal` saves a complete canonical checkpoint with the OS layer's safe
//! filesystem API. `AttemptOwner` is implemented by composition over run. Its
//! key is idempotent: a repeated key observes the existing attempt instead of
//! launching a duplicate provider process.

use provenant_core::{CancelToken, Clock, ProjectId, StateRoot, WaitBudget};
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode, PublishMode};
use provenant_records::{CanonicalError, CanonicalV1, DecodeError, Limits, decode_v1};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use thiserror::Error;

const MAX_CHECKPOINT_BYTES: u64 = 16_777_216;
const MAX_ADVANCE_ERROR_BYTES: usize = 2_048;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<GraphLimitOverrides>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_branch: Option<String>,
    /// A graph may open one PR after all its nodes succeed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<provenant_records::run::DeliveryMode>,
    pub schema_version: u64,
    pub graph_id: String,
    pub nodes: Vec<GraphNode>,
}

/// Graph-specific limits, bounded by the host's configured ceiling.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphLimitOverrides {
    pub nodes: Option<usize>,
    pub edges: Option<usize>,
    pub routes_per_node: Option<usize>,
    pub node_prompt_bytes: Option<usize>,
    pub graph_prompt_bytes: Option<usize>,
    pub expansion_depth: Option<usize>,
    pub fan_out_width: Option<usize>,
    pub concurrency: Option<usize>,
}

impl GraphLimitOverrides {
    pub fn resolve(
        &self,
        config: &crate::config::GraphConfig,
    ) -> Result<crate::config::GraphLimits, String> {
        let mut limits = config.limits.clone();
        macro_rules! apply {
            ($($field:ident),+) => {$({
                if let Some(value) = self.$field {
                    if value == 0 || value > config.limit_ceiling.$field {
                        return Err(format!("limits.{}={} in graph document exceeds configured ceiling {} or is zero", stringify!($field), value, config.limit_ceiling.$field));
                    }
                    limits.$field = value;
                }
            })+};
        }
        apply!(
            nodes,
            edges,
            routes_per_node,
            node_prompt_bytes,
            graph_prompt_bytes,
            expansion_depth,
            fan_out_width
        );
        if let Some(value) = self.concurrency {
            if value == 0
                || config
                    .limit_ceiling
                    .concurrency
                    .is_some_and(|ceiling| value > ceiling)
            {
                return Err(format!(
                    "limits.concurrency={value} in graph document exceeds configured ceiling {:?} or is zero",
                    config.limit_ceiling.concurrency
                ));
            }
            limits.concurrency = Some(value);
        }
        Ok(limits)
    }
}

/// Legacy Fabric route wire shape. Exactly one of `model` and `alias` is
/// required. `adapter` is required for a model and optional for an alias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphRoute {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// Host-only inputs retained for advancement after the submitting process exits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphExecution {
    /// Integration head pinned before the graph's first node is admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_base: Option<provenant_core::GitOid>,
    pub directory: std::path::PathBuf,
    pub document: GraphDocument,
}

/// `route` remains the one-route form. A node may instead provide ordered
/// `routes`; supplying both is invalid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphNode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<provenant_records::run::DeliveryMode>,
    pub access: AccessMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<GraphRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<Vec<GraphRoute>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Require this exact, case-sensitive line (ignoring surrounding whitespace)
    /// in a successful result. A missing line fails the node without fallback.
    pub required_verdict: Option<String>,
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
#[serde(deny_unknown_fields)]
pub struct GraphCheckpoint {
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub delivery_required: bool,
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub delivered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<String>,
    /// Durable admission fence. Resume observes the same attempt keys.
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<String>,
    /// Usage fields unavailable for settled attempts, including partial telemetry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_usage: Vec<String>,
    /// Durable, deduplicated observations of unavailable attempt telemetry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_events: Vec<UnknownUsageEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_seconds: Option<u64>,
    pub schema_version: u64,
    pub graph_id: String,
    /// Digest of the validated, normalised plan, including prompt bytes.
    pub plan_digest: String,
    /// Nodes are stored in topological order.
    pub nodes: Vec<NodeProgress>,
}

fn not_delivered(delivered: &bool) -> bool {
    !delivered
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Retain the writer checkout for chair inspection after a failed launch.
    pub worktree: Option<String>,
    pub id: String,
    pub state: NodeState,
    /// One durable row per route actually tried, in route order.
    pub attempts: Vec<GraphAttempt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphAttempt {
    pub launched: bool,
    pub key: AttemptKey,
    pub route: GraphRoute,
    pub state: AttemptState,
    pub attempt_ref: Option<String>,
    pub outcome: Option<AttemptOutcome>,
    /// Actor that first submitted this attempt. Run-store operation replay is
    /// actor-scoped, so a resume submits under it.
    pub launch_actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AttemptUsage>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptUsage {
    pub tokens: Option<u64>,
    pub cost_micro_usd: Option<u64>,
    pub wall_time_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub tokens_partial: bool,
    #[serde(default, skip_serializing_if = "not_delivered")]
    pub cost_partial: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnknownUsageEvent {
    pub node_id: String,
    pub route_index: u16,
    pub metric: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptKey {
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Starting,
    Running,
    Unresolved,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Succeeded,
    TaskFailed,
    OtherFailure,
    AuthRequired,
    UsageLimited,
    AdapterUnavailable,
}

impl AttemptOutcome {
    /// Only these three typed terminal results advance to the next route.
    fn allows_route_fallback(self, access: AccessMode, launched: bool) -> bool {
        matches!(
            self,
            Self::AuthRequired | Self::UsageLimited | Self::AdapterUnavailable
        ) && (!launched || access == AccessMode::ReadOnly)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttemptObservation {
    /// The same idempotency key has an accepted, nonterminal run.
    Pending { attempt_ref: Option<String> },
    /// Evidence is incomplete. Keep the attempt unresolved and observe the same
    /// key again; never consume another route on this result.
    Unknown { attempt_ref: Option<String> },
    Terminal {
        attempt_ref: Option<String>,
        outcome: AttemptOutcome,
        launched: bool,
        result: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphStatus {
    Pending,
    Complete,
    Cancelled,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphRun {
    pub status: GraphStatus,
    pub checkpoint: GraphCheckpoint,
    /// Why the last automatic advance failed, as recorded for `status`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advance_error: Option<String>,
}

/// What the checkpoint already knows about an attempt.
#[derive(Clone, Copy, Debug)]
pub struct Replay<'a> {
    pub delivery: Option<&'a provenant_records::run::DeliveryMode>,
    /// Reference to the accepted attempt, once saved.
    pub stored_ref: Option<&'a str>,
    /// Actor to submit under when no reference was saved.
    pub launch_actor: &'a str,
}

/// User-facing content for a graph node's submitted task.
pub struct NodeInput<'a> {
    pub title: &'a str,
    pub prompt: &'a str,
}

/// `observe_or_start` must be idempotent by `AttemptKey`. It starts the run
/// only when that key has no accepted operation, and otherwise observes it.
/// A `stored_ref` from the checkpoint names an accepted attempt: observe it as
/// is, never resubmit. Without one, the submission replays under `launch_actor`
/// (the actor recorded before the first submit) so a changed actor cannot
/// launch duplicate work. Provider launch and terminal publication remain run's responsibility.
pub trait AttemptOwner {
    type Error: Error + 'static;

    /// The actor this owner submits as, recorded before the first submit.
    fn actor(&self) -> String;
    fn wall_time_seconds(&self) -> u64 {
        0
    }
    fn deliver_graph(
        &mut self,
        _document: &GraphDocument,
        _checkpoint: &GraphCheckpoint,
    ) -> Result<Option<String>, Self::Error> {
        Ok(None)
    }

    fn usage(&mut self, _attempt_ref: &str) -> Result<AttemptUsage, Self::Error> {
        Ok(AttemptUsage::default())
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
/// fences concurrent writers for this graph.
pub trait GraphJournal {
    type Error: Error + 'static;

    fn load(&mut self, graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error>;
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error>;
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("invalid graph id for checkpoint path")]
    InvalidGraphId,
    #[error("journal was opened for a different graph")]
    GraphMismatch,
    #[error("graph checkpoint exceeds its byte limit")]
    TooLarge,
    #[error("checkpoint is visible but directory durability is unconfirmed")]
    DurabilityUnconfirmed(#[source] FsError),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("checkpoint file read failed")]
    Io(#[from] std::io::Error),
    #[error("unsupported graph checkpoint envelope")]
    BadEnvelope,
    #[error("graph checkpoint digest does not match its contents")]
    BadDigest,
}

/// Safe, atomic checkpoint file journal. The held owner lock fences concurrent
/// graph writers; all filesystem access after open is descriptor-relative.
pub struct FileJournal {
    graph_id: String,
    directory: Directory,
    execution: Option<GraphExecution>,
    _lock: LockGuard,
}

impl FileJournal {
    /// A successful explicit delivery reopens only nodes fenced by that attempt.
    pub fn retry_delivered_attempt(
        root: &StateRoot,
        project: ProjectId,
        attempt: provenant_core::AttemptId,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), JournalError> {
        let attempt = attempt.to_string();
        for id in Self::graph_ids(root, project)? {
            let snapshot = match Self::read_checkpoint(root, project, &id) {
                Ok(Some(checkpoint)) => checkpoint,
                Ok(None) => continue,
                Err(error) => {
                    Self::set_advance_error(root, project, &id, Some(&error.to_string()))?;
                    continue;
                }
            };
            if snapshot.cancelled
                || !snapshot.nodes.iter().any(|node| {
                    node.state == NodeState::Failed
                        && node.attempts.last().is_some_and(|last| {
                            last.outcome == Some(AttemptOutcome::OtherFailure)
                                && last
                                    .attempt_ref
                                    .as_deref()
                                    .and_then(|reference| reference.rsplit('/').next())
                                    == Some(attempt.as_str())
                        })
                })
            {
                continue;
            }
            let mut journal = Self::open(root, project, &id, budget, cancel, clock)?;
            let Some(mut checkpoint) = journal.load(&id)? else {
                continue;
            };
            if checkpoint.cancelled {
                continue;
            }
            let mut changed = false;
            for node in &mut checkpoint.nodes {
                if node.state != NodeState::Failed {
                    continue;
                }
                if let Some(last) = node.attempts.last_mut()
                    && last.outcome == Some(AttemptOutcome::OtherFailure)
                    && last
                        .attempt_ref
                        .as_deref()
                        .and_then(|reference| reference.rsplit('/').next())
                        == Some(attempt.as_str())
                {
                    node.state = NodeState::Running;
                    last.state = AttemptState::Starting;
                    last.outcome = None;
                    changed = true;
                }
            }
            if changed {
                for node in &mut checkpoint.nodes {
                    if node.state == NodeState::Blocked {
                        node.state = NodeState::Pending;
                    }
                }
                journal.save(&checkpoint)?;
            }
        }
        Ok(())
    }
    pub fn open(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<Self, JournalError> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId);
        }
        let root_dir = Directory::open_anchor(root.as_path())?;
        let fabric_dir = open_or_create(&root_dir, OsStr::new("fabric"))?;
        let graphs_dir = open_or_create(&fabric_dir, OsStr::new("graphs"))?;
        let graphs_dir = open_or_create(&graphs_dir, OsStr::new(&project.to_string()))?;
        let lock_name = format!("graph-{graph_id}.lock");
        let lock = graphs_dir.lock(
            OsStr::new(&lock_name),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )?;
        let execution = load_checkpoint(&graphs_dir, graph_id)?.map(|stored| stored.execution);
        Ok(Self {
            graph_id: graph_id.to_owned(),
            directory: graphs_dir,
            execution,
            _lock: lock,
        })
    }

    pub fn bind(&mut self, execution: GraphExecution) -> Result<(), JournalError> {
        if execution.document.graph_id != self.graph_id || !execution.directory.is_absolute() {
            return Err(JournalError::GraphMismatch);
        }
        if self.integration_base().is_some()
            && self.integration_base() != execution.integration_base
        {
            return Err(JournalError::GraphMismatch);
        }
        self.execution = Some(execution);
        Ok(())
    }

    pub fn integration_base(&self) -> Option<provenant_core::GitOid> {
        self.execution
            .as_ref()
            .and_then(|execution| execution.integration_base)
    }

    /// Discover journals without creating directories. Each is read separately
    /// so a corrupt graph need not prevent another graph from advancing.
    pub fn graph_ids(root: &StateRoot, project: ProjectId) -> Result<Vec<String>, JournalError> {
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut ids = Vec::new();
        for entry in directory.entries()? {
            let name = entry?;
            if let Some(id) = name
                .to_str()
                .and_then(|name| name.strip_prefix("graph-"))
                .and_then(|name| name.strip_suffix(".json"))
                && valid_identifier(id)
            {
                ids.push(id.to_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    pub fn pending_execution(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphExecution>, JournalError> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId);
        }
        let directory = graph_directory(root, project)?;
        Ok(load_checkpoint(&directory, graph_id)?.and_then(|stored| {
            (!stored.checkpoint.cancelled
                && (stored
                    .checkpoint
                    .nodes
                    .iter()
                    .any(|node| matches!(node.state, NodeState::Pending | NodeState::Running))
                    || !stored.checkpoint.delivered
                        && stored.execution.document.delivery
                            == Some(provenant_records::run::DeliveryMode::Pr)
                        && stored
                            .checkpoint
                            .nodes
                            .iter()
                            .all(|node| node.state == NodeState::Succeeded)))
            .then_some(stored.execution)
        }))
    }

    /// Read an atomic checkpoint snapshot without creating directories or
    /// acquiring the writer lock. Concurrent saves publish whole files.
    pub fn read_checkpoint(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphCheckpoint>, JournalError> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId);
        }
        let directory = graph_directory(root, project);
        match directory {
            Ok(directory) => {
                Ok(load_checkpoint(&directory, graph_id)?.map(|stored| stored.checkpoint))
            }
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Record why an automatic advance failed, or clear the record with
    /// `None`. It takes no graph lock, because the failure may be that lock.
    pub fn set_advance_error(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
        error: Option<&str>,
    ) -> Result<(), JournalError> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId);
        }
        let directory = graph_directory(root, project)?;
        let name = advance_error_name(graph_id);
        let Some(error) = error else {
            return match directory.unlink(&name) {
                Err(error) if error.is_not_found() => Ok(()),
                other => Ok(other?),
            };
        };
        let mut end = error.len().min(MAX_ADVANCE_ERROR_BYTES);
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        directory.atomic_write(
            &name,
            ".graph-advance-error.",
            &error.as_bytes()[..end],
            PublishMode::Replace,
        )?;
        Ok(())
    }

    /// The recorded failure of the last automatic advance, if any.
    pub fn read_advance_error(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<String>, JournalError> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId);
        }
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let file = match directory.open_read(&advance_error_name(graph_id)) {
            Ok(file) => file,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_ADVANCE_ERROR_BYTES as u64)
            .read_to_end(&mut bytes)?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    fn checkpoint_name(&self) -> OsString {
        format!("graph-{}.json", self.graph_id).into()
    }
}

impl GraphJournal for FileJournal {
    type Error = JournalError;

    fn load(&mut self, graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        if graph_id != self.graph_id {
            return Err(JournalError::GraphMismatch);
        }
        Ok(load_checkpoint(&self.directory, graph_id)?.map(|stored| stored.checkpoint))
    }

    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        if checkpoint.graph_id != self.graph_id {
            return Err(JournalError::GraphMismatch);
        }
        let execution = self.execution.as_ref().ok_or(JournalError::BadEnvelope)?;
        let stored = StoredCheckpoint {
            schema_version: 1,
            digest: CanonicalV1::record_digest(
                "fabric-graph-checkpoint",
                2,
                &(checkpoint, execution),
            )?
            .to_string(),
            checkpoint: checkpoint.clone(),
            execution: execution.clone(),
        };
        let bytes = CanonicalV1::bytes(&stored)?;
        if bytes.as_bytes().len() as u64 > MAX_CHECKPOINT_BYTES {
            return Err(JournalError::TooLarge);
        }
        self.directory
            .atomic_write(
                &self.checkpoint_name(),
                ".graph-checkpoint.",
                bytes.as_bytes(),
                PublishMode::Replace,
            )?
            .require_durable()
            .map_err(JournalError::DurabilityUnconfirmed)
    }
}

fn advance_error_name(graph_id: &str) -> OsString {
    format!("graph-{graph_id}.advance-error").into()
}

fn graph_directory(root: &StateRoot, project: ProjectId) -> Result<Directory, FsError> {
    let root = Directory::open_anchor(root.as_path())?;
    let fabric = root.open_verified_directory(OsStr::new("fabric"))?;
    let graphs = fabric.open_verified_directory(OsStr::new("graphs"))?;
    graphs.open_verified_directory(OsStr::new(&project.to_string()))
}

fn load_checkpoint(
    directory: &Directory,
    graph_id: &str,
) -> Result<Option<StoredCheckpoint>, JournalError> {
    let file = match directory.open_read(OsStr::new(&format!("graph-{graph_id}.json"))) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    let mut reader = file.take(MAX_CHECKPOINT_BYTES + 1);
    reader.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CHECKPOINT_BYTES {
        return Err(JournalError::TooLarge);
    }
    let stored: StoredCheckpoint = decode_v1(
        bytes.as_slice(),
        Limits {
            max_bytes: MAX_CHECKPOINT_BYTES,
            max_depth: 64,
            max_members: 100_000,
        },
    )?;
    if stored.schema_version != 1 {
        return Err(JournalError::BadEnvelope);
    }
    let digest = CanonicalV1::record_digest(
        "fabric-graph-checkpoint",
        2,
        &(&stored.checkpoint, &stored.execution),
    )?;
    if digest.to_string() != stored.digest {
        return Err(JournalError::BadDigest);
    }
    if stored.checkpoint.graph_id != graph_id
        || stored.execution.document.graph_id != graph_id
        || !stored.execution.directory.is_absolute()
    {
        return Err(JournalError::GraphMismatch);
    }
    Ok(Some(stored))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCheckpoint {
    schema_version: u64,
    digest: String,
    checkpoint: GraphCheckpoint,
    execution: GraphExecution,
}

fn open_or_create(directory: &Directory, name: &OsStr) -> Result<Directory, FsError> {
    match directory.open_verified_directory(name) {
        Ok(child) => Ok(child),
        Err(error) if error.is_not_found() => {
            match directory.mkdir(name, 0o700) {
                Ok(()) => directory.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error),
            }
            directory.open_verified_directory(name)
        }
        Err(error) => Err(error),
    }
}

#[derive(Debug, Error)]
pub enum GraphError<RunError: Error + 'static, JournalError: Error + 'static> {
    #[error("invalid graph: {0}")]
    Invalid(&'static str),
    #[error("invalid extension: {0}")]
    Extension(String),
    #[error("graph checkpoint does not match the submitted plan")]
    PlanChanged,
    #[error("graph checkpoint is inconsistent or unsupported")]
    BadCheckpoint,
    #[error("cannot canonically encode the validated graph plan")]
    Canonical(#[source] CanonicalError),
    #[error("run attempt owner failed: {0}")]
    Run(#[source] RunError),
    #[error("graph journal failed: {0}")]
    Journal(#[source] JournalError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct PlannedGraph {
    #[serde(skip_serializing_if = "Option::is_none")]
    integration_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<provenant_records::run::DeliveryMode>,
    schema_version: u64,
    graph_id: String,
    nodes: Vec<PlannedNode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct PlannedNode {
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<provenant_records::run::DeliveryMode>,
    access: AccessMode,
    worktree: Option<String>,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    prompt: String,
    depends_on: Vec<String>,
    routes: Vec<GraphRoute>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_verdict: Option<String>,
}

/// Advance all currently ready nodes once. Independent nodes can progress
/// while a dependency is still running. Calls are bounded by the document caps
/// and are safe to repeat after a crash because every route has a stable key.
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
        &crate::config::GraphLimits::default(),
        &crate::config::GraphBudget::default(),
    )
}

/// Explicitly reopen graph admission without changing any attempt identity.
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
    ceiling: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    advance_inner(
        document,
        owner,
        journal,
        (limits, ceiling),
        budget,
        false,
        &[],
    )
}

pub fn resume_with_budget<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    limits: &crate::config::GraphLimits,
    ceiling: &crate::config::GraphLimits,
    budget: &crate::config::GraphBudget,
    extensions: &[String],
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    advance_inner(
        document,
        owner,
        journal,
        (limits, ceiling),
        budget,
        true,
        extensions,
    )
}

fn advance_inner<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    (limits, ceiling): (&crate::config::GraphLimits, &crate::config::GraphLimits),
    budget: &crate::config::GraphBudget,
    resume: bool,
    extensions: &[String],
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    let unbounded = crate::config::GraphLimits {
        nodes: usize::MAX,
        edges: usize::MAX,
        routes_per_node: u16::MAX as usize,
        node_prompt_bytes: usize::MAX,
        graph_prompt_bytes: usize::MAX,
        ..limits.clone()
    };
    let (plan, positions) =
        validate_plan_with_limits(document, &unbounded).map_err(GraphError::Invalid)?;
    let plan_digest = CanonicalV1::record_digest("fabric-graph-plan", 1, &plan)
        .map_err(GraphError::Canonical)?
        .to_string();

    let mut checkpoint = match journal.load(&plan.graph_id).map_err(GraphError::Journal)? {
        Some(checkpoint) => {
            validate_checkpoint(&checkpoint, &plan, &plan_digest)?;
            checkpoint
        }
        None => {
            let checkpoint = GraphCheckpoint {
                delivery_required: document.delivery
                    == Some(provenant_records::run::DeliveryMode::Pr),
                delivered: false,
                pull_request: None,
                schema_version: 1,
                cancelled: false,
                pause_reason: None,
                unknown_usage: Vec::new(),
                usage_events: Vec::new(),
                extensions: Vec::new(),
                started_at_seconds: Some(owner.wall_time_seconds()),
                graph_id: plan.graph_id.clone(),
                plan_digest,
                nodes: plan
                    .nodes
                    .iter()
                    .map(|node| NodeProgress {
                        id: node.id.clone(),
                        title: Some(node.title.as_deref().unwrap_or(&node.id).to_owned()),
                        worktree: node.worktree.clone(),
                        state: NodeState::Pending,
                        attempts: Vec::new(),
                    })
                    .collect(),
            };
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
            checkpoint
        }
    };

    let mut effective_budget = budget.clone();
    let mut effective_limits = limits.clone();
    for (extension, replay) in checkpoint.extensions.iter().map(|e| (e, true)).chain(
        extensions
            .iter()
            .map(|e| (e, checkpoint.extensions.contains(e))),
    ) {
        if extension.starts_with("limits.") {
            apply_limit_extension(&mut effective_limits, ceiling, extension, replay)
                .map_err(GraphError::Extension)?;
        } else {
            apply_extension(&mut effective_budget, extension).map_err(GraphError::Extension)?;
        }
    }
    if resume {
        for extension in extensions {
            if !checkpoint.extensions.contains(extension) {
                checkpoint.extensions.push(extension.clone());
            }
        }
        checkpoint.pause_reason = None;
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }

    if let Some(reason) =
        structural_limit_reason(document, &plan, &effective_limits, &checkpoint.extensions)
    {
        checkpoint.pause_reason = Some(reason);
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }

    if checkpoint.cancelled {
        if !resume {
            return Ok(GraphRun {
                status: GraphStatus::Cancelled,
                checkpoint,
                advance_error: None,
            });
        }
        checkpoint.cancelled = false;
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }

    for node_index in 0..plan.nodes.len() {
        if matches!(
            checkpoint.nodes[node_index].state,
            NodeState::Succeeded | NodeState::Failed | NodeState::Blocked
        ) {
            continue;
        }

        let parent_states = plan.nodes[node_index]
            .depends_on
            .iter()
            .map(|parent| {
                positions
                    .get(parent)
                    .map(|index| checkpoint.nodes[*index].state)
                    .ok_or(GraphError::Invalid("dependency position"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        if parent_states
            .iter()
            .any(|state| matches!(state, NodeState::Failed | NodeState::Blocked))
        {
            checkpoint.nodes[node_index].state = NodeState::Blocked;
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
            continue;
        }
        if parent_states
            .iter()
            .any(|state| *state != NodeState::Succeeded)
        {
            continue;
        }

        let node = &plan.nodes[node_index];
        loop {
            let progress = &checkpoint.nodes[node_index];
            let route_index = match progress.attempts.last() {
                None => 0,
                Some(last) if last.state != AttemptState::Terminal => {
                    usize::from(last.key.route_index)
                }
                Some(last)
                    if last.outcome.is_some_and(|outcome| {
                        outcome.allows_route_fallback(node.access, last.launched)
                    }) =>
                {
                    progress.attempts.len()
                }
                Some(_) => break,
            };
            let route = node
                .routes
                .get(route_index)
                .ok_or(GraphError::BadCheckpoint)?
                .clone();
            let key = AttemptKey {
                graph_id: plan.graph_id.clone(),
                node_id: node.id.clone(),
                route_index: u16::try_from(route_index)
                    .map_err(|_| GraphError::Invalid("route count"))?,
            };

            let needs_record = checkpoint.nodes[node_index]
                .attempts
                .last()
                .is_none_or(|last| last.state == AttemptState::Terminal);
            if needs_record {
                if effective_limits.concurrency.is_some_and(|cap| {
                    checkpoint
                        .nodes
                        .iter()
                        .enumerate()
                        .filter(|(index, node)| {
                            *index != node_index && node.state == NodeState::Running
                        })
                        .count()
                        >= cap
                }) {
                    break;
                }
                update_usage_warnings(&mut checkpoint, &effective_budget);
                if let Some(reason) = checkpoint
                    .pause_reason
                    .clone()
                    .or_else(|| budget_reason(&checkpoint, &effective_budget))
                {
                    checkpoint.pause_reason = Some(reason);
                    journal.save(&checkpoint).map_err(GraphError::Journal)?;
                    break;
                }
                checkpoint.nodes[node_index].attempts.push(GraphAttempt {
                    key: key.clone(),
                    route: route.clone(),
                    state: AttemptState::Starting,
                    attempt_ref: None,
                    outcome: None,
                    launched: true,
                    launch_actor: owner.actor(),
                    usage: None,
                });
                checkpoint.nodes[node_index].state = NodeState::Running;
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
            let observation = owner
                .observe_or_start(
                    &key,
                    NodeInput {
                        title: node.title.as_deref().unwrap_or(&node.id),
                        prompt: &node.prompt,
                    },
                    &route,
                    node.access,
                    node.worktree.as_deref(),
                    Replay {
                        delivery: node.delivery.as_ref(),
                        stored_ref: stored_ref.as_deref(),
                        launch_actor: &launch_actor,
                    },
                )
                .map_err(GraphError::Run)?;
            let usage = if let AttemptObservation::Terminal {
                attempt_ref: Some(reference),
                ..
            } = &observation
            {
                Some(owner.usage(reference).map_err(GraphError::Run)?)
            } else {
                None
            };
            let should_continue = {
                let progress = &mut checkpoint.nodes[node_index];
                let attempt = progress
                    .attempts
                    .last_mut()
                    .ok_or(GraphError::BadCheckpoint)?;
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
                    } => {
                        attempt.usage = usage;
                        attempt.launched = launched;
                        attempt.state = AttemptState::Terminal;
                        attempt.attempt_ref = attempt_ref;
                        // Provider success alone does not satisfy an explicit result contract.
                        let outcome = if outcome == AttemptOutcome::Succeeded
                            && node.required_verdict.as_ref().is_some_and(|required| {
                                !result.as_deref().is_some_and(|text| {
                                    text.lines().any(|line| line.trim() == required.trim())
                                })
                            }) {
                            AttemptOutcome::TaskFailed
                        } else {
                            outcome
                        };
                        attempt.outcome = Some(outcome);
                        match outcome {
                            AttemptOutcome::Succeeded => {
                                progress.state = NodeState::Succeeded;
                                false
                            }
                            AttemptOutcome::AuthRequired
                            | AttemptOutcome::UsageLimited
                            | AttemptOutcome::AdapterUnavailable => {
                                if !outcome.allows_route_fallback(node.access, launched)
                                    || route_index + 1 == node.routes.len()
                                {
                                    progress.state = NodeState::Failed;
                                    false
                                } else {
                                    true
                                }
                            }
                            AttemptOutcome::TaskFailed | AttemptOutcome::OtherFailure => {
                                progress.state = NodeState::Failed;
                                false
                            }
                        }
                    }
                }
            };
            update_usage_warnings(&mut checkpoint, &effective_budget);
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
            if !should_continue {
                break;
            }
        }
    }

    let all_settled = checkpoint.nodes.iter().all(|node| {
        matches!(
            node.state,
            NodeState::Succeeded | NodeState::Failed | NodeState::Blocked
        )
    });
    if all_settled && checkpoint.pause_reason.take().is_some() {
        journal.save(&checkpoint).map_err(GraphError::Journal)?;
    }
    if document.delivery == Some(provenant_records::run::DeliveryMode::Pr)
        && !checkpoint.delivered
        && checkpoint
            .nodes
            .iter()
            .all(|node| node.state == NodeState::Succeeded)
    {
        checkpoint.pull_request = owner
            .deliver_graph(document, &checkpoint)
            .map_err(GraphError::Run)?;
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
    Ok(GraphRun {
        status,
        checkpoint,
        advance_error: None,
    })
}

fn apply_limit_extension(
    limits: &mut crate::config::GraphLimits,
    ceiling: &crate::config::GraphLimits,
    extension: &str,
    replay: bool,
) -> Result<(), String> {
    let (key, value) = extension
        .split_once('=')
        .ok_or_else(|| format!("{extension}: expected KEY=VALUE"))?;
    let value: usize = value
        .parse()
        .map_err(|_| format!("{extension}: invalid limit value"))?;
    if value == 0 {
        return Err(format!("{extension}: limit must be positive"));
    }
    if key == "limits.concurrency" {
        if !replay && ceiling.concurrency.is_some_and(|cap| value > cap) {
            return Err(format!(
                "{extension}: exceeds ceiling {:?}",
                ceiling.concurrency
            ));
        }
        let value = ceiling.concurrency.map_or(value, |cap| value.min(cap));
        if let Some(old) = limits.concurrency.as_mut() {
            *old = (*old).max(value);
        }
        return Ok(());
    }
    let (slot, cap) = match key {
        "limits.nodes" => (&mut limits.nodes, ceiling.nodes),
        "limits.edges" => (&mut limits.edges, ceiling.edges),
        "limits.routes_per_node" => (&mut limits.routes_per_node, ceiling.routes_per_node),
        "limits.node_prompt_bytes" => (&mut limits.node_prompt_bytes, ceiling.node_prompt_bytes),
        "limits.graph_prompt_bytes" => (&mut limits.graph_prompt_bytes, ceiling.graph_prompt_bytes),
        "limits.expansion_depth" => (&mut limits.expansion_depth, ceiling.expansion_depth),
        "limits.fan_out_width" => (&mut limits.fan_out_width, ceiling.fan_out_width),
        _ => return Err(format!("{extension}: unknown limit key")),
    };
    if !replay && value > cap {
        return Err(format!("{extension}: exceeds ceiling {cap}"));
    }
    *slot = (*slot).max(value.min(cap));
    Ok(())
}

fn structural_limit_reason(
    document: &GraphDocument,
    plan: &PlannedGraph,
    limits: &crate::config::GraphLimits,
    extensions: &[String],
) -> Option<String> {
    let check = |key: &str, actual: usize, max: usize| {
        let overridden = document.limits.as_ref().is_some_and(|overrides| match key {
            "nodes" => overrides.nodes.is_some(),
            "edges" => overrides.edges.is_some(),
            "routes_per_node" => overrides.routes_per_node.is_some(),
            "node_prompt_bytes" => overrides.node_prompt_bytes.is_some(),
            "graph_prompt_bytes" => overrides.graph_prompt_bytes.is_some(),
            "expansion_depth" => overrides.expansion_depth.is_some(),
            "fan_out_width" => overrides.fan_out_width.is_some(),
            _ => false,
        });
        let extension_prefix = format!("limits.{key}=");
        let source = if extensions
            .iter()
            .any(|entry| entry.starts_with(&extension_prefix))
        {
            "checkpoint extension"
        } else if overridden {
            "graph document"
        } else {
            "domains.fabric.graph.limits"
        };
        (actual > max).then(|| format!("limits.{key}={max} reached ({actual}); set by {source}"))
    };
    check("nodes", document.nodes.len(), limits.nodes)
        .or_else(|| {
            check(
                "edges",
                document
                    .nodes
                    .iter()
                    .map(|node| node.depends_on.len())
                    .sum(),
                limits.edges,
            )
        })
        .or_else(|| {
            document.nodes.iter().find_map(|node| {
                check(
                    "routes_per_node",
                    node.routes
                        .as_ref()
                        .map_or(usize::from(node.route.is_some()), Vec::len),
                    limits.routes_per_node,
                )
            })
        })
        .or_else(|| {
            document.nodes.iter().find_map(|node| {
                check(
                    "node_prompt_bytes",
                    node.prompt.len(),
                    limits.node_prompt_bytes,
                )
            })
        })
        .or_else(|| {
            check(
                "graph_prompt_bytes",
                document.nodes.iter().map(|node| node.prompt.len()).sum(),
                limits.graph_prompt_bytes,
            )
        })
        .or_else(|| {
            let mut children = std::collections::BTreeMap::<&str, usize>::new();
            for node in &plan.nodes {
                for parent in &node.depends_on {
                    *children.entry(parent).or_default() += 1;
                }
            }
            children
                .values()
                .find_map(|width| check("fan_out_width", *width, limits.fan_out_width))
        })
        .or_else(|| {
            let mut depths = std::collections::BTreeMap::<&str, usize>::new();
            for node in &plan.nodes {
                let depth = node
                    .depends_on
                    .iter()
                    .map(|parent| depths[parent.as_str()] + 1)
                    .max()
                    .unwrap_or(0);
                if let Some(reason) = check("expansion_depth", depth, limits.expansion_depth) {
                    return Some(reason);
                }
                depths.insert(&node.id, depth);
            }
            None
        })
}

fn apply_extension(budget: &mut crate::config::GraphBudget, extension: &str) -> Result<(), String> {
    let (key, value) = extension
        .split_once('=')
        .ok_or_else(|| format!("{extension}: expected KEY=VALUE"))?;
    let key = key.strip_prefix("budget.").unwrap_or(key);
    let (scope, field) = key
        .split_once('.')
        .ok_or_else(|| format!("{extension}: unknown budget key"))?;
    let target = match scope {
        "per_node" => &mut budget.per_node,
        "per_graph" => &mut budget.per_graph,
        _ => return Err(format!("{extension}: unknown budget key")),
    };
    match field {
        "tokens" | "wall_time_seconds" => {
            let number: u64 = value
                .parse()
                .map_err(|_| format!("{extension}: invalid budget value"))?;
            let slot = if field == "tokens" {
                &mut target.tokens
            } else {
                &mut target.wall_time_seconds
            };
            if number == 0 {
                return Err(format!("{extension}: budget must be positive"));
            }
            if let Some(old) = slot.as_mut() {
                *old = (*old).max(number);
            }
        }
        "cost_usd" => {
            let number: f64 = value
                .parse()
                .map_err(|_| format!("{extension}: invalid budget value"))?;
            if !number.is_finite() || number <= 0.0 {
                return Err(format!("{extension}: budget must be finite and positive"));
            }
            if let Some(old) = target.cost_usd.as_mut() {
                *old = old.max(number);
            }
        }
        _ => return Err(format!("{extension}: unknown budget key")),
    }
    Ok(())
}

fn budget_reason(
    checkpoint: &GraphCheckpoint,
    budget: &crate::config::GraphBudget,
) -> Option<String> {
    let mut graph_tokens = 0u64;
    let mut graph_cost = 0u64;
    let mut graph_seconds = 0u64;
    let mut graph_unknown = [false; 3];
    for node in &checkpoint.nodes {
        let mut tokens = 0u64;
        let mut cost = 0u64;
        let mut seconds = 0u64;
        let mut unknown = [false; 3];
        for attempt in node
            .attempts
            .iter()
            .filter(|attempt| attempt.state == AttemptState::Terminal && attempt.launched)
        {
            let usage = attempt.usage.as_ref();
            let values = [
                usage.and_then(|value| value.tokens),
                usage.and_then(|value| value.cost_micro_usd),
                usage.and_then(|value| value.wall_time_seconds),
            ];
            for (missing, value) in unknown.iter_mut().zip(values) {
                *missing |= value.is_none();
            }
            unknown[0] |= usage.is_some_and(|value| value.tokens_partial);
            unknown[1] |= usage.is_some_and(|value| value.cost_partial);
            tokens = tokens.saturating_add(values[0].unwrap_or(0));
            cost = cost.saturating_add(values[1].unwrap_or(0));
            seconds = seconds.saturating_add(values[2].unwrap_or(0));
        }
        graph_tokens = graph_tokens.saturating_add(tokens);
        graph_cost = graph_cost.saturating_add(cost);
        graph_seconds = graph_seconds.saturating_add(seconds);
        for (total, missing) in graph_unknown.iter_mut().zip(unknown) {
            *total |= missing;
        }
        if let Some(reason) =
            exceeded("per_node", &budget.per_node, tokens, cost, seconds).or_else(|| {
                unknown_budget_reason("per_node", &budget.per_node, unknown, budget.unknown_usage)
            })
        {
            return Some(format!("node {}: {reason}", node.id));
        }
    }
    exceeded(
        "per_graph",
        &budget.per_graph,
        graph_tokens,
        graph_cost,
        graph_seconds,
    )
    .or_else(|| {
        unknown_budget_reason(
            "per_graph",
            &budget.per_graph,
            graph_unknown,
            budget.unknown_usage,
        )
    })
}

fn update_usage_warnings(checkpoint: &mut GraphCheckpoint, _budget: &crate::config::GraphBudget) {
    let mut warnings = Vec::new();
    let mut events = Vec::new();
    for node in &checkpoint.nodes {
        for attempt in node
            .attempts
            .iter()
            .filter(|attempt| attempt.state == AttemptState::Terminal && attempt.launched)
        {
            let usage = attempt.usage.as_ref();
            for (name, known) in [
                (
                    "tokens",
                    usage.is_some_and(|value| value.tokens.is_some() && !value.tokens_partial),
                ),
                (
                    "cost_usd",
                    usage
                        .is_some_and(|value| value.cost_micro_usd.is_some() && !value.cost_partial),
                ),
                (
                    "wall_time_seconds",
                    usage.is_some_and(|value| value.wall_time_seconds.is_some()),
                ),
            ] {
                if !known {
                    warnings.push(format!(
                        "node {} route {}: {name} usage unknown",
                        node.id, attempt.key.route_index
                    ));
                    events.push(UnknownUsageEvent {
                        node_id: node.id.clone(),
                        route_index: attempt.key.route_index,
                        metric: name.to_owned(),
                    });
                }
            }
        }
    }
    checkpoint.unknown_usage = warnings;
    for event in events {
        if !checkpoint.usage_events.contains(&event) {
            checkpoint.usage_events.push(event);
        }
    }
}

fn unknown_budget_reason(
    scope: &str,
    limit: &crate::config::BudgetLimit,
    unknown: [bool; 3],
    policy: crate::config::UnknownUsage,
) -> Option<String> {
    if policy != crate::config::UnknownUsage::Pause {
        return None;
    }
    [
        ("tokens", limit.tokens.is_some()),
        ("cost_usd", limit.cost_usd.is_some()),
        ("wall_time_seconds", limit.wall_time_seconds.is_some()),
    ]
    .into_iter()
    .zip(unknown)
    .find_map(|((name, configured), missing)| {
        (configured && missing).then(|| format!("{scope}.{name} usage unknown"))
    })
}

fn exceeded(
    scope: &str,
    limit: &crate::config::BudgetLimit,
    tokens: u64,
    cost: u64,
    seconds: u64,
) -> Option<String> {
    if let Some(max) = limit.tokens.filter(|max| tokens >= *max) {
        return Some(format!("{scope}.tokens budget {max} reached ({tokens})"));
    }
    if let Some(max) = limit
        .cost_usd
        .filter(|max| (cost as f64) / 1_000_000.0 >= *max)
    {
        return Some(format!("{scope}.cost_usd budget {max} reached"));
    }
    if let Some(max) = limit.wall_time_seconds.filter(|max| seconds >= *max) {
        return Some(format!(
            "{scope}.wall_time_seconds budget {max} reached ({seconds})"
        ));
    }
    None
}

pub(crate) fn validate_access(
    access: AccessMode,
    worktree: Option<&str>,
) -> Result<(), &'static str> {
    if access == AccessMode::WorktreeWrite
        && worktree.is_none_or(|path| {
            path.is_empty()
                || path.len() > 4096
                || path.contains('\0')
                || std::path::Path::new(path)
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
        })
    {
        return Err("writer worktree");
    }
    if access == AccessMode::ReadOnly && worktree.is_some() {
        return Err("read-only worktree");
    }
    Ok(())
}

#[cfg(test)]
fn validate_plan(
    document: &GraphDocument,
) -> Result<(PlannedGraph, std::collections::BTreeMap<String, usize>), &'static str> {
    validate_plan_with_limits(document, &crate::config::GraphLimits::default())
}

fn validate_plan_with_limits(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> Result<(PlannedGraph, std::collections::BTreeMap<String, usize>), &'static str> {
    if document.schema_version != 1 {
        return Err("schema version");
    }
    use provenant_records::run::DeliveryMode;
    if !matches!(
        document.delivery,
        None | Some(DeliveryMode::None) | Some(DeliveryMode::Pr)
    ) || document.delivery == Some(DeliveryMode::Pr) && document.integration_branch.is_none()
        || document.integration_branch.as_ref().is_some_and(|branch| {
            format!("integrate:{branch}")
                .parse::<DeliveryMode>()
                .is_err()
        })
    {
        return Err("graph delivery requires an integration branch and at most one PR");
    }
    if !valid_identifier(&document.graph_id) {
        return Err("graph id");
    }
    if document.nodes.is_empty() || document.nodes.len() > limits.nodes {
        return Err("node count");
    }

    let mut positions = std::collections::BTreeMap::<String, usize>::new();
    for (index, node) in document.nodes.iter().enumerate() {
        if !valid_identifier(&node.id) || positions.insert(node.id.clone(), index).is_some() {
            return Err("node id");
        }
    }

    let mut edge_count = 0usize;
    let mut prompt_bytes = 0usize;
    let mut nodes = Vec::with_capacity(document.nodes.len());
    for node in &document.nodes {
        if let (Some(integration), Some(DeliveryMode::Integrate(target))) =
            (&document.integration_branch, &node.delivery)
            && integration != target
        {
            return Err("node integration target differs from graph integration branch");
        }
        if node.required_verdict.as_ref().is_some_and(|line| {
            line.trim().is_empty()
                || line.len() > limits.node_prompt_bytes
                || line.contains(['\n', '\r'])
        }) {
            return Err("required verdict must be a non-empty single line");
        }
        if let Some(title) = &node.title {
            provenant_core::validate_task_title(title)?;
        }
        validate_access(node.access, node.worktree.as_deref())?;
        if node.access == AccessMode::ReadOnly
            && !matches!(node.delivery, None | Some(DeliveryMode::None))
        {
            return Err("read-only node cannot deliver");
        }
        if document.delivery == Some(DeliveryMode::Pr) && node.delivery == Some(DeliveryMode::Pr) {
            return Err("graph PR is exclusive with node PRs");
        }
        if node.prompt.is_empty() || node.prompt.len() > limits.node_prompt_bytes {
            return Err("prompt size");
        }
        prompt_bytes = prompt_bytes
            .checked_add(node.prompt.len())
            .ok_or("prompt size")?;
        if prompt_bytes > limits.graph_prompt_bytes {
            return Err("total prompt size");
        }
        edge_count = edge_count
            .checked_add(node.depends_on.len())
            .ok_or("edge count")?;
        if edge_count > limits.edges {
            return Err("edge count");
        }

        let routes = match (&node.route, &node.routes) {
            (Some(route), None) => vec![route.clone()],
            (None, Some(routes)) if !routes.is_empty() => routes.clone(),
            (Some(_), Some(_)) => return Err("route and routes are exclusive"),
            (None, None) | (None, Some(_)) => return Err("route list is empty"),
        };
        if routes.len() > limits.routes_per_node {
            return Err("route count");
        }
        if routes
            .iter()
            .any(|route| route.adapter.is_none() && route.alias.is_none())
        {
            return Err("route requires an adapter or alias");
        }
        if routes
            .iter()
            .any(|route| route.model.is_some() && route.adapter.is_none())
        {
            return Err("model route requires an adapter");
        }
        if routes.iter().any(|route| !route_is_bounded(route)) {
            return Err("route field size");
        }

        let mut seen = std::collections::BTreeSet::new();
        for parent in &node.depends_on {
            if !seen.insert(parent) || parent == &node.id || !positions.contains_key(parent) {
                return Err("dependency");
            }
        }
        let mut depends_on = node.depends_on.clone();
        depends_on.sort();
        nodes.push(PlannedNode {
            delivery: node.delivery.clone(),
            access: node.access,
            worktree: node.worktree.clone(),
            id: node.id.clone(),
            title: node.title.clone(),
            prompt: node.prompt.clone(),
            depends_on,
            routes,
            required_verdict: node.required_verdict.clone(),
        });
    }

    let mut indegree = vec![0usize; nodes.len()];
    let mut children = vec![Vec::new(); nodes.len()];
    for (child, node) in nodes.iter().enumerate() {
        indegree[child] = node.depends_on.len();
        for parent in &node.depends_on {
            let parent_index = *positions.get(parent).ok_or("dependency")?;
            children[parent_index].push(child);
        }
    }
    let mut ready: std::collections::VecDeque<usize> = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(index) = ready.pop_front() {
        order.push(index);
        for child in &children[index] {
            indegree[*child] -= 1;
            if indegree[*child] == 0 {
                ready.push_back(*child);
            }
        }
    }
    if order.len() != nodes.len() {
        return Err("dependency cycle");
    }

    let topological_nodes: Vec<PlannedNode> =
        order.iter().map(|index| nodes[*index].clone()).collect();
    let topological_positions = topological_nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), index))
        .collect();
    Ok((
        PlannedGraph {
            integration_branch: document.integration_branch.clone(),
            delivery: document.delivery.clone(),
            schema_version: 1,
            graph_id: document.graph_id.clone(),
            nodes: topological_nodes,
        },
        topological_positions,
    ))
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn route_is_bounded(route: &GraphRoute) -> bool {
    fn field(value: &str, limit: usize) -> bool {
        !value.is_empty() && value.len() <= limit && !value.contains('\0')
    }
    route
        .adapter
        .as_deref()
        .is_none_or(|value| field(value, 128))
        && (route.model.is_some() ^ route.alias.is_some())
        && route.model.as_deref().is_none_or(|value| field(value, 256))
        && route.alias.as_deref().is_none_or(|value| field(value, 128))
        && route.effort.as_deref().is_none_or(|value| field(value, 64))
        && route
            .account
            .as_deref()
            .is_none_or(|value| field(value, 64))
}

fn validate_checkpoint<R: Error + 'static, J: Error + 'static>(
    checkpoint: &GraphCheckpoint,
    plan: &PlannedGraph,
    digest: &str,
) -> Result<(), GraphError<R, J>> {
    if checkpoint.schema_version != 1
        || checkpoint.graph_id != plan.graph_id
        || checkpoint.delivery_required
            != (plan.delivery == Some(provenant_records::run::DeliveryMode::Pr))
    {
        return Err(GraphError::BadCheckpoint);
    }
    if checkpoint.plan_digest != digest {
        return Err(GraphError::PlanChanged);
    }
    if checkpoint.nodes.len() != plan.nodes.len() {
        return Err(GraphError::BadCheckpoint);
    }
    for (progress, node) in checkpoint.nodes.iter().zip(&plan.nodes) {
        if progress.id != node.id
            || progress.worktree != node.worktree
            || progress.attempts.len() > node.routes.len()
        {
            return Err(GraphError::BadCheckpoint);
        }
        for (index, attempt) in progress.attempts.iter().enumerate() {
            if attempt.key.graph_id != plan.graph_id
                || attempt.key.node_id != node.id
                || usize::from(attempt.key.route_index) != index
                || attempt.route != node.routes[index]
                || (attempt.state == AttemptState::Terminal) != attempt.outcome.is_some()
                || (index + 1 < progress.attempts.len()
                    && (attempt.state != AttemptState::Terminal
                        || !attempt.outcome.is_some_and(|outcome| {
                            outcome.allows_route_fallback(node.access, attempt.launched)
                        })))
            {
                return Err(GraphError::BadCheckpoint);
            }
        }
        let last = progress.attempts.last();
        let state_is_consistent = match progress.state {
            NodeState::Pending | NodeState::Blocked => progress.attempts.is_empty(),
            NodeState::Running => match last {
                Some(attempt) if attempt.state != AttemptState::Terminal => true,
                Some(attempt) => {
                    attempt.outcome.is_some_and(|outcome| {
                        outcome.allows_route_fallback(node.access, attempt.launched)
                    }) && progress.attempts.len() < node.routes.len()
                }
                None => false,
            },
            NodeState::Succeeded => last.is_some_and(|attempt| {
                attempt.state == AttemptState::Terminal
                    && attempt.outcome == Some(AttemptOutcome::Succeeded)
            }),
            NodeState::Failed => last.is_some_and(|attempt| {
                attempt.state == AttemptState::Terminal
                    && match attempt.outcome {
                        Some(AttemptOutcome::TaskFailed | AttemptOutcome::OtherFailure) => true,
                        Some(outcome) if outcome != AttemptOutcome::Succeeded => {
                            !outcome.allows_route_fallback(node.access, attempt.launched)
                                || progress.attempts.len() == node.routes.len()
                        }
                        _ => false,
                    }
            }),
        };
        if !state_is_consistent {
            return Err(GraphError::BadCheckpoint);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::fmt;

    #[derive(Debug)]
    struct NoObservation;

    impl fmt::Display for NoObservation {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("no fake observation remains")
        }
    }
    impl Error for NoObservation {}

    #[derive(Default)]
    struct MemoryJournal(Option<GraphCheckpoint>);

    impl GraphJournal for MemoryJournal {
        type Error = Infallible;

        fn load(&mut self, graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            Ok(self
                .0
                .as_ref()
                .filter(|checkpoint| checkpoint.graph_id == graph_id)
                .cloned())
        }

        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            self.0 = Some(checkpoint.clone());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeOwner {
        deliveries: usize,
        delivery_modes: Vec<Option<provenant_records::run::DeliveryMode>>,
        observations: VecDeque<AttemptObservation>,
        calls: Vec<(AttemptKey, GraphRoute)>,
        titles: Vec<String>,
        permissions: Vec<(AccessMode, Option<String>)>,
        stored_refs: Vec<Option<String>>,
        actor: String,
        launch_actors: Vec<String>,
        usage: AttemptUsage,
    }

    impl AttemptOwner for FakeOwner {
        type Error = NoObservation;

        fn actor(&self) -> String {
            self.actor.clone()
        }
        fn usage(&mut self, _attempt_ref: &str) -> Result<AttemptUsage, Self::Error> {
            Ok(self.usage.clone())
        }
        fn deliver_graph(
            &mut self,
            _document: &GraphDocument,
            _checkpoint: &GraphCheckpoint,
        ) -> Result<Option<String>, Self::Error> {
            self.deliveries += 1;
            Ok(Some("https://example.invalid/pr/1".into()))
        }

        fn observe_or_start(
            &mut self,
            key: &AttemptKey,
            node: NodeInput<'_>,
            route: &GraphRoute,
            access: AccessMode,
            worktree: Option<&str>,
            replay: Replay<'_>,
        ) -> Result<AttemptObservation, Self::Error> {
            self.titles.push(node.title.to_owned());
            self.delivery_modes.push(replay.delivery.cloned());
            self.stored_refs.push(replay.stored_ref.map(str::to_owned));
            self.launch_actors.push(replay.launch_actor.to_owned());
            self.calls.push((key.clone(), route.clone()));
            self.permissions.push((access, worktree.map(str::to_owned)));
            self.observations.pop_front().ok_or(NoObservation)
        }
    }

    fn route(adapter: &str) -> GraphRoute {
        GraphRoute {
            account: None,
            adapter: Some(adapter.to_owned()),
            model: Some(format!("{adapter}/model")),
            alias: None,
            effort: None,
        }
    }

    fn node(id: &str, route: GraphRoute) -> GraphNode {
        GraphNode {
            delivery: None,
            access: AccessMode::ReadOnly,
            worktree: None,
            id: id.to_owned(),
            title: None,
            prompt: format!("work for {id}"),
            depends_on: Vec::new(),
            route: Some(route),
            routes: None,
            required_verdict: None,
        }
    }

    fn graph(nodes: Vec<GraphNode>) -> GraphDocument {
        GraphDocument {
            integration_branch: None,
            delivery: None,
            limits: None,
            schema_version: 1,
            graph_id: "g-test".to_owned(),
            nodes,
        }
    }

    #[test]
    fn structural_limit_pauses_and_extension_resumes() {
        let document = graph(vec![
            node("one", route("first")),
            node("two", route("first")),
        ]);
        let limits = crate::config::GraphLimits {
            nodes: 1,
            ..Default::default()
        };
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                terminal(AttemptOutcome::Succeeded),
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let paused = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(paused.status, GraphStatus::Paused);
        assert!(
            paused
                .checkpoint
                .pause_reason
                .unwrap()
                .contains("limits.nodes=1")
        );
        assert!(owner.calls.is_empty());
        let resumed = resume_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
            &Default::default(),
            &["limits.nodes=2".into()],
        )
        .unwrap();
        assert_eq!(resumed.status, GraphStatus::Complete);
        assert_eq!(resumed.checkpoint.extensions, ["limits.nodes=2"]);
    }

    #[test]
    fn extensions_are_absolute_idempotent_and_respect_changed_ceiling() {
        let mut limits = crate::config::GraphLimits {
            nodes: 1,
            ..Default::default()
        };
        let mut ceiling = crate::config::GraphLimits {
            nodes: 3,
            ..Default::default()
        };
        apply_limit_extension(&mut limits, &ceiling, "limits.nodes=2", false).unwrap();
        apply_limit_extension(&mut limits, &ceiling, "limits.nodes=2", false).unwrap();
        assert_eq!(limits.nodes, 2);
        limits.nodes = 3;
        apply_limit_extension(&mut limits, &ceiling, "limits.nodes=2", true).unwrap();
        assert_eq!(limits.nodes, 3);
        limits.nodes = 1;
        ceiling.nodes = 1;
        apply_limit_extension(&mut limits, &ceiling, "limits.nodes=2", true).unwrap();
        assert_eq!(limits.nodes, 1);
        assert!(
            apply_limit_extension(&mut limits, &ceiling, "limits.nodes=2", false)
                .unwrap_err()
                .contains("limits.nodes=2")
        );
        let mut budget = crate::config::GraphBudget {
            per_graph: crate::config::BudgetLimit {
                tokens: Some(100),
                ..Default::default()
            },
            ..Default::default()
        };
        apply_extension(&mut budget, "per_graph.tokens=200").unwrap();
        apply_extension(&mut budget, "per_graph.tokens=200").unwrap();
        budget.per_graph.tokens = Some(400);
        apply_extension(&mut budget, "per_graph.tokens=200").unwrap();
        assert_eq!(budget.per_graph.tokens, Some(400));
        assert!(
            apply_extension(&mut budget, "per_graph.tokens=0")
                .unwrap_err()
                .contains("per_graph.tokens=0")
        );
    }

    #[test]
    fn retried_resume_replays_extension_after_config_and_ceiling_changes() {
        let document = graph(vec![
            node("one", route("first")),
            node("two", route("first")),
        ]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                terminal(AttemptOutcome::Succeeded),
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let limits = crate::config::GraphLimits {
            nodes: 1,
            ..Default::default()
        };
        let ceiling = crate::config::GraphLimits {
            nodes: 2,
            ..Default::default()
        };
        let extension = ["limits.nodes=2".into()];
        let first = resume_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &ceiling,
            &Default::default(),
            &extension,
        )
        .unwrap();
        assert_eq!(first.status, GraphStatus::Complete);
        let raised = crate::config::GraphLimits {
            nodes: 3,
            ..Default::default()
        };
        let replay = resume_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &raised,
            &raised,
            &Default::default(),
            &extension,
        )
        .unwrap();
        assert_eq!(replay.status, GraphStatus::Complete);
        assert_eq!(replay.checkpoint.extensions, extension);
        let replay = resume_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &limits,
            &Default::default(),
            &extension,
        )
        .unwrap();
        assert_eq!(replay.status, GraphStatus::Complete);
        assert_eq!(owner.calls.len(), 2);
    }

    #[test]
    fn fan_out_and_depth_pause_before_launch() {
        let root = node("root", route("first"));
        let mut a = node("a", route("first"));
        a.depends_on.push("root".into());
        let mut b = node("b", route("first"));
        b.depends_on.push("root".into());
        let mut c = node("c", route("first"));
        c.depends_on.push("a".into());
        let document = graph(vec![root, a, b, c]);
        for (limits, key) in [
            (
                crate::config::GraphLimits {
                    fan_out_width: 1,
                    ..Default::default()
                },
                "fan_out_width",
            ),
            (
                crate::config::GraphLimits {
                    expansion_depth: 1,
                    ..Default::default()
                },
                "expansion_depth",
            ),
        ] {
            let mut owner = FakeOwner::default();
            let run = advance_with_budget(
                &document,
                &mut owner,
                &mut MemoryJournal::default(),
                &limits,
                &Default::default(),
                &Default::default(),
            )
            .unwrap();
            assert_eq!(run.status, GraphStatus::Paused);
            assert!(run.checkpoint.pause_reason.unwrap().contains(key));
            assert!(owner.calls.is_empty());
        }
    }

    #[test]
    fn final_budget_exhaustion_still_delivers() {
        let mut document = graph(vec![node("one", route("first"))]);
        document.delivery = Some(provenant_records::run::DeliveryMode::Pr);
        document.integration_branch = Some("integration".into());
        let mut owner = FakeOwner {
            observations: VecDeque::from([AttemptObservation::Terminal {
                attempt_ref: Some("host/attempt".into()),
                outcome: AttemptOutcome::Succeeded,
                launched: true,
                result: None,
            }]),
            usage: AttemptUsage {
                tokens: Some(10),
                ..Default::default()
            },
            ..Default::default()
        };
        let budget = crate::config::GraphBudget {
            per_graph: crate::config::BudgetLimit {
                tokens: Some(10),
                ..Default::default()
            },
            ..Default::default()
        };
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut MemoryJournal::default(),
            &Default::default(),
            &Default::default(),
            &budget,
        )
        .unwrap();
        assert_eq!(run.status, GraphStatus::Complete);
        assert!(run.checkpoint.delivered);
        assert_eq!(owner.deliveries, 1);
    }

    #[test]
    fn budget_pause_stops_admission_and_extension_resumes() {
        let document = graph(vec![
            node("one", route("first")),
            node("two", route("first")),
        ]);
        let budget = crate::config::GraphBudget {
            per_graph: crate::config::BudgetLimit {
                tokens: Some(10),
                ..Default::default()
            },
            ..Default::default()
        };
        let observation = || AttemptObservation::Terminal {
            attempt_ref: Some("host/attempt".into()),
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: None,
        };
        let mut owner = FakeOwner {
            observations: VecDeque::from([observation(), observation()]),
            usage: AttemptUsage {
                tokens: Some(10),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let paused = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &Default::default(),
            &budget,
        )
        .unwrap();
        assert_eq!(paused.status, GraphStatus::Paused);
        assert_eq!(owner.calls.len(), 1);
        let resumed = resume_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &Default::default(),
            &Default::default(),
            &budget,
            &["per_graph.tokens=30".into()],
        )
        .unwrap();
        assert_eq!(resumed.status, GraphStatus::Complete);
        assert_eq!(owner.calls.len(), 2);
    }

    #[test]
    fn unknown_usage_warns_or_pauses_and_events_are_idempotent() {
        let document = graph(vec![
            node("one", route("first")),
            node("two", route("first")),
        ]);
        let observation = || AttemptObservation::Terminal {
            attempt_ref: Some("host/attempt".into()),
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: None,
        };
        for policy in [
            crate::config::UnknownUsage::Warn,
            crate::config::UnknownUsage::Pause,
        ] {
            let budget = crate::config::GraphBudget {
                per_graph: crate::config::BudgetLimit {
                    tokens: Some(100),
                    ..Default::default()
                },
                unknown_usage: policy,
                ..Default::default()
            };
            let mut owner = FakeOwner {
                observations: VecDeque::from([observation(), observation()]),
                usage: AttemptUsage {
                    tokens: Some(5),
                    tokens_partial: true,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut journal = MemoryJournal::default();
            let run = advance_with_budget(
                &document,
                &mut owner,
                &mut journal,
                &Default::default(),
                &Default::default(),
                &budget,
            )
            .unwrap();
            assert_eq!(
                run.status,
                if policy == crate::config::UnknownUsage::Pause {
                    GraphStatus::Paused
                } else {
                    GraphStatus::Complete
                }
            );
            assert_eq!(
                owner.calls.len(),
                if policy == crate::config::UnknownUsage::Pause {
                    1
                } else {
                    2
                }
            );
            assert!(
                run.checkpoint
                    .unknown_usage
                    .iter()
                    .any(|warning| warning.contains("tokens"))
            );
            let mut checkpoint = run.checkpoint;
            let events = checkpoint.usage_events.clone();
            update_usage_warnings(&mut checkpoint, &budget);
            assert_eq!(checkpoint.usage_events, events);
        }
    }

    #[test]
    fn graph_wall_budget_sums_provider_time_and_preserves_partial_lower_bound() {
        let document = graph(vec![
            node("one", route("first")),
            node("two", route("first")),
        ]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([AttemptObservation::Terminal {
                attempt_ref: Some("host/attempt".into()),
                outcome: AttemptOutcome::Succeeded,
                launched: true,
                result: None,
            }]),
            usage: AttemptUsage {
                tokens: Some(12),
                tokens_partial: true,
                wall_time_seconds: Some(10),
                ..Default::default()
            },
            ..Default::default()
        };
        let budget = crate::config::GraphBudget {
            per_graph: crate::config::BudgetLimit {
                tokens: Some(12),
                wall_time_seconds: Some(100),
                ..Default::default()
            },
            ..Default::default()
        };
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut MemoryJournal::default(),
            &Default::default(),
            &Default::default(),
            &budget,
        )
        .unwrap();
        assert_eq!(run.status, GraphStatus::Paused);
        assert!(
            run.checkpoint
                .pause_reason
                .as_ref()
                .unwrap()
                .contains("tokens")
        );
        let mut checkpoint = run.checkpoint;
        checkpoint.started_at_seconds = Some(0);
        let wall_budget = crate::config::GraphBudget {
            per_graph: crate::config::BudgetLimit {
                wall_time_seconds: Some(11),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(budget_reason(&checkpoint, &wall_budget).is_none());
        checkpoint.nodes[0].attempts[0]
            .usage
            .as_mut()
            .unwrap()
            .wall_time_seconds = Some(11);
        assert!(
            budget_reason(&checkpoint, &wall_budget)
                .unwrap()
                .contains("wall_time_seconds")
        );
    }

    #[test]
    fn node_titles_are_validated_and_sent_to_attempts_and_status() {
        let mut named = node("named", route("first"));
        named.title = Some("Review changes".into());
        let defaulted = node("defaulted", route("first"));
        let document = graph(vec![named, defaulted]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                terminal(AttemptOutcome::Succeeded),
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..FakeOwner::default()
        };
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert_eq!(owner.titles, ["Review changes", "defaulted"]);
        let status = serde_json::to_value(&run).unwrap();
        assert_eq!(status["checkpoint"]["nodes"][0]["title"], "Review changes");
        assert_eq!(status["checkpoint"]["nodes"][1]["title"], "defaulted");

        let mut invalid = node("invalid", route("first"));
        invalid.title = Some("bad\ntitle".into());
        assert_eq!(
            validate_plan(&graph(vec![invalid])).err(),
            Some("task title must be 1 to 256 bytes and contain no control characters")
        );
    }

    fn terminal(outcome: AttemptOutcome) -> AttemptObservation {
        AttemptObservation::Terminal {
            attempt_ref: None,
            outcome,
            launched: true,
            result: None,
        }
    }

    fn terminal_with_result(outcome: AttemptOutcome, result: &str) -> AttemptObservation {
        AttemptObservation::Terminal {
            attempt_ref: None,
            outcome,
            launched: true,
            result: Some(result.to_string()),
        }
    }

    #[test]
    fn graph_delivers_nodes_and_opens_one_final_pr_only_after_success() {
        use provenant_records::run::DeliveryMode;
        let mut writer = node("writer", route("fixture"));
        writer.access = AccessMode::WorktreeWrite;
        writer.worktree = Some(".worktrees/writer".into());
        writer.delivery = Some(DeliveryMode::Integrate("integration".into()));
        let mut document = graph(vec![writer]);
        document.integration_branch = Some("integration".into());
        document.delivery = Some(DeliveryMode::Pr);
        let mut owner = FakeOwner {
            observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let result = advance(&document, &mut owner, &mut journal).unwrap();
        assert!(result.checkpoint.delivered);
        assert_eq!(owner.deliveries, 1);
        assert_eq!(
            owner.delivery_modes,
            vec![Some(DeliveryMode::Integrate("integration".into()))]
        );
        advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(owner.deliveries, 1);
        let mut failed = FakeOwner {
            observations: VecDeque::from([terminal(AttemptOutcome::OtherFailure)]),
            ..Default::default()
        };
        let result = advance(&document, &mut failed, &mut MemoryJournal::default()).unwrap();
        assert!(!result.checkpoint.delivered);
        assert_eq!(failed.deliveries, 0);
        document.nodes[0].access = AccessMode::ReadOnly;
        document.nodes[0].worktree = None;
        assert!(validate_plan(&document).is_err());
    }

    #[test]
    fn legacy_checkpoint_keeps_its_digest_without_delivery_fields() {
        let fixture = include_str!("fixtures/legacy-graph-checkpoint.json");
        for field in [
            "delivery_required",
            "delivered",
            "pull_request",
            "integration_branch",
            "delivery",
        ] {
            assert!(!fixture.contains(&format!("\"{field}\"")));
        }
        let temp = crate::test_support::tempdir().expect("legacy fixture directory");
        std::fs::write(temp.path().join("graph-g-test.json"), fixture)
            .expect("write frozen fixture");
        let directory = Directory::open_anchor(temp.path()).expect("fixture directory");
        let stored = load_checkpoint(&directory, "g-test")
            .expect("verify original digest")
            .expect("legacy checkpoint");
        assert_eq!(
            stored.digest,
            "sha256:9e6f6b7b7d78fee26418cb7ff5ef4e8e1149f7ba733514184259a680d8ecfa77"
        );
        let mut journal = MemoryJournal(Some(stored.checkpoint));
        let mut owner = FakeOwner::default();
        let result = advance(&stored.execution.document, &mut owner, &mut journal)
            .expect("resume legacy graph");
        assert_eq!(result.status, GraphStatus::Complete);
        assert!(owner.calls.is_empty());
        let mut corrupt: serde_json::Value = serde_json::from_str(fixture).expect("fixture JSON");
        corrupt["execution"]["document"]["nodes"][0]["prompt"] = serde_json::json!("tampered");
        std::fs::write(
            temp.path().join("graph-g-test.json"),
            serde_json::to_vec(&corrupt).expect("corrupt JSON"),
        )
        .expect("write corrupt fixture");
        assert!(matches!(
            load_checkpoint(&directory, "g-test"),
            Err(JournalError::BadDigest)
        ));
    }

    #[test]
    fn graph_refuses_node_integration_target_mismatch() {
        use provenant_records::run::DeliveryMode;
        let mut writer = node("writer", route("fixture"));
        writer.access = AccessMode::WorktreeWrite;
        writer.worktree = Some(".worktrees/writer".into());
        writer.delivery = Some(DeliveryMode::Integrate("origin/other".into()));
        let mut document = graph(vec![writer]);
        document.integration_branch = Some("origin/integration".into());
        document.delivery = Some(DeliveryMode::Pr);
        assert_eq!(
            validate_plan(&document).err(),
            Some("node integration target differs from graph integration branch")
        );
    }

    #[test]
    fn tries_routes_in_order_and_persists_each_terminal_attempt() {
        let routes = vec![
            route("first"),
            route("second"),
            route("third"),
            route("fourth"),
        ];
        let document = graph(vec![GraphNode {
            delivery: None,
            access: AccessMode::ReadOnly,
            worktree: None,
            id: "node".to_owned(),
            title: None,
            prompt: "do the work".to_owned(),
            depends_on: Vec::new(),
            route: None,
            routes: Some(routes.clone()),
            required_verdict: None,
        }]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                terminal(AttemptOutcome::AuthRequired),
                terminal(AttemptOutcome::UsageLimited),
                terminal(AttemptOutcome::AdapterUnavailable),
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();

        let run = advance(&document, &mut owner, &mut journal).expect("graph advances");

        assert_eq!(run.status, GraphStatus::Complete);
        assert_eq!(
            owner
                .calls
                .iter()
                .map(|call| call.1.clone())
                .collect::<Vec<_>>(),
            routes
        );
        let attempts = &run.checkpoint.nodes[0].attempts;
        assert_eq!(attempts.len(), 4);
        assert_eq!(attempts[0].outcome, Some(AttemptOutcome::AuthRequired));
        assert_eq!(attempts[1].outcome, Some(AttemptOutcome::UsageLimited));
        assert_eq!(
            attempts[2].outcome,
            Some(AttemptOutcome::AdapterUnavailable)
        );
        assert_eq!(attempts[3].outcome, Some(AttemptOutcome::Succeeded));
        assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    }

    #[test]
    fn route_fallback_preserves_each_selected_account() {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "accounts", "nodes": [{
                "id": "review", "prompt": "review", "access": "read_only", "routes": [
                    {"adapter": "claude", "model": "claude/model", "account": "work"},
                    {"adapter": "claude", "model": "claude/model", "account": "pro"}
                ]
            }]
        }))
        .unwrap();
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                terminal(AttemptOutcome::AuthRequired),
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(run.status, GraphStatus::Complete);
        assert_eq!(
            owner
                .calls
                .iter()
                .map(|call| call.1.account.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("work"), Some("pro")]
        );
        assert_eq!(
            run.checkpoint.nodes[0].attempts[1].route.account.as_deref(),
            Some("pro")
        );
    }

    #[test]
    fn single_route_form_remains_valid() {
        let document: GraphDocument = serde_json::from_str(
            r#"{"schema_version":1,"graph_id":"g-test","nodes":[{"id":"one","prompt":"work for one","access":"read_only","route":{"adapter":"only","alias":"only-alias","effort":"high"}}]}"#,
        )
        .expect("legacy single-route document parses");
        let mut owner = FakeOwner {
            observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();

        let run = advance(&document, &mut owner, &mut journal).expect("single route advances");

        assert_eq!(run.status, GraphStatus::Complete);
        assert_eq!(owner.calls.len(), 1);
        assert_eq!(run.checkpoint.nodes[0].attempts.len(), 1);
    }

    #[test]
    fn pool_only_and_mixed_routes_advance_in_order() {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "pools", "nodes": [
                {"id": "one", "prompt": "work", "access": "read_only", "route": {"alias": "senior"}},
                {"id": "two", "prompt": "work", "access": "read_only", "depends_on": ["one"], "routes": [
                    {"adapter": "codex", "model": "codex/model"}, {"alias": "senior"}
                ]}
            ]
        })).unwrap();
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                terminal(AttemptOutcome::Succeeded),
                terminal(AttemptOutcome::AuthRequired),
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(run.status, GraphStatus::Complete);
        assert_eq!(owner.calls.len(), 3);
        assert_eq!(owner.calls[0].1.adapter, None);
        assert_eq!(owner.calls[1].1.adapter.as_deref(), Some("codex"));
        assert_eq!(owner.calls[2].1.alias.as_deref(), Some("senior"));
    }

    #[test]
    fn route_requires_adapter_or_alias() {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "g", "nodes": [{
                "id": "one", "prompt": "work", "access": "read_only", "route": {}
            }]
        }))
        .unwrap();
        assert_eq!(
            validate_plan(&document).err(),
            Some("route requires an adapter or alias")
        );
    }

    #[test]
    fn pending_and_unknown_keep_observing_the_same_route_without_fallback() {
        for first in [
            AttemptObservation::Pending { attempt_ref: None },
            AttemptObservation::Unknown { attempt_ref: None },
        ] {
            let routes = vec![route("first"), route("second")];
            let document = graph(vec![GraphNode {
                delivery: None,
                access: AccessMode::ReadOnly,
                worktree: None,
                id: "node".to_owned(),
                title: None,
                prompt: "do the work".to_owned(),
                depends_on: Vec::new(),
                route: None,
                routes: Some(routes.clone()),
                required_verdict: None,
            }]);
            let mut owner = FakeOwner {
                observations: VecDeque::from([first, terminal(AttemptOutcome::Succeeded)]),
                ..FakeOwner::default()
            };
            let mut journal = MemoryJournal::default();

            let pending = advance(&document, &mut owner, &mut journal).expect("first observation");
            assert_eq!(pending.status, GraphStatus::Pending);
            assert_eq!(pending.checkpoint.nodes[0].attempts.len(), 1);
            assert_eq!(owner.calls[0].1, routes[0]);

            let completed =
                advance(&document, &mut owner, &mut journal).expect("same attempt settles");
            assert_eq!(completed.status, GraphStatus::Complete);
            assert_eq!(completed.checkpoint.nodes[0].attempts.len(), 1);
            assert_eq!(owner.calls[1].0, owner.calls[0].0);
            assert_eq!(owner.calls[1].1, routes[0]);
        }
    }

    #[test]
    fn writer_limit_after_launch_never_tries_a_second_route() {
        let mut writer = node("writer", route("first"));
        writer.access = AccessMode::WorktreeWrite;
        writer.worktree = Some("/private/tmp/writer".into());
        writer.route = None;
        writer.routes = Some(vec![route("first"), route("second")]);
        let document = graph(vec![writer]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                terminal(AttemptOutcome::UsageLimited),
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(owner.calls.len(), 1, "a launched writer cannot be retried");
        assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
    }

    fn check_fallback(access: AccessMode, launched: bool, outcome: AttemptOutcome, expected: bool) {
        let mut task = node("node", route("first"));
        task.access = access;
        task.worktree = (access == AccessMode::WorktreeWrite).then(|| "/private/tmp/writer".into());
        task.route = None;
        task.routes = Some(vec![route("first"), route("second")]);
        let document = graph(vec![task]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                AttemptObservation::Terminal {
                    attempt_ref: Some("host/attempt".into()),
                    outcome,
                    launched,
                    result: None,
                },
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(owner.calls.len(), if expected { 2 } else { 1 });
        assert!(
            owner
                .permissions
                .iter()
                .all(|(mode, worktree)| *mode == access
                    && worktree.as_deref()
                        == (access == AccessMode::WorktreeWrite).then_some("/private/tmp/writer"))
        );

        assert_eq!(
            run.checkpoint.nodes[0].state,
            if expected {
                NodeState::Succeeded
            } else {
                NodeState::Failed
            }
        );
        assert_eq!(
            run.checkpoint.nodes[0].worktree.as_deref(),
            (access == AccessMode::WorktreeWrite).then_some("/private/tmp/writer")
        );
        // Reopening the checkpoint must make the same safety decision.
        assert_eq!(advance(&document, &mut owner, &mut journal).unwrap(), run);
    }

    #[test]
    fn writer_preflight_admission_refusal_can_fall_back() {
        check_fallback(
            AccessMode::WorktreeWrite,
            false,
            AttemptOutcome::UsageLimited,
            true,
        );
    }

    #[test]
    fn writer_auth_refusal_before_launch_can_fall_back() {
        check_fallback(
            AccessMode::WorktreeWrite,
            false,
            AttemptOutcome::AuthRequired,
            true,
        );
    }

    #[test]
    fn writer_unavailable_adapter_before_launch_can_fall_back() {
        check_fallback(
            AccessMode::WorktreeWrite,
            false,
            AttemptOutcome::AdapterUnavailable,
            true,
        );
    }

    #[test]
    fn reader_limits_after_launch_can_fall_back() {
        check_fallback(
            AccessMode::ReadOnly,
            true,
            AttemptOutcome::UsageLimited,
            true,
        );
    }

    #[test]
    fn reader_auth_and_adapter_failures_after_launch_can_fall_back() {
        for outcome in [
            AttemptOutcome::AuthRequired,
            AttemptOutcome::AdapterUnavailable,
        ] {
            check_fallback(AccessMode::ReadOnly, true, outcome, true);
        }
    }

    #[test]
    fn writer_auth_and_adapter_failures_after_launch_do_not_fall_back() {
        for outcome in [
            AttemptOutcome::AuthRequired,
            AttemptOutcome::AdapterUnavailable,
        ] {
            check_fallback(AccessMode::WorktreeWrite, true, outcome, false);
        }
    }

    #[test]
    fn checkpoint_attempts_require_launch_evidence_and_actor() {
        let document = graph(vec![node("node", route("route"))]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([AttemptObservation::Pending { attempt_ref: None }]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        for member in ["launched", "launch_actor"] {
            let mut value = serde_json::to_value(&run.checkpoint).unwrap();
            value["nodes"][0]["attempts"][0]
                .as_object_mut()
                .unwrap()
                .remove(member);
            assert!(serde_json::from_value::<GraphCheckpoint>(value).is_err());
        }
    }

    #[test]
    fn cancelled_graph_admits_no_work_and_resume_reuses_the_attempt_key() {
        let document = graph(vec![node("node", route("route"))]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                AttemptObservation::Pending {
                    attempt_ref: Some("host/attempt".into()),
                },
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();
        let pending = advance(&document, &mut owner, &mut journal).unwrap();
        journal.0.as_mut().unwrap().cancelled = true;
        let stopped = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(stopped.status, GraphStatus::Cancelled);
        assert_eq!(owner.calls.len(), 1);
        let completed = resume(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(completed.status, GraphStatus::Complete);
        assert_eq!(owner.calls[0].0, owner.calls[1].0);
        // The resumed observation sees the stored reference instead of resubmitting.
        assert_eq!(
            owner.stored_refs,
            vec![None, Some("host/attempt".to_owned())]
        );
        assert_eq!(
            pending.checkpoint.nodes[0].attempts[0].key,
            completed.checkpoint.nodes[0].attempts[0].key
        );
    }

    #[test]
    fn resume_after_a_crash_before_the_reference_save_replays_under_the_launch_actor() {
        let document = graph(vec![node("node", route("route"))]);
        let mut journal = MemoryJournal::default();
        let mut first = FakeOwner {
            actor: "actor-a".into(),
            observations: VecDeque::from([AttemptObservation::Pending { attempt_ref: None }]),
            ..FakeOwner::default()
        };
        advance(&document, &mut first, &mut journal).unwrap();
        // The crash window: the run store accepted the attempt but no reference was saved.
        let attempt = &mut journal.0.as_mut().unwrap().nodes[0].attempts[0];
        assert_eq!(attempt.launch_actor, "actor-a");
        attempt.attempt_ref = None;
        let mut second = FakeOwner {
            actor: "actor-b".into(),
            observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
            ..FakeOwner::default()
        };
        resume(&document, &mut second, &mut journal).unwrap();
        assert_eq!(second.launch_actors, vec!["actor-a".to_owned()]);
        assert_eq!(second.stored_refs, vec![None]);
    }

    #[test]
    fn terminal_task_failure_does_not_try_a_fallback_route() {
        let document = graph(vec![GraphNode {
            delivery: None,
            access: AccessMode::ReadOnly,
            worktree: None,
            id: "node".to_owned(),
            title: None,
            prompt: "do the work".to_owned(),
            depends_on: Vec::new(),
            route: None,
            routes: Some(vec![route("first"), route("second")]),
            required_verdict: None,
        }]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([terminal(AttemptOutcome::TaskFailed)]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();

        let run = advance(&document, &mut owner, &mut journal).expect("terminal task failure");

        assert_eq!(run.status, GraphStatus::Complete);
        assert_eq!(owner.calls.len(), 1);
        assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
        assert_eq!(run.checkpoint.nodes[0].attempts.len(), 1);
    }

    #[test]
    fn concurrency_cap_observes_running_attempts_before_admitting_more() {
        let document = graph(vec![
            node("one", route("first")),
            node("two", route("first")),
            node("three", route("first")),
        ]);
        let limits = crate::config::GraphLimits {
            concurrency: Some(2),
            ..Default::default()
        };
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                AttemptObservation::Pending { attempt_ref: None },
                AttemptObservation::Pending { attempt_ref: None },
                AttemptObservation::Pending { attempt_ref: None },
                terminal(AttemptOutcome::Succeeded),
                AttemptObservation::Pending { attempt_ref: None },
            ]),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let first = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(first.status, GraphStatus::Pending);
        assert_eq!(owner.calls.len(), 2);
        assert_eq!(
            first
                .checkpoint
                .nodes
                .iter()
                .filter(|n| n.state == NodeState::Running)
                .count(),
            2
        );
        assert_eq!(
            first
                .checkpoint
                .nodes
                .iter()
                .filter(|n| n.state == NodeState::Pending)
                .count(),
            1
        );
        let first_keys: Vec<_> = owner.calls.iter().map(|call| call.0.clone()).collect();
        let second = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(second.status, GraphStatus::Pending);
        assert_eq!(owner.calls.len(), 5);
        assert_eq!(owner.calls[2].0, first_keys[0]);
        assert_eq!(owner.calls[3].0, first_keys[1]);
        assert_ne!(owner.calls[4].0, first_keys[0]);
        assert_ne!(owner.calls[4].0, first_keys[1]);
        assert_eq!(
            second
                .checkpoint
                .nodes
                .iter()
                .filter(|n| n.state == NodeState::Running)
                .count(),
            2
        );
    }

    #[test]
    fn dependent_node_waits_until_its_parent_succeeds() {
        let mut child = node("child", route("child-route"));
        child.depends_on.push("parent".to_owned());
        let document = graph(vec![child, node("parent", route("parent-route"))]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([
                AttemptObservation::Pending { attempt_ref: None },
                terminal(AttemptOutcome::Succeeded),
                terminal(AttemptOutcome::Succeeded),
            ]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();

        let pending = advance(&document, &mut owner, &mut journal).expect("parent starts");
        assert_eq!(pending.status, GraphStatus::Pending);
        assert_eq!(owner.calls.len(), 1);
        assert_eq!(owner.calls[0].0.node_id, "parent");

        let complete = advance(&document, &mut owner, &mut journal).expect("parent then child");
        assert_eq!(complete.status, GraphStatus::Complete);
        assert_eq!(owner.calls[1].0.node_id, "parent");
        assert_eq!(owner.calls[2].0.node_id, "child");
        assert_eq!(complete.checkpoint.nodes[0].state, NodeState::Succeeded);
        assert_eq!(complete.checkpoint.nodes[1].state, NodeState::Succeeded);
    }

    #[test]
    fn required_verdict_rejects_non_review_result() {
        let mut review = node("review", route("fixture"));
        review.prompt = "Review the change and finish with VERDICT: PASS if approved.".into();
        review.required_verdict = Some("VERDICT: PASS".into());
        let mut child = node("child", route("fixture"));
        child.depends_on.push("review".into());
        let mut document = graph(vec![review, child]);
        document.integration_branch = Some("integration".into());
        document.delivery = Some(provenant_records::run::DeliveryMode::Pr);
        let mut owner = FakeOwner {
            observations: VecDeque::from([terminal_with_result(
                AttemptOutcome::Succeeded,
                "I have no shell and cannot review",
            )]),
            ..Default::default()
        };
        let mut journal = MemoryJournal::default();
        let result = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(result.checkpoint.nodes[0].state, NodeState::Failed);
        assert_eq!(result.checkpoint.nodes[1].state, NodeState::Blocked);
        assert!(!result.checkpoint.delivered);
        assert_eq!(owner.calls.len(), 1);
        let replay = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(replay.checkpoint, result.checkpoint);
        assert_eq!(owner.calls.len(), 1);
    }

    #[test]
    fn required_verdict_checks_exact_lines_and_missing_results() {
        for (text, expected) in [
            (
                Some("Review complete.\n  VERDICT: PASS  \n"),
                NodeState::Succeeded,
            ),
            (Some("VERDICT: FAIL"), NodeState::Failed),
            (Some("verdict: pass"), NodeState::Failed),
            (Some("Use VERDICT: PASS when done"), NodeState::Failed),
            (Some(""), NodeState::Failed),
            (None, NodeState::Failed),
        ] {
            let mut review = node("arbitrary-id", route("fixture"));
            review.required_verdict = Some("VERDICT: PASS".into());
            let document = graph(vec![review]);
            let mut owner = FakeOwner {
                observations: VecDeque::from([AttemptObservation::Terminal {
                    attempt_ref: None,
                    outcome: AttemptOutcome::Succeeded,
                    launched: true,
                    result: text.map(str::to_owned),
                }]),
                ..Default::default()
            };
            let mut journal = MemoryJournal::default();
            let run = advance(&document, &mut owner, &mut journal).unwrap();
            assert_eq!(run.checkpoint.nodes[0].state, expected, "{text:?}");
            assert_eq!(
                advance(&document, &mut owner, &mut journal)
                    .unwrap()
                    .checkpoint,
                run.checkpoint
            );
        }
    }

    #[test]
    fn required_verdict_rejects_invalid_contracts() {
        for contract in ["", "  ", "PASS\nFAIL", "PASS\rFAIL"] {
            let mut review = node("review", route("fixture"));
            review.required_verdict = Some(contract.into());
            let mut owner = FakeOwner::default();
            assert!(
                advance(
                    &graph(vec![review]),
                    &mut owner,
                    &mut MemoryJournal::default()
                )
                .is_err()
            );
            assert!(owner.calls.is_empty());
        }
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use provenant_core::{Clock, ProjectId};
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    use std::{convert::Infallible, os::unix::fs::PermissionsExt};
    struct Owner;
    impl AttemptOwner for Owner {
        type Error = Infallible;
        fn actor(&self) -> String {
            "fixture-actor".into()
        }
        fn observe_or_start(
            &mut self,
            _: &AttemptKey,
            _: NodeInput<'_>,
            _: &GraphRoute,
            _: AccessMode,
            _: Option<&str>,
            _: Replay<'_>,
        ) -> Result<AttemptObservation, Infallible> {
            Ok(AttemptObservation::Pending {
                attempt_ref: Some("fixture-attempt".into()),
            })
        }
    }
    #[test]
    fn delivery_retry_reopens_only_the_matching_failed_attempt() {
        let temp = crate::test_support::tempdir().expect("state fixture");
        let base = std::fs::canonicalize(temp.path()).expect("absolute state");
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))
            .expect("private state");
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.clone());
        let root = StateRoot::resolve(&inputs, &SystemPathResolver).expect("state root");
        let project = ProjectId::from_bytes([1; 16]);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5).expect("budget");
        let mut legacy: StoredCheckpoint =
            serde_json::from_str(include_str!("fixtures/legacy-graph-checkpoint.json"))
                .expect("legacy fixture");
        let node = &mut legacy.checkpoint.nodes[0];
        node.state = NodeState::Failed;
        node.attempts[0].outcome = Some(AttemptOutcome::OtherFailure);
        {
            let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock)
                .expect("journal");
            journal.bind(legacy.execution).expect("bind execution");
            journal.save(&legacy.checkpoint).expect("failed delivery");
        }
        FileJournal::retry_delivered_attempt(
            &root,
            project,
            provenant_core::AttemptId::from_bytes([7; 16]),
            budget,
            &cancel,
            &clock,
        )
        .expect("unrelated pickup");
        assert_eq!(
            FileJournal::read_checkpoint(&root, project, "g-test")
                .expect("checkpoint")
                .expect("graph")
                .nodes[0]
                .state,
            NodeState::Failed
        );
        FileJournal::retry_delivered_attempt(
            &root,
            project,
            "22222222222222222222222222222222".parse().expect("attempt"),
            budget,
            &cancel,
            &clock,
        )
        .expect("successful pickup");
        let checkpoint = FileJournal::read_checkpoint(&root, project, "g-test")
            .expect("checkpoint")
            .expect("graph");
        assert_eq!(checkpoint.nodes[0].state, NodeState::Running);
        assert_eq!(
            checkpoint.nodes[0].attempts[0].state,
            AttemptState::Starting
        );
        assert_eq!(checkpoint.nodes[0].attempts[0].outcome, None);
        assert!(
            FileJournal::pending_execution(&root, project, "g-test")
                .expect("pending graph")
                .is_some()
        );
    }
    #[test]
    fn advance_error_is_recorded_replaced_and_cleared_and_rebind_moves_directory() {
        let root = crate::test_support::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = std::fs::canonicalize(root.path()).unwrap();
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        std::fs::create_dir(base.join("state")).unwrap();
        std::fs::set_permissions(base.join("state"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
        let project = ProjectId::from_bytes([1; 16]);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let document:GraphDocument=serde_json::from_value(serde_json::json!({"schema_version":1,"graph_id":"errors","nodes":[{"id":"one","prompt":"test","access":"read_only","route":{"adapter":"fixture","model":"fixture"}}]})).unwrap();
        let read = || FileJournal::read_advance_error(&root, project, "errors").unwrap();
        assert_eq!(read(), None);
        let mut journal =
            FileJournal::open(&root, project, "errors", budget, &cancel, &clock).unwrap();
        for directory in [base.clone(), base.join("state")] {
            journal
                .bind(GraphExecution {
                    integration_base: None,
                    directory,
                    document: document.clone(),
                })
                .unwrap();
        }
        // Recording takes no graph lock, so it works while the journal is held.
        let set = |error| FileJournal::set_advance_error(&root, project, "errors", error).unwrap();
        set(Some("first"));
        assert_eq!(read().as_deref(), Some("first"));
        set(Some("second"));
        assert_eq!(read().as_deref(), Some("second"));
        let long = "é".repeat(MAX_ADVANCE_ERROR_BYTES);
        set(Some(&long));
        assert_eq!(read().unwrap().len(), MAX_ADVANCE_ERROR_BYTES);
        set(None);
        assert_eq!(read(), None);
        set(None);
    }
    #[test]
    fn checkpoint_reopens_with_project_isolation_and_detects_tampering() {
        let root = crate::test_support::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = std::fs::canonicalize(root.path()).unwrap();
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        std::fs::create_dir(base.join("state")).unwrap();
        std::fs::set_permissions(base.join("state"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
        let project = ProjectId::from_bytes([1; 16]);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let document:GraphDocument=serde_json::from_value(serde_json::json!({"schema_version":1,"graph_id":"reopen","nodes":[{"id":"one","prompt":"test","access":"read_only","route":{"adapter":"fixture","model":"fixture"}}]})).unwrap();
        assert!(
            FileJournal::read_checkpoint(&root, project, "reopen")
                .unwrap()
                .is_none()
        );
        assert!(!root.as_path().join("fabric/graphs").exists());
        let checkpoint = {
            let mut journal =
                FileJournal::open(&root, project, "reopen", budget, &cancel, &clock).unwrap();
            journal
                .bind(GraphExecution {
                    integration_base: Some(provenant_core::GitOid::parse(&"a".repeat(40)).unwrap()),
                    directory: base.clone(),
                    document: document.clone(),
                })
                .unwrap();
            let checkpoint = advance(&document, &mut Owner, &mut journal)
                .unwrap()
                .checkpoint;
            // Status must remain available while the writer holds its lock.
            assert_eq!(
                FileJournal::read_checkpoint(&root, project, "reopen").unwrap(),
                Some(checkpoint.clone())
            );
            checkpoint
        };
        assert_eq!(
            FileJournal::graph_ids(&root, project).unwrap(),
            vec!["reopen"]
        );
        assert_eq!(
            FileJournal::pending_execution(&root, project, "reopen").unwrap(),
            Some(GraphExecution {
                integration_base: Some(provenant_core::GitOid::parse(&"a".repeat(40)).unwrap()),
                directory: base.clone(),
                document: document.clone()
            })
        );
        {
            let mut journal =
                FileJournal::open(&root, project, "reopen", budget, &cancel, &clock).unwrap();
            assert_eq!(journal.load("reopen").unwrap(), Some(checkpoint));
            assert_eq!(
                journal.integration_base(),
                Some(provenant_core::GitOid::parse(&"a".repeat(40)).unwrap())
            );
            assert!(matches!(
                journal.bind(GraphExecution {
                    integration_base: None,
                    directory: base.clone(),
                    document: document.clone()
                }),
                Err(JournalError::GraphMismatch)
            ));
        }
        {
            let other_project = ProjectId::from_bytes([2; 16]);
            assert!(
                FileJournal::read_checkpoint(&root, other_project, "reopen")
                    .unwrap()
                    .is_none()
            );
            assert!(
                !root
                    .as_path()
                    .join(format!("fabric/graphs/{other_project}"))
                    .exists()
            );
        }
        let lock = root
            .as_path()
            .join(format!("fabric/graphs/{project}/graph-reopen.lock"));
        std::fs::remove_file(&lock).unwrap();
        assert!(
            FileJournal::read_checkpoint(&root, project, "missing")
                .unwrap()
                .is_none()
        );
        assert!(
            FileJournal::read_checkpoint(&root, project, "reopen")
                .unwrap()
                .is_some()
        );
        assert!(!lock.exists());
        let path = root
            .as_path()
            .join(format!("fabric/graphs/{project}/graph-reopen.json"));
        let mut stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        stored["checkpoint"]["graph_id"] = serde_json::json!("changed");
        std::fs::write(path, serde_json::to_vec(&stored).unwrap()).unwrap();
        assert!(matches!(
            FileJournal::read_checkpoint(&root, project, "reopen"),
            Err(JournalError::BadDigest)
        ));
        assert!(matches!(
            FileJournal::open(&root, project, "reopen", budget, &cancel, &clock),
            Err(JournalError::BadDigest)
        ));
    }
}
