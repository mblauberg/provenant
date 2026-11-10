//! Review repair policy over ordinary loops and recorded attempt outputs.
use super::*;
use std::collections::BTreeMap;

pub(super) fn spec_is_repair(spec: &loops::LoopSpec) -> bool {
    spec.extra.get("review_repair") == Some(&serde_json::Value::Bool(true))
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
                && !output.findings.iter().any(blocking)
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
                    .all(|finding| finding.triage().is_some() && !blocking(finding))
        })
}

pub(super) fn blocking(finding: &Finding) -> bool {
    if finding.boundary_or_data_loss() {
        return !finding.falsified_with_evidence();
    }
    if finding.spec_defect()
        || matches!(finding.status.as_str(), "closed" | "resolved" | "fixed")
        || matches!(
            finding.disposition.as_deref(),
            Some("rejected" | "resolved" | "fixed" | "disputed" | "removed-the-case")
        )
    {
        return false;
    }
    let Some(triage) = finding.triage() else {
        return false;
    };
    if triage.decision != "confirmed" {
        return false;
    }
    if !triage.worth_fixing {
        return false;
    }
    if !finding.default_path() {
        return false;
    }
    matches!(
        triage.blocking_class.as_str(),
        "acceptance" | "default_path_defect"
    ) || blocking_severity(finding)
}

pub(super) fn blocking_severity(finding: &Finding) -> bool {
    matches!(
        finding.effective_severity().to_ascii_lowercase().as_str(),
        "critical"
            | "blocker"
            | "blocking"
            | "p0"
            | "high"
            | "error"
            | "p1"
            | "medium"
            | "moderate"
            | "should-fix"
    )
}

fn design_defect(finding: &Finding) -> bool {
    blocking(finding)
        && finding.extra.get("design_defect") == Some(&serde_json::Value::Bool(true))
        && finding
            .extra
            .get("evidence")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|evidence| !evidence.trim().is_empty())
}

fn grows_review(finding: &Finding) -> bool {
    ["risk_grew", "scope_grew"]
        .iter()
        .any(|field| finding.extra.get(*field) == Some(&serde_json::Value::Bool(true)))
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
    let cache = progress.extra.get("repair_growth")?;
    (cache.get("before")?.as_str()? == before && cache.get("after")?.as_str()? == after)
        .then(|| cache.get("grew").and_then(serde_json::Value::as_bool))
        .flatten()
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
            && member.extra.get("repair_risk_assessment") == Some(&serde_json::Value::Bool(true))
            && checkpoint.nodes[index].state != NodeState::Skipped
            && checkpoint.nodes[index]
                .outputs
                .as_ref()
                .and_then(|output| output.extra.get("risk_tier"))
                .is_none()
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
        || outputs.findings.iter().any(blocking)
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
                    .extra
                    .get("risk_tier")
                    .and_then(serde_json::Value::as_str)
                    .and_then(rank)
                    .is_some_and(|current| baseline.is_none_or(|baseline| current > baseline))
        })
    })
}

fn same_finding(left: &Finding, right: &Finding) -> bool {
    left.id.rsplit(':').next() == right.id.rsplit(':').next()
        && left.file == right.file
        && left.extra.get("lens") == right.extra.get("lens")
}

