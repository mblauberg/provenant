//! Live children: a running lane adds nodes to its own graph.
//!
//! A lane's `provenant run` or `fabric_run` crosses
//! `crates/fabric-mcp/src/lane.rs#HostRelay`; the host finds the node that
//! owns the calling attempt and admits the child through
//! [`super::patch::admit_patch`], so a child is an ordinary node named
//! `parent--child` in the same graph, sharing its budget, cancellation and
//! trace. The idempotency key makes a retried call return the same receipt.
//!
//! Spawning is recorded apart from completion: a parent that waits on its
//! children may yield its concurrency slot, and cancelling a parent cancels
//! its subtree except for children it explicitly detached.
use super::patch::{Admitted, LiveGraph, PATCH_SCHEMA_VERSION, Patch, PatchContext, PatchError};
use super::*;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Child depth when the graph document sets no `limits.child_depth`.
pub const DEFAULT_CHILD_DEPTH: usize = 8;
/// Node field marking a child that outlives its parent's cancellation.
pub const DETACHED: &str = "detached";
/// Progress field set while a parent waits on its children.
pub const YIELDED_SLOT: &str = "yielded_slot";
pub(super) const DETACHED_FROM_GRAPH_CANCEL: &str = "detached_from_graph_cancel";
pub(super) const SUBTREE_CANCEL: &str = "subtree_cancel";
const SEPARATOR: &str = "--";
const ID_BYTES: usize = 64;
const REQUEST_DIGEST: &str = "relay_request_digest";

/// A document launch retains its node overrides and inline template definitions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildLaunch {
    pub node: Value,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub templates: std::collections::BTreeMap<String, template::Template>,
}

/// A lane's request to add one child to its graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRequest {
    /// Local ID; the node joins as `parent--id`.
    pub id: String,
    /// Template to `use`; a plain lane is `task`.
    #[serde(default = "task", rename = "use")]
    pub template: String,
    pub brief: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Preserve the caller's provider-process limit when adding the graph node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    /// A writer child gets its own worktree.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub writer: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_scope: Vec<String>,
    /// A detached child is not cancelled with its parent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub detached: bool,
    /// Repeating a key returns the earlier receipt.
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<ChildLaunch>,
}

impl ChildRequest {
    /// An explicit worktree is an existing binding, not a request to create one.
    pub fn needs_worktree_provisioning(&self) -> bool {
        self.writer
            && self
                .launch
                .as_ref()
                .and_then(|launch| launch.node.get("worktree"))
                .and_then(Value::as_str)
                .is_none()
    }

    /// Whether this launch contains a writer, including below fan-out boundaries.
    pub fn has_writers(&self) -> bool {
        fn writes(node: &Value) -> bool {
            node["access"] == "worktree_write"
                || ["/composite/nodes", "/loop/body"].iter().any(|pointer| {
                    node.pointer(pointer)
                        .and_then(Value::as_array)
                        .is_some_and(|nodes| nodes.iter().any(writes))
                })
        }
        self.writer
            || self
                .launch
                .as_ref()
                .is_some_and(|launch| writes(&launch.node))
    }
}

/// One graph frame from a lane, tagged by `op`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RelayOp {
    /// Read the calling graph through the host service; never open lane journals.
    Read { input: Value },
    /// Add a child of the calling node.
    Add(ChildRequest),
    /// Apply a revisioned patch to the calling lane's graph.
    Edit { id: String, patch: Patch },
    /// Mark the calling node as waiting on its children, or working again.
    Wait { waiting: bool },
    /// Observe the calling node's children.
    Children,
    /// Stop one node within the calling lane's subtree.
    Stop { node: String },
}

fn task() -> String {
    "task".into()
}

/// What the host returns to the lane for an admitted child.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildReceipt {
    pub graph_id: String,
    pub parent: String,
    pub node: String,
    pub revision: u64,
    /// The key matched an earlier admission; nothing changed.
    pub replayed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub detached: bool,
    /// Explain whether the child outlives the calling lane.
    pub lifecycle_notice: String,
}

