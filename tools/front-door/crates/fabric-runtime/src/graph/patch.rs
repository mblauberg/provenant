//! One mutation path: every node that joins a graph arrives in a patch.
//!
//! `admit_patch` compiles a patch's nodes and inline templates, checks it
//! against an expected revision and an idempotency key, refuses cycles,
//! conflicting edits and secrets, and journals it as one checkpoint
//! expansion. A planner's fragment, a host submission and a live call from a
//! lane all use it. Started attempts are immutable: `skip` and `replace` act
//! only on nodes that have not started.
use super::template::{CompileError, Compiler, Layer, ParameterRefusal, Template, TemplateLibrary};
use super::*;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const PATCH_SCHEMA_VERSION: u64 = 1;
const KEY_BYTES: usize = 128;
const REASON_BYTES: usize = 512;

/// Nodes, inline templates and edits of unstarted nodes, committed atomically.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub schema_version: u64,
    /// The graph revision the author saw; a different revision is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    /// Repeating a key replays the earlier admission instead of adding twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Inline templates, visible to this patch's nodes only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub templates: BTreeMap<String, Template>,
    /// Template nodes: `use` plus overrides, or compiled-node fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<Value>,
    /// Unstarted nodes to skip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip: Vec<String>,
    /// Leaf nodes, each replacing the unstarted leaf with the same ID.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replace: Vec<Value>,
}

impl Patch {
    /// Read a planner's `fragment` block. A `patterns` array is read as nodes
    /// that `use` a catalogue template, and a bare input name selects `result`.
    pub fn from_fragment(json: &str) -> Result<Self, String> {
        let mut value: Value =
            serde_json::from_str(json).map_err(|error| format!("invalid fragment: {error}"))?;
        let object = value.as_object_mut().ok_or("fragment must be an object")?;
        if let Some(instances) = object.remove("patterns") {
            let instances = instances
                .as_array()
                .cloned()
                .ok_or("fragment patterns must be a list")?;
            let nodes = object
                .entry("nodes")
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or("fragment nodes must be a list")?;
            for instance in instances {
                let mut fields = instance
                    .as_object()
                    .cloned()
                    .ok_or("fragment pattern must be an object")?;
                let name = fields
                    .remove("pattern")
                    .ok_or("fragment pattern instance requires `pattern`")?;
                let mut node = Map::new();
                node.insert("use".into(), name);
                if let Some(parameters) = fields.remove("parameters") {
                    node.insert("with".into(), parameters);
                }
                if let Some(after) = fields.remove("depends_on") {
                    node.insert("after".into(), after);
                }
                for key in ["id", "inputs", "title"] {
                    if let Some(value) = fields.remove(key) {
                        node.insert(key.into(), value);
                    }
                }
                if let Some(key) = fields.keys().next() {
                    return Err(format!("fragment pattern instance has unknown field {key}"));
                }
                nodes.push(Value::Object(node));
            }
        }
        for key in ["nodes", "replace"] {
            for node in object
                .get_mut(key)
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                normalise_inputs(node);
            }
        }
        serde_json::from_value(value).map_err(|error| format!("invalid fragment: {error}"))
    }

    fn digest(&self) -> Result<String, PatchError> {
        CanonicalV1::record_digest("fabric-graph-patch", 1, self)
            .map(|digest| digest.to_string())
            .map_err(|error| PatchError::invalid(format!("patch is not canonical: {error}")))
    }
}

fn normalise_inputs(node: &mut Value) {
    if let Some(inputs) = node.get_mut("inputs").and_then(Value::as_array_mut) {
        for input in inputs {
            if let Some(name) = input.as_str() {
                *input = serde_json::json!({"node": name, "outputs": ["result"]});
            }
        }
    }
    for pointer in ["/composite/nodes", "/loop/body"] {
        if let Some(members) = node.pointer_mut(pointer).and_then(Value::as_array_mut) {
            members.iter_mut().for_each(normalise_inputs);
        }
    }
}

/// One template use retained with the expansion that produced it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateUse {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<String>,
}

/// The journal entry for an admitted patch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchRecord {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    /// Receipt saved with the admission for idempotent child replay.
    #[serde(
        default,
        rename = "relay_child_receipt",
        skip_serializing_if = "Option::is_none"
    )]
    pub child_receipt: Option<super::relay::StoredChildReceipt>,
    /// Revision the graph reached by admitting this patch.
    pub revision: u64,
    pub digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaced: Vec<GraphNode>,
    /// Whether `admit_patch` already wrote this admission to the journal. An
    /// unjournaled record (a planner fragment) is journaled at save instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub journaled: bool,
    /// Present when the host restarted a stopped node: the stop it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<Restart>,
}

/// The stop a host restart answered and the step it took. Restarts count
/// against the graph-wide `limits.continuations`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restart {
    pub node: String,
    pub cause: provenant_records::StopCause,
    #[serde(flatten)]
    pub step: provenant_records::NextStep,
    /// The routing ladder's choice, when a depleted stop was handed off by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ladder: Option<LadderChoice>,
}

/// The rung, target and tier the depletion ladder chose for a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderChoice {
    pub rung: provenant_routing::Rung,
    /// `adapter/model` with its account, or `host-native/name`.
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct PatchError {
    /// Stable cause: `stale_revision`, `idempotency_conflict`, `cycle`,
    /// `conflict`, `started`, `edit_scope`, `secret`, `journal` or `invalid`.
    pub code: &'static str,
    pub message: String,
    /// Closed diagnostic fact retained when a planner uses this admission path.
    pub cause: Option<PlanFailureFact>,
    /// The typed admission refusal when compilation reached
    /// `GraphDocument::admit`. Keeping the typed value lets the front door
    /// project it exactly as it projects a raw document, so one compile
    /// failure reads the same through every entry path.
    pub admission: Option<AdmissionError>,
    pub candidate: Option<Box<CandidateError>>,
    /// The supplied parameter a template refused, when compilation failed on one.
    pub parameter: Option<Box<ParameterRefusal>>,
    pub field: Option<refusal::SchemaPath>,
    pub rule: Option<refusal::SecretRule>,
}

