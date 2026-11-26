//! Deterministic contract for review template results and coverage.
use super::*;

/// The one result order every review prompt states: one verdict line after the
/// findings and prose, then the closed handoff as the very last thing. Settlement
/// tolerates only a lone verdict after the handoff, so prompts never invite it.
pub(super) const VERDICT_THEN_HANDOFF: &str = "After all findings and prose, write exactly one verdict line: VERDICT: PASS, VERDICT: FAIL or VERDICT: INCONCLUSIVE. Then put the shared closed handoff block last, with nothing after it.";

/// A delivery review is acceptable only with complete structured evidence.
/// Keep the original verdict in the record; this policy affects the gate only.
pub(super) fn delivery_matches(
    policy: &DeliveryReviewSpec,
    outputs: &NodeOutputs,
    expected: &str,
) -> bool {
    use provenant_records::workflow::ReviewVerdict;
    if ReviewVerdict::from_token(expected) != Some(ReviewVerdict::Approve) {
        return outputs.verdict.as_deref().is_some_and(|actual| {
            match (
                ReviewVerdict::from_token(actual),
                ReviewVerdict::from_token(expected),
            ) {
                (Some(actual), Some(expected)) => actual == expected,
                _ => actual == expected,
            }
        });
    }
    if outputs.partial
        || outputs.findings_available != Some(true)
        || outputs.verdict_error.is_some()
        || outputs.findings.iter().any(|finding| {
            severity_class(&finding.severity) == Some(SeverityClass::Blocking)
                || repair::blocks_acceptance(finding)
        })
    {
        return false;
    }
    match outputs
        .verdict
        .as_deref()
        .and_then(ReviewVerdict::from_token)
    {
        Some(ReviewVerdict::Approve) => true,
        Some(ReviewVerdict::Inconclusive) => policy.accept_inconclusive_without_blocking,
        _ => false,
    }
}

pub(super) fn malformed_delivery_evidence_error(code: Option<&str>) -> bool {
    matches!(
        code,
        Some(
            "review_verdict_invalid"
                | "review_findings_invalid"
                | "review_triage_missing"
                | "review_verdict_conflict"
                | "review_propagation_invalid"
                | "findings_required"
                | "disposition_overflow_incomplete"
                | "disposition_overflow_inconsistent"
                | "verification_invalid"
                | "verification_incomplete"
        )
    )
}

/// A composite delivery-review boundary has no attempt settlement to own its
/// evidence failure. Treat failed coverage and partial or unparseable aggregate
/// evidence as malformed so the graph can park the boundary through the inbox.
/// A complete, parseable FAIL remains a review decision and can route to repair.
pub(super) fn malformed_composite_delivery_evidence(
    node: &PlannedNode,
    progress: &NodeProgress,
) -> bool {
    if node.composite.is_none() || node.delivery_review.is_none() {
        return false;
    }
    match progress.state {
        NodeState::Pending | NodeState::Running | NodeState::Skipped => false,
        NodeState::Failed | NodeState::Blocked => true,
        NodeState::Succeeded => progress.outputs.as_ref().is_none_or(|outputs| {
            outputs.partial
                || outputs.findings_available != Some(true)
                || outputs.verdict_error.is_some()
                || outputs
                    .verdict
                    .as_deref()
                    .and_then(provenant_records::workflow::ReviewVerdict::from_token)
                    .is_none()
        }),
    }
}

pub(super) fn partial(outputs: &NodeOutputs) -> bool {
    outputs.partial
}

/// Coverage is separate from whether the consumer wrote a parseable block.
/// A complete overflow proof can cover truncated exports, but cannot replace
/// an unavailable lens or verifier.
pub(super) fn partial_inputs(
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    node.inputs.iter().any(|input| {
        input.outputs.iter().any(|kind| {
            matches!(
                kind,
                OutputKind::Findings | OutputKind::Result | OutputKind::Verdict
            )
        }) && positions.get(&input.node).is_some_and(|&index| {
            let source = &checkpoint.nodes[index];
            source.state != NodeState::Succeeded
                || source.outputs.as_ref().is_none_or(|outputs| {
                    partial(outputs)
                        || outputs
                            .verdict
                            .as_deref()
                            .and_then(provenant_records::workflow::ReviewVerdict::from_token)
                            == Some(provenant_records::workflow::ReviewVerdict::Inconclusive)
                        || (input.outputs.contains(&OutputKind::Findings)
                            && outputs.findings_available != Some(true)
                            && source.truncation_warnings().is_empty())
                })
        })
    })
}