fn child_lifecycle_notice(detached: bool) -> &'static str {
    if detached {
        "Detached child: it may continue after the parent settles."
    } else {
        "Attached child: live children are cancelled when the parent settles, including a successful exit. Use --wait to collect completion before exiting, or explicitly detach the child."
    }
}

/// The graph's child depth: `limits.child_depth` in its document, else 8.
pub fn child_depth(document: &GraphDocument) -> usize {
    document
        .limits
        .as_ref()
        .and_then(|limits| limits.get("child_depth"))
        .and_then(Value::as_u64)
        .and_then(|depth| usize::try_from(depth).ok())
        .unwrap_or(DEFAULT_CHILD_DEPTH)
}

/// Nesting below a top-level node: `a` is 0, `a--b` is 1.
pub fn depth(id: &str) -> usize {
    id.matches(SEPARATOR).count()
}

/// The running node whose live attempt is `attempt`. The relay passes the
/// attempt it was started for, so a lane cannot act as another node.
pub fn parent_of_attempt<'a>(checkpoint: &'a GraphCheckpoint, attempt: &str) -> Option<&'a str> {
    checkpoint
        .nodes
        .iter()
        .filter(|node| node.state == NodeState::Running)
        .find(|node| {
            node.attempts.iter().any(|row| {
                row.state != AttemptState::Terminal && row.attempt_ref.as_deref() == Some(attempt)
            })
        })
        .map(|node| node.id.as_str())
}

/// A writer child's own worktree, beside the graph's other lane worktrees.
pub fn child_worktree(graph_id: &str, node: &str) -> String {
    format!(".worktrees/{graph_id}-{node}")
}

/// The node that owns `attempt` in any state, including a settled parent or a
/// superseded attempt. Only receipt replay may act as such a node.
pub fn historical_parent_of_attempt<'a>(
    checkpoint: &'a GraphCheckpoint,
    attempt: &str,
) -> Option<&'a str> {
    checkpoint
        .nodes
        .iter()
        .find(|node| {
            node.recorded_attempts()
                .any(|row| row.attempt_ref.as_deref() == Some(attempt))
        })
        .map(|node| node.id.as_str())
}

fn writer_worktree(graph: &str, node: &str, request: &ChildRequest) -> Option<String> {
    request.writer.then(|| {
        request
            .launch
            .as_ref()
            .and_then(|launch| launch.node.get("worktree"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| child_worktree(graph, node))
    })
}

fn request_key(parent: &str, request: &ChildRequest) -> Result<String, PatchError> {
    CanonicalV1::record_digest("graph-child-key", 1, &(parent, &request.idempotency_key))
        .map(|digest| format!("child:{}", digest))
        .map_err(|error| PatchError {
            cause: None,
            admission: None,
            parameter: None,
            code: "invalid",
            message: error.to_string(),
        })
}

/// The earlier receipt for this request's idempotency key, if any. It compares
/// the submitted request before consulting mutable templates or moving host
/// bindings, and does not require the parent to be running, so a receipt lost
/// in flight can be replayed after the parent settles.
pub fn replay_child(
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    parent: &str,
    request: &ChildRequest,
) -> Result<Option<ChildReceipt>, PatchError> {
    let invalid = |message: String| PatchError {
        cause: None,
        admission: None,
        parameter: None,
        code: "invalid",
        message,
    };
    let graph_id = document.graph_id.clone();
    let digest = CanonicalV1::record_digest("fabric-relay-child", 1, request)
        .map_err(|error| invalid(error.to_string()))?
        .to_string();
    let key = request_key(parent, request)?;
    let Some(record) = checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref())
        .find(|record| record.key.as_deref() == Some(key.as_str()))
    else {
        return Ok(None);
    };
    let original_parent = record.parent.as_deref().unwrap_or_default();
    if original_parent != parent
        || record.extra.get(REQUEST_DIGEST).and_then(Value::as_str) != Some(digest.as_str())
    {
        return Err(PatchError {
            cause: None,
            admission: None,
            parameter: None,
            code: "idempotency_conflict",
            message: "child idempotency key was used for a different request or parent".into(),
        });
    }
    let node = format!("{original_parent}{SEPARATOR}{}", request.id);
    let worktree = writer_worktree(&graph_id, &node, request);
    Ok(Some(ChildReceipt {
        graph_id,
        parent: original_parent.into(),
        node,
        revision: record.revision,
        replayed: true,
        worktree,
        detached: request.detached,
        lifecycle_notice: child_lifecycle_notice(request.detached).into(),
    }))
}

