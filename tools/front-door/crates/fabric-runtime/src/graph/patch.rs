//! One mutation path: every node that joins a graph arrives in a patch.
//!
//! `admit_patch` compiles a patch's nodes and inline templates, checks it
//! against an expected revision and an idempotency key, refuses cycles,
//! conflicting edits and secrets, and journals it as one checkpoint
//! expansion. A planner's fragment, a host submission and a live call from a
//! lane all use it. Started attempts are immutable: `skip` and `replace` act
//! only on nodes that have not started.
use super::template::{Compiler, Layer, Template, TemplateLibrary};
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
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct PatchError {
    /// Stable cause: `stale_revision`, `idempotency_conflict`, `cycle`,
    /// `conflict`, `started`, `secret` or `invalid`.
    pub code: &'static str,
    pub message: String,
}

impl PatchError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }
}

impl From<String> for PatchError {
    fn from(message: String) -> Self {
        Self::invalid(message)
    }
}

/// Everything a patch is compiled and checked against, apart from the graph.
pub struct PatchContext<'a> {
    pub library: &'a TemplateLibrary,
    pub resolvers: &'a BTreeMap<String, Vec<String>>,
    /// Refuses text that matches a secret pattern.
    pub scan: &'a dyn Fn(&str) -> Result<(), String>,
    pub limits: &'a crate::config::GraphLimits,
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
    let expanded = plans::expand(
        graph.document,
        &graph.checkpoint.extensions,
        &graph.checkpoint.expansions,
        context.limits,
    )?;
    let (plan, _) = validate_flat_plan(&expanded, &plans::generous()).map_err(str::to_owned)?;
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
    )? {
        Prepared::Replayed(revision) => Ok(Admitted {
            revision,
            replayed: true,
            added: Vec::new(),
        }),
        Prepared::Ready(admission) => {
            let added = admission
                .expansion
                .nodes
                .iter()
                .map(|node| node.id.clone())
                .collect();
            let Admission {
                expansion,
                preview,
                plan,
                ..
            } = *admission;
            let revision = commit(graph.checkpoint, expansion, &preview, &plan, context.limits);
            Ok(Admitted {
                revision,
                replayed: false,
                added,
            })
        }
    }
}

/// Compile a submitted patch into a new graph document. Submission uses the
/// same compiler and checks as a running graph's patches.
pub fn submit(
    graph_id: &str,
    patch: &Patch,
    context: &PatchContext<'_>,
) -> Result<GraphDocument, PatchError> {
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
        review_base: None,
        context: None,
        extra: Default::default(),
        limits: None,
        integration_branch: None,
        delivery: None,
        decision_session: None,
        schema_version: 1,
        graph_id: graph_id.to_owned(),
        nodes,
    };
    document.schema_version = u64::from(minimum_schema_version(&document));
    document.admit(context.limits).map_err(classify)?;
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
    let text =
        serde_json::to_string(patch).map_err(|error| PatchError::invalid(error.to_string()))?;
    (context.scan)(&text).map_err(|reason| PatchError::new("secret", reason))
}

/// Compile and check a patch against the current expansion without changing
/// the checkpoint.
pub(super) fn prepare(
    source: &GraphDocument,
    expanded: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    parent: Option<&PlannedNode>,
    patch: &Patch,
    context: &PatchContext<'_>,
) -> Result<Prepared, PatchError> {
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
    let planner = parent.and_then(|node| node.plan.as_ref().map(|spec| (node, spec)));
    let catalogue: BTreeMap<String, Template> = planner
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
            catalogue.contains_key(name) && !patch.templates.contains_key(name)
        });
        if let Some(name) = used {
            // Record the effective parameters, defaults included, so a replay
            // reads exactly what the compiler bound. A swarm binds per copy.
            let supplied = raw.get("with").cloned().unwrap_or(Value::Null);
            let parameters = match raw.get("over") {
                None => library
                    .resolve(name)
                    .and_then(|template| template.bind(name, &supplied))
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
        if structural(target) || structural(&node) {
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
        replaced.push(node);
    }

    let revision = current + 1;
    let expansion = Expansion {
        review: None,
        extra: Default::default(),
        source: parent.map(|node| node.id.clone()).unwrap_or_default(),
        continuation: false,
        nodes: std::mem::take(&mut nodes),
        uses,
        patch: Some(PatchRecord {
            extra: Default::default(),
            revision,
            digest,
            key: patch.idempotency_key.clone(),
            parent: parent.map(|node| node.id.clone()),
            reason: patch.reason.clone(),
            skipped: patch.skip.clone(),
            replaced,
        }),
    };
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
    validate_schema(&preview).map_err(str::to_owned)?;
    let (plan, _) = validate_flat_plan(&preview, &plans::generous())
        .map_err(|reason| classify(reason.into()))?;
    validate_output_kinds(&preview)?;
    if let Some(reason) = structural_limit_reason(&preview, &plan, context.limits) {
        return Err(PatchError::invalid(reason));
    }
    let mut expansions = checkpoint.expansions.clone();
    expansions.push(expansion.clone());
    if let Some(reason) = graph_envelope_reason(source, &expansions, &checkpoint.extensions, &plan)
    {
        return Err(PatchError::invalid(reason));
    }
    Ok(Prepared::Ready(Box::new(Admission {
        expansion,
        preview,
        plan,
        warnings,
    })))
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
    plans::remaining(checkpoint, preview, limits);
    revision
}

/// Whether an admitted patch skipped `id`.
pub(super) fn skipped_by_patch(checkpoint: &GraphCheckpoint, id: &str) -> bool {
    checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref())
        .any(|record| record.skipped.iter().any(|skipped| skipped == id))
}