fn shared_flaw(findings: &[&Finding]) -> bool {
    findings.iter().any(|finding| {
        finding
            .extra
            .get("triage")
            .and_then(|triage| triage.get("shared_flaw"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    })
}

fn design_cannot_hold(outputs: &NodeOutputs) -> bool {
    outputs
        .extra
        .get("repair_items")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("status").and_then(serde_json::Value::as_str) == Some("design-cannot-hold")
            })
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
        .filter(|finding| blocking(finding))
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
            .filter(|finding| blocking(finding))
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
    let Some(lens) = node
        .extra
        .get("repair_lens")
        .and_then(serde_json::Value::as_str)
    else {
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
    for (_, finding) in findings.iter().filter(|(_, finding)| blocking(finding)) {
        let Some(raising) = finding
            .extra
            .get("lens")
            .and_then(serde_json::Value::as_str)
            .filter(|lens| !lens.is_empty())
        else {
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
    checkpoint.nodes[index].state == NodeState::Skipped
        && plan.nodes[index].extra.contains_key("repair_lens")
}

/// Escalation carries the latest complete qualified findings to the principal;
/// the failed loop verdict remains failed until its recovery path completes.
pub(super) fn terminal_outputs(
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    passed: bool,
) -> NodeOutputs {
    let mut outputs = gate_outputs(passed);
    if passed || !is_repair(node) {
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
        if let Some(items) = writer.extra.get("repair_items") {
            outputs.extra.insert("repair_items".into(), items.clone());
        }
    }
    outputs
}

#[cfg(test)]
mod tests {
    use super::*;
    fn finding(id: &str, lens: &str) -> Finding {
        serde_json::from_value(serde_json::json!({"id":id,"lens":lens,"severity":"high", "file":"a.rs", "summary":"constraint", "status":"open", "evidence":"counterexample", "trigger":{"sequence":"default invocation","reach":"default_path","observed":true}, "triage":{"decision":"confirmed","reason":"observed","worth_fixing":true,"root_cause":"constraint","blocking_class":"default_path_defect","shared_flaw":false}})).unwrap()
    }
    #[test]
    fn repair_eligibility_uses_checked_reach_and_preserves_boundaries() {
        let mut item = finding("F1", "correctness");
        item.extra.get_mut("triage").unwrap()["blocking_class"] = "none".into();
        for severity in [
            "critical",
            "BLOCKER",
            "p0",
            "HIGH",
            "error",
            "p1",
            "should-fix",
        ] {
            item.severity = severity.into();
            assert!(blocking(&item), "{severity}");
        }
        item.severity = "MEDIUM".into();
        assert!(blocking(&item));
        item.severity = "p2".into();
        assert!(!blocking(&item));
        item.severity = "should-fix".into();
        item.extra.get_mut("trigger").unwrap()["reach"] = "operator_mistake".into();
        assert!(!blocking(&item));
        for class in ["boundary", "data_loss"] {
            item.extra.get_mut("triage").unwrap()["blocking_class"] = class.into();
            assert!(blocking(&item));
        }
        item.extra.insert("kind".into(), "spec_defect".into());
        assert!(blocking(&item));
        item.extra.remove("kind");
        item.extra.get_mut("triage").unwrap()["decision"] = "rejected".into();
        item.disposition = Some("rejected".into());
        assert!(blocking(&item));
        item.extra.insert("verification".into(), "falsified".into());
        item.extra.remove("evidence");
        assert!(blocking(&item));
        item.extra
            .insert("evidence".into(), "test disproves trigger".into());
        assert!(!blocking(&item));
    }

    #[test]
    fn rejected_boundaries_require_falsification_with_evidence() {
        for class in ["boundary", "data_loss"] {
            let mut item = finding("F1", "security");
            item.extra.get_mut("triage").unwrap()["blocking_class"] = class.into();
            item.extra.get_mut("triage").unwrap()["decision"] = "rejected".into();
            item.extra.get_mut("triage").unwrap()["worth_fixing"] = false.into();
            item.disposition = Some("rejected".into());
            assert!(blocking(&item));
            item.extra.insert("verification".into(), "falsified".into());
            item.extra.remove("evidence");
            assert!(blocking(&item));
            item.extra
                .insert("evidence".into(), "test disproves trigger".into());
            assert!(!blocking(&item));
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
        item.extra.insert("design_defect".into(), true.into());
        assert!(design_defect(&item));
        item.extra.insert("evidence".into(), "".into());
        assert!(!design_defect(&item));
    }
    #[test]
    fn replanned_panel_prefers_separation_from_dependencies() {
        let patch = panel_patch("review", &"a".repeat(40), 2);
        assert_eq!(patch.nodes[0]["use"], "review-panel");
        assert_eq!(patch.nodes[0]["distinct_strength"], "prefer");
    }
}