const PENDING_PROVISIONING: &str = "pending_writer_provisioning";

/// Fence a writer child's worktree creation in the checkpoint. The caller saves
/// the checkpoint before any Git effect, so a crash leaves the intent durable.
pub fn fence_provisioning(checkpoint: &mut GraphCheckpoint, node: &str, path: &str, source: &str) {
    // Retain creation intent in the child spec too: an explicit retry clears
    // progress and needs to recreate this checkout after a definitive refusal.
    for spec in checkpoint
        .expansions
        .iter_mut()
        .flat_map(|expansion| &mut expansion.nodes)
    {
        if (spec.id == node || spec.id.starts_with(&format!("{node}--")))
            && spec.worktree.as_deref() == Some(path)
        {
            spec.extra.insert("provision_worktree".into(), true.into());
        }
    }
    let entry = checkpoint
        .extra
        .entry(PENDING_PROVISIONING.into())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(map) = entry.as_object_mut() {
        map.insert(
            node.into(),
            serde_json::json!({"path": path, "source": source}),
        );
    }
}

/// The logical node that owns `writer`'s provisioning: the outermost expanded
/// composite that exports the writer's commit (a planned implementation's
/// boundary), or the writer itself. Fences, refusals and retries key on it, as
/// a relayed add keys on its receipt node.
pub fn provisioning_owner(checkpoint: &GraphCheckpoint, writer: &str) -> String {
    let specs: Vec<&GraphNode> = checkpoint
        .expansions
        .iter()
        .flat_map(|expansion| &expansion.nodes)
        .collect();
    let mut owner = writer.to_owned();
    for _ in 0..specs.len() {
        let Some(boundary) = specs.iter().find(|spec| {
            owner.starts_with(&format!("{}--", spec.id))
                && spec.composite.as_ref().is_some_and(|composite| {
                    composite.exports.iter().any(|export| {
                        export.node == owner && export.outputs.contains(&OutputKind::Commit)
                    })
                })
        }) else {
            break;
        };
        owner.clone_from(&boundary.id);
    }
    owner
}

