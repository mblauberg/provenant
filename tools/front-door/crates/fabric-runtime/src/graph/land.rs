//! Writer continuation and graph `land` delivery.
//!
//! A node that `continues` an upstream writer reuses its worktree and delivery
//! branch: the host fast-forward pushes the new commits to that branch only,
//! so a repair updates the same pull request. A `land` node accepts either a
//! legacy PR writer or a queue publication receipt, then verifies the required
//! gates against the pinned head. The host's risk-tier policy decides whether it
//! lands at once or asks the chair, who lands by hand; the record keeps who or
//! what authorised it.
use super::*;
use crate::config::{LandChoice, LandPolicy};
use provenant_core::review_cover::Reviewed;
use provenant_records::run::DeliveryMode;
use provenant_records::workflow::{ReviewVerdict, RiskTier};
use std::collections::{BTreeMap, BTreeSet};

mod execution;
pub(super) use execution::{advance, land_ask, validate_progress};

/// Land the pull request delivered by `node`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandSpec {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub node: String,
    pub tier: RiskTier,
    /// Inbox session for a tier whose policy asks; falls back to the graph's
    /// `decision_session`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// What authorised a land.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LandAuthority {
    /// The tier policy was `auto`.
    Policy,
    /// The tier asked in `ask_id`, and the chair merged the pull request at the
    /// pinned head with `provenant land`, which the node observed. An inbox
    /// answer never authorises a land: lanes and peers can write answers.
    Chair { ask_id: String },
}

/// A land node's pinned subject and decision, kept as the receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandRecord {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub tier: RiskTier,
    pub policy: LandPolicy,
    pub policy_source: Layer,
    pub branch: String,
    pub head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorised_by: Option<LandAuthority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<u64>,
    /// The host's latest pending or merge note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Next retry of a pending land, which tick wake discovery includes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<provenant_core::WallTime>,
    /// Pending land observations already made, which selects the next backoff.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub pending_observations: u64,
}

/// One land, pinned to the head the reviews and CI saw.
#[derive(Clone, Copy, Debug)]
pub struct LandRequest<'a> {
    pub graph_id: &'a str,
    pub node_id: &'a str,
    pub branch: &'a str,
    pub head: &'a str,
    /// The PR identity saved before an authorised merge is attempted.
    pub pull_request: Option<u64>,
    pub tier: RiskTier,
    /// `None` while the tier asks: the host only observes whether the chair
    /// merged the pull request at `head`, and never merges.
    pub authority: Option<&'a LandAuthority>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandObservation {
    /// A uniquely identified PR; save this identity before any merge effect.
    Identified { pull_request: u64 },
    Merged {
        pull_request: Option<u64>,
        note: Option<String>,
    },
    /// Not merged yet, such as checks still running. Observe again later.
    Pending { reason: String },
    /// The land cannot proceed; the node fails with this reason.
    Refused { reason: String },
}

pub(super) const HOLD: &str = "hold";
pub(super) const LANDED: &str = "LANDED";
const PUSH: DeliveryMode = DeliveryMode::Push;

/// The delivery a node launches with: a continuing writer pushes.
pub(super) fn delivery(node: &PlannedNode) -> Option<&DeliveryMode> {
    if node.continues.is_some() {
        Some(&PUSH)
    } else {
        node.delivery.as_ref()
    }
}

/// Static checks on one declared node.
pub(super) fn validate_node(node: &GraphNode) -> Result<(), &'static str> {
    if let Some(target) = &node.continues
        && (node.access != AccessMode::WorktreeWrite
            || node.kind.is_some_and(|kind| kind != NodeKind::Task)
            || node.delivery.is_some()
            || node.worktree.is_none()
            || target == &node.id)
    {
        return Err(
            "continues requires a writer that names the target's worktree and no delivery of its own",
        );
    }
    if let Some(spec) = &node.land
        && (node.route.is_some()
            || node.routes.is_some()
            || node.access != AccessMode::ReadOnly
            || node.delivery.is_some()
            || node.worktree.is_some()
            || !node.prompt.is_empty()
            || node.required_verdict.is_some()
            || !node.inputs.is_empty()
            || node.on_fail.is_some()
            || node.continues.is_some()
            || !valid_node_identifier(&spec.node)
            || spec
                .session
                .as_ref()
                .is_some_and(|session| crate::inbox::session_id(session).is_err()))
    {
        return Err(
            "land takes a writer node, a tier and an optional session, and no agent configuration",
        );
    }
    Ok(())
}

fn ancestors(
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
    id: &str,
) -> BTreeSet<usize> {
    let mut seen = BTreeSet::new();
    let mut pending: Vec<usize> = positions.get(id).into_iter().copied().collect();
    while let Some(index) = pending.pop() {
        for parent in nodes[index].dependencies() {
            if let Some(parent) = positions.get(parent)
                && seen.insert(*parent)
            {
                pending.push(*parent);
            }
        }
    }
    seen
}

fn writer_chain_root(
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
    mut writer: usize,
) -> Option<usize> {
    for _ in 0..nodes.len() {
        let node = &nodes[writer];
        let Some(parent) = node.continues.as_deref() else {
            return plans::writer(node).then_some(writer);
        };
        let parent_index = *positions.get(parent)?;
        let parent_node = &nodes[parent_index];
        if !plans::writer(parent_node) || parent_node.worktree != node.worktree {
            return None;
        }
        writer = parent_index;
    }
    None
}

fn publish_source_hygiene(
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
    publish_index: usize,
) -> Option<usize> {
    let publish = nodes.get(publish_index)?;
    if publish.kind != Some(NodeKind::Publish) {
        return None;
    }
    let upstream = ancestors(nodes, positions, &publish.id);
    let hygiene_id = match publish.source.as_ref()? {
        SourceBinding::Node { node } => node,
        _ => return None,
    };
    let hygiene_index = *positions.get(hygiene_id)?;
    let hygiene = &nodes[hygiene_index];
    if hygiene.kind != Some(NodeKind::Hygiene) || !upstream.contains(&hygiene_index) {
        return None;
    }
    Some(hygiene_index)
}

fn publish_source_writer(
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
    publish_index: usize,
) -> Option<usize> {
    let hygiene_index = publish_source_hygiene(nodes, positions, publish_index)?;
    let publish = nodes.get(publish_index)?;
    let upstream = ancestors(nodes, positions, &publish.id);
    let hygiene = &nodes[hygiene_index];
    let writer_id = match hygiene.source.as_ref()? {
        SourceBinding::Node { node } => node,
        _ => return None,
    };
    let writer_index = *positions.get(writer_id)?;
    if !upstream.contains(&writer_index)
        || !plans::writer(&nodes[writer_index])
        || writer_chain_root(nodes, positions, writer_index).is_none()
    {
        return None;
    }
    Some(writer_index)
}

