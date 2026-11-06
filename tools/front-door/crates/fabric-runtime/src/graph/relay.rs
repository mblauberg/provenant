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
const SEPARATOR: &str = "--";
const ID_BYTES: usize = 64;

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
}

/// One graph frame from a lane, tagged by `op`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RelayOp {
    /// Add a child of the calling node.
    Add(ChildRequest),
    /// Mark the calling node as waiting on its children, or working again.
    Wait { waiting: bool },
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

/// Admit one child of `parent` through the patch path.
pub fn admit_child(
    graph: &mut LiveGraph<'_>,
    parent: &str,
    request: &ChildRequest,
    context: &PatchContext<'_>,
) -> Result<ChildReceipt, PatchError> {
    let invalid = |message: String| PatchError {
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
    let worktree = request.writer.then(|| child_worktree(&graph_id, &node));
    let patch = child_patch(request, worktree.as_deref(), context)?;
    // A replay is answered before the parent's state is checked, so a retry
    // after the parent settles still gets its receipt.
    let replay = graph
        .checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref())
        .any(|record| record.key.as_deref() == Some(&request.idempotency_key));
    if !replay {
        let progress = graph
            .checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == parent)
            .ok_or_else(|| invalid(format!("unknown parent node {parent}")))?;
        if progress.state != NodeState::Running {
            return Err(PatchError {
                code: "parent_settled",
                message: format!("parent node {parent} is not running"),
            });
        }
        let limit = child_depth(graph.document);
        if depth(&node) > limit {
            return Err(PatchError {
                code: "child_depth",
                message: format!(
                    "child {node} is {} levels deep; limits.child_depth={limit}",
                    depth(&node)
                ),
            });
        }
    }
    let Admitted {
        revision, replayed, ..
    } = super::patch::admit_patch(graph, Some(parent), &patch, context)?;
    Ok(ChildReceipt {
        graph_id,
        parent: parent.to_owned(),
        node,
        revision,
        replayed,
        worktree,
        detached: request.detached,
    })
}

fn child_patch(
    request: &ChildRequest,
    worktree: Option<&str>,
    context: &PatchContext<'_>,
) -> Result<Patch, PatchError> {
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
    } else if !request.write_scope.is_empty() {
        return Err(PatchError {
            code: "invalid",
            message: "write_scope requires a writer child".into(),
        });
    }
    if request.detached {
        node.insert(DETACHED.into(), true.into());
    }
    if let Ok(template) = context.library.resolve(&request.template)
        && template.is_composite()
    {
        if request.writer {
            return Err(PatchError {
                code: "invalid",
                message: "writer composite children require explicit per-node work bindings".into(),
            });
        }
        if !template.parameters.contains_key("brief") && !request.brief.is_empty() {
            return Err(PatchError {
                code: "invalid",
                message: "composite child template must declare a brief parameter".into(),
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
            .map_err(|message| PatchError {
                code: "invalid",
                message,
            })?;
        inherit_child_controls(&mut compiled, request);
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
    if node.get("kind").is_none()
        || matches!(node["kind"].as_str(), Some("task" | "plan" | "synthesise"))
    {
        if let Some(tier) = &request.tier {
            node["route"] = serde_json::json!({"alias": tier});
            if let Some(object) = node.as_object_mut() {
                object.remove("routes");
            }
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
            NodeState::Pending | NodeState::Running => result.running.push(progress.id.clone()),
            _ => {}
        }
    }
    if changed {
        asks::refresh_wake(checkpoint);
    }
    Ok(result)
}