/// Keep evidence from an invalid review reply without counting it as coverage.
/// This does not export failed writers or provider failures as review evidence.
pub(super) fn retained_findings(node: &PlannedNode, progress: &NodeProgress) -> bool {
    progress.state == NodeState::Failed
        && ((node.composite.is_some() && progress.outputs.as_ref().is_some_and(partial))
            || (template::review_behaviour(node).is_some()
                && progress
                    .attempts
                    .last()
                    .is_some_and(|attempt| attempt.outcome == Some(AttemptOutcome::TaskFailed))))
        && progress
            .outputs
            .as_ref()
            .is_some_and(|outputs| !outputs.findings.is_empty())
}

pub(super) fn mark_partial(outputs: &mut NodeOutputs) {
    outputs.partial = true;
    outputs.findings_available = Some(false);
    // Blocking evidence still warrants FAIL; approval cannot restore coverage.
    if outputs
        .verdict
        .as_deref()
        .and_then(provenant_records::workflow::ReviewVerdict::from_token)
        .is_none_or(|verdict| verdict != provenant_records::workflow::ReviewVerdict::RequestChanges)
    {
        outputs.verdict = Some("VERDICT: INCONCLUSIVE".into());
    }
}

pub(super) fn contract_error(
    node: &PlannedNode,
    upstream: &[Finding],
    outputs: &NodeOutputs,
) -> Option<(&'static str, String)> {
    let repair_triage = node.repair_triage;
    if template::review_behaviour(node).is_none()
        && node.delivery_review.is_none()
        && !repair_triage
    {
        return None;
    }
    // ResultCapture has already counted `VERDICT:` lines outside fenced
    // blocks. `verdict_error` also carries source truncation, which is
    // diagnosed by findings availability below; a captured verdict is not
    // reported as malformed for that reason.
    if !outputs
        .verdict
        .as_deref()
        .is_some_and(provenant_records::result_capture::is_review_verdict)
    {
        let cause = match outputs.verdict.as_deref() {
            Some(other) => format!("`{other}` is not VERDICT: PASS, FAIL or INCONCLUSIVE"),
            None => outputs
                .verdict_error
                .clone()
                .unwrap_or_else(|| "no VERDICT: line outside fenced blocks".to_owned()),
        };
        return Some((
            "review_verdict_invalid",
            format!(
                "review needs exactly one VERDICT: PASS, FAIL or INCONCLUSIVE line outside fenced blocks: {cause}"
            ),
        ));
    }
    if outputs.findings_available != Some(true) {
        return Some(("review_findings_invalid", "review findings are missing, malformed or incomplete; retained findings remain available as partial evidence".into()));
    }
    let mut ids = std::collections::BTreeSet::new();
    for finding in &outputs.findings {
        if upstream.iter().any(|source| source.id == finding.id) {
            // Identity-only decisions inherit the source evidence. Copies from
            // multiple verifiers are checked by their disposition contract.
            continue;
        }
        if !ids.insert(&finding.id) {
            return Some((
                "review_findings_invalid",
                format!("finding {} repeats an independent id", finding.id),
            ));
        }
        for (field, value) in [
            ("id", Some(finding.id.as_str())),
            ("severity", Some(finding.severity.as_str())),
            ("file", Some(finding.file.as_str())),
            ("summary", Some(finding.summary.as_str())),
            ("status", Some(finding.status.as_str())),
            ("evidence", finding.evidence.as_deref()),
            ("proposed_fix", finding.proposed_fix.as_deref()),
        ] {
            if value.is_none_or(|value| value.trim().is_empty()) {
                return Some((
                    "review_findings_invalid",
                    format!("finding {} needs non-empty string {field}", finding.id),
                ));
            }
        }
    }
    if repair_triage
        || matches!(
            template::review_behaviour(node),
            Some(template::ReviewBehaviour::Verify | template::ReviewBehaviour::Synthesis)
        )
    {
        for finding in &outputs.findings {
            let valid = finding.triage().is_some_and(|triage| {
                matches!(triage.decision.as_str(), "confirmed" | "rejected")
                    && !triage.reason.trim().is_empty()
                    && !triage.root_cause.trim().is_empty()
                    && matches!(
                        triage.blocking_class.as_str(),
                        "acceptance" | "default_path_defect" | "boundary" | "data_loss" | "none"
                    )
            });
            if !valid {
                return Some((
                    "review_triage_missing",
                    format!(
                        "finding {} needs checked triage: decision, reason, worth_fixing, root_cause, blocking_class",
                        finding.id
                    ),
                ));
            }
        }
    }
    if outputs.verdict.as_deref() == Some("VERDICT: PASS")
        && outputs.findings.iter().any(|finding| {
            (repair::blocks_acceptance(finding)
                && !(upstream.iter().any(|source| source.id == finding.id)
                    && finding.verification.as_deref() == Some("falsified")
                    && finding.falsified_with_evidence()))
                || finding.verification.as_deref() == Some("unverifiable")
        })
    {
        return Some((
            "review_verdict_conflict",
            "PASS contradicts unresolved blocking, accepted or unverifiable findings".into(),
        ));
    }
    None
}