fn recovery_members(
    root: usize,
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
) -> BTreeSet<usize> {
    let mut members = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(index) = pending.pop() {
        if !members.insert(index) {
            continue;
        }
        let node = &nodes[index];
        pending.extend(
            node.composite_members
                .iter()
                .filter_map(|id| positions.get(id).copied()),
        );
        if node.loop_body.is_some() {
            pending.extend(nodes.iter().enumerate().filter_map(|(index, member)| {
                member
                    .round
                    .as_ref()
                    .is_some_and(|round| round.loop_id == node.id)
                    .then_some(index)
            }));
        }
    }
    members
}

fn recovery_previous_publication_binding(
    writer_index: usize,
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
) -> bool {
    let writer = &nodes[writer_index];
    let Some(round) = writer.round.as_ref() else {
        return false;
    };
    let Some(loop_index) = positions.get(&round.loop_id).copied() else {
        return false;
    };
    let Some(spec) = nodes[loop_index].loop_body.as_ref() else {
        return false;
    };
    let previous_publication = if round.round == 1 {
        spec.selected_publication
            .as_ref()
            .and_then(|local| spec.previous_bindings.get(local))
            .cloned()
    } else {
        spec.selected_publication
            .as_ref()
            .map(|local| format!("{}--r{}--{}", round.loop_id, round.round - 1, local))
    };
    let Some(previous_publication) = previous_publication else {
        return false;
    };
    let Some(publication_index) = positions.get(&previous_publication).copied() else {
        return false;
    };
    let publication = &nodes[publication_index];
    publication.kind == Some(NodeKind::Publish)
        && writer.inputs.iter().any(|input| {
            input.node == previous_publication
                && input.outputs.contains(&OutputKind::Branch)
                && input.outputs.contains(&OutputKind::Commit)
        })
        && (writer.depends_on.contains(&previous_publication)
            || writer.after_settled.contains(&previous_publication))
}

fn recovery_previous_hygiene_binding<'a>(
    writer_index: usize,
    nodes: &'a [PlannedNode],
    positions: &BTreeMap<String, usize>,
) -> Option<(&'a str, &'a str)> {
    let writer = nodes.get(writer_index)?;
    let round = writer.round.as_ref()?;
    let loop_index = *positions.get(&round.loop_id)?;
    let spec = nodes[loop_index].loop_body.as_ref()?;
    let body_writer = spec.body.iter().find(|member| member.id == round.node_id)?;
    let SourceBinding::Node { node: source } = body_writer.source.as_ref()? else {
        return None;
    };
    let hygiene_local = source.strip_prefix("previous.")?;
    if !spec
        .body
        .iter()
        .any(|member| member.id == hygiene_local && member.kind == Some(NodeKind::Hygiene))
    {
        return None;
    }
    let initial = spec.previous_bindings.get(hygiene_local)?;
    Some((hygiene_local, initial.as_str()))
}

