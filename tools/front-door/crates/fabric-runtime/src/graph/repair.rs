//! Review repair policy over ordinary loops and recorded attempt outputs.
use super::*;
use std::collections::BTreeMap;

pub(super) fn spec_is_repair(spec: &loops::LoopSpec) -> bool {
    spec.extra.get("review_repair") == Some(&serde_json::Value::Bool(true))
}

pub(super) fn is_repair(node: &PlannedNode) -> bool {
    node.loop_body.as_ref().is_some_and(spec_is_repair)
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
                && output.verdict.as_deref() == Some("VERDICT: PASS")
                && !output.findings.iter().any(blocking)
        })
}

fn blocking(finding: &Finding) -> bool {
    !matches!(
        finding.status.to_ascii_lowercase().as_str(),
        "closed" | "resolved" | "fixed"
    ) && !matches!(
        finding.disposition.as_deref(),
        Some("rejected" | "resolved" | "fixed")
    ) && matches!(
        finding.severity.to_ascii_lowercase().as_str(),
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
            | "p2"
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
    if !round.until
        || !matches!(node.condition, Some(GateCondition::Verdict { .. }))
        || !positions
            .get(&round.loop_id)
            .is_some_and(|&i| is_repair(&plan.nodes[i]))
    {
        return false;
    }
    checkpoint.nodes.iter().any(|progress| {
        progress.round.as_ref().is_some_and(|member| {
            member.loop_id == round.loop_id
                && member.round == round.round
                && matches!(member.node_id.as_str(), "check" | "synthesis")
        }) && progress
            .outputs
            .as_ref()
            .is_some_and(|output| output.findings.iter().any(grows_review))
    })
}

fn same_finding(left: &Finding, right: &Finding) -> bool {
    left.id.rsplit(':').next() == right.id.rsplit(':').next()
        && left.file == right.file
        && left.extra.get("lens") == right.extra.get("lens")
}

/// A concrete design defect or a finding surviving an attempted repair takes
/// the principal path before consuming the remaining cycle budget.
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
    if initial.iter().any(|finding| design_defect(finding)) {
        return Some("concrete design defect");
    }
    let mut prior = initial;
    for round in 1..=node.loop_body.as_ref().map_or(0, |spec| spec.max_rounds) {
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
        if !check.state.settled() {
            break;
        }
        let Some(outputs) = &check.outputs else {
            continue;
        };
        if outputs.findings.iter().any(grows_review) {
            return Some("repair expanded risk or scope; full panel replan required");
        }
        let current: Vec<_> = outputs
            .findings
            .iter()
            .filter(|finding| blocking(finding))
            .collect();
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
    if clean_start(owner, checkpoint, positions) {
        return true;
    }
    if escalation(owner, checkpoint, positions).is_none() {
        return false;
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

#[cfg(test)]
mod tests {
    use super::*;
    fn finding(id: &str, lens: &str) -> Finding {
        serde_json::from_value(serde_json::json!({"id":id,"lens":lens,"severity":"high", "file":"a.rs", "summary":"constraint", "status":"open", "evidence":"counterexample"})).unwrap()
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
        .and_then(|progress| progress.outputs.as_ref())
        .or_else(|| initial_outputs(node, checkpoint, positions).last().copied());
    if let Some(latest) = latest {
        outputs.findings.clone_from(&latest.findings);
        outputs.findings_available = latest.findings_available;
        outputs.warnings.clone_from(&latest.warnings);
    }
    outputs
}