/// Every grouped item must return a typed outcome, including disputes.
pub(super) fn repair_result_error(
    node: &PlannedNode,
    upstream: &[Finding],
    outputs: &NodeOutputs,
) -> Option<(&'static str, String)> {
    if !repair_writer(node) {
        return None;
    }
    let required: std::collections::BTreeSet<_> = upstream
        .iter()
        .filter(|finding| repair::repair_eligible(finding))
        .filter_map(Finding::triage)
        .map(|triage| triage.root_cause)
        .collect();
    let Some(items) = outputs.repair_items.as_ref() else {
        return Some((
            "repair_items_missing",
            "repair writer must report every grouped item with a typed outcome".into(),
        ));
    };
    let mut seen = std::collections::BTreeSet::new();
    for item in items {
        if !required.contains(&item.id) || !seen.insert(item.id.clone()) {
            return Some((
                "repair_items_invalid",
                "repair outcomes must name each grouped item exactly once".into(),
            ));
        }
    }
    if seen != required {
        return Some((
            "repair_items_incomplete",
            "every grouped fix item needs a repair outcome".into(),
        ));
    }
    None
}

/// Round two and later review the repair delta against the named invariants.
/// An unnamed finding is flagged first as a warning; a blocking finding with
/// no invariant is refused at settlement.
/// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
fn delta_round(node: &PlannedNode) -> bool {
    node.round.as_ref().is_some_and(|round| round.round >= 2)
        && (template::review_behaviour(node).is_some() || repair_writer(node))
}

pub(super) fn invariant_warnings(node: &PlannedNode, outputs: &NodeOutputs) -> Vec<String> {
    if !delta_round(node) {
        return Vec::new();
    }
    outputs
        .findings
        .iter()
        .filter(|finding| finding.named_invariant().is_none())
        .map(|finding| {
            format!(
                "finding_invariant_missing: {} names no invariant; warn first, refuse when it blocks",
                finding.id
            )
        })
        .collect()
}

pub(super) fn invariant_error(
    node: &PlannedNode,
    outputs: &NodeOutputs,
) -> Option<(&'static str, String)> {
    if !delta_round(node) {
        return None;
    }
    let missing = outputs.findings.iter().find(|finding| {
        finding.named_invariant().is_none() && repair::blocking_severity(finding)
    })?;
    Some((
        "review_invariant_missing",
        format!(
            "blocking finding {} names no invariant; round-2 findings must name the invariant they break",
            missing.id
        ),
    ))
}

/// A fix round returns, per invariant, the sweep it ran, every site it changed
/// and the test that pins the invariant. A missing item is flagged at settlement.
/// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
pub(super) fn repair_return_warnings(node: &PlannedNode, outputs: &NodeOutputs) -> Vec<String> {
    if !repair_writer(node) {
        return Vec::new();
    }
    let Some(items) = outputs.repair_items.as_ref() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let mut missing = Vec::new();
            if item
                .sweep
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                missing.push("sweep");
            }
            if item.sites.as_ref().is_none_or(|sites| {
                sites.is_empty() || sites.iter().any(|site| site.trim().is_empty())
            }) {
                missing.push("sites");
            }
            if item
                .pin
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                missing.push("pin");
            }
            (!missing.is_empty()).then(|| {
                format!(
                    "repair_invariant_return_missing: {} has no {}",
                    item.id,
                    missing.join(", ")
                )
            })
        })
        .collect()
}