impl PatchError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            cause: None,
            admission: None,
            candidate: None,
            parameter: None,
            field: None,
            rule: None,
        }
    }
    pub fn secret(field: refusal::SchemaPath, rule: refusal::SecretRule) -> Self {
        Self {
            code: "secret",
            message: format!("the {field} contains a credential-shaped value (rule {rule})"),
            cause: None,
            admission: None,
            candidate: None,
            parameter: None,
            field: Some(field),
            rule: Some(rule),
        }
    }
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }
}

impl From<String> for PatchError {
    fn from(message: String) -> Self {
        Self::invalid(message)
    }
}

impl From<CompileError> for PatchError {
    fn from(error: CompileError) -> Self {
        match error {
            CompileError::Message(message) => Self::invalid(message),
            CompileError::Parameter(refusal) => Self {
                parameter: Some(Box::new(refusal.clone())),
                ..Self::invalid(refusal.to_string())
            },
        }
    }
}

/// Journal callback run before a graph patch commits.
pub type PatchJournal<'a> = dyn Fn(&PatchRecord) -> Result<(), String> + 'a;

/// Everything a patch is compiled and checked against, apart from the graph.
pub struct PatchContext<'a> {
    pub library: &'a TemplateLibrary,
    pub resolvers: &'a BTreeMap<String, Vec<String>>,
    /// Refuses text that matches a secret pattern.
    pub scan: &'a dyn Fn(&str) -> Result<(), String>,
    /// Writes an admitted patch to the graph's journal. `admit_patch` calls it
    /// before the admission is committed, so a failed write admits nothing.
    pub journal: Option<&'a PatchJournal<'a>>,
    pub limits: &'a crate::config::GraphLimits,
    /// The project's declared worktree layout used for generated child paths.
    pub worktree_layout: provenant_config::WorktreeLayoutPort,
}

/// The confinement secret scan, as submission applies it.
pub fn scan_for_secrets(text: &str) -> Result<(), String> {
    provenant_confinement::disclosure::scan_snapshot(text.as_bytes(), &[])
        .map_err(|_| "patch matches a secret pattern; remove the secret and retry".to_owned())
}

/// A running graph: its submitted document and its checkpoint.
pub struct LiveGraph<'a> {
    pub document: &'a GraphDocument,
    pub checkpoint: &'a mut GraphCheckpoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
    pub revision: u64,
    /// The idempotency key matched an earlier admission; nothing changed.
    pub replayed: bool,
    pub added: Vec<String>,
}

/// The graph revision: one per admitted expansion.
pub fn revision(checkpoint: &GraphCheckpoint) -> u64 {
    checkpoint.expansions.len() as u64
}

/// Admit `patch` into a running graph on behalf of `parent`, or the host when
/// `parent` is `None`. Nodes a parent adds are namespaced under it, and the
/// parent's dependants wait for them.
pub fn admit_patch(
    graph: &mut LiveGraph<'_>,
    parent: Option<&str>,
    patch: &Patch,
    context: &PatchContext<'_>,
) -> Result<Admitted, PatchError> {
    admit_patch_for_retry(graph, parent, patch, context, None)
}

fn admit_patch_for_retry(
    graph: &mut LiveGraph<'_>,
    parent: Option<&str>,
    patch: &Patch,
    context: &PatchContext<'_>,
    retry: Option<&str>,
) -> Result<Admitted, PatchError> {
    // Initial nodes live in the immutable source, rather than being copied
    // into an expansion. They still enter through the same admission gate.
    if parent.is_none() && graph.document.submission_patch.as_ref() == Some(patch) {
        let digest = patch.digest()?;
        if let Some(record) = graph
            .checkpoint
            .expansions
            .iter()
            .filter_map(|e| e.patch.as_ref())
            .find(|record| record.key == patch.idempotency_key)
        {
            return if record.digest == digest {
                Ok(Admitted {
                    revision: record.revision,
                    replayed: true,
                    added: Vec::new(),
                })
            } else {
                Err(PatchError::new(
                    "idempotency_conflict",
                    "submission key changed",
                ))
            };
        }
        if revision(graph.checkpoint) != 0
            || graph.checkpoint.nodes.iter().any(|node| {
                // A row a `when` condition skipped at submission has not started.
                let skipped_at_submission =
                    node.state == NodeState::Skipped && node.skip_reason.is_some();
                (node.state != NodeState::Pending && !skipped_at_submission)
                    || !node.attempts.is_empty()
            })
        {
            return Err(PatchError::new(
                "started",
                "initial submission must precede launch",
            ));
        }
        let compiled = submit(
            &graph.document.graph_id,
            patch,
            context,
            graph.document.integration_branch.as_deref(),
        )?;
        if compiled.nodes != graph.document.nodes {
            return Err(PatchError::invalid("submission differs from bound nodes"));
        }
        let uses = patch
            .nodes
            .iter()
            .filter_map(|raw| {
                Some(TemplateUse {
                    extra: Default::default(),
                    id: raw.get("id")?.as_str()?.to_owned(),
                    name: raw.get("use")?.as_str()?.to_owned(),
                    parameters: raw.get("with").cloned().unwrap_or(Value::Null),
                    nodes: graph
                        .checkpoint
                        .nodes
                        .iter()
                        .filter(|node| {
                            raw.get("id").and_then(Value::as_str).is_some_and(|id| {
                                node.id == id || node.id.starts_with(&format!("{id}--"))
                            })
                        })
                        .map(|node| node.id.clone())
                        .collect(),
                })
            })
            .collect();
        let mut record = PatchRecord {
            extra: Default::default(),
            child_receipt: None,
            revision: 1,
            digest,
            key: patch.idempotency_key.clone(),
            parent: None,
            reason: patch.reason.clone(),
            skipped: Vec::new(),
            replaced: Vec::new(),
            journaled: false,
            restart: None,
        };
        if let Some(journal) = context.journal {
            journal(&record).map_err(|reason| PatchError::new("journal", reason))?;
            record.journaled = true;
        }
        graph.checkpoint.expansions.push(Expansion {
            extra: Default::default(),
            retired: None,
            retry_generation: None,
            final_result: false,
            final_fragment: false,
            floor_warnings: Vec::new(),
            source: String::new(),
            nodes: Vec::new(),
            uses,
            patch: Some(record),
        });
        return Ok(Admitted {
            revision: 1,
            replayed: false,
            added: graph
                .checkpoint
                .nodes
                .iter()
                .map(|node| node.id.clone())
                .collect(),
        });
    }
    let expanded = plans::expand(
        graph.document,
        &graph.checkpoint.extensions,
        &graph.checkpoint.expansions,
        context.limits,
    )?;
    let (plan, _) =
        validate_flat_plan(&expanded, &plans::generous()).map_err(|error| error.to_string())?;
    let parent = match parent {
        Some(id) => Some(
            plan.nodes
                .iter()
                .find(|node| node.id == id)
                .ok_or_else(|| PatchError::invalid(format!("unknown parent node {id}")))?,
        ),
        None => None,
    };
    match prepare(
        graph.document,
        &expanded,
        graph.checkpoint,
        parent,
        patch,
        context,
        Preparation::Live { retry },
    )? {
        Prepared::Replayed(revision) => Ok(Admitted {
            revision,
            replayed: true,
            added: Vec::new(),
        }),
        Prepared::Ready(admission) => {
            let Admission {
                mut expansion,
                preview,
                plan,
                ..
            } = *admission;
            if let Some(parent) = parent {
                for node in &mut expansion.nodes {
                    node.parent_node = Some(parent.id.clone());
                }
            }
            if let (Some(journal), Some(record)) = (context.journal, expansion.patch.as_mut()) {
                journal(record).map_err(|reason| PatchError::new("journal", reason))?;
                record.journaled = true;
            }
            let added = expansion.nodes.iter().map(|node| node.id.clone()).collect();
            let revision = commit(graph.checkpoint, expansion, &preview, &plan, context.limits);
            Ok(Admitted {
                revision,
                replayed: false,
                added,
            })
        }
    }
}

