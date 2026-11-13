//! Dynamic plans and honest-stop continuations share the scheduler and journal.
use super::patch::{PatchRecord, TemplateUse};
use super::template::Template;
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn set_expansion_pause(
    checkpoint: &mut GraphCheckpoint,
    index: usize,
    reason: String,
    fallback: bool,
    fragment_error: Option<PlanFailureFact>,
) {
    checkpoint.nodes[index].expansion_pause = Some(reason.clone());
    checkpoint.nodes[index].expansion_cause = Some(NodeDecisionCause::PlanExpansionPaused {
        active: false,
        fallback,
        fragment_error,
    });
    checkpoint.activate_expansion_pause(index);
    checkpoint.set_pause(PauseCause::InvalidPlanExpansion, &reason);
}

pub(super) fn clear_expansion_pause(checkpoint: &mut GraphCheckpoint, index: usize, refresh: bool) {
    let active_expansion = checkpoint.nodes.iter().any(|node| {
        matches!(
            node.expansion_cause,
            Some(NodeDecisionCause::PlanExpansionPaused { active: true, .. })
        )
    });
    let keep_active = matches!(
        checkpoint.nodes[index].expansion_cause,
        Some(NodeDecisionCause::PlanExpansionPaused { active: true, .. })
    );
    checkpoint.nodes[index].expansion_pause = None;
    if !keep_active {
        checkpoint.nodes[index].expansion_cause = None;
    }
    if refresh && active_expansion {
        let next = checkpoint
            .nodes
            .iter()
            .enumerate()
            .find_map(|(index, node)| {
                node.expansion_pause
                    .as_ref()
                    .map(|reason| (index, reason.clone()))
            });
        checkpoint.deactivate_expansion_pauses();
        checkpoint.pause = None;
        if let Some((_, reason)) = &next {
            checkpoint.set_pause(PauseCause::InvalidPlanExpansion, reason);
        }
        if let Some((next, _)) = next {
            checkpoint.activate_expansion_pause(next);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSpec {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    /// Retained for the wire shape; a planner may emit any node kind.
    #[serde(default)]
    pub kinds: Vec<NodeKind>,
    pub max_nodes: usize,
    /// Host-authored templates this planner's patch may `use` with the
    /// authority they declare. Any other node is held to the planner's own.
    #[serde(default, rename = "patterns", skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<Template>,
    /// Fragment units that no dependency orders form one wave, and the writers
    /// of one wave must hold disjoint scopes. Without it, overlapping writers
    /// share a checkout and run in turn.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disjoint_waves: bool,
    /// Consumers of the plan node also read what the fragment's template
    /// uses export. Off, a consumer keeps exactly the inputs it declared.
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

impl PlanSpec {
    fn template(&self, name: &str) -> Option<&Template> {
        self.templates
            .iter()
            .find(|template| template.name.as_deref() == Some(name))
    }
}

/// Only validated, normalised additions enter the checkpoint. The original
/// document remains the digest authority, just as it does for loop rounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expansion {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub source: String,
    #[serde(with = "expanded_nodes")]
    pub nodes: Vec<GraphNode>,
    /// Template uses whose boundaries this expansion retains.
    #[serde(default, rename = "patterns", skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<TemplateUse>,
    /// Present when the expansion was admitted as a patch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<PatchRecord>,
}

impl Expansion {
    /// Live children and edits leave their producer open for its final output.
    pub(super) fn closes_producer(&self) -> bool {
        self.patch.is_none()
            || self.extra.get("final_result") == Some(&serde_json::Value::Bool(true))
            || self.extra.get("final_fragment") == Some(&serde_json::Value::Bool(true))
    }
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

fn catalogue_summary(spec: &PlanSpec) -> String {
    spec.templates
        .iter()
        .map(|template| {
            let name = template.name.clone().unwrap_or_default();
            let parameters = template.parameter_summary();
            if parameters.is_empty() {
                name
            } else {
                format!("{name} ({parameters})")
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn prompt(node: &GraphNode) -> String {
    let mut prompt = node.prompt.clone();
    if node.require_findings {
        prompt.push_str(
            "\n\nInclude a fenced `findings` JSON block in your final response, using an empty array when there are no findings. Example:\n```findings\n{\"schema_version\":1,\"findings\":[{\"id\":\"F1\",\"severity\":\"high\",\"file\":\"src/lib.rs\",\"summary\":\"Describe the defect\",\"status\":\"open\",\"evidence\":\"Observed behaviour\",\"proposed_fix\":\"Suggested repair\"}]}\n```",
        );
    }
    if let Some(spec) = &node.plan {
        let planner_limit = spec
            .extra
            .get("model_max_nodes")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(spec.max_nodes);
        prompt.push_str(&format!("\n\nReturn one final fenced `fragment` JSON block, at most 12288 bytes, holding a patch: {{\"schema_version\":1,\"nodes\":[...],\"templates\":{{...}}}}. Emit 1..{planner_limit} nodes. A node is either `use` of a template with `with` parameters and overrides (`brief`, `tier`, `after`, `inputs`, `over`, `min_ok`, `repeat`), or compiled-node fields with a `kind` of task, composite, loop, gate, decide, synthesise or land (use `plan` for a planning brief); composites, loops and further plan nodes are allowed. `over` copies a node once per named item with `${{item}}`; `repeat = {{\"until\":\"gate\",\"max_rounds\":N}}` loops a composite until its gate passes. Bundled templates: task, plan, explore, research, design, implement, implement.fix, review, review.security, verify, synthesise, repair; `templates` may define more inline. Each agent node requires id, a `brief` (or `goal`), `acceptance_checks` and local `after` or depends_on. `access` defaults to read_only; writers set worktree_write and also require `write_scope` (relative literal files/directories) ; omit `worktree` for a host-provisioned checkout or name an existing registered worktree; read-only agents may omit `write_scope`. The tier defaults to senior. Synthesise nodes require `inputs` selecting dependency outputs, either as `{{\"node\":\"id\",\"outputs\":[\"result\"]}}` or the shorthand `\"id\"` (which selects `result`). Gate nodes use the graph gate condition schema; decide nodes use the graph decide schema. Dependencies and inputs name fragment-local IDs. Keep each namespaced ID (this node's ID + -- + child ID) within 200 bytes."));
        if !spec.templates.is_empty() {
            prompt.push_str(&format!(
                " Host templates with their declared authority: {}.",
                catalogue_summary(spec)
            ));
        }
    }
    prompt
}

/// A plan node carries a plan specification and only reads. What its patch
/// may contain is not restricted here: any agent may emit any template.
pub(super) fn validate_spec(node: &GraphNode) -> Result<(), &'static str> {
    if let Some(spec) = &node.plan {
        if node.access != AccessMode::ReadOnly
            || node.required_verdict.is_some()
            || spec.max_nodes == 0
            || spec.max_nodes > 512
        {
            return Err("invalid plan configuration; a planner reads only and emits 1..=512 nodes");
        }
        let mut names = std::collections::BTreeSet::new();
        for template in &spec.templates {
            let Some(name) = template.name.as_deref() else {
                return Err("plan templates require a name");
            };
            if !names.insert(name) || !super::template::valid_template_name(name) {
                return Err("plan template names must be valid and unique");
            }
            template
                .check_authority()
                .map_err(|_| "plan template cannot vary authority by parameter")?;
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
/// A stopped-early handoff is stored whole, as the typed record.
pub(super) const STORED_HANDOFF_BYTES: usize = provenant_records::handoff::HANDOFF_BYTES;
/// Review decisions stored on review nodes, counted in the checkpoint size bound.
pub(super) const STORED_DECISION_BYTES: usize =
    provenant_records::finding_decision::FINDING_ID_BYTES
        + provenant_records::finding_decision::DECISION_REASON_BYTES
        + 256;

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
    let mut fence: Option<(u8, usize, bool, usize)> = None;
    let mut found = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        let delimiter = trimmed.as_bytes().first().copied().unwrap_or_default();
        let ticks = if matches!(delimiter, b'`' | b'~') {
            trimmed
                .bytes()
                .take_while(|byte| *byte == delimiter)
                .count()
        } else {
            0
        };
        if indent <= 3 && ticks >= 3 {
            let suffix = trimmed[ticks..].trim();
            match fence {
                Some((kind, opening, selected, begin))
                    if delimiter == kind && ticks >= opening && suffix.is_empty() =>
                {
                    if selected {
                        if found.is_some() {
                            return Err(format!("{tag} block repeated"));
                        }
                        found = Some(text[begin..offset].trim_end_matches('\n'));
                    }
                    fence = None;
                }
                None => fence = Some((delimiter, ticks, suffix == tag, offset + line.len())),
                _ => {}
            }
        }
        offset += line.len();
    }
    if fence.is_some_and(|(_, _, selected, _)| selected) {
        return Err(format!("{tag} block incomplete"));
    }
    let Some(json) = found else {
        return Ok(None);
    };
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
) -> Result<Option<provenant_records::handoff::Handoff>, String> {
    let handoff = capture.handoff.clone().map_err(str::to_owned)?;
    let Some(handoff) = handoff.filter(provenant_records::handoff::Handoff::unfinished) else {
        return Ok(None);
    };
    if serde_json::to_vec(&handoff).map_or(usize::MAX, |bytes| bytes.len()) > STORED_HANDOFF_BYTES {
        return Err(format!(
            "handoff block has too many escaped characters to store in {STORED_HANDOFF_BYTES} bytes"
        ));
    }
    Ok(Some(handoff))
}

pub(super) fn agent(node: &PlannedNode) -> bool {
    matches!(
        node.kind,
        None | Some(NodeKind::Task | NodeKind::Synthesise)
    )
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
    let mut expanded =
        loops::expand(document, extensions, limits).map_err(|error| error.to_string())?;
    let mut sources = std::collections::BTreeSet::new();
    for addition in expansions.iter().filter(|addition| !addition.retired()) {
        // A patch may come from the host (no source), a parent may patch more
        // than once, and a patch may only skip or replace.
        let valid = if addition.patch.is_some() {
            addition.source.is_empty() || expanded.nodes.iter().any(|n| n.id == addition.source)
        } else {
            !addition.nodes.is_empty()
                && sources.insert((
                    &addition.source,
                    addition
                        .extra
                        .get("retry_generation")
                        .and_then(serde_json::Value::as_u64),
                ))
                && expanded.nodes.iter().any(|n| n.id == addition.source)
        };
        if !valid {
            return Err("invalid expansion source".into());
        }
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
    for addition in expansions.iter().filter(|addition| !addition.retired()) {
        if addition.patch.is_some() {
            check_patch_topology(&expanded, addition)?;
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
    let fallback = addition
        .nodes
        .iter()
        .any(|node| node.id == fallback_id(addition));
    if !fallback {
        if let Some(node) = addition.nodes.iter().find(|node| node.composite.is_some()) {
            return Err((
                node.id.clone(),
                "composite topology must derive from a patch or fallback",
            ));
        }
        return Ok(vec![(Vec::new(), Vec::new()); addition.nodes.len()]);
    }
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
    let derived = fallback_expansion_for_generation(
        &addition.source,
        spec,
        document,
        planner.require_skill_location,
        addition.extra.get("retry_generation"),
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
    Ok(derived
        .nodes
        .into_iter()
        .map(|node| (node.composite_members, node.after_expansion))
        .collect())
}

/// A patch's persisted topology must be the one its own composites imply,
/// and a planned review's patch must keep its subject and path floors.
fn check_patch_topology(
    expanded: &GraphDocument,
    addition: &Expansion,
) -> Result<(), (String, &'static str)> {
    let ids: std::collections::BTreeSet<&str> =
        addition.nodes.iter().map(|node| node.id.as_str()).collect();
    for node in &addition.nodes {
        if node.composite.is_some() {
            let prefix = format!("{}--", node.id);
            if node.composite_members.is_empty()
                || node
                    .composite_members
                    .iter()
                    .any(|member| !member.starts_with(&prefix) || !ids.contains(member.as_str()))
                || node.after_expansion != node.composite_members
            {
                return Err((
                    node.id.clone(),
                    "persisted composite topology must match the patch's own members",
                ));
            }
        } else if !node.composite_members.is_empty() || !node.after_expansion.is_empty() {
            return Err((
                node.id.clone(),
                "persisted after_expansion must match the approved topology",
            ));
        }
    }
    if !addition.closes_producer() {
        return Ok(());
    }
    let Some(planner) = expanded
        .nodes
        .iter()
        .find(|node| node.id == addition.source)
    else {
        return Ok(());
    };
    let Some(spec) = planner.plan.as_ref() else {
        return Ok(());
    };
    if spec
        .extra
        .get("planned_review")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && addition
            .nodes
            .iter()
            .filter(|node| node.composite.is_none() && node.loop_body.is_none())
            .any(|node| node.source != planner.source)
    {
        return Err((
            addition.source.clone(),
            "planned review lenses must retain the pinned subject",
        ));
    }
    let names = |key: &str| -> Vec<String> {
        spec.extra
            .get(key)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    if names("required_floor_patterns")
        .iter()
        .any(|name| !addition.uses.iter().any(|used| &used.name == name))
    {
        return Err((
            addition.source.clone(),
            "expanded plan must include required path-floor patterns",
        ));
    }
    let candidates = names("floor_candidate_patterns");
    let minimum = spec
        .extra
        .get("floor_min_reviewers")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);
    let count = addition
        .uses
        .iter()
        .filter(|used| candidates.contains(&used.name))
        .map(|used| &used.name)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if count < minimum {
        return Err((
            addition.source.clone(),
            "expanded plan must meet the path-floor reviewer count",
        ));
    }
    Ok(())
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
    let fallback_minimum = document
        .nodes
        .iter()
        .find(|node| node.id == expansion.source)
        .and_then(|node| node.plan.as_ref())
        .and_then(|plan| plan.fallback.as_ref())
        .filter(|fallback| fallback.nodes.len() == 1)
        .and_then(|fallback| fallback.nodes[0].composite.as_ref())
        .filter(|inner| inner.reduce_verdict)
        .map(|inner| inner.min_successful);
    let review_minimum = document
        .nodes
        .iter()
        .find(|node| node.id == expansion.source)
        .and_then(|node| node.plan.as_ref())
        .filter(|plan| {
            plan.extra
                .get("planned_review")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        })
        .map(|plan| {
            plan.extra
                .get("review_min_successful")
                .and_then(serde_json::Value::as_u64)
                .map(|n| n as usize)
        });
    for node in &mut document.nodes {
        if node.dependencies().any(|name| name == &expansion.source) {
            node.after_expansion.extend(
                expansion
                    .nodes
                    .iter()
                    .filter(|n| {
                        !expansion.nodes.iter().any(|root| {
                            root.extra.get(super::relay::DETACHED)
                                == Some(&serde_json::Value::Bool(true))
                                && (n.id == root.id || n.id.starts_with(&format!("{}--", root.id)))
                        })
                    })
                    .map(|n| n.id.clone()),
            );

            if expansion.closes_producer() {
                wire_exports(node, &expansion.source, &exported);
                if let Some(composite) = &mut node.composite
                    && (composite.reduce_verdict || review_minimum.is_some())
                    && composite
                        .exports
                        .iter()
                        .any(|item| item.node == expansion.source)
                {
                    if let Some(minimum) = review_minimum {
                        composite.min_successful = minimum;
                    }
                    composite
                        .exports
                        .retain(|item| item.node != expansion.source);
                    // The fallback's own reduced panel enforces the original
                    // minimum, so its enclosing boundary counts that receipt.
                    let reduced_fallback = (fallback_minimum == Some(composite.min_successful)
                        || review_minimum.is_some())
                        && exported.len() == 1
                        && exported[0].node == fallback_id(expansion);
                    if reduced_fallback {
                        composite.min_successful = Some(1);
                    }
                    composite.exports.extend(exported.iter().cloned());
                    node.composite_members
                        .retain(|member| member != &expansion.source);
                    node.composite_members
                        .extend(exported.iter().map(|item| item.node.clone()));
                }
            }
        }
    }
    document
        .nodes
        .extend(expansion.nodes.iter().cloned().map(|mut child| {
            child.round = round.clone().map(|mut round| {
                round.until = false;
                round.node_id = child.id.clone();
                round
            });
            child
        }));
    if let Some(record) = &expansion.patch {
        restore_patch_rounds(document, expansion);
        for replacement in &record.replaced {
            if let Some(target) = document.nodes.iter_mut().find(|n| n.id == replacement.id) {
                let mut node = replacement.clone();
                node.composite_members = std::mem::take(&mut target.composite_members);
                node.after_expansion = std::mem::take(&mut target.after_expansion);
                node.round = target.round.take();
                *target = node;
            }
        }
    }
    composites::resolve_writer_targets(document);
}

/// Rounds of a loop that a patch added are derived again from their names,
/// as the base document's rounds are derived on every load.
fn restore_patch_rounds(document: &mut GraphDocument, expansion: &Expansion) {
    for boundary in &expansion.nodes {
        let Some(spec) = &boundary.loop_body else {
            continue;
        };
        for node in &mut document.nodes {
            let Some(rest) = node.id.strip_prefix(&format!("{}--r", boundary.id)) else {
                continue;
            };
            let Some((round, local)) = rest.split_once("--") else {
                continue;
            };
            let Ok(round) = round.parse::<usize>() else {
                continue;
            };
            if round == 0 || !spec.body.iter().any(|member| member.id == local) {
                continue;
            }
            node.round = Some(LoopRound {
                extra: Default::default(),
                loop_id: boundary.id.clone(),
                round,
                node_id: local.to_owned(),
                until: local == spec.until,
                previous_gate: (round > 1)
                    .then(|| format!("{}--r{}--{}", boundary.id, round - 1, spec.until)),
            });
        }
    }
}

/// The flat nodes and outputs that a fragment's pattern instances export, in
/// instantiation order. Instance IDs are already namespaced by the planner.
fn fragment_exports(document: &GraphDocument, expansion: &Expansion) -> Vec<GraphInput> {
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
            .any(|node| node.id == fallback_id(expansion))
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
            node: fallback_id(expansion),
            outputs,
        }];
    }
    // Each template use exports what its boundary exports, or its own result.
    let mut exports: Vec<GraphInput> = expansion
        .uses
        .iter()
        .filter_map(|used| expansion.nodes.iter().find(|node| node.id == used.id))
        .flat_map(|node| match &node.composite {
            Some(composite) => composite
                .exports
                .iter()
                .map(|export| GraphInput {
                    required_outputs: Vec::new(),
                    extra: Default::default(),
                    node: export.node.clone(),
                    outputs: export.outputs.clone(),
                })
                .collect::<Vec<_>>(),
            None => vec![GraphInput {
                required_outputs: Vec::new(),
                extra: Default::default(),
                node: node.id.clone(),
                outputs: vec![OutputKind::Result],
            }],
        })
        .collect();
    if spec
        .extra
        .get("planned_review")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        for (index, export) in exports.iter_mut().enumerate() {
            export.outputs.retain(|kind| {
                matches!(kind, OutputKind::Result | OutputKind::Findings)
                    || (index == 0 && *kind == OutputKind::Commit)
            });
        }
    }
    exports
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

pub(super) fn prepare(
    child: &mut GraphNode,
    node: &PlannedNode,
    spec: &PlanSpec,
    document: &GraphDocument,
    trusted: bool,
) -> Result<(), String> {
    prepare_nested(child, node, spec, document, trusted, false)
}

fn prepare_nested(
    child: &mut GraphNode,
    node: &PlannedNode,
    spec: &PlanSpec,
    document: &GraphDocument,
    trusted: bool,
    nested: bool,
) -> Result<(), String> {
    // Composite templates own their children's work bindings and delivery.
    if trusted && (child.composite.is_some() || child.loop_body.is_some()) {
        return Ok(());
    }
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
        if let Some(capability) = child
            .capabilities
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|capability| {
                !node
                    .capabilities
                    .as_deref()
                    .unwrap_or_default()
                    .contains(capability)
            })
        {
            return Err(format!(
                "fragment node {} capability {capability} exceeds the planner's capabilities",
                child.id
            ));
        }
        if child
            .attachments
            .iter()
            .any(|path| !node.attachments.contains(path))
        {
            return Err(format!(
                "fragment node {} attachment exceeds the planner's attachments",
                child.id
            ));
        }
        // Origin evidence is host-authored; a planner cannot assert it.
        child.extra.remove("capability_sources");
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
    // A nested planner's host templates carry authority, so an untrusted
    // author may only pass on templates its own planner was given.
    if !trusted
        && let Some(nested_spec) = &child.plan
        && nested_spec
            .templates
            .iter()
            .any(|template| !spec.templates.contains(template))
    {
        return Err(format!(
            "fragment node {} gives a planner templates its author was not given",
            child.id
        ));
    }
    if let Some(composite) = &mut child.composite {
        for member in &mut composite.nodes {
            prepare_nested(member, node, spec, document, trusted, true)?;
        }
    }
    if let Some(body) = &mut child.loop_body {
        for member in &mut body.body {
            prepare_nested(member, node, spec, document, trusted, true)?;
        }
    }
    // Fixed read-only preset leaves already carry their authored prompts;
    // a leaf with acceptance checks still gets them appended below.
    if trusted
        && nested
        && child.access == AccessMode::ReadOnly
        && child.acceptance_checks.is_empty()
        && child.composite.is_none()
        && !matches!(child.kind, Some(NodeKind::Gate | NodeKind::Decide))
    {
        if child.prompt.trim().is_empty() {
            return Err("fragment agent requires a prompt".into());
        }
        if child.route.is_none() && child.routes.is_none() {
            child.route = Some(GraphRoute {
                extra: Default::default(),
                alias: Some("senior".into()),
                adapter: None,
                name: None,
                model: None,
                effort: None,
                account: None,
                tier: None,
            });
        }
        return Ok(());
    }
    // Gate evidence lives in its condition and a decision in its question.
    // Writer nodes carry explicit scope; all agents carry acceptance prose.
    if !matches!(
        child.kind,
        Some(NodeKind::Gate | NodeKind::Decide | NodeKind::Composite | NodeKind::Loop)
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
                tier: None,
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

static BUNDLED_TEMPLATES: std::sync::OnceLock<super::template::TemplateLibrary> =
    std::sync::OnceLock::new();

pub(super) fn bundled_templates() -> &'static super::template::TemplateLibrary {
    BUNDLED_TEMPLATES.get_or_init(super::template::TemplateLibrary::bundled)
}

/// A planner's fragment is a patch, admitted through the one patch path.
fn fragment(
    node: &PlannedNode,
    source: &GraphDocument,
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
    json: &str,
    limits: &crate::config::GraphLimits,
    safe_reason: &mut PlanFailureFact,
) -> Result<Expansion, String> {
    let default_spec = PlanSpec {
        extra: Default::default(),
        kinds: Vec::new(),
        max_nodes: limits.nodes,
        templates: Vec::new(),
        disjoint_waves: false,
        wire_exports: false,
        fallback: None,
        fallback_depends_on: Vec::new(),
        fallback_inputs: Vec::new(),
        fallback_source: None,
        fallback_read_roots: Vec::new(),
        fallback_title: None,
    };
    let spec = node.plan.as_ref().unwrap_or(&default_spec);
    let patch = super::patch::Patch::from_fragment(json)?;
    let model_limit = spec
        .extra
        .get("model_max_nodes")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(spec.max_nodes)
        .min(spec.max_nodes);
    if patch.schema_version != 1 || patch.nodes.len() > model_limit {
        return Err(format!(
            "fragment requires 1..{model_limit} nodes and schema_version 1"
        ));
    }
    let snapshots: BTreeMap<String, Template> = source
        .extra
        .get("template_library")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid template snapshot: {error}"))?
        .unwrap_or_default();
    let layers: BTreeMap<String, super::template::Layer> = source
        .extra
        .get("template_layers")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid template layers: {error}"))?
        .unwrap_or_default();
    let mut library = bundled_templates().clone();
    for (name, template) in snapshots {
        library.insert(
            layers
                .get(&name)
                .copied()
                .unwrap_or(super::template::Layer::Inline),
            &name,
            template,
        )?;
    }
    let mut resolvers: BTreeMap<String, Vec<String>> = source
        .extra
        .get("template_resolvers")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid submission resolvers: {error}"))?
        .unwrap_or_default();
    let overrides: BTreeMap<String, Vec<String>> = spec
        .extra
        .get("resolvers")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid planner resolvers: {error}"))?
        .unwrap_or_default();
    resolvers.extend(overrides);
    let context = super::patch::PatchContext {
        library: &library,
        resolvers: &resolvers,
        scan: &super::patch::scan_for_secrets,
        journal: None,
        limits,
    };
    match super::patch::prepare(
        source,
        document,
        checkpoint,
        Some(node),
        &patch,
        &context,
        super::patch::Preparation::FinalFragment,
    )
    .map_err(|error| {
        *safe_reason = error.cause.unwrap_or(PlanFailureFact::InvalidFragment);
        error.message
    })? {
        super::patch::Prepared::Ready(admission) => {
            let mut expansion = admission.expansion;
            if !admission.warnings.is_empty() {
                expansion.extra.insert(
                    "floor_warnings".into(),
                    serde_json::json!(admission.warnings),
                );
            }
            Ok(expansion)
        }
        super::patch::Prepared::Replayed(_) => Err("fragment repeats an admitted patch".into()),
    }
}

/// Add each review lens a path floor requires that the patch did not use.
pub(super) fn add_floor_uses(
    planner: &PlannedNode,
    spec: &PlanSpec,
    document: &GraphDocument,
    compiler: &super::template::Compiler<'_>,
    local: &mut Vec<GraphNode>,
    uses: &mut Vec<TemplateUse>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let names = |key: &str| -> Vec<String> {
        spec.extra
            .get(key)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let mut required = names("required_floor_patterns");
    let candidates = names("floor_candidate_patterns");
    let minimum = spec
        .extra
        .get("floor_min_reviewers")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);
    let mut selected: std::collections::BTreeSet<String> = uses
        .iter()
        .filter(|used| candidates.contains(&used.name))
        .map(|used| used.name.clone())
        .chain(required.iter().cloned())
        .collect();
    for candidate in &candidates {
        if selected.len() >= minimum {
            break;
        }
        if selected.insert(candidate.clone()) {
            required.push(candidate.clone());
        }
    }
    if selected.len() < minimum {
        return Err(
            "path floor requires more distinct review lenses than the catalogue offers".into(),
        );
    }
    let classes = spec
        .extra
        .get("floor_classes")
        .and_then(serde_json::Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    for (index, name) in required.iter().enumerate() {
        if uses.iter().any(|used| &used.name == name) {
            continue;
        }
        if spec.template(name).is_none() {
            return Err(format!("required floor pattern {name} is not available"));
        }
        let mut id = format!("floor-{index}");
        while local.iter().any(|child| child.id == id) {
            id.push('x');
        }
        let mut node = compiler
            .compile(&serde_json::json!({"id": id, "use": name}))
            .map_err(|error| error.to_string())?;
        prepare(&mut node, planner, spec, document, true)?;
        local.push(node);
        uses.push(TemplateUse {
            extra: Default::default(),
            id,
            name: name.clone(),
            parameters: serde_json::json!({}),
            nodes: Vec::new(),
        });
        warnings.push(format!(
            "path floor {classes} added required review lens {name}"
        ));
    }
    Ok(())
}

/// Expand a patch's compiled nodes (composites and loops included), check
/// them as one graph, then namespace them under their parent and share
/// checkouts between overlapping writers.
pub(super) fn flatten_fragment(
    local_nodes: Vec<GraphNode>,
    parent: Option<&PlannedNode>,
    spec: Option<&PlanSpec>,
    document: &GraphDocument,
    checkpoint: &GraphCheckpoint,
) -> Result<Vec<GraphNode>, String> {
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
    // A host or lane patch may wait on nodes already in the graph; a planner
    // names only its own fragment's nodes. External nodes stand in as plain
    // read-only placeholders while the fragment is checked, then drop out.
    let external: BTreeSet<String> = if spec.is_none() {
        units
            .values()
            .flatten()
            .filter(|name| !units.contains_key(*name))
            .filter(|name| document.nodes.iter().any(|node| &node.id == *name))
            .cloned()
            .collect()
    } else {
        BTreeSet::new()
    };
    let mut local_nodes = local_nodes;
    fn provision_paths(nodes: &mut [GraphNode], graph: &str, prefix: &str) {
        for child in nodes {
            let id = if prefix.is_empty() {
                child.id.clone()
            } else {
                format!("{prefix}--{}", child.id)
            };
            if child.access == AccessMode::WorktreeWrite && child.worktree.is_none() {
                child.worktree = Some(super::relay::child_worktree(graph, &id));
                child.extra.insert("provision_worktree".into(), true.into());
            }
            if let Some(composite) = &mut child.composite {
                // A composite exporting one member writer's commit (a planned
                // implementation) names that checkout after itself, as a
                // relayed add of the same child does.
                let mut exported = composite
                    .exports
                    .iter()
                    .filter(|export| export.outputs.contains(&OutputKind::Commit));
                if let (Some(export), None) = (exported.next(), exported.next())
                    && let Some(writer) = composite.nodes.iter_mut().find(|member| {
                        member.id == export.node
                            && member.access == AccessMode::WorktreeWrite
                            && member.worktree.is_none()
                            && member.composite.is_none()
                            && member.loop_body.is_none()
                    })
                {
                    writer.worktree = Some(super::relay::child_worktree(graph, &id));
                    writer
                        .extra
                        .insert("provision_worktree".into(), true.into());
                }
                provision_paths(&mut composite.nodes, graph, &id);
            }
            if let Some(body) = &mut child.loop_body {
                provision_paths(&mut body.body, graph, &id);
            }
        }
    }
    provision_paths(
        &mut local_nodes,
        &document.graph_id,
        parent.map_or("", |parent| parent.id.as_str()),
    );
    for existing in document
        .nodes
        .iter()
        .filter(|node| external.contains(&node.id))
    {
        let name = &existing.id;
        let mut stub: GraphNode = serde_json::from_value(serde_json::json!({
            "id": name, "access": "read_only", "prompt": "placeholder", "route": {"alias": "senior"}
        }))
        .map_err(|error| error.to_string())?;
        stub.require_findings = existing.require_findings;
        stub.required_verdict.clone_from(&existing.required_verdict);
        local_nodes.push(stub);
    }
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
    // Composites and loops expand before the patch is checked, so the host
    // limits see every member and round.
    let mut local = loops::expand(&local, &[], &generous()).map_err(|error| error.to_string())?;
    for child in &mut local.nodes {
        if child.composite.is_none()
            && child.loop_body.is_none()
            && matches!(
                child.kind,
                None | Some(NodeKind::Task | NodeKind::Synthesise)
            )
            && child.route.is_none()
            && child.routes.is_none()
        {
            child.route = Some(GraphRoute {
                extra: Default::default(),
                alias: Some("senior".into()),
                adapter: None,
                name: None,
                model: None,
                effort: None,
                account: None,
                tier: None,
            });
        }
    }
    validate_schema(&local).map_err(|error| error.to_string())?;
    // The enclosing writer is not part of this isolated fragment. Preserve
    // its binding for the full patch preview, which checks ancestry, delivery
    // mode and worktree after the fragment has joined its parent graph.
    let mut local_validation = local.clone();
    for child in &mut local_validation.nodes {
        if child.continues.as_ref().is_some_and(|writer| {
            !local.nodes.iter().any(|node| &node.id == writer)
                && document.nodes.iter().any(|node| &node.id == writer)
        }) {
            child.continues = None;
        }
    }
    let (plan, _) =
        validate_flat_plan(&local_validation, &generous()).map_err(|error| error.to_string())?;
    validate_output_kinds(&local).map_err(|error| error.to_string())?;
    let mut nodes: Vec<GraphNode> = plan
        .nodes
        .iter()
        .filter(|n| !external.contains(&n.id))
        .filter_map(|n| local.nodes.iter().find(|child| child.id == n.id).cloned())
        .collect();
    if spec.is_some_and(|spec| spec.disjoint_waves) {
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
    if let Some(parent) = parent {
        let local_ids: BTreeSet<_> = nodes.iter().map(|node| node.id.clone()).collect();
        let id = |name: &str| {
            if external.contains(name)
                || (!local_ids.contains(name) && document.nodes.iter().any(|node| node.id == name))
            {
                name.to_owned()
            } else {
                format!("{}--{name}", parent.id)
            }
        };
        for child in &mut nodes {
            if id(&child.id).len() > 200 {
                return Err(format!(
                    "child {} of planner {} exceeds the namespaced node ID limit of 200 bytes; shorten either ID",
                    child.id, parent.id
                ));
            }
            child.id = id(&child.id);
            child.depends_on = child.depends_on.iter().map(|name| id(name)).collect();
            // A planner's patch consumes its plan; a lane's children do not
            // wait for the lane that spawned them.
            if child.depends_on.is_empty() && spec.is_some() {
                child.depends_on.push(parent.id.clone());
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
            if let Some(
                SourceBinding::Node { node: source } | SourceBinding::Head { node: source },
            ) = &mut child.source
            {
                *source = id(source);
            }
            if let Some(GateCondition::Ci {
                target: CiTarget::Node { node: target, .. },
                ..
            }) = &mut child.condition
            {
                *target = id(target);
            }
            for writer in child
                .continues
                .iter_mut()
                .chain(child.land.as_mut().map(|spec| &mut spec.node))
            {
                *writer = id(writer);
            }
            if let Some(plan) = &mut child.plan {
                for template in &mut plan.templates {
                    for fields in std::iter::once(&mut template.node).chain(
                        template
                            .nodes
                            .iter_mut()
                            .filter_map(serde_json::Value::as_object_mut),
                    ) {
                        if let Some(writer) = fields.get_mut("continues")
                            && let Some(name) = writer.as_str()
                        {
                            *writer = serde_json::Value::String(id(name));
                        }
                    }
                }
            }
            child.on_fail = child.on_fail.as_deref().map(id);
        }
    }
    for child in &mut nodes {
        child.round = None;
    }
    coordinate(&mut nodes, document, checkpoint)?;
    Ok(nodes)
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
            let continues_settled_branch = members.iter().all(|child| {
                child.continues.as_ref().is_some_and(|target| {
                    existing.iter().all(|writer| {
                        (writer.id == *target || writer.continues.as_ref() == Some(target))
                            && writer.worktree == child.worktree
                            && checkpoint.nodes.iter().any(|progress| {
                                progress.id == writer.id && progress.state.settled()
                            })
                    })
                })
            });
            if document.integration_branch.is_none() && !continues_settled_branch {
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

/// Host restarts after a stop share the graph-wide allowance.
pub(super) fn continuation_count(checkpoint: &GraphCheckpoint) -> usize {
    checkpoint
        .expansions
        .iter()
        .filter(|expansion| {
            expansion
                .patch
                .as_ref()
                .is_some_and(|record| record.restart.is_some())
        })
        .count()
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
        .any(|n| n.plan.is_some() || n.access == AccessMode::WorktreeWrite)
    {
        checkpoint.remaining = Some(RemainingBudgets {
            extra: Default::default(),
            nodes: limits.nodes.saturating_sub(document.nodes.len()),
            edges: limits.edges.saturating_sub(
                document
                    .nodes
                    .iter()
                    .map(|n| n.dependencies().count())
                    .sum(),
            ),
            continuations: limits
                .continuations
                .saturating_sub(continuation_count(checkpoint)),
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
    let generation = checkpoint
        .nodes
        .iter()
        .find(|n| n.id == node.id)
        .and_then(|n| n.extra.get("retry_generation"))
        .cloned();
    if checkpoint.expansions.iter().any(|e| {
        e.source == node.id
            && e.extra.get("retry_generation") == generation.as_ref()
            && e.closes_producer()
    }) {
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
        let mut safe_fallback_error =
            "fallback graph is invalid; check the declared graph constraints".to_owned();
        let built = (|| {
            let mut addition = fallback_expansion_for_generation(
                &node.id,
                spec,
                source,
                node.require_skill_location,
                generation.as_ref(),
            )?;
            if let Some(generation) = &generation {
                addition
                    .extra
                    .insert("retry_generation".into(), generation.clone());
            }
            let mut preview = document.clone();
            append(&mut preview, &addition);
            let (plan, positions) =
                validate_flat_plan(&preview, &generous()).map_err(|error| error.to_string())?;
            if let Some(reason) = structural_limit_reason(&preview, &plan, limits) {
                safe_fallback_error = reason.clone();
                return Err(reason);
            }
            let mut expansions = checkpoint.expansions.clone();
            expansions.push(addition.clone());
            if let Some(reason) =
                graph_envelope_reason(source, &expansions, &checkpoint.extensions, &plan)
            {
                safe_fallback_error = reason.clone();
                return Err(reason);
            }
            Ok((addition, plan, positions, preview))
        })();
        let (addition, plan, positions, preview) = match built {
            Ok(built) => built,
            Err(_reason) => {
                // Same diagnostic as a rejected planner expansion: pause with a reason.
                let reason = bounded(
                    &format!(
                        "node {} fallback expansion paused: {safe_fallback_error}",
                        node.id
                    ),
                    MAX_ADVANCE_ERROR_BYTES,
                );
                set_expansion_pause(checkpoint, index, reason, true, None);
                return None;
            }
        };
        checkpoint.nodes[index].state = NodeState::Succeeded;
        clear_expansion_pause(checkpoint, index, false);
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
    if node.plan.is_none()
        && !writer(node)
        && capture.is_none()
        && checkpoint.nodes[index].expansion_pause.is_some()
        && checkpoint.nodes[index].fragment.is_none()
    {
        return None;
    }
    // Source and deserialisation errors can contain captured provider output.
    // Retain a closed, safe explanation for status and fallback diagnostics.
    let mut safe_reason = PlanFailureFact::InvalidGraph;
    let final_patch = capture.map(|capture| {
        block(capture, "patch")
            .and_then(|patch| match patch {
                Some(patch) => Ok(Some(patch)),
                None => block(capture, "fragment"),
            })
            .map(|patch| patch.map(str::to_owned))
    });
    let has_patch = final_patch
        .as_ref()
        .is_some_and(|result| !matches!(result, Ok(None)));
    let addition = if node.plan.is_some() || has_patch || checkpoint.nodes[index].fragment.is_some()
    {
        let json = if let Some(capture) = capture {
            let _ = capture;
            match final_patch.unwrap_or(Ok(None)) {
                Ok(Some(json)) => super::patch::scan_for_secrets(&json).map(|()| {
                    checkpoint.nodes[index].fragment = Some(json.clone());
                    json
                }),
                Ok(None) => {
                    safe_reason = PlanFailureFact::MissingBlock;
                    Err("fragment block missing".into())
                }
                Err(error) => {
                    safe_reason = PlanFailureFact::InvalidBlock;
                    Err(error)
                }
            }
        } else {
            checkpoint.nodes[index].fragment.clone().ok_or_else(|| {
                safe_reason = PlanFailureFact::MissingResult;
                "planner result missing".into()
            })
        };
        json.and_then(|json| {
            safe_reason = PlanFailureFact::InvalidFragment;
            fragment(
                node,
                source,
                document,
                checkpoint,
                &json,
                limits,
                &mut safe_reason,
            )
        })
    } else {
        if writer(node)
            && capture.is_some_and(|capture| handoff(capture).is_err())
            && let Some(outputs) = &mut checkpoint.nodes[index].outputs
        {
            outputs
                .warnings
                .push("ignored optional handoff: invalid handoff; inspect the result".into());
        }
        return None;
    };
    let result = addition.and_then(|mut addition| {
        addition
            .extra
            .insert("final_result".into(), serde_json::Value::Bool(true));
        if let Some(generation) = &generation {
            addition
                .extra
                .insert("retry_generation".into(), generation.clone());
        }
        if let Some(warnings) = addition
            .extra
            .get("floor_warnings")
            .and_then(serde_json::Value::as_array)
            && let Some(outputs) = &mut checkpoint.nodes[index].outputs
        {
            outputs.warnings.extend(
                warnings
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(|warning| bounded(warning, WARNING_BYTES)),
            );
        }
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
        validate_schema(&preview).map_err(|error| {
            safe_reason = PlanFailureFact::Schema;
            error.to_string()
        })?;
        let (plan, positions) = validate_flat_plan(&preview, &generous()).map_err(|error| {
            safe_reason = PlanFailureFact::InvalidGraph;
            error.to_string()
        })?;
        validate_output_kinds(&preview).map_err(|error| {
            safe_reason = PlanFailureFact::OutputKind;
            error.to_string()
        })?;
        if let Some(limit) = structural_limit(&preview, &plan, limits) {
            safe_reason = PlanFailureFact::Structural(limit);
            return Err(limit.to_string());
        }
        let mut expansions = checkpoint.expansions.clone();
        expansions.push(addition.clone());
        let bound = graph_envelope_bound(source, &expansions, &checkpoint.extensions, &plan);
        if bound > MAX_CHECKPOINT_BYTES as usize {
            let limit = GraphEnvelopeLimit { bound };
            safe_reason = PlanFailureFact::Envelope(limit);
            return Err(limit.to_string());
        }
        Ok((addition, plan, positions, preview))
    });
    match result {
        Ok((addition, plan, positions, preview)) => {
            checkpoint.nodes[index].fragment = None;
            clear_expansion_pause(checkpoint, index, true);
            super::patch::commit(checkpoint, addition, &preview, &plan, limits);
            Some((preview, plan, positions))
        }
        Err(reason) => {
            let reason = if super::patch::scan_for_secrets(&reason).is_err() {
                "patch diagnostic contains a secret; inspect the captured result".to_owned()
            } else {
                reason
                    .chars()
                    .flat_map(|character| {
                        if character.is_control() {
                            character.escape_default().collect::<Vec<_>>()
                        } else {
                            vec![character]
                        }
                    })
                    .take(MAX_ADVANCE_ERROR_BYTES)
                    .collect()
            };
            if let Some(outputs) = &mut checkpoint.nodes[index].outputs {
                let warning = bounded(&format!("patch refused: {reason}"), WARNING_BYTES);
                if !outputs.warnings.contains(&warning) {
                    outputs.warnings.truncate(MAX_WARNINGS.saturating_sub(1));
                    outputs.warnings.push(warning);
                }
            }
            if let Some(spec) = node.plan.as_ref().filter(|spec| spec.fallback.is_some()) {
                let fragment_error = match &checkpoint.nodes[index].expansion_cause {
                    Some(NodeDecisionCause::PlanExpansionPaused {
                        fragment_error: Some(error),
                        ..
                    }) => *error,
                    _ => safe_reason,
                };
                let fallback_reason = bounded(
                    &format!("planner fragment invalid: {fragment_error}"),
                    NOTE_BYTES,
                );
                let mut safe_fallback_error =
                    "fallback graph is invalid; check the declared graph constraints".to_owned();
                let fallback = (|| {
                    let mut addition = fallback_expansion_for_generation(
                        &node.id,
                        spec,
                        source,
                        node.require_skill_location,
                        generation.as_ref(),
                    )?;
                    if let Some(generation) = &generation {
                        addition
                            .extra
                            .insert("retry_generation".into(), generation.clone());
                    }
                    let mut preview = document.clone();
                    append(&mut preview, &addition);
                    validate_schema(&preview).map_err(|error| error.to_string())?;
                    let (plan, positions) = validate_flat_plan(&preview, &generous())
                        .map_err(|error| error.to_string())?;
                    validate_output_kinds(&preview).map_err(|error| error.to_string())?;
                    if let Some(reason) = structural_limit_reason(&preview, &plan, limits) {
                        safe_fallback_error = reason.clone();
                        return Err(reason);
                    }
                    let mut expansions = checkpoint.expansions.clone();
                    expansions.push(addition.clone());
                    if let Some(reason) =
                        graph_envelope_reason(source, &expansions, &checkpoint.extensions, &plan)
                    {
                        safe_fallback_error = reason.clone();
                        return Err(reason);
                    }
                    Ok((addition, plan, positions, preview))
                })();
                match fallback {
                    Ok((addition, plan, positions, preview)) => {
                        checkpoint.nodes[index].state = NodeState::Succeeded;
                        checkpoint.nodes[index].fragment = None;
                        clear_expansion_pause(checkpoint, index, true);
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
                    Err(_fallback_error) => {
                        let display_fragment_error = bounded(&fragment_error.to_string(), 350);
                        let display_fallback_error = bounded(&safe_fallback_error, 350);
                        let display_reason = format!(
                            "node {} expansion paused: planner fragment invalid: {display_fragment_error}; fallback expansion paused: {display_fallback_error}",
                            node.id
                        );
                        set_expansion_pause(
                            checkpoint,
                            index,
                            bounded(&display_reason, MAX_ADVANCE_ERROR_BYTES),
                            false,
                            Some(fragment_error),
                        );
                        return None;
                    }
                }
            }
            let reason = bounded(
                &format!("node {} expansion paused: {safe_reason}: {reason}", node.id),
                MAX_ADVANCE_ERROR_BYTES,
            );
            set_expansion_pause(checkpoint, index, reason, false, None);
            None
        }
    }
}

/// The static stage a planner's `fallback` spec expands to in the original
/// document. Deterministic, so `settle` appends it and checkpoint validation
/// recomputes it to check what was persisted.
fn generation_namespace(source: &str, generation: Option<&serde_json::Value>) -> String {
    match generation.and_then(serde_json::Value::as_u64) {
        Some(generation) => format!("{source}--redo-{generation}"),
        None => source.to_owned(),
    }
}

fn fallback_id(expansion: &Expansion) -> String {
    format!(
        "{}--fallback",
        generation_namespace(&expansion.source, expansion.extra.get("retry_generation"))
    )
}

pub(super) fn fallback_expansion_for_generation(
    node_id: &str,
    spec: &PlanSpec,
    source: &GraphDocument,
    require_skill_location: bool,
    generation: Option<&serde_json::Value>,
) -> Result<Expansion, String> {
    let namespace = generation_namespace(node_id, generation);
    let mut expansion = fallback_expansion(&namespace, spec, source, require_skill_location)?;
    expansion.source = node_id.to_owned();
    if let Some(generation) = generation {
        expansion
            .extra
            .insert("retry_generation".into(), generation.clone());
    }
    Ok(expansion)
}

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
        extra: Default::default(),
        source: node_id.to_owned(),
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
        uses: Vec::new(),
        patch: None,
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
    // A result excerpt is captured output, not an approved diagnostic fact.
    let detail = progress
        .advance_error
        .as_deref()
        .filter(|detail| !detail.trim().is_empty());
    let reason = detail.map_or_else(
        || format!("planner attempt failed: {outcome}"),
        |detail| format!("planner attempt failed ({outcome}): {detail}"),
    );
    bounded(&reason, NOTE_BYTES)
}
