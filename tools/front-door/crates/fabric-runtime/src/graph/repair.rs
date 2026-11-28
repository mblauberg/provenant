//! Review repair policy over ordinary loops and recorded attempt outputs.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn spec_is_repair(spec: &loops::LoopSpec) -> bool {
    spec.review_repair
}

fn accept_below(node: &PlannedNode, checkpoint: &GraphCheckpoint) -> AcceptBelow {
    node.effective_review_policy
        .as_ref()
        .map(|policy| policy.accept_below.value)
        .unwrap_or_else(|| checkpoint.review_policy().accept_below.value)
}

pub(super) fn is_repair(node: &PlannedNode) -> bool {
    node.loop_body.as_ref().is_some_and(spec_is_repair)
}

pub(super) fn may_report_items(node: &PlannedNode, plan: &PlannedGraph) -> bool {
    repair_writer(node)
        || node.round.as_ref().is_some_and(|round| {
            round.node_id == "fix"
                && plan
                    .nodes
                    .iter()
                    .any(|owner| owner.id == round.loop_id && is_repair(owner))
        })
}

fn ledger_head_at(ledger: &FindingLedger, round: Option<usize>) -> Option<&FindingHead> {
    if let Some(round) = round {
        return ledger
            .heads
            .values()
            .filter(|head| head.round <= round)
            .max_by_key(|head| head.round);
    }
    ledger
        .last_reviewed_head
        .as_deref()
        .and_then(|head| ledger.heads.get(head))
        .or_else(|| ledger.heads.values().max_by_key(|head| head.round))
}

/// Whether a review panel judges the loop's chain: bound as evidence into the
/// loop, or expanded as one of its rounds. Only a panel settles a finding
/// ledger; a chain of plain reviewers keeps its latest review output instead.
fn chain_has_panel(owner_id: &str, plan: &PlannedGraph) -> bool {
    let panel = |node: &PlannedNode| {
        node.composite
            .as_ref()
            .is_some_and(|spec| spec.reduce_verdict)
    };
    let positions: BTreeMap<_, _> = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(index, candidate)| (candidate.id.clone(), index))
        .collect();
    let Some(owner) = plan.nodes.iter().find(|node| node.id == owner_id) else {
        return false;
    };
    let bound = owner.inputs.iter().map(|input| input.node.as_str()).chain(
        owner
            .loop_body
            .iter()
            .flat_map(|body| body.previous_bindings.values().map(String::as_str)),
    );
    plan.nodes.iter().any(|node| {
        panel(node)
            && node
                .round
                .as_ref()
                .is_some_and(|round| round.loop_id == owner_id)
    }) || bound.into_iter().any(|source| {
        review::evidence_dependency_closure(source, plan, &positions)
            .into_iter()
            .any(|index| panel(&plan.nodes[index]))
    })
}

/// The latest review output of a chain with no panel: the newest round's
/// check or synthesis, else the loop's explicitly bound initial review.
fn plain_chain_output(
    owner_id: &str,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
) -> Option<NodeOutputs> {
    if chain_has_panel(owner_id, plan) {
        return None;
    }
    let latest_round = checkpoint
        .nodes
        .iter()
        .filter(|progress| {
            progress.round.as_ref().is_some_and(|round| {
                round.loop_id == owner_id && matches!(round.node_id.as_str(), "synthesis" | "check")
            })
        })
        .filter(|progress| progress.outputs.is_some())
        .max_by_key(|progress| progress.round.as_ref().map(|round| round.round))
        .and_then(|progress| progress.outputs.clone());
    latest_round.or_else(|| {
        let owner = plan.nodes.iter().find(|node| node.id == owner_id)?;
        owner
            .inputs
            .iter()
            .filter(|input| input.outputs.contains(&OutputKind::Findings))
            .filter_map(|input| plan.nodes.iter().position(|node| node.id == input.node))
            .filter_map(|index| checkpoint.nodes[index].outputs.clone())
            .next()
    })
}

fn ledger_output_at(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    round: Option<usize>,
    base: Option<NodeOutputs>,
) -> Option<NodeOutputs> {
    ledger_output_for_owner(plan, checkpoint, &node.id, round, base)
}

fn ledger_output_for_owner(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    owner_id: &str,
    round: Option<usize>,
    mut base: Option<NodeOutputs>,
) -> Option<NodeOutputs> {
    let ledger = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == owner_id)
        .and_then(|progress| progress.finding_ledger.clone())
        .or_else(|| review::initial_chain_ledger(owner_id, plan, checkpoint));
    let Some(ledger) = ledger else {
        // No panel judges this chain: its plain reviewer's output is the evidence.
        return if chain_has_panel(owner_id, plan) {
            None
        } else {
            base
        };
    };
    let head = ledger_head_at(&ledger, round)?;
    let outputs = base.get_or_insert_with(NodeOutputs::default);
    outputs.findings.clone_from(&head.unresolved);
    outputs.findings_available = Some(true);
    outputs.full_artifact_findings = Some(head.unresolved.clone());
    if let Some(verdict) = head.dispositions.get("panel_verdict") {
        outputs.verdict = Some(format!("VERDICT: {verdict}"));
        outputs.verdict_error = None;
    }
    Some(base.unwrap_or_default())
}

/// The full unresolved snapshot for the current review-chain head. An empty
/// vector is meaningful: a complete adjudication resolved every repair item.
pub(super) fn ledger_findings(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
) -> Option<Vec<Finding>> {
    let index = plan
        .nodes
        .iter()
        .position(|candidate| candidate.id == node.id)?;
    let owner_id = review::chain_owner_id(index, plan);
    let stored = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == owner_id)?
        .finding_ledger
        .clone()
        .or_else(|| review::initial_chain_ledger(&owner_id, plan, checkpoint));
    let Some(ledger) = stored else {
        return plain_chain_output(&owner_id, plan, checkpoint).map(|output| output.findings);
    };
    let head = ledger
        .last_reviewed_head
        .as_deref()
        .and_then(|head| ledger.heads.get(head))?;
    Some(head.unresolved.clone())
}

pub(super) fn ledger_available(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
) -> bool {
    ledger_findings(node, plan, checkpoint).is_some()
}