/// Redo a selected node and its transitive consumers as one atomic mutation.
/// The host saves the resulting patch and state transitions to the events journal.
pub fn retry_node_and_dependants(
    graph: &mut LiveGraph<'_>,
    id: &str,
    brief: Option<&str>,
    context: &PatchContext<'_>,
) -> Result<Admitted, PatchError> {
    retry_node_scope(graph, id, brief, context, true, None, None)
}

/// Redo a settled node or container, retaining prior attempts and journalling the reset.
/// The caller holds the graph lock and saves the result before advancing.
pub fn retry_node(
    graph: &mut LiveGraph<'_>,
    id: &str,
    brief: Option<&str>,
    context: &PatchContext<'_>,
) -> Result<Admitted, PatchError> {
    retry_node_scope(graph, id, brief, context, false, None, None)
}

/// A host restart after a stop uses the same checked, journalled reset as an
/// explicit retry, and records the stop it answered.
pub(super) fn restart_stopped(
    graph: &mut LiveGraph<'_>,
    id: &str,
    brief: Option<&str>,
    routes: &[GraphRoute],
    restart: Restart,
    context: &PatchContext<'_>,
) -> Result<Admitted, PatchError> {
    let reason = format!("restart node {id} after a {} stop", restart.cause);
    debug_assert_eq!(restart.node, id);
    retry_node_scope(
        graph,
        id,
        brief,
        context,
        false,
        Some((&reason, Some(restart))),
        Some(routes),
    )
}

