//! Review repair policy over ordinary loops and recorded attempt outputs.
use super::*;
use std::collections::BTreeMap;

pub(super) fn spec_is_repair(spec: &loops::LoopSpec) -> bool {
    spec.review_repair
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

fn initial_outputs<'a>(
    node: &PlannedNode,
    checkpoint: &'a GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Vec<&'a NodeOutputs> {
    node.inputs
        .iter()
        .filter(|input| input.outputs.contains(&OutputKind::Findings))
        .filter_map(|input| positions.get(&input.node))
        .filter_map(|&index| checkpoint.nodes[index].outputs.as_ref())
        .collect()
}

pub(super) fn clean_start(
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> bool {
    if !is_repair(node) {
        return false;
    }
    let outputs = initial_outputs(node, checkpoint, positions);
    !outputs.is_empty()
        && outputs.iter().all(|output| {
            output.findings_available == Some(true)
                && approved(output)
                && !output.findings.iter().any(repair_eligible)
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
                panel_outputs(checkpoint, &progress.id).or(progress.outputs.as_ref())
            })
    });
    let outputs = latest_review.map_or_else(
        || initial_outputs(node, checkpoint, positions),
        |output| vec![output],
    );
    !outputs.is_empty()
        && outputs.iter().all(|output| {
            output.findings_available == Some(true)
                && output.verdict_error.is_none()
                && output
                    .findings
                    .iter()
                    .all(|finding| finding.triage().is_some() && !repair_eligible(finding))
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

fn confirmed_worth_fixing(finding: &Finding) -> bool {
    finding
        .triage()
        .is_some_and(|triage| triage.decision == "confirmed" && triage.worth_fixing)
}

/// The PASS contract: an unresolved boundary or data-loss finding needs
/// falsifying evidence, and a default-path finding blocks at blocking severity
/// whether or not it is triaged. Once triage confirms a finding is worth
/// fixing, either blocking class also blocks. A confirmed should-fix does not
/// contradict PASS.
pub(super) fn blocks_acceptance(finding: &Finding) -> bool {
    if finding.boundary_or_data_loss() {
        return !finding.falsified_with_evidence();
    }
    if answered(finding) || !finding.default_path() {
        return false;
    }
    blocking_severity(finding)
        || finding.triage().is_some_and(|triage| {
            triage.decision == "confirmed"
                && triage.worth_fixing
                && matches!(
                    triage.blocking_class.as_str(),
                    "acceptance" | "default_path_defect"
                )
        })
}

/// Work a repair writer must take: everything that blocks acceptance, plus a
/// confirmed, worth-fixing should-fix on the default path. A note without a
/// blocking class is reported, never repaired, and a finding that still needs
/// judgement is escalated rather than rewritten.
pub(super) fn repair_eligible(finding: &Finding) -> bool {
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
    blocks_acceptance(finding)
        || (confirmed_worth_fixing(finding)
            && finding.default_path()
            && severity_class(finding.effective_severity()) == Some(SeverityClass::ShouldFix))
}

pub(super) fn blocking_severity(finding: &Finding) -> bool {
    severity_class(finding.effective_severity()) == Some(SeverityClass::Blocking)
}

fn design_defect(finding: &Finding) -> bool {
    repair_eligible(finding)
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

/// One adjudication node resolves a split verdict on one head. The patch adds
/// only that node after the panel: no loop, no round, no second panel. An
/// upheld changes-request still needs repair, which the adjudicator declares.
/// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
pub(super) fn split_verdict_patch(parent: &str, revision: u64) -> patch::Patch {
    let adjudicate = serde_json::json!({
        "id": "adjudicate",
        "kind": "adjudicate",
        "access": "read_only",
        "after": [parent],
        "inputs": [{"node": parent, "outputs": ["verdict", "findings"]}],
        "require_findings": true,
        "required_verdict": "VERDICT: PASS",
        "prompt": "Read only. The panel above reported conflicting verdicts on one head. Resolve the split once: weigh each finding against the code and the named invariant in its top-level `class`. Return every input finding in the findings block with its id and invariant preserved, then exactly one verdict. Do not re-run a full panel or start another review round. If a blocking finding is upheld, name the invariant it breaks and say that repair is required; an upheld changes-request legitimately needs a repair round, but the split itself does not. Give VERDICT: PASS only when no blocking finding survives adjudication, otherwise VERDICT: FAIL or VERDICT: INCONCLUSIVE."
    });
    patch::Patch {
        schema_version: patch::PATCH_SCHEMA_VERSION,
        expected_revision: Some(revision),
        idempotency_key: Some(format!(
            "split-adjudicate:{}",
            CanonicalV1::file_digest(parent.as_bytes())
        )),
        reason: Some("split verdict on one head".into()),
        nodes: vec![adjudicate],
        ..Default::default()
    }
}

/// A settled panel that flagged a split and has no adjudication node yet.
pub(super) fn split_adjudication(checkpoint: &GraphCheckpoint) -> Option<(String, u64)> {
    if checkpoint
        .nodes
        .iter()
        .any(|node| node.id.contains("adjudicate"))
    {
        return None;
    }
    let parent = checkpoint.nodes.iter().find(|node| {
        node.state == NodeState::Succeeded
            && node.outputs.as_ref().is_some_and(|outputs| {
                outputs
                    .warnings
                    .iter()
                    .any(|warning| warning.starts_with("review_split_verdict:"))
            })
    })?;
    Some((parent.id.clone(), patch::revision(checkpoint)))
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
    checkpoint: &'a GraphCheckpoint,
    parent: &str,
) -> Option<&'a NodeOutputs> {
    let expansion = panel_expansion(checkpoint, parent)?;
    if panel_pending(checkpoint, expansion) {
        return None;
    }
    let root = expansion.nodes.iter().find(|candidate| {
        candidate.composite.is_some()
            && expansion.nodes.iter().all(|member| {
                member.id == candidate.id || member.id.starts_with(&format!("{}--", candidate.id))
            })
    })?;
    checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == root.id)?
        .outputs
        .as_ref()
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

pub(super) fn panel_failure(checkpoint: &GraphCheckpoint, expansion: &Expansion) -> Option<String> {
    let Some(root) = expansion.nodes.iter().find(|candidate| {
        candidate.composite.is_some()
            && expansion.nodes.iter().all(|member| {
                member.id == candidate.id || member.id.starts_with(&format!("{}--", candidate.id))
            })
    }) else {
        return Some("replanned panel boundary is missing".into());
    };
    let Some(progress) = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == root.id)
    else {
        return Some("replanned panel boundary is missing".into());
    };
    let Some(outputs) = progress.outputs.as_ref() else {
        return Some("replanned panel has no complete output".into());
    };
    if progress.state != NodeState::Succeeded
        || outputs.findings_available != Some(true)
        || !approved(outputs)
        || outputs.findings.iter().any(blocks_acceptance)
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
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Option<&'static str> {
    if !is_repair(node) {
        return None;
    }
    let initial: Vec<_> = initial_outputs(node, checkpoint, positions)
        .into_iter()
        .flat_map(|outputs| outputs.findings.iter())
        .filter(|finding| repair_eligible(finding))
        .collect();
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
    if initial.iter().any(|finding| design_defect(finding)) {
        return Some("concrete design defect");
    }
    let mut prior = initial;
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
        let Some(outputs) = panel_outputs(checkpoint, &check.id).or(check.outputs.as_ref()) else {
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
            .filter(|finding| repair_eligible(finding))
            .collect();
        if shared_flaw(&current) {
            return Some("checked findings share a design flaw");
        }
        if third_review_judgement && round >= 2 && !current.is_empty() {
            return Some("third review round requires design judgement");
        }
        if current.iter().any(|finding| design_defect(finding)) {
            return Some("concrete design defect");
        }
        if current
            .iter()
            .any(|finding| prior.iter().any(|previous| same_finding(finding, previous)))
        {
            return Some("recurring finding after repair");
        }
        prior = current;
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
    if clean_start(owner, checkpoint, positions) || no_repair_work(owner, checkpoint, positions) {
        return true;
    }
    if escalation(owner, checkpoint, positions).is_none() {
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
    let findings = selected_findings(node, plan, checkpoint, positions);
    if findings.is_empty() {
        return false;
    }
    // Keep the static delta stage runnable before the broader host panel.
    // Newly selected lenses may not exist in the original template.
    if prior_panel(node, checkpoint) {
        return false;
    }
    // Scope or risk expansion requires the whole host-selected panel.
    if findings.iter().any(|(_, finding)| grows_review(finding)) {
        return false;
    }
    let mut known = false;
    for (_, finding) in findings
        .iter()
        .filter(|(_, finding)| repair_eligible(finding))
    {
        let Some(raising) = finding.lens.as_deref().filter(|lens| !lens.is_empty()) else {
            return false;
        };
        if raising == lens {
            return false;
        }
        known = true;
    }
    known
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
    let latest = checkpoint
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
        })
        .and_then(|progress| panel_outputs(checkpoint, &progress.id).or(progress.outputs.as_ref()))
        .or_else(|| initial_outputs(node, checkpoint, positions).last().copied());
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
    fn finding(id: &str, lens: &str) -> Finding {
        serde_json::from_value(serde_json::json!({"id":id,"lens":lens,"severity":"high", "file":"a.rs", "summary":"constraint", "status":"open", "evidence":"counterexample", "trigger":{"sequence":"default invocation","reach":"default_path","observed":true}, "triage":{"decision":"confirmed","reason":"observed","worth_fixing":true,"root_cause":"constraint","blocking_class":"default_path_defect","shared_flaw":false}})).unwrap()
    }
    /// One vocabulary map classifies the reviewer's severity word. A confirmed
    /// should-fix does not block acceptance but does start a repair writer; a
    /// note is reported and never repaired.
    #[test]
    fn blocks_acceptance_follows_the_pass_contract_and_repair_adds_should_fix() {
        for (severity, class, acceptance, repairable) in [
            ("blocking", "none", true, true),
            ("critical", "none", true, true),
            ("p0", "none", true, true),
            ("HIGH", "none", true, true),
            ("should-fix", "none", false, true),
            ("medium", "none", false, true),
            ("moderate", "none", false, true),
            ("p2", "none", false, true),
            ("note", "none", false, false),
            ("low", "none", false, false),
            ("blocking", "acceptance", true, true),
            ("should-fix", "acceptance", true, true),
            ("note", "acceptance", true, true),
            ("blocking", "default_path_defect", true, true),
            ("should-fix", "default_path_defect", true, true),
            ("note", "default_path_defect", true, true),
        ] {
            let mut item = finding("F1", "correctness");
            item.severity = severity.into();
            item.triage.as_mut().unwrap().blocking_class = class.into();
            assert_eq!(
                blocks_acceptance(&item),
                acceptance,
                "acceptance {severity}/{class}"
            );
            assert_eq!(
                repair_eligible(&item),
                repairable,
                "repair {severity}/{class}"
            );
        }
    }

    #[test]
    fn only_a_checked_default_path_blocks_and_boundaries_need_falsification() {
        let mut item = finding("F1", "correctness");
        item.severity = "blocking".into();
        item.triage.as_mut().unwrap().blocking_class = "none".into();
        assert!(blocks_acceptance(&item));
        item.trigger.as_mut().unwrap().reach = FindingReach::OperatorMistake;
        assert!(!blocks_acceptance(&item));
        assert!(!repair_eligible(&item));
        item.trigger.as_mut().unwrap().reach = FindingReach::AttackerOrUnreachable;
        assert!(!blocks_acceptance(&item));
        assert!(!repair_eligible(&item));

        for class in ["boundary", "data_loss"] {
            let mut item = finding("F1", "security");
            item.severity = "should-fix".into();
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
        defect.severity = "note".into();
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
            item.severity = "should-fix".into();
            item.triage.as_mut().unwrap().blocking_class = "none".into();
            mutate(&mut item);
            assert!(!blocks_acceptance(&item));
            assert!(!repair_eligible(&item));
        }
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
        assert!(!design_defect(&item));
        item.design_defect = Some(true);
        assert!(design_defect(&item));
        item.evidence = Some(String::new());
        assert!(!design_defect(&item));
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
        let patch = split_verdict_patch("panel", 2);
        assert_eq!(patch.nodes.len(), 1);
        assert_eq!(patch.nodes[0]["kind"], "adjudicate");
        assert_eq!(patch.nodes[0]["after"][0], "panel");
        assert!(patch.nodes[0].get("loop").is_none());
        assert!(patch.nodes[0].get("repeat").is_none());
        assert!(patch.idempotency_key.is_some());
    }
    #[test]
    fn split_adjudication_detects_a_flagged_panel_once() {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"split","nodes":[
                {"id":"panel","access":"read_only","prompt":"review","route":{"alias":"fixture"}}
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
        assert!(split_adjudication(&checkpoint).is_none());
        checkpoint.nodes[0].state = NodeState::Succeeded;
        checkpoint.nodes[0].outputs = Some(NodeOutputs {
            warnings: vec!["review_split_verdict: conflicting verdicts on one head".into()],
            ..Default::default()
        });
        assert_eq!(
            split_adjudication(&checkpoint),
            Some(("panel".into(), patch::revision(&checkpoint)))
        );
        checkpoint.nodes[0].id = "panel--adjudicate".into();
        assert!(split_adjudication(&checkpoint).is_none());
    }
}