/// Provisioning fenced but not yet confirmed: `(node, path, source)`.
pub fn pending_provisioning(checkpoint: &GraphCheckpoint) -> Vec<(String, String, String)> {
    checkpoint
        .extra
        .get(PENDING_PROVISIONING)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(node, value)| {
            Some((
                node.clone(),
                value.get("path")?.as_str()?.to_owned(),
                value.get("source")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// Clear the fence when recording successful provisioning or a definitive refusal.
pub fn clear_provisioning(checkpoint: &mut GraphCheckpoint, node: &str) {
    if let Some(map) = checkpoint
        .extra
        .get_mut(PENDING_PROVISIONING)
        .and_then(Value::as_object_mut)
    {
        map.remove(node);
        if map.is_empty() {
            checkpoint.extra.remove(PENDING_PROVISIONING);
        }
    }
}

/// Admit one child of `parent` through the patch path.
pub fn admit_child(
    graph: &mut LiveGraph<'_>,
    parent: &str,
    request: &ChildRequest,
    context: &PatchContext<'_>,
) -> Result<ChildReceipt, PatchError> {
    admit_child_bound(graph, parent, request, context, None)
}

/// Host-bound writer destination and source; these are never lane arguments.
pub fn admit_child_bound(
    graph: &mut LiveGraph<'_>,
    parent: &str,
    request: &ChildRequest,
    context: &PatchContext<'_>,
    writer_base: Option<(&str, &str)>,
) -> Result<ChildReceipt, PatchError> {
    let invalid = |message: String| PatchError {
        cause: None,
        admission: None,
        parameter: None,
        code: "invalid",
        message,
    };
    if request.id.is_empty()
        || request.id.len() > ID_BYTES
        || request.id.contains(SEPARATOR)
        || !request
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid(format!(
            "child id must be 1..={ID_BYTES} letters, digits, `-` or `_`, without `--`"
        )));
    }
    let graph_id = graph.document.graph_id.clone();
    let node = format!("{parent}{SEPARATOR}{}", request.id);
    if let Some(receipt) = replay_child(graph.document, graph.checkpoint, parent, request)? {
        return Ok(receipt);
    }
    let worktree = writer_worktree(&graph_id, &node, request);
    let digest = CanonicalV1::record_digest("fabric-relay-child", 1, request)
        .map_err(|error| invalid(error.to_string()))?
        .to_string();
    let mut patch = child_patch(request, worktree.as_deref(), context)?;
    patch.idempotency_key = Some(request_key(parent, request)?);
    if let Some((branch, commit)) = writer_base {
        fn bind_writers(node: &mut Value, branch: &str, commit: &str, provision_root: bool) {
            // A generated planning boundary reads the same host-pinned source
            // as its writer. Composite expansion passes this to both leaves.
            if provision_root && node.get("source").is_none_or(Value::is_null) {
                node["source"] = serde_json::json!({"type":"commit", "commit":commit});
            }
            let mut boundary = false;
            for pointer in ["/composite/nodes", "/loop/body"] {
                if let Some(children) = node.pointer_mut(pointer).and_then(Value::as_array_mut) {
                    boundary = true;
                    for child in children {
                        bind_writers(child, branch, commit, false);
                    }
                }
            }
            if !boundary && node["access"] == "worktree_write" {
                if node.get("delivery").is_none_or(Value::is_null) {
                    node["delivery"] = serde_json::json!(
                        provenant_records::run::DeliveryMode::Integrate(branch.into())
                    );
                }
                if node.get("source").is_none_or(Value::is_null)
                    && (provision_root || node.get("worktree").and_then(Value::as_str).is_none())
                {
                    node["source"] = serde_json::json!({"type":"commit", "commit":commit});
                }
            }
        }
        for node in &mut patch.nodes {
            bind_writers(node, branch, commit, request.needs_worktree_provisioning());
        }
    }
    {
        let progress = graph
            .checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == parent)
            .ok_or_else(|| invalid(format!("unknown parent node {parent}")))?;
        if progress.state != NodeState::Running {
            return Err(PatchError {
                cause: None,
                admission: None,
                parameter: None,
                code: "parent_settled",
                message: format!("parent node {parent} is not running"),
            });
        }
        let limit = child_depth(graph.document);
        if depth(&node) > limit {
            return Err(PatchError {
                cause: None,
                admission: None,
                parameter: None,
                code: "child_depth",
                message: format!(
                    "child {node} is {} levels deep; limits.child_depth={limit}",
                    depth(&node)
                ),
            });
        }
    }
    // Add replay evidence to the same record before the journal callback runs.
    let capture = |record: &super::patch::PatchRecord| {
        let mut record = record.clone();
        record
            .extra
            .insert(REQUEST_DIGEST.into(), digest.clone().into());
        if let Some(journal) = context.journal {
            journal(&record)?;
        }
        Ok(())
    };
    let admission_context = PatchContext {
        journal: Some(&capture),
        ..*context
    };
    let Admitted {
        revision, replayed, ..
    } = super::patch::admit_patch(graph, Some(parent), &patch, &admission_context)?;
    let record = graph
        .checkpoint
        .expansions
        .last_mut()
        .and_then(|expansion| expansion.patch.as_mut())
        .ok_or_else(|| invalid("admitted child patch record missing".into()))?;
    record.extra.insert(REQUEST_DIGEST.into(), digest.into());
    record.journaled = context.journal.is_some();
    // A retried parent can admit fresh live children after an earlier settle.
    // Re-arm closure only when a new admission actually commits.
    if let Some(progress) = graph
        .checkpoint
        .nodes
        .iter_mut()
        .find(|node| node.id == parent)
    {
        progress.extra.remove("children_cancelled");
    }
    Ok(ChildReceipt {
        graph_id,
        parent: parent.to_owned(),
        node,
        revision,
        replayed,
        worktree,
        detached: request.detached,
        lifecycle_notice: child_lifecycle_notice(request.detached).into(),
    })
}

fn child_patch(
    request: &ChildRequest,
    worktree: Option<&str>,
    context: &PatchContext<'_>,
) -> Result<Patch, PatchError> {
    if let Some(launch) = &request.launch {
        let library = context
            .library
            .with(template::Layer::Inline, &launch.templates)
            .map_err(PatchError::from)?;
        let mut node = launch.node.clone();
        node["id"] = request.id.clone().into();
        if let Some(path) = worktree {
            node["access"] = "worktree_write".into();
            node["worktree"] = path.into();
            if !request.write_scope.is_empty() {
                node["write_scope"] = serde_json::json!(request.write_scope);
            }
        }
        let mut compiled = Compiler {
            library: &library,
            resolvers: context.resolvers,
        }
        .lower(&node)
        .map_err(PatchError::from)?;
        inherit_child_controls(&mut compiled, request);
        if request.detached {
            compiled[DETACHED] = true.into();
        }
        return Ok(Patch {
            schema_version: PATCH_SCHEMA_VERSION,
            idempotency_key: Some(request.idempotency_key.clone()),
            reason: request.reason.clone(),
            templates: launch.templates.clone(),
            nodes: vec![compiled],
            ..Default::default()
        });
    }
    let mut node = Map::new();
    node.insert("id".into(), request.id.clone().into());
    node.insert("use".into(), request.template.clone().into());
    node.insert("brief".into(), request.brief.clone().into());
    if let Some(tier) = &request.tier {
        node.insert("tier".into(), tier.clone().into());
    }
    if let Some(seconds) = request.timeout_seconds {
        node.insert("deadline_seconds".into(), seconds.into());
    }
    if let Some(worktree) = worktree {
        node.insert("access".into(), "worktree_write".into());
        node.insert("worktree".into(), worktree.into());
        if !request.write_scope.is_empty() {
            node.insert("write_scope".into(), request.write_scope.clone().into());
        }
    }
    if request.detached {
        node.insert("extensions".into(), serde_json::json!({DETACHED:true}));
    }
    if let Ok(template) = context.library.resolve(&request.template)
        && template.is_composite()
    {
        if request.writer {
            return Err(PatchError {
                cause: None,
                admission: None,
                parameter: None,
                code: "invalid",
                message: "writer composite children require explicit per-node work bindings".into(),
            });
        }
        node.remove("brief");
        node.remove("tier");
        if template.parameters.contains_key("brief") {
            node.insert("with".into(), serde_json::json!({"brief": request.brief}));
        }
        let compiler = Compiler {
            library: context.library,
            resolvers: context.resolvers,
        };
        let mut compiled = compiler
            .lower(&Value::Object(node.clone()))
            .map_err(PatchError::from)?;
        inherit_child_controls(&mut compiled, request);
        if !template.parameters.contains_key("brief") && !request.brief.is_empty() {
            append_child_brief(&mut compiled, &request.brief);
        }
        template::preserve_compiled_metadata(&mut compiled).map_err(PatchError::from)?;
        // Keep `use` and its effective parameters for admission evidence. The
        // compiled members override the template's default members on admission.
        if let Some(composite) = compiled.get("composite") {
            node.insert("composite".into(), composite.clone());
        }
    }
    Ok(Patch {
        schema_version: PATCH_SCHEMA_VERSION,
        idempotency_key: Some(request.idempotency_key.clone()),
        reason: request.reason.clone(),
        nodes: vec![Value::Object(node)],
        ..Default::default()
    })
}

// A composite with no brief parameter still receives the caller's subject.
// Preserve each template's instructions and append the supplied context.
fn append_child_brief(node: &mut Value, brief: &str) {
    let mut boundary = false;
    for pointer in ["/composite/nodes", "/loop/body"] {
        if let Some(children) = node.pointer_mut(pointer).and_then(Value::as_array_mut) {
            boundary = true;
            for child in children {
                append_child_brief(child, brief);
            }
        }
    }
    if !boundary && node.get("prompt").is_some() {
        node["prompt"] =
            format!("{}\n\n{brief}", node["prompt"].as_str().unwrap_or_default()).into();
    }
}

/// A launch limit applies to the provider processes, including those below
/// composite and loop boundaries. Containers do not launch a provider process.
fn inherit_child_controls(node: &mut Value, request: &ChildRequest) {
    let mut boundary = false;
    for pointer in ["/composite/nodes", "/loop/body"] {
        if let Some(children) = node.pointer_mut(pointer).and_then(Value::as_array_mut) {
            boundary = true;
            for child in children {
                inherit_child_controls(child, request);
            }
        }
    }
    if boundary {
        return;
    }
    if let Some(seconds) = request.timeout_seconds {
        node["deadline_seconds"] = seconds.into();
    }
    // Deterministic actions retain their executor and access policy.
    if (node.get("kind").is_none()
        || matches!(node["kind"].as_str(), Some("task" | "plan" | "synthesise")))
        && let Some(tier) = &request.tier
    {
        node["route"] = serde_json::json!({"alias": tier});
        if let Some(object) = node.as_object_mut() {
            object.remove("routes");
        }
    }
}

/// Mark or clear a parent as waiting on its children. A waiting parent does
/// not count against `limits.concurrency`, so its children can run.
pub fn set_waiting(checkpoint: &mut GraphCheckpoint, parent: &str, waiting: bool) -> bool {
    let Some(progress) = checkpoint.nodes.iter_mut().find(|node| node.id == parent) else {
        return false;
    };
    if waiting && progress.state == NodeState::Running {
        progress.extra.insert(YIELDED_SLOT.into(), true.into());
    } else {
        progress.extra.remove(YIELDED_SLOT);
    }
    true
}

/// Whether a running node has yielded its slot while it waits.
pub fn yields_slot(progress: &NodeProgress) -> bool {
    progress.state == NodeState::Running
        && progress.extra.get(YIELDED_SLOT) == Some(&Value::Bool(true))
}

/// The effect of cancelling a parent's subtree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubtreeCancel {
    /// Unstarted descendants now skipped.
    pub skipped: Vec<String>,
    /// Running descendants whose attempts the host must cancel.
    pub running: Vec<String>,
    /// Descendants left alone because they, or an ancestor below the
    /// parent, were detached.
    pub detached: Vec<String>,
}