fn retry_node_scope(
    graph: &mut LiveGraph<'_>,
    id: &str,
    brief: Option<&str>,
    context: &PatchContext<'_>,
    dependants: bool,
    reason: Option<(&str, Option<Restart>)>,
    replacement_routes: Option<&[GraphRoute]>,
) -> Result<Admitted, PatchError> {
    let expanded = plans::expand(
        graph.document,
        &graph.checkpoint.extensions,
        &graph.checkpoint.expansions,
        context.limits,
    )?;
    let mut node = expanded
        .nodes
        .iter()
        .find(|node| node.id == id)
        .cloned()
        .ok_or_else(|| PatchError::invalid(format!("unknown node {id}")))?;
    let index = graph
        .checkpoint
        .nodes
        .iter()
        .position(|node| node.id == id)
        .ok_or_else(|| PatchError::invalid(format!("unknown node {id}")))?;
    let container = node.composite.is_some() || node.loop_body.is_some();
    if container && brief.is_some() {
        return Err(container_brief_refusal(graph.checkpoint, id, context));
    }
    // Enclosing joins depend on their members, but redoing one member must
    // only recompute those joins, rather than retiring the selected member.
    let redo_container = |node: &GraphNode| {
        (node.composite.is_some() || node.loop_body.is_some())
            && (node.id == id || !id.starts_with(&format!("{}--", node.id)))
    };
    // Ordinary agents can produce final patches too. Their prior topology is
    // redone with the producer, while live-spawn-only children keep their own
    // execution. Enclosing joins are only recomputed when one member is redone.
    let redo_topology = |node: &GraphNode| {
        redo_container(node)
            || dependants
                && (node.id == id || !id.starts_with(&format!("{}--", node.id)))
                && (node.plan.is_some()
                    || graph.checkpoint.expansions.iter().any(|expansion| {
                        expansion.source == node.id && expansion.closes_producer()
                    }))
    };
    let prefix = format!("{id}--");
    let mut reset_ids = BTreeSet::from([id.to_owned()]);
    if container {
        reset_ids.extend(
            expanded
                .nodes
                .iter()
                .filter(|node| node.id.starts_with(&prefix))
                .map(|node| node.id.clone()),
        );
    }
    if dependants {
        loop {
            let before = reset_ids.len();
            let prefixes: Vec<_> = expanded
                .nodes
                .iter()
                .filter(|node| reset_ids.contains(&node.id) && redo_topology(node))
                .map(|node| format!("{}--", node.id))
                .collect();
            for node in &expanded.nodes {
                if node.dependencies().any(|parent| reset_ids.contains(parent))
                    || prefixes.iter().any(|prefix| node.id.starts_with(prefix))
                {
                    reset_ids.insert(node.id.clone());
                }
            }
            if reset_ids.len() == before {
                break;
            }
        }
    }
    for progress in &graph.checkpoint.nodes {
        if reset_ids.contains(&progress.id)
            && progress
                .attempts
                .iter()
                .any(|attempt| attempt.state != AttemptState::Terminal)
            && !(progress.state == NodeState::Skipped && progress.cancelled_from.is_some())
            && !refused_before_launch(progress)
        {
            return Err(PatchError::invalid(
                "node retry requires settled subtree attempts",
            ));
        }
    }
    let old = &graph.checkpoint.nodes[index];
    if !old.state.settled() {
        return Err(PatchError::invalid(
            "node retry requires a settled node and attempts",
        ));
    }
    if let Some(brief) = brief {
        replace_brief(&mut node, brief);
    }
    let mut candidate = graph.checkpoint.clone();
    if candidate.cancelled {
        if candidate.nodes.iter().any(|n| {
            !n.state.settled() && n.attempts.iter().any(|a| a.state != AttemptState::Terminal)
        }) {
            return Err(PatchError::invalid(
                "cancelled graph still has live attempts",
            ));
        }
        candidate.cancelled = false;
        candidate.cancel_reason = None;
        // Resuming this redo must not resurrect the rest of a cancelled graph.
        for progress in &mut candidate.nodes {
            if progress.cancelled_from.is_some() && progress.state == NodeState::Skipped {
                progress.skip_reason = Some("parent graph cancelled".into());
            }
        }
    }
    // Settled consumers keep the generations they observed, including any
    // internal outputs they selected directly. Composite rows are derived
    // from their current members, so they must not snapshot a member being
    // retried. Pending consumers see the redo.
    let mut affected = reset_ids.clone();
    loop {
        let before = affected.len();
        for node in &expanded.nodes {
            if node.dependencies().any(|parent| affected.contains(parent)) {
                affected.insert(node.id.clone());
            }
        }
        if affected.len() == before {
            break;
        }
    }
    // A member retry also changes the enclosing composite's derived export.
    // Settled external consumers must keep the composite generation they
    // observed, alongside snapshots of the directly reset members.
    let mut snapshot_ids = reset_ids.clone();
    snapshot_ids.extend(
        expanded
            .nodes
            .iter()
            .filter(|node| node.composite.is_some() && affected.contains(&node.id))
            .map(|node| node.id.clone()),
    );
    for consumer in &mut candidate.nodes {
        if !reset_ids.contains(&consumer.id)
            && affected.contains(&consumer.id)
            && consumer.state.settled()
        {
            let composite_members = expanded
                .nodes
                .iter()
                .find(|node| node.id == consumer.id && node.composite.is_some())
                .map(|node| node.composite_members.as_slice());
            let is_composite_member = |id: &str| {
                composite_members.is_some_and(|members| {
                    members.iter().any(|member| {
                        id == member
                            || id
                                .strip_prefix(member)
                                .is_some_and(|suffix| suffix.starts_with("--"))
                    })
                })
            };
            let snapshots = &mut consumer.settled_dependency_snapshots;
            if composite_members.is_some() {
                // Composite outputs and state are derived from current member
                // settlement, even when the composite itself was settled.
                snapshots.retain(|id, _| !is_composite_member(id));
            }
            for prior in graph.checkpoint.nodes.iter().filter(|node| {
                node.id != consumer.id
                    && snapshot_ids.contains(&node.id)
                    && node.state.settled()
                    && !is_composite_member(&node.id)
            }) {
                let mut snapshot = prior.clone();
                snapshot.settled_dependency_snapshots.clear();
                snapshot.previous_attempts.clear();
                snapshots.entry(prior.id.clone()).or_insert(snapshot);
            }
        }
    }
    // A producer's old descendants cannot share names and dependency edges
    // with a new execution, so their expansions retire in this same candidate.
    let mut retired = BTreeSet::new();
    for container in expanded
        .nodes
        .iter()
        .filter(|node| reset_ids.contains(&node.id) && redo_topology(node))
    {
        retired.extend(
            super::retire::dynamic_subtree(
                &mut candidate,
                &container.id,
                &format!("retry node {id}"),
            )
            .map_err(PatchError::invalid)?,
        );
    }
    if !retired.is_empty() {
        // The container's own spec loses the wiring the retired expansions added.
        node = plans::expand(
            graph.document,
            &candidate.extensions,
            &candidate.expansions,
            context.limits,
        )?
        .nodes
        .into_iter()
        .find(|node| node.id == id)
        .ok_or_else(|| PatchError::invalid(format!("unknown node {id}")))?;
        if let Some(brief) = brief {
            replace_brief(&mut node, brief);
        }
    }
    if let Some(routes) = replacement_routes {
        node.route = None;
        node.routes = Some(routes.to_vec());
    }
    // A reset removes its expansion-pause marker. Move the graph pause to
    // another waiting node, or clear it, before removing that marker.
    for index in 0..candidate.nodes.len() {
        if reset_ids.contains(&candidate.nodes[index].id)
            && (candidate.nodes[index].expansion_pause.is_some()
                || matches!(
                    &candidate.nodes[index].expansion_cause,
                    Some(NodeDecisionCause::PlanExpansionPaused { active: true, .. })
                ))
        {
            plans::clear_expansion_pause(&mut candidate, index, true);
        }
    }
    for progress in &mut candidate.nodes {
        if reset_ids.contains(&progress.id) {
            let container = expanded.nodes.iter().any(|node| {
                reset_ids.contains(&node.id)
                    && redo_container(node)
                    && (node.id == progress.id
                        || progress.id.starts_with(&format!("{}--", node.id)))
            });
            *progress = reset_progress(progress, container)?;
        }
    }
    if container || dependants {
        // Terminal loop decisions belong to the old execution, while explicit
        // round allowances remain part of the graph's configured limits.
        candidate.extensions.retain(|extension| {
            !reset_ids
                .iter()
                .any(|id| extension.starts_with(&format!("loop.{id}.decision=")))
        });
        candidate
            .asks
            .retain(|ask| ask.node.as_ref().is_none_or(|id| !reset_ids.contains(id)));
        loops::refresh_pause(&mut candidate);
    }
    let patch = Patch {
        schema_version: PATCH_SCHEMA_VERSION,
        expected_revision: Some(revision(graph.checkpoint)),
        reason: Some(
            reason
                .as_ref()
                .map(|(reason, _)| (*reason).to_owned())
                .unwrap_or_else(|| format!("retry node {id}")),
        ),
        replace: vec![serde_json::to_value(node).map_err(|e| PatchError::invalid(e.to_string()))?],
        ..Default::default()
    };
    let admitted = admit_patch_for_retry(
        &mut LiveGraph {
            document: graph.document,
            checkpoint: &mut candidate,
        },
        None,
        &patch,
        context,
        Some(id),
    )?;
    if let Some((_, Some(restart))) = reason
        && let Some(record) = candidate
            .expansions
            .last_mut()
            .and_then(|expansion| expansion.patch.as_mut())
    {
        record.restart = Some(restart);
    }
    asks::refresh_wake(&mut candidate);
    *graph.checkpoint = candidate;
    Ok(admitted)
}