/// A malformed verdict gets one isolated, checkpointed correction. Expansion
/// history survives resets and restarts, so it is also the retry fence.
pub(super) fn correct_verdict(
    document: &GraphDocument,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
    worktree_layout: &provenant_config::WorktreeLayoutPort,
) -> Result<Option<GraphCheckpoint>, PatchError> {
    if checkpoint.cancelled || checkpoint.pause.is_some() {
        return Ok(None);
    }
    let Some((index, node)) = plan.nodes.iter().enumerate().find(|(index, node)| {
        template::review_behaviour(node) == Some(template::ReviewBehaviour::Lens)
            && checkpoint.nodes[*index].state == NodeState::Failed
            && checkpoint.nodes[*index]
                .review_verdict_correction_refused
                .is_none()
            && checkpoint.nodes[*index].advance_error_code.as_deref()
                == Some("review_verdict_invalid")
            && !checkpoint
                .expansions
                .iter()
                .filter_map(|expansion| expansion.patch.as_ref())
                .any(|patch| {
                    patch.reason.as_deref()
                        == Some(format!("correct review verdict {}", node.id).as_str())
                })
    }) else {
        return Ok(None);
    };
    let mut next = checkpoint.clone();
    let brief = format!(
        "{}\n\nThe previous reply had an invalid verdict. Return the findings. {VERDICT_THEN_HANDOFF} This is the only automatic verdict correction.",
        node.prompt
    );
    if let Err(error) = patch::correct_review_verdict(
        &mut LiveGraph {
            document,
            checkpoint: &mut next,
        },
        &node.id,
        &brief,
        &PatchContext {
            catalogue: None,
            library: &TemplateLibrary::bundled(),
            resolvers: &Default::default(),
            limits,

            journal: None,
            worktree_layout: worktree_layout.clone(),
        },
    ) {
        if let Some(PlanFailureFact::Structural(limit)) = &error.cause {
            next = checkpoint.clone();
            plans::set_expansion_pause(
                &mut next,
                index,
                limit.to_string(),
                false,
                None,
                Some(*limit),
            );
            return Ok(Some(next));
        }
        // Admission is atomic: keep the failed attempt and fence further
        // automatic corrections so coverage can settle on subsequent advances.
        next = checkpoint.clone();
        if next.nodes[index].expansion_pause.is_some() {
            plans::clear_expansion_pause(&mut next, index, true);
        }
        next.nodes[index].review_verdict_correction_refused =
            Some(bounded(&error.to_string(), MAX_ADVANCE_ERROR_BYTES));
        return Ok(Some(next));
    }
    transitions::retain_automatic_retry_accounting(
        &checkpoint.nodes[index],
        &mut next.nodes[index],
    );
    Ok(Some(next))
}

/// Review evidence can cross several aggregation boundaries.
pub(super) fn composite_review(
    spec: &CompositeSpec,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    spec.exports.iter().any(|export| {
        let node = &plan.nodes[positions[&export.node]];
        template::review_behaviour(node).is_some()
            || node
                .composite
                .as_ref()
                .is_some_and(|spec| composite_review(spec, plan, positions))
    })
}

#[cfg(test)]
mod delivery_tests {
    use super::*;

    #[test]
    fn delivery_review_requires_complete_nonblocking_evidence() {
        let mut policy = DeliveryReviewSpec::default();
        let mut outputs = NodeOutputs {
            verdict: Some("VERDICT: INCONCLUSIVE".into()),
            findings_available: Some(true),
            ..Default::default()
        };
        assert!(delivery_matches(&policy, &outputs, "VERDICT: PASS"));
        policy.accept_inconclusive_without_blocking = false;
        assert!(!delivery_matches(&policy, &outputs, "VERDICT: PASS"));
        policy.accept_inconclusive_without_blocking = true;
        outputs.findings_available = None;
        assert!(!delivery_matches(&policy, &outputs, "VERDICT: PASS"));
        outputs.findings_available = Some(true);
        outputs.partial = true;
        assert!(!delivery_matches(&policy, &outputs, "VERDICT: PASS"));
        outputs.partial = false;
        outputs.findings.push(
            serde_json::from_value(serde_json::json!({
                "id":"F1", "severity":"blocking", "file":"a.rs", "line":1,
                "summary":"defect", "status":"open"
            }))
            .unwrap(),
        );
        assert!(!delivery_matches(&policy, &outputs, "VERDICT: PASS"));
        outputs.findings.clear();
        outputs.verdict = Some("VERDICT: FAIL".into());
        assert!(!delivery_matches(&policy, &outputs, "VERDICT: PASS"));
    }

    #[test]
    fn delivery_review_evidence_parks_contract_errors_but_not_valid_failures() {
        assert!(malformed_delivery_evidence_error(Some(
            "review_verdict_invalid"
        )));
        assert!(malformed_delivery_evidence_error(Some(
            "review_findings_invalid"
        )));
        assert!(malformed_delivery_evidence_error(Some(
            "review_triage_missing"
        )));
        assert!(!malformed_delivery_evidence_error(None));
        assert!(!malformed_delivery_evidence_error(Some("review_failed")));
    }
}