/// Fence explicitly detached branches out of whole-graph cancellation. The
/// persisted flags let recovery settle cancellation without reloading templates.
pub fn fence_detached_cancellation(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
) -> Result<BTreeSet<String>, String> {
    let expanded = plans::expand(
        document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &plans::generous(),
    )?;
    let roots: Vec<_> = expanded
        .nodes
        .iter()
        .filter(|node| node.extra.get(DETACHED) == Some(&Value::Bool(true)))
        .map(|node| node.id.as_str())
        .collect();
    let mut protected = BTreeSet::new();
    for node in &mut checkpoint.nodes {
        if roots
            .iter()
            .any(|root| node.id == *root || node.id.starts_with(&format!("{root}{SEPARATOR}")))
        {
            node.extra
                .insert(DETACHED_FROM_GRAPH_CANCEL.into(), true.into());
            protected.insert(node.id.clone());
        } else {
            node.extra.remove(DETACHED_FROM_GRAPH_CANCEL);
        }
    }
    Ok(protected)
}

/// Cancel every descendant of `parent` that is not detached. Settled nodes
/// are left as they are; running ones are returned for the host to stop.
pub fn cancel_subtree(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    parent: &str,
) -> Result<SubtreeCancel, String> {
    let expanded = plans::expand(
        document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &plans::generous(),
    )?;
    let detached: BTreeSet<&str> = expanded
        .nodes
        .iter()
        .filter(|node| node.extra.get(DETACHED) == Some(&Value::Bool(true)))
        .map(|node| node.id.as_str())
        .collect();
    let prefix = format!("{parent}{SEPARATOR}");
    let mut result = SubtreeCancel::default();
    let mut changed = false;
    for progress in &mut checkpoint.nodes {
        if progress.id == parent {
            continue;
        }
        let Some(rest) = progress.id.strip_prefix(&prefix) else {
            continue;
        };
        // `parent--a--b` is protected if `parent--a` or `parent--a--b` is detached.
        let mut ancestor = parent.to_owned();
        let protected = rest.split(SEPARATOR).any(|part| {
            ancestor.push_str(SEPARATOR);
            ancestor.push_str(part);
            detached.contains(ancestor.as_str())
        });
        if protected {
            result.detached.push(progress.id.clone());
            continue;
        }
        next_step::fence_stopped(progress);
        let live = progress
            .attempts
            .iter()
            .any(|attempt| attempt.state != AttemptState::Terminal);
        match progress.state {
            NodeState::Pending | NodeState::Running if !live => {
                progress.cancelled_from = Some(progress.state);
                progress.state = NodeState::Skipped;
                progress.skip_reason = Some(format!("parent {parent} cancelled"));
                progress.extra.remove(YIELDED_SLOT);
                result.skipped.push(progress.id.clone());
                changed = true;
            }
            NodeState::Pending | NodeState::Running => {
                progress.extra.insert(SUBTREE_CANCEL.into(), parent.into());
                progress.extra.remove(YIELDED_SLOT);
                result.running.push(progress.id.clone());
            }
            _ => {}
        }
    }
    if changed {
        asks::refresh_wake(checkpoint);
    }
    Ok(result)
}