/// Replace a leaf's brief. A template derives its single acceptance check
/// from the brief, so a check still equal to the old brief follows it.
fn replace_brief(node: &mut GraphNode, brief: &str) {
    if node.acceptance_checks.len() == 1 && node.acceptance_checks[0] == node.prompt {
        node.acceptance_checks = vec![brief.to_owned()];
    }
    node.prompt = brief.to_owned();
}

/// A container's brief lives in its member nodes. When the template it was
/// built from declares no `brief` parameter, the refusal names that field and
/// template as the same typed fault a bad `with` produces.
fn container_brief_refusal(
    checkpoint: &GraphCheckpoint,
    id: &str,
    context: &PatchContext<'_>,
) -> PatchError {
    let mut error = PatchError::invalid("container retry requires per-leaf brief edits");
    let used = checkpoint
        .expansions
        .iter()
        .flat_map(|expansion| &expansion.uses)
        .find(|used| used.id == id);
    if let Some(used) = used
        && let Ok(template) = context.library.resolve(&used.name)
        && !template.parameters.contains_key("brief")
    {
        error.parameter = Some(Box::new(ParameterRefusal {
            node: Some(id.to_owned()),
            template: used.name.clone(),
            fault: template::ParameterFault::Unknown {
                key: Some("brief".into()),
                accepted: template.parameters.keys().cloned().collect(),
                suggestion: None,
            },
        }));
    }
    error
}

// An explicit host-proven refusal leaves a Starting row but no process to
// stop. Apply the same proof to the selected node and every reset descendant;
// stale launch flags alone never establish that a handoff was not accepted.
fn refused_before_launch(progress: &NodeProgress) -> bool {
    progress.state == NodeState::Blocked
        && progress.advance_error_code.as_deref() == Some("launch_refused")
        && progress
            .attempts
            .iter()
            .filter(|attempt| attempt.state != AttemptState::Terminal)
            .all(|attempt| {
                attempt.host_refused_before_acceptance
                    && attempt.attempt_ref.is_none()
                    && attempt.state == AttemptState::Starting
            })
}

fn reset_progress(old: &NodeProgress, container: bool) -> Result<NodeProgress, PatchError> {
    let generation = old
        .retry_generation
        .unwrap_or_default()
        .checked_add(1)
        .ok_or_else(|| PatchError::invalid("retry generation exhausted"))?;
    let mut previous = old.previous_attempts.clone();
    previous.extend(old.attempts.iter().cloned());
    let mut reset = serde_json::to_value(old).map_err(|e| PatchError::invalid(e.to_string()))?;
    let fields = reset
        .as_object_mut()
        .ok_or_else(|| PatchError::invalid("node snapshot must be an object"))?;
    fields.retain(|key, _| {
        matches!(
            key.as_str(),
            "id" | "worktree"
                | "writer_binding"
                | "source_binding"
                | "source_commit"
                | "review_base"
                | "round"
                | "title"
        )
    });
    fields.insert("state".into(), "pending".into());
    fields.insert("attempts".into(), serde_json::json!([]));
    fields.insert(
        "previous_attempts".into(),
        serde_json::to_value(previous).map_err(|error| PatchError::invalid(error.to_string()))?,
    );
    fields.insert("retry_generation".into(), generation.into());
    if container {
        fields.insert("container_retry".into(), true.into());
    }
    serde_json::from_value(reset).map_err(|e| PatchError::invalid(e.to_string()))
}

/// Retain the initial mutation alongside the immutable submitted topology.
/// Host binding has already resolved routes and worktrees. Inline snapshots
/// keep those effective nodes replayable without consulting mutable layers.
pub fn submission(
    document: &mut GraphDocument,
    key: &str,
    limits: &crate::config::GraphLimits,
    worktree_layout: provenant_config::WorktreeLayoutPort,
) -> Result<(), PatchError> {
    for node in &mut document.nodes {
        node.template.get_or_insert_with(|| "raw".into());
    }
    let mut patch = Patch {
        schema_version: PATCH_SCHEMA_VERSION,
        expected_revision: Some(0),
        idempotency_key: Some(key.to_owned()),
        reason: Some("initial submission".into()),
        ..Default::default()
    };
    let parameters = document
        .provenance
        .as_ref()
        .map(|provenance| provenance.parameters.clone())
        .unwrap_or_default();
    for (index, node) in document.nodes.iter().enumerate() {
        let mut raw = serde_json::to_value(node).map_err(|e| PatchError::invalid(e.to_string()))?;
        // Snapshot compiled nodes without inheriting template behaviour a
        // second time. A requested sizing plan must not run again after the
        // original brief has already been compiled.
        // Explicit extensions retain their provenance.
        let original = raw.get("template").and_then(Value::as_str).unwrap_or("raw");
        let name = if original == "implement" || original.contains('.') {
            format!("submission-node-{index}")
        } else {
            original.to_owned()
        };
        let mut template = Template::default();
        let mut supplied = Map::new();
        for (name, value) in &parameters {
            use super::template::{ParameterKind, TemplateParameter};
            let kind = match value {
                Value::Null => continue,
                Value::Bool(_) => ParameterKind::Boolean,
                Value::Number(number) if number.is_i64() || number.is_u64() => {
                    ParameterKind::Integer
                }
                Value::Number(_) => ParameterKind::Number,
                Value::String(_) => ParameterKind::String,
                Value::Array(_) => ParameterKind::List,
                Value::Object(_) => ParameterKind::Table,
            };
            template.parameters.insert(
                name.clone(),
                TemplateParameter {
                    extra: Default::default(),
                    kind,
                    default: None,
                    min: None,
                    max: None,
                    allowed: Vec::new(),
                    doc: None,
                },
            );
            supplied.insert(name.clone(), value.clone());
        }
        patch.templates.insert(name.clone(), template);
        template::preserve_compiled_metadata(&mut raw).map_err(PatchError::invalid)?;
        raw["use"] = Value::String(name);
        raw["with"] = Value::Object(supplied);
        patch.nodes.push(raw);
    }
    let library = TemplateLibrary::bundled();
    let resolvers = BTreeMap::new();
    let context = PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan: &scan_for_secrets,
        journal: None,
        limits,
        worktree_layout,
    };
    let compiled = submit(
        &document.graph_id,
        &patch,
        &context,
        document.integration_branch.as_deref(),
    )?;
    if compiled.nodes != document.nodes {
        return Err(PatchError::invalid("submission differs from bound nodes"));
    }
    document.submission_patch = Some(patch.clone());
    Ok(())
}

