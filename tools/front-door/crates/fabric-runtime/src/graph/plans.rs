//! Dynamic plans and honest-stop continuations share the scheduler and journal.
use super::*;
use std::collections::BTreeMap;

const FALLBACK_RETRY_CONTEXT: &str = "\n[planner-fragment-retry-context:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSpec {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub kinds: Vec<NodeKind>,
    pub max_nodes: usize,
    /// Patterns this plan node allows a fragment to instantiate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<PatternSpec>,
    /// A fragment may instantiate patterns only, never emit raw nodes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub patterns_only: bool,
    /// Fragment units that no dependency orders form one wave, and the writers
    /// of one wave must hold disjoint scopes. Without it, overlapping writers
    /// share a checkout and run in turn.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disjoint_waves: bool,
    /// Consumers of the plan node also read what the fragment's pattern
    /// instances export. Off, a consumer keeps exactly the inputs it declared.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wire_exports: bool,
    /// Static preset stage to run if this planner's attempt fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<CompositeSpec>,
    /// Existing dependencies and inputs inherited by the static stage.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_inputs: Vec<GraphInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_source: Option<SourceBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_read_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternSpec {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, PatternParameter>,
    /// Kept as JSON until parameter interpolation, because a typed field such as
    /// `write_scope` may be a single `${scope}` placeholder for a list value.
    pub nodes: Vec<serde_json::Value>,
    pub exports: Vec<GraphInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternParameter {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    #[serde(rename = "type")]
    pub kind: PatternParameterKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<serde_json::Value>,
    #[serde(default, rename = "enum", skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternParameterKind {
    String,
    Integer,
    Number,
    Boolean,
    List,
    Table,
}

impl PatternParameterKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::List => "list",
            Self::Table => "table",
        }
    }
}

/// One pattern instantiation retained with the expansion that produced it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternInstantiation {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub id: String,
    pub name: String,
    pub parameters: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<String>,
}

/// Only validated, normalised additions enter the checkpoint. The original
/// document remains the digest authority, just as it does for loop rounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expansion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewResolution>,
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub source: String,
    pub continuation: bool,
    #[serde(with = "expanded_nodes")]
    pub nodes: Vec<GraphNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<PatternInstantiation>,
}

// Documents and untrusted fragments cannot set computed topology. Flattened
// expansion records, however, must retain it across a journal JSON round trip.
mod expanded_nodes {
    use super::*;