fn recovery_source_is_published_head(
    index: usize,
    published_id: &str,
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
    members: &BTreeSet<usize>,
    visiting: &mut BTreeSet<usize>,
) -> bool {
    if !members.contains(&index) || !visiting.insert(index) {
        return false;
    }
    let bound = match &nodes[index].source {
        Some(SourceBinding::Head { node }) if node == published_id => true,
        Some(SourceBinding::Head { node }) => positions.get(node).is_some_and(|source| {
            let Some(current_round) = nodes[index].round.as_ref() else {
                return false;
            };
            let Some(source_round) = nodes[*source].round.as_ref() else {
                return false;
            };
            if nodes[*source].kind != Some(NodeKind::Publish)
                || !members.contains(source)
                || source_round.loop_id != current_round.loop_id
                || source_round.round + 1 != current_round.round
            {
                return false;
            }
            let Some(source_writer) = publish_source_writer(nodes, positions, *source) else {
                return false;
            };
            nodes[source_writer].worktree == nodes[index].worktree
                && recovery_source_is_published_head(
                    source_writer,
                    published_id,
                    nodes,
                    positions,
                    members,
                    visiting,
                )
        }),
        Some(SourceBinding::Node { node }) => {
            let Some(source_index) = positions.get(node).copied() else {
                visiting.remove(&index);
                return false;
            };
            if node == published_id {
                recovery_previous_publication_binding(index, nodes, positions)
                    && recovery_previous_hygiene_binding(index, nodes, positions)
                        .is_some_and(|(_, initial)| initial == node)
            } else if members.contains(&source_index) {
                let same_worktree = nodes[index].worktree == nodes[source_index].worktree;
                let selected_hygiene = if nodes[source_index].kind == Some(NodeKind::Hygiene) {
                    let Some(current_round) = nodes[index].round.as_ref() else {
                        visiting.remove(&index);
                        return false;
                    };
                    let Some(source_round) = nodes[source_index].round.as_ref() else {
                        visiting.remove(&index);
                        return false;
                    };
                    let Some(loop_index) = positions.get(&current_round.loop_id).copied() else {
                        visiting.remove(&index);
                        return false;
                    };
                    let Some(spec) = nodes[loop_index].loop_body.as_ref() else {
                        visiting.remove(&index);
                        return false;
                    };
                    let Some(local_publish) = spec.selected_publication.as_deref() else {
                        visiting.remove(&index);
                        return false;
                    };
                    let prior_publish = format!(
                        "{}--r{}--{}",
                        current_round.loop_id, source_round.round, local_publish
                    );
                    positions.get(&prior_publish).is_some_and(|publication| {
                        source_round.loop_id == current_round.loop_id
                            && source_round.round + 1 == current_round.round
                            && spec.retry_on.contains(&source_round.node_id)
                            && publish_source_hygiene(nodes, positions, *publication)
                                == Some(source_index)
                    })
                } else {
                    true
                };
                same_worktree
                    && selected_hygiene
                    && (nodes[source_index].kind != Some(NodeKind::Hygiene)
                        || recovery_previous_publication_binding(index, nodes, positions))
                    && recovery_source_is_published_head(
                        source_index,
                        published_id,
                        nodes,
                        positions,
                        members,
                        visiting,
                    )
            } else {
                let Some(current_round) = nodes[index].round.as_ref() else {
                    visiting.remove(&index);
                    return false;
                };
                let Some(loop_index) = positions.get(&current_round.loop_id).copied() else {
                    visiting.remove(&index);
                    return false;
                };
                let Some(spec) = nodes[loop_index].loop_body.as_ref() else {
                    visiting.remove(&index);
                    return false;
                };
                let initial_hygiene = recovery_previous_hygiene_binding(index, nodes, positions)
                    .map(|(_, initial)| initial);
                let initial_publication = spec.initial_publication.as_deref();
                let initial_seed_matches = positions.get(published_id).is_some_and(|publication| {
                    source_index == *publication
                        || publish_source_hygiene(nodes, positions, *publication)
                            == Some(source_index)
                });
                current_round.round == 1
                    && initial_hygiene.is_some_and(|initial| initial == node.as_str())
                    && initial_publication == Some(published_id)
                    && initial_seed_matches
                    && recovery_previous_publication_binding(index, nodes, positions)
                    && nodes[source_index].kind == Some(NodeKind::Hygiene)
                    && nodes[source_index].worktree == nodes[index].worktree
            }
        }
        _ => false,
    };
    visiting.remove(&index);
    bound
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BoundedConflictRecheck {
    pub(super) loop_id: usize,
    pub(super) round: usize,
    pub(super) initial_conflict: usize,
    pub(super) fresh_conflict: usize,
    pub(super) fresh_ci: usize,
}

fn dependency_ancestors(
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
    id: &str,
) -> BTreeSet<usize> {
    let mut seen = BTreeSet::new();
    let mut pending: Vec<usize> = positions.get(id).into_iter().copied().collect();
    while let Some(index) = pending.pop() {
        for parent in &nodes[index].depends_on {
            if let Some(parent) = positions.get(parent)
                && seen.insert(*parent)
            {
                pending.push(*parent);
            }
        }
    }
    seen
}

/// Find same-round conflict recovery routes that can end successfully only
/// after checking CI on their fresh publication. The CI dependency is a real
/// `depends_on` edge so a failed conflict check blocks CI and permits retry.
pub(super) fn bounded_conflict_rechecks(
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
) -> Vec<BoundedConflictRecheck> {
    let mut rechecks = Vec::new();
    for (fresh_conflict_index, gate) in nodes.iter().enumerate() {
        let Some(round) = gate.round.as_ref().filter(|round| !round.until) else {
            continue;
        };
        let Some(GateCondition::Conflict {
            target: CiTarget::Node {
                node: published_id, ..
            },
            ..
        }) = gate.condition.as_ref()
        else {
            continue;
        };
        if gate.on_fail.is_some() {
            continue;
        }
        let Some(published_index) = positions.get(published_id).copied() else {
            continue;
        };
        let published = &nodes[published_index];
        if published.kind != Some(NodeKind::Publish)
            || !published.round.as_ref().is_some_and(|published_round| {
                published_round.loop_id == round.loop_id && published_round.round == round.round
            })
        {
            continue;
        }
        let Some(writer_index) = publish_source_writer(nodes, positions, published_index) else {
            continue;
        };
        let writer = &nodes[writer_index];
        if !writer.round.as_ref().is_some_and(|writer_round| {
            writer_round.loop_id == round.loop_id && writer_round.round == round.round
        }) {
            continue;
        }
        let Some(SourceBinding::Head {
            node: prior_publish,
        }) = writer.source.as_ref()
        else {
            continue;
        };
        let Some(prior_publish_index) = positions.get(prior_publish).copied() else {
            continue;
        };
        let prior_round = nodes[prior_publish_index].round.as_ref();
        let prior_binding_is_current = if round.round == 1 {
            prior_round.is_none_or(|prior_round| prior_round.loop_id != round.loop_id)
        } else {
            prior_round.is_some_and(|prior_round| {
                prior_round.loop_id == round.loop_id && prior_round.round + 1 == round.round
            })
        };
        if !prior_binding_is_current {
            continue;
        }
        let Some(initial_conflict) = nodes.iter().enumerate().find_map(|(index, prior_gate)| {
            (prior_gate.round.as_ref().is_some_and(|prior_round| {
                prior_round.loop_id == round.loop_id && prior_round.round == round.round
            }) && matches!(
                prior_gate.condition.as_ref(),
                Some(GateCondition::Conflict {
                    target: CiTarget::Node { node, .. },
                    ..
                }) if node == prior_publish
            ) && prior_gate.on_fail.as_ref().is_some_and(|recovery| {
                positions.get(recovery).is_some_and(|recovery_index| {
                    recovery_members(*recovery_index, nodes, positions).contains(&writer_index)
                })
            }))
            .then_some(index)
        }) else {
            continue;
        };
        let conflict_ancestors = ancestors(nodes, positions, &gate.id);
        if !conflict_ancestors.contains(&published_index)
            || !conflict_ancestors.contains(&initial_conflict)
            || !conflict_ancestors.contains(&prior_publish_index)
        {
            continue;
        }
        let Some(fresh_ci) = nodes.iter().enumerate().find_map(|(index, ci)| {
            (ci.round.as_ref().is_some_and(|ci_round| {
                ci_round.loop_id == round.loop_id && ci_round.round == round.round
            }) && matches!(
                ci.condition.as_ref(),
                Some(GateCondition::Ci {
                    target: CiTarget::Node { node, .. },
                    ..
                }) if node == published_id
            ) && {
                let ancestors = dependency_ancestors(nodes, positions, &ci.id);
                ancestors.contains(&fresh_conflict_index) && ancestors.contains(&published_index)
            })
            .then_some(index)
        }) else {
            continue;
        };
        let has_initial_ci_until = nodes.iter().any(|until| {
            until.round.as_ref().is_some_and(|until_round| {
                until_round.loop_id == round.loop_id
                    && until_round.round == round.round
                    && until_round.until
            }) && matches!(
                until.condition.as_ref(),
                Some(GateCondition::Ci {
                    target: CiTarget::Node { node, .. },
                    ..
                }) if node == prior_publish
            ) && dependency_ancestors(nodes, positions, &until.id).contains(&initial_conflict)
                && ancestors(nodes, positions, &until.id).contains(&fresh_ci)
        });
        if !has_initial_ci_until {
            continue;
        }
        rechecks.push(BoundedConflictRecheck {
            loop_id: positions[&round.loop_id],
            round: round.round,
            initial_conflict,
            fresh_conflict: fresh_conflict_index,
            fresh_ci,
        });
    }
    rechecks
}

pub(super) fn retryable_conflict_failure(
    index: usize,
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
) -> bool {
    let Some(round) = nodes[index].round.as_ref() else {
        return false;
    };
    bounded_conflict_rechecks(nodes, positions)
        .iter()
        .any(|route| {
            route.loop_id == positions[&round.loop_id]
                && route.round == round.round
                && (index == route.initial_conflict
                    || index == route.fresh_conflict
                    || index == route.fresh_ci)
        })
}

fn recovery_returns_through_fresh_gates(
    writers: &[usize],
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
) -> bool {
    let terminal_writers: Vec<_> = writers
        .iter()
        .copied()
        .filter(|writer| {
            !writers.iter().any(|descendant| {
                *descendant != *writer
                    && ancestors(nodes, positions, &nodes[*descendant].id).contains(writer)
            })
        })
        .collect();
    !terminal_writers.is_empty()
        && terminal_writers.iter().all(|writer| {
            nodes.iter().enumerate().any(|(publish_index, publish)| {
                if publish.kind != Some(NodeKind::Publish)
                    || publish_source_writer(nodes, positions, publish_index) != Some(*writer)
                {
                    return false;
                }
                let conflicts: Vec<_> = nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, gate)| {
                        matches!(
                            gate.condition.as_ref(),
                            Some(GateCondition::Conflict { .. })
                        ) && gate_targets_node(&gate.condition, &publish.id)
                            && ancestors(nodes, positions, &gate.id).contains(&publish_index)
                    })
                    .map(|(index, _)| index)
                    .collect();
                conflicts.iter().any(|conflict| {
                    nodes.iter().any(|ci| {
                        matches!(ci.condition.as_ref(), Some(GateCondition::Ci { .. }))
                            && gate_targets_node(&ci.condition, &publish.id)
                            && ancestors(nodes, positions, &ci.id).contains(&publish_index)
                            && ancestors(nodes, positions, &ci.id).contains(conflict)
                    })
                })
            })
        })
}