/// Compile a submitted patch into a new graph document. Submission uses the
/// same compiler and checks as a running graph's patches.
pub fn submit(
    graph_id: &str,
    patch: &Patch,
    context: &PatchContext<'_>,
    integration_branch: Option<&str>,
) -> Result<GraphDocument, PatchError> {
    if patch.schema_version != PATCH_SCHEMA_VERSION {
        return Err(PatchError::invalid(format!(
            "patch requires schema_version {PATCH_SCHEMA_VERSION}"
        )));
    }
    if patch
        .idempotency_key
        .as_ref()
        .is_some_and(|key| key.is_empty() || key.len() > KEY_BYTES || key.contains('\0'))
    {
        return Err(PatchError::invalid(format!(
            "idempotency key must be 1..={KEY_BYTES} bytes"
        )));
    }
    if patch
        .reason
        .as_ref()
        .is_some_and(|reason| reason.len() > REASON_BYTES)
    {
        return Err(PatchError::invalid(format!(
            "patch reason exceeds {REASON_BYTES} bytes"
        )));
    }
    if patch
        .expected_revision
        .is_some_and(|revision| revision != 0)
    {
        return Err(PatchError::new(
            "stale_revision",
            "a new graph is at revision 0",
        ));
    }
    if !patch.skip.is_empty() || !patch.replace.is_empty() {
        return Err(PatchError::new(
            "conflict",
            "a new graph has no nodes to skip or replace",
        ));
    }
    scan(patch, context)?;
    let library = context
        .library
        .with(Layer::Inline, &patch.templates)
        .map_err(PatchError::invalid)?;
    let compiler = Compiler {
        library: &library,
        resolvers: context.resolvers,
    };
    let nodes = compiler.compile_all(&patch.nodes)?;
    if nodes.is_empty() {
        return Err(PatchError::invalid("patch adds no nodes"));
    }
    let mut document = GraphDocument {
        review: None,
        review_subject_missing: false,
        exclude: Vec::new(),
        network: None,
        capability: Vec::new(),
        no_capability: Vec::new(),
        provenance: None,

        review_base: None,
        context: None,
        replay_digest: None,
        submission_patch: None,
        template_library: library.snapshot(),
        template_layers: library.snapshot_layers(),
        template_resolvers: context.resolvers.clone(),
        peer_origin_work_key: None,
        review_tier_override: None,
        extra: Default::default(),
        limits: None,
        integration_branch: integration_branch.map(str::to_owned),
        delivery: None,
        decision_session: None,
        schema_version: 1,
        graph_id: graph_id.to_owned(),
        nodes,
    };

    document.schema_version = u64::from(minimum_schema_version(&document));
    document.admit(context.limits).map_err(|error| {
        let mut refusal = classify(error.to_string());
        refusal.admission = Some(error);
        refusal
    })?;
    admit_candidate(
        &document,
        None,
        &[],
        context.limits,
        &Default::default(),
        None,
    )
    .map_err(|error| {
        let mut refusal = PatchError::invalid(error.to_string());
        refusal.candidate = Some(Box::new(error));
        refusal
    })?;
    Ok(document)
}

/// A dependency cycle gets its own code so a caller can tell it from other
/// invalid patches.
fn classify(reason: String) -> PatchError {
    if reason.contains("cycle") {
        PatchError::new("cycle", format!("patch would create a {reason}"))
    } else {
        PatchError::invalid(reason)
    }
}

pub(super) enum Prepared {
    Replayed(u64),
    Ready(Box<Admission>),
}

pub(super) struct Admission {
    pub expansion: Expansion,
    pub preview: GraphDocument,
    pub plan: PlannedGraph,
    pub warnings: Vec<String>,
}

fn scan(patch: &Patch, context: &PatchContext<'_>) -> Result<(), PatchError> {
    let value = serde_json::to_value(patch).map_err(|_| PatchError::invalid("invalid patch"))?;
    refusal::scan_value(&value, context.scan)
        .map_err(|cause| PatchError::secret(cause.field, cause.rule))
}

/// Compile and check a patch against the current expansion without changing
/// the checkpoint.
pub(super) enum Preparation<'a> {
    Live { retry: Option<&'a str> },
    FinalFragment,
}