/// Fence settled parents' children before stopping their attempts. Repeated
/// advances retry uncertain stops without ever observing/relaunching the child.
pub(super) fn settle_subtrees<R: AttemptOwner, J: GraphJournal>(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut R,
    journal: &mut J,
    stopped: &mut BTreeSet<(String, u16)>,
) -> Result<(), GraphError<R::Error, J::Error>> {
    let parents: Vec<String> = checkpoint
        .nodes
        .iter()
        .filter(|node| {
            node.state.settled() && node.extra.get("children_cancelled") != Some(&Value::Bool(true))
        })
        .map(|node| node.id.clone())
        .collect();
    if !parents.is_empty() {
        for parent in parents {
            let succeeded = checkpoint
                .nodes
                .iter()
                .any(|node| node.id == parent && node.state == NodeState::Succeeded);
            if succeeded {
                // Final patches deliberately schedule work after their producer
                // completes. Only live relay children share the process lifetime.
                let children: Vec<String> = checkpoint
                    .expansions
                    .iter()
                    .filter(|expansion| {
                        expansion.patch.as_ref().is_some_and(|record| {
                            record
                                .parent
                                .as_deref()
                                .is_some_and(|source| source == parent)
                                && record.extra.contains_key(REQUEST_DIGEST)
                        })
                    })
                    .flat_map(|expansion| expansion.nodes.iter())
                    .filter(|node| {
                        node.extra.get(DETACHED) != Some(&Value::Bool(true))
                            && checkpoint
                                .nodes
                                .iter()
                                .any(|progress| progress.id == node.id && !progress.state.settled())
                    })
                    .map(|node| node.id.clone())
                    .collect();
                for child in children {
                    super::cancel_node_subtree(
                        document,
                        checkpoint,
                        &child,
                        Some("parent settled"),
                    )
                    .map_err(GraphError::Refused)?;
                }
            } else {
                cancel_subtree(document, checkpoint, &parent).map_err(GraphError::Refused)?;
            }
            checkpoint
                .nodes
                .iter_mut()
                .find(|node| node.id == parent)
                .ok_or_else(|| {
                    GraphError::Refused("cancelled parent missing from checkpoint".into())
                })?
                .extra
                .insert("children_cancelled".into(), true.into());
        }
        journal.save(checkpoint).map_err(GraphError::Journal)?;
    }
    let mut first_error = None;
    for index in 0..checkpoint.nodes.len() {
        let node = &checkpoint.nodes[index];
        let Some(parent) = node
            .extra
            .get(SUBTREE_CANCEL)
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        if node.state.settled() {
            continue;
        }
        let mut settled = true;
        for attempt in node
            .attempts
            .iter()
            .filter(|attempt| attempt.state != AttemptState::Terminal)
        {
            match owner.cancelled_attempt_settled(attempt) {
                Ok(true) => {}
                observed => {
                    settled = false;
                    if let Err(error) = observed {
                        first_error.get_or_insert(error);
                    }
                    if stopped.insert((attempt.key.node_id.clone(), attempt.key.route_index))
                        && let Err(error) = owner.stop_cancelled_attempt(attempt)
                    {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        if settled {
            let node = &mut checkpoint.nodes[index];
            node.cancelled_from = Some(node.state);
            node.state = NodeState::Skipped;
            node.skip_reason = Some(format!("parent {parent} cancelled"));
            asks::refresh_wake(checkpoint);
            journal.save(checkpoint).map_err(GraphError::Journal)?;
        }
    }
    match first_error {
        Some(error) => Err(GraphError::Run(error)),
        None => Ok(()),
    }
}

/// Child states visible to their parent, including terminal delivery outcomes.
pub fn child_states(checkpoint: &GraphCheckpoint, parent: &str) -> Value {
    let roots: Vec<_> = checkpoint
        .expansions
        .iter()
        .filter(|expansion| {
            expansion
                .patch
                .as_ref()
                .and_then(|record| record.parent.as_deref())
                .is_some_and(|source| source == parent)
        })
        .flat_map(|expansion| expansion.nodes.iter().map(|node| &node.id))
        .collect();
    let children: Vec<_> = checkpoint
        .nodes
        .iter()
        .filter(|node| {
            roots
                .iter()
                .any(|root| node.id == **root || node.id.starts_with(&format!("{root}--")))
        })
        .map(|node| {
            serde_json::json!({"node":node.id, "state":node.state,
            "settled":node.state.settled()})
        })
        .collect();
    serde_json::json!({"children":children})
}

/// Resolve the running parent's worktree from the current graph revision.
pub fn parent_worktree(
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    parent: &str,
    limits: &crate::config::GraphLimits,
) -> Result<Option<String>, String> {
    Ok(plans::expand(
        document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        limits,
    )?
    .nodes
    .into_iter()
    .find(|node| node.id == parent)
    .and_then(|node| node.worktree))
}

/// Captured writer source used when provisioning or recovering a child checkout.
pub fn child_source(
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    child: &str,
    limits: &crate::config::GraphLimits,
) -> Result<Option<String>, String> {
    let expanded = plans::expand(
        document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        limits,
    )?;
    let mut id = child;
    // A planning composite keeps the receipt's logical ID; its commit output
    // identifies the writer whose checkout the host provisions.
    for _ in 0..=expanded.nodes.len() {
        let Some(node) = expanded.nodes.iter().find(|node| node.id == id) else {
            return Ok(None);
        };
        if let Some(SourceBinding::Commit { commit }) = &node.source {
            return Ok(Some(commit.clone()));
        }
        let Some(export) = node.composite.as_ref().and_then(|spec| {
            spec.exports
                .iter()
                .find(|export| export.outputs.contains(&OutputKind::Commit))
        }) else {
            return Ok(None);
        };
        id = &export.node;
    }
    Err("cyclic child commit exports".into())
}