fn initial_outputs(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Vec<NodeOutputs> {
    let mut outputs: Vec<_> = node
        .inputs
        .iter()
        .filter(|input| input.outputs.contains(&OutputKind::Findings))
        .filter_map(|input| positions.get(&input.node))
        .filter_map(|&index| checkpoint.nodes[index].outputs.as_ref())
        .cloned()
        .collect();
    if is_repair(node) {
        let Some(ledger_output) =
            ledger_output_at(node, plan, checkpoint, Some(0), outputs.first().cloned())
        else {
            return Vec::new();
        };
        // The loop's explicit initial binding owns this same chain. Replace
        // the aggregate projection with the complete persisted per-item view.
        outputs.clear();
        outputs.push(ledger_output);
    }
    outputs
}

pub(super) fn clean_start(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> bool {
    if !is_repair(node) {
        return false;
    }
    let outputs = initial_outputs(node, plan, checkpoint, positions);
    !outputs.is_empty()
        && outputs.iter().all(|output| {
            output.findings_available == Some(true)
                && approved(output)
                && !output
                    .findings
                    .iter()
                    .any(|finding| repair_eligible_for(finding, accept_below(node, checkpoint)))
        })
}

fn approved(outputs: &NodeOutputs) -> bool {
    outputs.verdict_error.is_none()
        && outputs
            .verdict
            .as_deref()
            .and_then(provenant_records::workflow::ReviewVerdict::from_token)
            == Some(provenant_records::workflow::ReviewVerdict::Approve)
}

/// No eligible work must not launch an empty repair. A negative or incomplete
/// verdict still needs judgement and must not become a successful clean start.
fn no_repair_work(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> bool {
    let latest_failed_round = checkpoint
        .nodes
        .iter()
        .filter_map(|progress| {
            let round = progress.round.as_ref()?;
            (round.loop_id == node.id && round.until && progress.state == NodeState::Failed)
                .then_some(round.round)
        })
        .max();
    let latest_review = latest_failed_round.and_then(|round| {
        checkpoint
            .nodes
            .iter()
            .filter(|progress| {
                progress.round.as_ref().is_some_and(|member| {
                    member.loop_id == node.id
                        && member.round == round
                        && matches!(member.node_id.as_str(), "synthesis" | "check")
                })
            })
            .max_by_key(|progress| {
                progress
                    .round
                    .as_ref()
                    .map(|member| member.node_id == "synthesis")
            })
            .and_then(|progress| {
                panel_outputs(plan, checkpoint, &progress.id).or(progress.outputs.as_ref())
            })
    });
    let outputs = if let Some(round) = latest_failed_round {
        if is_repair(node) {
            ledger_output_at(node, plan, checkpoint, Some(round), latest_review.cloned())
                .map(|output| vec![output])
                .unwrap_or_default()
        } else {
            latest_review.cloned().into_iter().collect()
        }
    } else {
        initial_outputs(node, plan, checkpoint, positions)
    };
    !outputs.is_empty()
        && outputs.iter().all(|output| {
            output.findings_available == Some(true)
                && output.verdict_error.is_none()
                && output.findings.iter().all(|finding| {
                    finding.triage().is_some()
                        && !repair_eligible_for(finding, accept_below(node, checkpoint))
                })
        })
}

/// A finding its own record has settled: a spec defect routed to the chair, or
/// a status or disposition that ends it. Neither acceptance nor repair reads it.
fn answered(finding: &Finding) -> bool {
    finding.spec_defect()
        || matches!(finding.status.as_str(), "closed" | "resolved" | "fixed")
        || matches!(
            finding.disposition.as_deref(),
            Some("rejected" | "resolved" | "fixed" | "disputed" | "removed-the-case")
        )
}

/// An unresolved blocking severity or blocking class applies on every path.
/// Boundary and data-loss findings require evidence of falsification.
pub(super) fn blocks_acceptance_for(finding: &Finding, threshold: AcceptBelow) -> bool {
    review::blocks_threshold(finding, threshold)
}

/// Whether the finding's severity alone is at or above the default acceptance
/// ceiling. An unknown severity word decodes as high, so it counts. The
/// trigger never changes this: severity applies on every path.
pub(super) fn blocking_severity(finding: &Finding) -> bool {
    review::finding_severity(finding).0 >= Severity::High
}

/// Work a repair writer must take at the exact acceptance threshold. Findings
/// below the threshold are reported and posted; they do not enter repair.
pub(super) fn repair_eligible(finding: &Finding) -> bool {
    repair_eligible_for(finding, AcceptBelow::High)
}

pub(super) fn repair_eligible_for(finding: &Finding, threshold: AcceptBelow) -> bool {
    if finding.boundary_or_data_loss() {
        return !finding.falsified_with_evidence();
    }
    // Triage that rejects a finding, or calls it not worth fixing, keeps it from
    // a writer even at blocking severity; an untriaged blocking finding is work.
    let declined = finding
        .triage()
        .is_some_and(|triage| !(triage.decision == "confirmed" && triage.worth_fixing));
    if answered(finding) || declined {
        return false;
    }
    blocks_acceptance_for(finding, threshold)
}

fn design_defect(finding: &Finding, threshold: AcceptBelow) -> bool {
    repair_eligible_for(finding, threshold)
        && finding.design_defect == Some(true)
        && finding
            .evidence
            .as_deref()
            .is_some_and(|evidence| !evidence.trim().is_empty())
}

fn grows_review(finding: &Finding) -> bool {
    finding.risk_grew == Some(true) || finding.scope_grew == Some(true)
}

/// Construct the host patch that reviews the repaired commit through a fresh
/// resolver result. The patch is idempotent per repair round.
pub(super) fn panel_patch(parent: &str, commit: &str, revision: u64) -> patch::Patch {
    let panel = serde_json::json!({
        "id": "panel", "use": "review-panel", "after": [parent], "repair_panel": true,
        "inputs": [{"node": parent, "outputs": ["findings"]}],
        "source": {"type": "commit", "commit": commit},
        "distinct_from": "dependencies", "distinct_strength": "prefer"
    });
    patch::Patch {
        schema_version: patch::PATCH_SCHEMA_VERSION,
        expected_revision: Some(revision),
        idempotency_key: Some(format!(
            "repair-panel:{}",
            CanonicalV1::file_digest(parent.as_bytes())
        )),
        reason: Some("repair expanded risk or scope".into()),
        nodes: vec![panel],
        ..Default::default()
    }
}

/// One adjudication node resolves every settled split panel for one review
/// chain and pinned head. The patch adds only that node after its evidence
/// panels: no loop, no round, no second panel. An upheld changes-request still
/// needs repair, which the adjudicator declares.
/// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
pub(super) fn split_verdict_patch(
    item: &str,
    panels: &[String],
    head: Option<&str>,
    revision: u64,
    adjudicator_route: &str,
) -> patch::Patch {
    let mut adjudicate = serde_json::json!({
        "id": "adjudicate",
        "kind": "adjudicate",
        "access": "read_only",
        "route": {"alias": adjudicator_route},
        "after": panels,
        "inputs": panels.iter().map(|panel| serde_json::json!({
            "node": panel,
            "outputs": ["verdict", "findings"]
        })).collect::<Vec<_>>(),
        "require_findings": true,
        "prompt": "Read only. The review panels above reported conflicting verdicts on one head. Resolve the split once: weigh every finding against the code and the named invariant in its top-level `class`. Return every input finding in the findings block with its id and invariant preserved and a non-empty top-level disposition for each, then exactly one verdict. Do not re-run a full panel or start another review round. If a blocking finding is upheld, name the invariant it breaks and say that repair is required; an upheld changes-request legitimately needs a repair round, but the split itself does not. Give VERDICT: PASS only when no blocking finding survives adjudication, otherwise VERDICT: FAIL or VERDICT: INCONCLUSIVE."
    });
    if let Some(head) = head {
        adjudicate["source"] = serde_json::json!({"type": "commit", "commit": head});
    }
    patch::Patch {
        schema_version: patch::PATCH_SCHEMA_VERSION,
        expected_revision: Some(revision),
        idempotency_key: Some(split_adjudication_key(item, head)),
        reason: Some("split verdict on one head".into()),
        nodes: vec![adjudicate],
        ..Default::default()
    }
}

fn split_adjudication_key(item: &str, head: Option<&str>) -> String {
    format!(
        "split-adjudicate:{}",
        CanonicalV1::file_digest(format!("{item}\0{}", head.unwrap_or("")).as_bytes())
    )
}

/// The subject is recorded on the panel's launched members, not its composite boundary.
pub(super) fn split_panel_head<'a>(
    parent: &'a str,
    checkpoint: &'a GraphCheckpoint,
) -> Result<Option<&'a str>, &'static str> {
    let mut pending = vec![parent];
    let mut head = None;
    while let Some(id) = pending.pop() {
        if let Some(progress) = checkpoint.nodes.iter().find(|node| node.id == id) {
            pending.extend(progress.children.iter().map(String::as_str));
            if let Some(commit) = progress.source_commit.as_deref() {
                if head.is_some_and(|previous| previous != commit) {
                    return Err("split panel members reviewed different heads");
                }
                head = Some(commit);
            }
        }
    }
    Ok(head)
}

/// Locate an adjudicator through its persisted patch parent and pinned source.
pub(super) fn panel_adjudicator<'a>(
    item: &str,
    head: Option<&str>,
    checkpoint: &'a GraphCheckpoint,
) -> Option<&'a GraphNode> {
    let key = split_adjudication_key(item, head);
    let original = checkpoint
        .expansions
        .iter()
        .find(|expansion| {
            expansion
                .patch
                .as_ref()
                .and_then(|patch| patch.key.as_deref())
                == Some(key.as_str())
        })
        .and_then(|expansion| {
            expansion.nodes.iter().find(|node| {
                node.kind == Some(NodeKind::Adjudicate)
                    && match (&node.source, head) {
                        (Some(SourceBinding::Commit { commit }), Some(head)) => commit == head,
                        (None, None) => true,
                        _ => false,
                    }
            })
        })?;
    let id = original.id.as_str();
    checkpoint
        .expansions
        .iter()
        .rev()
        .filter_map(|expansion| expansion.patch.as_ref())
        .find_map(|record| record.replaced.iter().find(|node| node.id == id))
        .or(Some(original))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SplitAdjudication {
    pub(super) item: String,
    pub(super) parent: String,
    pub(super) panels: Vec<String>,
    pub(super) head: Option<String>,
    pub(super) revision: u64,
    /// Existing stable node to continue with the unioned panel evidence.
    pub(super) existing_node_id: Option<String>,
}

/// A panel can synthesise its verdict after a nested lens group split.
fn panel_has_split(parent: &NodeProgress, checkpoint: &GraphCheckpoint) -> bool {
    let mut pending = vec![parent];
    while let Some(node) = pending.pop() {
        if node.state == NodeState::Succeeded
            && node.outputs.as_ref().is_some_and(|outputs| {
                outputs
                    .warnings
                    .iter()
                    .any(|warning| warning.starts_with("review_split_verdict:"))
            })
        {
            return true;
        }
        pending.extend(
            node.children
                .iter()
                .filter_map(|id| checkpoint.nodes.iter().find(|child| child.id == *id)),
        );
    }
    false
}

fn panel_adjudication_evidence_complete(progress: &NodeProgress) -> bool {
    progress.state == NodeState::Succeeded
        && progress.outputs.as_ref().is_some_and(|outputs| {
            !outputs.partial
                && outputs.findings_available == Some(true)
                && outputs
                    .verdict
                    .as_deref()
                    .and_then(provenant_records::workflow::ReviewVerdict::from_token)
                    .is_some()
        })
}

/// A gate only adjudicates split panels whose verdicts it consumes.
pub(super) fn split_adjudication(
    gate: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
) -> Result<Option<SplitAdjudication>, &'static str> {
    let positions: BTreeMap<_, _> = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), index))
        .collect();
    let mut candidates = BTreeSet::new();
    for input in &gate.inputs {
        let closure = review::evidence_dependency_closure(&input.node, plan, &positions);
        let panels: Vec<_> = closure
            .iter()
            .filter_map(|index| {
                let node = &plan.nodes[*index];
                node.composite
                    .as_ref()
                    .is_some_and(|composite| composite.reduce_verdict)
                    .then_some(node.id.clone())
            })
            .collect();
        let outermost: Vec<_> = panels
            .iter()
            .filter(|panel| {
                panels
                    .iter()
                    .all(|other| other == *panel || !panel.starts_with(&format!("{other}--")))
            })
            .cloned()
            .collect();
        if outermost.is_empty() {
            candidates.insert(input.node.clone());
        } else {
            candidates.extend(outermost);
        }
    }
    let mut split_panels: Vec<(String, String, Option<String>)> = Vec::new();
    for id in candidates {
        let candidate = panel_expansion(checkpoint, &id)
            .and_then(panel_root)
            .map_or(id.as_str(), |root| root.id.as_str());
        let Some(parent) = checkpoint.nodes.iter().find(|node| node.id == candidate) else {
            continue;
        };
        if parent.state != NodeState::Succeeded || !panel_has_split(parent, checkpoint) {
            continue;
        }
        if !panel_adjudication_evidence_complete(parent) {
            return Err("split panel has partial or incomplete verdict/findings evidence");
        }
        let head = split_panel_head(&parent.id, checkpoint)?;
        let item = review_item_for_panel(&parent.id, plan);
        let panel = (item, parent.id.clone(), head.map(str::to_owned));
        if !split_panels.contains(&panel) {
            split_panels.push(panel);
        }
    }
    for (item, parent, head) in &split_panels {
        let mut panels: Vec<_> = split_panels
            .iter()
            .filter(|(candidate_item, _, candidate_head)| {
                candidate_item == item && candidate_head == head
            })
            .map(|(_, panel, _)| panel.clone())
            .collect();
        panels.sort();
        panels.dedup();
        if let Some(adjudicator) = panel_adjudicator(item, head.as_deref(), checkpoint) {
            let mut union: Vec<_> = adjudicator
                .inputs
                .iter()
                .map(|input| input.node.clone())
                .collect();
            union.extend(panels);
            union.sort();
            union.dedup();
            for panel in &union {
                let progress = checkpoint
                    .nodes
                    .iter()
                    .find(|progress| progress.id == *panel)
                    .ok_or("split adjudicator input panel is unavailable")?;
                if !panel_adjudication_evidence_complete(progress) {
                    return Err(
                        "split adjudicator input panel lacks complete verdict/findings evidence",
                    );
                }
            }
            let missing = union
                .iter()
                .any(|panel| !adjudicator.inputs.iter().any(|input| input.node == *panel));
            let settled = checkpoint
                .nodes
                .iter()
                .find(|progress| progress.id == adjudicator.id)
                .is_some_and(|progress| progress.state == NodeState::Succeeded);
            if missing || !settled {
                let parent = adjudicator
                    .parent_node
                    .clone()
                    .unwrap_or_else(|| parent.clone());
                return Ok(Some(SplitAdjudication {
                    item: item.clone(),
                    parent,
                    panels: union,
                    head: head.clone(),
                    revision: patch::revision(checkpoint),
                    existing_node_id: Some(adjudicator.id.clone()),
                }));
            }
            continue;
        }
        return Ok(Some(SplitAdjudication {
            item: item.clone(),
            parent: parent.clone(),
            panels,
            head: head.clone(),
            revision: patch::revision(checkpoint),
            existing_node_id: None,
        }));
    }
    Ok(None)
}