// A generic bounded recovery round may retry its conflict gate without a
// separate on-fail writer. The round's own publication must feed conflict,
// then CI, then its terminal gate; the next round is owned by the loop.
fn bounded_recovery_conflict(
    gate: &PlannedNode,
    publication_index: usize,
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
) -> bool {
    let Some(round) = gate.round.as_ref().filter(|round| round.retry_on) else {
        return false;
    };
    let Some(owner) = positions.get(&round.loop_id).map(|index| &nodes[*index]) else {
        return false;
    };
    let Some(spec) = owner.loop_body.as_ref() else {
        return false;
    };
    let publication = &nodes[publication_index];
    if spec.selected_publication.as_deref()
        != publication.round.as_ref().map(|r| r.node_id.as_str())
        || publication
            .round
            .as_ref()
            .is_none_or(|r| r.loop_id != round.loop_id || r.round != round.round)
        || publish_source_writer(nodes, positions, publication_index).is_none()
    {
        return false;
    }
    nodes.iter().any(|ci| {
        matches!(&ci.condition, Some(GateCondition::Ci { .. }))
            && ci
                .round
                .as_ref()
                .is_some_and(|r| r.loop_id == round.loop_id && r.round == round.round)
            && gate_targets_node(&ci.condition, &publication.id)
            && ancestors(nodes, positions, &ci.id).contains(&positions[&gate.id])
            && nodes.iter().any(|terminal| {
                terminal.round.as_ref().is_some_and(|r| {
                    r.loop_id == round.loop_id && r.round == round.round && r.until
                }) && matches!(&terminal.condition, Some(GateCondition::Verdict { .. }))
                    && ancestors(nodes, positions, &terminal.id).contains(&positions[&ci.id])
            })
    })
}