    #[derive(Serialize)]
    struct Borrowed<'a> {
        #[serde(flatten)]
        node: &'a GraphNode,
        #[serde(skip_serializing_if = "Option::is_none")]
        composite_members: Option<&'a Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        after_expansion: Option<&'a Vec<String>>,
    }

    #[derive(Deserialize)]
    struct Owned {
        #[serde(flatten)]
        node: GraphNode,
        #[serde(default, deserialize_with = "present_vec")]
        composite_members: Option<Vec<String>>,
        #[serde(default, deserialize_with = "present_vec")]
        after_expansion: Option<Vec<String>>,
    }

    fn present_vec<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<String>>, D::Error> {
        Vec::deserialize(deserializer).map(Some)
    }

    pub(super) fn serialize<S: serde::Serializer>(
        nodes: &[GraphNode],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        if let Some(node) = nodes.iter().find(|node| {
            node.extra.contains_key("composite_members")
                || node.extra.contains_key("after_expansion")
        }) {
            return Err(serde::ser::Error::custom(format!(
                "node {}: computed topology fields are reserved to checkpoint expansions",
                node.id
            )));
        }
        nodes
            .iter()
            .map(|node| Borrowed {
                node,
                composite_members: (node.topology_present.0[0]
                    || !node.composite_members.is_empty())
                .then_some(&node.composite_members),
                after_expansion: (node.topology_present.0[1] || !node.after_expansion.is_empty())
                    .then_some(&node.after_expansion),
            })
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<GraphNode>, D::Error> {
        Ok(Vec::<Owned>::deserialize(deserializer)?
            .into_iter()
            .map(|wire| {
                let mut node = wire.node;
                node.topology_present.0 = [
                    wire.composite_members.is_some(),
                    wire.after_expansion.is_some(),
                ];
                node.composite_members = wire.composite_members.unwrap_or_default();
                node.after_expansion = wire.after_expansion.unwrap_or_default();
                node
            })
            .collect())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemainingBudgets {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub nodes: usize,
    pub edges: usize,
    pub continuations: usize,
}

#[derive(Deserialize)]
struct Fragment {
    schema_version: u64,
    #[serde(default)]
    nodes: Vec<GraphNode>,
    #[serde(default)]
    patterns: Vec<pattern_plan::PatternInstance>,
}

fn pattern_summary(spec: &PlanSpec) -> String {
    spec.patterns
        .iter()
        .map(|pattern| {
            let parameters = pattern
                .parameters
                .iter()
                .map(|(name, parameter)| {
                    let kind = parameter.kind.name();
                    let mut details = vec![match &parameter.default {
                        Some(value) => format!("default={value}"),
                        None => "required".into(),
                    }];
                    if let Some(value) = &parameter.min {
                        details.push(format!("min={value}"));
                    }
                    if let Some(value) = &parameter.max {
                        details.push(format!("max={value}"));
                    }
                    if !parameter.allowed.is_empty() {
                        details.push(format!("enum={}", serde_json::json!(parameter.allowed)));
                    }
                    if let Some(doc) = &parameter.doc {
                        details.push(doc.clone());
                    }
                    format!("{name}:{kind} ({})", details.join(", "))
                })
                .collect::<Vec<_>>()
                .join(", ");
            if parameters.is_empty() {
                pattern.name.clone()
            } else {
                format!("{} ({parameters})", pattern.name)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn prompt(node: &GraphNode) -> String {
    let mut prompt = node.prompt.clone();
    if node.require_findings {
        prompt.push_str(
            "\n\nInclude a fenced `findings` JSON block in your final response. If there are no findings, emit exactly:\n```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nEach finding must include its id, severity, file, summary and status.",
        );
    }
    if let Some(spec) = &node.plan {
        let kinds = serde_json::to_string(&spec.kinds).unwrap_or_default();
        if spec.patterns.is_empty() {
            prompt.push_str(&format!("\n\nReturn one final fenced `fragment` JSON block, at most 12288 bytes: {{\"schema_version\":1,\"nodes\":[...]}}. Emit 1..{} nodes, using only {kinds}. Each agent node requires id, goal or brief, access, `acceptance_checks` and local depends_on. Writers also require `write_scope` (relative literal files/directories) and the `worktree` key naming an existing registered worktree; read-only agents may omit `write_scope`. Route defaults to {{\"alias\":\"senior\"}}. Synthesise nodes require `inputs` selecting dependency outputs, either as `{{\"node\":\"id\",\"outputs\":[\"result\"]}}` or the shorthand `\"id\"` (which selects `result`). Gate nodes use the graph gate condition schema; decide nodes use the graph decide schema. Dependencies and inputs name fragment-local IDs. Keep each namespaced ID (this node's ID + -- + child ID) within 64 bytes. Do not emit presets or review shapes.", spec.max_nodes));
        } else {
            prompt.push_str(&format!("\n\nReturn one final fenced `fragment` JSON block, at most 12288 bytes: {{\"schema_version\":1,\"nodes\":[...],\"patterns\":[...]}}. Emit 1..{} nodes or pattern instances, using only {kinds}. Allowed patterns: {}. Instantiate them in a `patterns` array with id, pattern, parameters, depends_on and inputs; pattern parameters are validated against their schemas and pattern nodes are fixed. Do not emit other presets or review shapes. Each agent node requires id, goal or brief, access, `acceptance_checks` and local depends_on. Writers also require `write_scope` (relative literal files/directories) and the `worktree` key naming an existing registered worktree; read-only agents may omit `write_scope`. Route defaults to {{\"alias\":\"senior\"}}. Synthesise nodes require `inputs` selecting dependency outputs using `{{\"node\":\"id\",\"outputs\":[\"result\"]}}` or the shorthand `\"id\"` (which selects `result`). Gate nodes use the graph gate condition schema; decide nodes use the graph decide schema. Dependencies and inputs name fragment-local IDs. Keep each namespaced ID (this node's ID + -- + child ID) within 64 bytes.", spec.max_nodes, pattern_summary(spec)));
        }
    }
    prompt
}

pub(super) fn validate_spec(node: &GraphNode) -> Result<(), &'static str> {
    if (node.kind == Some(NodeKind::Plan)) != node.plan.is_some() {
        return Err("plan requires fragment kinds and max_nodes");
    }
    if let Some(spec) = &node.plan {
        let mut seen = Vec::new();
        if node.access != AccessMode::ReadOnly
            || node.required_verdict.is_some()
            || spec.max_nodes == 0
            || spec.max_nodes > 512
            || spec.kinds.is_empty()
            || spec.kinds.iter().any(|kind| {
                let duplicate = seen.contains(kind);
                seen.push(*kind);
                duplicate || matches!(*kind, NodeKind::Loop | NodeKind::Composite)
            })
        {
            return Err(
                "invalid plan configuration; fragments support task, plan, synthesise and gate",
            );
        }
        let mut names = std::collections::BTreeSet::new();
        for pattern in &spec.patterns {
            if !names.insert(&pattern.name) {
                return Err("plan pattern names must be unique");
            }
            pattern_plan::validate(pattern)?;
        }
    }
    Ok(())
}

/// A complete fenced block must fit in one existing capture window. Never
/// parse a clipped prefix or infer an honest stop from prose.
/// Raw cap on a planner or handoff block, and the canonical bytes it may
/// occupy once stored in a checkpoint.
const BLOCK_BYTES: usize = 12 * 1024;
pub(super) const STORED_BLOCK_BYTES: usize = 2 * BLOCK_BYTES;
/// A stopped-early handoff is stored whole: its fenced block plus escapes.
pub(super) const STORED_HANDOFF_BYTES: usize =
    2 * (provenant_records::handoff::HANDOFF_BYTES + "```handoff\n\n```".len());

fn block<'a>(
    capture: &'a crate::result_capture::CapturedResult,
    tag: &str,
) -> Result<Option<&'a str>, String> {
    let marker = format!("```{tag}\n");
    let tail = if capture.tail_truncated {
        capture.tail.split_once('\n').map_or("", |(_, tail)| tail)
    } else {
        &capture.tail
    };
    let text = if tail.contains(&marker) {
        tail
    } else if !capture.truncated {
        &capture.excerpt
    } else {
        ""
    };
    let Some((_, rest)) = text.split_once(&marker) else {
        return Ok(None);
    };
    if rest.contains(&marker) {
        return Err(format!("{tag} block repeated"));
    }
    let (json, _) = rest
        .split_once("\n```")
        .ok_or_else(|| format!("{tag} block incomplete"))?;
    if json.len() > BLOCK_BYTES {
        return Err(format!("{tag} block exceeds {BLOCK_BYTES} bytes"));
    }
    if encoded_len(json) > STORED_BLOCK_BYTES {
        return Err(format!(
            "{tag} block has too many escaped characters to store in {STORED_BLOCK_BYTES} bytes"
        ));
    }
    Ok(Some(json))
}

pub(super) fn handoff(
    capture: &crate::result_capture::CapturedResult,
) -> Result<Option<String>, String> {
    let handoff = capture.handoff.clone().map_err(str::to_owned)?;
    let Some(fenced) = handoff
        .filter(|handoff| handoff.status == provenant_records::handoff::HandoffStatus::StoppedEarly)
        .map(|handoff| handoff.fenced())
    else {
        return Ok(None);
    };
    if encoded_len(&fenced) > STORED_HANDOFF_BYTES {
        return Err(format!(
            "handoff block has too many escaped characters to store in {STORED_HANDOFF_BYTES} bytes"
        ));
    }
    Ok(Some(fenced))
}

pub(super) fn writer(node: &PlannedNode) -> bool {
    node.access == AccessMode::WorktreeWrite && node.kind.is_none_or(|kind| kind == NodeKind::Task)
}

pub(super) fn expand(
    document: &GraphDocument,
    extensions: &[String],
    expansions: &[Expansion],
    limits: &crate::config::GraphLimits,
) -> Result<GraphDocument, String> {
    let mut expanded = loops::expand(document, extensions, limits)?;
    let mut sources = std::collections::BTreeSet::new();
    for addition in expansions {
        if addition.nodes.is_empty()
            || !sources.insert(&addition.source)
            || !expanded.nodes.iter().any(|n| n.id == addition.source)
        {
            return Err("invalid expansion source".into());
        }
        validate_review_addition(&expanded, addition)?;
        append(&mut expanded, addition);
    }
    Ok(expanded)
}

/// Check persisted topology against the approved catalogue or fallback,
/// independently of progress rows.
pub(super) fn validate_persisted_topology(
    document: &GraphDocument,
    extensions: &[String],
    expansions: &[Expansion],
    limits: &crate::config::GraphLimits,
) -> Result<(), (String, &'static str)> {
    let mut expanded = loops::expand(document, extensions, limits).map_err(|_| {
        (
            document.graph_id.clone(),
            "expansion topology requires a valid base plan",
        )
    })?;
    for addition in expansions {
        // A deferred review is resolved by the host, not derived from a planner
        // specification; its topology has always been persisted with it.
        if expanded
            .nodes
            .iter()
            .any(|node| node.id == addition.source && node.review.is_some())
        {
            append(&mut expanded, addition);
            continue;
        }
        let expected = derive_topology(document, &expanded, addition)?;
        for (node, (members, edges)) in addition.nodes.iter().zip(expected) {
            for (stored, expected, invariant) in [
                (
                    &node.composite_members,
                    members,
                    "persisted composite_members must match the approved topology",
                ),
                (
                    &node.after_expansion,
                    edges,
                    "persisted after_expansion must match the approved topology",
                ),
            ] {
                if *stored != expected {
                    return Err((node.id.clone(), invariant));
                }
            }
        }
        append(&mut expanded, addition);
    }
    Ok(())
}

type NodeTopology = (Vec<String>, Vec<String>);

fn derive_topology(
    document: &GraphDocument,
    expanded: &GraphDocument,
    addition: &Expansion,
) -> Result<Vec<NodeTopology>, (String, &'static str)> {
    let mut expected = vec![(Vec::new(), Vec::new()); addition.nodes.len()];
    let fallback = addition
        .nodes
        .iter()
        .any(|node| node.id == format!("{}--fallback", addition.source));
    if fallback
        || !addition.patterns.is_empty()
        || addition.nodes.iter().any(|node| node.composite.is_some())
    {
        let planner = expanded
            .nodes
            .iter()
            .find(|node| node.id == addition.source)
            .filter(|node| node.plan.is_some())
            .ok_or_else(|| {
                (
                    addition.source.clone(),
                    "composite topology requires a planner specification",
                )
            })?;
        let spec = planner.plan.as_ref().ok_or_else(|| {
            (
                addition.source.clone(),
                "composite topology requires a planner specification",
            )
        })?;
        if fallback {
            let derived = fallback_expansion(
                &addition.source,
                spec,
                document,
                planner.require_skill_location,
            )
            .map_err(|_| {
                (
                    addition.source.clone(),
                    "fallback topology must be derivable from its specification",
                )
            })?;
            if !matches!(
                (serde_json::to_value(&addition.nodes), serde_json::to_value(&derived.nodes)),
                (Ok(stored), Ok(expected)) if stored == expected
            ) {
                return Err((
                    addition.source.clone(),
                    "fallback nodes must match the submitted fallback specification",
                ));
            }
            expected = derived
                .nodes
                .into_iter()
                .map(|node| (node.composite_members, node.after_expansion))
                .collect();
        } else {
            for instance in &addition.patterns {
                let pattern = spec
                    .patterns
                    .iter()
                    .find(|pattern| pattern.name == instance.name)
                    .ok_or_else(|| {
                        (
                            instance.id.clone(),
                            "pattern must name an approved catalogue entry",
                        )
                    })?;
                let members: Vec<_> = pattern
                    .nodes
                    .iter()
                    .map(|node| {
                        node.get("id")
                            .and_then(serde_json::Value::as_str)
                            .map(|id| format!("{}--{id}", instance.id))
                            .ok_or_else(|| {
                                (
                                    instance.id.clone(),
                                    "catalogue member must have a node identity",
                                )
                            })
                    })
                    .collect::<Result<_, _>>()?;
                let mut sorted = members.clone();
                sorted.sort();
                if sorted != instance.nodes
                    || members
                        .iter()
                        .any(|id| !addition.nodes.iter().any(|node| &node.id == id))
                {
                    return Err((
                        instance.id.clone(),
                        "pattern members must match the approved catalogue",
                    ));
                }
                let index = addition
                    .nodes
                    .iter()
                    .position(|node| node.id == instance.id)
                    .ok_or_else(|| {
                        (
                            instance.id.clone(),
                            "pattern must retain its composite boundary",
                        )
                    })?;
                expected[index] = (members.clone(), members);
            }
            if let Some(node) = addition.nodes.iter().find(|node| {
                node.composite.is_some()
                    && !addition
                        .patterns
                        .iter()
                        .any(|pattern| pattern.id == node.id)
            }) {
                return Err((
                    node.id.clone(),
                    "composite topology must derive from an approved pattern or fallback",
                ));
            }
        }
    }
    Ok(expected)
}

pub(super) fn append(document: &mut GraphDocument, expansion: &Expansion) {
    let round = document
        .nodes
        .iter()
        .find(|n| n.id == expansion.source)
        .and_then(|n| n.round.clone());
    // Retain the original dependency for selected upstream outputs, and wait
    // for the entire fragment before admitting any of its consumers.
    // A consumer that read the planner's result also reads what the fragment's
    // pattern instances export, as it would read the static composite.
    let exported = fragment_exports(document, expansion);
    for node in &mut document.nodes {
        if node.dependencies().any(|name| name == &expansion.source) {
            if expansion.review.is_some() {
                // The review boundary settles after its members and owns their
                // success threshold; consumers must not require every leaf.
                if let Some(root) = exported.first() {
                    node.after_expansion.push(root.node.clone());
                }
            } else {
                node.after_expansion
                    .extend(expansion.nodes.iter().map(|n| n.id.clone()));
            }
            if !expansion.continuation {
                wire_exports(node, &expansion.source, &exported);
                if expansion.review.is_some() {
                    // A resolved review is a structural node, not an agent
                    // attempt, and has no result of its own to consume.
                    node.inputs.retain(|input| input.node != expansion.source);
                }
                if expansion.review.is_some()
                    && let Some(root) = exported.first()
                    && let Some(composite) = &mut node.composite
                {
                    for export in &mut composite.exports {
                        if export.node == expansion.source {
                            export.node = root.node.clone();
                        }
                    }
                    for member in &mut node.composite_members {
                        if *member == expansion.source {
                            *member = root.node.clone();
                        }
                    }
                }
            }
            if expansion.continuation {
                if let Some(composite) = &mut node.composite {
                    for export in &mut composite.exports {
                        if export.node == expansion.source {
                            export.node = expansion.nodes[0].id.clone();
                        }
                    }
                }
                for input in &mut node.inputs {
                    if input.node == expansion.source {
                        input.node = expansion.nodes[0].id.clone();
                        if !node.depends_on.contains(&input.node) {
                            node.depends_on.push(input.node.clone());
                        }
                    }
                }
            }
        }
    }
    document
        .nodes
        .extend(expansion.nodes.iter().cloned().map(|mut child| {
            if expansion.review.is_some()
                && child.composite.is_some()
                && child.composite_members.is_empty()
            {
                let prefix = format!("{}--", child.id);
                child.composite_members = expansion
                    .nodes
                    .iter()
                    .filter_map(|member| {
                        member
                            .id
                            .strip_prefix(&prefix)
                            .filter(|_| {
                                !expansion.nodes.iter().any(|parent| {
                                    parent.composite.is_some()
                                        && parent.id != child.id
                                        && parent.id != member.id
                                        && parent.id.starts_with(&prefix)
                                        && member.id.starts_with(&format!("{}--", parent.id))
                                })
                            })
                            .map(|_| member.id.clone())
                    })
                    .collect();
                child
                    .after_expansion
                    .extend(child.composite_members.clone());
            }
            child.round = round.clone().map(|mut round| {
                round.until = false;
                round.node_id = child.id.clone();
                round
            });
            child
        }));
}

/// Check the host's review fragment before publication and again on replay.
pub(super) fn validate_review_addition(
    document: &GraphDocument,
    addition: &Expansion,
) -> Result<(), String> {
    let source = document
        .nodes
        .iter()
        .find(|node| node.id == addition.source)
        .ok_or("invalid expansion source")?;
    let Some(spec) = &source.review else {
        return if addition.review.is_none() {
            Ok(())
        } else {
            Err("review expansion has no review source".into())
        };
    };
    let root = addition
        .nodes
        .iter()
        .find(|root| {
            root.composite.is_some()
                && root.id.starts_with(&format!("{}--", source.id))
                && addition.nodes.iter().all(|node| {
                    node.id == root.id || node.id.starts_with(&format!("{}--", root.id))
                })
        })
        .ok_or("review composite root missing")?;
    let subject = addition
        .review
        .as_ref()
        .map(|review| review.subject.to_string());
    if addition.review.is_none()
        || addition.nodes.iter().filter(|node| node.composite.is_none()).any(|node| !matches!(&node.source, Some(SourceBinding::Commit { commit }) if Some(commit) == subject.as_ref()))
        || addition.continuation
        || !addition.patterns.is_empty()
        || addition.nodes.len() < 2
        || addition.nodes.len() > spec.max_nodes
        || root.composite.as_ref().is_none_or(|composite| {
            let mut exported = std::collections::BTreeSet::new();
            composite.min_successful != spec.min_successful.map(|n| if n == 0 { composite.exports.len().saturating_sub(1).max(1) } else { n })
                || composite.exports.is_empty()
                || composite.exports.iter().any(|export| {
                    export.outputs.is_empty() || !exported.insert(&export.node)
                        || !addition.nodes.iter().any(|member| member.id == export.node && member.id != root.id)
                        || addition.nodes.iter().any(|parent| parent.id != root.id && parent.id != export.node && parent.composite.is_some() && export.node.starts_with(&format!("{}--", parent.id)))
                })
        })
        || addition
            .nodes
            .iter()
            .any(|node| node.access != AccessMode::ReadOnly || node.review.is_some())
    {
        return Err("invalid review expansion".into());
    }
    Ok(())
}

/// The flat nodes and outputs that a fragment's pattern instances export, in
/// instantiation order. Instance IDs are already namespaced by the planner.
fn fragment_exports(document: &GraphDocument, expansion: &Expansion) -> Vec<GraphInput> {
    if expansion.review.is_some()
        && let Some(root) = expansion.nodes.iter().find(|root| {
            root.composite.is_some()
                && expansion.nodes.iter().all(|node| {
                    node.id == root.id || node.id.starts_with(&format!("{}--", root.id))
                })
        })
    {
        let mut outputs = Vec::new();
        for output in root
            .composite
            .iter()
            .flat_map(|spec| &spec.exports)
            .flat_map(|export| &export.outputs)
        {
            if !outputs.contains(output) {
                outputs.push(*output);
            }
        }
        return vec![GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: root.id.clone(),
            outputs,
        }];
    }
    let Some(spec) = document
        .nodes
        .iter()
        .find(|n| n.id == expansion.source)
        .and_then(|n| n.plan.as_ref())
        .filter(|spec| spec.wire_exports)
    else {
        return Vec::new();
    };
    if let Some(fallback) = &spec.fallback
        && expansion
            .nodes
            .iter()
            .any(|node| node.id == format!("{}--fallback", expansion.source))
    {
        let mut outputs = Vec::new();
        for output in fallback
            .exports
            .iter()
            .flat_map(|export| export.outputs.iter().copied())
        {
            if !outputs.contains(&output) {
                outputs.push(output);
            }
        }
        return vec![GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: format!("{}--fallback", expansion.source),
            outputs,
        }];
    }
    expansion
        .patterns
        .iter()
        .filter_map(|instance| {
            let pattern = spec.patterns.iter().find(|p| p.name == instance.name)?;
            Some(pattern.exports.iter().map(move |export| GraphInput {
                required_outputs: Vec::new(),
                extra: Default::default(),
                node: format!("{}--{}", instance.id, export.node),
                outputs: export.outputs.clone(),
            }))
        })
        .flatten()
        .collect()
}

/// Add the exported nodes a consumer asked of the planner, keeping only the
/// outputs it selected. The planner's own input stays, for its rationale.
fn wire_exports(node: &mut GraphNode, source: &str, exported: &[GraphInput]) {
    let wanted: Vec<_> = node
        .inputs
        .iter()
        .filter(|input| input.node == source)
        .flat_map(|input| input.outputs.iter().copied())
        .collect();
    for export in exported {
        let outputs: Vec<_> = export
            .outputs
            .iter()
            .copied()
            .filter(|output| wanted.contains(output))
            .collect();
        if outputs.is_empty() || node.inputs.iter().any(|input| input.node == export.node) {
            continue;
        }
        node.inputs.push(GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: export.node.clone(),
            outputs,
        });
        if !node.depends_on.contains(&export.node) {
            node.depends_on.push(export.node.clone());
        }
    }
}

