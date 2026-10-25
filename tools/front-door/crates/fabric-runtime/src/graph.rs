//! Bounded DAG execution for Fabric graphs.
//!
//! `FileJournal` saves a complete canonical checkpoint with supervisor's safe
//! filesystem API. `AttemptOwner` is implemented by composition over run. Its
//! key is idempotent: a repeated key observes the existing attempt instead of
//! launching a duplicate provider process.

use provenant_core::{CancelToken, Clock, ProjectId, StateRoot, WaitBudget};
use provenant_records::{CanonicalError, CanonicalV1, DecodeError, Limits, decode_v1};
use provenant_supervisor::fs::{
    FsError, LockGuard, LockMode, Publication, PublishMode, SafeDir, SafeFs,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use thiserror::Error;

pub const MAX_GRAPH_NODES: usize = 64;
pub const MAX_GRAPH_EDGES: usize = 256;
pub const MAX_ROUTES_PER_NODE: usize = 8;
pub const MAX_NODE_PROMPT_BYTES: usize = 64 * 1024;
pub const MAX_GRAPH_PROMPT_BYTES: usize = 256 * 1024;
const MAX_CHECKPOINT_BYTES: u64 = 1_048_576;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphDocument {
    pub schema_version: u64,
    pub graph_id: String,
    pub nodes: Vec<GraphNode>,
}

/// Legacy Fabric route wire shape. Exactly one of `model` and `alias` is
/// required. `adapter` is the adapter id, not the run record's provider field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphRoute {
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// `route` remains the one-route form. A node may instead provide ordered
/// `routes`; supplying both is invalid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphNode {
    pub access: AccessMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<GraphRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<Vec<GraphRoute>>,
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
    /// Durable admission fence. Resume observes the same attempt keys.
    #[serde(default)]
    pub cancelled: bool,
    pub schema_version: u64,
    pub graph_id: String,
    /// Digest of the validated, normalised plan, including prompt bytes.
    pub plan_digest: String,
    /// Nodes are stored in topological order.
    pub nodes: Vec<NodeProgress>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeProgress {
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
    /// Missing launch evidence is conservatively treated as launched.
    #[serde(default = "assume_launched")]
    pub launched: bool,
    pub key: AttemptKey,
    pub route: GraphRoute,
    pub state: AttemptState,
    pub attempt_ref: Option<String>,
    pub outcome: Option<AttemptOutcome>,
    /// Actor that first submitted this attempt. Run-store operation replay is
    /// actor-scoped, so a resume submits under it. Absent in older checkpoints,
    /// which use the resuming actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_actor: Option<String>,
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

fn assume_launched() -> bool {
    true
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
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphStatus {
    Pending,
    Complete,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphRun {
    pub status: GraphStatus,
    pub checkpoint: GraphCheckpoint,
}

/// What the checkpoint already knows about an attempt.
#[derive(Clone, Copy, Debug)]
pub struct Replay<'a> {
    /// Reference to the accepted attempt, once saved.
    pub stored_ref: Option<&'a str>,
    /// Actor to submit under when no reference was saved.
    pub launch_actor: &'a str,
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

    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        prompt: &str,
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
    directory: Box<dyn SafeDir>,
    _lock: LockGuard,
}

impl FileJournal {
    pub fn open(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
        fs: &dyn SafeFs,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<Self, JournalError> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId);
        }
        let root_dir = fs.open_anchor(root.as_path())?;
        let fabric_dir = open_or_create(root_dir.as_ref(), OsStr::new("fabric"))?;
        let graphs_dir = open_or_create(fabric_dir.as_ref(), OsStr::new("graphs"))?;
        let graphs_dir = open_or_create(graphs_dir.as_ref(), OsStr::new(&project.to_string()))?;
        let lock_name = format!("graph-{graph_id}.lock");
        let lock = graphs_dir.lock(
            OsStr::new(&lock_name),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )?;
        Ok(Self {
            graph_id: graph_id.to_owned(),
            directory: graphs_dir,
            _lock: lock,
        })
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
        let file = match self.directory.open_read(&self.checkpoint_name()) {
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
                max_members: 10_000,
            },
        )?;
        if stored.schema_version != 1 {
            return Err(JournalError::BadEnvelope);
        }
        let digest = CanonicalV1::record_digest("fabric-graph-checkpoint", 1, &stored.checkpoint)?;
        if digest.to_string() != stored.digest {
            return Err(JournalError::BadDigest);
        }
        let checkpoint = stored.checkpoint;
        if checkpoint.graph_id != self.graph_id {
            return Err(JournalError::GraphMismatch);
        }
        Ok(Some(checkpoint))
    }

    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        if checkpoint.graph_id != self.graph_id {
            return Err(JournalError::GraphMismatch);
        }
        let stored = StoredCheckpoint {
            schema_version: 1,
            digest: CanonicalV1::record_digest("fabric-graph-checkpoint", 1, checkpoint)?
                .to_string(),
            checkpoint: checkpoint.clone(),
        };
        let bytes = CanonicalV1::bytes(&stored)?;
        if bytes.as_bytes().len() as u64 > MAX_CHECKPOINT_BYTES {
            return Err(JournalError::TooLarge);
        }
        match self.directory.atomic_write(
            &self.checkpoint_name(),
            ".graph-checkpoint.",
            bytes.as_bytes(),
            0o600,
            PublishMode::Replace,
        )? {
            Publication::Durable => Ok(()),
            Publication::VisibleDurabilityUnconfirmed { sync_error } => {
                Err(JournalError::DurabilityUnconfirmed(sync_error))
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCheckpoint {
    schema_version: u64,
    digest: String,
    checkpoint: GraphCheckpoint,
}

fn open_or_create(directory: &dyn SafeDir, name: &OsStr) -> Result<Box<dyn SafeDir>, FsError> {
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
    schema_version: u64,
    graph_id: String,
    nodes: Vec<PlannedNode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct PlannedNode {
    access: AccessMode,
    worktree: Option<String>,
    id: String,
    prompt: String,
    depends_on: Vec<String>,
    routes: Vec<GraphRoute>,
}

/// Advance all currently ready nodes once. Independent nodes can progress
/// while a dependency is still running. Calls are bounded by the document caps
/// and are safe to repeat after a crash because every route has a stable key.
pub fn advance<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    advance_inner(document, owner, journal, false)
}

/// Explicitly reopen graph admission without changing any attempt identity.
pub fn resume<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    advance_inner(document, owner, journal, true)
}

fn advance_inner<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    owner: &mut R,
    journal: &mut J,
    resume: bool,
) -> Result<GraphRun, GraphError<R::Error, J::Error>> {
    let (plan, positions) = validate_plan(document).map_err(GraphError::Invalid)?;
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
                schema_version: 1,
                cancelled: false,
                graph_id: plan.graph_id.clone(),
                plan_digest,
                nodes: plan
                    .nodes
                    .iter()
                    .map(|node| NodeProgress {
                        id: node.id.clone(),
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

    if checkpoint.cancelled {
        if !resume {
            return Ok(GraphRun {
                status: GraphStatus::Cancelled,
                checkpoint,
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
                checkpoint.nodes[node_index].attempts.push(GraphAttempt {
                    key: key.clone(),
                    route: route.clone(),
                    state: AttemptState::Starting,
                    attempt_ref: None,
                    outcome: None,
                    launched: true,
                    launch_actor: Some(owner.actor()),
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
                .and_then(|last| last.launch_actor.clone())
                .unwrap_or_else(|| owner.actor());
            let observation = owner
                .observe_or_start(
                    &key,
                    &node.prompt,
                    &route,
                    node.access,
                    node.worktree.as_deref(),
                    Replay {
                        stored_ref: stored_ref.as_deref(),
                        launch_actor: &launch_actor,
                    },
                )
                .map_err(GraphError::Run)?;
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
                    } => {
                        attempt.launched = launched;
                        attempt.state = AttemptState::Terminal;
                        attempt.attempt_ref = attempt_ref;
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
            journal.save(&checkpoint).map_err(GraphError::Journal)?;
            if !should_continue {
                break;
            }
        }
    }

    let status = if checkpoint.nodes.iter().all(|node| {
        matches!(
            node.state,
            NodeState::Succeeded | NodeState::Failed | NodeState::Blocked
        )
    }) {
        GraphStatus::Complete
    } else {
        GraphStatus::Pending
    };
    Ok(GraphRun { status, checkpoint })
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

fn validate_plan(
    document: &GraphDocument,
) -> Result<(PlannedGraph, std::collections::BTreeMap<String, usize>), &'static str> {
    if document.schema_version != 1 {
        return Err("schema version");
    }
    if !valid_identifier(&document.graph_id) {
        return Err("graph id");
    }
    if document.nodes.is_empty() || document.nodes.len() > MAX_GRAPH_NODES {
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
        validate_access(node.access, node.worktree.as_deref())?;
        if node.prompt.is_empty() || node.prompt.len() > MAX_NODE_PROMPT_BYTES {
            return Err("prompt size");
        }
        prompt_bytes = prompt_bytes
            .checked_add(node.prompt.len())
            .ok_or("prompt size")?;
        if prompt_bytes > MAX_GRAPH_PROMPT_BYTES {
            return Err("total prompt size");
        }
        edge_count = edge_count
            .checked_add(node.depends_on.len())
            .ok_or("edge count")?;
        if edge_count > MAX_GRAPH_EDGES {
            return Err("edge count");
        }

        let routes = match (&node.route, &node.routes) {
            (Some(route), None) => vec![route.clone()],
            (None, Some(routes)) if !routes.is_empty() => routes.clone(),
            (Some(_), Some(_)) => return Err("route and routes are exclusive"),
            (None, None) | (None, Some(_)) => return Err("route list is empty"),
        };
        if routes.len() > MAX_ROUTES_PER_NODE {
            return Err("route count");
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
            access: node.access,
            worktree: node.worktree.clone(),
            id: node.id.clone(),
            prompt: node.prompt.clone(),
            depends_on,
            routes,
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
    field(&route.adapter, 128)
        && (route.model.is_some() ^ route.alias.is_some())
        && route.model.as_deref().is_none_or(|value| field(value, 256))
        && route.alias.as_deref().is_none_or(|value| field(value, 128))
        && route.effort.as_deref().is_none_or(|value| field(value, 64))
}

fn validate_checkpoint<R: Error + 'static, J: Error + 'static>(
    checkpoint: &GraphCheckpoint,
    plan: &PlannedGraph,
    digest: &str,
) -> Result<(), GraphError<R, J>> {
    if checkpoint.schema_version != 1 || checkpoint.graph_id != plan.graph_id {
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
        observations: VecDeque<AttemptObservation>,
        calls: Vec<(AttemptKey, GraphRoute)>,
        permissions: Vec<(AccessMode, Option<String>)>,
        stored_refs: Vec<Option<String>>,
        actor: String,
        launch_actors: Vec<String>,
    }

    impl AttemptOwner for FakeOwner {
        type Error = NoObservation;

        fn actor(&self) -> String {
            self.actor.clone()
        }

        fn observe_or_start(
            &mut self,
            key: &AttemptKey,
            _prompt: &str,
            route: &GraphRoute,
            access: AccessMode,
            worktree: Option<&str>,
            replay: Replay<'_>,
        ) -> Result<AttemptObservation, Self::Error> {
            self.stored_refs.push(replay.stored_ref.map(str::to_owned));
            self.launch_actors.push(replay.launch_actor.to_owned());
            self.calls.push((key.clone(), route.clone()));
            self.permissions.push((access, worktree.map(str::to_owned)));
            self.observations.pop_front().ok_or(NoObservation)
        }
    }

    fn route(adapter: &str) -> GraphRoute {
        GraphRoute {
            adapter: adapter.to_owned(),
            model: Some(format!("{adapter}/model")),
            alias: None,
            effort: None,
        }
    }

    fn node(id: &str, route: GraphRoute) -> GraphNode {
        GraphNode {
            access: AccessMode::ReadOnly,
            worktree: None,
            id: id.to_owned(),
            prompt: format!("work for {id}"),
            depends_on: Vec::new(),
            route: Some(route),
            routes: None,
        }
    }

    fn graph(nodes: Vec<GraphNode>) -> GraphDocument {
        GraphDocument {
            schema_version: 1,
            graph_id: "g-test".to_owned(),
            nodes,
        }
    }

    fn terminal(outcome: AttemptOutcome) -> AttemptObservation {
        AttemptObservation::Terminal {
            attempt_ref: None,
            outcome,
            launched: true,
        }
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
            access: AccessMode::ReadOnly,
            worktree: None,
            id: "node".to_owned(),
            prompt: "do the work".to_owned(),
            depends_on: Vec::new(),
            route: None,
            routes: Some(routes.clone()),
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
    fn pending_and_unknown_keep_observing_the_same_route_without_fallback() {
        for first in [
            AttemptObservation::Pending { attempt_ref: None },
            AttemptObservation::Unknown { attempt_ref: None },
        ] {
            let routes = vec![route("first"), route("second")];
            let document = graph(vec![GraphNode {
                access: AccessMode::ReadOnly,
                worktree: None,
                id: "node".to_owned(),
                prompt: "do the work".to_owned(),
                depends_on: Vec::new(),
                route: None,
                routes: Some(routes.clone()),
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
    fn missing_checkpoint_launch_evidence_cannot_authorise_writer_fallback() {
        let mut task = node("writer", route("first"));
        task.access = AccessMode::WorktreeWrite;
        task.worktree = Some("/private/tmp/writer".into());
        task.route = None;
        task.routes = Some(vec![route("first"), route("second")]);
        let document = graph(vec![task]);
        let mut owner = FakeOwner {
            observations: VecDeque::from([terminal(AttemptOutcome::UsageLimited)]),
            ..FakeOwner::default()
        };
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let mut value = serde_json::to_value(&run.checkpoint).unwrap();
        value["nodes"][0]["attempts"][0]
            .as_object_mut()
            .unwrap()
            .remove("launched");
        journal.0 = Some(serde_json::from_value(value).unwrap());
        assert_eq!(advance(&document, &mut owner, &mut journal).unwrap(), run);
        assert_eq!(owner.calls.len(), 1);
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
        assert_eq!(attempt.launch_actor.as_deref(), Some("actor-a"));
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
    fn checkpoint_without_launch_actor_decodes_and_uses_the_resuming_actor() {
        let document = graph(vec![node("node", route("route"))]);
        let mut journal = MemoryJournal::default();
        let mut first = FakeOwner {
            actor: "actor-a".into(),
            observations: VecDeque::from([AttemptObservation::Pending { attempt_ref: None }]),
            ..FakeOwner::default()
        };
        let run = advance(&document, &mut first, &mut journal).unwrap();
        let mut value = serde_json::to_value(&run.checkpoint).unwrap();
        value["nodes"][0]["attempts"][0]
            .as_object_mut()
            .unwrap()
            .remove("launch_actor");
        journal.0 = Some(serde_json::from_value(value).unwrap());
        let mut second = FakeOwner {
            actor: "actor-b".into(),
            observations: VecDeque::from([terminal(AttemptOutcome::Succeeded)]),
            ..FakeOwner::default()
        };
        resume(&document, &mut second, &mut journal).unwrap();
        assert_eq!(second.launch_actors, vec!["actor-b".to_owned()]);
    }

    #[test]
    fn terminal_task_failure_does_not_try_a_fallback_route() {
        let document = graph(vec![GraphNode {
            access: AccessMode::ReadOnly,
            worktree: None,
            id: "node".to_owned(),
            prompt: "do the work".to_owned(),
            depends_on: Vec::new(),
            route: None,
            routes: Some(vec![route("first"), route("second")]),
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
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use provenant_core::{Clock, ProjectId};
    use provenant_supervisor::{clock::SystemClock, fs::LocalFs};
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
            _: &str,
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
    fn checkpoint_reopens_with_project_isolation_and_detects_tampering() {
        let root = crate::test_support::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = std::fs::canonicalize(root.path()).unwrap();
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        std::fs::create_dir(base.join("state")).unwrap();
        std::fs::set_permissions(base.join("state"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let root = StateRoot::resolve(&inputs, &LocalFs).unwrap();
        let project = ProjectId::from_bytes([1; 16]);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let document:GraphDocument=serde_json::from_value(serde_json::json!({"schema_version":1,"graph_id":"reopen","nodes":[{"id":"one","prompt":"test","access":"read_only","route":{"adapter":"fixture","model":"fixture"}}]})).unwrap();
        let checkpoint = {
            let mut journal =
                FileJournal::open(&root, project, "reopen", &LocalFs, budget, &cancel, &clock)
                    .unwrap();
            advance(&document, &mut Owner, &mut journal)
                .unwrap()
                .checkpoint
        };
        {
            let mut journal =
                FileJournal::open(&root, project, "reopen", &LocalFs, budget, &cancel, &clock)
                    .unwrap();
            assert_eq!(journal.load("reopen").unwrap(), Some(checkpoint));
        }
        {
            let mut journal = FileJournal::open(
                &root,
                ProjectId::from_bytes([2; 16]),
                "reopen",
                &LocalFs,
                budget,
                &cancel,
                &clock,
            )
            .unwrap();
            assert!(journal.load("reopen").unwrap().is_none());
        }
        let path = root
            .as_path()
            .join(format!("fabric/graphs/{project}/graph-reopen.json"));
        let mut stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        stored["checkpoint"]["graph_id"] = serde_json::json!("changed");
        std::fs::write(path, serde_json::to_vec(&stored).unwrap()).unwrap();
        let mut journal =
            FileJournal::open(&root, project, "reopen", &LocalFs, budget, &cancel, &clock).unwrap();
        assert!(matches!(
            journal.load("reopen"),
            Err(JournalError::BadDigest)
        ));
    }
}