/// Queue repairs must use their exact published head and retained worktree,
/// then return through hygiene, a new publication, and gates for that head.
/// Compound recovery targets are inspected after expansion.
pub(super) fn validate_conflict_recovery(
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
    queue_enabled: bool,
) -> Result<(), String> {
    if !queue_enabled {
        return Ok(());
    }
    for gate in nodes {
        let is_conflict = matches!(
            gate.condition.as_ref(),
            Some(GateCondition::Conflict { .. })
        );
        let is_ci_repair = matches!(gate.condition.as_ref(), Some(GateCondition::Ci { .. }))
            && gate.on_fail.is_some();
        if !is_conflict && !is_ci_repair {
            continue;
        }
        let published_id = match &gate.condition {
            Some(GateCondition::Conflict {
                target: CiTarget::Node { node, .. },
                ..
            })
            | Some(GateCondition::Ci {
                target: CiTarget::Node { node, .. },
                ..
            }) => node,
            _ => {
                return Err(format!(
                    "node {}: queue conflict and CI repairs require a published node target",
                    gate.id
                ));
            }
        };
        let invalid = || {
            format!(
                "node {}: queue repair must use its published head and return through hygiene, fresh publication, conflict and CI gates",
                gate.id
            )
        };
        let Some(published_index) = positions.get(published_id).copied() else {
            return Err(invalid());
        };
        let published = &nodes[published_index];
        if published.kind != Some(NodeKind::Publish) {
            return Err(invalid());
        }
        let Some(recovery_id) = gate.on_fail.as_deref() else {
            // A bounded loop recheck has no repair edge of its own: a failed
            // conflict blocks the same-round CI outcome and activates retry.
            if is_conflict
                && (bounded_recovery_conflict(gate, published_index, nodes, positions)
                    || bounded_conflict_rechecks(nodes, positions)
                        .iter()
                        .any(|route| route.fresh_conflict == positions[&gate.id]))
            {
                continue;
            }
            return Err(invalid());
        };
        let retained_worktree = published
            .delivery_adoption
            .as_ref()
            .map(|intent| intent.worktree.as_str())
            .or_else(|| {
                publish_source_writer(nodes, positions, published_index)
                    .and_then(|writer| nodes[writer].worktree.as_deref())
            });
        let Some(retained_worktree) = retained_worktree else {
            return Err(invalid());
        };
        let Some(recovery_index) = positions.get(recovery_id).copied() else {
            return Err(invalid());
        };
        let recovery = &nodes[recovery_index];
        let members = recovery_members(recovery_index, nodes, positions);
        let writers: Vec<_> = members
            .iter()
            .copied()
            .filter(|index| plans::writer(&nodes[*index]))
            .collect();
        if writers.is_empty()
            || writers.iter().any(|index| {
                !recovery_source_is_published_head(
                    *index,
                    published_id,
                    nodes,
                    positions,
                    &members,
                    &mut BTreeSet::new(),
                )
            })
            || writers.iter().any(|index| {
                nodes[*index].worktree.as_deref() != Some(retained_worktree)
                    || gate.round.as_ref().is_some_and(|gate_round| {
                        recovery.loop_body.is_none()
                            && nodes[*index].round.as_ref().is_none_or(|round| {
                                round.loop_id != gate_round.loop_id
                                    || round.round != gate_round.round
                            })
                    })
            })
            || !recovery_returns_through_fresh_gates(&writers, nodes, positions)
            || is_conflict
                && gate.round.is_some()
                && !bounded_conflict_rechecks(nodes, positions)
                    .iter()
                    .any(|route| route.initial_conflict == positions[&gate.id])
        {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Checks that need the whole acyclic plan.
pub(super) fn validate_relations(
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
) -> Result<(), String> {
    for node in nodes {
        if let Some(SourceBinding::Head { node: target }) = &node.source {
            let upstream = ancestors(nodes, positions, &node.id);
            let valid = positions.get(target).is_some_and(|index| {
                let source = &nodes[*index];
                upstream.contains(index)
                    && if source.kind == Some(NodeKind::Publish) {
                        source.delivery_adoption.as_ref().is_some_and(|intent| {
                            !plans::writer(node)
                                || node.worktree.as_deref() == Some(intent.worktree.as_str())
                        }) || publish_source_writer(nodes, positions, *index).is_some_and(
                            |writer| {
                                !plans::writer(node)
                                    || (node.worktree.is_some()
                                        && node.worktree == nodes[writer].worktree)
                            },
                        )
                    } else {
                        plans::writer(source)
                            && matches!(
                                source.delivery,
                                Some(DeliveryMode::Pr | DeliveryMode::Push)
                            )
                            && source.continues.is_none()
                    }
            });
            if !valid {
                return Err(format!(
                    "node {}: head source must name an upstream delivery writer or queue publication",
                    node.id
                ));
            }
        }
        if let Some(target) = &node.continues {
            let upstream = ancestors(nodes, positions, &node.id);
            let valid = positions.get(target).is_some_and(|index| {
                let writer = &nodes[*index];
                upstream.contains(index)
                    && plans::writer(writer)
                    && matches!(writer.delivery, Some(DeliveryMode::Pr | DeliveryMode::Push))
                    && writer.continues.is_none()
                    && writer.worktree == node.worktree
            });
            if !valid {
                let candidates: Vec<&str> = upstream
                    .iter()
                    .filter(|index| {
                        let writer = &nodes[**index];
                        plans::writer(writer)
                            && matches!(
                                writer.delivery,
                                Some(DeliveryMode::Pr | DeliveryMode::Push)
                            )
                            && writer.continues.is_none()
                            && writer.worktree == node.worktree
                    })
                    .map(|index| nodes[*index].id.as_str())
                    .collect();
                return Err(format!(
                    "node {}: continues must name an upstream writer that delivers pr or push from the same worktree (got {:?}; expected its expanded id; candidates: {})",
                    node.id,
                    target,
                    if candidates.is_empty() {
                        "none".to_owned()
                    } else {
                        candidates.join(", ")
                    }
                ));
            }
        }
        if let Some(spec) = &node.land {
            let upstream = ancestors(nodes, positions, &node.id);
            let Some(target) = positions.get(&spec.node).copied().filter(|index| {
                let target = &nodes[*index];
                upstream.contains(index)
                    && target.continues.is_none()
                    && (target.kind == Some(NodeKind::Publish)
                        || (plans::writer(target) && target.delivery == Some(DeliveryMode::Pr))
                        || target
                            .loop_body
                            .as_ref()
                            .is_some_and(|body| body.selected_publication.is_some()))
            }) else {
                return Err(format!(
                    "node {}: land must follow a PR writer or queue publication node",
                    node.id
                ));
            };
            let after_target =
                |gate: usize| ancestors(nodes, positions, &nodes[gate].id).contains(&target);
            let gate_after_writer = |ci: bool| {
                upstream.iter().any(|index| {
                    let gate = &nodes[*index];
                    let kind = match &gate.condition {
                        Some(GateCondition::Ci { .. }) => true,
                        Some(GateCondition::Verdict { .. }) => false,
                        _ => return false,
                    };
                    kind == ci && after_target(*index)
                })
            };
            if nodes[target].kind == Some(NodeKind::Publish) {
                let conflicts: Vec<_> = upstream
                    .iter()
                    .copied()
                    .filter(|index| {
                        gate_targets_node(&nodes[*index].condition, &spec.node)
                            && matches!(
                                &nodes[*index].condition,
                                Some(GateCondition::Conflict { .. })
                            )
                            && after_target(*index)
                    })
                    .collect();
                let ci_after_conflict = upstream.iter().any(|index| {
                    gate_targets_node(&nodes[*index].condition, &spec.node)
                        && matches!(&nodes[*index].condition, Some(GateCondition::Ci { .. }))
                        && after_target(*index)
                        && conflicts.iter().any(|conflict| {
                            ancestors(nodes, positions, &nodes[*index].id).contains(conflict)
                        })
                });
                let verdict_after_ci = upstream.iter().any(|index| {
                    matches!(
                        &nodes[*index].condition,
                        Some(GateCondition::Verdict { .. })
                    ) && after_target(*index)
                        && upstream.iter().any(|ci| {
                            matches!(&nodes[*ci].condition, Some(GateCondition::Ci { .. }))
                                && ancestors(nodes, positions, &nodes[*index].id).contains(ci)
                        })
                });
                if conflicts.is_empty() || !ci_after_conflict || !verdict_after_ci {
                    return Err(format!(
                        "node {}: land must follow conflict, CI and verdict gates after its publication node",
                        node.id
                    ));
                }
            } else if let Some(local) = nodes[target]
                .loop_body
                .as_ref()
                .and_then(|body| body.selected_publication.as_deref())
            {
                let publications: Vec<_> = upstream
                    .iter()
                    .copied()
                    .filter(|index| {
                        nodes[*index].kind == Some(NodeKind::Publish)
                            && nodes[*index].round.as_ref().is_some_and(|round| {
                                round.loop_id == spec.node && round.node_id == local
                            })
                    })
                    .collect();
                let gates_valid = !publications.is_empty()
                    && publications.iter().all(|publication| {
                        let publication_id = &nodes[*publication].id;
                        let after_publication = |gate: usize| {
                            ancestors(nodes, positions, &nodes[gate].id).contains(publication)
                        };
                        let conflicts: Vec<_> = upstream
                            .iter()
                            .copied()
                            .filter(|gate| {
                                gate_targets_node(&nodes[*gate].condition, publication_id)
                                    && matches!(
                                        &nodes[*gate].condition,
                                        Some(GateCondition::Conflict { .. })
                                    )
                                    && after_publication(*gate)
                            })
                            .collect();
                        let ci_after_conflict = upstream.iter().any(|gate| {
                            gate_targets_node(&nodes[*gate].condition, publication_id)
                                && matches!(&nodes[*gate].condition, Some(GateCondition::Ci { .. }))
                                && after_publication(*gate)
                                && conflicts.iter().any(|conflict| {
                                    ancestors(nodes, positions, &nodes[*gate].id).contains(conflict)
                                })
                        });
                        let verdict_after_ci = upstream.iter().any(|gate| {
                            matches!(&nodes[*gate].condition, Some(GateCondition::Verdict { .. }))
                                && after_publication(*gate)
                                && upstream.iter().any(|ci| {
                                    matches!(&nodes[*ci].condition, Some(GateCondition::Ci { .. }))
                                        && ancestors(nodes, positions, &nodes[*gate].id)
                                            .contains(ci)
                                })
                        });
                        !conflicts.is_empty() && ci_after_conflict && verdict_after_ci
                    });
                let initial_valid = nodes[target]
                    .loop_body
                    .as_ref()
                    .and_then(|spec| spec.initial_publication.as_deref())
                    .is_none_or(|initial| {
                        let entries: Vec<_> = nodes[target]
                            .depends_on
                            .iter()
                            .filter_map(|dependency| positions.get(dependency).copied())
                            .filter(|index| {
                                nodes[*index].on_fail.as_deref() == Some(nodes[target].id.as_str())
                            })
                            .collect();
                        entries.iter().any(|conflict| {
                            matches!(
                                &nodes[*conflict].condition,
                                Some(GateCondition::Conflict { .. })
                            ) && gate_targets_node(&nodes[*conflict].condition, initial)
                                && entries.iter().any(|ci| {
                                    matches!(&nodes[*ci].condition, Some(GateCondition::Ci { .. }))
                                        && gate_targets_node(&nodes[*ci].condition, initial)
                                        && ancestors(nodes, positions, &nodes[*ci].id)
                                            .contains(conflict)
                                        && entries.iter().any(|verdict| {
                                            matches!(
                                                &nodes[*verdict].condition,
                                                Some(GateCondition::Verdict { .. })
                                            ) && ancestors(nodes, positions, &nodes[*verdict].id)
                                                .contains(ci)
                                        })
                                })
                        })
                    });
                if !gates_valid || !initial_valid {
                    return Err(format!(
                        "node {}: land must follow conflict, CI and verdict gates for every selected publication round",
                        node.id
                    ));
                }
            } else if !gate_after_writer(true) || !gate_after_writer(false) {
                return Err(format!(
                    "node {}: land must follow a verdict gate and a CI gate after its writer",
                    node.id
                ));
            }
        }
    }
    Ok(())
}

/// The bound branch of a continuing writer, from the writer it continues.
pub(super) fn continued_branch(
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Option<String> {
    let target = positions.get(node.continues.as_ref()?)?;
    checkpoint.nodes[*target].outputs.as_ref()?.branch.clone()
}

/// The branch and newest commit delivered by `writer` and the writers that
/// continue it, ordered by settlement.
pub(super) fn delivered_head(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    writer: &str,
) -> Option<(String, String)> {
    delivered_within(plan, checkpoint, writer, |_| true).map(|(branch, head, _)| (branch, head))
}

/// A queue land pins the exact branch and head the publication owner returned.
/// Its PR reference is opaque (it may be a URL), so the landing owner resolves
/// and persists its native PR number using this exact branch/head pair.
fn land_subject(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    target: &str,
) -> Option<(String, String)> {
    let index = *positions.get(target)?;
    if plan.nodes[index].loop_body.is_some() {
        let boundary = checkpoint.nodes.get(index)?;
        if boundary.state != NodeState::Succeeded {
            return None;
        }
        let selected = selected_loop_publication_node(plan, checkpoint, positions, target)?;
        let subject = land_subject(plan, checkpoint, positions, &selected)?;
        let selected_outputs = checkpoint.nodes[positions[&selected]].outputs.as_ref()?;
        let selected_receipt = selected_outputs.publication.as_ref()?;
        let boundary_outputs = boundary.outputs.as_ref()?;
        if boundary_outputs.publication.as_ref() != Some(selected_receipt)
            || boundary_outputs.branch.as_deref() != Some(subject.0.as_str())
            || boundary_outputs.commit.as_deref() != Some(subject.1.as_str())
        {
            return None;
        }
        return Some(subject);
    }
    if plan.nodes[index].kind != Some(NodeKind::Publish) {
        return delivered_head(plan, checkpoint, target);
    }
    let progress = checkpoint.nodes.get(index)?;
    if progress.state != NodeState::Succeeded {
        return None;
    }
    let outputs = progress.outputs.as_ref()?;
    let publication = outputs.publication.as_ref()?;
    let receipt = if let Some(intent) = plan.nodes[index].delivery_adoption.as_ref() {
        let adoption = progress.delivery_adoption()?;
        if !adoption.matches_intent(intent) || adoption.ci_admission.is_none() {
            return None;
        }
        adoption.receipt?
    } else {
        progress
            .delivery_publication
            .as_ref()?
            .receipt
            .as_ref()?
            .clone()
    };
    if publication != &receipt
        || publication.branch.trim().is_empty()
        || publication.pull_request.trim().is_empty()
        || publication.head.parse::<provenant_core::GitOid>().is_err()
        || outputs.branch.as_deref() != Some(publication.branch.as_str())
        || outputs.commit.as_deref() != Some(publication.head.as_str())
        || checkpoint.pull_request.as_deref() != Some(publication.pull_request.as_str())
    {
        return None;
    }
    Some((publication.branch.clone(), publication.head.clone()))
}

fn selected_loop_publication_node(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    target: &str,
) -> Option<String> {
    let boundary_index = *positions.get(target)?;
    let boundary = &plan.nodes[boundary_index];
    let index = repair::selected_publication_index(boundary, checkpoint, plan)?;
    Some(plan.nodes[index].id.clone())
}

fn verified_publication_writer<'a>(
    plan: &'a PlannedGraph,
    checkpoint: &'a GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    target: &str,
) -> Option<(
    &'a PlannedNode,
    &'a NodeProgress,
    &'a DeliveryPublicationProgress,
)> {
    let index = *positions.get(target)?;
    if plan.nodes[index].kind != Some(NodeKind::Publish) {
        return None;
    }
    let (_, published_head) = land_subject(plan, checkpoint, positions, target)?;
    let publication = checkpoint.nodes[index].delivery_publication.as_ref()?;
    if publication.input_head != published_head {
        return None;
    }
    let expected_writer = publish_source_writer(&plan.nodes, positions, index)?;
    let writer_index = *positions.get(&publication.source_node)?;
    if writer_index != expected_writer {
        return None;
    }
    let writer = &plan.nodes[writer_index];
    let progress = &checkpoint.nodes[writer_index];
    if !plans::writer(writer)
        || progress.state != NodeState::Succeeded
        || progress.writer_binding.is_none()
        || !progress.recorded_attempts().any(|attempt| {
            attempt.key == publication.source_key
                && attempt.state == AttemptState::Terminal
                && attempt.attempt_ref.as_deref() == Some(publication.source_attempt.as_str())
        })
    {
        return None;
    }
    Some((writer, progress, publication))
}

/// The writer retained by a completed queue publication, if its receipt and
/// exact source attempt still agree with the checkpoint.
pub(super) fn published_source_writer<'a>(
    plan: &'a PlannedGraph,
    checkpoint: &'a GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    target: &str,
) -> Option<&'a str> {
    verified_publication_writer(plan, checkpoint, positions, target)
        .map(|(writer, _, _)| writer.id.as_str())
}

fn gate_targets_node(condition: &Option<GateCondition>, target: &str) -> bool {
    match condition {
        Some(GateCondition::Conflict {
            target: CiTarget::Node { node, .. },
            ..
        })
        | Some(GateCondition::Ci {
            target: CiTarget::Node { node, .. },
            ..
        }) => node == target,
        _ => false,
    }
}

/// Queue publication evidence must still be the evidence that allowed the
/// graph to reach land. The landing owner independently re-reads the live PR,
/// its mergeability, required checks and current review before merging.
fn queue_landing_gate_error(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    land_node: &str,
    publication_node: &str,
    head: &str,
) -> Option<String> {
    let upstream = ancestors(&plan.nodes, positions, land_node);
    let publication_index = positions.get(publication_node).copied()?;
    let conflict_index = upstream.iter().rev().copied().find(|index| {
        let progress = &checkpoint.nodes[*index];
        plan.nodes[*index].id != publication_node
            && gate_targets_node(&plan.nodes[*index].condition, publication_node)
            && matches!(
                &plan.nodes[*index].condition,
                Some(GateCondition::Conflict { .. })
            )
            && ancestors(&plan.nodes, positions, &plan.nodes[*index].id)
                .contains(&publication_index)
            && progress.state == NodeState::Succeeded
            && progress.gate.as_ref().is_some_and(|gate| gate.passed)
            && progress.conflict.as_ref().is_some_and(|evidence| {
                evidence.state == ConflictState::Clear && evidence.head_sha.as_deref() == Some(head)
            })
    });
    let Some(conflict_index) = conflict_index else {
        return Some(format!(
            "node {land_node}: the published PR lacks a clear conflict observation for head {head}; return it through the conflict gate"
        ));
    };
    let ci_passed = upstream.iter().any(|index| {
        let progress = &checkpoint.nodes[*index];
        gate_targets_node(&plan.nodes[*index].condition, publication_node)
            && matches!(
                &plan.nodes[*index].condition,
                Some(GateCondition::Ci { .. })
            )
            && ancestors(&plan.nodes, positions, &plan.nodes[*index].id).contains(&conflict_index)
            && progress.state == NodeState::Succeeded
            && progress.gate.as_ref().is_some_and(|gate| gate.passed)
            && progress.ci.as_ref().is_some_and(|evidence| {
                evidence.state == CiState::Passed && evidence.head_sha.as_deref() == Some(head)
            })
            && progress
                .outputs
                .as_ref()
                .and_then(|outputs| outputs.commit.as_deref())
                == Some(head)
    });
    (!ci_passed).then(|| {
        format!(
            "node {land_node}: the published PR lacks green required CI for head {head}; return it through CI"
        )
    })
}

/// As [`delivered_head`], counting only the chain nodes `include` accepts.
fn delivered_within(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    writer: &str,
    include: impl Fn(usize) -> bool,
) -> Option<(String, String, Option<AttemptKey>)> {
    plan.nodes
        .iter()
        .zip(&checkpoint.nodes)
        .enumerate()
        .filter(|(index, (node, progress))| {
            (node.id == writer || node.continues.as_deref() == Some(writer))
                && progress.state == NodeState::Succeeded
                && include(*index)
        })
        .filter_map(|(_, (_, progress))| {
            let outputs = progress.outputs.as_ref()?;
            let settled = progress
                .attempts
                .last()
                .map_or(0, |attempt| attempt.settled);
            Some((
                settled,
                outputs.branch.clone()?,
                outputs.commit.clone()?,
                progress.attempts.last().map(|attempt| attempt.key.clone()),
            ))
        })
        .max_by_key(|(settled, _, _, _)| *settled)
        .map(|(_, branch, commit, attempt)| (branch, commit, attempt))
}

/// The head a reviewer judged and the base it read that head against, or
/// `None` for a node that is not a reviewer.
/// The head is its pinned source commit or, without one, the
/// newest head of `writer`'s chain among its own upstream nodes. A review that
/// ran before a continuation therefore never judges the continuation's head.
/// The base is the node's resolved review base or, without one, the graph's
/// bound review or integration base. A finding decision recorded against
/// another head, or without the resolved review base, leaves no subject.
/// A composite boundary has no subject of its own, so its verdict resolves to
/// the member that exported it: a council pinned to an earlier head never
/// vouches for a later one that merely precedes the boundary. Members that read
/// one head against different bases leave no base to carry the review over.
fn reviewed_head(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    reviewer: &str,
    writer: &str,
) -> Option<ReviewedSubject> {
    let index = *positions.get(reviewer)?;
    let node = &plan.nodes[index];
    if let Some(spec) = &node.composite {
        if spec.reduce_verdict {
            let mut subject: Option<ReviewedSubject> = None;
            let mut seen = std::collections::BTreeSet::new();
            for export in &spec.exports {
                if !seen.insert(&export.node) {
                    continue;
                }
                let member = *positions.get(&export.node)?;
                if checkpoint.nodes[member].state != NodeState::Succeeded {
                    continue;
                }
                if plan.nodes[member].access != AccessMode::ReadOnly {
                    return None;
                }
                let reviewed = reviewed_head(plan, checkpoint, positions, &export.node, writer)?;
                subject = match subject {
                    None => Some(reviewed),
                    Some(prior) if prior.head != reviewed.head => return None,
                    Some(prior) => Some(ReviewedSubject {
                        base: prior
                            .base
                            .filter(|base| reviewed.base.as_ref() == Some(base)),
                        head: prior.head,
                    }),
                };
            }
            return subject;
        }
        let producer = spec
            .exports
            .iter()
            .find(|export| export.outputs.contains(&OutputKind::Verdict))?;
        return reviewed_head(plan, checkpoint, positions, &producer.node, writer);
    }
    // Only an agent attempt outside the writer's chain reviews. A gate, loop,
    // decision or land output, such as CI's PASS, is never review evidence.
    if node.condition.is_some()
        || node.loop_body.is_some()
        || node.decide.is_some()
        || node.land.is_some()
        || node.id == writer
        || node.continues.as_deref() == Some(writer)
        || checkpoint.nodes[index].attempts.is_empty()
    {
        return None;
    }
    let progress = &checkpoint.nodes[index];
    let head = match &progress.source_commit {
        Some(commit) => commit.clone(),
        None => chain_head(plan, checkpoint, positions, reviewer, writer)?,
    };
    let graph_review_base = checkpoint
        .bases
        .and_then(|bases| bases.review)
        .map(|base| base.to_string());
    let review_base = progress
        .review_base
        .as_deref()
        .or(graph_review_base.as_deref())
        .or(node.review_base.as_deref());
    for decision in &progress.decisions {
        let decision_base = decision.reviewed_base()?;
        if review_base.is_none_or(|b| provenant_core::GitOid::parse(b).ok() != Some(decision_base))
        {
            return None;
        }
        if provenant_core::GitOid::parse(&head).ok() != Some(decision.reviewed_head()) {
            return None;
        }
    }
    let base = review_base.map(str::to_owned).or_else(|| {
        checkpoint
            .bases
            .and_then(|bases| bases.integration)
            .map(|base| base.to_string())
    });
    Some(ReviewedSubject { head, base })
}

/// What one reviewer judged: a head, read against a base.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReviewedSubject {
    head: String,
    base: Option<String>,
}