fn valid_scope(scope: &str) -> bool {
    scope == "."
        || (!scope.is_empty()
            && !scope
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            && !scope.contains(['\\', '*', '?', '[', ']']))
}

fn overlaps(left: &[String], right: &[String]) -> bool {
    left.is_empty()
        || right.is_empty()
        || left.iter().chain(right).any(|scope| !valid_scope(scope))
        || left.iter().any(|a| {
            right.iter().any(|b| {
                a == "."
                    || b == "."
                    || a == b
                    || a.starts_with(&format!("{b}/"))
                    || b.starts_with(&format!("{a}/"))
            })
        })
}

pub(super) fn writers_conflict(left: &PlannedNode, right: &PlannedNode) -> bool {
    left.access == AccessMode::WorktreeWrite
        && right.access == AccessMode::WorktreeWrite
        && (left.worktree == right.worktree || overlaps(&left.write_scope, &right.write_scope))
}

fn prepare(
    child: &mut GraphNode,
    node: &PlannedNode,
    spec: &PlanSpec,
    document: &GraphDocument,
    trusted: bool,
) -> Result<(), String> {
    child.require_skill_location |= node.require_skill_location;
    if !trusted {
        // A planner may narrow but never widen the access its own node holds.
        if let Some(role) = &child.role
            && node.role.as_ref() != Some(role)
        {
            return Err(format!(
                "fragment node {} sets role {role}; planner-emitted nodes cannot choose a role",
                child.id
            ));
        }
        if let Some(source) = &child.source
            && node.source.as_ref() != Some(source)
        {
            return Err(format!(
                "fragment node {} sets source; planner-emitted nodes may only repeat the planner's own source",
                child.id
            ));
        }
        if let Some(root) = child
            .read_roots
            .iter()
            .find(|r| !node.read_roots.contains(r))
        {
            return Err(format!(
                "fragment node {} read root {root} is outside the planner's read roots",
                child.id
            ));
        }
    }
    if !spec.kinds.contains(&child.kind.unwrap_or(NodeKind::Task)) {
        return Err(format!(
            "fragment kind {:?} was not declared",
            child.kind.unwrap_or(NodeKind::Task)
        ));
    }
    // Gate evidence lives in its condition and a decision in its question.
    // Writer nodes carry explicit scope; all agents carry acceptance prose.
    if !matches!(
        child.kind,
        Some(NodeKind::Gate | NodeKind::Decide | NodeKind::Composite)
    ) {
        if child.prompt.trim().is_empty()
            || child.acceptance_checks.is_empty()
            || child.acceptance_checks.iter().any(|s| s.trim().is_empty())
            || child.access == AccessMode::WorktreeWrite && child.write_scope.is_empty()
            || child.write_scope.iter().any(|s| !valid_scope(s))
        {
            return Err("fragment agent requires goal or brief and acceptance_checks; writers also require relative literal write_scope".into());
        }
        child.prompt.push_str("\n\n");
        if !child.write_scope.is_empty() {
            child
                .prompt
                .push_str(&format!("Write scope: {}\n", child.write_scope.join(", ")));
        }
        child.prompt.push_str(&format!(
            "Acceptance checks:\n{}",
            child.acceptance_checks.join("\n")
        ));
        if child.route.is_none() && child.routes.is_none() {
            child.route = Some(GraphRoute {
                extra: Default::default(),
                alias: Some("senior".into()),
                adapter: None,
                name: None,
                model: None,
                effort: None,
                account: None,
            });
        }
    }
    // The destination is chair authority: a planner never names one.
    if matches!(
        child.delivery,
        Some(provenant_records::run::DeliveryMode::Integrate(_))
    ) && document.integration_branch.is_none()
    {
        return Err(format!(
            "fragment node {} sets integrate delivery, but the graph names no integration_branch",
            child.id
        ));
    }
    if child.access == AccessMode::WorktreeWrite {
        if child.delivery.as_ref().is_some_and(|mode| {
            !matches!(
                mode,
                provenant_records::run::DeliveryMode::None
                    | provenant_records::run::DeliveryMode::Integrate(_)
            )
        }) {
            return Err("fragment writers use integrate delivery, never individual PRs".into());
        }
        if let Some(target) = &document.integration_branch {
            child.delivery = Some(provenant_records::run::DeliveryMode::Integrate(
                target.clone(),
            ));
        }
    }
    Ok(())
}