pub(super) fn prepare(
    source: &GraphDocument,
    expanded: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    parent: Option<&PlannedNode>,
    patch: &Patch,
    context: &PatchContext<'_>,
    mode: Preparation<'_>,
) -> Result<Prepared, PatchError> {
    let retry = match mode {
        Preparation::Live { retry } => retry,
        Preparation::FinalFragment => None,
    };
    if patch.schema_version != PATCH_SCHEMA_VERSION {
        return Err(PatchError::invalid(format!(
            "patch requires schema_version {PATCH_SCHEMA_VERSION}"
        )));
    }
    let digest = patch.digest()?;
    if let Some(key) = &patch.idempotency_key {
        if key.is_empty() || key.len() > KEY_BYTES || key.contains('\0') {
            return Err(PatchError::invalid(format!(
                "idempotency key must be 1..={KEY_BYTES} bytes"
            )));
        }
        if let Some(record) = checkpoint
            .expansions
            .iter()
            .filter_map(|expansion| expansion.patch.as_ref())
            .find(|record| record.key.as_ref() == Some(key))
        {
            return if record.digest == digest {
                Ok(Prepared::Replayed(record.revision))
            } else {
                Err(PatchError::new(
                    "idempotency_conflict",
                    format!("idempotency key {key} was used for a different patch"),
                ))
            };
        }
    }
    let current = revision(checkpoint);
    if let Some(expected) = patch.expected_revision
        && expected != current
    {
        return Err(PatchError::new(
            "stale_revision",
            format!("patch expects revision {expected}, but the graph is at revision {current}"),
        ));
    }
    if patch
        .reason
        .as_ref()
        .is_some_and(|reason| reason.len() > REASON_BYTES)
    {
        return Err(PatchError::invalid(format!(
            "patch reason exceeds {REASON_BYTES} bytes"
        )));
    }
    if patch.nodes.is_empty() && patch.skip.is_empty() && patch.replace.is_empty() {
        return Err(PatchError::invalid(
            "patch adds, skips and replaces nothing",
        ));
    }
    scan(patch, context)?;

    // A planner's catalogue is host-authored and trusted; inline templates
    // are the author's own and get no more authority than raw nodes.
    let catalogue_spec = parent.and_then(|node| node.plan.as_ref().map(|spec| (node, spec)));
    let planner = match mode {
        Preparation::Live { .. } => None,
        Preparation::FinalFragment => catalogue_spec,
    };
    let catalogue: BTreeMap<String, Template> = catalogue_spec
        .map(|(_, spec)| {
            spec.templates
                .iter()
                .filter_map(|template| template.name.clone().map(|name| (name, template.clone())))
                .collect()
        })
        .unwrap_or_default();
    let library = context
        .library
        .with(Layer::Repository, &catalogue)
        .and_then(|library| library.with(Layer::Inline, &patch.templates))
        .map_err(PatchError::invalid)?;
    let compiler = Compiler {
        library: &library,
        resolvers: context.resolvers,
    };
    let mut local = Vec::with_capacity(patch.nodes.len());
    let mut uses = Vec::new();
    for raw in &patch.nodes {
        let mut node = compiler.compile(raw)?;
        let used = raw.get("use").and_then(Value::as_str);
        let trusted = used.is_some_and(|name| {
            (catalogue.contains_key(name) || context.library.layer(name).is_some())
                && !patch.templates.contains_key(name)
                && ![
                    "role",
                    "source",
                    "capabilities",
                    "attachments",
                    "read_roots",
                    "composite",
                    "loop",
                    "plan",
                ]
                .iter()
                .any(|field| raw.get(*field).is_some())
        });
        if let Some(name) = used {
            // Record the effective parameters, defaults included, so a replay
            // reads exactly what the compiler bound. A swarm binds per copy.
            let supplied = raw.get("with").cloned().unwrap_or(Value::Null);
            let parameters = match raw.get("over") {
                None => library
                    .resolve(name)
                    .and_then(|template| {
                        template
                            .bind(name, &supplied)
                            .map_err(|refusal| refusal.to_string())
                    })
                    .map(Value::Object)
                    .unwrap_or(supplied),
                Some(_) => supplied,
            };
            uses.push(TemplateUse {
                extra: Default::default(),
                id: node.id.clone(),
                name: name.to_owned(),
                parameters,
                nodes: Vec::new(),
            });
        }
        if let Some((planner, spec)) = planner {
            plans::prepare(&mut node, planner, spec, source, trusted)?;
        }
        local.push(node);
    }
    let mut warnings = Vec::new();
    if let Some((planner, spec)) = planner {
        plans::add_floor_uses(
            planner,
            spec,
            source,
            &compiler,
            &mut local,
            &mut uses,
            &mut warnings,
        )?;
        if local.len() > spec.max_nodes {
            return Err(PatchError::invalid(format!(
                "fragment requires 1..{} nodes and schema_version 1",
                spec.max_nodes
            )));
        }
    }

    let mut nodes = if local.is_empty() {
        Vec::new()
    } else {
        plans::flatten_fragment(
            local,
            parent,
            planner.map(|(_, spec)| spec),
            expanded,
            checkpoint,
            &context.worktree_layout,
        )
        .map_err(classify)?
    };
    for instance in &mut uses {
        let id = match parent {
            Some(parent) => format!("{}--{}", parent.id, instance.id),
            None => instance.id.clone(),
        };
        instance.nodes = nodes
            .iter()
            .filter(|child| child.id.starts_with(&format!("{id}--")))
            .map(|child| child.id.clone())
            .collect();
        instance.nodes.sort();
        instance.id = id;
    }
    let existing: BTreeSet<&str> = expanded.nodes.iter().map(|node| node.id.as_str()).collect();
    if let Some(node) = nodes
        .iter()
        .find(|node| existing.contains(node.id.as_str()))
    {
        return Err(PatchError::new(
            "conflict",
            format!("node {} already exists", node.id),
        ));
    }

    // Edits of unstarted nodes.
    let unstarted = |id: &str| -> Result<(), PatchError> {
        if let Some(parent) = parent
            && parent.plan.is_none()
            && parent.template.as_deref() != Some("plan")
            && id != parent.id
            && !id.starts_with(&format!("{}--", parent.id))
        {
            return Err(PatchError::new(
                "edit_scope",
                format!("node {id}: a lane may edit only its own subtree"),
            ));
        }
        let progress = checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == id)
            .ok_or_else(|| PatchError::invalid(format!("unknown node {id}")))?;
        if progress.state != NodeState::Pending || !progress.attempts.is_empty() {
            return Err(PatchError::new(
                "started",
                format!("node {id} has started; started attempts are immutable"),
            ));
        }
        Ok(())
    };
    let mut edited = BTreeSet::new();
    for id in &patch.skip {
        unstarted(id)?;
        if !edited.insert(id.clone()) {
            return Err(PatchError::new(
                "conflict",
                format!("node {id} is edited twice"),
            ));
        }
    }
    let mut replaced = Vec::with_capacity(patch.replace.len());
    for raw in &patch.replace {
        let mut node = compiler.compile(raw)?;
        let id = node.id.clone();
        unstarted(&id)?;
        if !edited.insert(id.clone()) {
            return Err(PatchError::new(
                "conflict",
                format!("node {id} is edited twice"),
            ));
        }
        let target = expanded
            .nodes
            .iter()
            .find(|candidate| candidate.id == id)
            .ok_or_else(|| PatchError::invalid(format!("unknown node {id}")))?;
        let structural = |node: &GraphNode| {
            node.composite.is_some()
                || node.loop_body.is_some()
                || node.plan.is_some()
                || node.round.is_some()
                || !node.composite_members.is_empty()
        };
        let same_retry = retry == Some(id.as_str()) && {
            let mut comparable = node.clone();
            comparable.prompt = target.prompt.clone();
            comparable.round = target.round.clone();
            comparable.after_expansion = target.after_expansion.clone();
            comparable.composite_members = target.composite_members.clone();
            comparable == *target
                && (target.composite.is_none() && target.loop_body.is_none()
                    || node.prompt == target.prompt)
        };
        if (structural(target) || structural(&node)) && !same_retry {
            return Err(PatchError::new(
                "conflict",
                format!("node {id}: replace swaps one unstarted leaf for another"),
            ));
        }
        if let Some((planner, spec)) = planner {
            plans::prepare(&mut node, planner, spec, source, false)?;
        }
        if node.depends_on.is_empty() {
            node.depends_on.clone_from(&target.depends_on);
        }
        if node.after_settled.is_empty() {
            node.after_settled.clone_from(&target.after_settled);
        }
        replaced.push(node);
    }
    // Every node a lane or planner adds or replaces names it, whatever the
    // author wrote, so the graph owner can apply that lane's sandbox floor.
    if let Some(parent) = parent {
        for node in nodes.iter_mut().chain(replaced.iter_mut()) {
            stamp_admitter(node, &parent.id);
        }
    }

    let revision = current + 1;
    let mut expansion = Expansion {
        extra: Default::default(),
        retired: None,
        retry_generation: None,
        final_result: false,
        final_fragment: false,
        floor_warnings: Vec::new(),
        source: parent.map(|node| node.id.clone()).unwrap_or_default(),
        nodes: std::mem::take(&mut nodes),
        uses,
        patch: Some(PatchRecord {
            extra: Default::default(),
            child_receipt: None,
            revision,
            digest,
            key: patch.idempotency_key.clone(),
            parent: parent.map(|node| node.id.clone()),
            reason: patch.reason.clone(),
            skipped: patch.skip.clone(),
            replaced,
            journaled: false,
            restart: None,
        }),
    };
    if matches!(mode, Preparation::FinalFragment) {
        expansion.final_fragment = true;
    }
    let mut preview = expanded.clone();
    plans::append(&mut preview, &expansion);
    let mut loop_writers = BTreeMap::new();
    for writer in preview
        .nodes
        .iter()
        .filter(|node| node.access == AccessMode::WorktreeWrite)
    {
        if let Some(round) = &writer.round
            && let Some(previous) = loop_writers.insert(&round.loop_id, &writer.worktree)
            && previous != &writer.worktree
        {
            return Err(PatchError::new(
                "conflict",
                "loop writers must share one worktree",
            ));
        }
    }
    let mut candidate = checkpoint.clone();
    candidate.expansions.push(expansion.clone());
    // Retry preparation may temporarily remove asks before it refreshes wake.
    asks::refresh_wake(&mut candidate);
    let admitted = admit_candidate(
        source,
        Some(&candidate),
        &[],
        context.limits,
        &Default::default(),
        None,
    )
    .map_err(|error| {
        let mut refusal = match &error {
            CandidateError::Secret(cause) => PatchError::secret(cause.field.clone(), cause.rule),
            _ => PatchError::invalid(error.to_string()),
        };
        refusal.cause = match &error {
            CandidateError::Structural(limit) => Some(PlanFailureFact::Structural(*limit)),
            CandidateError::Envelope(limit) => Some(PlanFailureFact::Envelope(*limit)),
            _ => None,
        };
        refusal.candidate = Some(Box::new(error));
        refusal
    })?;
    let preview = admitted.expanded;
    let plan = admitted.plan;
    Ok(Prepared::Ready(Box::new(Admission {
        expansion,
        preview,
        plan,
        warnings,
    })))
}