/// The newest head of `writer`'s chain among `node`'s upstream nodes, which a
/// `head` source binding pins.
pub(super) fn chain_head(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    node: &str,
    writer: &str,
) -> Option<String> {
    let upstream = ancestors(&plan.nodes, positions, node);
    if let Some(index) = positions.get(writer)
        && plan.nodes[*index].kind == Some(NodeKind::Publish)
    {
        if !upstream.contains(index) {
            return None;
        }
        if plan.nodes[*index].delivery_adoption.is_some() {
            return land_subject(plan, checkpoint, positions, writer).map(|(_, head)| head);
        }
        return verified_publication_writer(plan, checkpoint, positions, writer)
            .and_then(|_| land_subject(plan, checkpoint, positions, writer))
            .map(|(_, head)| head);
    }
    delivered_within(plan, checkpoint, writer, |index| upstream.contains(&index))
        .map(|(_, head, _)| head)
}

/// The attempt that supplied the head selected by the same chain ordering.
pub(super) fn chain_source_attempt(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    node: &str,
    writer: &str,
) -> Option<AttemptKey> {
    let upstream = ancestors(&plan.nodes, positions, node);
    if let Some(index) = positions.get(writer)
        && plan.nodes[*index].kind == Some(NodeKind::Publish)
    {
        if !upstream.contains(index) {
            return None;
        }
        if plan.nodes[*index].delivery_adoption.is_some() {
            return None;
        }
        return verified_publication_writer(plan, checkpoint, positions, writer)
            .map(|(_, _, publication)| publication.source_key.clone());
    }
    delivered_within(plan, checkpoint, writer, |index| upstream.contains(&index))
        .and_then(|(_, _, attempt)| attempt)
}

