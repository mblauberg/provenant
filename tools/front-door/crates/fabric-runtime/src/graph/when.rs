//! Launch conditions on graph nodes.
//!
//! A node may carry `when = { name = value, ... }`. At submission each name is
//! looked up among the enclosing template's parameters, then among the launch
//! resolvers that hold a single value, and the pair is recorded as a term with
//! the value it saw. A node whose terms do not all hold is skipped: it stays in
//! the graph as a skipped row that names its condition, and the nodes around it
//! behave as if it were not there.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// One resolved condition of a node's `when`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhenTerm {
    pub name: String,
    pub expected: String,
    pub actual: String,
}

impl WhenTerm {
    pub fn holds(&self) -> bool {
        self.expected == self.actual
    }
}

/// The launch values the bundled review panel reads: one boolean per optional
/// stage of a review (framing, verification and model synthesis).
pub fn review_stages(frame: bool, verify: bool, synthesis: bool) -> [(String, Vec<String>); 3] {
    [
        ("review-frame", frame),
        ("review-verify", verify),
        ("review-synthesis", synthesis),
    ]
    .map(|(name, on)| (name.to_owned(), vec![on.to_string()]))
}

pub(super) fn holds(terms: &[WhenTerm]) -> bool {
    terms.iter().all(WhenTerm::holds)
}

/// The first unmet term, read as a sentence for status and explain output.
pub fn skip_reason(terms: &[WhenTerm]) -> Option<String> {
    terms.iter().find(|term| !term.holds()).map(|term| {
        format!(
            "when {} = {}; it is {}",
            term.name, term.expected, term.actual
        )
    })
}

/// Nodes whose condition does not hold, with their terms, in document order.
pub fn skipped(nodes: &[GraphNode]) -> Vec<(&str, &[WhenTerm])> {
    nodes
        .iter()
        .filter(|node| !holds(&node.when))
        .map(|node| (node.id.as_str(), node.when.as_slice()))
        .collect()
}

/// Required inputs removed because a producer is skipped.
fn removed_requirements(nodes: &[GraphNode], producer: &str) -> Vec<serde_json::Value> {
    let mut removed = Vec::new();
    for node in nodes.iter().filter(|node| holds(&node.when)) {
        for input in node
            .inputs
            .iter()
            .filter(|input| input.node == producer && !input.required_outputs.is_empty())
        {
            removed.push(serde_json::json!({"consumer": node.id, "required_outputs": input.required_outputs}));
        }
        if let Some(spec) = &node.composite {
            if spec.reduce_verdict
                && spec.exports.iter().any(|export| {
                    export.node == producer && export.outputs.contains(&OutputKind::Verdict)
                })
            {
                removed.push(serde_json::json!({"consumer": node.id, "verdict_input": true}));
            }
        }
    }
    removed
}

/// The skipped rows an expanded document will start with, each with its
/// condition, for `explain` to show beside the document.
pub fn skipped_rows(
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) -> Vec<serde_json::Value> {
    let Ok(expanded) = document.expanded(limits) else {
        return Vec::new();
    };
    skipped(&expanded.nodes)
        .into_iter()
        .map(|(id, terms)| {
            serde_json::json!({"node": id, "when": terms, "reason": skip_reason(terms), "removed_requirements": removed_requirements(&expanded.nodes, id)})
        })
        .collect()
}

/// Remove skipped nodes from the edges of the nodes that stay active. A skipped
/// node keeps its own row; every active node loses its inputs, exports, member
/// listing and ordering edges, and inherits the skipped node's own parents so
/// that the order it had is kept. A reference that needs the skipped node's
/// output, such as a source or a writer to continue, is refused.
pub(super) fn prune(document: &GraphDocument) -> Result<Option<GraphDocument>, AdmissionError> {
    // A loop's round nodes are cloned from its body and do not carry its
    // condition, so a skipped loop would still run its rounds. Whatever the
    // value, the condition is refused rather than half applied.
    if let Some(node) = document
        .nodes
        .iter()
        .find(|node| !node.when.is_empty() && node.loop_body.is_some())
    {
        return Err(AdmissionError::Plan(format!(
            "node {}: when is not supported on a loop",
            node.id
        )));
    }
    let skipped: BTreeSet<&str> = skipped(&document.nodes)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    if skipped.is_empty() {
        return Ok(None);
    }
    let by_id: BTreeMap<&str, &GraphNode> = document
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let refuse = |node: &GraphNode, reason: String| {
        AdmissionError::Plan(format!("node {}: {reason}", node.id))
    };
    // The nearest parents that stay active, through any run of skipped nodes.
    let live = |start: &str| -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pending = vec![start];
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(node) = by_id.get(id) else { continue };
            if skipped.contains(id) {
                pending.extend(node.dependencies().map(String::as_str));
            } else if !found.iter().any(|name| name == id) {
                found.push(id.to_owned());
            }
        }
        found.sort();
        found
    };
    let mut pruned = document.clone();
    for node in &mut pruned.nodes {
        if skipped.contains(node.id.as_str()) {
            let removed = removed_requirements(&document.nodes, &node.id);
            if !removed.is_empty() {
                node.extra
                    .insert("removed_requirements".into(), serde_json::json!(removed));
            }
        }
    }
    for node in &mut pruned.nodes {
        if skipped.contains(node.id.as_str()) {
            continue;
        }
        let mentions = |name: &str| skipped.contains(name);
        let outputs_of = node
            .source
            .as_ref()
            .and_then(|source| match source {
                SourceBinding::Node { node } | SourceBinding::Head { node } => Some(node.as_str()),
                _ => None,
            })
            .into_iter()
            .chain(node.continues.as_deref())
            .chain(node.land.as_ref().map(|spec| spec.node.as_str()))
            .chain(node.on_fail.as_deref())
            .chain(match &node.condition {
                Some(GateCondition::Ci {
                    target: CiTarget::Node { node, .. },
                    ..
                }) => Some(node.as_str()),
                _ => None,
            });
        for name in outputs_of {
            if mentions(name) {
                return Err(refuse(
                    node,
                    format!(
                        "{name} is skipped by its when condition, but this node needs its output"
                    ),
                ));
            }
        }
        let mut depends_on = Vec::new();
        for parent in &node.depends_on {
            let replacement = if mentions(parent) {
                live(parent)
            } else {
                vec![parent.clone()]
            };
            for name in replacement {
                if !depends_on.contains(&name) {
                    depends_on.push(name);
                }
            }
        }
        node.depends_on = depends_on;
        node.inputs.retain(|input| !mentions(&input.node));
        node.composite_members.retain(|name| !mentions(name));
        node.after_expansion.retain(|name| !mentions(name));
        if let Some(spec) = &mut node.composite {
            let before = spec.exports.len();
            spec.exports.retain(|export| !mentions(&export.node));
            if before > 0 && spec.exports.is_empty() {
                return Err(refuse(
                    node,
                    "every export is skipped by a when condition".into(),
                ));
            }
        }
    }
    Ok(Some(pruned))
}