pub(super) fn review_item_for_panel(panel: &str, plan: &PlannedGraph) -> String {
    let first_review_leaf = plan.nodes.iter().position(|node| {
        (node.id == panel || node.id.starts_with(&format!("{panel}--")))
            && node.composite.is_none()
            && (node.review_lens.is_some() || node.review_behaviour.is_some())
    });
    let exact_index = plan.nodes.iter().position(|node| node.id == panel);
    let ancestor_index = plan
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| panel.starts_with(&format!("{}--", node.id)))
        .max_by_key(|(_, node)| node.id.len())
        .map(|(index, _)| index);
    let first_descendant = plan
        .nodes
        .iter()
        .position(|node| node.id.starts_with(&format!("{panel}--")));
    first_review_leaf
        .or(exact_index)
        .or(ancestor_index)
        .or(first_descendant)
        .map(|index| review::chain_owner_id(index, plan))
        .filter(|owner| !owner.is_empty())
        .unwrap_or_else(|| panel.to_owned())
}

pub(super) fn replan_source<'a>(
    node: &PlannedNode,
    checkpoint: &'a GraphCheckpoint,
) -> Option<(&'a str, &'a str)> {
    let round = node.round.as_ref()?;
    let member = |name: &str| {
        checkpoint.nodes.iter().find(|progress| {
            progress.round.as_ref().is_some_and(|item| {
                item.loop_id == round.loop_id && item.round == round.round && item.node_id == name
            })
        })
    };
    let parent = member("synthesis")
        .filter(|progress| progress.state == NodeState::Succeeded && progress.outputs.is_some())
        .or_else(|| {
            member("check").filter(|progress| {
                progress.state == NodeState::Succeeded && progress.outputs.is_some()
            })
        })?;
    let commit = member("fix")?.outputs.as_ref()?.commit.as_deref()?;
    Some((&parent.id, commit))
}

/// A growth observation is reusable only for the exact commits measured.
pub(super) fn cached_growth(progress: &NodeProgress, before: &str, after: &str) -> Option<bool> {
    let cache = progress.repair_growth.as_ref()?;
    (cache.before == before && cache.after == after).then_some(cache.grew)
}

pub(super) fn repair_commits<'a>(
    node: &PlannedNode,
    checkpoint: &'a GraphCheckpoint,
) -> Option<(&'a str, &'a str)> {
    let round = node.round.as_ref()?;
    let fix = checkpoint.nodes.iter().find(|progress| {
        progress.round.as_ref().is_some_and(|item| {
            item.loop_id == round.loop_id && item.round == round.round && item.node_id == "fix"
        })
    })?;
    Some((
        &fix.writer_binding.as_ref()?.head,
        fix.outputs.as_ref()?.commit.as_deref()?,
    ))
}