/// Attempt references of `writer` and of the writers that continue it among
/// `node`'s upstream nodes: every author of the head a `head` binding pins.
pub(super) fn chain_attempts(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    node: &str,
    writer: &str,
) -> Vec<String> {
    let upstream = ancestors(&plan.nodes, positions, node);
    if let Some(publish_index) = positions.get(writer)
        && plan.nodes[*publish_index].kind == Some(NodeKind::Publish)
    {
        if !upstream.contains(publish_index) {
            return Vec::new();
        }
        if let Some(intent) = plan.nodes[*publish_index].delivery_adoption.as_ref() {
            return vec![intent.custody_attempt.clone()];
        }
        let Some((published_writer, _, _)) =
            verified_publication_writer(plan, checkpoint, positions, writer)
        else {
            return Vec::new();
        };
        let Some(writer_index) = positions.get(&published_writer.id).copied() else {
            return Vec::new();
        };
        let Some(root_index) = writer_chain_root(&plan.nodes, positions, writer_index) else {
            return Vec::new();
        };
        let published_ancestors = ancestors(&plan.nodes, positions, writer);
        let mut attempts: Vec<String> = plan
            .nodes
            .iter()
            .zip(&checkpoint.nodes)
            .enumerate()
            .filter(|(index, _)| {
                published_ancestors.contains(index)
                    && continues_from(&plan.nodes, positions, *index, root_index)
            })
            .flat_map(|(_, (_, progress))| progress.recorded_attempts())
            .filter_map(|attempt| attempt.attempt_ref.clone())
            .collect();
        attempts.sort();
        attempts.dedup();
        return attempts;
    }
    let mut attempts: Vec<String> = plan
        .nodes
        .iter()
        .zip(&checkpoint.nodes)
        .enumerate()
        .filter(|(index, (chain, _))| {
            (chain.id == writer || chain.continues.as_deref() == Some(writer))
                && upstream.contains(index)
        })
        .flat_map(|(_, (_, progress))| &progress.attempts)
        .filter_map(|attempt| attempt.attempt_ref.clone())
        .collect();
    attempts.sort();
    attempts.dedup();
    attempts
}