fn fragment(
    node: &PlannedNode,
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    json: &str,
) -> Result<Expansion, String> {
    let spec = node.plan.as_ref().ok_or("missing plan configuration")?;
    let mut value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid fragment: {e}"))?;
    normalise_input_shorthand(&mut value);
    if let Some(instances) = value
        .get_mut("patterns")
        .and_then(serde_json::Value::as_array_mut)
    {
        for instance in instances {
            let name = instance
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<unknown>");
            serde_json::from_value::<pattern_plan::PatternInstance>(instance.clone())
                .map_err(|e| format!("pattern {name}: {e}"))?;
        }
    }
    let mut fragment: Fragment =
        serde_json::from_value(value).map_err(|e| format!("invalid fragment: {e}"))?;
    if spec.patterns_only && !fragment.nodes.is_empty() {
        return Err("this plan accepts pattern instances only; raw nodes are refused".into());
    }
    if fragment.schema_version != 1
        || (fragment.nodes.is_empty() && fragment.patterns.is_empty())
        || fragment.nodes.len() > spec.max_nodes
    {
        return Err(format!(
            "fragment requires 1..{} nodes and schema_version 1",
            spec.max_nodes
        ));
    }
    for child in &mut fragment.nodes {
        prepare(child, node, spec, document, false)?;
    }
    let mut instantiations = Vec::new();
    let mut wrappers = Vec::new();
    for instance in &fragment.patterns {
        let pattern = spec
            .patterns
            .iter()
            .find(|pattern| pattern.name == instance.pattern)
            .ok_or_else(|| {
                format!(
                    "pattern {} is not in this plan's allow-list: {}",
                    instance.pattern,
                    spec.patterns
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        let (mut wrapper, instantiation) = pattern_plan::instantiate(pattern, instance)?;
        if let Some(composite) = &mut wrapper.composite {
            for child in &mut composite.nodes {
                prepare(child, node, spec, document, true)?;
            }
        }
        wrappers.push(wrapper);
        instantiations.push(instantiation);
    }
    let fragment_nodes = fragment.nodes.len() + wrappers.len();
    if fragment_nodes > spec.max_nodes {
        return Err(format!(
            "fragment requires 1..{} nodes and schema_version 1",
            spec.max_nodes
        ));
    }
    let mut local_nodes = fragment.nodes;
    local_nodes.extend(wrappers);
    let units: BTreeMap<String, Vec<String>> = local_nodes
        .iter()
        .map(|unit| {
            let after = unit
                .depends_on
                .iter()
                .chain(unit.inputs.iter().map(|i| &i.node));
            (unit.id.clone(), after.cloned().collect())
        })
        .collect();
    let local = GraphDocument {
        review_base: None,
        context: document.context.clone(),
        extra: Default::default(),
        schema_version: document.schema_version,
        graph_id: "fragment".into(),
        nodes: local_nodes,
        limits: None,
        delivery: None,
        decision_session: document.decision_session.clone(),
        integration_branch: document.integration_branch.clone(),
    };
    // Pattern wrappers are ordinary static composites. Expand them before the
    // fragment is validated so the host limits see every child.
    let local = composites::expand(&local, &generous())?;
    validate_schema(&local).map_err(str::to_owned)?;
    let (plan, _) = validate_flat_plan(&local, &generous()).map_err(str::to_owned)?;
    validate_output_kinds(&local)?;
    let mut nodes: Vec<GraphNode> = plan
        .nodes
        .iter()
        .filter_map(|n| local.nodes.iter().find(|child| child.id == n.id).cloned())
        .collect();
    if spec.disjoint_waves {
        check_waves(&units, &nodes)?;
    }
    // Union overlapping writer scopes, including transitive overlaps. Every
    // component uses one checkout; admission fences serialise its writers.
    let mut components: Vec<usize> = (0..nodes.len()).collect();
    for i in 0..nodes.len() {
        for j in 0..i {
            if nodes[i].access == AccessMode::WorktreeWrite
                && nodes[j].access == AccessMode::WorktreeWrite
                && (nodes[i].worktree == nodes[j].worktree
                    || overlaps(&nodes[i].write_scope, &nodes[j].write_scope))
            {
                let old = components[i].max(components[j]);
                let new = components[i].min(components[j]);
                for value in &mut components {
                    if *value == old {
                        *value = new;
                    }
                }
            }
        }
    }
    for i in 0..nodes.len() {
        if components[i] != i {
            let first = nodes[components[i]].clone();
            if document.integration_branch.is_none() {
                return Err("overlapping writers require an integration_branch".into());
            }
            nodes[i].worktree = first.worktree;
        }
    }
    let id = |name: &str| format!("{}--{name}", node.id);
    for child in &mut nodes {
        if id(&child.id).len() > 64 {
            return Err(format!(
                "child {} of planner {} exceeds the namespaced node ID limit of 64 bytes; shorten either ID",
                child.id, node.id
            ));
        }
        child.id = id(&child.id);
        child.depends_on = child.depends_on.iter().map(|name| id(name)).collect();
        if child.depends_on.is_empty() {
            child.depends_on.push(node.id.clone());
        }
        child.after_expansion = child.after_expansion.iter().map(|name| id(name)).collect();
        child.composite_members = child
            .composite_members
            .iter()
            .map(|name| id(name))
            .collect();
        if let Some(composite) = &mut child.composite {
            for export in &mut composite.exports {
                export.node = id(&export.node);
            }
        }
        for input in &mut child.inputs {
            input.node = id(&input.node);
        }
        if let Some(SourceBinding::Node { node: source }) = &mut child.source {
            *source = id(source);
        }
        child.on_fail = child.on_fail.as_deref().map(id);
        child.round = None;
    }
    for instantiation in &mut instantiations {
        let instance = id(&instantiation.id);
        instantiation.nodes = nodes
            .iter()
            .filter(|child| child.id.starts_with(&format!("{instance}--")))
            .map(|child| child.id.clone())
            .collect();
        instantiation.nodes.sort();
        instantiation.id = instance;
    }
    coordinate(&mut nodes, document, checkpoint)?;
    Ok(Expansion {
        review: None,
        extra: Default::default(),
        source: node.id.clone(),
        continuation: false,
        nodes,
        patterns: instantiations,
    })
}

/// Planner fragments commonly abbreviate a result input as its local node ID.
/// Normalise only this untrusted fragment JSON; persisted graphs remain typed.
fn normalise_input_shorthand(value: &mut serde_json::Value) {
    if let Some(fields) = value.as_object_mut() {
        for key in ["nodes", "patterns"] {
            if let Some(items) = fields
                .get_mut(key)
                .and_then(serde_json::Value::as_array_mut)
            {
                for item in items {
                    if let Some(inputs) = item
                        .get_mut("inputs")
                        .and_then(serde_json::Value::as_array_mut)
                    {
                        for input in inputs {
                            if let Some(node) = input.as_str() {
                                *input = serde_json::json!({
                                    "node": node,
                                    "outputs": ["result"]
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Refuse a wave whose units write overlapping scopes. A unit is a fragment
/// node or pattern instance; two units are in one wave when neither depends,
/// directly or through other units, on the other.
fn check_waves(units: &BTreeMap<String, Vec<String>>, nodes: &[GraphNode]) -> Result<(), String> {
    fn reaches(units: &BTreeMap<String, Vec<String>>, from: &str, to: &str) -> bool {
        let mut seen = std::collections::BTreeSet::new();
        let mut stack = vec![from];
        while let Some(current) = stack.pop() {
            if current == to {
                return true;
            }
            if seen.insert(current) {
                stack.extend(units.get(current).into_iter().flatten().map(String::as_str));
            }
        }
        false
    }
    let writers = |unit: &str| -> Vec<&GraphNode> {
        nodes
            .iter()
            .filter(|n| n.id == unit || n.id.starts_with(&format!("{unit}--")))
            .filter(|n| n.access == AccessMode::WorktreeWrite)
            .collect()
    };
    let names: Vec<_> = units.keys().collect();
    for (position, left) in names.iter().enumerate() {
        for right in &names[position + 1..] {
            let (a, b) = (writers(left), writers(right));
            if a.is_empty()
                || b.is_empty()
                || reaches(units, left, right)
                || reaches(units, right, left)
            {
                continue;
            }
            if a.iter()
                .any(|x| b.iter().any(|y| overlaps(&x.write_scope, &y.write_scope)))
            {
                return Err(format!(
                    "{left} and {right} write overlapping scopes in one wave; order one after the other with depends_on or split their write scopes"
                ));
            }
        }
    }
    Ok(())
}

fn coordinate(
    nodes: &mut [GraphNode],
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
) -> Result<(), String> {
    let paths: std::collections::BTreeSet<_> = nodes
        .iter()
        .filter(|n| n.access == AccessMode::WorktreeWrite)
        .filter_map(|n| n.worktree.clone())
        .collect();
    for path in paths {
        let members: Vec<_> = nodes
            .iter()
            .filter(|n| n.worktree.as_ref() == Some(&path))
            .collect();
        let existing: Vec<_> = document
            .nodes
            .iter()
            .filter(|n| {
                n.access == AccessMode::WorktreeWrite
                    && !checkpoint.nodes.iter().any(|progress| {
                        progress.id == n.id
                            && matches!(progress.state, NodeState::Skipped | NodeState::Blocked)
                    })
                    && members.iter().any(|child| {
                        n.worktree == child.worktree || overlaps(&n.write_scope, &child.write_scope)
                    })
            })
            .collect();
        let checkouts: std::collections::BTreeSet<_> = existing
            .iter()
            .filter_map(|n| n.worktree.as_ref())
            .collect();
        if checkouts.len() > 1 {
            return Err("expansion connects existing writers on different branches; use one shared worktree".into());
        }
        if let Some(checkout) = checkouts.first() {
            if document.integration_branch.is_none() {
                return Err("overlapping writers require an integration_branch".into());
            }
            for child in nodes
                .iter_mut()
                .filter(|n| n.worktree.as_ref() == Some(&path))
            {
                child.worktree = Some((*checkout).clone());
            }
        }
    }
    Ok(())
}

pub(super) fn generous() -> crate::config::GraphLimits {
    crate::config::GraphLimits {
        nodes: usize::MAX,
        edges: usize::MAX,
        routes_per_node: u16::MAX as usize,
        node_prompt_bytes: usize::MAX,
        graph_prompt_bytes: usize::MAX,
        ..Default::default()
    }
}

fn continuation(
    node: &PlannedNode,
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    note: &str,
    limits: &crate::config::GraphLimits,
) -> Result<Expansion, String> {
    if checkpoint
        .expansions
        .iter()
        .filter(|e| e.continuation)
        .count()
        >= limits.continuations
    {
        return Err(format!(
            "limits.continuations={} reached; extend the continuation budget",
            limits.continuations
        ));
    }
    let mut child = document
        .nodes
        .iter()
        .find(|n| n.id == node.id)
        .cloned()
        .ok_or("continuation source missing")?;
    child.id = format!("{}--continue", node.id);
    if child.id.len() > 64 {
        return Err(format!(
            "continuation {} exceeds the node ID limit of 64 bytes; shorten the source ID",
            child.id
        ));
    }
    child.title = None;
    child.depends_on = vec![node.id.clone()];
    child.after_expansion.clear();
    child.inputs.clear();
    if child.access == AccessMode::WorktreeWrite && child.source.is_some() {
        child.source = Some(SourceBinding::Node {
            node: node.id.clone(),
        });
    }
    child.prompt.push_str(&format!(
        "\n\nContinue this work in the same worktree. Stopped-early handoff:\n{note}"
    ));
    // Integrate retains the shared checkout and leaves the graph owning its PR.
    if child.delivery == Some(provenant_records::run::DeliveryMode::Pr) {
        return Err("honest-stop continuation requires integrate or no delivery".into());
    }
    if let Some(target) = &document.integration_branch {
        child.delivery = Some(provenant_records::run::DeliveryMode::Integrate(
            target.clone(),
        ));
    }
    child.round = None;
    Ok(Expansion {
        review: None,
        extra: Default::default(),
        source: node.id.clone(),
        continuation: true,
        nodes: vec![child],
        patterns: Vec::new(),
    })
}

pub(super) fn remaining(
    checkpoint: &mut GraphCheckpoint,
    document: &GraphDocument,
    limits: &crate::config::GraphLimits,
) {
    // Preserve the historical wire shape for graphs that do not use expansion.
    if document
        .nodes
        .iter()
        .any(|n| n.plan.is_some() || n.review.is_some() || n.access == AccessMode::WorktreeWrite)
    {
        checkpoint.remaining = Some(RemainingBudgets {
            extra: Default::default(),
            nodes: limits.nodes.saturating_sub(
                document
                    .nodes
                    .iter()
                    .filter_map(|node| {
                        node.review.as_ref().filter(|_| {
                            !checkpoint
                                .expansions
                                .iter()
                                .any(|expansion| expansion.source == node.id)
                        })
                    })
                    .fold(document.nodes.len(), |count, review| {
                        count.saturating_add(review.max_nodes)
                    }),
            ),
            edges: limits.edges.saturating_sub(
                document
                    .nodes
                    .iter()
                    .map(|n| n.dependencies().count())
                    .sum(),
            ),
            continuations: limits.continuations.saturating_sub(
                checkpoint
                    .expansions
                    .iter()
                    .filter(|e| e.continuation)
                    .count(),
            ),
        });
    }
}

/// Validate the entire prospective graph before publishing any added node.
pub(super) fn settle(
    node: &PlannedNode,
    source: &GraphDocument,
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    capture: Option<&crate::result_capture::CapturedResult>,
    limits: &crate::config::GraphLimits,
) -> Option<(
    GraphDocument,
    PlannedGraph,
    std::collections::BTreeMap<String, usize>,
)> {
    if checkpoint.expansions.iter().any(|e| e.source == node.id) {
        return None;
    }
    if node
        .plan
        .as_ref()
        .is_some_and(|spec| spec.fallback.is_some())
        && checkpoint
            .nodes
            .iter()
            .any(|progress| progress.id == node.id && progress.state == NodeState::Failed)
    {
        let spec = node.plan.as_ref()?;
        let index = checkpoint
            .nodes
            .iter()
            .position(|progress| progress.id == node.id)?;
        let reason = fallback_reason(&checkpoint.nodes[index]);
        let built = (|| {
            let addition = fallback_expansion(&node.id, spec, source, node.require_skill_location)?;
            let mut preview = document.clone();
            append(&mut preview, &addition);
            let (plan, positions) =
                validate_flat_plan(&preview, &generous()).map_err(str::to_owned)?;
            if let Some(reason) = structural_limit_reason(&preview, &plan, limits) {
                return Err(reason);
            }
            let mut expansions = checkpoint.expansions.clone();
            expansions.push(addition.clone());
            if let Some(reason) =
                graph_envelope_reason(source, &expansions, &checkpoint.extensions, &plan)
            {
                return Err(reason);
            }
            Ok((addition, plan, positions, preview))
        })();
        let (addition, plan, positions, preview) = match built {
            Ok(built) => built,
            Err(reason) => {
                // Same diagnostic as a rejected planner expansion: pause with a reason.
                let reason = bounded(
                    &format!("node {} fallback expansion paused: {reason}", node.id),
                    MAX_ADVANCE_ERROR_BYTES,
                );
                checkpoint.nodes[index].expansion_pause = Some(reason.clone());
                checkpoint.pause_reason = Some(reason);
                checkpoint.pause_code = Some("invalid_plan_expansion".into());
                return None;
            }
        };
        checkpoint.nodes[index].state = NodeState::Succeeded;
        checkpoint.nodes[index].expansion_pause = None;
        let outputs = checkpoint.nodes[index]
            .outputs
            .get_or_insert_with(|| super::outputs::make_outputs(None, None, None, None, None));
        outputs.plan = Some(PlanOutput {
            extra: Default::default(),
            fallback_reason: reason,
        });
        checkpoint.expansions.push(addition);
        loops::reconcile(checkpoint, &plan);
        remaining(checkpoint, &preview, limits);
        return Some((preview, plan, positions));
    }
    let index = checkpoint
        .nodes
        .iter()
        .position(|n| n.id == node.id && n.state == NodeState::Succeeded)?;
    let addition = if node.plan.is_some() {
        let json = if let Some(capture) = capture {
            block(capture, "fragment")
                .and_then(|json| json.ok_or_else(|| "fragment block missing".into()))
                .map(|json| {
                    checkpoint.nodes[index].fragment = Some(json.to_owned());
                    json.to_owned()
                })
        } else {
            checkpoint.nodes[index]
                .fragment
                .clone()
                .ok_or_else(|| "planner result missing".into())
        };
        json.and_then(|json| fragment(node, document, checkpoint, &json))
    } else if writer(node) {
        let handoff = capture
            .map(handoff)
            .unwrap_or_else(|| Ok(checkpoint.nodes[index].handoff.clone()));
        match handoff {
            Ok(Some(note)) => {
                checkpoint.nodes[index].handoff = Some(note.clone());
                continuation(node, document, checkpoint, &note, limits)
            }
            Ok(None) => return None,
            Err(reason) => {
                if let Some(outputs) = &mut checkpoint.nodes[index].outputs {
                    outputs.warnings.push(bounded(
                        &format!("ignored optional handoff: {reason}"),
                        WARNING_BYTES,
                    ));
                }
                return None;
            }
        }
    } else {
        return None;
    };
    let result = addition.and_then(|addition| {
        let mut preview = document.clone();
        append(&mut preview, &addition);
        let mut loop_writers = std::collections::BTreeMap::new();
        for writer in preview
            .nodes
            .iter()
            .filter(|n| n.access == AccessMode::WorktreeWrite)
        {
            if let Some(round) = &writer.round
                && let Some(previous) = loop_writers.insert(&round.loop_id, &writer.worktree)
                && previous != &writer.worktree
            {
                return Err("loop writers must share one worktree".into());
            }
        }
        validate_schema(&preview).map_err(str::to_owned)?;
        let (plan, positions) = validate_flat_plan(&preview, &generous()).map_err(str::to_owned)?;
        validate_output_kinds(&preview)?;
        if let Some(reason) = structural_limit_reason(&preview, &plan, limits) {
            return Err(reason);
        }
        let mut expansions = checkpoint.expansions.clone();
        expansions.push(addition.clone());
        if let Some(reason) =
            graph_envelope_reason(source, &expansions, &checkpoint.extensions, &plan)
        {
            return Err(reason);
        }
        Ok((addition, plan, positions, preview))
    });
    match result {
        Ok((addition, plan, positions, preview)) => {
            checkpoint.nodes[index].fragment = None;
            checkpoint.nodes[index].expansion_pause = None;
            if checkpoint
                .pause_reason
                .as_ref()
                .is_some_and(|r| r.starts_with("node ") && r.contains(" expansion paused:"))
            {
                checkpoint.pause_reason = checkpoint
                    .nodes
                    .iter()
                    .find_map(|n| n.expansion_pause.clone());
                checkpoint.pause_code = checkpoint
                    .pause_reason
                    .as_ref()
                    .map(|_| "invalid_plan_expansion".into());
            }
            checkpoint.expansions.push(addition);
            loops::reconcile(checkpoint, &plan);
            remaining(checkpoint, &preview, limits);
            Some((preview, plan, positions))
        }
        Err(reason) => {
            if let Some(spec) = node.plan.as_ref().filter(|spec| spec.fallback.is_some()) {
                let fragment_error = checkpoint.nodes[index]
                    .expansion_pause
                    .as_deref()
                    .and_then(retry_fragment_error)
                    .unwrap_or_else(|| bounded(&reason, NOTE_BYTES));
                let fallback_reason = bounded(
                    &format!("planner fragment invalid: {fragment_error}"),
                    NOTE_BYTES,
                );
                let fallback = (|| {
                    let addition =
                        fallback_expansion(&node.id, spec, source, node.require_skill_location)?;
                    let mut preview = document.clone();
                    append(&mut preview, &addition);
                    validate_schema(&preview).map_err(str::to_owned)?;
                    let (plan, positions) =
                        validate_flat_plan(&preview, &generous()).map_err(str::to_owned)?;
                    validate_output_kinds(&preview)?;
                    if let Some(reason) = structural_limit_reason(&preview, &plan, limits) {
                        return Err(reason);
                    }
                    let mut expansions = checkpoint.expansions.clone();
                    expansions.push(addition.clone());
                    if let Some(reason) =
                        graph_envelope_reason(source, &expansions, &checkpoint.extensions, &plan)
                    {
                        return Err(reason);
                    }
                    Ok((addition, plan, positions, preview))
                })();
                match fallback {
                    Ok((addition, plan, positions, preview)) => {
                        checkpoint.nodes[index].state = NodeState::Succeeded;
                        checkpoint.nodes[index].expansion_pause = None;
                        checkpoint.nodes[index].fragment = None;
                        if checkpoint.pause_reason.as_ref().is_some_and(|reason| {
                            reason.starts_with("node ") && reason.contains(" expansion paused:")
                        }) {
                            checkpoint.pause_reason = checkpoint.nodes.iter().find_map(|node| {
                                node.expansion_pause
                                    .as_deref()
                                    .map(pause_reason_for_display)
                            });
                            checkpoint.pause_code = checkpoint
                                .pause_reason
                                .as_ref()
                                .map(|_| "invalid_plan_expansion".into());
                        }
                        let outputs = checkpoint.nodes[index].outputs.get_or_insert_with(|| {
                            super::outputs::make_outputs(None, None, None, None, None)
                        });
                        outputs.plan = Some(PlanOutput {
                            extra: Default::default(),
                            fallback_reason,
                        });
                        checkpoint.expansions.push(addition);
                        loops::reconcile(checkpoint, &plan);
                        remaining(checkpoint, &preview, limits);
                        return Some((preview, plan, positions));
                    }
                    Err(fallback_error) => {
                        let display_fragment_error = bounded(&fragment_error, 350);
                        let display_fallback_error = bounded(&fallback_error, 350);
                        let display_reason = format!(
                            "node {} expansion paused: planner fragment invalid: {display_fragment_error}; fallback expansion paused: {display_fallback_error}",
                            node.id
                        );
                        let retry_context =
                            provenant_core::hex(bounded(&fragment_error, 1500).as_bytes());
                        let persisted_reason = bounded(
                            &format!("{display_reason}{FALLBACK_RETRY_CONTEXT}{retry_context}"),
                            MAX_ADVANCE_ERROR_BYTES,
                        );
                        checkpoint.nodes[index].expansion_pause = Some(persisted_reason);
                        checkpoint.pause_reason = Some(display_reason);
                        checkpoint.pause_code = Some("invalid_plan_expansion".into());
                        return None;
                    }
                }
            }
            let reason = bounded(
                &format!("node {} expansion paused: {reason}", node.id),
                MAX_ADVANCE_ERROR_BYTES,
            );
            checkpoint.nodes[index].expansion_pause = Some(reason.clone());
            checkpoint.pause_reason = Some(reason);
            checkpoint.pause_code = Some("invalid_plan_expansion".into());
            None
        }
    }
}

/// The static stage a planner's `fallback` spec expands to in the original
/// document. Deterministic, so `settle` appends it and checkpoint validation
/// recomputes it to check what was persisted.
pub(super) fn fallback_expansion(
    node_id: &str,
    spec: &PlanSpec,
    source: &GraphDocument,
    require_skill_location: bool,
) -> Result<Expansion, String> {
    let fallback = spec
        .fallback
        .as_ref()
        .ok_or_else(|| "planner has no fallback stage".to_owned())?;
    let root_id = format!("{node_id}--fallback");
    let root: GraphNode = serde_json::from_value(serde_json::json!({
        "id": root_id,
        "kind": "composite",
        "access": "read_only",
        "depends_on": spec.fallback_depends_on,
        "inputs": spec.fallback_inputs,
        "source": spec.fallback_source,
        "read_roots": spec.fallback_read_roots,
        "title": spec.fallback_title,
        "composite": fallback,
    }))
    .map_err(|error| format!("invalid fallback stage: {error}"))?;
    let mut with_root = source.clone();
    with_root.nodes.push(root);
    let expanded = composites::expand(&with_root, &generous())
        .map_err(|error| format!("fallback stage does not expand: {error}"))?;
    Ok(Expansion {
        review: None,
        extra: Default::default(),
        source: node_id.to_owned(),
        continuation: false,
        nodes: expanded
            .nodes
            .into_iter()
            .filter(|child| child.id == root_id || child.id.starts_with(&format!("{root_id}--")))
            .map(|mut child| {
                if child.composite.is_none()
                    && !matches!(
                        child.kind,
                        Some(NodeKind::Gate | NodeKind::Decide | NodeKind::Land)
                    )
                {
                    child.require_skill_location |= require_skill_location;
                }
                child
            })
            .collect(),
        patterns: Vec::new(),
    })
}

fn fallback_reason(progress: &NodeProgress) -> String {
    let outcome = progress
        .attempts
        .last()
        .and_then(|attempt| attempt.outcome)
        .map_or_else(
            || "unknown failure".to_owned(),
            |outcome| format!("{outcome:?}"),
        );
    let detail = progress
        .advance_error
        .as_deref()
        .or_else(|| {
            progress
                .outputs
                .as_ref()
                .and_then(|outputs| outputs.result.as_ref())
                .map(|result| result.excerpt.as_str())
        })
        .filter(|detail| !detail.trim().is_empty());
    let reason = detail.map_or_else(
        || format!("planner attempt failed: {outcome}"),
        |detail| format!("planner attempt failed ({outcome}): {detail}"),
    );
    bounded(&reason, NOTE_BYTES)
}

fn retry_fragment_error(pause: &str) -> Option<String> {
    let (_, context) = pause.rsplit_once(FALLBACK_RETRY_CONTEXT)?;
    String::from_utf8(provenant_core::unhex(context)?).ok()
}

pub(super) fn pause_reason_for_display(reason: &str) -> String {
    reason
        .rsplit_once(FALLBACK_RETRY_CONTEXT)
        .map_or(reason, |(display, _)| display)
        .to_owned()
}