pub(super) fn is_repair_gate(
    node: &PlannedNode,
    plan: &PlannedGraph,
    positions: &BTreeMap<String, usize>,
) -> bool {
    node.round.as_ref().is_some_and(|round| {
        round.until
            && matches!(node.condition, Some(GateCondition::Verdict { .. }))
            && positions
                .get(&round.loop_id)
                .is_some_and(|&index| is_repair(&plan.nodes[index]))
    })
}

/// Every active delta lens must report a valid risk assessment before its
/// verdict can establish that the repair did not expand risk.
pub(super) fn risk_assessment_missing(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
) -> bool {
    let Some(round) = &node.round else {
        return false;
    };
    plan.nodes.iter().enumerate().any(|(index, member)| {
        member
            .round
            .as_ref()
            .is_some_and(|item| item.loop_id == round.loop_id && item.round == round.round)
            && member.repair_risk_assessment
            && checkpoint.nodes[index].state != NodeState::Skipped
            && checkpoint.nodes[index]
                .outputs
                .as_ref()
                .is_none_or(|output| output.risk_tier.is_none())
    })
}

pub(super) fn panel_expansion<'a>(
    checkpoint: &'a GraphCheckpoint,
    parent: &str,
) -> Option<&'a Expansion> {
    checkpoint.expansions.iter().find(|expansion| {
        expansion.patch.as_ref().is_some_and(|record| {
            record.parent.as_deref() == Some(parent)
                && record.reason.as_deref() == Some("repair expanded risk or scope")
        })
    })
}

/// A prior immutable panel can certify only its pinned repaired commit.
pub(super) fn panel_reviews_commit(expansion: &Expansion, commit: &str) -> bool {
    let mut leaves = expansion
        .nodes
        .iter()
        .filter(|node| node.composite.is_none())
        .peekable();
    leaves.peek().is_some()
        && leaves.all(|node| {
            matches!(&node.source,
        Some(SourceBinding::Commit { commit: subject }) if subject == commit)
        })
}

pub(super) fn panel_pending(checkpoint: &GraphCheckpoint, expansion: &Expansion) -> bool {
    expansion.nodes.iter().any(|member| {
        checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == member.id)
            .is_some_and(|progress| !progress.state.settled())
    })
}

/// A fresh panel supersedes delta findings for the next repair attempt.
/// Preserve both immutable review receipts in the checkpoint.
pub(super) fn panel_outputs<'a>(
    plan: &PlannedGraph,
    checkpoint: &'a GraphCheckpoint,
    parent: &str,
) -> Option<&'a NodeOutputs> {
    let expansion = panel_expansion(checkpoint, parent)?;
    if panel_pending(checkpoint, expansion) {
        return None;
    }
    let root = panel_root(expansion)?;
    let head = split_panel_head(&root.id, checkpoint).ok()?;
    let root_progress = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == root.id)?;
    let progress = if panel_has_split(root_progress, checkpoint) {
        let item = review_item_for_panel(&root.id, plan);
        if let Some(adjudicator) = panel_adjudicator(&item, head, checkpoint) {
            if !adjudicator.inputs.iter().any(|input| input.node == root.id) {
                return None;
            }
            checkpoint.nodes.iter().find(|progress| {
                progress.id == adjudicator.id && progress.state == NodeState::Succeeded
            })?
        } else {
            root_progress
        }
    } else {
        root_progress
    };
    progress.outputs.as_ref()
}

fn panel_root(expansion: &Expansion) -> Option<&GraphNode> {
    expansion.nodes.iter().find(|candidate| {
        candidate.composite.is_some()
            && expansion.nodes.iter().all(|member| {
                member.id == candidate.id || member.id.starts_with(&format!("{}--", candidate.id))
            })
    })
}

/// A failed expanded panel is recovered by its enclosing bounded repair loop.
pub(super) fn handled_panel(checkpoint: &GraphCheckpoint, node: &NodeProgress) -> bool {
    checkpoint.expansions.iter().any(|expansion| {
        expansion.nodes.iter().any(|member| member.id == node.id)
            && expansion.patch.as_ref().is_some_and(|patch| {
                patch.reason.as_deref() == Some("repair expanded risk or scope")
                    && patch
                        .parent
                        .as_ref()
                        .and_then(|parent| {
                            checkpoint
                                .nodes
                                .iter()
                                .find(|progress| &progress.id == parent)
                        })
                        .and_then(|parent| parent.round.as_ref())
                        .and_then(|round| {
                            checkpoint
                                .nodes
                                .iter()
                                .find(|progress| progress.id == round.loop_id)
                        })
                        .is_some_and(|owner| checkpoint.node_successful(owner))
            })
    })
}

pub(super) fn panel_failure(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    expansion: &Expansion,
) -> Option<String> {
    let Some(root) = panel_root(expansion) else {
        return Some("replanned panel boundary is missing".into());
    };
    let Some(progress) = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == root.id)
    else {
        return Some("replanned panel boundary is missing".into());
    };
    let Some(outputs) = panel_outputs(plan, checkpoint, &expansion.source) else {
        return Some("replanned panel has no complete output".into());
    };
    let ledger_owner = expansion
        .patch
        .as_ref()
        .and_then(|patch| patch.parent.as_deref())
        .and_then(|parent| {
            checkpoint
                .nodes
                .iter()
                .find(|progress| progress.id == parent)
        })
        .and_then(|progress| progress.round.as_ref().map(|round| round.loop_id.clone()));
    let Some(ledger_owner) = ledger_owner else {
        return Some("replanned panel has no finding-ledger owner".into());
    };
    let Some(output) =
        ledger_output_for_owner(plan, checkpoint, &ledger_owner, None, Some(outputs.clone()))
    else {
        return Some("replanned panel has no complete finding ledger".into());
    };
    let threshold = plan
        .nodes
        .iter()
        .find(|node| node.id == root.id)
        .map(|node| accept_below(node, checkpoint))
        .unwrap_or_else(|| checkpoint.review_policy().accept_below.value);
    if progress.state != NodeState::Succeeded
        || output.findings_available != Some(true)
        || !approved(&output)
        || output
            .findings
            .iter()
            .any(|finding| blocks_acceptance_for(finding, threshold))
    {
        return Some("replanned panel did not pass".into());
    }
    None
}

/// Once the panel expands, later repair commits need that broader coverage too.
/// Re-resolve it at each head, including lenses absent from the initial template.
fn prior_panel(node: &PlannedNode, checkpoint: &GraphCheckpoint) -> bool {
    let Some(round) = &node.round else {
        return false;
    };
    checkpoint.expansions.iter().any(|expansion| {
        expansion.patch.as_ref().is_some_and(|patch| {
            patch.reason.as_deref() == Some("repair expanded risk or scope")
                && patch
                    .parent
                    .as_ref()
                    .and_then(|parent| {
                        checkpoint
                            .nodes
                            .iter()
                            .find(|progress| &progress.id == parent)
                    })
                    .and_then(|parent| parent.round.as_ref())
                    .is_some_and(|previous| {
                        previous.loop_id == round.loop_id && previous.round < round.round
                    })
        })
    })
}