fn continues_from(
    nodes: &[PlannedNode],
    positions: &BTreeMap<String, usize>,
    mut node: usize,
    root: usize,
) -> bool {
    for _ in 0..nodes.len() {
        if node == root {
            return true;
        }
        let Some(parent) = nodes[node].continues.as_deref() else {
            return false;
        };
        let Some(index) = positions.get(parent).copied() else {
            return false;
        };
        node = index;
    }
    false
}

/// Why no passing verdict gate after `writer` accepted `head`, if none did.
/// Landing needs a gate that passed and whose every input reviewed `head`, or
/// reviewed an earlier head that the owner proves `head` carries over: a
/// merge-in from the target or a rebase with an unchanged delta. A failed gate
/// whose `on_fail` reaches the land is not acceptance. A gate replaced with a
/// condition accepting rejection is not approval either.
pub(super) fn acceptance_error<R: AttemptOwner>(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    owner: &mut R,
    land: &str,
    writer: &str,
    (branch, head): (&str, &str),
) -> Result<Option<String>, R::Error> {
    let writer_index = positions.get(writer).copied();
    let mut reviewed = Vec::new();
    for index in ancestors(&plan.nodes, positions, land) {
        let gate = &plan.nodes[index];
        let progress = &checkpoint.nodes[index];
        if !matches!(
            &gate.condition,
            Some(GateCondition::Verdict { expected, .. })
                if matches!(ReviewVerdict::from_token(expected),
                    Some(ReviewVerdict::Approve
                        | ReviewVerdict::ApproveWithNits))
        ) || !writer_index
            .is_some_and(|writer| ancestors(&plan.nodes, positions, &gate.id).contains(&writer))
            || progress.state != NodeState::Succeeded
            || !progress.gate.as_ref().is_some_and(|gate| gate.passed)
        {
            continue;
        }
        let mut covered = !gate.inputs.is_empty();
        let mut subjects = Vec::new();
        for input in &gate.inputs {
            let Some(subject) = reviewed_head(plan, checkpoint, positions, &input.node, writer)
            else {
                covered = false;
                subjects.push("no head".to_owned());
                continue;
            };
            if subject.head == head {
                subjects.push(subject.head);
                continue;
            }
            let coverage = owner.review_coverage(
                branch,
                Reviewed {
                    head: &subject.head,
                    base: subject.base.as_deref(),
                },
                head,
            )?;
            match coverage {
                Some(coverage) if coverage.covers() => subjects.push(subject.head),
                Some(coverage) => {
                    covered = false;
                    subjects.push(format!("{} ({})", subject.head, coverage.reason()));
                }
                None => {
                    covered = false;
                    subjects.push(subject.head);
                }
            }
        }
        if covered {
            return Ok(None);
        }
        reviewed.push(format!("{} passed {}", gate.id, subjects.join(", ")));
    }
    let evidence = if reviewed.is_empty() {
        "no approval verdict gate after the writer passed".to_owned()
    } else {
        reviewed.join("; ")
    };
    Ok(Some(format!(
        "node {land}: no passing verdict gate after {writer} reviewed the land head {head} ({evidence}); route the newest head through review before land"
    )))
}

pub(super) fn session<'a>(spec: &'a LandSpec, plan: &'a PlannedGraph) -> Option<&'a str> {
    spec.session.as_deref().or(plan.decision_session.as_deref())
}

/// The ID of a land node's ask: the sequence number, which moves past a request
/// a cancel closed, and the node's retry generation, which keeps a retried node
/// from finding the request an earlier run left answered.
pub(super) fn ask_id(node_id: &str, sequence: u64, generation: u64) -> String {
    let base = format!("{node_id}.ask-{sequence}");
    match generation {
        0 => base,
        generation => format!("{base}.retry-{generation}"),
    }
}

/// The sequence and retry generation an ask ID names.
pub(super) fn parse(ask_id: &str, node_id: &str) -> Option<(u64, u64)> {
    let identity = ask_id.strip_prefix(node_id)?.strip_prefix(".ask-")?;
    let (sequence, generation) = match identity.split_once(".retry-") {
        Some((sequence, generation)) => {
            (sequence, generation.parse::<u64>().ok().filter(|g| *g > 0)?)
        }
        None => (identity, 0),
    };
    let sequence = sequence.parse::<u64>().ok().filter(|s| *s > 0)?;
    Some((sequence, generation))
}

/// The most closed requests one land node may leave behind before a new raise
/// is refused.
const MAX_LAND_SEQUENCE: u64 = 1_000;

fn source_name(source: Layer) -> &'static str {
    source.as_str()
}

fn tier_name(tier: RiskTier) -> &'static str {
    match tier {
        RiskTier::Routine => "routine",
        RiskTier::Substantial => "substantial",
        RiskTier::Crucial => "crucial",
        RiskTier::Terminal => "terminal",
    }
}