/// Name `admitter` on `node` and every node nested in it, including loop
/// bodies and a planner's fallback stage, which expand later.
pub(super) fn stamp_admitter(node: &mut GraphNode, admitter: &str) {
    node.attempt
        .get_or_insert_with(Default::default)
        .admitted_by = Some(admitter.to_owned());
    let nested = [
        node.composite.as_mut().map(|spec| &mut spec.nodes),
        node.loop_body.as_mut().map(|spec| &mut spec.body),
        node.plan
            .as_mut()
            .and_then(|spec| spec.fallback.as_mut())
            .map(|spec| &mut spec.nodes),
    ];
    for member in nested.into_iter().flatten().flatten() {
        stamp_admitter(member, admitter);
    }
}

/// Journal an admission: skip its targets, record the expansion and align
/// progress rows with the new plan. Returns the new revision.
pub(super) fn commit(
    checkpoint: &mut GraphCheckpoint,
    expansion: Expansion,
    preview: &GraphDocument,
    plan: &PlannedGraph,
    limits: &crate::config::GraphLimits,
) -> u64 {
    let skipped = expansion
        .patch
        .as_ref()
        .map(|record| record.skipped.clone())
        .unwrap_or_default();
    let revision = revision(checkpoint) + 1;
    checkpoint.expansions.push(expansion);
    loops::reconcile(checkpoint, plan);
    for progress in &mut checkpoint.nodes {
        if skipped.contains(&progress.id) {
            progress.state = NodeState::Skipped;
            progress.skip_reason = Some(format!("skipped by patch revision {revision}"));
        }
    }
    composites::reconcile_checkpoint(plan, checkpoint);
    plans::remaining(checkpoint, preview, limits);
    revision
}

/// Whether the checkpoint row still carries an admitted patch skip for `id`.
/// A retry resets the row and clears its patch skip reason.
pub(super) fn skipped_by_patch(checkpoint: &GraphCheckpoint, id: &str) -> bool {
    let Some(reason) = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == id)
        .and_then(|progress| progress.skip_reason.as_deref())
    else {
        return false;
    };
    checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref())
        .any(|record| {
            record.skipped.iter().any(|skipped| skipped == id)
                && reason == format!("skipped by patch revision {}", record.revision)
        })
}

pub(super) fn correct_review_verdict(
    graph: &mut LiveGraph<'_>,
    id: &str,
    brief: &str,
    context: &PatchContext<'_>,
) -> Result<Admitted, PatchError> {
    retry_node_scope(
        graph,
        id,
        Some(brief),
        context,
        false,
        Some((&format!("correct review verdict {id}"), None)),
        None,
    )
}