/// A repair cannot be accepted on the old panel after a reviewer reports
/// expanded risk or scope. Preserve the evidence for the recovery path.
pub(super) fn growth_in_round(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> bool {
    let Some(round) = &node.round else {
        return false;
    };
    if !is_repair_gate(node, plan, positions) {
        return false;
    }
    if prior_panel(node, checkpoint) {
        return true;
    }
    let rank = |tier: &str| match tier {
        "routine" => Some(0),
        "substantial" => Some(1),
        "crucial" => Some(2),
        "terminal" => Some(3),
        _ => None,
    };
    let baseline = node
        .context
        .as_ref()
        .and_then(|context| context.risk_tier.as_deref())
        .or_else(|| {
            plan.context
                .as_ref()
                .and_then(|context| context.risk_tier.as_deref())
        })
        .and_then(rank);
    checkpoint.nodes.iter().any(|progress| {
        progress.round.as_ref().is_some_and(|member| {
            member.loop_id == round.loop_id
                && member.round == round.round
                && (matches!(member.node_id.as_str(), "check" | "synthesis")
                    || member.node_id.starts_with("check--"))
        }) && progress.outputs.as_ref().is_some_and(|output| {
            output.findings.iter().any(grows_review)
                || output
                    .risk_tier
                    .as_ref()
                    .map(|tier| match tier {
                        provenant_records::workflow::RiskTier::Routine => 0,
                        provenant_records::workflow::RiskTier::Substantial => 1,
                        provenant_records::workflow::RiskTier::Crucial => 2,
                        provenant_records::workflow::RiskTier::Terminal => 3,
                    })
                    .is_some_and(|current| baseline.is_none_or(|baseline| current > baseline))
        })
    })
}

fn same_finding(left: &Finding, right: &Finding) -> bool {
    left.id.rsplit(':').next() == right.id.rsplit(':').next()
        && left.file == right.file
        && left.lens == right.lens
}

/// Two distinct findings that name the same invariant are one shared flaw,
/// whether or not a reviewer remembered to set the boolean. The named class is
/// the cause the design path keys on, so a repeated invariant routes to design
/// even when the boolean was omitted.
/// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
fn shared_flaw(findings: &[&Finding]) -> bool {
    if findings.iter().any(|finding| {
        finding
            .triage
            .as_ref()
            .is_some_and(|triage| triage.shared_flaw)
    }) {
        return true;
    }
    let mut owners: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for finding in findings {
        let Some(invariant) = finding.named_invariant() else {
            continue;
        };
        if owners
            .get(invariant)
            .is_some_and(|owner| *owner != finding.id.as_str())
        {
            return true;
        }
        owners.entry(invariant).or_insert(finding.id.as_str());
    }
    false
}

fn design_cannot_hold(outputs: &NodeOutputs) -> bool {
    outputs.repair_items.as_ref().is_some_and(|items| {
        items
            .iter()
            .any(|item| item.status == RepairItemStatus::DesignCannotHold)
    })
}

/// Default repair-cycle budget of the review-repair composites.
pub(super) const DEFAULT_REPAIR_CYCLES: usize = 3;

/// Checked shared flaws and writer design failures take the principal path;
/// within the default budget the third review requests design judgement
/// before another writer starts.
pub(super) fn escalation(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Option<&'static str> {
    if !is_repair(node) {
        return None;
    }
    let initial_findings: Vec<_> = initial_outputs(node, plan, checkpoint, positions)
        .into_iter()
        .flat_map(|outputs| outputs.findings)
        .filter(|finding| repair_eligible_for(finding, accept_below(node, checkpoint)))
        .collect();
    let initial: Vec<_> = initial_findings.iter().collect();
    if shared_flaw(&initial) {
        return Some("checked findings share a design flaw");
    }
    if checkpoint.nodes.iter().any(|progress| {
        progress
            .round
            .as_ref()
            .is_some_and(|round| round.loop_id == node.id)
            && progress.outputs.as_ref().is_some_and(design_cannot_hold)
    }) {
        return Some("writer reports design cannot hold");
    }
    if initial
        .iter()
        .any(|finding| design_defect(finding, accept_below(node, checkpoint)))
    {
        return Some("concrete design defect");
    }
    let mut prior: Vec<Finding> = initial.into_iter().cloned().collect();
    let budget = node.loop_body.as_ref().map_or(0, |spec| spec.max_rounds);
    // The third-review judgement applies within the default budget; a chair
    // who raised the budget has already authorised further rounds, and
    // exhaustion still reaches the principal through `on_fail`.
    let third_review_judgement = budget <= DEFAULT_REPAIR_CYCLES;
    for round in 1..=budget {
        let selected = |name: &str| {
            checkpoint.nodes.iter().find(|progress| {
                progress.round.as_ref().is_some_and(|member| {
                    member.loop_id == node.id && member.round == round && member.node_id == name
                })
            })
        };
        let Some(check) = selected("synthesis").or_else(|| selected("check")) else {
            break;
        };
        if !check.state.settled() || selected("accepted").is_some_and(|gate| !gate.state.settled())
        {
            break;
        }
        let panel_output = panel_outputs(plan, checkpoint, &check.id)
            .or(check.outputs.as_ref())
            .cloned();
        let Some(outputs) = ledger_output_at(node, plan, checkpoint, Some(round), panel_output)
        else {
            continue;
        };
        if panel_expansion(checkpoint, &check.id).is_none()
            && outputs.findings.iter().any(grows_review)
        {
            return Some("repair expanded risk or scope; full panel replan required");
        }
        let current: Vec<_> = outputs
            .findings
            .iter()
            .filter(|finding| repair_eligible_for(finding, accept_below(node, checkpoint)))
            .collect();
        if shared_flaw(&current) {
            return Some("checked findings share a design flaw");
        }
        if third_review_judgement && round >= 2 && !current.is_empty() {
            return Some("third review round requires design judgement");
        }
        if current
            .iter()
            .any(|finding| design_defect(finding, accept_below(node, checkpoint)))
        {
            return Some("concrete design defect");
        }
        if current
            .iter()
            .any(|finding| prior.iter().any(|previous| same_finding(finding, previous)))
        {
            return Some("recurring finding after repair");
        }
        prior = current.into_iter().cloned().collect();
    }
    None
}

pub(super) fn inactive_round(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> bool {
    let Some(round) = &node.round else {
        return false;
    };
    let Some(&owner) = positions.get(&round.loop_id) else {
        return false;
    };
    let owner = &plan.nodes[owner];
    if !is_repair(owner) {
        return false;
    }
    if clean_start(owner, plan, checkpoint, positions)
        || no_repair_work(owner, plan, checkpoint, positions)
    {
        return true;
    }
    if escalation(owner, plan, checkpoint, positions).is_none() {
        return false;
    }
    // A writer's design failure ends this round before launching any reviewers.
    if checkpoint.nodes.iter().any(|progress| {
        progress
            .round
            .as_ref()
            .is_some_and(|member| member.loop_id == round.loop_id)
            && progress.outputs.as_ref().is_some_and(design_cannot_hold)
    }) {
        return checkpoint.nodes[positions[&node.id]].attempts.is_empty();
    }
    // Only later unstarted rounds are inactive; preserve the round producing
    // the escalation evidence and every attempt already launched.
    !checkpoint.nodes.iter().any(|progress| {
        progress
            .round
            .as_ref()
            .is_some_and(|member| member.loop_id == round.loop_id && member.round == round.round)
            && (!progress.attempts.is_empty()
                || matches!(progress.state, NodeState::Succeeded | NodeState::Failed))
    })
}

/// A delta reviewer is inactive only when complete findings identify other
/// lenses. Unknown lens metadata keeps the reviewer active.
pub(super) fn inactive_lens(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> bool {
    let Some(lens) = node.repair_lens.as_deref() else {
        return false;
    };
    if dependency_state(node, plan, checkpoint, positions).is_some() {
        return false;
    }
    let Some(round) = node.round.as_ref() else {
        return false;
    };
    if round.round == 0 {
        return false;
    }
    let Some(owner) = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == round.loop_id)
    else {
        return false;
    };
    let Some(ledger) = owner.finding_ledger.as_ref() else {
        return false;
    };
    let Some(progress_index) = positions.get(&node.id).copied() else {
        return false;
    };
    let Some(ReviewRoundScope::Delta { base, head }) =
        checkpoint.nodes[progress_index].review_round_scope.as_ref()
    else {
        // A missing or Full scope means host ancestry was not proved for a
        // delta, so every configured lens must run.
        return false;
    };
    if schedule::pinned_review_head(
        node,
        &checkpoint.nodes[progress_index],
        checkpoint,
        positions,
    )
    .as_deref()
        != Some(head.as_str())
        || ledger.last_reviewed_head.as_deref() != Some(base.as_str())
    {
        return false;
    }
    let Some(previous) = ledger.heads.get(base) else {
        return false;
    };
    if !review::delta_attributable(previous) {
        // Unknown attribution, or an invariant whose only covering lens was
        // unavailable, keeps every configured lens active.
        return false;
    }
    // Risk and scope growth require the full host-selected panel even when
    // their finding metadata otherwise points at one lens.
    if previous.findings.iter().any(grows_review) {
        return false;
    }
    !previous
        .unresolved
        .iter()
        .any(|finding| review::ledger_unresolved(finding) && finding.lens.as_deref() == Some(lens))
}

pub(super) fn intentionally_skipped_lens(
    index: usize,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
) -> bool {
    checkpoint.nodes[index].state == NodeState::Skipped && plan.nodes[index].repair_lens.is_some()
}

/// Escalation carries the latest complete qualified findings to the principal;
/// the failed loop verdict remains failed until its recovery path completes.
pub(super) fn terminal_outputs(
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    plan: &PlannedGraph,
    passed: bool,
) -> NodeOutputs {
    let mut outputs = gate_outputs(passed);
    if passed {
        if let Some(publication) = selected_publication(node, checkpoint, plan) {
            outputs.publication = Some(publication.clone());
            outputs.branch = Some(publication.branch);
            outputs.commit = Some(publication.head);
        }
        return outputs;
    }
    if !is_repair(node) {
        return outputs;
    }
    let latest_review = checkpoint
        .nodes
        .iter()
        .filter(|progress| {
            progress.round.as_ref().is_some_and(|round| {
                round.loop_id == node.id && matches!(round.node_id.as_str(), "synthesis" | "check")
            })
        })
        .filter(|progress| progress.outputs.is_some())
        .max_by_key(|progress| {
            progress
                .round
                .as_ref()
                .map(|round| (round.round, round.node_id == "synthesis"))
        });
    let latest = latest_review
        .and_then(|progress| {
            let round = progress.round.as_ref().map(|round| round.round);
            let output = panel_outputs(plan, checkpoint, &progress.id)
                .or(progress.outputs.as_ref())
                .cloned();
            if is_repair(node) {
                ledger_output_at(node, plan, checkpoint, round, output)
            } else {
                output
            }
        })
        .or_else(|| {
            initial_outputs(node, plan, checkpoint, positions)
                .last()
                .cloned()
        });
    if let Some(latest) = latest {
        outputs.findings.clone_from(&latest.findings);
        outputs.findings_available = latest.findings_available;
        outputs.warnings.clone_from(&latest.warnings);
    }
    // The principal needs the writer's explanation and structural alternative,
    // including when this round ended before any reviewer could run.
    if let Some(writer) = checkpoint
        .nodes
        .iter()
        .filter(|progress| {
            progress
                .round
                .as_ref()
                .is_some_and(|round| round.loop_id == node.id)
        })
        .filter_map(|progress| progress.outputs.as_ref())
        .find(|output| design_cannot_hold(output))
    {
        outputs.result.clone_from(&writer.result);
        outputs.repair_items.clone_from(&writer.repair_items);
    }
    outputs
}

pub(super) fn selected_publication_index(
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    plan: &PlannedGraph,
) -> Option<usize> {
    let spec = node.loop_body.as_ref()?;
    let local = spec.selected_publication.as_deref()?;
    let passed_round = checkpoint
        .nodes
        .iter()
        .filter_map(|progress| {
            let round = progress.round.as_ref()?;
            (round.loop_id == node.id && round.until && progress.state == NodeState::Succeeded)
                .then_some(round.round)
        })
        .max();
    if let Some(round_number) = passed_round {
        return plan
            .nodes
            .iter()
            .zip(&checkpoint.nodes)
            .position(|(member, progress)| {
                member.round.as_ref().is_some_and(|round| {
                    round.loop_id == node.id
                        && round.round == round_number
                        && round.node_id == local
                }) && progress.state == NodeState::Succeeded
            });
    }
    let initial = spec.initial_publication.as_deref()?;
    let mut entry_gate = false;
    for dependency in &node.depends_on {
        let index = plan
            .nodes
            .iter()
            .position(|candidate| candidate.id == *dependency)?;
        if plan.nodes[index].round.is_some() {
            continue;
        }
        if checkpoint.nodes[index].state != NodeState::Succeeded {
            return None;
        }
        entry_gate |= plan.nodes[index].on_fail.as_deref() == Some(node.id.as_str());
    }
    if !entry_gate
        || checkpoint.nodes.iter().any(|progress| {
            progress
                .round
                .as_ref()
                .is_some_and(|round| round.loop_id == node.id)
                && progress.state != NodeState::Skipped
        })
    {
        return None;
    }
    plan.nodes
        .iter()
        .zip(&checkpoint.nodes)
        .position(|(member, progress)| {
            member.id == initial && progress.state == NodeState::Succeeded
        })
}

pub(super) fn selected_publication(
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    plan: &PlannedGraph,
) -> Option<DeliveryPublication> {
    let index = selected_publication_index(node, checkpoint, plan)?;
    let member = &plan.nodes[index];
    let progress = &checkpoint.nodes[index];
    if member.kind != Some(NodeKind::Publish) {
        return None;
    }
    let receipt = if let Some(intent) = member.delivery_adoption.as_ref() {
        let adoption = progress.delivery_adoption()?;
        if !adoption.matches_intent(intent) || adoption.ci_admission.is_none() {
            return None;
        }
        adoption.receipt?
    } else {
        progress.delivery_publication.as_ref()?.receipt.clone()?
    };
    let outputs = progress.outputs.as_ref()?;
    (outputs.publication.as_ref() == Some(&receipt)
        && outputs.branch.as_deref() == Some(receipt.branch.as_str())
        && outputs.commit.as_deref() == Some(receipt.head.as_str())
        && receipt.head.parse::<provenant_core::GitOid>().is_ok()
        && !receipt.branch.trim().is_empty()
        && !receipt.pull_request.trim().is_empty()
        && checkpoint.pull_request.as_deref() == Some(receipt.pull_request.as_str()))
    .then_some(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Acceptance at the default threshold.
    fn blocks_acceptance(finding: &Finding) -> bool {
        blocks_acceptance_for(finding, AcceptBelow::High)
    }
    fn finding(id: &str, lens: &str) -> Finding {
        serde_json::from_value(serde_json::json!({"id":id,"lens":lens,"severity":"high", "file":"a.rs", "summary":"constraint", "status":"open", "evidence":"counterexample", "trigger":{"sequence":"default invocation","reach":"default_path","observed":true}, "triage":{"decision":"confirmed","reason":"observed","worth_fixing":true,"root_cause":"constraint","blocking_class":"default_path_defect","shared_flaw":false}})).unwrap()
    }
    /// The exclusive acceptance threshold also selects repair work. Findings
    /// below it stay in the record and are not sent to a writer.
    #[test]
    fn repair_selection_follows_the_acceptance_threshold() {
        for (severity, class, acceptance, repairable) in [
            ("blocking", "none", true, true),
            ("critical", "none", true, true),
            ("urgent", "none", true, true),
            ("HIGH", "none", true, true),
            ("medium", "none", false, false),
            ("moderate", "none", false, false),
            ("nit", "none", false, false),
            ("low", "none", false, false),
            ("blocking", "acceptance", true, true),
            ("medium", "acceptance", true, true),
            ("nit", "acceptance", true, true),
            ("blocking", "default_path_defect", true, true),
            ("medium", "default_path_defect", true, true),
            ("nit", "default_path_defect", true, true),
        ] {
            let mut item = finding("F1", "correctness");
            item.severity = severity.into();
            item.triage.as_mut().unwrap().blocking_class = class.into();
            assert_eq!(
                blocks_acceptance_for(&item, AcceptBelow::High),
                acceptance,
                "acceptance {severity}/{class}"
            );
            assert_eq!(
                repair_eligible_for(&item, AcceptBelow::High),
                repairable,
                "repair {severity}/{class}"
            );
        }
    }

    #[test]
    fn sealed_threshold_controls_high_severity_repair_without_losing_overrides() {
        let mut high = finding("F1", "correctness");
        high.severity = "high".into();
        high.triage.as_mut().unwrap().blocking_class = "none".into();
        assert!(repair_eligible_for(&high, AcceptBelow::High));
        assert!(!repair_eligible_for(&high, AcceptBelow::Blocking));

        let mut boundary = high;
        boundary.severity = "low".into();
        boundary.triage.as_mut().unwrap().blocking_class = "boundary".into();
        assert!(repair_eligible_for(&boundary, AcceptBelow::Blocking));

        let mut data_loss = boundary;
        data_loss.triage.as_mut().unwrap().blocking_class = "data_loss".into();
        assert!(repair_eligible_for(&data_loss, AcceptBelow::Blocking));
    }

    #[test]
    fn missing_trigger_does_not_hide_blocking_evidence() {
        let mut item = finding("F1", "correctness");
        item.trigger = None;
        item.severity = "blocking".into();
        item.triage.as_mut().unwrap().blocking_class = "none".into();
        assert!(blocks_acceptance(&item));
        assert!(repair_eligible(&item));
        assert!(matches!(
            review::acceptance_decision(&[item], AcceptBelow::Blocking, true),
            review::Acceptance::Reject(_)
        ));
    }

    #[test]
    fn severity_blocks_on_every_path_and_boundaries_need_falsification() {
        let mut item = finding("F1", "correctness");
        item.severity = "blocking".into();
        item.triage.as_mut().unwrap().blocking_class = "none".into();
        assert!(blocks_acceptance(&item));
        item.trigger.as_mut().unwrap().reach = FindingReach::OperatorMistake;
        assert!(blocks_acceptance(&item));
        assert!(repair_eligible(&item));
        item.trigger.as_mut().unwrap().reach = FindingReach::AttackerOrUnreachable;
        assert!(blocks_acceptance(&item));
        assert!(repair_eligible(&item));

        for class in ["boundary", "data_loss"] {
            let mut item = finding("F1", "security");
            item.severity = "medium".into();
            item.triage.as_mut().unwrap().blocking_class = class.into();
            item.triage.as_mut().unwrap().decision = "rejected".into();
            item.triage.as_mut().unwrap().worth_fixing = false;
            item.disposition = Some("rejected".into());
            assert!(blocks_acceptance(&item));
            assert!(repair_eligible(&item));
            item.verification = Some("falsified".into());
            item.evidence = None;
            assert!(blocks_acceptance(&item), "falsification without evidence");
            assert!(repair_eligible(&item), "falsification without evidence");
            item.evidence = Some("test disproves trigger".into());
            assert!(!blocks_acceptance(&item));
            assert!(!repair_eligible(&item));
        }

        // A spec defect is routed to the chair: it neither contradicts PASS nor
        // takes a repair writer.
        let mut defect = finding("F1", "spec");
        defect.severity = "nit".into();
        defect.triage.as_mut().unwrap().blocking_class = "none".into();
        defect.kind = Some("spec_defect".into());
        assert!(!blocks_acceptance(&defect));
        assert!(!repair_eligible(&defect));
    }

    #[test]
    fn a_settled_or_unconfirmed_should_fix_takes_no_writer() {
        let mutations: [fn(&mut Finding); 4] = [
            |item: &mut Finding| item.triage.as_mut().unwrap().worth_fixing = false,
            |item: &mut Finding| item.triage.as_mut().unwrap().decision = "rejected".into(),
            |item: &mut Finding| item.disposition = Some("disputed".into()),
            |item: &mut Finding| item.status = "fixed".into(),
        ];
        for mutate in mutations {
            let mut item = finding("F1", "correctness");
            item.severity = "medium".into();
            item.triage.as_mut().unwrap().blocking_class = "none".into();
            mutate(&mut item);
            assert!(!blocks_acceptance(&item));
            assert!(!repair_eligible(&item));
        }
    }

    #[test]
    fn delta_lens_selection_resolves_writer_head_and_widens_unknown_or_unavailable_invariant() {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "graph_id": "delta-lens-source",
            "nodes": [
                {"id":"fix", "route":{"alias":"fixture"}, "access":"worktree_write", "prompt":"write",
                 "worktree":"/fixture", "write_scope":["src"]},
                {"id":"review", "route":{"alias":"fixture"}, "access":"read_only", "prompt":"review",
                 "depends_on":["fix"], "source":{"type":"node", "node":"fix"}, "repair_lens":"correctness"}
            ]
        }))
        .unwrap();
        let (mut plan, mut positions) =
            validate_flat_plan(&document, &crate::config::GraphLimits::default()).unwrap();
        let review_index = positions["review"];
        plan.nodes[review_index].review_base = Some("1".repeat(40));
        plan.nodes[review_index].round = Some(LoopRound {
            extra: Default::default(),
            loop_id: "cycle".into(),
            round: 1,
            node_id: "review".into(),
            until: false,
            retry_on: false,
            previous_gate: None,
        });
        let mut cycle = plan.nodes[review_index].clone();
        cycle.id = "cycle".into();
        cycle.source = None;
        cycle.review_lens = None;
        cycle.repair_lens = None;
        cycle.round = None;
        plan.nodes.push(cycle);
        positions.insert("cycle".into(), plan.nodes.len() - 1);
        let mut checkpoint = transitions::initial_checkpoint(
            &document,
            &plan,
            "delta-lens-source".into(),
            None,
            None,
            None,
        );
        checkpoint.nodes[positions["fix"]].state = NodeState::Succeeded;
        checkpoint.nodes[positions["fix"]].outputs = Some(NodeOutputs {
            commit: Some("2".repeat(40)),
            ..Default::default()
        });
        let mut open = finding("F1", "tests");
        let mut previous = FindingHead {
            unresolved: vec![open.clone()],
            findings: vec![open.clone()],
            ..Default::default()
        };
        previous.dispositions.insert("base".into(), "1".repeat(40));
        let mut ledger = FindingLedger::default();
        ledger.heads.insert("3".repeat(40), previous);
        ledger.last_reviewed_head = Some("3".repeat(40));
        checkpoint.nodes[positions["cycle"]].finding_ledger = Some(ledger.clone());
        checkpoint.nodes[positions[&plan.nodes[review_index].id]].review_round_scope =
            Some(ReviewRoundScope::Delta {
                base: "3".repeat(40),
                head: "2".repeat(40),
            });

        // The planned lens has only SourceBinding::Node; its own source_commit
        // is unset until launch. The producer's committed output still pins a
        // valid delta, so the lens with no open findings is skipped.
        assert!(inactive_lens(
            &plan.nodes[review_index],
            &plan,
            &checkpoint,
            &positions
        ));

        open.lens = None;
        ledger
            .heads
            .get_mut("3".repeat(40).as_str())
            .unwrap()
            .unresolved = vec![open.clone()];
        ledger
            .heads
            .get_mut("3".repeat(40).as_str())
            .unwrap()
            .findings = vec![open];
        checkpoint.nodes[positions["cycle"]].finding_ledger = Some(ledger.clone());
        assert!(!inactive_lens(
            &plan.nodes[review_index],
            &plan,
            &checkpoint,
            &positions
        ));

        let mut invariant = finding("F2", "tests");
        invariant.class = Some("invariant-A".into());
        let previous = ledger.heads.get_mut("3".repeat(40).as_str()).unwrap();
        previous.unresolved = vec![invariant.clone()];
        previous.findings = vec![invariant];
        previous
            .invariant_coverage
            .insert("invariant-A".into(), BTreeSet::from(["tests".into()]));
        previous.unavailable_lenses.insert("tests".into());
        checkpoint.nodes[positions["cycle"]].finding_ledger = Some(ledger);
        assert!(!inactive_lens(
            &plan.nodes[review_index],
            &plan,
            &checkpoint,
            &positions
        ));
    }

    #[test]
    fn recurring_identity_keeps_lens_and_location() {
        assert!(same_finding(
            &finding("review:security:F1", "security"),
            &finding("repair:check:F1", "security")
        ));
        assert!(!same_finding(
            &finding("review:F1", "security"),
            &finding("repair:F1", "tests")
        ));
    }
    #[test]
    fn design_escalation_needs_concrete_evidence() {
        let mut item = finding("F1", "design");
        assert!(!design_defect(&item, AcceptBelow::High));
        item.design_defect = Some(true);
        assert!(design_defect(&item, AcceptBelow::High));
        item.evidence = Some(String::new());
        assert!(!design_defect(&item, AcceptBelow::High));
    }
    #[test]
    fn replanned_panel_prefers_separation_from_dependencies() {
        let patch = panel_patch("review", &"a".repeat(40), 2);
        assert_eq!(patch.nodes[0]["use"], "review-panel");
        assert_eq!(patch.nodes[0]["distinct_strength"], "prefer");
    }
    #[test]
    // Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
    fn split_verdict_patch_creates_one_adjudicator_and_no_round() {
        let panels = ["panel".to_owned(), "second-panel".to_owned()];
        let patch = split_verdict_patch("split", &panels, None, 2, "principal");
        assert_eq!(patch.nodes.len(), 1);
        assert_eq!(patch.nodes[0]["kind"], "adjudicate");
        assert_eq!(patch.nodes[0]["route"]["alias"], "principal");
        assert_eq!(patch.nodes[0]["after"][0], "panel");
        assert_eq!(patch.nodes[0]["after"][1], "second-panel");
        assert_eq!(patch.nodes[0]["inputs"].as_array().unwrap().len(), 2);
        assert!(patch.nodes[0].get("loop").is_none());
        assert!(patch.nodes[0].get("repeat").is_none());
        assert_eq!(
            patch.idempotency_key,
            split_verdict_patch("split", &panels[1..], None, 3, "principal").idempotency_key,
            "one item/head split has one adjudicator regardless of panel"
        );
        assert_ne!(
            patch.idempotency_key,
            split_verdict_patch("another-item", &panels, None, 2, "principal").idempotency_key,
            "different queue items do not share adjudicators"
        );
    }
    #[test]
    fn split_adjudication_detects_a_flagged_panel_once() {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"split","nodes":[
                {"id":"panel","access":"read_only","prompt":"review","route":{"alias":"fixture"}},
                {"id":"gate","kind":"gate","access":"read_only","depends_on":["panel"],
                 "inputs":[{"node":"panel","outputs":["verdict"]}],
                 "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
            ]
        }))
        .unwrap();
        let (plan, _) =
            validate_flat_plan(&document, &crate::config::GraphLimits::default()).unwrap();
        let mut checkpoint = transitions::initial_checkpoint(
            &document,
            &plan,
            CanonicalV1::file_digest(b"split").to_string(),
            None,
            None,
            None,
        );
        assert!(
            split_adjudication(&plan.nodes[1], &plan, &checkpoint)
                .unwrap()
                .is_none()
        );
        checkpoint.nodes[0].state = NodeState::Succeeded;
        checkpoint.nodes[0].outputs = Some(NodeOutputs {
            verdict: Some("VERDICT: FAIL".into()),
            findings_available: Some(true),
            warnings: vec!["review_split_verdict: conflicting verdicts on one head".into()],
            ..Default::default()
        });
        let incomplete_cases: [fn(&mut NodeOutputs); 3] = [
            |outputs: &mut NodeOutputs| outputs.partial = true,
            |outputs: &mut NodeOutputs| outputs.findings_available = Some(false),
            |outputs: &mut NodeOutputs| outputs.verdict = None,
        ];
        for make_incomplete in incomplete_cases {
            let mut incomplete = checkpoint.clone();
            make_incomplete(incomplete.nodes[0].outputs.as_mut().unwrap());
            assert!(
                split_adjudication(&plan.nodes[1], &plan, &incomplete).is_err(),
                "a split warning cannot admit partial or unusable panel evidence"
            );
        }
        assert_eq!(
            split_adjudication(&plan.nodes[1], &plan, &checkpoint).unwrap(),
            Some(SplitAdjudication {
                item: "panel".into(),
                parent: "panel".into(),
                panels: vec!["panel".into()],
                head: None,
                revision: patch::revision(&checkpoint),
                existing_node_id: None,
            })
        );
        checkpoint.expansions.push(
            serde_json::from_value(serde_json::json!({
                "source":"panel", "patch":{"revision":2,"digest":"split-digest",
                    "key":split_adjudication_key("panel", None),"parent":"gate"},
                "nodes":[{"id":"panel--adjudicate", "kind":"adjudicate",
                    "parent_node":"panel", "access":"read_only", "prompt":"resolve",
                    "inputs":[{"node":"panel","outputs":["findings","verdict"]}]}]
            }))
            .unwrap(),
        );
        assert_eq!(
            split_adjudication(&plan.nodes[1], &plan, &checkpoint)
                .unwrap()
                .map(|state| state.existing_node_id),
            Some(Some("panel--adjudicate".into())),
            "a pending complete-input adjudicator must not expose a stale verdict"
        );
        let mut adjudicator_progress = checkpoint.nodes[0].clone();
        adjudicator_progress.id = "panel--adjudicate".into();
        adjudicator_progress.outputs = Some(NodeOutputs::default());
        adjudicator_progress.state = NodeState::Succeeded;
        checkpoint.nodes.push(adjudicator_progress);
        assert!(
            split_adjudication(&plan.nodes[1], &plan, &checkpoint)
                .unwrap()
                .is_none()
        );
        checkpoint.expansions.push(
            serde_json::from_value(serde_json::json!({
                "source":"panel", "patch":{"revision":3,"digest":"continuation-digest",
                    "key":"split-adjudicate-continuation:2","parent":"panel",
                    "replaced":[{"id":"panel--adjudicate","kind":"adjudicate",
                        "parent_node":"panel","access":"read_only","prompt":"resolve",
                        "inputs":[
                            {"node":"panel","outputs":["findings","verdict"]},
                            {"node":"second-panel","outputs":["findings","verdict"]}
                        ]}]
                }, "nodes":[]
            }))
            .unwrap(),
        );
        assert_eq!(
            panel_adjudicator("panel", None, &checkpoint)
                .unwrap()
                .inputs
                .len(),
            2,
            "the stable adjudicator resolves to its latest unioned inputs"
        );
    }
    // Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
    #[test]
    fn replanned_split_requires_its_own_successful_adjudicator_output() {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "panel-output", "nodes": [
                {"id":"check", "access":"read_only", "prompt":"review", "route":{"alias":"fixture"}}
            ]
        }))
        .unwrap();
        let (plan, _) =
            validate_flat_plan(&document, &crate::config::GraphLimits::default()).unwrap();
        let mut checkpoint = transitions::initial_checkpoint(
            &document,
            &plan,
            CanonicalV1::file_digest(b"panel-output").to_string(),
            None,
            None,
            None,
        );
        let mut panel = checkpoint.nodes[0].clone();
        panel.id = "check--panel".into();
        panel.state = NodeState::Succeeded;
        panel.outputs = Some(NodeOutputs {
            verdict: Some("VERDICT: PASS".into()),
            warnings: vec!["review_split_verdict: conflicting verdicts on one head".into()],
            ..Default::default()
        });
        let mut adjudicator = panel.clone();
        adjudicator.id = "check--panel--adjudicate".into();
        adjudicator.state = NodeState::Failed;
        adjudicator.outputs = None;
        checkpoint.nodes.extend([panel, adjudicator]);
        checkpoint.expansions.push(
            serde_json::from_value(serde_json::json!({
                "source":"check", "patch":{"revision":1,"digest":"fixture","parent":"check",
                    "reason":"repair expanded risk or scope"},
                "nodes":[{"id":"check--panel", "kind":"composite", "access":"read_only",
                    "composite":{"nodes":[],"exports":[]}}]
            }))
            .unwrap(),
        );
        checkpoint.expansions.push(serde_json::from_value(serde_json::json!({
            "source":"check--panel",
            "patch":{"revision":2,"digest":"adjudication","key":split_adjudication_key("check", None)},
            "nodes":[{"id":"check--panel--adjudicate", "kind":"adjudicate",
                "parent_node":"check--panel", "access":"read_only", "prompt":"resolve",
                "inputs":[{"node":"check--panel","outputs":["findings","verdict"]}]}]
        })).unwrap());
        assert!(
            panel_outputs(&plan, &checkpoint, "check").is_none(),
            "a failed adjudicator cannot be bypassed by the panel verdict"
        );
        let adjudicator = checkpoint.nodes.last_mut().unwrap();
        adjudicator.state = NodeState::Succeeded;
        adjudicator.outputs = Some(NodeOutputs {
            verdict: Some("VERDICT: FAIL".into()),
            ..Default::default()
        });
        assert_eq!(
            panel_outputs(&plan, &checkpoint, "check")
                .and_then(|outputs| outputs.verdict.as_deref()),
            Some("VERDICT: FAIL")
        );
    }
}
